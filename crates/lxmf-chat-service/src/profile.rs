//! Owner-private host profile paths for one physical Reticulum appliance.

use std::fs::{self, File, OpenOptions, symlink_metadata};
use std::path::{Path, PathBuf};

use crate::normalize_usb_serial;

const DEVICES_DIRECTORY: &str = "devices";
const CREDENTIAL_FILE: &str = "credential.rdpkey";
const DATABASE_FILE: &str = "chat.sqlite3";

/// Owner-private root containing one profile directory per stable USB serial.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileRoot {
    path: PathBuf,
}

impl ProfileRoot {
    /// Create or validate a private profile root.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        ensure_secure_profile_host()?;
        let path = path.into();
        if !path.is_absolute() {
            return Err("managed profile root must be an absolute path".to_owned());
        }
        if path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        }) {
            return Err("managed profile root must be lexically normalized".to_owned());
        }
        reject_existing_symlink_ancestors(&path)?;
        create_private_directory(&path, "profile root")?;
        reject_symlink_ancestors(&path)?;
        let devices = path.join(DEVICES_DIRECTORY);
        create_private_directory(&devices, "profile devices directory")?;
        reject_symlink_ancestors(&devices)?;
        Ok(Self { path })
    }

    /// Root path selected by the operator.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the normalized serials with existing profile directories.
    pub fn existing_usb_serials(&self) -> Result<Vec<String>, String> {
        let devices = self.path.join(DEVICES_DIRECTORY);
        reject_symlink_ancestors(&devices)?;
        require_private_directory(&devices, "profile devices directory")?;
        let mut serials = Vec::new();
        for entry in fs::read_dir(&devices)
            .map_err(|error| format!("could not read {}: {error}", devices.display()))?
        {
            let entry = entry.map_err(|error| {
                format!("could not read an entry in {}: {error}", devices.display())
            })?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| format!("{} contains a non-UTF-8 profile name", devices.display()))?;
            let normalized = normalize_usb_serial(&name).ok_or_else(|| {
                format!(
                    "{} contains invalid profile directory {name}",
                    devices.display()
                )
            })?;
            if normalized != name {
                return Err(format!(
                    "{} contains non-canonical profile directory {name}",
                    devices.display()
                ));
            }
            require_private_directory(&entry.path(), "device profile")?;
            serials.push(normalized);
        }
        serials.sort_unstable();
        Ok(serials)
    }

    /// Create or reopen the profile for one exact descriptor serial.
    pub fn device(&self, usb_serial: &str) -> Result<DeviceProfile, String> {
        let usb_serial = normalize_usb_serial(usb_serial).ok_or_else(|| {
            "USB serial must contain exactly twelve hexadecimal digits".to_owned()
        })?;
        let directory = self.path.join(DEVICES_DIRECTORY).join(&usb_serial);
        reject_existing_symlink_ancestors(&directory)?;
        create_private_directory(&directory, "device profile")?;
        reject_symlink_ancestors(&directory)?;
        Ok(DeviceProfile {
            usb_serial,
            directory,
        })
    }
}

fn reject_existing_symlink_ancestors(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        match symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "managed profile root must not traverse symlink {}",
                    ancestor.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect profile ancestor {}: {error}",
                    ancestor.display()
                ));
            }
        }
    }
    Ok(())
}

