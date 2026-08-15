//! Volatile authenticated API state for one standard Reticulum proof probe.
//!
//! A probe deliberately bypasses the durable application-submission journal.
//! The normal transport-neutral DATA router, receipt table, proof validation,
//! interface actors, and ingress path remain authoritative. This owner adds
//! only principal-scoped API metadata and a bounded state machine around the
//! exact node-core attempt correlation.

use reticulum_device_api::{
    CapabilityAvailability, IdempotencyKey, IngressObservation, IngressSignal, PrincipalId,
    ProbeFailure, ProbeId, ProbePhase, ProbePollResponse, ProbeStartRequest, ProbeSuccess,
};
use reticulum_device_api_adapter::{
    ReticulumProbePort, ReticulumProbePortError, ReticulumProbeStartDisposition,
};
use reticulum_node_core::{
    AttemptOutcome, AuthorizedFrameObservation, DestinationHash, MonotonicMillis, PreparedPacket,
    ProofProbeCorrelation, ProofProbeEvictionError, ProofProbeObservation, ProofProbeStartError,
    TerminalAttempt, TxRecoveryObservation, VolatileProofProbe,
};

/// Maximum time spent resolving the identity behind the user-supplied
/// announce-known destination.
pub const PROBE_IDENTITY_LOOKUP_TIMEOUT_MS: u64 = 60_000;
/// Maximum time spent resolving a usable route to `rnstransport.probe`.
pub const PROBE_PATH_LOOKUP_TIMEOUT_MS: u64 = 60_000;
/// Maximum time a path-ready probe may wait for packet and receipt capacity.
pub const PROBE_PREPARE_TIMEOUT_MS: u64 = 30_000;
/// Minimum interval between bounded path-request attempts.
pub const PROBE_PATH_REQUEST_INTERVAL_MS: u64 = 5_000;
/// Owner deadline supplied when the exact probe DATA packet is prepared.
pub const PROBE_DATA_OWNER_LEASE_MS: u64 = 60_000;
/// Reticulum-compatible probe payload size used by the canonical `rnprobe`.
pub const PROBE_PAYLOAD_BYTES: usize = 16;

/// Reviewed upper bound for all boot-lifetime proof-probe metadata.
pub const MAXIMUM_RETICULUM_PROBE_STATE_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProbeRecord {
    id: ProbeId,
    principal: PrincipalId,
    idempotency_key: IdempotencyKey,
    requested_destination: DestinationHash,
    prepared_route_hops: Option<u8>,
    terminal_polled: bool,
    phase: ProductProbePhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProductProbePhase {
    ResolvingIdentity {
        deadline_ms: u64,
        next_request_ms: u64,
    },
    ResolvingPath {
        probe_destination: DestinationHash,
        deadline_ms: u64,
        next_request_ms: u64,
    },
    ReadyToPrepare {
        probe_destination: DestinationHash,
        deadline_ms: u64,
    },
    AwaitingDispatch,
    AwaitingProof,
    Terminal(ProbePollResponse),
}

impl ProductProbePhase {
    const fn public_response(self) -> ProbePollResponse {
        match self {
            Self::ResolvingIdentity { .. } | Self::ResolvingPath { .. } => {
                ProbePollResponse::Pending(ProbePhase::PathLookup)
            }
            Self::ReadyToPrepare { .. } | Self::AwaitingDispatch => {
                ProbePollResponse::Pending(ProbePhase::AwaitingDispatch)
            }
            Self::AwaitingProof => ProbePollResponse::Pending(ProbePhase::AwaitingProof),
            Self::Terminal(response) => response,
        }
    }

    const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal(_))
    }
}

/// One bounded action requested from the node task by the volatile state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReticulumProbeDrive {
    /// No probe work is currently pending.
    Idle,
    /// Await an identity and optionally emit a path request for the original
    /// announce-known destination.
    ResolveIdentity {
        /// Original destination supplied by the authenticated client.
        destination: DestinationHash,
        /// Whether the rate-limited path request is due now.
        request_path: bool,
    },
    /// Await a usable path and optionally emit a path request for the canonical
    /// remote probe destination.
    ResolvePath {
        /// Derived `rnstransport.probe` destination.
        destination: DestinationHash,
        /// Whether the rate-limited path request is due now.
        request_path: bool,
    },
    /// Prepare one exact destination-DATA attempt.
    Prepare {
        /// Derived `rnstransport.probe` destination.
        destination: DestinationHash,
    },
    /// A prepared attempt is awaiting transport dispatch or a proof.
    AwaitingAttempt,
}

