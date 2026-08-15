//! Portable BLE GATT contract for the authenticated Reticulum device API.
//!
//! BLE is only a reliable fragment carrier for the existing ordered RDA1 byte
//! stream. It does not define a second framing or application protocol.

#![no_std]
#![deny(missing_docs)]

/// Incompatible generation of the GATT service contract.
///
/// Generation 2 moves the complete service into a new UUID namespace. Bonded
/// mobile platforms may otherwise reuse the generation-1 characteristic table,
/// which did not contain the retained-link readiness characteristic.
pub const GATT_PROFILE_MAJOR: u16 = 2;
/// Backward-compatible revision of the GATT service contract.
pub const GATT_PROFILE_MINOR: u16 = 3;

/// Project-owned primary service UUID in canonical text form.
pub const SERVICE_UUID: &str = "f3c8a0b0-5e7a-4c51-a3b9-7d2160d20a02";
/// Phone-to-device write-with-response characteristic UUID.
pub const RX_UUID: &str = "f3c8a0b1-5e7a-4c51-a3b9-7d2160d20a02";
/// Device-to-phone indication characteristic UUID.
pub const TX_UUID: &str = "f3c8a0b2-5e7a-4c51-a3b9-7d2160d20a02";
/// Public retained-link readiness characteristic UUID.
///
/// The marker remains `WAIT` until firmware has authenticated the current link
/// and durably committed its bond. Reading it is deliberately unprotected so
/// polling cannot initiate platform security before firmware observes physical
/// presence. The value is not an application credential or a substitute for
/// the authenticated RDA1 handshake.
pub const SECURITY_CONFIRMATION_UUID: &str = "f3c8a0b3-5e7a-4c51-a3b9-7d2160d20a02";

/// Project-owned primary service UUID as one 128-bit value.
pub const SERVICE_UUID_U128: u128 = 0xf3c8_a0b0_5e7a_4c51_a3b9_7d21_60d2_0a02;
/// Phone-to-device characteristic UUID as one 128-bit value.
pub const RX_UUID_U128: u128 = 0xf3c8_a0b1_5e7a_4c51_a3b9_7d21_60d2_0a02;
/// Device-to-phone characteristic UUID as one 128-bit value.
pub const TX_UUID_U128: u128 = 0xf3c8_a0b2_5e7a_4c51_a3b9_7d21_60d2_0a02;
/// Public retained-link readiness characteristic UUID as one 128-bit value.
pub const SECURITY_CONFIRMATION_UUID_U128: u128 = 0xf3c8_a0b3_5e7a_4c51_a3b9_7d21_60d2_0a02;

/// Public value returned while authenticated application traffic is not ready.
pub const SECURITY_CONFIRMATION_PENDING_VALUE: [u8; 4] = *b"WAIT";
/// Public value returned after authenticated, durable pairing state is ready
/// to accept application protocol bytes on the retained link.
///
/// Requiring this firmware-owned transition, rather than treating link
/// encryption alone as ready, prevents a central from racing the firmware's
/// `PairingComplete` handling and durable bond commit.
pub const SECURITY_CONFIRMATION_READY_VALUE: [u8; 4] = *b"RDY1";

/// Primary service UUID bytes in the little-endian order used in BLE
/// advertising payloads.
pub const SERVICE_UUID_LE: [u8; 16] = SERVICE_UUID_U128.to_le_bytes();

/// Prefix used for the MAC-derived local advertising name.
pub const LOCAL_NAME_PREFIX: &[u8] = b"reticulum-e290-";
/// Prefix used while an E290 is accepting a physical BLE recovery pairing.
///
/// Keeping recovery advertisements in a distinct name namespace prevents an
/// application with a stale saved profile from taking the appliance's single
/// BLE connection slot before a recovery client can claim it.
pub const RECOVERY_LOCAL_NAME_PREFIX: &[u8] = b"reticulum-pair-";
/// Number of trailing EUI-48 bytes encoded in the local name.
pub const LOCAL_NAME_SUFFIX_BYTES: usize = 3;
/// Exact number of bytes in the complete local name.
pub const LOCAL_NAME_BYTES: usize = LOCAL_NAME_PREFIX.len() + LOCAL_NAME_SUFFIX_BYTES * 2;
/// Exact number of bytes in the complete recovery local name.
pub const RECOVERY_LOCAL_NAME_BYTES: usize =
    RECOVERY_LOCAL_NAME_PREFIX.len() + LOCAL_NAME_SUFFIX_BYTES * 2;
