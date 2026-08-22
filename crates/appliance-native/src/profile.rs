//! App-private mobile profiles keyed by Reticulum management destination.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::appliance::NativeApplianceError;

const PROFILES_DIRECTORY: &str = "profiles";
const UNCONFIGURED_DIRECTORY: &str = "unconfigured";
const DATABASE_FILE: &str = "chat.sqlite3";
const DATABASE_WAL_FILE: &str = "chat.sqlite3-wal";
const DATABASE_SHM_FILE: &str = "chat.sqlite3-shm";
const PROFILE_METADATA_FILE: &str = "reticulum-profile-v1";
const PROFILE_METADATA_MAGIC_V1: &str = "RETICULUM-APPLIANCE-PROFILE-1";
const PROFILE_METADATA_MAGIC_V2: &str = "RETICULUM-APPLIANCE-PROFILE-2";
const ACTIVE_PROFILE_FILE: &str = "active-reticulum-profile-v1";
const ACTIVE_PROFILE_MAGIC: &str = "RETICULUM-APPLIANCE-ACTIVE-RETICULUM-PROFILE-1\n";
const DESTINATION_HEX_BYTES: usize = 32;
const METADATA_MAX_BYTES: u64 = 256;

/// Public facts for one saved appliance management application.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativeProfileSummary {
    /// Canonical profile key, equal to the management destination hash.
    pub profile_key: String,
    /// Canonical Reticulum management destination hash.
    pub management_destination: String,
    /// Canonical Reticulum LXMF delivery destination hash.
    pub lxmf_destination: String,
    /// Last authorized product-owned appliance label read from this node.
    pub appliance_label: Option<String>,
}

/// Secret-free projection of the management-destination profile store.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativeProfileStoreSnapshot {
    /// Canonical key selected for the active application session.
    pub active_profile_key: Option<String>,
    /// All validated profiles, sorted by canonical key.
    pub profiles: Vec<NativeProfileSummary>,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeProfileRuntimePaths {
    pub(crate) database: PathBuf,
    pub(crate) management_destination: Option<[u8; 16]>,
}

/// Native owner for management-destination keyed SQLite profile paths.
///
/// Enrollment is ordinary Reticulum identified-Link authorization. This store
/// contains no bearer credential, Bluetooth bond, or duplicate identity.
#[derive(uniffi::Object)]
pub struct NativeProfileStore {
    root: PathBuf,
    gate: Mutex<()>,
}

#[uniffi::export]
impl NativeProfileStore {
    /// Open or create an app-private profile root.
    #[uniffi::constructor]
    pub fn open(root_directory: String) -> Result<Arc<Self>, NativeApplianceError> {
        let root = validated_absolute_path(&root_directory, "profile root")?;
        create_private_directory(&root, "profile root")?;
        create_private_directory(&root.join(PROFILES_DIRECTORY), "profiles directory")?;
        create_private_directory(
            &root.join(UNCONFIGURED_DIRECTORY),
            "unconfigured profile directory",
        )?;
        let store = Arc::new(Self {
            root,
            gate: Mutex::new(()),
        });
        {
            let _guard = store.lock_gate()?;
            if let Some(active) = read_active_profile(&store.root)? {
                store.profile_summary_locked(&active)?;
            }
        }
        Ok(store)
    }

    /// Return all validated profiles and the selected profile key.
    pub fn snapshot(&self) -> Result<NativeProfileStoreSnapshot, NativeApplianceError> {
        let _guard = self.lock_gate()?;
        self.snapshot_locked()
    }