/// Error applying one exact node-owner observation to the probe state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReticulumProbeObservationError {
    /// The observation partially matched the retained generation-safe
    /// correlation, indicating an internal ownership contradiction.
    CorrelationConflict,
    /// The state machine and volatile tracker disagree about an active probe.
    Invariant,
}

/// Boot-lifetime owner for one volatile standard proof probe.
#[must_use = "probe metadata and exact attempt correlation must remain resident"]
pub struct ProductReticulumProbeState {
    tracker: VolatileProofProbe,
    record: Option<ProbeRecord>,
    next_sequence: Option<u64>,
}

impl ProductReticulumProbeState {
    /// Construct an empty state whose first accepted probe has sequence one.
    pub const fn new() -> Self {
        Self {
            tracker: VolatileProofProbe::new(),
            record: None,
            next_sequence: Some(1),
        }
    }

    /// Return the next bounded node-task action, applying lookup timeouts
    /// before exposing it.
    pub fn next_drive(&mut self, now_ms: u64) -> ReticulumProbeDrive {
        let Some(mut record) = self.record else {
            return ReticulumProbeDrive::Idle;
        };
        let drive = match record.phase {
            ProductProbePhase::ResolvingIdentity {
                deadline_ms,
                next_request_ms,
            } => {
                if now_ms >= deadline_ms {
                    record.phase = ProductProbePhase::Terminal(ProbePollResponse::Failed(
                        ProbeFailure::IdentityUnavailable,
                    ));
                    record.terminal_polled = false;
                    ReticulumProbeDrive::Idle
                } else {
                    ReticulumProbeDrive::ResolveIdentity {
                        destination: record.requested_destination,
                        request_path: now_ms >= next_request_ms,
                    }
                }
            }
            ProductProbePhase::ResolvingPath {
                probe_destination,
                deadline_ms,
                next_request_ms,
            } => {
                if now_ms >= deadline_ms {
                    record.phase = ProductProbePhase::Terminal(ProbePollResponse::Failed(
                        ProbeFailure::NoPath,
                    ));
                    record.terminal_polled = false;
                    ReticulumProbeDrive::Idle
                } else {
                    ReticulumProbeDrive::ResolvePath {
                        destination: probe_destination,
                        request_path: now_ms >= next_request_ms,
                    }
                }
            }
            ProductProbePhase::ReadyToPrepare {
                probe_destination,
                deadline_ms,
            } => {
                if now_ms >= deadline_ms {
                    record.phase = ProductProbePhase::Terminal(ProbePollResponse::Failed(
                        ProbeFailure::Dispatch,
                    ));
                    record.terminal_polled = false;
                    ReticulumProbeDrive::Idle
                } else {
                    ReticulumProbeDrive::Prepare {
                        destination: probe_destination,
                    }
                }
            }
            ProductProbePhase::AwaitingDispatch | ProductProbePhase::AwaitingProof => {
                ReticulumProbeDrive::AwaitingAttempt
            }
            ProductProbePhase::Terminal(_) => ReticulumProbeDrive::Idle,
        };
        self.record = Some(record);
        drive
    }

    /// Advance identity lookup to a canonical remote probe destination.
    pub fn identity_resolved(
        &mut self,
        expected: DestinationHash,
        probe_destination: DestinationHash,
        now_ms: u64,
    ) -> Result<(), ReticulumProbeObservationError> {
        let record = self
            .record
            .as_mut()
            .ok_or(ReticulumProbeObservationError::Invariant)?;
        if record.requested_destination != expected
            || !matches!(record.phase, ProductProbePhase::ResolvingIdentity { .. })
        {
            return Err(ReticulumProbeObservationError::Invariant);
        }
        record.phase = ProductProbePhase::ResolvingPath {
            probe_destination,
            deadline_ms: now_ms.saturating_add(PROBE_PATH_LOOKUP_TIMEOUT_MS),
            next_request_ms: now_ms,
        };
        Ok(())
    }

