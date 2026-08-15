//! Fixed-size production diagnostics for the permanent E290 LoRa actor.
//!
//! The radio actor is the sole writer. Other product owners may copy one
//! coherent snapshot through the short critical-section boundary without
//! retaining packet bytes, driver errors, or heap-backed state.

use core::{cell::RefCell, num::NonZeroU64};

use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use reticulum_board_e290_radio::E290RadioConfiguration;
use reticulum_node_core::{EncodedPacketSha256, PacketInterfaceId, rns_packet_hash};
use reticulum_radio_interface::{FrameSignal, RxDiagnostics};
use reticulum_radio_tx_dispatch::{
    DataPacketDispatchObservation, DispatchFamily, DispatchOutcome, DispatchReport,
};
use sha2::{Digest, Sha256};

use crate::radio_trace::{
    RadioTraceAttemptTerminal, RadioTraceCursor, RadioTraceDataTxTerminal, RadioTraceEvent,
    RadioTraceEventKind, RadioTraceLogicalRx, RadioTracePage, RadioTraceRing,
    RadioTraceRouteSelected,
};

/// Maximum in-memory size admitted for one copyable radio snapshot.
///
/// This bound keeps both critical-section copies and any future API projection
/// small even though all lifetime counters remain 64-bit and saturating.
pub const MAXIMUM_RADIO_DIAGNOSTICS_SNAPSHOT_BYTES: usize = 320;

/// Immutable LoRa settings applied to the running physical-radio owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppliedLoRaProfile {
    /// Complete board-owned configuration fingerprint.
    pub configuration_fingerprint: [u8; 16],
    /// Carrier frequency applied to the SX1262.
    pub frequency_hz: u32,
    /// LoRa modulation bandwidth.
    pub bandwidth_hz: u32,
    /// Configured preamble length.
    pub preamble_symbols: u16,
    /// Requested SX1262 output power, without an antenna-path claim.
    pub requested_power_dbm: i8,
    /// LoRa spreading-factor number.
    pub spreading_factor: u8,
    /// Denominator of the `4/x` LoRa coding rate.
    pub coding_rate_denominator: u8,
    /// Whether the explicit packet header is enabled.
    pub explicit_header: bool,
    /// Whether the packet CRC is enabled.
    pub crc: bool,
    /// Whether LoRa IQ polarity is inverted.
    pub iq_inverted: bool,
}

impl AppliedLoRaProfile {
    fn from_configuration(configuration: E290RadioConfiguration) -> Self {
        let profile = configuration.profile();
        Self {
            configuration_fingerprint: configuration.fingerprint().as_bytes(),
            frequency_hz: profile.frequency_hz(),
            bandwidth_hz: profile.bandwidth().hz(),
            preamble_symbols: profile.preamble_symbols(),
            requested_power_dbm: i8::try_from(configuration.requested_power_dbm())
                .expect("the E290 board policy admits only i8 LoRa power values"),
            spreading_factor: u8::try_from(profile.spreading_factor().factor())
                .expect("the validated E290 LoRa spreading factor fits in u8"),
            coding_rate_denominator: u8::try_from(profile.coding_rate().denom())
                .expect("the validated E290 LoRa coding-rate denominator fits in u8"),
            explicit_header: profile.explicit_header(),
            crc: profile.crc(),
            iq_inverted: profile.iq_inverted(),
        }
    }
}

/// Current permanent LoRa actor lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioRuntimeState {
    /// Hardware configuration is applied but the actor is not online.
    Offline,
    /// The interface lifecycle accepted the actor's generation-bound lease.
    Online,
    /// The actor entered fail-stop containment and performs no further RF work.
    Faulted,
}

/// One accepted logical packet observed atomically with receive counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptedLoRaPacketObservation {
    received_at_us: u64,
    packet_len: u16,
    signal: FrameSignal,
}

impl AcceptedLoRaPacketObservation {
    /// Construct a packet observation with conservative whole-packet signal metadata.
    pub const fn new(received_at_us: u64, packet_len: u16, signal: FrameSignal) -> Self {
        Self {
            received_at_us,
            packet_len,
            signal,
        }
    }
}