    /// Idempotently remember and select one verified appliance application.
    ///
    /// Callers invoke this only after the public identity request has matched
    /// `management_destination` and an identified request has demonstrated
    /// authorization or completed physical-presence enrollment.
    pub fn remember_profile(
        &self,
        management_destination: String,
        lxmf_destination: String,
    ) -> Result<NativeProfileSummary, NativeApplianceError> {
        let management_destination =
            validate_destination(&management_destination, "management destination")?;
        let lxmf_destination = validate_destination(&lxmf_destination, "LXMF destination")?;
        let summary = NativeProfileSummary {
            profile_key: management_destination.clone(),
            management_destination,
            lxmf_destination,
            appliance_label: None,
        };
        let _guard = self.lock_gate()?;
        let directory = self.profile_directory(&summary.profile_key);
        create_private_directory(&directory, "Reticulum profile directory")?;
        let metadata = directory.join(PROFILE_METADATA_FILE);
        if metadata.exists() {
            let existing = read_profile_metadata(&metadata)?;
            if existing.management_destination != summary.management_destination
                || existing.lxmf_destination != summary.lxmf_destination
            {
                return Err(NativeApplianceError::Storage {
                    reason: "saved management destination has conflicting application metadata"
                        .to_owned(),
                });
            }
            write_active_profile(&self.root, &existing.profile_key)?;
            return Ok(existing);
        } else {
            write_profile_metadata(&metadata, &summary)?;
        }
        write_active_profile(&self.root, &summary.profile_key)?;
        Ok(summary)
    }

    /// Replace the cached label for the active authorized appliance profile.
    pub fn update_active_appliance_label(
        &self,
        appliance_label: Option<String>,
    ) -> Result<NativeProfileSummary, NativeApplianceError> {
        let appliance_label = appliance_label
            .map(|label| validate_appliance_label(&label).map(str::to_owned))
            .transpose()?;
        let _guard = self.lock_gate()?;
        let profile_key =
            read_active_profile(&self.root)?.ok_or_else(|| NativeApplianceError::Storage {
                reason: "no active appliance profile exists for a label update".to_owned(),
            })?;
        let mut summary = self.profile_summary_locked(&profile_key)?;
        if summary.appliance_label != appliance_label {
            summary.appliance_label = appliance_label;
            write_profile_metadata(
                &self
                    .profile_directory(&profile_key)
                    .join(PROFILE_METADATA_FILE),
                &summary,
            )?;
        }
        Ok(summary)
    }

    /// Select one existing validated Reticulum profile.
    pub fn activate_profile(
        &self,
        profile_key: String,
    ) -> Result<NativeProfileSummary, NativeApplianceError> {
        let profile_key = validate_destination(&profile_key, "profile key")?;
        let _guard = self.lock_gate()?;
        let profile = self.profile_summary_locked(&profile_key)?;
        write_active_profile(&self.root, &profile_key)?;
        Ok(profile)
    }

    /// Delete one validated inactive profile and its local application data.
    ///
    /// This does not revoke the app identity from the appliance allow-list.
    pub fn delete_inactive_profile(
        &self,
        profile_key: String,
    ) -> Result<NativeProfileStoreSnapshot, NativeApplianceError> {
        let profile_key = validate_destination(&profile_key, "profile key")?;
        let _guard = self.lock_gate()?;
        if read_active_profile(&self.root)?.as_deref() == Some(profile_key.as_str()) {
            return Err(NativeApplianceError::InvalidArgument {
                reason: "the active appliance profile cannot be deleted".to_owned(),
            });
        }
        self.profile_summary_locked(&profile_key)?;
        let directory = self.profile_directory(&profile_key);
        for entry in fs::read_dir(&directory).map_err(storage_error("read profile directory"))? {
            let entry = entry.map_err(storage_error("read profile entry"))?;
            let name =
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| NativeApplianceError::Storage {
                        reason: "profile contains a non-UTF-8 artifact name".to_owned(),
                    })?;
            if !matches!(
                name.as_str(),
                PROFILE_METADATA_FILE | DATABASE_FILE | DATABASE_WAL_FILE | DATABASE_SHM_FILE
            ) {
                return Err(NativeApplianceError::Storage {
                    reason: format!("profile contains unsupported artifact {name}"),
                });
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(storage_error("inspect profile artifact"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(NativeApplianceError::Storage {
                    reason: format!("profile artifact {name} is not a regular file"),
                });
            }
        }
        for name in [
            DATABASE_WAL_FILE,
            DATABASE_SHM_FILE,
            DATABASE_FILE,
            PROFILE_METADATA_FILE,
        ] {
            let path = directory.join(name);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(NativeApplianceError::Storage {
                        reason: format!("could not remove profile artifact {name}: {error}"),
                    });
                }
            }
        }
        fs::remove_dir(&directory).map_err(storage_error("remove empty profile directory"))?;
        sync_directory(&self.root.join(PROFILES_DIRECTORY), "profiles directory")?;
        self.snapshot_locked()
    }
}