    /// Mark the current lookup's path-request opportunity as consumed.
    pub fn path_request_attempted(
        &mut self,
        destination: DestinationHash,
        now_ms: u64,
    ) -> Result<(), ReticulumProbeObservationError> {
        let record = self
            .record
            .as_mut()
            .ok_or(ReticulumProbeObservationError::Invariant)?;
        let next = now_ms.saturating_add(PROBE_PATH_REQUEST_INTERVAL_MS);
        match &mut record.phase {
            ProductProbePhase::ResolvingIdentity {
                next_request_ms, ..
            } if record.requested_destination == destination => {
                *next_request_ms = next;
                Ok(())
            }
            ProductProbePhase::ResolvingPath {
                probe_destination,
                next_request_ms,
                ..
            } if *probe_destination == destination => {
                *next_request_ms = next;
                Ok(())
            }
            _ => Err(ReticulumProbeObservationError::Invariant),
        }
    }

    /// Advance an exact retained path to packet preparation.
    pub fn path_resolved(
        &mut self,
        destination: DestinationHash,
        now_ms: u64,
    ) -> Result<(), ReticulumProbeObservationError> {
        let record = self
            .record
            .as_mut()
            .ok_or(ReticulumProbeObservationError::Invariant)?;
        match record.phase {
            ProductProbePhase::ResolvingPath {
                probe_destination, ..
            } if probe_destination == destination => {
                record.phase = ProductProbePhase::ReadyToPrepare {
                    probe_destination,
                    deadline_ms: now_ms.saturating_add(PROBE_PREPARE_TIMEOUT_MS),
                };
                Ok(())
            }
            _ => Err(ReticulumProbeObservationError::Invariant),
        }
    }

    /// Bind an exact prepared DATA attempt to this probe before router
    /// dispatch can expose it.
    pub fn prepared(
        &mut self,
        destination: DestinationHash,
        prepared: PreparedPacket,
        route_hops: u8,
        now_ms: u64,
    ) -> Result<(), ReticulumProbeObservationError> {
        let record = self
            .record
            .as_mut()
            .ok_or(ReticulumProbeObservationError::Invariant)?;
        if !matches!(
            record.phase,
            ProductProbePhase::ReadyToPrepare {
                probe_destination,
                ..
            } if probe_destination == destination
        ) {
            return Err(ReticulumProbeObservationError::Invariant);
        }
        self.tracker
            .try_start(destination, prepared, MonotonicMillis::new(now_ms))
            .map_err(|reason| match reason {
                ProofProbeStartError::Busy | ProofProbeStartError::RequiresDestinationData => {
                    ReticulumProbeObservationError::Invariant
                }
            })?;
        record.prepared_route_hops = Some(route_hops);
        record.phase = ProductProbePhase::AwaitingDispatch;
        Ok(())
    }

    /// End a pre-attempt probe with one bounded public failure.
    pub fn fail_before_attempt(
        &mut self,
        failure: ProbeFailure,
    ) -> Result<(), ReticulumProbeObservationError> {
        if !self.tracker.is_vacant() {
            return Err(ReticulumProbeObservationError::Invariant);
        }
        let record = self
            .record
            .as_mut()
            .ok_or(ReticulumProbeObservationError::Invariant)?;
        if record.phase.is_terminal() {
            return Err(ReticulumProbeObservationError::Invariant);
        }
        record.phase = ProductProbePhase::Terminal(ProbePollResponse::Failed(failure));
        record.terminal_polled = false;
        Ok(())
    }

    /// Return the active probe destination for one exact prepared packet.
    pub fn destination_for_prepared(
        &self,
        prepared: PreparedPacket,
    ) -> Result<Option<DestinationHash>, ReticulumProbeObservationError> {
        if self.tracker.matches_active_prepared(prepared) {
            return self
                .tracker
                .active()
                .map(|active| Some(active.destination()))
                .ok_or(ReticulumProbeObservationError::Invariant);
        }
        if self.tracker.active().is_some_and(|active| {
            active.prepared().handle() == prepared.handle()
                || active.prepared().attempt() == prepared.attempt()
        }) {
            return Err(ReticulumProbeObservationError::CorrelationConflict);
        }
        Ok(None)
    }

