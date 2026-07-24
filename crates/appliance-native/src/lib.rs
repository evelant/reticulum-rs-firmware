//! Native mobile binding surface for the Reticulum appliance client.
//!
//! The immutable bridge contract establishes that an installed application and
//! its generated TypeScript declarations agree with the Rust device API. The
//! [`NativeAppliance`] facade additionally owns durable offline chat state. The
//! opt-in Wi-Fi and BLE constructors connect that state to the authenticated
//! device API without moving protocol parsing into the platform application.

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

use reticulum_device_api::{
    ApiVersion, MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES,
    MAX_LXMF_READ_CHUNK_BYTES, MAX_MESSAGE_BYTES,
};
use reticulum_device_api_ble::{
    GATT_PROFILE_MAJOR, GATT_PROFILE_MINOR, INITIAL_ATT_VALUE_BYTES, RX_UUID, SERVICE_UUID, TX_UUID,
};

mod appliance;
mod ble;
mod credential;
mod wifi;

pub use appliance::{NativeAppliance, NativeApplianceError, NativeTransport};
pub use ble::{NativeBleError, NativeBlePlatformCommand};
pub use credential::{NativeCredentialStatus, NativeCredentialSummary};

/// Incompatible generation of the callable native bridge.
pub const BRIDGE_API_MAJOR: u16 = 1;
/// Backward-compatible revision of the callable native bridge.
pub const BRIDGE_API_MINOR: u16 = 5;

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

/// BLE GATT profile compiled into the firmware and native client.
///
/// The generated TypeScript binding is the app's source for these values; the
/// Expo layer does not duplicate UUID or fragmentation constants.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NativeBleGattProfile {
    /// Incompatible GATT profile generation.
    pub major: u16,
    /// Backward-compatible GATT profile revision.
    pub minor: u16,
    /// Project-owned primary service UUID.
    pub service_uuid: String,
    /// Phone-to-device write-with-response characteristic UUID.
    pub rx_uuid: String,
    /// Device-to-phone indication characteristic UUID.
    pub tx_uuid: String,
    /// Universally safe initial ATT value size.
    pub initial_att_value_bytes: u32,
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

/// Return the shared BLE GATT profile used by firmware discovery and native
/// app connections.
#[must_use]
#[uniffi::export]
pub fn native_ble_gatt_profile() -> NativeBleGattProfile {
    NativeBleGattProfile {
        major: GATT_PROFILE_MAJOR,
        minor: GATT_PROFILE_MINOR,
        service_uuid: SERVICE_UUID.to_owned(),
        rx_uuid: RX_UUID.to_owned(),
        tx_uuid: TX_UUID.to_owned(),
        initial_att_value_bytes: u32::try_from(INITIAL_ATT_VALUE_BYTES)
            .expect("ATT value bound must fit u32"),
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
                bridge_api_minor: 5,
                device_api_major: 1,
                device_api_minor: 5,
                max_message_bytes: 512,
                max_lxmf_read_chunk_bytes: 416,
                max_lxmf_basic_title_bytes: 295,
                max_lxmf_basic_content_bytes: 295,
            }
        );
    }

    #[test]
    fn exported_ble_profile_matches_the_portable_firmware_contract() {
        assert_eq!(
            native_ble_gatt_profile(),
            NativeBleGattProfile {
                major: 1,
                minor: 0,
                service_uuid: "f3c8a0b0-5e7a-4c51-a3b9-7d2160d20a01".to_owned(),
                rx_uuid: "f3c8a0b1-5e7a-4c51-a3b9-7d2160d20a01".to_owned(),
                tx_uuid: "f3c8a0b2-5e7a-4c51-a3b9-7d2160d20a01".to_owned(),
                initial_att_value_bytes: 20,
            }
        );
    }
}
