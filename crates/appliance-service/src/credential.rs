//! Owner-private credential loading and host randomness for authenticated bearers.

use std::fs::{File, symlink_metadata};
use std::num::NonZeroU32;
use std::path::Path;

use rand_core::{CryptoRng, RngCore};
use reticulum_device_client::ActivatedCredential;

pub(crate) fn read_credential(path: &Path) -> Result<ActivatedCredential, String> {
    let path_metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect configured credential: {error}"))?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err("configured credential must be a regular non-symlink file".to_owned());
    }
    enforce_owner_only(&path_metadata)?;
    let mut file = File::open(path)
        .map_err(|error| format!("could not open configured credential: {error}"))?;
    verify_open_file_identity(&path_metadata, &file)?;
    ActivatedCredential::read_from(&mut file)
        .map_err(|error| format!("could not load configured credential: {error}"))
}

#[cfg(unix)]
fn enforce_owner_only(metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(
            "configured credential must not grant group or other permissions (use chmod 600)"
                .to_owned(),
        );
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err("configured credential must be owned by the effective user".to_owned());
    }
    if metadata.nlink() != 1 {
        return Err("configured credential must have exactly one hard link".to_owned());
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_owner_only(_metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn verify_open_file_identity(expected: &std::fs::Metadata, file: &File) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let opened = file
        .metadata()
        .map_err(|error| format!("could not recheck configured credential: {error}"))?;
    if expected.dev() != opened.dev() || expected.ino() != opened.ino() {
        return Err("configured credential changed while it was being opened".to_owned());
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_open_file_identity(_expected: &std::fs::Metadata, _file: &File) -> Result<(), String> {
    Ok(())
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