    /// Record first transport-router dispatch acceptance for the exact
    /// prepared attempt.
    pub fn observe_routed(
        &mut self,
        prepared: PreparedPacket,
        now_ms: u64,
    ) -> Result<bool, ReticulumProbeObservationError> {
        let route_hops = if self.tracker.matches_active_prepared(prepared) {
            let record = self
                .record
                .ok_or(ReticulumProbeObservationError::Invariant)?;
            if !matches!(
                record.phase,
                ProductProbePhase::AwaitingDispatch | ProductProbePhase::AwaitingProof
            ) {
                return Err(ReticulumProbeObservationError::Invariant);
            }
            Some(
                record
                    .prepared_route_hops
                    .ok_or(ReticulumProbeObservationError::Invariant)?,
            )
        } else {
            None
        };
        match self
            .tracker
            .mark_first_dispatch(prepared, MonotonicMillis::new(now_ms), route_hops)
        {
            ProofProbeObservation::Recorded | ProofProbeObservation::Duplicate => {
                let record = self
                    .record
                    .as_mut()
                    .ok_or(ReticulumProbeObservationError::Invariant)?;
                if !matches!(
                    record.phase,
                    ProductProbePhase::AwaitingDispatch | ProductProbePhase::AwaitingProof
                ) {
                    return Err(ReticulumProbeObservationError::Invariant);
                }
                record.phase = ProductProbePhase::AwaitingProof;
                Ok(true)
            }
            ProofProbeObservation::NoProbe | ProofProbeObservation::Unrelated => Ok(false),
            ProofProbeObservation::CorrelationConflict => {
                Err(ReticulumProbeObservationError::CorrelationConflict)
            }
        }
    }

    /// Classify one authorized-frame handoff without mutating either owner.
    pub fn classifies_authorized(
        &self,
        observation: AuthorizedFrameObservation,
    ) -> Result<bool, ReticulumProbeObservationError> {
        match self.tracker.classify_authorized(observation) {
            ProofProbeCorrelation::Exact => Ok(true),
            ProofProbeCorrelation::NoProbe | ProofProbeCorrelation::Unrelated => Ok(false),
            ProofProbeCorrelation::Conflict => {
                Err(ReticulumProbeObservationError::CorrelationConflict)
            }
        }
    }

