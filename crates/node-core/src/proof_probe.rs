//! Bounded volatile state for one standard Reticulum proof probe.
//!
//! Probe attempts deliberately do not enter the durable application
//! submission journal. This tracker retains the exact node-core attempt
//! correlation needed to intercept its dispatch, recovery, and terminal
//! records without allowing an unrelated DATA attempt to be consumed.

use super::{
    AttemptHandle, AttemptOutcome, AttemptToken, AuthorizedFrameObservation, DataReceiptKind,
    DestinationHash, InterfaceSet, MonotonicMillis, PacketIngressObservation, PreparedPacket,
    TerminalAttempt, TxRecoveryObservation, TxRecoveryRecord,
};

/// Read-only correlation of one scalar with the retained probe slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProofProbeCorrelation {
    /// The complete generation-safe handle and proof token both match.
    Exact,
    /// No active or terminal probe occupies the slot.
    NoProbe,
    /// Neither the handle nor token names the retained probe.
    Unrelated,
    /// Only one correlation component matches.
    Conflict,
}

/// Strict result of correlating one observation with the volatile probe slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProofProbeObservation {
    /// The observation was retained for the active probe.
    Recorded,
    /// The exact observation had already been retained.
    Duplicate,
    /// No active or terminal probe currently owns the observation.
    NoProbe,
    /// Neither the generation-safe handle nor proof token names this probe.
    Unrelated,
    /// Only part of the correlation matched, or exact-correlation metadata
    /// disagreed with the retained observation.
    CorrelationConflict,
}

/// Failure to reserve the single volatile probe slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProofProbeStartError {
    /// An active probe or unconsumed terminal result already occupies the slot.
    Busy,
    /// Standard proof probes must use destination DATA, not Link DATA.
    RequiresDestinationData,
}

/// Failure to evict a completed probe result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProofProbeEvictionError {
    /// No completed result is retained.
    NotTerminal,
    /// The acknowledged node-core tombstone is not the exact retained probe
    /// terminal.
    TerminalMismatch,
}

/// First dispatch timing and path information retained for one probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProofProbeFirstDispatch {
    at: MonotonicMillis,
    route_hops: Option<u8>,
}

impl ProofProbeFirstDispatch {
    /// Monotonic time at which the first dispatch was accepted by the router.
    pub const fn at(self) -> MonotonicMillis {
        self.at
    }

    /// Retained Reticulum path hop count at first dispatch, when known.
    pub const fn route_hops(self) -> Option<u8> {
        self.route_hops
    }
}

/// Copy-only correlation and measurements for the currently active probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveProofProbe {
    destination: DestinationHash,
    prepared: PreparedPacket,
    prepared_at: MonotonicMillis,
    first_dispatch: Option<ProofProbeFirstDispatch>,
    authorized_interfaces: InterfaceSet,
    authorized_interface_outside_profile: bool,
    recovery: Option<TxRecoveryRecord>,
}

impl ActiveProofProbe {
    /// Canonical remote `rnstransport.probe` destination.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Exact node-core packet and attempt correlation.
    pub const fn prepared(self) -> PreparedPacket {
        self.prepared
    }

    /// Monotonic time when the volatile slot accepted this attempt.
    pub const fn prepared_at(self) -> MonotonicMillis {
        self.prepared_at
    }

    /// First dispatch timing and retained path hops, when dispatch began.
    pub const fn first_dispatch(self) -> Option<ProofProbeFirstDispatch> {
        self.first_dispatch
    }

    /// Interfaces on which an exact frame authorization was observed.
    pub const fn authorized_interfaces(self) -> InterfaceSet {
        self.authorized_interfaces
    }

    /// Whether an authorized interface could not fit the compact 0-63 set.
    pub const fn authorized_interface_outside_profile(self) -> bool {
        self.authorized_interface_outside_profile
    }

    /// Exact retained fail-closed recovery record, when one was observed.
    pub const fn recovery(self) -> Option<TxRecoveryRecord> {
        self.recovery
    }
}

/// Completed volatile probe retained until its node-core tombstone is
/// acknowledged and the result is explicitly evicted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalProofProbe {
    active: ActiveProofProbe,
    terminal: TerminalAttempt,
    observed_at: MonotonicMillis,
}

impl TerminalProofProbe {
    /// Correlation and dispatch measurements captured before completion.
    pub const fn active(self) -> ActiveProofProbe {
        self.active
    }

