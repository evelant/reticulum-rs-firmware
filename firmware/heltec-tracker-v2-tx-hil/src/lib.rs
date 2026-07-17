//! Tracker-specific physical role gate over board-independent HIL fixtures.
//!
//! The reusable sentinel, signed-announce, and semantic round-trip policy lives
//! in `reticulum-semantic-roundtrip-hil`. This crate retains only the exact
//! factory eFuse MACs that may construct the Tracker radio.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub use reticulum_semantic_roundtrip_hil::*;

/// Factory eFuse base MAC of the dedicated Tracker HIL initiator.
pub const INITIATOR_BASE_MAC: [u8; 6] = SEMANTIC_INITIATOR_SELECTOR;

/// Factory eFuse base MAC of the dedicated Tracker HIL responder.
pub const RESPONDER_BASE_MAC: [u8; 6] = SEMANTIC_RESPONDER_SELECTOR;

/// Select the only Tracker role authorized for an exact six-byte eFuse MAC.
pub fn role_for_base_mac(mac: &[u8]) -> HilRole {
    if mac == INITIATOR_BASE_MAC {
        HilRole::Initiator
    } else if mac == RESPONDER_BASE_MAC {
        HilRole::Responder
    } else {
        HilRole::Inert
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_exact_tracker_base_macs_are_active() {
        assert_eq!(role_for_base_mac(&INITIATOR_BASE_MAC), HilRole::Initiator);
        assert_eq!(role_for_base_mac(&RESPONDER_BASE_MAC), HilRole::Responder);

        let mut near_miss = INITIATOR_BASE_MAC;
        near_miss[5] ^= 1;
        assert_eq!(role_for_base_mac(&near_miss), HilRole::Inert);
        assert_eq!(role_for_base_mac(&INITIATOR_BASE_MAC[..5]), HilRole::Inert);
        assert_eq!(role_for_base_mac(&[]), HilRole::Inert);
    }
}
