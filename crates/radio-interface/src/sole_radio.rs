//! Static-dispatch ownership boundary for one logical RNode radio.

use core::{future::Future, num::NonZeroU64};

use crate::{FrameSignal, LoRaProfile, RnodeTxFrames, SX1262_FRAME_MTU};

/// Opaque identity of one complete immutable radio configuration.
///
/// This fingerprint binds every transmit-relevant setting owned by a concrete
/// radio adapter, including modulation, packet parameters, frequency, power,
/// sync/network selection, regulator, PA, RF-switch and front-end policy. It
/// is not merely an airtime-profile identifier. Changing any such setting
/// requires a new fingerprint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RadioConfigurationFingerprint([u8; 16]);

impl RadioConfigurationFingerprint {
    /// Construct an implementation-owned stable fingerprint.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Fixed bytes suitable for binding into an authorization request.
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Stable class for a board-neutral receive, CAD, or transmit failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SoleRadioFaultClass {
    /// SPI command or bus failure.
    Spi,
    /// Radio reset-pin or reset-sequence failure.
    Reset,
    /// Integrated RF-switch or external front-end transition failure.
    RfSwitch,
    /// BUSY pin or ready-wait failure.
    Busy,
    /// IRQ status processing or interrupt-pin failure.
    Interrupt,
    /// Static modulation, mode, frequency, or power configuration failure.
    Configuration,
    /// Radio-reported operational error.
    Operation,
    /// Physical-frame length or payload-buffer contract failure.
    Payload,
    /// Receive-operation, CAD, or transmit timeout reported as a fault.
    ///
    /// An expected receive symbol timeout before preamble detection is instead
    /// [`BoundedRxOutcome::NoPreambleTimeout`] and never uses this class. A
    /// continuous receive scheduler yield is likewise a normal outcome.
    Timeout,
    /// Requested radio capability is unsupported.
    Unsupported,
    /// The final DIO observation timestamp was unavailable.
    Timestamp,
    /// An operation was attempted after the sole owner had faulted.
    Faulted,
}

/// Stable operation phase in which a sole-radio failure was observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SoleRadioFaultPhase {
    /// Modem and IRQ preparation for channel activity detection.
    ChannelActivityPreparation,
    /// Channel activity detection and its DIO/IRQ completion.
    ChannelActivityDetection,
    /// Explicit standby and RF-path cleanup after CAD.
    ChannelActivityCleanup,
    /// Modem and IRQ preparation for one bounded preamble-search window.
    ReceivePreparation,
    /// Preamble search, frame reception, and receive IRQ processing.
    Receive,
    /// Explicit standby and RF-path cleanup after a received frame.
    ReceiveCleanup,
    /// Capture of a successful receive observation.
    ReceiveTimestampCapture,
    /// Validation of the one or two physical RNode frames.
    PacketValidation,
    /// FIFO, packet, channel, and power preparation for one frame.
    FramePreparation,
    /// Physical transmission and its final DIO/IRQ completion.
    FrameTransmission,
    /// Explicit standby and RF-path cleanup after one frame.
    FrameCleanup,
    /// Capture of a successful CAD or frame-completion observation.
    TimestampCapture,
}

/// Copyable fault contract consumed by a generic sole-radio dispatcher.
pub trait SoleRadioFault: Copy + core::fmt::Debug + Eq {
    /// Stable phase in which the fault occurred.
    fn phase(self) -> SoleRadioFaultPhase;

    /// Stable implementation-independent fault class.
    fn class(self) -> SoleRadioFaultClass;
}

/// Minimal project-owned fault summary suitable for board adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SoleRadioFaultSummary {
    phase: SoleRadioFaultPhase,
    class: SoleRadioFaultClass,
}

impl SoleRadioFaultSummary {
    /// Construct a stable phase/class summary without retaining driver errors.
    pub const fn new(phase: SoleRadioFaultPhase, class: SoleRadioFaultClass) -> Self {
        Self { phase, class }
    }
}

impl SoleRadioFault for SoleRadioFaultSummary {
    fn phase(self) -> SoleRadioFaultPhase {
        self.phase
    }

    fn class(self) -> SoleRadioFaultClass {
        self.class
    }
}

/// Result of one completed channel activity detection operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CadObservation {
    activity_detected: bool,
    observed_at_us: u64,
}

