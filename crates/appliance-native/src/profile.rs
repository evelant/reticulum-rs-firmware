//! App-private, device-keyed mobile profile storage.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::appliance::NativeApplianceError;
use crate::credential::{
    CredentialImportError, CredentialImportPolicy, NativeCredentialStatus, NativeCredentialSummary,
    credential_summary_from_bytes, inspect_credential, install_credential, read_credential_bytes,
    read_import_credential_file,
};

const PROFILES_DIRECTORY: &str = "profiles";
const UNCONFIGURED_DIRECTORY: &str = "unconfigured";
const CREDENTIAL_FILE: &str = "credential.rdpkey";
const ONBOARDING_CREDENTIAL_FILE: &str = "ble-onboarding.rdpkey";
const DATABASE_FILE: &str = "chat.sqlite3";
const ACTIVE_PROFILE_FILE: &str = "active-profile-v1";
const ACTIVE_PROFILE_MAGIC: &[u8] = b"RETICULUM-APPLIANCE-ACTIVE-PROFILE-1\n";
const METADATA_STAGING_ATTEMPTS: usize = 16;
const DEVICE_ID_HEX_BYTES: usize = 32;

/// Public, secret-free facts for one stored physical-device profile.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativeProfileSummary {
    /// Canonical lowercase hexadecimal profile key.
    pub profile_key: String,
    /// Public facts decoded from the profile's canonical Active credential.
    pub credential: NativeCredentialSummary,
}

/// Secret-free projection of the device-keyed profile store.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativeProfileStoreSnapshot {
    /// Canonical key selected for the single active application session.
    pub active_profile_key: Option<String>,
    /// All validated profiles, sorted by canonical key.
    pub profiles: Vec<NativeProfileSummary>,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeProfileRuntimePaths {
    pub(crate) database: PathBuf,
    pub(crate) credential: PathBuf,
}

#[derive(Clone, Debug)]
struct LegacyPaths {
    database: PathBuf,
    credential: PathBuf,
}

enum ProfileMetadataError {
    Rejected { reason: String },
    PublicationUncertain { reason: String },
}

impl ProfileMetadataError {
    fn into_native(self) -> NativeApplianceError {
        let reason = match self {
            Self::Rejected { reason } | Self::PublicationUncertain { reason } => reason,
        };
        NativeApplianceError::Storage { reason }
    }

    fn into_import(self) -> CredentialImportError {
        match self {
            Self::Rejected { reason } => CredentialImportError::Rejected { reason },
            Self::PublicationUncertain { reason } => {
                CredentialImportError::PublicationUncertain { reason }
            }
        }
    }
}

/// Native owner for device-keyed credential and SQLite profile paths.
///
/// The generated mobile binding exposes only validated public identity
/// summaries. Credential bytes remain inside Rust and app-private files.
#[derive(uniffi::Object)]
pub struct NativeProfileStore {
    root: PathBuf,
    legacy: Option<LegacyPaths>,
    gate: Mutex<()>,
}

