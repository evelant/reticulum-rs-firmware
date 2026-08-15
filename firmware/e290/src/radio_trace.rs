//! Boot-scoped, bounded packet-correlated radio trace storage.
//!
//! The trace is deliberately separate from the durable submission journal.
//! A single boot retains the newest fixed number of copy-only observations;
//! authenticated clients page them by an explicit boot-and-sequence cursor and
//! persist any long-lived history outside this high-frequency firmware path.

use reticulum_radio_tx_dispatch::DispatchOutcome;

/// Number of newest radio events retained for one boot.
///
/// Inbound durable delivery contributes six correlated stages, so 32 entries
/// retain several complete exchanges. The ring lives in fixed RAM; keeping the
/// established bound preserves the gateway's reviewed stack headroom.
pub const RADIO_TRACE_CAPACITY: usize = 32;

/// Largest number of radio events returned by one bounded page.
///
/// Two maximum-size inbound-proof events fit the device API's fixed response
/// envelope. A third does not, so pagination splits consecutive proof stages
/// before projection instead of discovering an encoding failure later.
pub const RADIO_TRACE_PAGE_CAPACITY: usize = 2;

/// Cursor naming the last event already consumed by one reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceCursor {
    boot_sequence: u64,
    after_sequence: u64,
}

impl RadioTraceCursor {
    /// Construct a cursor after one exact event sequence in one boot.
    pub const fn new(boot_sequence: u64, after_sequence: u64) -> Self {
        Self {
            boot_sequence,
            after_sequence,
        }
    }

    /// Boot incarnation to which this cursor belongs.
    pub const fn boot_sequence(self) -> u64 {
        self.boot_sequence
    }

    /// Last event sequence already consumed by the reader.
    pub const fn after_sequence(self) -> u64 {
        self.after_sequence
    }
}

/// Why a selected DATA route chose its concrete interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceRouteResolution {
    /// A retained path selected its exact interface.
    ExactRetainedPath,
    /// No retained exact path existed and shared-medium broadcast was selected.
    BroadcastFallback,
}

/// Route facts captured for one exact prepared DATA packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceRouteSelected {
    submission_id: u64,
    destination: [u8; 16],
    next_hop: Option<[u8; 16]>,
    hops: u8,
    selected_interface: u8,
    resolution: RadioTraceRouteResolution,
    packet_len: u16,
    encoded_packet_sha256: [u8; 32],
    rns_attempt_token: [u8; 32],
}

impl RadioTraceRouteSelected {
    /// Construct immutable route evidence for one prepared DATA attempt.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        submission_id: u64,
        destination: [u8; 16],
        next_hop: Option<[u8; 16]>,
        hops: u8,
        selected_interface: u8,
        resolution: RadioTraceRouteResolution,
        packet_len: u16,
        encoded_packet_sha256: [u8; 32],
        rns_attempt_token: [u8; 32],
    ) -> Self {
        Self {
            submission_id,
            destination,
            next_hop,
            hops,
            selected_interface,
            resolution,
            packet_len,
            encoded_packet_sha256,
            rns_attempt_token,
        }
    }

    /// Durable application submission correlated with this prepared attempt.
    pub const fn submission_id(self) -> u64 {
        self.submission_id
    }

    /// Addressed Reticulum destination.
    pub const fn destination(self) -> [u8; 16] {
        self.destination
    }

    /// Retained next-hop transport identity, absent for a direct route.
    pub const fn next_hop(self) -> Option<[u8; 16]> {
        self.next_hop
    }

    /// Reticulum hop count captured with route selection.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Concrete interface selected for this serialized dispatch.
    pub const fn selected_interface(self) -> u8 {
        self.selected_interface
    }

    /// Resolution mode that selected the concrete interface.
    pub const fn resolution(self) -> RadioTraceRouteResolution {
        self.resolution
    }

    /// Complete encoded packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// SHA-256 over every complete encoded packet byte.
    pub const fn encoded_packet_sha256(self) -> [u8; 32] {
        self.encoded_packet_sha256
    }

    /// Hop-invariant Reticulum proof-correlation hash.
    pub const fn rns_attempt_token(self) -> [u8; 32] {
        self.rns_attempt_token
    }
}