    /// Retain an exact authorized interface observation.
    pub fn observe_authorized(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> Result<(), ReticulumProbeObservationError> {
        match self.tracker.observe_authorized(observation) {
            ProofProbeObservation::Recorded | ProofProbeObservation::Duplicate => Ok(()),
            ProofProbeObservation::NoProbe | ProofProbeObservation::Unrelated => {
                Err(ReticulumProbeObservationError::Invariant)
            }
            ProofProbeObservation::CorrelationConflict => {
                Err(ReticulumProbeObservationError::CorrelationConflict)
            }
        }
    }

    /// Whether the active or terminal probe owns one exact recovery record.
    pub fn classifies_recovery(
        &self,
        observation: TxRecoveryObservation,
    ) -> Result<bool, ReticulumProbeObservationError> {
        if self.tracker.owns_attempt(observation.attempt()) {
            Ok(true)
        } else if self
            .tracker
            .active()
            .is_some_and(|active| active.prepared().handle() == observation.attempt_handle())
            || self.tracker.terminal().is_some_and(|terminal| {
                terminal.active().prepared().handle() == observation.attempt_handle()
            })
        {
            Err(ReticulumProbeObservationError::CorrelationConflict)
        } else {
            Ok(false)
        }
    }

    /// Retain exact transport recovery evidence before its owner is released.
    pub fn observe_recovery(
        &mut self,
        observation: TxRecoveryObservation,
    ) -> Result<(), ReticulumProbeObservationError> {
        match self.tracker.observe_recovery(observation) {
            ProofProbeObservation::Recorded | ProofProbeObservation::Duplicate => Ok(()),
            ProofProbeObservation::NoProbe | ProofProbeObservation::Unrelated => {
                Err(ReticulumProbeObservationError::Invariant)
            }
            ProofProbeObservation::CorrelationConflict => {
                Err(ReticulumProbeObservationError::CorrelationConflict)
            }
        }
    }

    /// Whether the active or retained terminal probe owns this exact terminal.
    pub fn classifies_terminal(
        &self,
        terminal: TerminalAttempt,
    ) -> Result<bool, ReticulumProbeObservationError> {
        let expected = self
            .tracker
            .active()
            .map(|active| active.prepared())
            .or_else(|| {
                self.tracker
                    .terminal()
                    .map(|terminal| terminal.active().prepared())
            });
        let Some(expected) = expected else {
            return Ok(false);
        };
        match (
            expected.handle() == terminal.handle(),
            expected.attempt() == terminal.token(),
        ) {
            (true, true) => Ok(true),
            (false, false) => Ok(false),
            (true, false) | (false, true) => {
                Err(ReticulumProbeObservationError::CorrelationConflict)
            }
        }
    }

    /// Retain one exact node-core terminal before acknowledging its tombstone.
    pub fn observe_terminal(
        &mut self,
        terminal: TerminalAttempt,
        now_ms: u64,
    ) -> Result<(), ReticulumProbeObservationError> {
        match self
            .tracker
            .observe_terminal(terminal, MonotonicMillis::new(now_ms))
        {
            ProofProbeObservation::Recorded | ProofProbeObservation::Duplicate => Ok(()),
            ProofProbeObservation::NoProbe | ProofProbeObservation::Unrelated => {
                Err(ReticulumProbeObservationError::Invariant)
            }
            ProofProbeObservation::CorrelationConflict => {
                Err(ReticulumProbeObservationError::CorrelationConflict)
            }
        }
    }

    /// Evict the exact acknowledged tombstone and retain its repeatable public
    /// terminal result.
    pub fn finalize_terminal(
        &mut self,
        acknowledged: TerminalAttempt,
    ) -> Result<ProbePollResponse, ReticulumProbeObservationError> {
        let terminal = self
            .tracker
            .terminal()
            .ok_or(ReticulumProbeObservationError::Invariant)?;
        if terminal.terminal_attempt() != acknowledged {
            return Err(ReticulumProbeObservationError::CorrelationConflict);
        }
        let response = match terminal.outcome() {
            AttemptOutcome::Delivered => {
                let elapsed = terminal
                    .elapsed_since_first_dispatch_ms()
                    .ok_or(ReticulumProbeObservationError::Invariant)?;
                let elapsed = u32::try_from(elapsed).unwrap_or(u32::MAX);
                let hops = terminal
                    .active()
                    .first_dispatch()
                    .and_then(|dispatch| dispatch.route_hops())
                    .ok_or(ReticulumProbeObservationError::Invariant)?;
                let ingress = terminal
                    .proof_ingress()
                    .ok_or(ReticulumProbeObservationError::Invariant)?;
                let signal = ingress
                    .signal()
                    .map(|signal| IngressSignal::new(signal.rssi_dbm(), signal.snr_db()));
                ProbePollResponse::Succeeded(ProbeSuccess::new(
                    elapsed,
                    hops,
                    IngressObservation::new(ingress.interface().get(), signal),
                ))
            }
            AttemptOutcome::DeliveryTimeout => {
                let failure = if terminal.active().recovery().is_some() {
                    ProbeFailure::Dispatch
                } else {
                    ProbeFailure::Timeout
                };
                ProbePollResponse::Failed(failure)
            }
            AttemptOutcome::Unsent(_) => ProbePollResponse::Failed(ProbeFailure::Dispatch),
        };
        self.tracker
            .evict_terminal(acknowledged)
            .map_err(|reason| match reason {
                ProofProbeEvictionError::NotTerminal
                | ProofProbeEvictionError::TerminalMismatch => {
                    ReticulumProbeObservationError::Invariant
                }
            })?;
        let record = self
            .record
            .as_mut()
            .ok_or(ReticulumProbeObservationError::Invariant)?;
        record.phase = ProductProbePhase::Terminal(response);
        record.terminal_polled = false;
        Ok(response)
    }

    fn record_matches_runtime(&self) -> bool {
        match self.record {
            None => self.tracker.is_vacant(),
            Some(record) if record.phase.is_terminal() => self.tracker.is_vacant(),
            Some(record)
                if matches!(
                    record.phase,
                    ProductProbePhase::ResolvingIdentity { .. }
                        | ProductProbePhase::ResolvingPath { .. }
                        | ProductProbePhase::ReadyToPrepare { .. }
                ) =>
            {
                self.tracker.is_vacant()
            }
            Some(record)
                if matches!(
                    record.phase,
                    ProductProbePhase::AwaitingDispatch | ProductProbePhase::AwaitingProof
                ) =>
            {
                !self.tracker.is_vacant()
            }
            Some(_) => false,
        }
    }
}

impl Default for ProductReticulumProbeState {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(
    core::mem::size_of::<ProductReticulumProbeState>() <= MAXIMUM_RETICULUM_PROBE_STATE_BYTES
);

/// Operation-scoped adapter joining authenticated API metadata to the
/// boot-lifetime volatile probe owner.
pub struct ProductReticulumProbePort<'owner> {
    state: &'owner mut ProductReticulumProbeState,
    incarnation: [u8; 8],
    now_ms: u64,
    service_enabled: bool,
}

impl<'owner> ProductReticulumProbePort<'owner> {
    /// Borrow the probe owner for one synchronous authenticated API call.
    pub fn new(
        state: &'owner mut ProductReticulumProbeState,
        incarnation: [u8; 8],
        now_ms: u64,
        service_enabled: bool,
    ) -> Self {
        Self {
            state,
            incarnation,
            now_ms,
            service_enabled,
        }
    }