#[uniffi::export]
impl NativeProfileStore {
    /// Open or create an app-private profile root.
    ///
    /// The optional legacy paths identify the previous single-database and
    /// single-credential layout. When the legacy credential is canonical,
    /// both artifacts are migrated to its validated device-ID profile before
    /// the store is returned. An invalid legacy credential remains untouched
    /// and is reported through [`Self::credential_status`].
    #[uniffi::constructor]
    pub fn open(
        root_directory: String,
        legacy_database_path: Option<String>,
        legacy_credential_path: Option<String>,
    ) -> Result<Arc<Self>, NativeApplianceError> {
        let root = validated_absolute_path(&root_directory, "profile root")?;
        let legacy = match (legacy_database_path, legacy_credential_path) {
            (None, None) => None,
            (Some(database), Some(credential)) => Some(LegacyPaths {
                database: validated_absolute_path(&database, "legacy database path")?,
                credential: validated_absolute_path(&credential, "legacy credential path")?,
            }),
            _ => {
                return Err(NativeApplianceError::InvalidArgument {
                    reason: "legacy database and credential paths must be supplied together"
                        .to_owned(),
                });
            }
        };
        if let Some(legacy) = &legacy
            && (legacy.database.starts_with(&root) || legacy.credential.starts_with(&root))
        {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "legacy artifact paths must be outside the new profile root".to_owned(),
            });
        }
        if let Some(legacy) = &legacy
            && legacy.database == legacy.credential
        {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "legacy database and credential paths must differ".to_owned(),
            });
        }

        create_private_directory(&root, "profile root")?;
        create_private_directory(&root.join(PROFILES_DIRECTORY), "profiles directory")?;
        create_private_directory(
            &root.join(UNCONFIGURED_DIRECTORY),
            "unconfigured profile directory",
        )?;

        let store = Arc::new(Self {
            root,
            legacy,
            gate: Mutex::new(()),
        });
        {
            let _guard = store.lock_gate()?;
            store.migrate_legacy_if_present()?;
            store.validate_active_profile_if_present()?;
        }
        Ok(store)
    }

    /// Return all validated profiles and the currently active profile key.
    pub fn snapshot(&self) -> Result<NativeProfileStoreSnapshot, NativeApplianceError> {
        let _guard = self.lock_gate()?;
        self.snapshot_locked()
    }

    /// Inspect the active profile's credential without returning secret bytes.
    ///
    /// An empty store is `Missing`. A malformed legacy artifact or active
    /// profile is `Invalid` so the existing onboarding recovery boundary is
    /// preserved.
    pub fn credential_status(&self) -> Result<NativeCredentialStatus, NativeApplianceError> {
        let _guard = self.lock_gate()?;
        self.credential_status_locked()
    }

    /// Select one existing validated profile for the next native appliance.
    ///
    /// The current Expo UI remains single-profile and does not call this yet;
    /// exposing the native operation establishes the future board-switching
    /// boundary without putting credential bytes or filesystem paths in
    /// TypeScript.
    pub fn activate_profile(
        &self,
        device_id: String,
    ) -> Result<NativeProfileSummary, NativeApplianceError> {
        let profile_key = validate_profile_key(&device_id)?;
        let _guard = self.lock_gate()?;
        let profile = self.profile_summary_locked(&profile_key)?;
        write_active_profile(&self.root, &profile_key)
            .map_err(ProfileMetadataError::into_native)?;
        Ok(profile)
    }
}

impl NativeProfileStore {
    pub(crate) fn onboarding_credential_path(&self) -> PathBuf {
        self.root
            .join(UNCONFIGURED_DIRECTORY)
            .join(ONBOARDING_CREDENTIAL_FILE)
    }

    pub(crate) fn promote_onboarding_credential(
        &self,
        expected_device_id: [u8; 16],
        expected_credential_id: [u8; 16],
        expected_generation: u64,
    ) -> Result<NativeProfileSummary, NativeApplianceError> {
        let _guard = self.lock_gate()?;
        let onboarding_path = self.onboarding_credential_path();
        let bytes = read_credential_bytes(&onboarding_path)
            .map_err(|reason| NativeApplianceError::Storage { reason })?;
        let summary =
            credential_summary_from_bytes(bytes.as_slice(), CredentialImportPolicy::AnyDevice)
                .map_err(NativeApplianceError::from)?;
        if summary.device_id != hex::encode(expected_device_id)
            || summary.credential_id != hex::encode(expected_credential_id)
            || summary.generation != expected_generation
        {
            return Err(NativeApplianceError::Storage {
                reason: "activated onboarding artifact does not match its authenticated pairing response"
                    .to_owned(),
            });
        }

        let profile = self
            .install_and_activate_profile_locked(
                bytes.as_slice(),
                &summary,
                CredentialImportPolicy::AnyDevice,
            )
            .map_err(NativeApplianceError::from)?;
        fs::remove_file(&onboarding_path)
            .map_err(storage_error("remove promoted onboarding credential"))?;
        sync_directory(
            onboarding_path
                .parent()
                .expect("onboarding credential has a directory"),
            "unconfigured profile directory",
        )?;
        Ok(profile)
    }

    pub(crate) fn runtime_paths(&self) -> Result<NativeProfileRuntimePaths, NativeApplianceError> {
        let _guard = self.lock_gate()?;
        if let Some(profile_key) = read_active_profile(&self.root)? {
            self.profile_summary_locked(&profile_key)?;
            return Ok(self.profile_paths(&profile_key));
        }

        if let Some(legacy) = &self.legacy
            && !matches!(
                inspect_credential(&legacy.credential),
                NativeCredentialStatus::Missing
            )
        {
            return Ok(NativeProfileRuntimePaths {
                database: legacy.database.clone(),
                credential: legacy.credential.clone(),
            });
        }

        Ok(NativeProfileRuntimePaths {
            database: self.root.join(UNCONFIGURED_DIRECTORY).join(DATABASE_FILE),
            credential: self.root.join(UNCONFIGURED_DIRECTORY).join(CREDENTIAL_FILE),
        })
    }

