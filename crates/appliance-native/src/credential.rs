//! Shared app-private credential loading, import, and host randomness.

use std::fmt;
use std::fs::{self, File, OpenOptions, symlink_metadata};
use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api_ble::local_name;
use reticulum_device_client::{ACTIVATED_CREDENTIAL_STATE_BYTES, ActivatedCredential};
use zeroize::Zeroizing;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const E290_DEVICE_ID_PREFIX: &[u8; 10] = b"e290-api-1";
const IMPORT_STAGING_ATTEMPTS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CredentialImportPolicy {
    AnyDevice,
    E290BleTarget,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CredentialImportError {
    Rejected { reason: String },
    PublicationUncertain { reason: String },
}

impl fmt::Display for CredentialImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected { reason } | Self::PublicationUncertain { reason } => {
                formatter.write_str(reason)
            }
        }
    }
}

impl From<String> for CredentialImportError {
    fn from(reason: String) -> Self {
        Self::Rejected { reason }
    }
}

/// Public facts decoded from one canonical Active credential.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativeCredentialSummary {
    /// Expected device API identifier as lowercase hexadecimal.
    pub device_id: String,
    /// Opaque credential identifier as lowercase hexadecimal.
    pub credential_id: String,
    /// Active device-owned credential generation.
    pub generation: u64,
    /// Stable E290 BLE advertising name when the device ID uses that namespace.
    pub expected_ble_local_name: Option<String>,
}

/// App-private activated-credential state without exposing secret bytes.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum NativeCredentialStatus {
    /// The configured app-private credential path does not exist.
    Missing,
    /// One canonical Active credential is ready for authentication.
    Active {
        /// Public credential and target-device facts.
        summary: NativeCredentialSummary,
    },
    /// A path exists but cannot be safely used as an Active credential.
    Invalid {
        /// Bounded local diagnostic; never includes credential bytes.
        reason: String,
    },
}

pub(crate) fn read_credential(path: &Path) -> Result<ActivatedCredential, String> {
    let bytes = read_credential_bytes(path)?;
    ActivatedCredential::decode(&bytes[..])
        .map_err(|error| format!("could not decode app-private credential: {error}"))
}

fn read_credential_bytes(
    path: &Path,
) -> Result<Zeroizing<[u8; ACTIVATED_CREDENTIAL_STATE_BYTES]>, String> {
    let metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect app-private credential: {error}"))?;
    require_regular_file(&metadata, "app-private credential")?;
    enforce_credential_owner_only(&metadata)?;
    let mut file = File::open(path)
        .map_err(|error| format!("could not open app-private credential: {error}"))?;
    verify_open_file_identity(&metadata, &file, "app-private credential")?;
    let mut bytes = Zeroizing::new([0_u8; ACTIVATED_CREDENTIAL_STATE_BYTES]);
    file.read_exact(&mut bytes[..])
        .map_err(|error| format!("could not read complete app-private credential: {error}"))?;
    let mut trailing = [0_u8; 1];
    if file
        .read(&mut trailing)
        .map_err(|error| format!("could not finish reading app-private credential: {error}"))?
        != 0
    {
        return Err("app-private credential contains trailing bytes".to_owned());
    }
    Ok(bytes)
}

pub(crate) fn inspect_credential(path: &Path) -> NativeCredentialStatus {
    match symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            NativeCredentialStatus::Missing
        }
        Err(error) => NativeCredentialStatus::Invalid {
            reason: format!("could not inspect app-private credential: {error}"),
        },
        Ok(_) => match read_credential(path) {
            Ok(credential) => NativeCredentialStatus::Active {
                summary: credential_summary(&credential),
            },
            Err(reason) => NativeCredentialStatus::Invalid { reason },
        },
    }
}