/// Cumulative RNode receive-pipeline diagnostics and last accepted packet.
///
/// The counter fields are an exact projection of [`RxDiagnostics`]. The last
/// packet fields advance only for an `Ok(TimedReceiveOutcome::Packet)` result,
/// so a later over-MTU completion cannot replace accepted signal metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoRaReceiveDiagnostics {
    /// Physical frames offered to RNode framing.
    pub frames_seen: u64,
    /// Physical framing or reassembly errors.
    pub framing_errors: u64,
    /// First split frames admitted to the pending slot.
    pub pending_started: u64,
    /// Pending first frames replaced by another sequence.
    pub pending_replaced: u64,
    /// Pending first frames removed at or after their deadline.
    pub pending_expired: u64,
    /// Pending first frames discarded by a complete non-split frame.
    pub pending_discarded: u64,
    /// Logical packets completed within the physical interface capacity.
    pub packets_completed: u64,
    /// Completed packets admitted by the base RNS MTU guard.
    pub packets_accepted: u64,
    /// Completed packets rejected by the base RNS MTU guard.
    pub packets_too_long: u64,
    last_accepted_at_plus_one_us: u64,
    last_accepted_signal: FrameSignal,
    last_accepted_packet_len: u16,
}

impl LoRaReceiveDiagnostics {
    const EMPTY: Self = Self {
        frames_seen: 0,
        framing_errors: 0,
        pending_started: 0,
        pending_replaced: 0,
        pending_expired: 0,
        pending_discarded: 0,
        packets_completed: 0,
        packets_accepted: 0,
        packets_too_long: 0,
        last_accepted_at_plus_one_us: 0,
        last_accepted_signal: FrameSignal::new(0, 0),
        last_accepted_packet_len: 0,
    };

    /// Monotonic completion time of the last accepted logical packet.
    pub fn last_accepted_at_us(self) -> Option<u64> {
        NonZeroU64::new(self.last_accepted_at_plus_one_us)
            .map(|encoded| encoded.get().saturating_sub(1))
    }

    /// Length of the last accepted logical packet.
    pub fn last_accepted_packet_len(self) -> Option<u16> {
        self.last_accepted_at_us()
            .map(|_| self.last_accepted_packet_len)
    }

    /// Conservative RSSI and SNR for the last accepted logical packet.
    pub fn last_accepted_signal(self) -> Option<FrameSignal> {
        self.last_accepted_at_us()
            .map(|_| self.last_accepted_signal)
    }

    fn replace_pipeline_counters(&mut self, diagnostics: RxDiagnostics) {
        self.frames_seen = diagnostics.frames_seen;
        self.framing_errors = diagnostics.framing_errors;
        self.pending_started = diagnostics.pending_started;
        self.pending_replaced = diagnostics.pending_replaced;
        self.pending_expired = diagnostics.pending_expired;
        self.pending_discarded = diagnostics.pending_discarded;
        self.packets_completed = diagnostics.packets_completed;
        self.packets_accepted = diagnostics.packets_accepted;
        self.packets_too_long = diagnostics.packets_too_long;
    }

    fn record_accepted(&mut self, observation: AcceptedLoRaPacketObservation) {
        self.last_accepted_at_plus_one_us = encode_optional_timestamp(observation.received_at_us);
        self.last_accepted_packet_len = observation.packet_len;
        self.last_accepted_signal = observation.signal;
    }
}

/// Cumulative actor-to-ingress handoff attempts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoRaIngressDiagnostics {
    /// Packets successfully retained by the interface ingress queue.
    pub enqueued: u64,
    /// Enqueue attempts deferred because the bounded queue was full.
    pub deferred: u64,
    /// Packets rejected by sealing, capacity, or a terminal actor error.
    pub failed: u64,
}

impl LoRaIngressDiagnostics {
    const EMPTY: Self = Self {
        enqueued: 0,
        deferred: 0,
        failed: 0,
    };
}

/// Cumulative channel-activity observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoRaCadDiagnostics {
    /// CAD operations that observed a clear channel.
    pub clear: u64,
    /// CAD operations that detected configured LoRa activity.
    pub busy: u64,
}

impl LoRaCadDiagnostics {
    const EMPTY: Self = Self { clear: 0, busy: 0 };
}

