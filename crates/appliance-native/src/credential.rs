//! Shared app-private credential loading and host randomness.

use std::fs::{File, symlink_metadata};
use std::num::NonZeroU32;
use std::path::Path;

use rand_core::{CryptoRng, RngCore};
use reticulum_device_client::ActivatedCredential;

pub(crate) fn read_credential(path: &Path) -> Result<ActivatedCredential, String> {
    let metadata = symlink_metadata(path)
        .map_err(|error| format!("could not inspect app-private credential: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("app-private credential must be a regular non-symlink file".to_owned());
    }
    let mut file = File::open(path)
        .map_err(|error| format!("could not open app-private credential: {error}"))?;
    ActivatedCredential::read_from(&mut file)
        .map_err(|error| format!("could not decode app-private credential: {error}"))
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
