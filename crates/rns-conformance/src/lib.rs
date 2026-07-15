//! Small, deliberately incomplete vocabulary shared by Phase-0 RNS probes.
//!
//! This crate records immutable wire limits and comparable candidate metadata.
//! It is not the production RNS abstraction. Candidate-specific events,
//! storage, allocation behavior and errors remain visible during evaluation.

#![no_std]
#![forbid(unsafe_code)]

/// Maximum size of a base Reticulum packet in bytes.
pub const RNS_MTU: usize = 500;

/// Maximum packet accepted by an RNode physical interface in bytes.
pub const RNODE_HW_MTU: usize = 508;

/// Maximum SX1262 radio payload in bytes.
pub const SX1262_FRAME_MTU: usize = 255;

/// Bytes reserved by the RNode LoRa framing header in every radio frame.
pub const RNODE_LORA_HEADER_LEN: usize = 1;

/// RNode packet bytes available after the one-byte LoRa header.
pub const RNODE_LORA_DATA_PER_FRAME: usize = SX1262_FRAME_MTU - RNODE_LORA_HEADER_LEN;

/// Current project decision for an RNS candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateStatus {
    /// Candidate used as the working foundation while conformance and bounds
    /// are still being proven.
    ProvisionalFoundation,
    /// Foundation that has passed every documented production gate.
    ProductionFoundation,
    /// Candidate retained as an independently implemented fallback.
    Fallback,
}

/// Immutable source and status data emitted by every candidate runner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CandidateMetadata {
    /// Stable project-local candidate identifier.
    pub id: &'static str,
    /// Source repository containing the reviewed revision.
    pub source: &'static str,
    /// Full reviewed source revision.
    pub revision: &'static str,
    /// SPDX license expression governing this candidate graph.
    pub license: &'static str,
    /// Current project decision for this candidate.
    pub status: CandidateStatus,
}

/// Rejection from a physical or protocol boundary guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LengthError {
    /// Observed byte length.
    pub actual: usize,
    /// Maximum accepted byte length.
    pub maximum: usize,
}

/// Validate a packet against the base Reticulum MTU.
pub const fn validate_rns_packet_len(actual: usize) -> Result<(), LengthError> {
    validate_len(actual, RNS_MTU)
}

/// Validate a packet against the RNode physical-interface MTU.
pub const fn validate_rnode_packet_len(actual: usize) -> Result<(), LengthError> {
    validate_len(actual, RNODE_HW_MTU)
}

/// Validate a raw SX1262 frame length.
pub const fn validate_sx1262_frame_len(actual: usize) -> Result<(), LengthError> {
    validate_len(actual, SX1262_FRAME_MTU)
}

const fn validate_len(actual: usize, maximum: usize) -> Result<(), LengthError> {
    if actual <= maximum {
        Ok(())
    } else {
        Err(LengthError { actual, maximum })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_boundaries_are_distinct() {
        assert_eq!(RNS_MTU, 500);
        assert_eq!(RNODE_HW_MTU, 508);
        assert_eq!(SX1262_FRAME_MTU, 255);
        assert_eq!(RNODE_LORA_DATA_PER_FRAME, 254);
    }

    #[test]
    fn each_guard_accepts_its_boundary_and_rejects_one_more() {
        assert_eq!(validate_rns_packet_len(500), Ok(()));
        assert_eq!(
            validate_rns_packet_len(501),
            Err(LengthError {
                actual: 501,
                maximum: 500,
            })
        );

        assert_eq!(validate_rnode_packet_len(508), Ok(()));
        assert!(validate_rnode_packet_len(509).is_err());

        assert_eq!(validate_sx1262_frame_len(255), Ok(()));
        assert!(validate_sx1262_frame_len(256).is_err());
    }
}
