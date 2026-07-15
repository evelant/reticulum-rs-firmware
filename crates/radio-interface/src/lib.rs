//! Portable facts and planning for RNode-compatible LoRa framing.
//!
//! The actual framing/reassembly state machine is a Phase-1 deliverable. This
//! crate prevents callers from conflating the 500-byte RNS MTU, the 508-byte
//! RNode hardware MTU and the SX1262's 255-byte frame capacity.

#![no_std]
#![forbid(unsafe_code)]

use reticulum_rns_conformance::{LengthError, validate_rnode_packet_len};
pub use reticulum_rns_conformance::{
    RNODE_HW_MTU, RNODE_LORA_DATA_PER_FRAME, RNODE_LORA_HEADER_LEN, RNS_MTU, SX1262_FRAME_MTU,
};

/// Physical radio-frame lengths needed for one RNode packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramePlan {
    /// Payload bytes placed after the header in the first frame.
    pub first_data_len: usize,
    /// Payload bytes placed after the header in a second frame, if required.
    pub second_data_len: Option<usize>,
}

impl FramePlan {
    /// Number of SX1262 frames in this plan.
    pub const fn frame_count(self) -> usize {
        if self.second_data_len.is_some() { 2 } else { 1 }
    }
}

/// Plan the one- or two-frame RNode representation for a packet length.
///
/// This does not produce header bytes or authorize transmission.
pub const fn plan_frames(packet_len: usize) -> Result<FramePlan, LengthError> {
    match validate_rnode_packet_len(packet_len) {
        Err(error) => Err(error),
        Ok(()) if packet_len <= RNODE_LORA_DATA_PER_FRAME => Ok(FramePlan {
            first_data_len: packet_len,
            second_data_len: None,
        }),
        Ok(()) => Ok(FramePlan {
            first_data_len: RNODE_LORA_DATA_PER_FRAME,
            second_data_len: Some(packet_len - RNODE_LORA_DATA_PER_FRAME),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_frame_boundary_accounts_for_header() {
        assert_eq!(
            plan_frames(254),
            Ok(FramePlan {
                first_data_len: 254,
                second_data_len: None,
            })
        );
        assert_eq!(plan_frames(254).unwrap().frame_count(), 1);
    }

    #[test]
    fn split_boundaries_are_exact() {
        assert_eq!(
            plan_frames(255),
            Ok(FramePlan {
                first_data_len: 254,
                second_data_len: Some(1),
            })
        );
        assert_eq!(
            plan_frames(508),
            Ok(FramePlan {
                first_data_len: 254,
                second_data_len: Some(254),
            })
        );
        assert!(plan_frames(509).is_err());
    }

    #[test]
    fn base_rns_mtu_fits_the_physical_interface() {
        let plan = plan_frames(RNS_MTU).unwrap();
        assert_eq!(plan.second_data_len, Some(246));
    }
}
