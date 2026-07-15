//! Portable RNode-compatible LoRa framing boundary.
//!
//! This crate prevents callers from conflating the 500-byte RNS MTU, the
//! 508-byte RNode hardware MTU and the SX1262's 255-byte frame capacity. It
//! also owns the protocol-level receive reassembly state. A target ingress
//! actor owns the reassembler, expires pending fragments using its monotonic
//! timer, and passes only completed packets through the separate RNS MTU guard.
//!
//! No API in this crate can initialize a radio or authorize transmission.

#![no_std]
#![forbid(unsafe_code)]

use reticulum_rns_conformance::{
    LengthError, validate_rnode_packet_len, validate_sx1262_frame_len,
};
pub use reticulum_rns_conformance::{
    RNODE_HW_MTU, RNODE_LORA_DATA_PER_FRAME, RNODE_LORA_HEADER_LEN, RNS_MTU, SX1262_FRAME_MTU,
};

mod lab_rx_backpressure;
mod lab_rx_profile;
mod radio_diagnostics;
mod reset_quarantine;
mod rx_pipeline;
mod stack_watermark;

pub use lab_rx_backpressure::{
    LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US, LabRxBackpressureHook, LabRxBackpressureHookState,
    LabRxBackpressureStall, LabRxBackpressureTransitionError, LabRxBackpressureValidationError,
};
pub use lab_rx_profile::{
    LabRxProfile, LabRxProfileConfig, LabRxProfileError, RNODE_FRAGMENT_TIMEOUT_GUARD_US,
    RNODE_MIN_PREAMBLE_SYMBOLS, ReceiveFrequencyRange, ReceiveFrequencyRangeError,
};
pub use radio_diagnostics::{
    RadioRxDiagnostics, RadioRxFaultClass, RadioRxFaultClassification, RadioRxFaultCounters,
    RadioRxFaultPhase,
};
pub use reset_quarantine::{
    HealthyLeaseCommit, RESET_QUARANTINE_JOURNAL_WORDS, RESET_QUARANTINE_SLOT_WORDS,
    RESET_STORM_QUARANTINE_THRESHOLD, ResetFaultHistory, ResetQuarantineDecision,
    ResetQuarantineReason, ResetQuarantineStorage, ResetQuarantineWriteError, RetainedBootReason,
    complete_healthy_radio_lease, prepare_reset_quarantine_boot, record_radio_fault_before_reset,
};
pub use rx_pipeline::{
    ExpiredFragment, FrameSignal, RawFrameHandoff, RawFrameHandoffDiagnostics,
    RawFrameHandoffOutcome, RawReceivedFrame, RxDiagnostics, TimedReceiveError,
    TimedReceiveOutcome, TimedRnodeRx,
};
pub use stack_watermark::{
    STACK_WATERMARK_PATTERN_SEED, STACK_WATERMARK_WORD_BYTES, StackWatermarkLayout,
    StackWatermarkLayoutError, StackWatermarkScan, scan_stack_watermark, stack_watermark_word,
};

/// Sequence-number bits in an RNode LoRa frame header.
pub const RNODE_LORA_SEQUENCE_MASK: u8 = 0xf0;

/// Flag bits in an RNode LoRa frame header.
pub const RNODE_LORA_FLAGS_MASK: u8 = 0x0f;

/// Marks both frames of a split RNode packet.
pub const RNODE_LORA_SPLIT_FLAG: u8 = 0x01;

/// Decoded one-byte RNode LoRa frame header.
///
/// Bits 7..=4 hold a four-bit sequence number, bit 0 is the split flag, and
/// bits 3..=1 are currently unused. Unknown low-nibble flags are retained and
/// ignored for compatibility with the reference firmware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnodeFrameHeader {
    /// Four-bit sequence number normalized to `0..=15`.
    pub sequence: u8,
    /// Complete low-nibble flags, including currently unknown bits.
    pub flags: u8,
}