/// Metadata for one frame received into a caller-owned SX1262 buffer.
///
/// The length remains deliberately wider than the 255-byte physical buffer so
/// the sole-radio dispatcher can validate an adapter result before exposing
/// any bytes to ingress. Board adapters must report their driver result
/// without truncating it to fit this contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundedRxObservation {
    len: usize,
    signal: FrameSignal,
    received_at_us: u64,
}

impl BoundedRxObservation {
    /// Construct a receive observation for bytes already written to the caller's buffer.
    pub const fn new(len: usize, signal: FrameSignal, received_at_us: u64) -> Self {
        Self {
            len,
            signal,
            received_at_us,
        }
    }

    /// Driver-reported valid byte count, pending dispatcher validation.
    pub const fn len(self) -> usize {
        self.len
    }

    /// Whether the physical frame contains no bytes.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Packet RSSI and SNR reported by the radio.
    pub const fn signal(self) -> FrameSignal {
        self.signal
    }

    /// Monotonic microseconds sampled when the receive DIO wait resumed.
    pub const fn received_at_us(self) -> u64 {
        self.received_at_us
    }

    /// Borrow only the driver-reported prefix of a matching receive buffer.
    ///
    /// Returns `None` for an untrusted adapter length outside the fixed
    /// physical capacity. A dispatcher validates this before forwarding the
    /// observation, but keeping this method total also makes raw adapter tests
    /// safe. A zero-length frame yields an empty slice so ingress can retain
    /// the observation for diagnostics before framing rejects it.
    pub fn payload(self, buffer: &[u8; SX1262_FRAME_MTU]) -> Option<&[u8]> {
        buffer.get(..self.len)
    }
}

/// Normal result of one receive operation with bounded preamble search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundedRxOutcome {
    /// A physical frame was received into the supplied buffer.
    Frame(BoundedRxObservation),
    /// The configured symbol timeout expired before a preamble was detected.
    ///
    /// This outcome supports radios that expose finite hardware receive
    /// windows. The E290 path uses [`Self::SchedulerYield`] while leaving the
    /// modem in continuous receive.
    NoPreambleTimeout,
    /// The caller-provided scheduler future completed before a receive IRQ.
    ///
    /// The radio remains continuously armed. Callers may inspect queued TX
    /// work and then resume waiting without reconfiguring or restarting RX.
    SchedulerYield,
    /// A corrupt, terminally timed-out, or stalled-progress frame was discarded.
    ///
    /// Continuous RX remains armed (or has been rearmed after stalled
    /// progress), and the caller-owned buffer contains no admissible frame.
    InvalidFrame,
}

impl CadObservation {
    /// Bind a CAD result to the monotonic sample taken when its DIO wait resumed.
    pub const fn new(activity_detected: bool, observed_at_us: u64) -> Self {
        Self {
            activity_detected,
            observed_at_us,
        }
    }

    /// Whether the configured LoRa signal was detected.
    pub const fn activity_detected(self) -> bool {
        self.activity_detected
    }

    /// Monotonic microseconds sampled when the final CAD DIO wait resumed.
    pub const fn observed_at_us(self) -> u64 {
        self.observed_at_us
    }
}

/// Completed physical-frame progress for one logical packet transmission.
///
/// A frame becomes completed when its final transmit DIO wait and `TxDone`
/// processing succeed. Timestamp capture is separate evidence: a completed
/// frame can therefore retain a missing timestamp without being misclassified
/// as untransmitted. A second completion can never exist without the first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketTxProgress {
    state: PacketTxProgressState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PacketTxProgressState {
    None,
    FirstCompleted {
        completed_at_us: Option<u64>,
    },
    BothCompleted {
        first_completed_at_us: u64,
        second_completed_at_us: Option<u64>,
    },
}

impl PacketTxProgress {
    /// No physical frame is known to have completed.
    pub const fn none() -> Self {
        Self {
            state: PacketTxProgressState::None,
        }
    }

    /// Exactly the first physical frame completed.
    pub const fn first_completed(first_completed_at_us: u64) -> Self {
        Self {
            state: PacketTxProgressState::FirstCompleted {
                completed_at_us: Some(first_completed_at_us),
            },
        }
    }