/// Stable, bounded classification of the last terminal dispatch outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTxTerminalOutcome {
    /// Every planned physical frame completed successfully.
    Transmitted,
    /// Initial channel access rejected the logical packet.
    AccessRejected,
    /// The node owner denied the exact permit request.
    PermitDenied,
    /// A matching authorization arrived after its deadline.
    AuthorizationExpired,
    /// Fresh post-grant channel access rejected the logical packet.
    PostGrantAccessRejected,
    /// Airtime could not be calculated or admitted.
    AirtimeRejected,
    /// A dispatch deadline could not be represented.
    DeadlineConversionOverflow,
    /// The sole radio was already inactive.
    RadioInactive,
    /// Router and dispatcher configuration identities differed.
    InterfaceConfigurationMismatch,
    /// Immutable radio configuration changed before permit negotiation.
    RadioConfigurationChangedBeforePermit,
    /// Immutable radio configuration changed after permit negotiation.
    RadioConfigurationChangedAfterPermit,
    /// Channel-activity detection failed.
    CadFault,
    /// Physical transmission failed.
    TxFault,
    /// A permit exchange could not be reconciled.
    ControlPlaneRecovery,
    /// Authorized framing or byte exposure violated an invariant.
    FrameInvariantRecovery,
    /// A dropped CAD or transmit future was explicitly reconciled.
    CancelledRadioOperation,
}

/// Packet-owner family of one terminal radio dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTxDispatchFamily {
    /// Destination DATA owned by one durable message attempt.
    Data,
    /// Ordinary Reticulum control or application packet.
    Ordinary,
}

impl From<DispatchFamily> for RadioTxDispatchFamily {
    fn from(family: DispatchFamily) -> Self {
        match family {
            DispatchFamily::Data => Self::Data,
            DispatchFamily::Ordinary => Self::Ordinary,
        }
    }
}

/// Last terminal DATA dispatch, including pre-authorization packet identity.
///
/// This prepared-packet evidence does not assert that bytes were authorized or
/// transmitted. It is retained separately so a later ordinary packet cannot
/// hide the DATA result an app is trying to correlate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LastDataTxObservation {
    observed_at_plus_one_us: u64,
    outcome: RadioTxTerminalOutcome,
    interface: PacketInterfaceId,
    packet_len: u16,
    encoded_packet_sha256: EncodedPacketSha256,
}

impl LastDataTxObservation {
    fn new(
        observed_at_us: u64,
        outcome: RadioTxTerminalOutcome,
        packet: DataPacketDispatchObservation,
    ) -> Self {
        Self {
            observed_at_plus_one_us: encode_optional_timestamp(observed_at_us),
            outcome,
            interface: packet.interface(),
            packet_len: packet.packet_len(),
            encoded_packet_sha256: packet.encoded_packet_sha256(),
        }
    }

    /// Actor observation time for this terminal DATA report.
    pub fn observed_at_us(self) -> u64 {
        self.observed_at_plus_one_us.saturating_sub(1)
    }

    /// Stable terminal outcome classification.
    pub const fn outcome(self) -> RadioTxTerminalOutcome {
        self.outcome
    }

    /// Exact Reticulum interface selected for this dispatch attempt.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Complete encoded interface-packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// SHA-256 over every byte in the complete encoded interface packet.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }
}

/// Cumulative terminal logical-TX diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoRaTransmitDiagnostics {
    /// Logical TX jobs that reached a terminal [`DispatchReport`].
    pub terminal_jobs: u64,
    /// Terminal jobs that completed every physical frame.
    pub transmitted_jobs: u64,
    /// Jobs rejected by initial or post-grant channel access.
    pub access_rejected_jobs: u64,
    /// Other terminal failures, excluding channel-access rejections.
    pub failed_jobs: u64,
    /// Physical frames known to have completed across terminal reports.
    pub completed_physical_frames: u64,
    last_terminal_at_plus_one_us: u64,
    last_outcome: Option<RadioTxTerminalOutcome>,
    last_family: Option<RadioTxDispatchFamily>,
    last_data: Option<LastDataTxObservation>,
}

impl LoRaTransmitDiagnostics {
    const EMPTY: Self = Self {
        terminal_jobs: 0,
        transmitted_jobs: 0,
        access_rejected_jobs: 0,
        failed_jobs: 0,
        completed_physical_frames: 0,
        last_terminal_at_plus_one_us: 0,
        last_outcome: None,
        last_family: None,
        last_data: None,
    };

    /// Monotonic time when the actor collected the last terminal report.
    ///
    /// This is an actor observation timestamp, not an RF completion claim.
    pub fn last_terminal_at_us(self) -> Option<u64> {
        NonZeroU64::new(self.last_terminal_at_plus_one_us)
            .map(|encoded| encoded.get().saturating_sub(1))
    }

