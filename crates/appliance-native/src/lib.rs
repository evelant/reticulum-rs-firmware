//! Native mobile binding surface for the Reticulum appliance client.
//!
//! The immutable bridge contract establishes that an installed application and
//! its generated TypeScript declarations agree with the Rust device API. The
//! [`NativeAppliance`] facade additionally owns durable offline chat state.
//! [`NativePrnsNode`] is the replacement network owner: it runs an ordinary
//! PRNS node and Bluetooth Auto interface without the alpha device bearer.

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

use reticulum_device_api::{
    ApiVersion, MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES,
    MAX_LXMF_READ_CHUNK_BYTES, MAX_MESSAGE_BYTES, MAX_NOMAD_PAGE_BYTES, MAX_NOMAD_PAGE_PATH_BYTES,
    MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS,
};
#[cfg(target_os = "android")]
mod android_prns_jni;
mod appliance;
mod prns_node;
mod prns_session;
mod profile;

pub use appliance::{NativeAppliance, NativeApplianceError};
pub use prns_node::{
    NativePrnsManagementCandidate, NativePrnsManagementError, NativePrnsManagementIdentity,
    NativePrnsNode, NativePrnsNodeError, NativePrnsNodeSnapshot, NativePrnsNodeState,
    NativePrnsOtaError, NativePrnsOtaPhase, NativePrnsOtaSlot, NativePrnsOtaStatus,
};
pub use prns_session::PrnsConnector;
pub use profile::{NativeProfileStore, NativeProfileStoreSnapshot, NativeProfileSummary};

/// Incompatible generation of the callable native bridge.
pub const BRIDGE_API_MAJOR: u16 = 3;
/// Backward-compatible revision of the callable native bridge.
pub const BRIDGE_API_MINOR: u16 = 2;

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
    /// Largest UTF-8 NomadNet page path accepted by the device.
    pub max_nomad_page_path_bytes: u32,
    /// Largest complete UTF-8 NomadNet page returned by the device.
    pub max_nomad_page_bytes: u32,
    /// Largest exact Unix-millisecond NomadNet request timestamp.
    pub max_nomad_request_timestamp_unix_ms: u64,
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
        max_nomad_page_path_bytes: u32::try_from(MAX_NOMAD_PAGE_PATH_BYTES)
            .expect("NomadNet path bound must fit u32"),
        max_nomad_page_bytes: u32::try_from(MAX_NOMAD_PAGE_BYTES)
            .expect("NomadNet page bound must fit u32"),
        max_nomad_request_timestamp_unix_ms: MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS,
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
                bridge_api_major: 3,
                bridge_api_minor: 2,
                device_api_major: 6,
                device_api_minor: 0,
                max_message_bytes: 512,
                max_lxmf_read_chunk_bytes: 416,
                max_lxmf_basic_title_bytes: 295,
                max_lxmf_basic_content_bytes: 295,
                max_nomad_page_path_bytes: 128,
                max_nomad_page_bytes: 400,
                max_nomad_request_timestamp_unix_ms: 9_007_199_254_740_991,
            }
        );
    }
}