/// Terminal physical-radio result for one DATA dispatch hop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceDataTxTerminal {
    rns_attempt_token: [u8; 32],
    encoded_packet_sha256: [u8; 32],
    packet_len: u16,
    interface: u8,
    outcome: DispatchOutcome,
    planned_frames: u8,
    completed_frames: u8,
    frame_completed_at_us: [Option<u64>; 2],
    authorized_frame_observed: bool,
}

impl RadioTraceDataTxTerminal {
    /// Construct one exact terminal DATA dispatch observation.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        rns_attempt_token: [u8; 32],
        encoded_packet_sha256: [u8; 32],
        packet_len: u16,
        interface: u8,
        outcome: DispatchOutcome,
        planned_frames: u8,
        completed_frames: u8,
        frame_completed_at_us: [Option<u64>; 2],
        authorized_frame_observed: bool,
    ) -> Self {
        Self {
            rns_attempt_token,
            encoded_packet_sha256,
            packet_len,
            interface,
            outcome,
            planned_frames,
            completed_frames,
            frame_completed_at_us,
            authorized_frame_observed,
        }
    }

    /// Hop-invariant Reticulum proof-correlation hash.
    pub const fn rns_attempt_token(self) -> [u8; 32] {
        self.rns_attempt_token
    }

    /// SHA-256 over every complete encoded packet byte.
    pub const fn encoded_packet_sha256(self) -> [u8; 32] {
        self.encoded_packet_sha256
    }

    /// Complete encoded packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// Concrete interface selected for this serialized dispatch.
    pub const fn interface(self) -> u8 {
        self.interface
    }

    /// Exact terminal dispatcher diagnosis, including portable fault detail.
    pub const fn outcome(self) -> DispatchOutcome {
        self.outcome
    }

    /// Number of physical RNode frames planned for the logical packet.
    pub const fn planned_frames(self) -> u8 {
        self.planned_frames
    }

    /// Number of physical frames with definitive completion evidence.
    pub const fn completed_frames(self) -> u8 {
        self.completed_frames
    }

    /// DIO/TxDone completion timestamp for one zero-based physical frame.
    pub const fn frame_completed_at_us(self, index: usize) -> Option<u64> {
        if index < self.frame_completed_at_us.len() {
            self.frame_completed_at_us[index]
        } else {
            None
        }
    }

    /// Whether native DATA bytes crossed the authorized-frame boundary.
    pub const fn authorized_frame_observed(self) -> bool {
        self.authorized_frame_observed
    }
}

/// One complete logical packet reconstructed from LoRa physical frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceLogicalRx {
    encoded_packet_sha256: [u8; 32],
    rns_packet_hash: Option<[u8; 32]>,
    packet_len: u16,
    interface: u8,
    rssi_dbm: i16,
    snr_db: i16,
}

impl RadioTraceLogicalRx {
    /// Construct one logical receive observation before RNS ingress queueing.
    pub const fn new(
        encoded_packet_sha256: [u8; 32],
        rns_packet_hash: Option<[u8; 32]>,
        packet_len: u16,
        interface: u8,
        rssi_dbm: i16,
        snr_db: i16,
    ) -> Self {
        Self {
            encoded_packet_sha256,
            rns_packet_hash,
            packet_len,
            interface,
            rssi_dbm,
            snr_db,
        }
    }

    /// SHA-256 over every received encoded packet byte.
    pub const fn encoded_packet_sha256(self) -> [u8; 32] {
        self.encoded_packet_sha256
    }

    /// Hop-invariant RNS hash, absent when the reconstructed packet did not parse.
    pub const fn rns_packet_hash(self) -> Option<[u8; 32]> {
        self.rns_packet_hash
    }

    /// Reconstructed native Reticulum packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// Interface that received the packet.
    pub const fn interface(self) -> u8 {
        self.interface
    }

    /// Conservative whole-packet received signal strength in dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Conservative whole-packet signal-to-noise ratio in dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// Receiver-side durable-proof lifecycle for one inbound DATA packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceInboundProofStage {
    /// A complete DATA packet was reconstructed from LoRa frames.
    DataLogicalRx,
    /// The application message became durable in the local LXMF store.
    DurableCommit,
    /// The exact proof became ready in the durable delayed-proof owner.
    ProofRetained,
    /// The proof moved into the dedicated one-packet admission holder.
    ProofStaged,
    /// The ordinary coordinator accepted the proof as its next owned packet.
    OrdinaryQueued,
    /// The radio reported physical TxDone for the complete proof packet.
    PhysicalTxDone,
    /// The exact proof packet reached a terminal radio result without TxDone.
    PhysicalTxFailed,
}