/// Namespace prefix used for stable E290 device-API identifiers.
pub const E290_DEVICE_API_ID_PREFIX: &[u8; 10] = b"e290-api-1";

/// ATT payload supported even when the central retains the minimum MTU of 23.
pub const MINIMUM_ATT_VALUE_BYTES: usize = 20;
/// Largest characteristic value carried by this GATT profile.
///
/// Trouble's current 255-byte packet pool can carry a 251-byte ATT MTU. The
/// profile uses at most its 248-byte attribute payload and falls back to
/// [`MINIMUM_ATT_VALUE_BYTES`] until a larger MTU has been negotiated.
pub const MAXIMUM_ATT_VALUE_BYTES: usize = 248;
/// Maximum number of centrals owned by the initial appliance profile.
pub const MAX_CONNECTIONS: usize = 1;

/// Build the stable advertising name from the final three EUI-48 bytes.
pub const fn local_name(eui48: [u8; 6]) -> [u8; LOCAL_NAME_BYTES] {
    derived_local_name::<LOCAL_NAME_BYTES>(LOCAL_NAME_PREFIX, eui48)
}

/// Build the physical-recovery advertising name from the final three EUI-48
/// bytes.
///
/// The suffix matches [`local_name`], allowing a client with an activated E290
/// credential to target the same physical appliance without accepting normal
/// saved-profile advertisements.
pub const fn recovery_local_name(eui48: [u8; 6]) -> [u8; RECOVERY_LOCAL_NAME_BYTES] {
    derived_local_name::<RECOVERY_LOCAL_NAME_BYTES>(RECOVERY_LOCAL_NAME_PREFIX, eui48)
}