    /// Stable classification of the last terminal report.
    pub const fn last_outcome(self) -> Option<RadioTxTerminalOutcome> {
        self.last_outcome
    }

    /// Packet-owner family of the last terminal report.
    pub const fn last_family(self) -> Option<RadioTxDispatchFamily> {
        self.last_family
    }

    /// Most recent terminal DATA observation, retained across ordinary TX.
    pub const fn last_data(self) -> Option<LastDataTxObservation> {
        self.last_data
    }

    fn record_terminal(
        &mut self,
        outcome: RadioTxTerminalOutcome,
        family: RadioTxDispatchFamily,
        data_packet: Option<DataPacketDispatchObservation>,
        completed_physical_frames: u8,
        observed_at_us: u64,
    ) {
        saturating_increment(&mut self.terminal_jobs);
        self.completed_physical_frames = self
            .completed_physical_frames
            .saturating_add(u64::from(completed_physical_frames));
        match outcome {
            RadioTxTerminalOutcome::Transmitted => {
                saturating_increment(&mut self.transmitted_jobs);
            }
            RadioTxTerminalOutcome::AccessRejected
            | RadioTxTerminalOutcome::PostGrantAccessRejected => {
                saturating_increment(&mut self.access_rejected_jobs);
            }
            _ => saturating_increment(&mut self.failed_jobs),
        }
        self.last_terminal_at_plus_one_us = encode_optional_timestamp(observed_at_us);
        self.last_outcome = Some(outcome);
        self.last_family = Some(family);
        if let Some(data_packet) = data_packet {
            self.record_data(LastDataTxObservation::new(
                observed_at_us,
                outcome,
                data_packet,
            ));
        }
    }

    fn record_data(&mut self, observation: LastDataTxObservation) {
        self.last_data = Some(observation);
    }
}

/// Complete copyable production snapshot for the permanent LoRa actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioDiagnosticsSnapshot {
    /// Immutable board-applied profile and requested SX1262 power.
    pub applied: AppliedLoRaProfile,
    /// Current actor lifecycle.
    pub state: RadioRuntimeState,
    /// RNode receive-pipeline counters and last accepted packet.
    pub receive: LoRaReceiveDiagnostics,
    /// Physical frames rejected before RNode framing.
    pub invalid_physical_frames: u64,
    /// Actor-to-ingress handoff attempts.
    pub ingress: LoRaIngressDiagnostics,
    /// Channel-activity observations.
    pub cad: LoRaCadDiagnostics,
    /// Terminal logical-TX observations.
    pub transmit: LoRaTransmitDiagnostics,
}

impl RadioDiagnosticsSnapshot {
    fn new(configuration: E290RadioConfiguration) -> Self {
        Self {
            applied: AppliedLoRaProfile::from_configuration(configuration),
            state: RadioRuntimeState::Offline,
            receive: LoRaReceiveDiagnostics::EMPTY,
            invalid_physical_frames: 0,
            ingress: LoRaIngressDiagnostics::EMPTY,
            cad: LoRaCadDiagnostics::EMPTY,
            transmit: LoRaTransmitDiagnostics::EMPTY,
        }
    }
}

const _: () = assert!(
    core::mem::size_of::<RadioDiagnosticsSnapshot>() <= MAXIMUM_RADIO_DIAGNOSTICS_SNAPSHOT_BYTES
);

/// Blocking latest-value cell shared by the sole radio actor and product API.
pub struct RadioDiagnosticsCell {
    state: Mutex<CriticalSectionRawMutex, RefCell<RadioDiagnosticsSnapshot>>,
    trace: Mutex<CriticalSectionRawMutex, RefCell<RadioTraceRing>>,
}

impl RadioDiagnosticsCell {
    /// Construct an offline cell bound to one immutable applied configuration.
    pub fn new(configuration: E290RadioConfiguration) -> Self {
        Self {
            state: Mutex::new(RefCell::new(RadioDiagnosticsSnapshot::new(configuration))),
            // Main supplies the persistent boot sequence before tasks start.
            // Zero keeps host construction and pre-initialization explicit.
            trace: Mutex::new(RefCell::new(RadioTraceRing::new(0))),
        }
    }

