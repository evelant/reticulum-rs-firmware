//! Pure board-role gate for the two-board Vision Master E290 semantic HIL.
//!
//! The cryptographic fixtures and four-step protocol state machine come from
//! the board-independent semantic HIL policy crate. Only the physical E290
//! eFuse MAC-to-role binding lives here. Unknown or truncated identifiers are
//! always inert.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub use reticulum_semantic_roundtrip_hil::HilRole;
use reticulum_semantic_roundtrip_hil::{SEMANTIC_INITIATOR_SELECTOR, SEMANTIC_RESPONDER_SELECTOR};

/// Factory eFuse base MAC of E290 board A, the semantic initiator.
pub const E290_INITIATOR_BASE_MAC: [u8; 6] = [0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88];

/// Factory eFuse base MAC of E290 board B, the semantic responder.
pub const E290_RESPONDER_BASE_MAC: [u8; 6] = [0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88];

/// Select the sole active role authorized for an exact E290 base MAC.
pub fn role_for_e290_base_mac(mac: &[u8]) -> HilRole {
    if mac == E290_INITIATOR_BASE_MAC {
        HilRole::Initiator
    } else if mac == E290_RESPONDER_BASE_MAC {
        HilRole::Responder
    } else {
        HilRole::Inert
    }
}

/// Map an authorized E290 role onto the stable semantic identity fixture.
///
/// The returned bytes are identity selectors only. They are never compared
/// with the connected board and never authorize physical radio construction.
pub const fn semantic_fixture_mac(role: HilRole) -> Option<&'static [u8; 6]> {
    match role {
        HilRole::Initiator => Some(&SEMANTIC_INITIATOR_SELECTOR),
        HilRole::Responder => Some(&SEMANTIC_RESPONDER_SELECTOR),
        HilRole::Inert => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_two_exact_e290_macs_are_active() {
        assert_eq!(
            role_for_e290_base_mac(&E290_INITIATOR_BASE_MAC),
            HilRole::Initiator
        );
        assert_eq!(
            role_for_e290_base_mac(&E290_RESPONDER_BASE_MAC),
            HilRole::Responder
        );

        let mut near_miss = E290_INITIATOR_BASE_MAC;
        near_miss[5] ^= 1;
        assert_eq!(role_for_e290_base_mac(&near_miss), HilRole::Inert);
        assert_eq!(
            role_for_e290_base_mac(&E290_INITIATOR_BASE_MAC[..5]),
            HilRole::Inert
        );
        assert_eq!(role_for_e290_base_mac(&[]), HilRole::Inert);
    }

    #[test]
    fn active_roles_select_distinct_existing_semantic_fixtures() {
        assert_eq!(
            semantic_fixture_mac(HilRole::Initiator),
            Some(&SEMANTIC_INITIATOR_SELECTOR)
        );
        assert_eq!(
            semantic_fixture_mac(HilRole::Responder),
            Some(&SEMANTIC_RESPONDER_SELECTOR)
        );
        assert_eq!(semantic_fixture_mac(HilRole::Inert), None);
    }

    #[test]
    fn mapped_roles_construct_distinct_usable_rete_nodes() {
        let initiator_mac = semantic_fixture_mac(HilRole::Initiator).unwrap();
        let responder_mac = semantic_fixture_mac(HilRole::Responder).unwrap();
        let initiator =
            reticulum_semantic_roundtrip_hil::semantic_roundtrip_node_for_base_mac(initiator_mac)
                .unwrap();
        let responder =
            reticulum_semantic_roundtrip_hil::semantic_roundtrip_node_for_base_mac(responder_mac)
                .unwrap();
        assert_ne!(initiator.destination_hash(), responder.destination_hash());
        assert_eq!(
            reticulum_semantic_roundtrip_hil::semantic_roundtrip_peer_destination_for_base_mac(
                initiator_mac,
            )
            .unwrap(),
            responder.destination_hash(),
        );
    }
}