const fn derived_local_name<const NAME_BYTES: usize>(
    prefix_bytes: &[u8],
    eui48: [u8; 6],
) -> [u8; NAME_BYTES] {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut name = [0_u8; NAME_BYTES];
    let mut prefix = 0;
    while prefix < prefix_bytes.len() {
        name[prefix] = prefix_bytes[prefix];
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < LOCAL_NAME_SUFFIX_BYTES {
        let byte = eui48[eui48.len() - LOCAL_NAME_SUFFIX_BYTES + suffix];
        let output = prefix_bytes.len() + suffix * 2;
        name[output] = HEX[(byte >> 4) as usize];
        name[output + 1] = HEX[(byte & 0x0f) as usize];
        suffix += 1;
    }
    name
}

/// Derive the stable public E290 device-API identifier from an EUI-48.
pub const fn device_api_id(eui48: [u8; 6]) -> [u8; 16] {
    let mut device_id = [0_u8; 16];
    let mut prefix = 0;
    while prefix < E290_DEVICE_API_ID_PREFIX.len() {
        device_id[prefix] = E290_DEVICE_API_ID_PREFIX[prefix];
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < eui48.len() {
        device_id[E290_DEVICE_API_ID_PREFIX.len() + suffix] = eui48[suffix];
        suffix += 1;
    }
    device_id
}

/// Recover the EUI-48 from a stable E290 device-API identifier.
///
/// Returns `None` when the identifier belongs to another device namespace.
pub const fn eui48_from_device_api_id(device_id: [u8; 16]) -> Option<[u8; 6]> {
    let mut prefix = 0;
    while prefix < E290_DEVICE_API_ID_PREFIX.len() {
        if device_id[prefix] != E290_DEVICE_API_ID_PREFIX[prefix] {
            return None;
        }
        prefix += 1;
    }
    let mut eui48 = [0_u8; 6];
    let mut suffix = 0;
    while suffix < eui48.len() {
        eui48[suffix] = device_id[E290_DEVICE_API_ID_PREFIX.len() + suffix];
        suffix += 1;
    }
    Some(eui48)
}

/// Derive the stable local advertising name from an E290 device-API identifier.
///
/// Returns `None` when the identifier belongs to another device namespace.
pub const fn local_name_for_device_api_id(device_id: [u8; 16]) -> Option<[u8; LOCAL_NAME_BYTES]> {
    match eui48_from_device_api_id(device_id) {
        Some(eui48) => Some(local_name(eui48)),
        None => None,
    }
}

/// Derive the physical-recovery advertising name from an E290 device-API
/// identifier.
///
/// Returns `None` when the identifier belongs to another device namespace.
pub const fn recovery_local_name_for_device_api_id(
    device_id: [u8; 16],
) -> Option<[u8; RECOVERY_LOCAL_NAME_BYTES]> {
    match eui48_from_device_api_id(device_id) {
        Some(eui48) => Some(recovery_local_name(eui48)),
        None => None,
    }
}

const _: () = assert!(LOCAL_NAME_BYTES <= 29);
const _: () = assert!(RECOVERY_LOCAL_NAME_BYTES <= 29);
const _: () = assert!(MINIMUM_ATT_VALUE_BYTES == 23 - 3);
const _: () = assert!(MAXIMUM_ATT_VALUE_BYTES == 251 - 3);
const _: () = assert!(MINIMUM_ATT_VALUE_BYTES <= MAXIMUM_ATT_VALUE_BYTES);
const _: () = assert!(MAX_CONNECTIONS == 1);
const _: () = assert!(E290_DEVICE_API_ID_PREFIX.len() + 6 == 16);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_related_but_unambiguous() {
        assert_eq!(SERVICE_UUID_LE, SERVICE_UUID_U128.to_le_bytes());
        assert_ne!(SERVICE_UUID_U128, RX_UUID_U128);
        assert_ne!(SERVICE_UUID_U128, TX_UUID_U128);
        assert_ne!(SERVICE_UUID_U128, SECURITY_CONFIRMATION_UUID_U128);
        assert_ne!(RX_UUID_U128, TX_UUID_U128);
        assert_ne!(RX_UUID_U128, SECURITY_CONFIRMATION_UUID_U128);
        assert_ne!(TX_UUID_U128, SECURITY_CONFIRMATION_UUID_U128);
        assert_eq!(SERVICE_UUID.len(), 36);
        assert_eq!(RX_UUID.len(), 36);
        assert_eq!(TX_UUID.len(), 36);
        assert_eq!(SECURITY_CONFIRMATION_UUID.len(), 36);
        assert_eq!(SECURITY_CONFIRMATION_PENDING_VALUE, *b"WAIT");
        assert_eq!(SECURITY_CONFIRMATION_READY_VALUE, *b"RDY1");
        assert_ne!(
            SECURITY_CONFIRMATION_PENDING_VALUE,
            SECURITY_CONFIRMATION_READY_VALUE
        );
    }

    #[test]
    fn negotiated_value_profile_keeps_the_att_fallback_and_248_byte_ceiling() {
        assert_eq!((GATT_PROFILE_MAJOR, GATT_PROFILE_MINOR), (2, 3));
        assert_eq!(MINIMUM_ATT_VALUE_BYTES, 20);
        assert_eq!(MAXIMUM_ATT_VALUE_BYTES, 248);
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

    #[test]
    fn recovery_local_name_is_distinct_but_keeps_the_board_suffix() {
        let eui48 = [0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88];
        let ordinary = local_name(eui48);
        let recovery = recovery_local_name(eui48);

        assert_eq!(&recovery, b"reticulum-pair-e13e88");
        assert_ne!(ordinary.as_slice(), recovery.as_slice());
        assert_eq!(
            &ordinary[LOCAL_NAME_PREFIX.len()..],
            &recovery[RECOVERY_LOCAL_NAME_PREFIX.len()..]
        );
        assert!(recovery.len() <= 29);
    }

    #[test]
    fn e290_device_api_id_round_trips_to_its_name() {
        let eui48 = [0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88];
        let device_id = device_api_id(eui48);
        assert_eq!(device_id, *b"e290-api-1\xac\xa7\x04\xe1\x3e\x88");
        assert_eq!(eui48_from_device_api_id(device_id), Some(eui48));
        assert_eq!(
            local_name_for_device_api_id(device_id),
            Some(*b"reticulum-e290-e13e88")
        );
        assert_eq!(
            recovery_local_name_for_device_api_id(device_id),
            Some(*b"reticulum-pair-e13e88")
        );
    }

    #[test]
    fn another_device_namespace_does_not_claim_an_e290_identity() {
        let device_id = *b"other-api-\xac\xa7\x04\xe1\x3e\x88";
        assert_eq!(eui48_from_device_api_id(device_id), None);
        assert_eq!(local_name_for_device_api_id(device_id), None);
        assert_eq!(recovery_local_name_for_device_api_id(device_id), None);
    }
}
