//! Native mobile binding surface for the Reticulum appliance client.
//!
//! The immutable bridge contract establishes that an installed application and
//! its generated TypeScript declarations agree with the Rust device API. The
//! [`NativeAppliance`] facade additionally owns durable offline chat state while
//! platform USB, BLE, and Wi-Fi connector implementations remain explicit
//! unavailable stubs.

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

use reticulum_device_api::{
    ApiVersion, MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES,
    MAX_LXMF_READ_CHUNK_BYTES, MAX_MESSAGE_BYTES,
};

mod appliance;

pub use appliance::{NativeAppliance, NativeApplianceError, NativeTransport};

/// Incompatible generation of the callable native bridge.
pub const BRIDGE_API_MAJOR: u16 = 1;
/// Backward-compatible revision of the callable native bridge.
pub const BRIDGE_API_MINOR: u16 = 1;

/// Exact protocol contract compiled into a native client binary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativeBridgeContract {
    /// Incompatible generation of this callable bridge.
    pub bridge_api_major: u16,
    /// Backward-compatible revision within the bridge generation.
    pub bridge_api_minor: u16,
    /// Incompatible generation of the authenticated device API.
    pub device_api_major: u16,
    /// Backward-compatible revision within the device API generation.
    pub device_api_minor: u16,
    /// Largest complete logical device-API message.
    pub max_message_bytes: u32,
    /// Largest LXMF wire fragment returned by one device-API response.
    pub max_lxmf_read_chunk_bytes: u32,
    /// Structural maximum for a basic LXMF title.
    pub max_lxmf_basic_title_bytes: u32,
    /// Structural maximum for basic LXMF content.
    pub max_lxmf_basic_content_bytes: u32,
}

/// Return the immutable API contract compiled into this native library.
#[must_use]
#[uniffi::export]
pub fn native_bridge_contract() -> NativeBridgeContract {
    NativeBridgeContract {
        bridge_api_major: BRIDGE_API_MAJOR,
        bridge_api_minor: BRIDGE_API_MINOR,
        device_api_major: ApiVersion::CURRENT.major,
        device_api_minor: ApiVersion::CURRENT.minor,
        max_message_bytes: u32::try_from(MAX_MESSAGE_BYTES)
            .expect("device API message bound must fit u32"),
        max_lxmf_read_chunk_bytes: u32::try_from(MAX_LXMF_READ_CHUNK_BYTES)
            .expect("LXMF chunk bound must fit u32"),
        max_lxmf_basic_title_bytes: u32::try_from(MAX_LXMF_BASIC_TITLE_BYTES)
            .expect("LXMF title bound must fit u32"),
        max_lxmf_basic_content_bytes: u32::try_from(MAX_LXMF_BASIC_CONTENT_BYTES)
            .expect("LXMF content bound must fit u32"),
    }
}

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_contract_matches_the_supported_device_api() {
        assert_eq!(
            native_bridge_contract(),
            NativeBridgeContract {
                bridge_api_major: 1,
                bridge_api_minor: 1,
                device_api_major: 1,
                device_api_minor: 4,
                max_message_bytes: 512,
                max_lxmf_read_chunk_bytes: 416,
                max_lxmf_basic_title_bytes: 295,
                max_lxmf_basic_content_bytes: 295,
            }
        );
    }
}