impl NativeProfileStore {
    pub(crate) fn runtime_paths(&self) -> Result<NativeProfileRuntimePaths, NativeApplianceError> {
        let _guard = self.lock_gate()?;
        let Some(profile_key) = read_active_profile(&self.root)? else {
            return Ok(NativeProfileRuntimePaths {
                database: self.root.join(UNCONFIGURED_DIRECTORY).join(DATABASE_FILE),
                management_destination: None,
            });
        };
        self.profile_summary_locked(&profile_key)?;
        Ok(NativeProfileRuntimePaths {
            database: self.profile_directory(&profile_key).join(DATABASE_FILE),
            management_destination: Some(decode_destination(&profile_key)?),
        })
    }

    fn snapshot_locked(&self) -> Result<NativeProfileStoreSnapshot, NativeApplianceError> {
        let mut profiles = Vec::new();
        let profiles_root = self.root.join(PROFILES_DIRECTORY);
        for entry in fs::read_dir(&profiles_root).map_err(storage_error("read profiles"))? {
            let entry = entry.map_err(storage_error("read profile entry"))?;
            let metadata = entry
                .metadata()
                .map_err(storage_error("inspect profile entry"))?;
            if !metadata.is_dir() {
                return Err(NativeApplianceError::Storage {
                    reason: "profiles directory contains a non-directory entry".to_owned(),
                });
            }
            let key =
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| NativeApplianceError::Storage {
                        reason: "profiles directory contains a non-UTF-8 entry".to_owned(),
                    })?;
            validate_destination(&key, "stored profile key")?;
            profiles.push(self.profile_summary_locked(&key)?);
        }
        profiles.sort_by(|left, right| left.profile_key.cmp(&right.profile_key));
        let active_profile_key = read_active_profile(&self.root)?;
        if let Some(active) = active_profile_key.as_deref()
            && !profiles.iter().any(|profile| profile.profile_key == active)
        {
            return Err(NativeApplianceError::Storage {
                reason: "active profile metadata names a missing profile".to_owned(),
            });
        }
        Ok(NativeProfileStoreSnapshot {
            active_profile_key,
            profiles,
        })
    }

    fn profile_summary_locked(
        &self,
        profile_key: &str,
    ) -> Result<NativeProfileSummary, NativeApplianceError> {
        let summary = read_profile_metadata(
            &self
                .profile_directory(profile_key)
                .join(PROFILE_METADATA_FILE),
        )?;
        if summary.profile_key != profile_key || summary.management_destination != profile_key {
            return Err(NativeApplianceError::Storage {
                reason: "profile metadata does not match its directory key".to_owned(),
            });
        }
        Ok(summary)
    }

    fn profile_directory(&self, profile_key: &str) -> PathBuf {
        self.root.join(PROFILES_DIRECTORY).join(profile_key)
    }

    fn lock_gate(&self) -> Result<MutexGuard<'_, ()>, NativeApplianceError> {
        self.gate
            .lock()
            .map_err(|_| NativeApplianceError::Internal {
                reason: "profile store lock is poisoned".to_owned(),
            })
    }
}

fn validated_absolute_path(path: &str, label: &str) -> Result<PathBuf, NativeApplianceError> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(NativeApplianceError::InvalidArgument {
            reason: format!("{label} must be absolute"),
        });
    }
    Ok(path)
}

fn validate_destination(value: &str, label: &str) -> Result<String, NativeApplianceError> {
    if value.len() != DESTINATION_HEX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(NativeApplianceError::InvalidArgument {
            reason: format!("{label} must be 32 lowercase hexadecimal characters"),
        });
    }
    Ok(value.to_owned())
}

fn validate_appliance_label(value: &str) -> Result<&str, NativeApplianceError> {
    if value.is_empty()
        || value.len() > 32
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        return Err(NativeApplianceError::InvalidArgument {
            reason: "appliance label must contain between 1 and 32 UTF-8 bytes without control characters"
                .to_owned(),
        });
    }
    Ok(value)
}

fn decode_destination(value: &str) -> Result<[u8; 16], NativeApplianceError> {
    let mut decoded = [0; 16];
    hex::decode_to_slice(value, &mut decoded).map_err(|error| NativeApplianceError::Storage {
        reason: format!("stored profile destination could not be decoded: {error}"),
    })?;
    Ok(decoded)
}