/// One stage in the receiver-side durable DATA-to-proof lifecycle.
///
/// `correlation_token` is always the complete hash of the covered inbound DATA
/// packet. Message identity appears once LXMF validation succeeds; packet and
/// signal fields are populated only at stages that own that evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceInboundProof {
    correlation_token: [u8; 32],
    stage: RadioTraceInboundProofStage,
    message_id: Option<[u8; 32]>,
    encoded_packet_sha256: Option<[u8; 32]>,
    packet_len: Option<u16>,
    interface: Option<u8>,
    signal: Option<(i16, i16)>,
    dispatch_outcome: Option<DispatchOutcome>,
}

impl RadioTraceInboundProof {
    /// Construct one immutable receiver proof-lifecycle stage.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        correlation_token: [u8; 32],
        stage: RadioTraceInboundProofStage,
        message_id: Option<[u8; 32]>,
        encoded_packet_sha256: Option<[u8; 32]>,
        packet_len: Option<u16>,
        interface: Option<u8>,
        signal: Option<(i16, i16)>,
        dispatch_outcome: Option<DispatchOutcome>,
    ) -> Self {
        Self {
            correlation_token,
            stage,
            message_id,
            encoded_packet_sha256,
            packet_len,
            interface,
            signal,
            dispatch_outcome,
        }
    }

    /// Complete hash of the inbound DATA packet covered by the proof.
    pub const fn correlation_token(self) -> [u8; 32] {
        self.correlation_token
    }

    /// Durable receiver lifecycle stage.
    pub const fn stage(self) -> RadioTraceInboundProofStage {
        self.stage
    }

    /// Validated LXMF message ID, once known.
    pub const fn message_id(self) -> Option<[u8; 32]> {
        self.message_id
    }

    /// SHA-256 over the encoded DATA or proof packet owned at this stage.
    pub const fn encoded_packet_sha256(self) -> Option<[u8; 32]> {
        self.encoded_packet_sha256
    }

    /// Complete encoded DATA or proof packet length owned at this stage.
    pub const fn packet_len(self) -> Option<u16> {
        self.packet_len
    }

    /// Exact receive or proof-return interface, when known.
    pub const fn interface(self) -> Option<u8> {
        self.interface
    }

    /// Whole-dB `(RSSI, SNR)` for the original DATA receive, when known.
    pub const fn signal(self) -> Option<(i16, i16)> {
        self.signal
    }

    /// Exact radio terminal outcome for a physical proof-TX stage.
    pub const fn dispatch_outcome(self) -> Option<DispatchOutcome> {
        self.dispatch_outcome
    }
}

/// Application-visible terminal classification for one exact DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceAttemptOutcome {
    /// A valid Reticulum delivery proof completed the attempt.
    Delivered,
    /// The Reticulum receipt expired without a valid proof.
    DeliveryTimeout,
    /// No final hop was permitted to transmit.
    Unsent,
}

/// Final-hop ingress facts for the proof that completed one attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceProofIngress {
    interface: u8,
    signal: Option<(i16, i16)>,
}

impl RadioTraceProofIngress {
    /// Construct proof ingress with optional whole-dB RSSI and SNR.
    pub const fn new(interface: u8, signal: Option<(i16, i16)>) -> Self {
        Self { interface, signal }
    }

    /// Interface on which the proof arrived.
    pub const fn interface(self) -> u8 {
        self.interface
    }

    /// Whole-dB `(RSSI, SNR)` values, when supplied by the interface.
    pub const fn signal(self) -> Option<(i16, i16)> {
        self.signal
    }
}

/// Terminal Reticulum receipt evidence for one DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceAttemptTerminal {
    rns_attempt_token: [u8; 32],
    outcome: RadioTraceAttemptOutcome,
    proof_ingress: Option<RadioTraceProofIngress>,
}