impl RnodeFrameHeader {
    /// Decode a header byte received from the radio.
    pub const fn decode(raw: u8) -> Self {
        Self {
            sequence: (raw & RNODE_LORA_SEQUENCE_MASK) >> 4,
            flags: raw & RNODE_LORA_FLAGS_MASK,
        }
    }

    /// Whether the frame participates in a two-frame packet.
    pub const fn is_split(self) -> bool {
        self.flags & RNODE_LORA_SPLIT_FLAG != 0
    }
}

/// Observable description of a first split frame held for reassembly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingFragment {
    /// Four-bit sequence number expected on the continuation.
    pub sequence: u8,
    /// Packet-data bytes held after stripping the frame header.
    pub data_len: usize,
}

/// Result of consuming one valid raw LoRa frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveOutcome {
    /// A split first frame is buffered and needs a matching continuation.
    AwaitingContinuation {
        /// Sequence number expected on the next split frame.
        sequence: u8,
        /// Data bytes now held in the bounded reassembly buffer.
        data_len: usize,
        /// A different-sequence pending fragment was discarded first.
        replaced_pending: bool,
    },
    /// A complete physical-interface packet was copied to the output buffer.
    Complete {
        /// Valid bytes in the caller's output buffer.
        packet_len: usize,
        /// A stale pending split was discarded by this non-split frame.
        discarded_pending: bool,
    },
}

/// Explicit rejection from the raw receive/reassembly boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveError {
    /// No header byte was present.
    MissingHeader,
    /// A frame exceeded the SX1262's 255-byte physical limit.
    FrameTooLong(LengthError),
    /// The caller did not provide enough room for the completed packet.
    OutputTooSmall {
        /// Required output capacity.
        needed: usize,
        /// Capacity supplied by the caller.
        available: usize,
    },
}

/// Bounded receive-side reassembly for RNode-compatible LoRa framing.
///
/// The official format has only a split bit and a four-bit packet sequence;
/// it has no fragment index. Consequently, the first split frame seen for a
/// sequence is held and the next split frame with the same sequence completes
/// it. Duplicate or reordered same-sequence frames are inherently ambiguous.
/// The owning radio actor must call [`Self::expire_pending`] at its configured
/// deadline so a lost continuation cannot remain indefinitely.
pub struct RnodeRxReassembler {
    first_data: [u8; RNODE_LORA_DATA_PER_FRAME],
    pending: Option<PendingFragment>,
}

impl RnodeRxReassembler {
    /// Construct an empty reassembler without allocation.
    pub const fn new() -> Self {
        Self {
            first_data: [0; RNODE_LORA_DATA_PER_FRAME],
            pending: None,
        }
    }

    /// Return the currently buffered first fragment, if any.
    pub const fn pending(&self) -> Option<PendingFragment> {
        self.pending
    }

    /// Drop and report a pending split fragment.
    ///
    /// Timer selection belongs to the radio actor because it depends on the
    /// configured LoRa airtime; this protocol object deliberately has no clock.
    pub fn expire_pending(&mut self) -> Option<PendingFragment> {
        self.pending.take()
    }

    /// Consume one raw SX1262 frame and optionally produce a complete packet.
    ///
    /// The one-byte header is always stripped. A malformed frame leaves any
    /// pending fragment unchanged. A completed split is cleared even when the
    /// output buffer is too small, preventing accidental reuse of stale data.
    pub fn feed(
        &mut self,
        frame: &[u8],
        output: &mut [u8],
    ) -> Result<ReceiveOutcome, ReceiveError> {
        validate_sx1262_frame_len(frame.len()).map_err(ReceiveError::FrameTooLong)?;
        let (&raw_header, data) = frame.split_first().ok_or(ReceiveError::MissingHeader)?;
        let header = RnodeFrameHeader::decode(raw_header);

        if header.is_split() {
            return self.feed_split(header.sequence, data, output);
        }

        let discarded_pending = self.pending.take().is_some();
        copy_completed(data, output)?;
        Ok(ReceiveOutcome::Complete {
            packet_len: data.len(),
            discarded_pending,
        })
    }

