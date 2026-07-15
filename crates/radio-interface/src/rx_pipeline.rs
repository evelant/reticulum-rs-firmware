//! Timed, allocation-free receive policy above RNode frame reassembly.

use core::num::NonZeroU64;

use crate::{
    PendingFragment, RNODE_HW_MTU, RNS_MTU, ReceiveError, ReceiveOutcome, RnodeRxReassembler,
};

/// Signal metadata reported for one physical LoRa frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameSignal {
    /// Received signal strength in dBm.
    pub rssi_dbm: i16,
    /// Signal-to-noise ratio in dB.
    pub snr_db: i16,
}

impl FrameSignal {
    /// Construct signal metadata without applying a radio-specific offset.
    pub const fn new(rssi_dbm: i16, snr_db: i16) -> Self {
        Self { rssi_dbm, snr_db }
    }

    /// Conservatively combine signal reports from both halves of a packet.
    ///
    /// Lower RSSI and SNR values represent the weaker observation, so each
    /// field is minimized independently.
    pub const fn weakest(self, other: Self) -> Self {
        Self {
            rssi_dbm: if self.rssi_dbm < other.rssi_dbm {
                self.rssi_dbm
            } else {
                other.rssi_dbm
            },
            snr_db: if self.snr_db < other.snr_db {
                self.snr_db
            } else {
                other.snr_db
            },
        }
    }
}

/// A split fragment removed when its monotonic deadline elapsed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpiredFragment {
    /// RNode sequence and buffered data length.
    pub fragment: PendingFragment,
    /// Deadline that caused this fragment to expire.
    pub deadline_ticks: u64,
    /// Signal metadata captured for the discarded first half.
    pub signal: FrameSignal,
}

/// Successful result of consuming one physical frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimedReceiveOutcome {
    /// A split first half is buffered until its deadline.
    AwaitingContinuation {
        /// Four-bit sequence expected on the continuation.
        sequence: u8,
        /// RNS data bytes held after stripping the physical header.
        data_len: usize,
        /// A different-sequence first half was replaced.
        replaced_pending: bool,
        /// Absolute monotonic tick at which this fragment becomes stale.
        deadline_ticks: u64,
    },
    /// A complete packet at or below the base 500-byte RNS MTU.
    Packet {
        /// Valid bytes in the caller-owned output buffer.
        packet_len: usize,
        /// Conservative signal metadata for the complete packet.
        signal: FrameSignal,
        /// A pending split first half was discarded by a non-split frame.
        discarded_pending: bool,
    },
}

/// Explicit receive rejection from framing or the independent RNS MTU guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimedReceiveError {
    /// The physical RNode frame or reassembly operation failed.
    Framing(ReceiveError),
    /// A valid physical-interface packet exceeded the base RNS MTU.
    RnsPacketTooLong {
        /// Reassembled packet length.
        actual: usize,
        /// Maximum packet length admitted to the RNS core.
        maximum: usize,
    },
}

/// Fixed-size, allocation-free receive diagnostics.
///
/// Counters saturate instead of wrapping. Last-observation fields are replaced
/// in place and never retain packet data.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RxDiagnostics {
    /// Physical frames offered to [`TimedRnodeRx::feed`].
    pub frames_seen: u64,
    /// Physical framing or reassembly errors.
    pub framing_errors: u64,
    /// First halves admitted to the pending slot.
    pub pending_started: u64,
    /// Pending first halves replaced by another sequence.
    pub pending_replaced: u64,
    /// Pending first halves removed at or after their deadline.
    pub pending_expired: u64,
    /// Pending first halves discarded by a complete non-split frame.
    pub pending_discarded: u64,
    /// Packets completed within the 508-byte physical interface capacity.
    pub packets_completed: u64,
    /// Completed packets admitted by the 500-byte RNS MTU guard.
    pub packets_accepted: u64,
    /// Completed packets rejected by the 500-byte RNS MTU guard.
    pub packets_too_long: u64,
    /// Length of the most recently offered physical frame.
    pub last_frame_len: u16,
    /// Length of the most recently completed physical-interface packet.
    pub last_packet_len: u16,
    /// Signal metadata for the most recently offered physical frame.
    pub last_frame_signal: Option<FrameSignal>,
    /// Aggregated signal metadata for the most recently completed packet.
    pub last_packet_signal: Option<FrameSignal>,
}