    fn allocate_id(&self) -> Result<ProbeId, ReticulumProbeStartDisposition> {
        let Some(sequence) = self.state.next_sequence else {
            return Err(ReticulumProbeStartDisposition::CapacityExhausted);
        };
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&self.incarnation);
        bytes[8..].copy_from_slice(&sequence.to_be_bytes());
        ProbeId::new(bytes).map_err(|_| ReticulumProbeStartDisposition::CapacityExhausted)
    }
}

impl ReticulumProbePort for ProductReticulumProbePort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        if self.service_enabled {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Disabled
        }
    }

    fn start(
        &mut self,
        principal: PrincipalId,
        request: ProbeStartRequest,
    ) -> Result<ReticulumProbeStartDisposition, ReticulumProbePortError> {
        if !self.service_enabled {
            return Err(ReticulumProbePortError::Unavailable);
        }
        if !self.state.record_matches_runtime() {
            return Err(ReticulumProbePortError::Invariant);
        }
        let requested_destination = DestinationHash::new(request.destination().0);
        if let Some(record) = self.state.record {
            if record.principal == principal && record.idempotency_key == request.idempotency_key()
            {
                return if record.requested_destination == requested_destination {
                    Ok(ReticulumProbeStartDisposition::Replay(record.id))
                } else {
                    Ok(ReticulumProbeStartDisposition::IdempotencyConflict)
                };
            }
            if !record.phase.is_terminal() || !record.terminal_polled {
                return Ok(ReticulumProbeStartDisposition::CapacityExhausted);
            }
            if self.state.next_sequence.is_none() {
                return Ok(ReticulumProbeStartDisposition::CapacityExhausted);
            }
        }

        let id = match self.allocate_id() {
            Ok(id) => id,
            Err(disposition) => return Ok(disposition),
        };
        let sequence = self
            .state
            .next_sequence
            .ok_or(ReticulumProbePortError::Invariant)?;
        self.state.record = Some(ProbeRecord {
            id,
            principal,
            idempotency_key: request.idempotency_key(),
            requested_destination,
            prepared_route_hops: None,
            terminal_polled: false,
            phase: ProductProbePhase::ResolvingIdentity {
                deadline_ms: self.now_ms.saturating_add(PROBE_IDENTITY_LOOKUP_TIMEOUT_MS),
                next_request_ms: self.now_ms,
            },
        });
        self.state.next_sequence = sequence.checked_add(1);
        Ok(ReticulumProbeStartDisposition::Accepted(id))
    }

    fn poll(
        &mut self,
        principal: PrincipalId,
        id: ProbeId,
    ) -> Result<Option<ProbePollResponse>, ReticulumProbePortError> {
        if !self.service_enabled {
            return Err(ReticulumProbePortError::Unavailable);
        }
        let Some(record) = self.state.record else {
            return Ok(None);
        };
        if record.principal != principal || record.id != id {
            return Ok(None);
        }
        if !self.state.record_matches_runtime() {
            return Err(ReticulumProbePortError::Invariant);
        }
        let response = record.phase.public_response();
        if record.phase.is_terminal() {
            self.state
                .record
                .as_mut()
                .ok_or(ReticulumProbePortError::Invariant)?
                .terminal_polled = true;
        }
        Ok(Some(response))
    }
}

#[cfg(test)]
#[path = "reticulum_probe_tests.rs"]
mod tests;