    fn feed_split(
        &mut self,
        sequence: u8,
        data: &[u8],
        output: &mut [u8],
    ) -> Result<ReceiveOutcome, ReceiveError> {
        match self.pending {
            Some(pending) if pending.sequence == sequence => {
                let total = pending.data_len + data.len();
                // A matching frame consumes the pending state whether delivery
                // succeeds or not. The state must never be reused after a drop.
                self.pending = None;
                if output.len() < total {
                    return Err(ReceiveError::OutputTooSmall {
                        needed: total,
                        available: output.len(),
                    });
                }
                output[..pending.data_len].copy_from_slice(&self.first_data[..pending.data_len]);
                output[pending.data_len..total].copy_from_slice(data);
                Ok(ReceiveOutcome::Complete {
                    packet_len: total,
                    discarded_pending: false,
                })
            }
            previous => {
                self.first_data[..data.len()].copy_from_slice(data);
                self.pending = Some(PendingFragment {
                    sequence,
                    data_len: data.len(),
                });
                Ok(ReceiveOutcome::AwaitingContinuation {
                    sequence,
                    data_len: data.len(),
                    replaced_pending: previous.is_some(),
                })
            }
        }
    }
}

impl Default for RnodeRxReassembler {
    fn default() -> Self {
        Self::new()
    }
}