fn reject_symlink_ancestors(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        let metadata = symlink_metadata(ancestor).map_err(|error| {
            format!(
                "could not inspect profile ancestor {}: {error}",
                ancestor.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "managed profile root must not traverse symlink {}",
                ancestor.display()
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
const fn ensure_secure_profile_host() -> Result<(), String> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_secure_profile_host() -> Result<(), String> {
    Err("managed profiles require implemented owner-only filesystem semantics on this host".into())
}

/// Stable paths owned by one exact physical-device profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceProfile {
    usb_serial: String,
    directory: PathBuf,
}

impl DeviceProfile {
    /// Canonical twelve-digit descriptor serial.
    pub fn usb_serial(&self) -> &str {
        &self.usb_serial
    }

    /// Owner-private device-profile directory.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// All-state pairing credential artifact. This path must never be sent to
    /// the browser or included in ordinary diagnostics.
    pub fn credential_path(&self) -> PathBuf {
        self.directory.join(CREDENTIAL_FILE)
    }

    /// SQLite conversation database bound to the authenticated device.
    pub fn database_path(&self) -> PathBuf {
        self.directory.join(DATABASE_FILE)
    }

    /// Create an empty owner-only database file, or validate the existing one,
    /// before SQLite opens it and creates any same-directory sidecars.
    pub fn prepare_database(&self) -> Result<(), String> {
        reject_symlink_ancestors(&self.directory)?;
        require_private_directory(&self.directory, "device profile")?;
        let path = self.database_path();
        match secure_create_new(&path) {
            Ok(file) => file
                .sync_all()
                .map_err(|error| format!("could not sync {}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                require_private_file(&path, "chat database")
            }
            Err(error) => Err(format!("could not create {}: {error}", path.display())),
        }
    }
}

fn create_private_directory(path: &Path, label: &str) -> Result<(), String> {
    create_directory_all(path)
        .map_err(|error| format!("could not create {label} {}: {error}", path.display()))?;
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

fn require_private_directory(path: &Path, label: &str) -> Result<(), String> {
    let metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect {label} {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{label} {} must be a non-symlink directory",
            path.display()
        ));
    }
    enforce_owner_only(path, &metadata, label)
}

fn require_private_file(path: &Path, label: &str) -> Result<(), String> {
    let metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect {label} {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "{label} {} must be a regular non-symlink file",
            path.display()
        ));
    }
    enforce_owner_only(path, &metadata, label)
}

#[cfg(unix)]
fn enforce_owner_only(path: &Path, metadata: &fs::Metadata, label: &str) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "{label} {} must not grant group or other permissions",
            path.display()
        ));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!(
            "{label} {} must be owned by the effective user",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_owner_only(_path: &Path, _metadata: &fs::Metadata, _label: &str) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn secure_create_new(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn secure_create_new(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn temporary_root() -> PathBuf {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().canonicalize().unwrap().join(format!(
            "reticulum-lxmf-profile-test-{}-{serial}",
            std::process::id()
        ))
    }

    #[test]
    fn profiles_use_canonical_non_secret_paths() {
        let path = temporary_root();
        let root = ProfileRoot::open(&path).unwrap();
        let profile = root.device("ac:a7:04:e1:3e:88").unwrap();
        profile.prepare_database().unwrap();

        assert_eq!(profile.usb_serial(), "ACA704E13E88");
        assert_eq!(
            profile.credential_path(),
            path.join("devices/ACA704E13E88/credential.rdpkey")
        );
        assert_eq!(root.existing_usb_serials().unwrap(), vec!["ACA704E13E88"]);

        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn invalid_or_noncanonical_profile_entries_fail_closed() {
        let path = temporary_root();
        let root = ProfileRoot::open(&path).unwrap();
        fs::create_dir(path.join("devices/not-a-serial")).unwrap();

        assert!(root.existing_usb_serials().is_err());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn managed_profile_root_must_be_absolute() {
        assert!(ProfileRoot::open("relative-profile").is_err());
        assert!(ProfileRoot::open("/private/profile/../other").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_ancestor_is_rejected_before_creating_through_it() {
        use std::os::unix::fs::symlink;

        let container = temporary_root();
        let target = container.join("target");
        let alias = container.join("alias");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &alias).unwrap();

        let requested = alias.join("profile");
        let error = ProfileRoot::open(&requested).unwrap_err();
        assert!(error.contains("must not traverse symlink"));
        assert!(!target.join("profile").exists());

        fs::remove_dir_all(container).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn broad_existing_permissions_are_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let path = temporary_root();
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        let error = ProfileRoot::open(&path).unwrap_err();
        assert!(error.contains("must not grant group or other permissions"));
        fs::remove_dir_all(path).unwrap();
    }
}