    /// The first physical frame completed, but its observation timestamp was lost.
    pub const fn first_completed_timestamp_missing() -> Self {
        Self {
            state: PacketTxProgressState::FirstCompleted {
                completed_at_us: None,
            },
        }
    }

    /// Both physical frames of a split packet completed in order.
    pub const fn both_completed(first_completed_at_us: u64, second_completed_at_us: u64) -> Self {
        Self {
            state: PacketTxProgressState::BothCompleted {
                first_completed_at_us,
                second_completed_at_us: Some(second_completed_at_us),
            },
        }
    }

    /// Both physical frames completed, but the second observation timestamp was lost.
    ///
    /// The first timestamp remains mandatory because a missing first timestamp
    /// terminates the packet operation before a second frame can start.
    pub const fn both_completed_second_timestamp_missing(first_completed_at_us: u64) -> Self {
        Self {
            state: PacketTxProgressState::BothCompleted {
                first_completed_at_us,
                second_completed_at_us: None,
            },
        }
    }

    /// Number of physical frames known to have completed.
    pub const fn completed_frame_count(self) -> u8 {
        match self.state {
            PacketTxProgressState::None => 0,
            PacketTxProgressState::FirstCompleted { .. } => 1,
            PacketTxProgressState::BothCompleted { .. } => 2,
        }
    }

    /// Completion timestamp for a zero-based physical-frame index.
    pub const fn frame_completed_at_us(self, index: usize) -> Option<u64> {
        match (self.state, index) {
            (PacketTxProgressState::FirstCompleted { completed_at_us }, 0) => completed_at_us,
            (
                PacketTxProgressState::BothCompleted {
                    first_completed_at_us,
                    ..
                },
                0,
            ) => Some(first_completed_at_us),
            (
                PacketTxProgressState::BothCompleted {
                    second_completed_at_us,
                    ..
                },
                1,
            ) => second_completed_at_us,
            (PacketTxProgressState::None, _)
            | (PacketTxProgressState::FirstCompleted { .. }, _)
            | (PacketTxProgressState::BothCompleted { .. }, _) => None,
        }
    }

    /// Whether a completed frame is missing its completion timestamp.
    ///
    /// Returns `false` for a frame that has not completed or is outside the
    /// one/two-frame logical-packet range.
    pub const fn frame_completion_timestamp_missing(self, index: usize) -> bool {
        index < self.completed_frame_count() as usize && self.frame_completed_at_us(index).is_none()
    }
}

impl Default for PacketTxProgress {
    fn default() -> Self {
        Self::none()
    }
}

/// Successful completion of every physical frame in one logical packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketTxObservation {
    progress: PacketTxProgress,
}

impl PacketTxObservation {
    /// Complete a one-frame logical packet.
    pub const fn one_frame(completed_at_us: u64) -> Self {
        Self {
            progress: PacketTxProgress::first_completed(completed_at_us),
        }
    }

    /// Complete a two-frame logical packet.
    pub const fn two_frames(first_completed_at_us: u64, second_completed_at_us: u64) -> Self {
        Self {
            progress: PacketTxProgress::both_completed(
                first_completed_at_us,
                second_completed_at_us,
            ),
        }
    }

    /// Per-frame completion progress and DIO-resume timestamps.
    pub const fn progress(self) -> PacketTxProgress {
        self.progress
    }
}

/// Logical-packet TX failure with retained completed-frame progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketTxFault<Fault> {
    fault: Fault,
    progress: PacketTxProgress,
}

impl<Fault> PacketTxFault<Fault> {
    /// Preserve the portable fault and every frame already known to have completed.
    pub const fn new(fault: Fault, progress: PacketTxProgress) -> Self {
        Self { fault, progress }
    }

    /// Underlying board adapter fault.
    pub const fn fault(&self) -> &Fault {
        &self.fault
    }

    /// Per-frame completion progress before the failure.
    pub const fn progress(&self) -> PacketTxProgress {
        self.progress
    }

    /// Consume this wrapper and return the underlying fault.
    pub fn into_fault(self) -> Fault {
        self.fault
    }
}