/// Timed receive policy for RNode-compatible LoRa frames.
///
/// This wrapper owns no clock. The caller supplies monotonic ticks, schedules
/// [`Self::expire`] for [`Self::next_deadline`], and supplies the 508-byte
/// packet output buffer. [`Self::feed`] also expires overdue state before
/// consuming a frame, preventing a delayed continuation from reviving stale
/// data if a timer wakeup races with frame delivery.
///
/// Completed physical-interface packets of 501 through 508 bytes are rejected
/// here and must never be passed to the RNS core.
pub struct TimedRnodeRx {
    reassembler: RnodeRxReassembler,
    fragment_timeout_ticks: NonZeroU64,
    pending_deadline: Option<u64>,
    pending_signal: FrameSignal,
    diagnostics: RxDiagnostics,
}

impl TimedRnodeRx {
    /// Construct an empty receive state with an explicit non-zero timeout.
    pub const fn new(fragment_timeout_ticks: NonZeroU64) -> Self {
        Self {
            reassembler: RnodeRxReassembler::new(),
            fragment_timeout_ticks,
            pending_deadline: None,
            pending_signal: FrameSignal::new(0, 0),
            diagnostics: RxDiagnostics {
                frames_seen: 0,
                framing_errors: 0,
                pending_started: 0,
                pending_replaced: 0,
                pending_expired: 0,
                pending_discarded: 0,
                packets_completed: 0,
                packets_accepted: 0,
                packets_too_long: 0,
                last_frame_len: 0,
                last_packet_len: 0,
                last_frame_signal: None,
                last_packet_signal: None,
            },
        }
    }

    /// Configured delay between receiving a first half and its expiry.
    pub const fn fragment_timeout_ticks(&self) -> NonZeroU64 {
        self.fragment_timeout_ticks
    }

    /// Pending RNode fragment, if any.
    pub const fn pending(&self) -> Option<PendingFragment> {
        self.reassembler.pending()
    }

    /// Absolute monotonic deadline for the pending fragment, if any.
    pub const fn next_deadline(&self) -> Option<u64> {
        self.pending_deadline
    }

    /// Return a fixed-size copy of the cumulative receive diagnostics.
    pub const fn diagnostics(&self) -> RxDiagnostics {
        self.diagnostics
    }

    /// Expire pending state when `now_ticks` reaches its deadline.
    ///
    /// The caller must use one monotonic tick epoch and ensure it does not wrap
    /// during this object's lifetime. Deadline addition saturates at
    /// [`u64::MAX`]. Calling this before the deadline has no effect.
    pub fn expire(&mut self, now_ticks: u64) -> Option<ExpiredFragment> {
        let deadline_ticks = self.pending_deadline?;
        if now_ticks < deadline_ticks {
            return None;
        }

        self.pending_deadline = None;
        let fragment = self.reassembler.expire_pending()?;
        saturating_increment(&mut self.diagnostics.pending_expired);
        Some(ExpiredFragment {
            fragment,
            deadline_ticks,
            signal: self.pending_signal,
        })
    }