    /// Copy one coherent fixed-size snapshot.
    pub fn snapshot(&self) -> RadioDiagnosticsSnapshot {
        self.state.lock(|state| *state.borrow())
    }

    /// Clear provisional trace state and bind future events to one boot sequence.
    ///
    /// Product startup calls this once before spawning radio and API tasks.
    /// Calling it later intentionally invalidates cursors and clears all
    /// retained events from the prior trace incarnation.
    pub fn set_trace_boot_sequence(&self, boot_sequence: u64) {
        self.trace
            .lock(|trace| trace.borrow_mut().reset_boot_sequence(boot_sequence));
    }

    /// Copy one bounded ascending page from the boot-scoped radio trace.
    pub fn radio_trace_page(&self, cursor: Option<RadioTraceCursor>) -> RadioTracePage {
        self.trace.lock(|trace| trace.borrow().page(cursor))
    }

    /// Append route evidence captured immediately before DATA authorization.
    pub fn record_route_selected(
        &self,
        observed_at_us: u64,
        route: RadioTraceRouteSelected,
    ) -> Option<RadioTraceEvent> {
        self.record_trace(observed_at_us, RadioTraceEventKind::RouteSelected(route))
    }

    /// Append one terminal Reticulum receipt observation.
    pub fn record_attempt_terminal(
        &self,
        observed_at_us: u64,
        terminal: RadioTraceAttemptTerminal,
    ) -> Option<RadioTraceEvent> {
        self.record_trace(
            observed_at_us,
            RadioTraceEventKind::AttemptTerminal(terminal),
        )
    }

    /// Append one complete logical LoRa receive observation.
    ///
    /// The complete-byte digest and hop-invariant RNS hash are calculated
    /// before the ingress buffer can be recycled or its packet bytes mutate.
    pub fn record_logical_rx(
        &self,
        observed_at_us: u64,
        interface: u8,
        packet: &[u8],
        signal: FrameSignal,
    ) -> Option<RadioTraceEvent> {
        let packet_len = u16::try_from(packet.len())
            .expect("the admitted native Reticulum packet length fits u16");
        let encoded_packet_sha256 = Sha256::digest(packet).into();
        self.record_trace(
            observed_at_us,
            RadioTraceEventKind::LogicalRx(RadioTraceLogicalRx::new(
                encoded_packet_sha256,
                rns_packet_hash(packet),
                packet_len,
                interface,
                signal.rssi_dbm,
                signal.snr_db,
            )),
        )
    }

    /// Mark the generation-bound LoRa actor online.
    pub fn mark_online(&self) {
        self.update(|state| state.state = RadioRuntimeState::Online);
    }

    /// Mark the LoRa actor offline without asserting a fault.
    pub fn mark_offline(&self) {
        self.update(|state| state.state = RadioRuntimeState::Offline);
    }

    /// Mark the LoRa actor permanently faulted for this boot.
    pub fn mark_faulted(&self) {
        self.update(|state| state.state = RadioRuntimeState::Faulted);
    }

    /// Replace cumulative RNode counters and optionally advance the last
    /// accepted logical-packet observation in the same critical section.
    pub fn record_receive_pipeline(
        &self,
        diagnostics: RxDiagnostics,
        accepted: Option<AcceptedLoRaPacketObservation>,
    ) {
        self.update(|state| {
            state.receive.replace_pipeline_counters(diagnostics);
            if let Some(accepted) = accepted {
                state.receive.record_accepted(accepted);
            }
        });
    }

    /// Count one physical frame rejected before RNode framing.
    pub fn record_invalid_physical_frame(&self) {
        self.update(|state| saturating_increment(&mut state.invalid_physical_frames));
    }

    /// Count one successful actor-to-ingress enqueue.
    pub fn record_ingress_enqueued(&self) {
        self.update(|state| saturating_increment(&mut state.ingress.enqueued));
    }

    /// Count one bounded-queue ingress deferral.
    pub fn record_ingress_deferred(&self) {
        self.update(|state| saturating_increment(&mut state.ingress.deferred));
    }

    /// Count one ingress capacity, sealing, or terminal handoff failure.
    pub fn record_ingress_failed(&self) {
        self.update(|state| saturating_increment(&mut state.ingress.failed));
    }