pub(crate) fn install_credential(
    path: &Path,
    bytes: &[u8],
    policy: CredentialImportPolicy,
) -> Result<NativeCredentialSummary, CredentialImportError> {
    let credential = ActivatedCredential::decode(bytes)
        .map_err(|error| format!("could not decode imported credential: {error}"))?;
    let summary = credential_summary(&credential);
    drop(credential);
    enforce_import_policy(&summary, policy)?;

    match symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(
                format!("could not inspect app-private credential destination: {error}").into(),
            );
        }
        Ok(_) => {
            return Err(
                "app-private credential destination already exists; replacement requires an explicit recovery flow"
                    .to_owned()
                    .into(),
            );
        }
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "app-private credential destination has no parent directory".to_owned())?;
    let parent_metadata = symlink_metadata(parent)
        .map_err(|error| format!("could not inspect credential parent directory: {error}"))?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err("credential parent must be a real app-private directory"
            .to_owned()
            .into());
    }

    let staging = create_staging_file(path)?;
    let staging_path = staging.path.clone();
    let install_result = write_and_publish(staging, path, bytes);
    if install_result.is_err() {
        let _ = fs::remove_file(&staging_path);
    }
    install_result?;

    verify_exact_credential_readback(path, bytes)?;
    Ok(summary)
}

pub(crate) fn import_credential_file(
    destination: &Path,
    source: &Path,
    policy: CredentialImportPolicy,
) -> Result<NativeCredentialSummary, CredentialImportError> {
    if !source.is_absolute() {
        return Err("credential import staging path must be absolute"
            .to_owned()
            .into());
    }
    if source == destination {
        return Err(
            "credential import staging path must differ from its destination"
                .to_owned()
                .into(),
        );
    }
    let source_metadata = symlink_metadata(source)
        .map_err(|error| format!("could not inspect credential import staging file: {error}"))?;
    require_regular_file(&source_metadata, "credential import staging path")?;

    let mut file = File::open(source)
        .map_err(|error| format!("could not open credential import staging file: {error}"))?;
    verify_open_file_identity(&source_metadata, &file, "credential import staging file")?;
    let mut bytes = Zeroizing::new([0_u8; ACTIVATED_CREDENTIAL_STATE_BYTES]);
    file.read_exact(&mut bytes[..])
        .map_err(|error| format!("could not read credential import staging file: {error}"))?;
    let mut trailing = [0_u8; 1];
    if file.read(&mut trailing).map_err(|error| {
        format!("could not finish reading credential import staging file: {error}")
    })? != 0
    {
        return Err("credential import staging file contains trailing bytes"
            .to_owned()
            .into());
    }
    drop(file);

    // The caller owns and removes this app-private staging copy in a `finally`
    // path. Keeping deletion out of the public native method prevents an
    // arbitrary caller-provided path from becoming a file-deletion capability.
    install_credential(destination, &bytes[..], policy)
}

struct StagingFile {
    file: File,
    path: PathBuf,
}

fn create_staging_file(destination: &Path) -> Result<StagingFile, String> {
    let parent = destination
        .parent()
        .expect("credential destination parent was validated");
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "credential destination filename is not valid UTF-8".to_owned())?;

    for _ in 0..IMPORT_STAGING_ATTEMPTS {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce)
            .map_err(|error| format!("could not generate credential staging name: {error}"))?;
        let path = parent.join(format!(".{name}.import-{}", hex::encode(nonce)));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(file) => return Ok(StagingFile { file, path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "could not create app-private credential staging file: {error}"
                ));
            }
        }
    }
    Err("could not allocate a unique credential staging file".to_owned())
}

fn write_and_publish(
    mut staging: StagingFile,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), CredentialImportError> {
    debug_assert_eq!(bytes.len(), ACTIVATED_CREDENTIAL_STATE_BYTES);
    staging
        .file
        .write_all(bytes)
        .map_err(|error| format!("could not write credential staging file: {error}"))?;
    staging
        .file
        .sync_all()
        .map_err(|error| format!("could not synchronize credential staging file: {error}"))?;
    drop(staging.file);

    // A hard link is an atomic no-replace publication primitive on the same
    // filesystem. Ordinary rename would overwrite a destination that appeared
    // after our earlier absence check.
    fs::hard_link(&staging.path, destination).map_err(|error| CredentialImportError::Rejected {
        reason: format!("could not publish app-private credential without replacement: {error}"),
    })?;
    fs::remove_file(&staging.path).map_err(|error| {
        CredentialImportError::PublicationUncertain {
            reason: format!("credential was published but its staging link remains: {error}"),
        }
    })?;
    sync_directory(
        destination
            .parent()
            .expect("credential destination parent was validated"),
        "credential destination directory",
    )
    .map_err(|reason| CredentialImportError::PublicationUncertain { reason })
}