    /// Consume one physical frame and optionally complete one RNS packet.
    ///
    /// `output` remains caller-owned and has the exact RNode hardware capacity.
    /// A successful [`TimedReceiveOutcome::Packet`] is the only result that
    /// authorizes the caller to pass `output[..packet_len]` to an RNS core.
    pub fn feed(
        &mut self,
        frame: &[u8],
        now_ticks: u64,
        signal: FrameSignal,
        output: &mut [u8; RNODE_HW_MTU],
    ) -> Result<TimedReceiveOutcome, TimedReceiveError> {
        let _ = self.expire(now_ticks);

        saturating_increment(&mut self.diagnostics.frames_seen);
        self.diagnostics.last_frame_len = u16::try_from(frame.len()).unwrap_or(u16::MAX);
        self.diagnostics.last_frame_signal = Some(signal);

        let pending_before = self.reassembler.pending();
        let outcome = match self.reassembler.feed(frame, output) {
            Ok(outcome) => outcome,
            Err(error) => {
                saturating_increment(&mut self.diagnostics.framing_errors);
                if self.reassembler.pending().is_none() {
                    self.pending_deadline = None;
                }
                return Err(TimedReceiveError::Framing(error));
            }
        };

        match outcome {
            ReceiveOutcome::AwaitingContinuation {
                sequence,
                data_len,
                replaced_pending,
            } => {
                saturating_increment(&mut self.diagnostics.pending_started);
                if replaced_pending {
                    saturating_increment(&mut self.diagnostics.pending_replaced);
                }
                self.pending_signal = signal;
                let deadline_ticks = now_ticks.saturating_add(self.fragment_timeout_ticks.get());
                self.pending_deadline = Some(deadline_ticks);
                Ok(TimedReceiveOutcome::AwaitingContinuation {
                    sequence,
                    data_len,
                    replaced_pending,
                    deadline_ticks,
                })
            }
            ReceiveOutcome::Complete {
                packet_len,
                discarded_pending,
            } => {
                let packet_signal = if pending_before.is_some() && !discarded_pending {
                    self.pending_signal.weakest(signal)
                } else {
                    signal
                };
                self.pending_deadline = None;

                if discarded_pending {
                    saturating_increment(&mut self.diagnostics.pending_discarded);
                }
                saturating_increment(&mut self.diagnostics.packets_completed);
                self.diagnostics.last_packet_len = u16::try_from(packet_len).unwrap_or(u16::MAX);
                self.diagnostics.last_packet_signal = Some(packet_signal);

                if packet_len > RNS_MTU {
                    saturating_increment(&mut self.diagnostics.packets_too_long);
                    return Err(TimedReceiveError::RnsPacketTooLong {
                        actual: packet_len,
                        maximum: RNS_MTU,
                    });
                }

                saturating_increment(&mut self.diagnostics.packets_accepted);
                Ok(TimedReceiveOutcome::Packet {
                    packet_len,
                    signal: packet_signal,
                    discarded_pending,
                })
            }
        }
    }
}