    pub(crate) fn import_activated_credential(
        &self,
        staging_path: &Path,
        policy: CredentialImportPolicy,
    ) -> Result<NativeCredentialSummary, CredentialImportError> {
        let _guard = self.lock_gate().map_err(profile_error_as_import)?;
        if let Some(legacy) = &self.legacy
            && matches!(
                inspect_credential(&legacy.credential),
                NativeCredentialStatus::Invalid { .. }
            )
        {
            return Err(CredentialImportError::Rejected {
                reason: "legacy app-private credential is invalid; replacement requires an explicit recovery flow"
                    .to_owned(),
            });
        }

        let (bytes, summary) = read_import_credential_file(staging_path, policy)?;
        self.install_and_activate_profile_locked(bytes.as_slice(), &summary, policy)
            .map(|profile| profile.credential)
    }

    fn install_and_activate_profile_locked(
        &self,
        bytes: &[u8],
        summary: &NativeCredentialSummary,
        policy: CredentialImportPolicy,
    ) -> Result<NativeProfileSummary, CredentialImportError> {
        let profile_key =
            validate_profile_key(&summary.device_id).map_err(profile_error_as_import)?;
        let paths = self.profile_paths(&profile_key);
        create_private_directory(
            paths
                .credential
                .parent()
                .expect("profile credential has a directory"),
            "device profile",
        )
        .map_err(profile_error_as_import)?;

        match inspect_credential(&paths.credential) {
            NativeCredentialStatus::Missing => {
                install_credential(&paths.credential, bytes, policy)?;
            }
            NativeCredentialStatus::Active {
                summary: existing_summary,
            } => {
                let existing = read_credential_bytes(&paths.credential)
                    .map_err(CredentialImportError::from)?;
                if existing.as_slice() != bytes || existing_summary != *summary {
                    return Err(CredentialImportError::Rejected {
                        reason: "device profile already contains a different credential; replacement requires an explicit recovery flow"
                            .to_owned(),
                    });
                }
            }
            NativeCredentialStatus::Invalid { reason } => {
                return Err(CredentialImportError::Rejected {
                    reason: format!(
                        "device profile credential is invalid and cannot be replaced: {reason}"
                    ),
                });
            }
        }

        write_active_profile(&self.root, &profile_key)
            .map_err(ProfileMetadataError::into_import)?;
        Ok(NativeProfileSummary {
            profile_key,
            credential: summary.clone(),
        })
    }