impl RadioTraceAttemptTerminal {
    /// Construct one terminal receipt observation.
    pub const fn new(
        rns_attempt_token: [u8; 32],
        outcome: RadioTraceAttemptOutcome,
        proof_ingress: Option<RadioTraceProofIngress>,
    ) -> Self {
        Self {
            rns_attempt_token,
            outcome,
            proof_ingress,
        }
    }

    /// Hop-invariant Reticulum proof-correlation hash.
    pub const fn rns_attempt_token(self) -> [u8; 32] {
        self.rns_attempt_token
    }

    /// Terminal application-visible receipt classification.
    pub const fn outcome(self) -> RadioTraceAttemptOutcome {
        self.outcome
    }

    /// Proof ingress and signal values for delivered attempts.
    pub const fn proof_ingress(self) -> Option<RadioTraceProofIngress> {
        self.proof_ingress
    }
}

/// Typed payload of one boot-scoped radio trace event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioTraceEventKind {
    /// Route selected immediately before DATA authorization.
    RouteSelected(RadioTraceRouteSelected),
    /// DATA dispatch reached a terminal physical-radio result.
    DataTxTerminal(RadioTraceDataTxTerminal),
    /// A complete logical packet was reconstructed from LoRa frames.
    LogicalRx(RadioTraceLogicalRx),
    /// One correlated receiver-side durable DATA-to-proof stage.
    InboundProof(RadioTraceInboundProof),
    /// A DATA receipt reached delivered, timeout, or definitely-unsent state.
    AttemptTerminal(RadioTraceAttemptTerminal),
}

/// One immutable event identified within a boot incarnation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTraceEvent {
    boot_sequence: u64,
    sequence: u64,
    observed_at_us: u64,
    kind: RadioTraceEventKind,
}

impl RadioTraceEvent {
    /// Boot incarnation that owns this event.
    pub const fn boot_sequence(self) -> u64 {
        self.boot_sequence
    }

    /// Strictly ascending event sequence within the boot.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Firmware monotonic observation time in microseconds.
    pub const fn observed_at_us(self) -> u64 {
        self.observed_at_us
    }

    /// Typed immutable event payload.
    pub const fn kind(self) -> RadioTraceEventKind {
        self.kind
    }
}

/// One bounded ascending page from the current boot trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RadioTracePage {
    boot_sequence: u64,
    oldest_retained_sequence: Option<u64>,
    newest_retained_sequence: Option<u64>,
    next_sequence: Option<u64>,
    overwritten_events: u64,
    sequence_exhausted_events: u64,
    boot_changed: bool,
    history_gap: bool,
    has_more: bool,
    events: [Option<RadioTraceEvent>; RADIO_TRACE_PAGE_CAPACITY],
    len: u8,
}

impl RadioTracePage {
    /// Current trace boot incarnation.
    pub const fn boot_sequence(self) -> u64 {
        self.boot_sequence
    }

    /// Oldest sequence still resident in the bounded ring.
    pub const fn oldest_retained_sequence(self) -> Option<u64> {
        self.oldest_retained_sequence
    }

    /// Newest sequence still resident in the bounded ring.
    pub const fn newest_retained_sequence(self) -> Option<u64> {
        self.newest_retained_sequence
    }

    /// Sequence that the next successfully appended event will receive.
    ///
    /// `None` means the practically unreachable per-boot sequence exhaustion
    /// condition has been reached.
    pub const fn next_sequence(self) -> Option<u64> {
        self.next_sequence
    }

    /// Events overwritten by ring pressure during this boot.
    pub const fn overwritten_events(self) -> u64 {
        self.overwritten_events
    }

    /// Events rejected because the per-boot sequence space was exhausted.
    pub const fn sequence_exhausted_events(self) -> u64 {
        self.sequence_exhausted_events
    }

    /// Whether the supplied cursor belonged to another boot incarnation.
    pub const fn boot_changed(self) -> bool {
        self.boot_changed
    }

    /// Whether unread same-boot sequences were overwritten before this page.
    pub const fn history_gap(self) -> bool {
        self.history_gap
    }

    /// Whether another page exists after the returned events.
    pub const fn has_more(self) -> bool {
        self.has_more
    }

