//! Portable BLE GATT contract for the authenticated Reticulum device API.
//!
//! BLE is only a reliable fragment carrier for the existing ordered RDA1 byte
//! stream. It does not define a second framing or application protocol.

#![no_std]
#![deny(missing_docs)]

/// Incompatible generation of the GATT service contract.
pub const GATT_PROFILE_MAJOR: u16 = 1;
/// Backward-compatible revision of the GATT service contract.
pub const GATT_PROFILE_MINOR: u16 = 0;

/// Project-owned primary service UUID in canonical text form.
pub const SERVICE_UUID: &str = "f3c8a0b0-5e7a-4c51-a3b9-7d2160d20a01";
/// Phone-to-device write-with-response characteristic UUID.
pub const RX_UUID: &str = "f3c8a0b1-5e7a-4c51-a3b9-7d2160d20a01";
/// Device-to-phone indication characteristic UUID.
pub const TX_UUID: &str = "f3c8a0b2-5e7a-4c51-a3b9-7d2160d20a01";

/// Project-owned primary service UUID as one 128-bit value.
pub const SERVICE_UUID_U128: u128 = 0xf3c8_a0b0_5e7a_4c51_a3b9_7d21_60d2_0a01;
/// Phone-to-device characteristic UUID as one 128-bit value.
pub const RX_UUID_U128: u128 = 0xf3c8_a0b1_5e7a_4c51_a3b9_7d21_60d2_0a01;
/// Device-to-phone characteristic UUID as one 128-bit value.
pub const TX_UUID_U128: u128 = 0xf3c8_a0b2_5e7a_4c51_a3b9_7d21_60d2_0a01;

/// Primary service UUID bytes in the little-endian order used in BLE
/// advertising payloads.
pub const SERVICE_UUID_LE: [u8; 16] = SERVICE_UUID_U128.to_le_bytes();

/// Prefix used for the MAC-derived local advertising name.
pub const LOCAL_NAME_PREFIX: &[u8] = b"reticulum-e290-";
/// Number of trailing EUI-48 bytes encoded in the local name.
pub const LOCAL_NAME_SUFFIX_BYTES: usize = 3;
/// Exact number of bytes in the complete local name.
pub const LOCAL_NAME_BYTES: usize = LOCAL_NAME_PREFIX.len() + LOCAL_NAME_SUFFIX_BYTES * 2;

/// ATT payload supported even when the central retains the minimum MTU of 23.
///
/// A later profile revision may use a larger negotiated payload. Starting at
/// twenty bytes keeps the first proof interoperable and bounded.
pub const INITIAL_ATT_VALUE_BYTES: usize = 20;
/// Maximum number of centrals owned by the initial appliance profile.
pub const MAX_CONNECTIONS: usize = 1;

/// Build the stable advertising name from the final three EUI-48 bytes.
pub const fn local_name(eui48: [u8; 6]) -> [u8; LOCAL_NAME_BYTES] {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut name = [0_u8; LOCAL_NAME_BYTES];
    let mut prefix = 0;
    while prefix < LOCAL_NAME_PREFIX.len() {
        name[prefix] = LOCAL_NAME_PREFIX[prefix];
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < LOCAL_NAME_SUFFIX_BYTES {
        let byte = eui48[eui48.len() - LOCAL_NAME_SUFFIX_BYTES + suffix];
        let output = LOCAL_NAME_PREFIX.len() + suffix * 2;
        name[output] = HEX[(byte >> 4) as usize];
        name[output + 1] = HEX[(byte & 0x0f) as usize];
        suffix += 1;
    }
    name
}

const _: () = assert!(LOCAL_NAME_BYTES <= 29);
const _: () = assert!(INITIAL_ATT_VALUE_BYTES == 23 - 3);
const _: () = assert!(MAX_CONNECTIONS == 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_related_but_unambiguous() {
        assert_eq!(SERVICE_UUID_LE, SERVICE_UUID_U128.to_le_bytes());
        assert_ne!(SERVICE_UUID_U128, RX_UUID_U128);
        assert_ne!(SERVICE_UUID_U128, TX_UUID_U128);
        assert_ne!(RX_UUID_U128, TX_UUID_U128);
        assert_eq!(SERVICE_UUID.len(), 36);
        assert_eq!(RX_UUID.len(), 36);
        assert_eq!(TX_UUID.len(), 36);
    }

    #[test]
    fn local_name_is_complete_and_board_specific() {
        assert_eq!(
            &local_name([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]),
            b"reticulum-e290-e13e88"
        );
        assert_ne!(
            local_name([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]),
            local_name([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88])
        );
    }
}