    fn lock_gate(&self) -> Result<MutexGuard<'_, ()>, NativeApplianceError> {
        self.gate
            .lock()
            .map_err(|_| NativeApplianceError::Internal {
                reason: "native profile store lock is poisoned".to_owned(),
            })
    }

    fn snapshot_locked(&self) -> Result<NativeProfileStoreSnapshot, NativeApplianceError> {
        let active_profile_key = read_active_profile(&self.root)?;
        let profiles_root = self.root.join(PROFILES_DIRECTORY);
        require_private_directory(&profiles_root, "profiles directory")?;
        let mut profiles = Vec::new();
        for entry in
            fs::read_dir(&profiles_root).map_err(storage_error("read profiles directory"))?
        {
            let entry = entry.map_err(storage_error("read profile directory entry"))?;
            let name =
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| NativeApplianceError::Storage {
                        reason: "profile directory contains a non-UTF-8 name".to_owned(),
                    })?;
            let profile_key =
                validate_profile_key(&name).map_err(|_| NativeApplianceError::Storage {
                    reason: format!("profile directory contains invalid device key {name}"),
                })?;
            require_private_directory(&entry.path(), "device profile")?;
            profiles.push(self.profile_summary_locked(&profile_key)?);
        }
        profiles.sort_unstable_by(|left, right| left.profile_key.cmp(&right.profile_key));

        if let Some(active) = &active_profile_key
            && !profiles
                .iter()
                .any(|profile| &profile.profile_key == active)
        {
            return Err(NativeApplianceError::Storage {
                reason: format!("active profile {active} does not have a validated profile"),
            });
        }
        Ok(NativeProfileStoreSnapshot {
            active_profile_key,
            profiles,
        })
    }

    fn credential_status_locked(&self) -> Result<NativeCredentialStatus, NativeApplianceError> {
        let Some(profile_key) = read_active_profile(&self.root)? else {
            if let Some(legacy) = &self.legacy {
                return Ok(inspect_credential(&legacy.credential));
            }
            return Ok(NativeCredentialStatus::Missing);
        };
        let paths = self.profile_paths(&profile_key);
        Ok(match inspect_credential(&paths.credential) {
            NativeCredentialStatus::Active { summary } if summary.device_id == profile_key => {
                NativeCredentialStatus::Active { summary }
            }
            NativeCredentialStatus::Active { summary } => NativeCredentialStatus::Invalid {
                reason: format!(
                    "profile key {profile_key} does not match credential device ID {}",
                    summary.device_id
                ),
            },
            status => status,
        })
    }

    fn profile_summary_locked(
        &self,
        profile_key: &str,
    ) -> Result<NativeProfileSummary, NativeApplianceError> {
        let paths = self.profile_paths(profile_key);
        require_private_directory(
            paths
                .credential
                .parent()
                .expect("profile credential has a directory"),
            "device profile",
        )?;
        match inspect_credential(&paths.credential) {
            NativeCredentialStatus::Active { summary } if summary.device_id == profile_key => {
                Ok(NativeProfileSummary {
                    profile_key: profile_key.to_owned(),
                    credential: summary,
                })
            }
            NativeCredentialStatus::Active { summary } => Err(NativeApplianceError::Storage {
                reason: format!(
                    "profile key {profile_key} does not match credential device ID {}",
                    summary.device_id
                ),
            }),
            NativeCredentialStatus::Missing => Err(NativeApplianceError::Storage {
                reason: format!("profile {profile_key} has no credential"),
            }),
            NativeCredentialStatus::Invalid { reason } => Err(NativeApplianceError::Storage {
                reason: format!("profile {profile_key} credential is invalid: {reason}"),
            }),
        }
    }

    fn profile_paths(&self, profile_key: &str) -> NativeProfileRuntimePaths {
        let directory = self.root.join(PROFILES_DIRECTORY).join(profile_key);
        NativeProfileRuntimePaths {
            database: directory.join(DATABASE_FILE),
            credential: directory.join(CREDENTIAL_FILE),
        }
    }

    fn migrate_legacy_if_present(&self) -> Result<(), NativeApplianceError> {
        let Some(legacy) = &self.legacy else {
            return Ok(());
        };
        match inspect_credential(&legacy.credential) {
            NativeCredentialStatus::Missing => {
                let unconfigured_database =
                    self.root.join(UNCONFIGURED_DIRECTORY).join(DATABASE_FILE);
                move_sqlite_family_if_present(&legacy.database, &unconfigured_database)?;
                Ok(())
            }
            NativeCredentialStatus::Invalid { .. } => Ok(()),
            NativeCredentialStatus::Active { summary } => {
                let profile_key = validate_profile_key(&summary.device_id)?;
                if let Some(active) = read_active_profile(&self.root)?
                    && active != profile_key
                {
                    return Err(NativeApplianceError::Storage {
                        reason: format!(
                            "legacy credential identifies {profile_key}, but active profile is {active}"
                        ),
                    });
                }

                let paths = self.profile_paths(&profile_key);
                create_private_directory(
                    paths
                        .credential
                        .parent()
                        .expect("profile credential has a directory"),
                    "device profile",
                )?;
                let legacy_bytes = read_credential_bytes(&legacy.credential)
                    .map_err(|reason| NativeApplianceError::Storage { reason })?;
                match inspect_credential(&paths.credential) {
                    NativeCredentialStatus::Missing => {
                        install_credential(
                            &paths.credential,
                            &legacy_bytes[..],
                            CredentialImportPolicy::AnyDevice,
                        )?;
                    }
                    NativeCredentialStatus::Active {
                        summary: existing_summary,
                    } => {
                        let existing = read_credential_bytes(&paths.credential)
                            .map_err(|reason| NativeApplianceError::Storage { reason })?;
                        if existing.as_slice() != legacy_bytes.as_slice()
                            || existing_summary != summary
                        {
                            return Err(NativeApplianceError::Storage {
                                reason: "legacy credential conflicts with its device profile"
                                    .to_owned(),
                            });
                        }
                    }
                    NativeCredentialStatus::Invalid { reason } => {
                        return Err(NativeApplianceError::Storage {
                            reason: format!(
                                "legacy credential target profile is invalid: {reason}"
                            ),
                        });
                    }
                }

                move_sqlite_family_if_present(&legacy.database, &paths.database)?;
                write_active_profile(&self.root, &profile_key)
                    .map_err(ProfileMetadataError::into_native)?;
                fs::remove_file(&legacy.credential)
                    .map_err(storage_error("remove migrated legacy credential"))?;
                sync_directory(
                    legacy
                        .credential
                        .parent()
                        .expect("validated legacy credential has a directory"),
                    "legacy credential directory",
                )?;
                Ok(())
            }
        }
    }

    fn validate_active_profile_if_present(&self) -> Result<(), NativeApplianceError> {
        if let Some(profile_key) = read_active_profile(&self.root)? {
            self.profile_summary_locked(&profile_key)?;
        }
        Ok(())
    }
}