fn copy_completed(data: &[u8], output: &mut [u8]) -> Result<(), ReceiveError> {
    if output.len() < data.len() {
        return Err(ReceiveError::OutputTooSmall {
            needed: data.len(),
            available: output.len(),
        });
    }
    output[..data.len()].copy_from_slice(data);
    Ok(())
}

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

    #[test]
    fn all_accepted_packet_lengths_have_an_exact_plan() {
        for packet_len in 0..=RNODE_HW_MTU {
            let plan = plan_frames(packet_len).unwrap();
            let planned = plan.first_data_len + plan.second_data_len.unwrap_or(0);
            assert_eq!(planned, packet_len);
            assert!(plan.first_data_len <= RNODE_LORA_DATA_PER_FRAME);
            assert!(
                plan.second_data_len
                    .is_none_or(|len| len <= RNODE_LORA_DATA_PER_FRAME)
            );
            assert_eq!(plan.frame_count(), usize::from(packet_len > 254) + 1);
        }
    }

    #[test]
    fn header_decoding_preserves_unknown_flags() {
        let header = RnodeFrameHeader::decode(0xbd);
        assert_eq!(header.sequence, 11);
        assert_eq!(header.flags, 0x0d);
        assert!(header.is_split());

        let header = RnodeFrameHeader::decode(0xae);
        assert_eq!(header.sequence, 10);
        assert_eq!(header.flags, 0x0e);
        assert!(!header.is_split());
    }

    #[test]
    fn header_only_non_split_frame_preserves_reference_behavior() {
        let mut rx = RnodeRxReassembler::new();
        let mut output = [0u8; RNODE_HW_MTU];
        assert_eq!(
            rx.feed(&[0x30], &mut output),
            Ok(ReceiveOutcome::Complete {
                packet_len: 0,
                discarded_pending: false,
            })
        );
    }

    #[test]
    fn single_frame_strips_header_and_clears_stale_split() {
        let mut rx = RnodeRxReassembler::new();
        let mut output = [0u8; RNODE_HW_MTU];

        assert_eq!(
            rx.feed(&[0x31, 1, 2], &mut output),
            Ok(ReceiveOutcome::AwaitingContinuation {
                sequence: 3,
                data_len: 2,
                replaced_pending: false,
            })
        );
        assert_eq!(
            rx.feed(&[0x40, 7, 8, 9], &mut output),
            Ok(ReceiveOutcome::Complete {
                packet_len: 3,
                discarded_pending: true,
            })
        );
        assert_eq!(&output[..3], &[7, 8, 9]);
        assert_eq!(rx.pending(), None);
    }

    #[test]
    fn two_frames_reassemble_the_full_physical_mtu() {
        let mut first = [0x51; SX1262_FRAME_MTU];
        first[0] = 0x51;
        let mut second = [0xa5; SX1262_FRAME_MTU];
        second[0] = 0x51;
        let mut output = [0u8; RNODE_HW_MTU];
        let mut rx = RnodeRxReassembler::new();

        assert!(matches!(
            rx.feed(&first, &mut output),
            Ok(ReceiveOutcome::AwaitingContinuation { .. })
        ));
        assert_eq!(
            rx.feed(&second, &mut output),
            Ok(ReceiveOutcome::Complete {
                packet_len: RNODE_HW_MTU,
                discarded_pending: false,
            })
        );
        assert_eq!(&output[..RNODE_LORA_DATA_PER_FRAME], &[0x51; 254]);
        assert_eq!(&output[RNODE_LORA_DATA_PER_FRAME..], &[0xa5; 254]);
        assert_eq!(rx.pending(), None);
    }

    #[test]
    fn different_sequence_replaces_pending_fragment() {
        let mut rx = RnodeRxReassembler::new();
        let mut output = [0u8; RNODE_HW_MTU];

        rx.feed(&[0x21, 1, 2, 3], &mut output).unwrap();
        assert_eq!(
            rx.feed(&[0x71, 4, 5], &mut output),
            Ok(ReceiveOutcome::AwaitingContinuation {
                sequence: 7,
                data_len: 2,
                replaced_pending: true,
            })
        );
        assert_eq!(
            rx.pending(),
            Some(PendingFragment {
                sequence: 7,
                data_len: 2,
            })
        );
    }

    #[test]
    fn caller_expires_lost_continuation() {
        let mut rx = RnodeRxReassembler::new();
        let mut output = [0u8; RNODE_HW_MTU];
        rx.feed(&[0x91, 1, 2, 3], &mut output).unwrap();

        assert_eq!(
            rx.expire_pending(),
            Some(PendingFragment {
                sequence: 9,
                data_len: 3,
            })
        );
        assert_eq!(rx.expire_pending(), None);
    }

    #[test]
    fn malformed_frame_does_not_mutate_pending_state() {
        let mut rx = RnodeRxReassembler::new();
        let mut output = [0u8; RNODE_HW_MTU];
        rx.feed(&[0x61, 1, 2], &mut output).unwrap();
        let pending = rx.pending();

        assert_eq!(rx.feed(&[], &mut output), Err(ReceiveError::MissingHeader));
        let oversized = [0u8; SX1262_FRAME_MTU + 1];
        assert_eq!(
            rx.feed(&oversized, &mut output),
            Err(ReceiveError::FrameTooLong(LengthError {
                actual: SX1262_FRAME_MTU + 1,
                maximum: SX1262_FRAME_MTU,
            }))
        );
        assert_eq!(rx.pending(), pending);
    }

    #[test]
    fn insufficient_output_drops_completed_split_state() {
        let mut rx = RnodeRxReassembler::new();
        let mut full_output = [0u8; RNODE_HW_MTU];
        rx.feed(&[0x11, 1, 2, 3], &mut full_output).unwrap();

        let mut short_output = [0u8; 4];
        assert_eq!(
            rx.feed(&[0x11, 4, 5], &mut short_output),
            Err(ReceiveError::OutputTooSmall {
                needed: 5,
                available: 4,
            })
        );
        assert_eq!(rx.pending(), None);
    }

    #[test]
    fn same_sequence_duplicate_is_protocol_ambiguous() {
        let mut rx = RnodeRxReassembler::new();
        let mut output = [0u8; RNODE_HW_MTU];
        let first = [0x41, 9, 8, 7];

        rx.feed(&first, &mut output).unwrap();
        assert_eq!(
            rx.feed(&first, &mut output),
            Ok(ReceiveOutcome::Complete {
                packet_len: 6,
                discarded_pending: false,
            })
        );
        assert_eq!(&output[..6], &[9, 8, 7, 9, 8, 7]);
    }
}