fn saturating_increment(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RNODE_LORA_DATA_PER_FRAME, SX1262_FRAME_MTU};

    const TIMEOUT: NonZeroU64 = NonZeroU64::new(10).unwrap();

    fn signal(rssi_dbm: i16, snr_db: i16) -> FrameSignal {
        FrameSignal::new(rssi_dbm, snr_db)
    }

    fn split_frame(sequence: u8, data: &[u8]) -> [u8; SX1262_FRAME_MTU] {
        let mut frame = [0u8; SX1262_FRAME_MTU];
        frame[0] = (sequence << 4) | 0x01;
        frame[1..=data.len()].copy_from_slice(data);
        frame
    }

    #[test]
    fn continuation_before_deadline_completes() {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];

        assert_eq!(
            rx.feed(&[0x21, 1, 2], 100, signal(-70, 8), &mut output),
            Ok(TimedReceiveOutcome::AwaitingContinuation {
                sequence: 2,
                data_len: 2,
                replaced_pending: false,
                deadline_ticks: 110,
            })
        );
        assert_eq!(rx.expire(109), None);
        assert_eq!(
            rx.feed(&[0x21, 3, 4], 109, signal(-72, 7), &mut output),
            Ok(TimedReceiveOutcome::Packet {
                packet_len: 4,
                signal: signal(-72, 7),
                discarded_pending: false,
            })
        );
        assert_eq!(&output[..4], &[1, 2, 3, 4]);
        assert_eq!(rx.next_deadline(), None);
    }

    #[test]
    fn exact_deadline_expires_and_stale_continuation_cannot_combine() {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];

        rx.feed(&[0x31, 1, 2], 20, signal(-80, 2), &mut output)
            .unwrap();
        assert_eq!(rx.next_deadline(), Some(30));

        assert_eq!(
            rx.feed(&[0x31, 3, 4], 30, signal(-81, 1), &mut output),
            Ok(TimedReceiveOutcome::AwaitingContinuation {
                sequence: 3,
                data_len: 2,
                replaced_pending: false,
                deadline_ticks: 40,
            })
        );
        assert_eq!(rx.pending().unwrap().data_len, 2);
        assert_eq!(rx.diagnostics().pending_expired, 1);

        assert_eq!(
            rx.feed(&[0x31, 5], 39, signal(-82, 0), &mut output),
            Ok(TimedReceiveOutcome::Packet {
                packet_len: 3,
                signal: signal(-82, 0),
                discarded_pending: false,
            })
        );
        assert_eq!(&output[..3], &[3, 4, 5]);
    }

    #[test]
    fn explicit_expiry_reports_fragment_deadline_and_signal() {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];
        let first_signal = signal(-91, -4);

        rx.feed(&[0xa1, 9, 8, 7], 5, first_signal, &mut output)
            .unwrap();
        assert_eq!(rx.expire(14), None);
        assert_eq!(
            rx.expire(15),
            Some(ExpiredFragment {
                fragment: PendingFragment {
                    sequence: 10,
                    data_len: 3,
                },
                deadline_ticks: 15,
                signal: first_signal,
            })
        );
        assert_eq!(rx.pending(), None);
        assert_eq!(rx.next_deadline(), None);
    }

    #[test]
    fn replacement_refreshes_deadline_and_signal() {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];

        rx.feed(&[0x11, 1], 10, signal(-70, 8), &mut output)
            .unwrap();
        assert_eq!(
            rx.feed(&[0x21, 2], 15, signal(-90, -3), &mut output),
            Ok(TimedReceiveOutcome::AwaitingContinuation {
                sequence: 2,
                data_len: 1,
                replaced_pending: true,
                deadline_ticks: 25,
            })
        );
        assert_eq!(rx.expire(20), None);
        assert_eq!(rx.expire(25).unwrap().signal, signal(-90, -3));
        assert_eq!(rx.diagnostics().pending_replaced, 1);
    }

    #[test]
    fn non_split_frame_clears_pending_deadline() {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];

        rx.feed(&[0x41, 1, 2], 0, signal(-60, 10), &mut output)
            .unwrap();
        assert_eq!(
            rx.feed(&[0x50, 7, 8, 9], 1, signal(-75, 4), &mut output),
            Ok(TimedReceiveOutcome::Packet {
                packet_len: 3,
                signal: signal(-75, 4),
                discarded_pending: true,
            })
        );
        assert_eq!(&output[..3], &[7, 8, 9]);
        assert_eq!(rx.pending(), None);
        assert_eq!(rx.next_deadline(), None);
        assert_eq!(rx.diagnostics().pending_discarded, 1);
    }

    fn assert_packet_len_is_accepted(packet_len: usize) {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];
        let first_len = packet_len.min(RNODE_LORA_DATA_PER_FRAME);
        let first_data = [0x5a; RNODE_LORA_DATA_PER_FRAME];

        if packet_len <= RNODE_LORA_DATA_PER_FRAME {
            let mut frame = [0u8; SX1262_FRAME_MTU];
            frame[0] = 0x70;
            frame[1..=first_len].copy_from_slice(&first_data[..first_len]);
            assert_eq!(
                rx.feed(&frame[..first_len + 1], 0, signal(-70, 5), &mut output),
                Ok(TimedReceiveOutcome::Packet {
                    packet_len,
                    signal: signal(-70, 5),
                    discarded_pending: false,
                })
            );
        } else {
            let first = split_frame(7, &first_data);
            rx.feed(&first, 0, signal(-70, 5), &mut output).unwrap();

            let second_len = packet_len - first_len;
            let second_data = [0xa5; RNODE_LORA_DATA_PER_FRAME];
            let second = split_frame(7, &second_data[..second_len]);
            assert_eq!(
                rx.feed(&second[..second_len + 1], 1, signal(-72, 3), &mut output,),
                Ok(TimedReceiveOutcome::Packet {
                    packet_len,
                    signal: signal(-72, 3),
                    discarded_pending: false,
                })
            );
        }
        assert_eq!(rx.diagnostics().packets_accepted, 1);
    }

    #[test]
    fn rns_boundary_lengths_are_accepted() {
        for packet_len in [0, 1, 253, 254, 255, 256, 499, 500] {
            assert_packet_len_is_accepted(packet_len);
        }
    }

    fn assert_packet_len_is_rejected(packet_len: usize) {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];
        let first_data = [0x11; RNODE_LORA_DATA_PER_FRAME];
        let second_data = [0x22; RNODE_LORA_DATA_PER_FRAME];
        let first = split_frame(9, &first_data);
        let second_len = packet_len - RNODE_LORA_DATA_PER_FRAME;
        let second = split_frame(9, &second_data[..second_len]);

        rx.feed(&first, 0, signal(-80, 1), &mut output).unwrap();
        assert_eq!(
            rx.feed(&second[..second_len + 1], 1, signal(-82, 0), &mut output,),
            Err(TimedReceiveError::RnsPacketTooLong {
                actual: packet_len,
                maximum: RNS_MTU,
            })
        );
        assert_eq!(rx.diagnostics().packets_completed, 1);
        assert_eq!(rx.diagnostics().packets_accepted, 0);
        assert_eq!(rx.diagnostics().packets_too_long, 1);
        assert_eq!(rx.pending(), None);
    }

    #[test]
    fn physical_packets_above_rns_mtu_are_rejected_independently() {
        for packet_len in [501, 507, 508] {
            assert_packet_len_is_rejected(packet_len);
        }
    }

    #[test]
    fn physical_frame_above_sx1262_limit_is_rejected() {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];
        let frame = [0u8; SX1262_FRAME_MTU + 1];

        assert!(matches!(
            rx.feed(&frame, 0, signal(-70, 2), &mut output),
            Err(TimedReceiveError::Framing(ReceiveError::FrameTooLong(_)))
        ));
        assert_eq!(rx.diagnostics().frames_seen, 1);
        assert_eq!(rx.diagnostics().framing_errors, 1);
    }

    #[test]
    fn split_signal_uses_independent_minima() {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];

        rx.feed(&[0x61, 1], 0, signal(-95, 6), &mut output).unwrap();
        assert_eq!(
            rx.feed(&[0x61, 2], 1, signal(-70, -5), &mut output),
            Ok(TimedReceiveOutcome::Packet {
                packet_len: 2,
                signal: signal(-95, -5),
                discarded_pending: false,
            })
        );
        assert_eq!(rx.diagnostics().last_packet_signal, Some(signal(-95, -5)));
    }

    #[test]
    fn diagnostic_counters_saturate() {
        let mut rx = TimedRnodeRx::new(TIMEOUT);
        let mut output = [0u8; RNODE_HW_MTU];
        rx.diagnostics.frames_seen = u64::MAX;
        rx.diagnostics.pending_started = u64::MAX;
        rx.diagnostics.pending_expired = u64::MAX;

        rx.feed(&[0x11, 1], 0, signal(-70, 2), &mut output).unwrap();
        rx.expire(10).unwrap();

        assert_eq!(rx.diagnostics().frames_seen, u64::MAX);
        assert_eq!(rx.diagnostics().pending_started, u64::MAX);
        assert_eq!(rx.diagnostics().pending_expired, u64::MAX);
    }
}