/// Sole static-dispatch owner of one RNode-compatible LoRa radio.
///
/// This boundary deliberately exposes complete logical packets rather than a
/// public physical-frame transmit primitive. One [`Self::transmit`] call owns
/// the radio across both frames of a split packet, so no other operation can
/// interleave in the split-frame gap.
///
/// Calling [`Self::receive_bounded`], [`Self::cad`], or [`Self::transmit`] must
/// synchronously move active hardware ownership into the returned future.
/// Dropping that future at any point, including before its first poll, must
/// contain the RF path and leave [`Self::is_active`] false. Any returned error
/// has the same terminal effect; a frame, no-preamble timeout, or other normal
/// success is the only path allowed to restore active ownership.
///
/// The receive symbol timeout bounds preamble search only. Once a preamble is
/// detected, an SX1262 can suspend that timer while the frame completes. A
/// caller that imposes a whole-operation watchdog must therefore treat expiry
/// as cancellation requiring fail-closed recovery, never as a normal
/// no-preamble result. Implementations must sample every observation in
/// monotonic microseconds, in the same clock domain passed to logical packet
/// channel access.
pub trait SoleRnodeRadio {
    /// Copyable project-owned fault exposed to the generic dispatcher.
    type Fault: SoleRadioFault;

    /// Full immutable configuration actually initialized by this radio owner.
    fn configuration_fingerprint(&self) -> RadioConfigurationFingerprint;

    /// Validated modulation/packet profile actually used for airtime.
    fn airtime_profile(&self) -> LoRaProfile;

    /// Conservative watchdog bound for one complete receive operation.
    ///
    /// This non-zero, configuration-fingerprint-bound value must cover the
    /// configured preamble-symbol search, reception of a maximum 255-byte
    /// physical frame after preamble detection, IRQ/status SPI work, standby
    /// cleanup, and a board-qualified scheduling margin. The generic API
    /// cannot derive the latter hardware/driver margins from modulation alone.
    /// Watchdog expiry cancels and destroys the radio owner; it must never be
    /// reported as [`BoundedRxOutcome::NoPreambleTimeout`].
    fn maximum_receive_operation_us(&self) -> NonZeroU64;