fn profile_error_as_import(error: NativeApplianceError) -> CredentialImportError {
    CredentialImportError::Rejected {
        reason: error.to_string(),
    }
}

fn validated_absolute_path(value: &str, label: &str) -> Result<PathBuf, NativeApplianceError> {
    if value.is_empty() {
        return Err(NativeApplianceError::InvalidArgument {
            reason: format!("{label} must not be empty"),
        });
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(NativeApplianceError::InvalidArgument {
            reason: format!("{label} must be absolute"),
        });
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(NativeApplianceError::InvalidArgument {
            reason: format!("{label} must be lexically normalized"),
        });
    }
    Ok(path)
}

fn validate_profile_key(value: &str) -> Result<String, NativeApplianceError> {
    if value.len() != DEVICE_ID_HEX_BYTES
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(NativeApplianceError::InvalidArgument {
            reason: "device ID must be exactly 32 lowercase hexadecimal digits".to_owned(),
        });
    }
    if value.as_bytes().iter().all(|byte| *byte == b'0') {
        return Err(NativeApplianceError::InvalidArgument {
            reason: "device ID must not be all zeroes".to_owned(),
        });
    }
    Ok(value.to_owned())
}

fn read_active_profile(root: &Path) -> Result<Option<String>, NativeApplianceError> {
    let path = root.join(ACTIVE_PROFILE_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage_error("inspect active profile metadata")(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(NativeApplianceError::Storage {
            reason: "active profile metadata must be a regular non-symlink file".to_owned(),
        });
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(NativeApplianceError::Storage {
            reason: "active profile metadata must not grant group or other permissions".to_owned(),
        });
    }
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(error) => return Err(storage_error("open active profile metadata")(error)),
    };
    let mut bytes = Vec::with_capacity(ACTIVE_PROFILE_MAGIC.len() + DEVICE_ID_HEX_BYTES + 1);
    file.read_to_end(&mut bytes)
        .map_err(storage_error("read active profile metadata"))?;
    if !bytes.starts_with(ACTIVE_PROFILE_MAGIC)
        || bytes.len() != ACTIVE_PROFILE_MAGIC.len() + DEVICE_ID_HEX_BYTES + 1
        || bytes.last() != Some(&b'\n')
    {
        return Err(NativeApplianceError::Storage {
            reason: "active profile metadata has an unsupported or malformed format".to_owned(),
        });
    }
    let key = std::str::from_utf8(
        &bytes[ACTIVE_PROFILE_MAGIC.len()..ACTIVE_PROFILE_MAGIC.len() + DEVICE_ID_HEX_BYTES],
    )
    .map_err(|_| NativeApplianceError::Storage {
        reason: "active profile metadata device ID is not UTF-8".to_owned(),
    })?;
    validate_profile_key(key)
        .map(Some)
        .map_err(|_| NativeApplianceError::Storage {
            reason: "active profile metadata contains a non-canonical device ID".to_owned(),
        })
}

fn write_active_profile(root: &Path, profile_key: &str) -> Result<(), ProfileMetadataError> {
    let profile_key =
        validate_profile_key(profile_key).map_err(|error| ProfileMetadataError::Rejected {
            reason: error.to_string(),
        })?;
    let destination = root.join(ACTIVE_PROFILE_FILE);
    let mut staging = None;
    for _ in 0..METADATA_STAGING_ATTEMPTS {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).map_err(|error| ProfileMetadataError::Rejected {
            reason: format!("could not generate active-profile staging name: {error}"),
        })?;
        let path = root.join(format!(".active-profile-{}", hex::encode(nonce)));
        match secure_create_new(&path) {
            Ok(file) => {
                staging = Some((file, path));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(ProfileMetadataError::Rejected {
                    reason: format!("could not create active profile staging file: {error}"),
                });
            }
        }
    }
    let (mut file, staging_path) = staging.ok_or_else(|| ProfileMetadataError::Rejected {
        reason: "could not allocate an active profile staging file".to_owned(),
    })?;
    if let Err(error) = file
        .write_all(ACTIVE_PROFILE_MAGIC)
        .and_then(|_| file.write_all(profile_key.as_bytes()))
        .and_then(|_| file.write_all(b"\n"))
    {
        drop(file);
        let _ = fs::remove_file(staging_path);
        return Err(ProfileMetadataError::Rejected {
            reason: format!("could not write active profile staging file: {error}"),
        });
    }
    if let Err(error) = file.sync_all() {
        drop(file);
        let _ = fs::remove_file(staging_path);
        return Err(ProfileMetadataError::Rejected {
            reason: format!("could not synchronize active profile staging file: {error}"),
        });
    }
    drop(file);
    if let Err(error) = fs::rename(&staging_path, &destination) {
        let _ = fs::remove_file(staging_path);
        return Err(ProfileMetadataError::Rejected {
            reason: format!("could not publish active profile metadata: {error}"),
        });
    }
    sync_directory(root, "profile root").map_err(|error| {
        ProfileMetadataError::PublicationUncertain {
            reason: format!(
                "active profile metadata was published but its directory durability is uncertain: {error}"
            ),
        }
    })
}