    /// Count one completed channel-activity observation.
    pub fn record_cad(&self, activity_detected: bool) {
        self.update(|state| {
            let counter = if activity_detected {
                &mut state.cad.busy
            } else {
                &mut state.cad.clear
            };
            saturating_increment(counter);
        });
    }

    /// Record one exact terminal dispatcher report.
    pub fn record_dispatch_report(&self, report: DispatchReport, observed_at_us: u64) {
        let outcome = classify_dispatch_outcome(report.outcome());
        let completed_physical_frames = report
            .progress()
            .map_or(0, |progress| progress.completed_frame_count());
        let family = report.family().into();
        let data_packet = report.data_packet();
        self.update(|state| {
            state.transmit.record_terminal(
                outcome,
                family,
                data_packet,
                completed_physical_frames,
                observed_at_us,
            );
        });
        if let Some(data_packet) = data_packet {
            let progress = report.progress();
            let completed_frames = progress.map_or(0, |progress| progress.completed_frame_count());
            let frame_completed_at_us = [
                progress.and_then(|progress| progress.frame_completed_at_us(0)),
                progress.and_then(|progress| progress.frame_completed_at_us(1)),
            ];
            let trace = RadioTraceDataTxTerminal::new(
                *data_packet.attempt().as_bytes(),
                *data_packet.encoded_packet_sha256().as_bytes(),
                data_packet.packet_len(),
                data_packet.interface().get(),
                report.outcome(),
                report.frame_count(),
                completed_frames,
                frame_completed_at_us,
                report.authorized_frame().is_some(),
            );
            let _ = self.record_trace(observed_at_us, RadioTraceEventKind::DataTxTerminal(trace));
        }
    }

    fn record_trace(
        &self,
        observed_at_us: u64,
        event: RadioTraceEventKind,
    ) -> Option<RadioTraceEvent> {
        self.trace
            .lock(|trace| trace.borrow_mut().push(observed_at_us, event))
    }

    fn update(&self, update: impl FnOnce(&mut RadioDiagnosticsSnapshot)) {
        self.state.lock(|state| update(&mut state.borrow_mut()));
    }
}

fn classify_dispatch_outcome(outcome: DispatchOutcome) -> RadioTxTerminalOutcome {
    match outcome {
        DispatchOutcome::Transmitted => RadioTxTerminalOutcome::Transmitted,
        DispatchOutcome::AccessRejected(_) => RadioTxTerminalOutcome::AccessRejected,
        DispatchOutcome::PermitDenied => RadioTxTerminalOutcome::PermitDenied,
        DispatchOutcome::AuthorizationExpired => RadioTxTerminalOutcome::AuthorizationExpired,
        DispatchOutcome::PostGrantAccessRejected(_) => {
            RadioTxTerminalOutcome::PostGrantAccessRejected
        }
        DispatchOutcome::AirtimeRejected(_) => RadioTxTerminalOutcome::AirtimeRejected,
        DispatchOutcome::DeadlineConversionOverflow => {
            RadioTxTerminalOutcome::DeadlineConversionOverflow
        }
        DispatchOutcome::RadioInactive => RadioTxTerminalOutcome::RadioInactive,
        DispatchOutcome::InterfaceConfigurationMismatch { .. } => {
            RadioTxTerminalOutcome::InterfaceConfigurationMismatch
        }
        DispatchOutcome::RadioConfigurationChangedBeforePermit => {
            RadioTxTerminalOutcome::RadioConfigurationChangedBeforePermit
        }
        DispatchOutcome::RadioConfigurationChangedAfterPermit => {
            RadioTxTerminalOutcome::RadioConfigurationChangedAfterPermit
        }
        DispatchOutcome::CadFault { .. } => RadioTxTerminalOutcome::CadFault,
        DispatchOutcome::TxFault { .. } => RadioTxTerminalOutcome::TxFault,
        DispatchOutcome::ControlPlaneRecovery => RadioTxTerminalOutcome::ControlPlaneRecovery,
        DispatchOutcome::FrameInvariantRecovery => RadioTxTerminalOutcome::FrameInvariantRecovery,
        DispatchOutcome::CancelledRadioOperation => RadioTxTerminalOutcome::CancelledRadioOperation,
    }
}

fn encode_optional_timestamp(timestamp_us: u64) -> u64 {
    timestamp_us.saturating_add(1)
}

fn saturating_increment(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

#[cfg(test)]
#[path = "radio_diagnostics_tests.rs"]
mod tests;