fn require_regular_file(metadata: &fs::Metadata, label: &str) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label} must be a regular non-symlink file"));
    }
    Ok(())
}

#[cfg(unix)]
fn enforce_credential_owner_only(metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(
            "app-private credential must not grant group or other permissions (use mode 600)"
                .to_owned(),
        );
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err("app-private credential must be owned by the effective user".to_owned());
    }
    if metadata.nlink() != 1 {
        return Err("app-private credential must have exactly one hard link".to_owned());
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_credential_owner_only(_metadata: &fs::Metadata) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn verify_open_file_identity(
    expected: &fs::Metadata,
    file: &File,
    label: &str,
) -> Result<(), String> {
    let opened = file
        .metadata()
        .map_err(|error| format!("could not recheck {label}: {error}"))?;
    if expected.dev() != opened.dev() || expected.ino() != opened.ino() {
        return Err(format!("{label} changed while it was being opened"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_open_file_identity(
    _expected: &fs::Metadata,
    _file: &File,
    _label: &str,
) -> Result<(), String> {
    Ok(())
}

fn sync_directory(path: &Path, label: &str) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("could not synchronize {label}: {error}"))
}

fn credential_summary(credential: &ActivatedCredential) -> NativeCredentialSummary {
    let device_id = credential.device_id();
    NativeCredentialSummary {
        device_id: hex::encode(device_id.as_bytes()),
        credential_id: hex::encode(credential.credential_id().as_bytes()),
        generation: credential.generation().get(),
        expected_ble_local_name: e290_ble_advertised_name(device_id.as_bytes()),
    }
}

fn enforce_import_policy(
    summary: &NativeCredentialSummary,
    policy: CredentialImportPolicy,
) -> Result<(), String> {
    match policy {
        CredentialImportPolicy::AnyDevice => Ok(()),
        CredentialImportPolicy::E290BleTarget if summary.expected_ble_local_name.is_some() => {
            Ok(())
        }
        CredentialImportPolicy::E290BleTarget => Err(
            "current BLE import requires an E290 credential with a derivable advertising name"
                .to_owned(),
        ),
    }
}

fn verify_exact_credential_readback(path: &Path, expected: &[u8]) -> Result<(), String> {
    let installed = read_credential_bytes(path).map_err(|reason| {
        format!("installed credential failed exact readback validation: {reason}")
    })?;
    if installed.as_slice() != expected {
        return Err("installed credential bytes changed during publication or readback".to_owned());
    }
    Ok(())
}

fn e290_ble_advertised_name(device_id: &[u8; 16]) -> Option<String> {
    if &device_id[..E290_DEVICE_ID_PREFIX.len()] != E290_DEVICE_ID_PREFIX {
        return None;
    }
    let eui48: [u8; 6] = device_id[E290_DEVICE_ID_PREFIX.len()..]
        .try_into()
        .expect("E290 device ID suffix is one EUI-48");
    String::from_utf8(local_name(eui48).to_vec()).ok()
}

pub(crate) struct HostRng;

impl RngCore for HostRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0_u8; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0_u8; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        self.try_fill_bytes(destination)
            .unwrap_or_else(|error| panic!("operating-system randomness failed: {error}"));
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        getrandom::fill(destination).map_err(|_| {
            NonZeroU32::new(rand_core::Error::CUSTOM_START)
                .expect("rand_core custom error base is nonzero")
                .into()
        })
    }
}

impl CryptoRng for HostRng {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock follows Unix epoch")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "reticulum-native-credential-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test credential directory is created");
            Self(path)
        }

        fn credential(&self) -> PathBuf {
            self.0.join("credential.rdpkey")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn credential_bytes(device_suffix: [u8; 6]) -> [u8; ACTIVATED_CREDENTIAL_STATE_BYTES] {
        let mut bytes = [0_u8; ACTIVATED_CREDENTIAL_STATE_BYTES];
        bytes[..8].copy_from_slice(b"RDPKEY1\0");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        bytes[10] = 2;
        bytes[16..26].copy_from_slice(E290_DEVICE_ID_PREFIX);
        bytes[26..32].copy_from_slice(&device_suffix);
        bytes[32..48].fill(0x42);
        bytes[48..56].copy_from_slice(&7_u64.to_le_bytes());
        bytes[56..88].fill(0x24);
        bytes
    }

    #[test]
    fn one_time_import_is_validated_and_targets_the_matching_e290() {
        let directory = TestDirectory::new();
        let path = directory.credential();
        assert_eq!(inspect_credential(&path), NativeCredentialStatus::Missing);

        let bytes = credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]);
        let summary = install_credential(&path, &bytes, CredentialImportPolicy::AnyDevice)
            .expect("valid import succeeds");
        assert_eq!(summary.device_id, "653239302d6170692d31aca704e13e88");
        assert_eq!(summary.credential_id, "42".repeat(16));
        assert_eq!(summary.generation, 7);
        assert_eq!(
            summary.expected_ble_local_name.as_deref(),
            Some("reticulum-e290-e13e88")
        );
        assert_eq!(
            inspect_credential(&path),
            NativeCredentialStatus::Active { summary }
        );
    }

    #[test]
    fn import_rejects_malformed_input_and_never_replaces_an_existing_path() {
        let directory = TestDirectory::new();
        let path = directory.credential();
        assert!(
            install_credential(
                &path,
                b"not a credential",
                CredentialImportPolicy::AnyDevice
            )
            .is_err()
        );
        assert!(!path.exists());

        fs::write(&path, b"existing invalid owner").expect("test invalid owner is written");
        let error = install_credential(
            &path,
            &credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88]),
            CredentialImportPolicy::AnyDevice,
        )
        .expect_err("existing path is never replaced");
        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            fs::read(&path).expect("existing bytes remain"),
            b"existing invalid owner"
        );
    }

    #[test]
    fn exact_readback_compares_secret_bytes_not_only_public_identity() {
        let directory = TestDirectory::new();
        let path = directory.credential();
        let expected = credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88]);
        install_credential(&path, &expected, CredentialImportPolicy::AnyDevice)
            .expect("test credential is installed");

        let mut changed_psk = expected;
        changed_psk[56] ^= 0xff;
        fs::write(&path, changed_psk).expect("test changes only the secret PSK");

        let changed =
            ActivatedCredential::decode(&changed_psk).expect("changed PSK remains canonical");
        let original = ActivatedCredential::decode(&expected).expect("original is canonical");
        assert_eq!(credential_summary(&changed), credential_summary(&original));
        assert!(
            verify_exact_credential_readback(&path, &expected)
                .expect_err("full-byte readback detects the changed PSK")
                .contains("bytes changed")
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_credential_must_remain_owner_only() {
        let directory = TestDirectory::new();
        let path = directory.credential();
        install_credential(
            &path,
            &credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88]),
            CredentialImportPolicy::AnyDevice,
        )
        .expect("test credential is installed");
        assert_eq!(
            fs::metadata(&path)
                .expect("installed credential has metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("test broadens credential permissions");
        assert!(matches!(
            inspect_credential(&path),
            NativeCredentialStatus::Invalid { reason }
                if reason.contains("group or other permissions")
        ));
    }

    #[test]
    fn non_e290_credentials_do_not_guess_a_ble_name() {
        let mut bytes = credential_bytes([1, 2, 3, 4, 5, 6]);
        bytes[16..26].copy_from_slice(b"other-api-");
        let credential = ActivatedCredential::decode(&bytes).expect("fixture remains canonical");
        assert_eq!(
            credential_summary(&credential).expected_ble_local_name,
            None
        );
    }

    #[test]
    fn e290_ble_policy_rejects_a_generic_credential_before_publication() {
        let directory = TestDirectory::new();
        let destination = directory.credential();
        let mut bytes = credential_bytes([1, 2, 3, 4, 5, 6]);
        bytes[16..26].copy_from_slice(b"other-api-");

        let error = install_credential(&destination, &bytes, CredentialImportPolicy::E290BleTarget)
            .expect_err("current BLE profile rejects a non-E290 target");
        assert!(
            error
                .to_string()
                .contains("current BLE import requires an E290 credential")
        );
        assert_eq!(
            inspect_credential(&destination),
            NativeCredentialStatus::Missing
        );

        let summary = install_credential(&destination, &bytes, CredentialImportPolicy::AnyDevice)
            .expect("generic connector policy retains future-board compatibility");
        assert_eq!(summary.expected_ble_local_name, None);
    }

    #[test]
    fn file_import_leaves_staging_cleanup_to_the_app_owner() {
        let directory = TestDirectory::new();
        let destination = directory.credential();
        let staging = directory.0.join("picked-credential.rdpkey");
        fs::write(
            &staging,
            credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88]),
        )
        .expect("test staging credential is written");

        let summary =
            import_credential_file(&destination, &staging, CredentialImportPolicy::AnyDevice)
                .expect("staged import succeeds");
        assert_eq!(
            summary.expected_ble_local_name.as_deref(),
            Some("reticulum-e290-e13f88")
        );
        assert!(staging.exists());
        assert!(destination.exists());
    }

    #[test]
    fn file_import_rejects_noncanonical_source_shapes() {
        let directory = TestDirectory::new();
        let destination = directory.credential();
        let relative = Path::new("relative-credential.rdpkey");
        assert!(
            import_credential_file(&destination, relative, CredentialImportPolicy::AnyDevice,)
                .expect_err("relative import path is rejected")
                .to_string()
                .contains("must be absolute")
        );

        let trailing = directory.0.join("trailing-credential.rdpkey");
        let mut bytes = credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88]).to_vec();
        bytes.push(0xff);
        fs::write(&trailing, bytes).expect("test trailing source is written");
        assert!(
            import_credential_file(&destination, &trailing, CredentialImportPolicy::AnyDevice,)
                .expect_err("trailing import bytes are rejected")
                .to_string()
                .contains("trailing bytes")
        );
        assert!(!destination.exists());

        #[cfg(unix)]
        {
            let symlink = directory.0.join("symlink-credential.rdpkey");
            std::os::unix::fs::symlink(&trailing, &symlink)
                .expect("test source symlink is created");
            assert!(
                import_credential_file(&destination, &symlink, CredentialImportPolicy::AnyDevice,)
                    .expect_err("source symlink is rejected")
                    .to_string()
                    .contains("regular non-symlink")
            );
        }
    }

    #[test]
    fn concurrent_installs_publish_exactly_one_credential_without_replacement() {
        let directory = TestDirectory::new();
        let destination = directory.credential();
        let first = credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]);
        let second = credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88]);

        std::thread::scope(|scope| {
            let first_install = scope.spawn(|| {
                install_credential(&destination, &first, CredentialImportPolicy::AnyDevice)
            });
            let second_install = scope.spawn(|| {
                install_credential(&destination, &second, CredentialImportPolicy::AnyDevice)
            });
            let first_result = first_install
                .join()
                .expect("first installer does not panic");
            let second_result = second_install
                .join()
                .expect("second installer does not panic");
            assert_ne!(first_result.is_ok(), second_result.is_ok());
        });

        let status = inspect_credential(&destination);
        assert!(matches!(status, NativeCredentialStatus::Active { .. }));
        let installed = fs::read(&destination).expect("one canonical credential was published");
        assert!(installed == first || installed == second);
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_ambiguity_is_not_accepted_as_an_active_credential() {
        let directory = TestDirectory::new();
        let path = directory.credential();
        install_credential(
            &path,
            &credential_bytes([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88]),
            CredentialImportPolicy::AnyDevice,
        )
        .expect("test credential is installed");
        let second_link = directory.0.join("ambiguous-second-link.rdpkey");
        fs::hard_link(&path, &second_link).expect("test creates an ambiguous second link");

        assert!(matches!(
            inspect_credential(&path),
            NativeCredentialStatus::Invalid { reason }
                if reason.contains("exactly one hard link")
        ));
    }
}