    /// Number of initialized events in this page.
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// Whether this page contains no events.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Iterate initialized page events in ascending sequence order.
    pub fn events(&self) -> impl Iterator<Item = RadioTraceEvent> + '_ {
        self.events.iter().take(self.len()).flatten().copied()
    }

    /// Cursor immediately after the final returned event.
    pub fn next_cursor(&self) -> Option<RadioTraceCursor> {
        self.events()
            .last()
            .map(|event| RadioTraceCursor::new(self.boot_sequence, event.sequence()))
    }
}

/// Fixed newest-retained event owner for one firmware boot.
pub struct RadioTraceRing {
    boot_sequence: u64,
    last_sequence: u64,
    head: usize,
    len: usize,
    overwritten_events: u64,
    sequence_exhausted_events: u64,
    events: [Option<RadioTraceEvent>; RADIO_TRACE_CAPACITY],
}

impl RadioTraceRing {
    /// Construct an empty trace for one explicit boot incarnation.
    pub const fn new(boot_sequence: u64) -> Self {
        Self {
            boot_sequence,
            last_sequence: 0,
            head: 0,
            len: 0,
            overwritten_events: 0,
            sequence_exhausted_events: 0,
            events: [None; RADIO_TRACE_CAPACITY],
        }
    }

    /// Clear all events and begin a newly supplied boot incarnation.
    pub fn reset_boot_sequence(&mut self, boot_sequence: u64) {
        *self = Self::new(boot_sequence);
    }

    /// Current boot incarnation.
    pub const fn boot_sequence(&self) -> u64 {
        self.boot_sequence
    }

    /// Retained event count.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether no event is currently retained.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append one event, overwriting the oldest event when the ring is full.
    ///
    /// `None` is returned only after the practically unreachable exhaustion of
    /// all nonzero `u64` event sequences in one boot.
    pub fn push(
        &mut self,
        observed_at_us: u64,
        kind: RadioTraceEventKind,
    ) -> Option<RadioTraceEvent> {
        let Some(sequence) = self.last_sequence.checked_add(1) else {
            self.sequence_exhausted_events = self.sequence_exhausted_events.saturating_add(1);
            return None;
        };
        let event = RadioTraceEvent {
            boot_sequence: self.boot_sequence,
            sequence,
            observed_at_us,
            kind,
        };
        if self.len == RADIO_TRACE_CAPACITY {
            self.overwritten_events = self.overwritten_events.saturating_add(1);
        } else {
            self.len += 1;
        }
        self.events[self.head] = Some(event);
        self.head = (self.head + 1) % RADIO_TRACE_CAPACITY;
        self.last_sequence = sequence;
        Some(event)
    }

    /// Copy at most two events after an optional reader cursor.
    ///
    /// A cursor from another boot restarts at the oldest current event and is
    /// reported through [`RadioTracePage::boot_changed`]. A same-boot cursor
    /// older than retained history similarly restarts at the oldest event and
    /// reports [`RadioTracePage::history_gap`].
    pub fn page(&self, cursor: Option<RadioTraceCursor>) -> RadioTracePage {
        let oldest_index = (self.head + RADIO_TRACE_CAPACITY - self.len) % RADIO_TRACE_CAPACITY;
        let oldest_retained_sequence = self
            .event_at_offset(oldest_index, 0)
            .map(RadioTraceEvent::sequence);
        let newest_retained_sequence = self
            .event_at_offset(oldest_index, self.len.saturating_sub(1))
            .map(RadioTraceEvent::sequence);
        let boot_changed = cursor.is_some_and(|cursor| cursor.boot_sequence != self.boot_sequence);
        let mut history_gap = false;
        let start_offset = match (
            oldest_retained_sequence,
            newest_retained_sequence,
            cursor.filter(|_| !boot_changed),
        ) {
            (Some(oldest), Some(newest), Some(cursor)) => {
                if cursor.after_sequence == newest {
                    return self.page_from_offset(
                        oldest_retained_sequence,
                        newest_retained_sequence,
                        boot_changed,
                        false,
                        oldest_index,
                        self.len,
                    );
                }
                let Some(wanted) = cursor.after_sequence.checked_add(1) else {
                    history_gap = true;
                    return self.page_from_offset(
                        oldest_retained_sequence,
                        newest_retained_sequence,
                        boot_changed,
                        history_gap,
                        oldest_index,
                        0,
                    );
                };
                if wanted < oldest || wanted > newest {
                    history_gap = true;
                    0
                } else {
                    usize::try_from(wanted - oldest)
                        .expect("a retained ring offset always fits usize")
                }
            }
            (Some(_), Some(_), None) => 0,
            (None, None, _) => 0,
            _ => unreachable!("oldest and newest retained sequence appear together"),
        };

        self.page_from_offset(
            oldest_retained_sequence,
            newest_retained_sequence,
            boot_changed,
            history_gap,
            oldest_index,
            start_offset,
        )
    }