fn create_private_directory(path: &Path, label: &str) -> Result<(), NativeApplianceError> {
    fs::create_dir_all(path).map_err(storage_error("create private directory"))?;
    let metadata =
        fs::symlink_metadata(path).map_err(storage_error("inspect private directory"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(NativeApplianceError::Storage {
            reason: format!("{label} must be a real directory"),
        });
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(storage_error("set private directory permissions"))?;
    Ok(())
}

fn read_profile_metadata(path: &Path) -> Result<NativeProfileSummary, NativeApplianceError> {
    let text = read_bounded_file(path, "profile metadata")?;
    let text = text
        .strip_suffix('\n')
        .ok_or_else(|| NativeApplianceError::Storage {
            reason: "profile metadata has an unsupported format".to_owned(),
        })?;
    let mut fields = text.split('\n');
    let magic = fields.next().ok_or_else(|| NativeApplianceError::Storage {
        reason: "profile metadata has an unsupported format".to_owned(),
    })?;
    if !matches!(magic, PROFILE_METADATA_MAGIC_V1 | PROFILE_METADATA_MAGIC_V2) {
        return Err(NativeApplianceError::Storage {
            reason: "profile metadata has an unsupported format".to_owned(),
        });
    }
    let management = fields.next().ok_or_else(|| NativeApplianceError::Storage {
        reason: "profile metadata is missing its management destination".to_owned(),
    })?;
    let lxmf = fields.next().ok_or_else(|| NativeApplianceError::Storage {
        reason: "profile metadata is missing its LXMF destination".to_owned(),
    })?;
    let appliance_label = if magic == PROFILE_METADATA_MAGIC_V2 {
        fields
            .next()
            .map(|label| {
                if label.is_empty() {
                    Ok(None)
                } else {
                    validate_appliance_label(label).map(|label| Some(label.to_owned()))
                }
            })
            .transpose()?
            .flatten()
    } else {
        None
    };
    if fields.next().is_some() {
        return Err(NativeApplianceError::Storage {
            reason: "profile metadata contains trailing fields".to_owned(),
        });
    }
    let management_destination = validate_destination(management, "stored management destination")?;
    let lxmf_destination = validate_destination(lxmf, "stored LXMF destination")?;
    Ok(NativeProfileSummary {
        profile_key: management_destination.clone(),
        management_destination,
        lxmf_destination,
        appliance_label,
    })
}

fn write_profile_metadata(
    path: &Path,
    summary: &NativeProfileSummary,
) -> Result<(), NativeApplianceError> {
    let body = format!(
        "{PROFILE_METADATA_MAGIC_V2}\n{}\n{}\n{}\n",
        summary.management_destination,
        summary.lxmf_destination,
        summary.appliance_label.as_deref().unwrap_or("")
    );
    write_atomic(path, body.as_bytes(), "profile metadata")
}

fn read_active_profile(root: &Path) -> Result<Option<String>, NativeApplianceError> {
    let path = root.join(ACTIVE_PROFILE_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text = read_bounded_file(&path, "active profile metadata")?;
    let value = text
        .strip_prefix(ACTIVE_PROFILE_MAGIC)
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or_else(|| NativeApplianceError::Storage {
            reason: "active profile metadata has an unsupported format".to_owned(),
        })?;
    validate_destination(value, "active profile key").map(Some)
}

fn write_active_profile(root: &Path, profile_key: &str) -> Result<(), NativeApplianceError> {
    let body = format!("{ACTIVE_PROFILE_MAGIC}{profile_key}\n");
    write_atomic(
        &root.join(ACTIVE_PROFILE_FILE),
        body.as_bytes(),
        "active profile metadata",
    )
}

fn read_bounded_file(path: &Path, label: &str) -> Result<String, NativeApplianceError> {
    let metadata = fs::symlink_metadata(path).map_err(storage_error("inspect metadata file"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > METADATA_MAX_BYTES
    {
        return Err(NativeApplianceError::Storage {
            reason: format!("{label} must be a small regular file"),
        });
    }
    let mut file = File::open(path).map_err(storage_error("open metadata file"))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(storage_error("read metadata file"))?;
    Ok(text)
}

fn write_atomic(path: &Path, bytes: &[u8], label: &str) -> Result<(), NativeApplianceError> {
    let parent = path.parent().ok_or_else(|| NativeApplianceError::Storage {
        reason: format!("{label} has no parent directory"),
    })?;
    let mut nonce = [0; 8];
    getrandom::fill(&mut nonce).map_err(|error| NativeApplianceError::Storage {
        reason: format!("could not generate {label} staging name: {error}"),
    })?;
    let staging = parent.join(format!(".profile-{}.tmp", hex::encode(nonce)));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&staging)
        .map_err(storage_error("create metadata staging file"))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(storage_error("write metadata staging file"))?;
        file.sync_all()
            .map_err(storage_error("sync metadata staging file"))?;
        fs::rename(&staging, path).map_err(storage_error("publish metadata file"))?;
        sync_directory(parent, label)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
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
        reason: format!("{operation}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "reticulum-prns-profiles-{}-{nonce}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn open_store(directory: &TestDirectory) -> Arc<NativeProfileStore> {
        NativeProfileStore::open(directory.0.to_string_lossy().into_owned()).unwrap()
    }

    #[test]
    fn verified_destinations_are_remembered_without_bearer_credentials() {
        let directory = TestDirectory::new();
        let store = open_store(&directory);
        let management = "11".repeat(16);
        let lxmf = "22".repeat(16);
        let profile = store
            .remember_profile(management.clone(), lxmf.clone())
            .unwrap();
        assert_eq!(profile.profile_key, management);
        assert_eq!(profile.lxmf_destination, lxmf);
        assert_eq!(store.snapshot().unwrap().profiles, vec![profile]);
    }

    #[test]
    fn runtime_targets_the_selected_management_destination() {
        let directory = TestDirectory::new();
        let store = open_store(&directory);
        let management = "ab".repeat(16);
        store
            .remember_profile(management.clone(), "cd".repeat(16))
            .unwrap();
        let paths = store.runtime_paths().unwrap();
        assert_eq!(paths.management_destination, Some([0xab; 16]));
        assert!(paths.database.ends_with(DATABASE_FILE));
    }

    #[test]
    fn one_store_can_select_any_number_of_appliance_profiles() {
        let directory = TestDirectory::new();
        let store = open_store(&directory);
        let first = store
            .remember_profile("01".repeat(16), "11".repeat(16))
            .unwrap();
        let second = store
            .remember_profile("02".repeat(16), "22".repeat(16))
            .unwrap();
        assert_eq!(store.snapshot().unwrap().profiles.len(), 2);
        store.activate_profile(first.profile_key.clone()).unwrap();
        assert_eq!(
            store.snapshot().unwrap().active_profile_key,
            Some(first.profile_key.clone())
        );
        assert_ne!(first, second);
    }

    #[test]
    fn conflicting_metadata_cannot_replace_a_saved_destination() {
        let directory = TestDirectory::new();
        let store = open_store(&directory);
        let management = "33".repeat(16);
        store
            .remember_profile(management.clone(), "44".repeat(16))
            .unwrap();
        assert!(store.remember_profile(management, "55".repeat(16)).is_err());
    }

    #[test]
    fn appliance_label_cache_survives_reopen_and_profile_refresh() {
        let directory = TestDirectory::new();
        let management = "66".repeat(16);
        let lxmf = "77".repeat(16);
        let store = open_store(&directory);
        store
            .remember_profile(management.clone(), lxmf.clone())
            .unwrap();
        let labeled = store
            .update_active_appliance_label(Some("North node".to_owned()))
            .unwrap();
        assert_eq!(labeled.appliance_label.as_deref(), Some("North node"));
        assert_eq!(
            store
                .remember_profile(management, lxmf)
                .unwrap()
                .appliance_label
                .as_deref(),
            Some("North node")
        );
        drop(store);

        let reopened = open_store(&directory);
        assert_eq!(
            reopened.snapshot().unwrap().profiles[0]
                .appliance_label
                .as_deref(),
            Some("North node")
        );
        assert!(
            reopened
                .update_active_appliance_label(Some("line\u{2028}break".to_owned()))
                .is_err()
        );
        assert_eq!(
            reopened
                .update_active_appliance_label(None)
                .unwrap()
                .appliance_label,
            None
        );
    }
}