fn move_sqlite_family_if_present(
    source: &Path,
    destination: &Path,
) -> Result<(), NativeApplianceError> {
    let destination_parent = destination
        .parent()
        .expect("profile database has a directory");
    create_private_directory(destination_parent, "database profile directory")?;
    move_file_if_present(source, destination, "legacy chat database")?;
    for suffix in ["-wal", "-shm"] {
        let source_sidecar = PathBuf::from(format!("{}{suffix}", source.display()));
        let destination_sidecar = PathBuf::from(format!("{}{suffix}", destination.display()));
        move_file_if_present(
            &source_sidecar,
            &destination_sidecar,
            "legacy SQLite sidecar",
        )?;
    }
    sync_directory(destination_parent, "database profile directory")
}

fn move_file_if_present(
    source: &Path,
    destination: &Path,
    label: &str,
) -> Result<(), NativeApplianceError> {
    let source_exists = match fs::symlink_metadata(source) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(NativeApplianceError::Storage {
                    reason: format!("{label} must be a regular non-symlink file"),
                });
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(storage_error("inspect legacy database artifact")(error)),
    };
    let destination_exists = match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(NativeApplianceError::Storage {
                    reason: format!("profile {label} target must be a regular non-symlink file"),
                });
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(storage_error("inspect profile database artifact")(error)),
    };
    match (source_exists, destination_exists) {
        (false, _) => Ok(()),
        (true, false) => fs::rename(source, destination)
            .map_err(storage_error("migrate legacy database artifact")),
        (true, true) => Err(NativeApplianceError::Storage {
            reason: format!(
                "both legacy and per-profile {label} exist; refusing to guess which database is authoritative"
            ),
        }),
    }
}

fn create_private_directory(path: &Path, label: &str) -> Result<(), NativeApplianceError> {
    create_directory_all(path).map_err(storage_error("create private profile directory"))?;
    require_private_directory(path, label)
}

