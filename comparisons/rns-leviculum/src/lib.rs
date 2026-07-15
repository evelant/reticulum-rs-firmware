//! Isolated Leviculum comparison graph for Phase 0.
//!
//! Any binary linking this crate is an AGPL combined work. The Rete firmware
//! and product crates do not depend on it.

#![no_std]
#![forbid(unsafe_code)]

use reticulum_rns_conformance::{CandidateMetadata, CandidateRole};

/// Reviewed Leviculum source revision.
pub const SOURCE_REVISION: &str = "5fb1db0e5e5a490291ee5f6b81312cf0c9de622a";

/// Metadata emitted with every Leviculum comparison result.
pub const fn metadata() -> CandidateMetadata {
    CandidateMetadata {
        id: "leviculum",
        source: "https://codeberg.org/Lew_Palm/leviculum",
        revision: SOURCE_REVISION,
        license: "AGPL-3.0-or-later",
        role: CandidateRole::Comparison,
        accepted: false,
    }
}

/// Known integration findings that must remain visible during comparison.
pub const KNOWN_INTEGRATION_GAPS: &[&str] = &[
    "NodeCore packet handling does not expose every transport parse error",
    "fixed-capacity transport storage does not bound all node allocations",
    "the ESP32-S3 runtime adapter remains to be implemented",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_core_agrees_with_project_mtu() {
        assert_eq!(
            leviculum_core::constants::MTU,
            reticulum_rns_conformance::RNS_MTU
        );
    }

    #[test]
    fn malformed_empty_packet_is_rejected_without_panic() {
        assert!(leviculum_core::packet::Packet::unpack(&[]).is_err());
    }

    #[test]
    fn comparison_is_not_prematurely_marked_accepted() {
        assert!(!metadata().accepted);
        assert!(!KNOWN_INTEGRATION_GAPS.is_empty());
    }
}