    fn page_from_offset(
        &self,
        oldest_retained_sequence: Option<u64>,
        newest_retained_sequence: Option<u64>,
        boot_changed: bool,
        history_gap: bool,
        oldest_index: usize,
        start_offset: usize,
    ) -> RadioTracePage {
        let remaining = self.len.saturating_sub(start_offset);
        let take = remaining.min(RADIO_TRACE_PAGE_CAPACITY);
        let mut events = [None; RADIO_TRACE_PAGE_CAPACITY];
        for (page_index, slot) in events.iter_mut().take(take).enumerate() {
            *slot = self.event_at_offset(oldest_index, start_offset + page_index);
        }
        RadioTracePage {
            boot_sequence: self.boot_sequence,
            oldest_retained_sequence,
            newest_retained_sequence,
            next_sequence: self.last_sequence.checked_add(1),
            overwritten_events: self.overwritten_events,
            sequence_exhausted_events: self.sequence_exhausted_events,
            boot_changed,
            history_gap,
            has_more: remaining > take,
            events,
            len: u8::try_from(take).expect("the fixed page capacity fits u8"),
        }
    }

    fn event_at_offset(&self, oldest_index: usize, offset: usize) -> Option<RadioTraceEvent> {
        if offset >= self.len {
            None
        } else {
            self.events[(oldest_index + offset) % RADIO_TRACE_CAPACITY]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rx(tag: u8) -> RadioTraceEventKind {
        RadioTraceEventKind::LogicalRx(RadioTraceLogicalRx::new(
            [tag; 32],
            Some([tag.wrapping_add(1); 32]),
            u16::from(tag).saturating_add(1),
            1,
            -90,
            5,
        ))
    }

    fn inbound(tag: u8, stage: RadioTraceInboundProofStage) -> RadioTraceEventKind {
        RadioTraceEventKind::InboundProof(RadioTraceInboundProof::new(
            [tag; 32],
            stage,
            Some([tag.wrapping_add(1); 32]),
            Some([tag.wrapping_add(2); 32]),
            Some(113),
            Some(1),
            None,
            None,
        ))
    }

    fn sequences(page: &RadioTracePage) -> std::vec::Vec<u64> {
        page.events().map(RadioTraceEvent::sequence).collect()
    }

    #[test]
    fn pages_are_fixed_bounded_and_strictly_ascending() {
        let mut ring = RadioTraceRing::new(77);
        for tag in 0..7 {
            let event = ring.push(1_000 + u64::from(tag), rx(tag)).unwrap();
            assert_eq!(event.boot_sequence(), 77);
            assert_eq!(event.sequence(), u64::from(tag) + 1);
        }

        let first = ring.page(None);
        assert_eq!(sequences(&first), [1, 2]);
        assert!(first.has_more());
        assert!(!first.history_gap());
        let second = ring.page(first.next_cursor());
        assert_eq!(sequences(&second), [3, 4]);
        assert!(second.has_more());
        let third = ring.page(second.next_cursor());
        assert_eq!(sequences(&third), [5, 6]);
        assert!(third.has_more());
        let fourth = ring.page(third.next_cursor());
        assert_eq!(sequences(&fourth), [7]);
        assert!(!fourth.has_more());
    }

    #[test]
    fn consecutive_maximum_proof_events_split_before_device_api_projection() {
        let mut ring = RadioTraceRing::new(78);
        ring.push(1, inbound(1, RadioTraceInboundProofStage::DurableCommit));
        ring.push(2, inbound(1, RadioTraceInboundProofStage::ProofRetained));
        ring.push(3, inbound(1, RadioTraceInboundProofStage::ProofStaged));

        let first = ring.page(None);
        assert_eq!(first.len(), 2);
        assert!(first.has_more());
        let second = ring.page(first.next_cursor());
        assert_eq!(second.len(), 1);
        assert!(!second.has_more());
    }

    #[test]
    fn wrap_reports_exact_overwrite_and_stale_cursor_gap() {
        let mut ring = RadioTraceRing::new(88);
        for tag in 0..(RADIO_TRACE_CAPACITY as u8 + 3) {
            ring.push(u64::from(tag), rx(tag)).unwrap();
        }
        assert_eq!(ring.len(), RADIO_TRACE_CAPACITY);

        let retained = ring.page(None);
        assert_eq!(retained.oldest_retained_sequence(), Some(4));
        assert_eq!(retained.newest_retained_sequence(), Some(35));
        assert_eq!(retained.overwritten_events(), 3);
        assert_eq!(sequences(&retained), [4, 5]);
        assert!(!retained.history_gap());

        let stale = ring.page(Some(RadioTraceCursor::new(88, 0)));
        assert!(stale.history_gap());
        assert_eq!(sequences(&stale), [4, 5]);

        let current = ring.page(Some(RadioTraceCursor::new(88, 33)));
        assert!(!current.history_gap());
        assert_eq!(sequences(&current), [34, 35]);

        let caught_up = ring.page(Some(RadioTraceCursor::new(88, 35)));
        assert!(!caught_up.history_gap());
        assert!(caught_up.is_empty());

        let stale_boot_without_boot_id = ring.page(Some(RadioTraceCursor::new(88, 99)));
        assert!(stale_boot_without_boot_id.history_gap());
        assert_eq!(sequences(&stale_boot_without_boot_id), [4, 5]);
    }

    #[test]
    fn boot_reset_clears_history_and_invalidates_old_cursor() {
        let mut ring = RadioTraceRing::new(9);
        ring.push(1, rx(1)).unwrap();
        let old_cursor = RadioTraceCursor::new(9, 1);

        ring.reset_boot_sequence(10);
        let event = ring.push(2, rx(2)).unwrap();
        assert_eq!(event.sequence(), 1);
        let page = ring.page(Some(old_cursor));
        assert!(page.boot_changed());
        assert!(!page.history_gap());
        assert_eq!(page.overwritten_events(), 0);
        assert_eq!(sequences(&page), [1]);
    }

    #[test]
    fn typed_events_preserve_exact_packet_and_radio_evidence() {
        let tx = RadioTraceDataTxTerminal::new(
            [0x11; 32],
            [0x22; 32],
            211,
            1,
            DispatchOutcome::Transmitted,
            1,
            1,
            [Some(1_234), None],
            true,
        );
        let terminal = RadioTraceAttemptTerminal::new(
            [0x11; 32],
            RadioTraceAttemptOutcome::Delivered,
            Some(RadioTraceProofIngress::new(1, Some((-98, 4)))),
        );
        let route = RadioTraceRouteSelected::new(
            7,
            [0x33; 16],
            None,
            1,
            1,
            RadioTraceRouteResolution::ExactRetainedPath,
            211,
            [0x22; 32],
            [0x11; 32],
        );
        let mut ring = RadioTraceRing::new(1);
        ring.push(1_000, RadioTraceEventKind::RouteSelected(route));
        ring.push(2_000, RadioTraceEventKind::DataTxTerminal(tx));
        ring.push(3_000, RadioTraceEventKind::AttemptTerminal(terminal));

        let first = ring.page(None);
        let events = first.events().collect::<std::vec::Vec<_>>();
        assert_eq!(events[0].kind(), RadioTraceEventKind::RouteSelected(route));
        assert_eq!(route.submission_id(), 7);
        assert_eq!(events[1].kind(), RadioTraceEventKind::DataTxTerminal(tx));
        assert_eq!(events[1].observed_at_us(), 2_000);
        assert_eq!(tx.frame_completed_at_us(0), Some(1_234));
        let events = ring
            .page(first.next_cursor())
            .events()
            .collect::<std::vec::Vec<_>>();
        assert_eq!(
            events[0].kind(),
            RadioTraceEventKind::AttemptTerminal(terminal)
        );
    }
}