    /// Receive one frame using the adapter's bounded preamble-search policy.
    ///
    /// The caller owns the fixed physical-frame buffer across the complete
    /// operation. A normal no-preamble timeout must restore usable radio
    /// ownership; every error and every dropped future must leave it inactive.
    fn receive_bounded<'a>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
    ) -> impl Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a;

    /// Wait in persistent continuous RX until a frame or scheduler yield.
    ///
    /// Implementations must configure and start continuous receive at most
    /// once per uninterrupted RX session, and must leave it armed after a
    /// frame, invalid-frame terminal, or scheduler yield. The supplied future
    /// may be raced only against the implementation's documented
    /// cancellation-safe IRQ wait. Once an IRQ is observed, all SPI and IRQ
    /// processing must complete without selecting or polling this future.
    /// Observing preamble/header progress locks the operation until a terminal
    /// receive IRQ, fault, or the separate progress-deadline future. That
    /// future must begin measuring only when first polled. If it wins, the
    /// implementation must rearm continuous RX before returning a recoverable
    /// invalid-frame outcome; it must never reinterpret the deadline as TX
    /// permission while the receive state is still latched.
    ///
    /// CAD or TX must explicitly quiesce continuous RX, disable its IRQ
    /// routing, and clear pending receive flags before selecting a new mode.
    fn receive_continuous_until<'a, SchedulerYield, ProgressDeadline>(
        &'a mut self,
        buffer: &'a mut [u8; SX1262_FRAME_MTU],
        scheduler_yield: SchedulerYield,
        progress_deadline: ProgressDeadline,
    ) -> impl Future<Output = Result<BoundedRxOutcome, Self::Fault>> + 'a
    where
        SchedulerYield: Future<Output = ()> + 'a,
        ProgressDeadline: Future<Output = ()> + 'a;

    /// Mark the current continuous-RX epoch untrusted without touching hardware.
    ///
    /// A scheduler calls this when it stops draining RX in order to own a TX
    /// job. Before the next receive result can be admitted, the implementation
    /// must quiesce the old session, clear pending IRQ/FIFO state, and start a
    /// new continuous receive epoch. CAD/TX may perform that quiescence as part
    /// of their own explicit mode transition.
    fn invalidate_receive_session(&mut self);

    /// Run one low-level LoRa channel activity detection operation.
    fn cad(&mut self) -> impl Future<Output = Result<CadObservation, Self::Fault>> + '_;

    /// Transmit every physical frame of one logical RNode packet in order.
    fn transmit<'a>(
        &'a mut self,
        frames: RnodeTxFrames<'a>,
    ) -> impl Future<Output = Result<PacketTxObservation, PacketTxFault<Self::Fault>>> + 'a;

    /// Permanently contain the RF path and discard initialized radio ownership.
    fn shutdown(&mut self);

    /// Whether this wrapper still owns initialized, usable radio hardware.
    fn is_active(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_cannot_represent_a_second_completion_without_the_first() {
        let none = PacketTxProgress::none();
        assert_eq!(none.completed_frame_count(), 0);
        assert_eq!(none.frame_completed_at_us(0), None);
        assert_eq!(none.frame_completed_at_us(1), None);
        assert_eq!(none.frame_completed_at_us(2), None);

        let first = PacketTxProgress::first_completed(11);
        assert_eq!(first.completed_frame_count(), 1);
        assert_eq!(first.frame_completed_at_us(0), Some(11));
        assert_eq!(first.frame_completed_at_us(1), None);
        assert!(!first.frame_completion_timestamp_missing(0));

        let missing_first = PacketTxProgress::first_completed_timestamp_missing();
        assert_eq!(missing_first.completed_frame_count(), 1);
        assert_eq!(missing_first.frame_completed_at_us(0), None);
        assert!(missing_first.frame_completion_timestamp_missing(0));
        assert!(!missing_first.frame_completion_timestamp_missing(1));

        let both = PacketTxProgress::both_completed(11, 22);
        assert_eq!(both.completed_frame_count(), 2);
        assert_eq!(both.frame_completed_at_us(0), Some(11));
        assert_eq!(both.frame_completed_at_us(1), Some(22));
        assert!(!both.frame_completion_timestamp_missing(0));
        assert!(!both.frame_completion_timestamp_missing(1));

        let missing_second = PacketTxProgress::both_completed_second_timestamp_missing(11);
        assert_eq!(missing_second.completed_frame_count(), 2);
        assert_eq!(missing_second.frame_completed_at_us(0), Some(11));
        assert_eq!(missing_second.frame_completed_at_us(1), None);
        assert!(!missing_second.frame_completion_timestamp_missing(0));
        assert!(missing_second.frame_completion_timestamp_missing(1));
    }

    #[test]
    fn portable_observations_and_faults_retain_only_fixed_summaries() {
        let cad = CadObservation::new(false, 31);
        assert!(!cad.activity_detected());
        assert_eq!(cad.observed_at_us(), 31);

        let receive = BoundedRxObservation::new(7, FrameSignal::new(-91, 8), 37);
        assert_eq!(receive.len(), 7);
        assert!(!receive.is_empty());
        assert_eq!(receive.signal(), FrameSignal::new(-91, 8));
        assert_eq!(receive.received_at_us(), 37);
        let buffer = [0x5a; SX1262_FRAME_MTU];
        assert_eq!(receive.payload(&buffer), Some(&buffer[..7]));
        assert_eq!(
            BoundedRxObservation::new(SX1262_FRAME_MTU + 1, receive.signal(), 37).payload(&buffer),
            None
        );
        assert_eq!(
            BoundedRxOutcome::Frame(receive),
            BoundedRxOutcome::Frame(receive)
        );
        assert_ne!(
            BoundedRxOutcome::Frame(receive),
            BoundedRxOutcome::NoPreambleTimeout
        );

        let observation = PacketTxObservation::two_frames(41, 52);
        assert_eq!(observation.progress().completed_frame_count(), 2);

        let summary = SoleRadioFaultSummary::new(
            SoleRadioFaultPhase::FrameCleanup,
            SoleRadioFaultClass::RfSwitch,
        );
        let fault = PacketTxFault::new(summary, PacketTxProgress::first_completed(41));
        assert_eq!(fault.fault().phase(), SoleRadioFaultPhase::FrameCleanup);
        assert_eq!(fault.fault().class(), SoleRadioFaultClass::RfSwitch);
        assert_eq!(fault.progress().completed_frame_count(), 1);
    }
}