#[cfg(unix)]
fn create_directory_all(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_directory_all(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

fn require_private_directory(path: &Path, label: &str) -> Result<(), NativeApplianceError> {
    let metadata =
        fs::symlink_metadata(path).map_err(storage_error("inspect private profile directory"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(NativeApplianceError::Storage {
            reason: format!("{label} must be a real directory"),
        });
    }
    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(NativeApplianceError::Storage {
                reason: format!("{label} must not grant group or other permissions"),
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn secure_create_new(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn secure_create_new(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn sync_directory(path: &Path, label: &str) -> Result<(), NativeApplianceError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| NativeApplianceError::Storage {
            reason: format!("could not synchronize {label}: {error}"),
        })
}

fn storage_error(operation: &'static str) -> impl FnOnce(std::io::Error) -> NativeApplianceError {
    move |error| NativeApplianceError::Storage {
        reason: format!("could not {operation}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "reticulum-mobile-profile-{label}-{}-{sequence}",
                std::process::id()
            ));
            Self(path)
        }

        fn path_string(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn activated_credential_bytes(
        device_id: [u8; 16],
        psk_byte: u8,
    ) -> [u8; reticulum_device_client::ACTIVATED_CREDENTIAL_STATE_BYTES] {
        let mut bytes = [0_u8; reticulum_device_client::ACTIVATED_CREDENTIAL_STATE_BYTES];
        bytes[..8].copy_from_slice(b"RDPKEY1\0");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        bytes[10] = 2;
        bytes[16..32].copy_from_slice(&device_id);
        bytes[32..48].fill(0x42);
        bytes[48..56].copy_from_slice(&7_u64.to_le_bytes());
        bytes[56..88].fill(psk_byte);
        bytes
    }

    fn open_store(directory: &TestDirectory) -> Arc<NativeProfileStore> {
        NativeProfileStore::open(directory.path_string(), None, None).unwrap()
    }

    #[test]
    fn only_post_publication_metadata_failures_are_reconcilable() {
        assert!(matches!(
            ProfileMetadataError::Rejected {
                reason: "staging write failed".to_owned()
            }
            .into_import(),
            CredentialImportError::Rejected { .. }
        ));
        assert!(matches!(
            ProfileMetadataError::PublicationUncertain {
                reason: "directory sync failed".to_owned()
            }
            .into_import(),
            CredentialImportError::PublicationUncertain { .. }
        ));
    }

    #[test]
    fn imported_profiles_are_keyed_by_validated_device_id_and_switch_explicitly() {
        let directory = TestDirectory::new("multiple");
        let store = open_store(&directory);
        let first_id = *b"device-prof-0001";
        let second_id = *b"device-prof-0002";
        let first_path = directory.0.join("first-import.rdpkey");
        let second_path = directory.0.join("second-import.rdpkey");
        fs::write(&first_path, activated_credential_bytes(first_id, 0x24)).unwrap();
        fs::write(&second_path, activated_credential_bytes(second_id, 0x25)).unwrap();

        let first = store
            .import_activated_credential(&first_path, CredentialImportPolicy::AnyDevice)
            .unwrap();
        let first_paths = store.runtime_paths().unwrap();
        let second = store
            .import_activated_credential(&second_path, CredentialImportPolicy::AnyDevice)
            .unwrap();
        let second_paths = store.runtime_paths().unwrap();

        assert_ne!(first.device_id, second.device_id);
        assert_ne!(first_paths.database, second_paths.database);
        assert_ne!(first_paths.credential, second_paths.credential);
        let snapshot = store.snapshot().unwrap();
        assert_eq!(
            snapshot.active_profile_key.as_deref(),
            Some(second.device_id.as_str())
        );
        assert_eq!(snapshot.profiles.len(), 2);
        assert_eq!(
            snapshot
                .profiles
                .iter()
                .map(|profile| profile.profile_key.as_str())
                .collect::<Vec<_>>(),
            vec![first.device_id.as_str(), second.device_id.as_str()]
        );

        store.activate_profile(first.device_id.clone()).unwrap();
        assert_eq!(
            store.runtime_paths().unwrap().database,
            first_paths.database
        );
        assert_eq!(
            store.snapshot().unwrap().active_profile_key.as_deref(),
            Some(first.device_id.as_str())
        );

        fs::remove_file(first_path).unwrap();
        fs::remove_file(second_path).unwrap();
    }

    #[test]
    fn profile_activation_rejects_noncanonical_or_unknown_device_ids() {
        let directory = TestDirectory::new("invalid-key");
        let store = open_store(&directory);

        for invalid in [
            "ab",
            "ABABABABABABABABABABABABABABABAB",
            "00000000000000000000000000000000",
            "gggggggggggggggggggggggggggggggg",
        ] {
            assert!(matches!(
                store.activate_profile(invalid.to_owned()),
                Err(NativeApplianceError::InvalidArgument { .. })
            ));
        }
        assert!(matches!(
            store.activate_profile("ab".repeat(16)),
            Err(NativeApplianceError::Storage { .. })
        ));
    }

    #[test]
    fn legacy_single_profile_is_migrated_without_exposing_or_rewriting_secret_bytes() {
        let container = TestDirectory::new("legacy");
        create_directory_all(&container.0).unwrap();
        let root = container.0.join("new-profiles");
        let legacy_database = container
            .0
            .join("reticulum-lxmf-chat-alpha-schema3.sqlite3");
        let legacy_credential = container.0.join("reticulum-device-credential.rdpkey");
        let bytes = activated_credential_bytes(*b"legacy-device-01", 0x31);
        fs::write(&legacy_database, b"legacy database").unwrap();
        fs::write(
            PathBuf::from(format!("{}-wal", legacy_database.display())),
            b"legacy wal",
        )
        .unwrap();
        fs::write(
            PathBuf::from(format!("{}-shm", legacy_database.display())),
            b"legacy shm",
        )
        .unwrap();
        fs::write(&legacy_credential, bytes).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&legacy_credential, fs::Permissions::from_mode(0o600)).unwrap();

        let store = NativeProfileStore::open(
            root.to_string_lossy().into_owned(),
            Some(legacy_database.to_string_lossy().into_owned()),
            Some(legacy_credential.to_string_lossy().into_owned()),
        )
        .unwrap();
        let NativeCredentialStatus::Active { summary } = store.credential_status().unwrap() else {
            panic!("migrated credential must be active");
        };
        let paths = store.runtime_paths().unwrap();

        assert_eq!(summary.device_id, hex::encode(*b"legacy-device-01"));
        assert_eq!(fs::read(&paths.credential).unwrap(), bytes);
        assert_eq!(fs::read(&paths.database).unwrap(), b"legacy database");
        assert_eq!(
            fs::read(PathBuf::from(format!("{}-wal", paths.database.display()))).unwrap(),
            b"legacy wal"
        );
        assert_eq!(
            fs::read(PathBuf::from(format!("{}-shm", paths.database.display()))).unwrap(),
            b"legacy shm"
        );
        assert!(!legacy_credential.exists());
        assert!(!legacy_database.exists());
        assert_eq!(
            store.snapshot().unwrap().active_profile_key,
            Some(summary.device_id)
        );
    }

    #[test]
    fn duplicate_import_is_idempotent_but_different_secret_cannot_replace_profile() {
        let directory = TestDirectory::new("duplicate");
        let store = open_store(&directory);
        let staging = directory.0.join("duplicate-import.rdpkey");
        let original = activated_credential_bytes(*b"duplicate-dev-01", 0x41);
        fs::write(&staging, original).unwrap();
        let summary = store
            .import_activated_credential(&staging, CredentialImportPolicy::AnyDevice)
            .unwrap();
        assert_eq!(
            store
                .import_activated_credential(&staging, CredentialImportPolicy::AnyDevice)
                .unwrap(),
            summary
        );

        fs::write(
            &staging,
            activated_credential_bytes(*b"duplicate-dev-01", 0x42),
        )
        .unwrap();
        assert!(matches!(
            store.import_activated_credential(&staging, CredentialImportPolicy::AnyDevice),
            Err(CredentialImportError::Rejected { reason })
                if reason.contains("different credential")
        ));
        fs::remove_file(staging).unwrap();
    }

    #[test]
    fn invalid_legacy_credential_preserves_existing_recovery_boundary() {
        let container = TestDirectory::new("invalid-legacy");
        create_directory_all(&container.0).unwrap();
        let root = container.0.join("new-profiles");
        let legacy_database = container.0.join("legacy.sqlite3");
        let legacy_credential = container.0.join("legacy.rdpkey");
        fs::write(&legacy_database, b"legacy database").unwrap();
        fs::write(&legacy_credential, b"not a credential").unwrap();

        let store = NativeProfileStore::open(
            root.to_string_lossy().into_owned(),
            Some(legacy_database.to_string_lossy().into_owned()),
            Some(legacy_credential.to_string_lossy().into_owned()),
        )
        .unwrap();
        assert!(matches!(
            store.credential_status().unwrap(),
            NativeCredentialStatus::Invalid { .. }
        ));
        assert_eq!(store.runtime_paths().unwrap().database, legacy_database);
        assert!(legacy_credential.exists());
        assert!(legacy_database.exists());
    }

    #[test]
    fn credential_free_legacy_database_moves_to_the_unconfigured_profile() {
        let container = TestDirectory::new("unconfigured-legacy");
        create_directory_all(&container.0).unwrap();
        let root = container.0.join("new-profiles");
        let legacy_database = container.0.join("legacy.sqlite3");
        let legacy_credential = container.0.join("missing.rdpkey");
        fs::write(&legacy_database, b"credential-free database").unwrap();

        let store = NativeProfileStore::open(
            root.to_string_lossy().into_owned(),
            Some(legacy_database.to_string_lossy().into_owned()),
            Some(legacy_credential.to_string_lossy().into_owned()),
        )
        .unwrap();
        let paths = store.runtime_paths().unwrap();

        assert_eq!(
            store.credential_status().unwrap(),
            NativeCredentialStatus::Missing
        );
        assert_eq!(
            fs::read(paths.database).unwrap(),
            b"credential-free database"
        );
        assert!(!legacy_database.exists());
    }

    #[test]
    fn malformed_active_metadata_fails_closed_instead_of_selecting_a_profile() {
        let directory = TestDirectory::new("bad-active");
        let store = open_store(&directory);
        drop(store);
        let metadata = directory.0.join(ACTIVE_PROFILE_FILE);
        fs::write(&metadata, b"not active metadata").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&metadata, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(
            NativeProfileStore::open(directory.path_string(), None, None),
            Err(NativeApplianceError::Storage { reason })
                if reason.contains("unsupported or malformed")
        ));
    }
}