    /// Exact terminal attempt that must be acknowledged in node-core.
    pub const fn terminal_attempt(self) -> TerminalAttempt {
        self.terminal
    }

    /// Terminal delivery, timeout, or definitely-unsent outcome.
    pub const fn outcome(self) -> AttemptOutcome {
        self.terminal.outcome()
    }

    /// Proof-packet ingress and optional final-hop signal values.
    ///
    /// For multihop probes this signal describes the proof's final return hop,
    /// not an end-to-end RF measurement and not the remote receiver's request
    /// RSSI.
    pub const fn proof_ingress(self) -> Option<PacketIngressObservation> {
        self.terminal.ingress()
    }

    /// Monotonic time when node-core exposed the terminal tombstone.
    pub const fn observed_at(self) -> MonotonicMillis {
        self.observed_at
    }

    /// Milliseconds from first dispatch to terminal observation, when the
    /// monotonic samples are ordered.
    pub const fn elapsed_since_first_dispatch_ms(self) -> Option<u64> {
        match self.active.first_dispatch {
            Some(first) => self.observed_at.get().checked_sub(first.at.get()),
            None => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProofProbeSlot {
    Active(ActiveProofProbe),
    Terminal(TerminalProofProbe),
}

/// Allocation-free owner for at most one volatile standard proof probe.
///
/// A terminal result continues to occupy the slot until
/// [`Self::evict_terminal`] receives the exact tombstone returned by
/// node-core acknowledgement. This prevents a second probe from overwriting
/// the only correlation needed to release the first attempt safely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VolatileProofProbe {
    slot: Option<ProofProbeSlot>,
}

impl VolatileProofProbe {
    /// Construct an empty one-slot tracker.
    pub const fn new() -> Self {
        Self { slot: None }
    }

    /// Whether no active probe or terminal result occupies the slot.
    pub const fn is_vacant(&self) -> bool {
        self.slot.is_none()
    }

    /// Copy the currently active probe, if any.
    pub const fn active(&self) -> Option<ActiveProofProbe> {
        match self.slot {
            Some(ProofProbeSlot::Active(active)) => Some(active),
            Some(ProofProbeSlot::Terminal(_)) | None => None,
        }
    }

    /// Poll the retained terminal result without evicting it.
    pub const fn terminal(&self) -> Option<TerminalProofProbe> {
        match self.slot {
            Some(ProofProbeSlot::Terminal(terminal)) => Some(terminal),
            Some(ProofProbeSlot::Active(_)) | None => None,
        }
    }

    /// Reserve the one volatile slot for an already prepared destination-DATA
    /// attempt.
    pub fn try_start(
        &mut self,
        destination: DestinationHash,
        prepared: PreparedPacket,
        prepared_at: MonotonicMillis,
    ) -> Result<ActiveProofProbe, ProofProbeStartError> {
        if self.slot.is_some() {
            return Err(ProofProbeStartError::Busy);
        }
        if prepared.receipt_kind() != DataReceiptKind::DestinationData {
            return Err(ProofProbeStartError::RequiresDestinationData);
        }
        let active = ActiveProofProbe {
            destination,
            prepared,
            prepared_at,
            first_dispatch: None,
            authorized_interfaces: InterfaceSet::empty(),
            authorized_interface_outside_profile: false,
            recovery: None,
        };
        self.slot = Some(ProofProbeSlot::Active(active));
        Ok(active)
    }

    /// Whether the active slot owns this exact complete prepared-packet scalar.
    pub fn matches_active_prepared(&self, prepared: PreparedPacket) -> bool {
        matches!(
            self.slot,
            Some(ProofProbeSlot::Active(active)) if active.prepared == prepared
        )
    }

    /// Whether the active slot owns this complete proof-correlation token.
    pub fn matches_active_attempt(&self, attempt: AttemptToken) -> bool {
        matches!(
            self.slot,
            Some(ProofProbeSlot::Active(active)) if active.prepared.attempt() == attempt
        )
    }

    /// Whether either the active or terminal slot owns this exact prepared
    /// packet scalar.
    pub fn owns_prepared(&self, prepared: PreparedPacket) -> bool {
        match self.slot {
            Some(ProofProbeSlot::Active(active)) => active.prepared == prepared,
            Some(ProofProbeSlot::Terminal(terminal)) => terminal.active.prepared == prepared,
            None => false,
        }
    }

    /// Whether either the active or terminal slot owns this proof token.
    pub fn owns_attempt(&self, attempt: AttemptToken) -> bool {
        match self.slot {
            Some(ProofProbeSlot::Active(active)) => active.prepared.attempt() == attempt,
            Some(ProofProbeSlot::Terminal(terminal)) => {
                terminal.active.prepared.attempt() == attempt
            }
            None => false,
        }
    }

    /// Retain the first router dispatch time and path hops for the exact
    /// prepared packet.
    pub fn mark_first_dispatch(
        &mut self,
        prepared: PreparedPacket,
        dispatched_at: MonotonicMillis,
        route_hops: Option<u8>,
    ) -> ProofProbeObservation {
        let Some(ProofProbeSlot::Active(mut active)) = self.slot else {
            return match self.slot {
                None => ProofProbeObservation::NoProbe,
                Some(ProofProbeSlot::Terminal(terminal)) => {
                    classify_prepared(terminal.active.prepared, prepared)
                        .map_exact(ProofProbeObservation::Duplicate)
                }
                Some(ProofProbeSlot::Active(_)) => unreachable!(),
            };
        };
        match classify_prepared(active.prepared, prepared) {
            Correlation::Exact => {
                let observed = ProofProbeFirstDispatch {
                    at: dispatched_at,
                    route_hops,
                };
                match active.first_dispatch {
                    None => {
                        active.first_dispatch = Some(observed);
                        self.slot = Some(ProofProbeSlot::Active(active));
                        ProofProbeObservation::Recorded
                    }
                    // One prepared packet can be routed across more than one
                    // eligible interface. RTT and route evidence are defined
                    // by the first accepted dispatch, so every later exact
                    // observation is a duplicate even when its timestamp or
                    // current route snapshot differs.
                    Some(_) => ProofProbeObservation::Duplicate,
                }
            }
            correlation => correlation.into_observation(),
        }
    }

    /// Strictly classify one authorized-frame observation against the active
    /// probe.
    pub fn classify_authorized(
        &self,
        observation: AuthorizedFrameObservation,
    ) -> ProofProbeCorrelation {
        match self.slot {
            Some(ProofProbeSlot::Active(active)) => classify_handle_token(
                active.prepared.handle(),
                active.prepared.attempt(),
                observation.attempt_handle(),
                observation.attempt(),
            )
            .into_public(),
            Some(ProofProbeSlot::Terminal(terminal)) => classify_handle_token(
                terminal.active.prepared.handle(),
                terminal.active.prepared.attempt(),
                observation.attempt_handle(),
                observation.attempt(),
            )
            .into_public(),
            None => ProofProbeCorrelation::NoProbe,
        }
    }

    /// Retain the interface of one exact authorized probe frame.
    pub fn observe_authorized(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> ProofProbeObservation {
        match self.slot {
            None => ProofProbeObservation::NoProbe,
            Some(ProofProbeSlot::Active(active)) => {
                let (active, result) = retain_authorized(active, observation);
                self.slot = Some(ProofProbeSlot::Active(active));
                result
            }
            Some(ProofProbeSlot::Terminal(mut terminal)) => {
                let (active, result) = retain_authorized(terminal.active, observation);
                terminal.active = active;
                self.slot = Some(ProofProbeSlot::Terminal(terminal));
                result
            }
        }
    }

    /// Retain fail-closed recovery for the exact active probe.
    pub fn observe_recovery(
        &mut self,
        observation: TxRecoveryObservation,
    ) -> ProofProbeObservation {
        match self.slot {
            None => ProofProbeObservation::NoProbe,
            Some(ProofProbeSlot::Active(active)) => {
                let (active, result) = retain_recovery(active, observation);
                self.slot = Some(ProofProbeSlot::Active(active));
                result
            }
            Some(ProofProbeSlot::Terminal(mut terminal)) => {
                let (active, result) = retain_recovery(terminal.active, observation);
                terminal.active = active;
                self.slot = Some(ProofProbeSlot::Terminal(terminal));
                result
            }
        }
    }

    /// Move an exact node-core terminal attempt into the retained result slot.
    pub fn observe_terminal(
        &mut self,
        terminal: TerminalAttempt,
        observed_at: MonotonicMillis,
    ) -> ProofProbeObservation {
        match self.slot {
            None => ProofProbeObservation::NoProbe,
            Some(ProofProbeSlot::Active(active)) => {
                match classify_handle_token(
                    active.prepared.handle(),
                    active.prepared.attempt(),
                    terminal.handle(),
                    terminal.token(),
                ) {
                    Correlation::Exact
                        if terminal.receipt_kind() != DataReceiptKind::DestinationData =>
                    {
                        ProofProbeObservation::CorrelationConflict
                    }
                    Correlation::Exact => {
                        self.slot = Some(ProofProbeSlot::Terminal(TerminalProofProbe {
                            active,
                            terminal,
                            observed_at,
                        }));
                        ProofProbeObservation::Recorded
                    }
                    correlation => correlation.into_observation(),
                }
            }
            Some(ProofProbeSlot::Terminal(retained)) => {
                match classify_handle_token(
                    retained.active.prepared.handle(),
                    retained.active.prepared.attempt(),
                    terminal.handle(),
                    terminal.token(),
                ) {
                    Correlation::Exact if retained.terminal == terminal => {
                        ProofProbeObservation::Duplicate
                    }
                    Correlation::Exact => ProofProbeObservation::CorrelationConflict,
                    correlation => correlation.into_observation(),
                }
            }
        }
    }

    /// Evict a completed result only after node-core returned the exact
    /// acknowledged terminal tombstone.
    pub fn evict_terminal(
        &mut self,
        acknowledged: TerminalAttempt,
    ) -> Result<TerminalProofProbe, ProofProbeEvictionError> {
        let Some(ProofProbeSlot::Terminal(retained)) = self.slot else {
            return Err(ProofProbeEvictionError::NotTerminal);
        };
        if retained.terminal != acknowledged {
            return Err(ProofProbeEvictionError::TerminalMismatch);
        }
        self.slot = None;
        Ok(retained)
    }
}

impl Default for VolatileProofProbe {
    fn default() -> Self {
        Self::new()
    }
}

fn retain_authorized(
    mut active: ActiveProofProbe,
    observation: AuthorizedFrameObservation,
) -> (ActiveProofProbe, ProofProbeObservation) {
    let result = match classify_handle_token(
        active.prepared.handle(),
        active.prepared.attempt(),
        observation.attempt_handle(),
        observation.attempt(),
    ) {
        Correlation::Exact => {
            let interface = observation.interface();
            if active.authorized_interfaces.contains(interface)
                || (interface.get() >= 64 && active.authorized_interface_outside_profile)
            {
                ProofProbeObservation::Duplicate
            } else {
                match active.authorized_interfaces.with(interface) {
                    Some(interfaces) => active.authorized_interfaces = interfaces,
                    None => active.authorized_interface_outside_profile = true,
                }
                ProofProbeObservation::Recorded
            }
        }
        correlation => correlation.into_observation(),
    };
    (active, result)
}

fn retain_recovery(
    mut active: ActiveProofProbe,
    observation: TxRecoveryObservation,
) -> (ActiveProofProbe, ProofProbeObservation) {
    let result = match classify_handle_token(
        active.prepared.handle(),
        active.prepared.attempt(),
        observation.attempt_handle(),
        observation.attempt(),
    ) {
        Correlation::Exact => match active.recovery {
            None => {
                active.recovery = Some(observation.record());
                ProofProbeObservation::Recorded
            }
            Some(retained) if retained == observation.record() => ProofProbeObservation::Duplicate,
            Some(_) => ProofProbeObservation::CorrelationConflict,
        },
        correlation => correlation.into_observation(),
    };
    (active, result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Correlation {
    Exact,
    Unrelated,
    Conflict,
}

impl Correlation {
    const fn into_public(self) -> ProofProbeCorrelation {
        match self {
            Self::Exact => ProofProbeCorrelation::Exact,
            Self::Unrelated => ProofProbeCorrelation::Unrelated,
            Self::Conflict => ProofProbeCorrelation::Conflict,
        }
    }

    const fn into_observation(self) -> ProofProbeObservation {
        match self {
            Self::Exact => ProofProbeObservation::Recorded,
            Self::Unrelated => ProofProbeObservation::Unrelated,
            Self::Conflict => ProofProbeObservation::CorrelationConflict,
        }
    }

    const fn map_exact(self, exact: ProofProbeObservation) -> ProofProbeObservation {
        match self {
            Self::Exact => exact,
            Self::Unrelated => ProofProbeObservation::Unrelated,
            Self::Conflict => ProofProbeObservation::CorrelationConflict,
        }
    }
}

fn classify_prepared(expected: PreparedPacket, actual: PreparedPacket) -> Correlation {
    if expected == actual {
        Correlation::Exact
    } else {
        classify_handle_token(
            expected.handle(),
            expected.attempt(),
            actual.handle(),
            actual.attempt(),
        )
        .map_exact_conflict()
    }
}

fn classify_handle_token(
    expected_handle: AttemptHandle,
    expected_token: AttemptToken,
    actual_handle: AttemptHandle,
    actual_token: AttemptToken,
) -> Correlation {
    match (
        expected_handle == actual_handle,
        expected_token == actual_token,
    ) {
        (true, true) => Correlation::Exact,
        (false, false) => Correlation::Unrelated,
        (true, false) | (false, true) => Correlation::Conflict,
    }
}

impl Correlation {
    const fn map_exact_conflict(self) -> Self {
        match self {
            Self::Exact | Self::Conflict => Self::Conflict,
            Self::Unrelated => Self::Unrelated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EncodedPacketSha256, NodeInstanceId, PacketInterfaceId, PacketSignalObservation,
        PacketSlotId, TxLeaseDeadline, TxRecoveryPriorPhase, TxRecoveryReason, TxTarget,
    };

    fn prepared_packet(tag: u8, kind: DataReceiptKind) -> PreparedPacket {
        PreparedPacket {
            packet_len: 64,
            encoded_packet_sha256: EncodedPacketSha256::new([tag.wrapping_add(1); 32]),
            attempt: AttemptToken([tag; 32]),
            receipt_kind: kind,
            handle: AttemptHandle {
                owner_identity: [0x44; 16],
                instance: NodeInstanceId::new([0x55; 16]),
                slot: usize::from(tag),
                generation: u64::from(tag) + 1,
            },
            slot: PacketSlotId(u16::from(tag)),
            target: TxTarget::Only(PacketInterfaceId::new(3)),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(5_000)),
        }
    }

    fn authorized(
        prepared: PreparedPacket,
        interface: PacketInterfaceId,
    ) -> AuthorizedFrameObservation {
        AuthorizedFrameObservation {
            handle: prepared.handle(),
            attempt: prepared.attempt(),
            interface,
            packet_len: usize::from(prepared.packet_len()),
            encoded_packet_sha256: prepared.encoded_packet_sha256(),
        }
    }

    fn terminal_attempt(
        prepared: PreparedPacket,
        outcome: AttemptOutcome,
        ingress: Option<PacketIngressObservation>,
    ) -> TerminalAttempt {
        TerminalAttempt {
            handle: prepared.handle(),
            token: prepared.attempt(),
            kind: prepared.receipt_kind(),
            link: None,
            outcome,
            ingress,
        }
    }

    fn recovery(prepared: PreparedPacket) -> TxRecoveryObservation {
        TxRecoveryObservation {
            handle: prepared.handle(),
            attempt: prepared.attempt(),
            record: TxRecoveryRecord {
                instance: NodeInstanceId::new([0x55; 16]),
                slot: prepared.slot_id(),
                dispatch_generation: 9,
                interface: Some(PacketInterfaceId::new(3)),
                deadline: prepared.deadline(),
                observed_at: MonotonicMillis::new(1_250),
                prior_phase: TxRecoveryPriorPhase::Authorized,
                may_have_transmitted: true,
                reason: TxRecoveryReason::DeadlineExpired,
            },
        }
    }

    #[test]
    fn one_slot_retains_dispatch_authorization_recovery_and_proof_signal_until_exact_eviction() {
        let destination = DestinationHash::new([0xa1; 16]);
        let prepared = prepared_packet(2, DataReceiptKind::DestinationData);
        let mut tracker = VolatileProofProbe::new();
        let active = tracker
            .try_start(destination, prepared, MonotonicMillis::new(1_000))
            .unwrap();
        assert_eq!(active.destination(), destination);
        assert!(tracker.matches_active_prepared(prepared));
        assert!(tracker.matches_active_attempt(prepared.attempt()));
        assert_eq!(
            tracker.try_start(destination, prepared, MonotonicMillis::new(1_001)),
            Err(ProofProbeStartError::Busy)
        );

        assert_eq!(
            tracker.mark_first_dispatch(prepared, MonotonicMillis::new(1_100), Some(2)),
            ProofProbeObservation::Recorded
        );
        assert_eq!(
            tracker.mark_first_dispatch(prepared, MonotonicMillis::new(1_100), Some(2)),
            ProofProbeObservation::Duplicate
        );
        assert_eq!(
            tracker.mark_first_dispatch(prepared, MonotonicMillis::new(1_250), Some(3)),
            ProofProbeObservation::Duplicate
        );
        assert_eq!(
            tracker.active().unwrap().first_dispatch(),
            Some(ProofProbeFirstDispatch {
                at: MonotonicMillis::new(1_100),
                route_hops: Some(2),
            })
        );
        let authorized = authorized(prepared, PacketInterfaceId::new(3));
        assert_eq!(
            tracker.classify_authorized(authorized),
            ProofProbeCorrelation::Exact
        );
        assert_eq!(
            tracker.observe_authorized(authorized),
            ProofProbeObservation::Recorded
        );
        assert_eq!(
            tracker.observe_authorized(authorized),
            ProofProbeObservation::Duplicate
        );
        let ingress = PacketIngressObservation::remote(
            PacketInterfaceId::new(7),
            Some(PacketSignalObservation::new(-91, 6)),
        );
        let terminal = terminal_attempt(prepared, AttemptOutcome::Delivered, Some(ingress));
        assert_eq!(
            tracker.observe_terminal(terminal, MonotonicMillis::new(1_450)),
            ProofProbeObservation::Recorded
        );
        assert!(!tracker.matches_active_prepared(prepared));
        let result = tracker.terminal().unwrap();
        assert_eq!(result.terminal_attempt(), terminal);
        assert_eq!(result.outcome(), AttemptOutcome::Delivered);
        assert_eq!(result.proof_ingress(), Some(ingress));
        assert_eq!(
            result.active().first_dispatch().unwrap().route_hops(),
            Some(2)
        );
        assert_eq!(
            result.active().authorized_interfaces(),
            InterfaceSet::from_bits(1 << 3)
        );
        assert_eq!(result.active().recovery(), None);
        assert_eq!(result.elapsed_since_first_dispatch_ms(), Some(350));
        assert_eq!(
            tracker.observe_terminal(terminal, MonotonicMillis::new(1_451)),
            ProofProbeObservation::Duplicate
        );
        let recovery = recovery(prepared);
        assert_eq!(
            tracker.observe_recovery(recovery),
            ProofProbeObservation::Recorded
        );
        assert_eq!(
            tracker.observe_recovery(recovery),
            ProofProbeObservation::Duplicate
        );
        let result = tracker.terminal().unwrap();
        assert_eq!(result.active().recovery(), Some(recovery.record()));

        let mismatched = terminal_attempt(
            prepared_packet(3, DataReceiptKind::DestinationData),
            AttemptOutcome::Delivered,
            Some(ingress),
        );
        assert_eq!(
            tracker.evict_terminal(mismatched),
            Err(ProofProbeEvictionError::TerminalMismatch)
        );
        assert_eq!(tracker.terminal(), Some(result));
        assert_eq!(tracker.evict_terminal(terminal), Ok(result));
        assert!(tracker.is_vacant());
    }

    #[test]
    fn partial_and_unrelated_correlations_never_mutate_the_active_probe() {
        let destination = DestinationHash::new([0xb1; 16]);
        let prepared = prepared_packet(4, DataReceiptKind::DestinationData);
        let mut tracker = VolatileProofProbe::new();
        tracker
            .try_start(destination, prepared, MonotonicMillis::new(2_000))
            .unwrap();
        let before = tracker.active().unwrap();

        let other = prepared_packet(5, DataReceiptKind::DestinationData);
        assert_eq!(
            tracker.observe_authorized(authorized(other, PacketInterfaceId::new(8))),
            ProofProbeObservation::Unrelated
        );
        let conflict = AuthorizedFrameObservation {
            handle: prepared.handle(),
            attempt: other.attempt(),
            interface: PacketInterfaceId::new(9),
            packet_len: 64,
            encoded_packet_sha256: prepared.encoded_packet_sha256(),
        };
        assert_eq!(
            tracker.observe_authorized(conflict),
            ProofProbeObservation::CorrelationConflict
        );
        assert_eq!(tracker.active(), Some(before));
        assert!(tracker.terminal().is_none());
    }

    #[test]
    fn link_data_cannot_occupy_the_standard_probe_slot() {
        let mut tracker = VolatileProofProbe::new();
        assert_eq!(
            tracker.try_start(
                DestinationHash::new([0xc1; 16]),
                prepared_packet(6, DataReceiptKind::LinkData),
                MonotonicMillis::new(3_000),
            ),
            Err(ProofProbeStartError::RequiresDestinationData)
        );
        assert!(tracker.is_vacant());
    }
}
