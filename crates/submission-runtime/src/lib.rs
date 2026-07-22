//! Transport-neutral orchestration for durable outbound Reticulum submissions.
//!
//! This crate joins the sole storage actor to a narrow native-packet node port.
//! It deliberately knows nothing about LoRa, RNode framing, radios, boards,
//! local client transports, executors, or firmware. A product task can own this
//! runtime beside the permanent node supervisor and enforce the two critical
//! ordering rules:
//!
//! 1. persist `Queued -> Preparing` before asking the node to allocate entropy
//!    or create a packet attempt; and
//! 2. persist frame, terminal, and recovery observations before acknowledging
//!    the exact upstream owner.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use embassy_sync::blocking_mutex::raw::RawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_node_core::{
    AcknowledgeError, AuthorizedFrameObservation, DestinationHash, MonotonicMillis,
    MonotonicSeconds, SubmitError, TerminalAttempt, TxAuthorizationPolicy, TxLeaseDeadline,
    TxRecoveryObservation,
};
use reticulum_storage_actor::{
    AcceptanceProgress, BootRecoveryProgress, BoundJournalAccess, DriveError, MountError,
    PendingProgress, ProjectorOperationError, StorageActor,
};
use reticulum_storage_model::{AcceptanceCandidate, LifecycleState, SubmissionId, SubmissionIndex};
use reticulum_submission_projector::{
    AcknowledgementAction, AcknowledgementKind, AcknowledgementReply, PersistenceProgress,
    PreparedFrameObservation, ProjectionProgress, ProjectorError, SubmissionPreparationObservation,
};
use reticulum_tx_supervisor::{
    DataRecoveryAckError, DataRouterPrepareRequest, DataRouterPrepareResult,
    NodeInterfaceDataPrepareResult, NodeInterfaceSupervisor,
};

/// Native Reticulum DATA inputs supplied to the permanent node owner.
///
/// There is intentionally no concrete interface identifier here. The node's
/// authoritative router chooses from its current eligible-interface snapshot.
pub struct SubmissionPrepareRequest<'a> {
    /// Complete destination hash for the encrypted DATA packet.
    pub destination: DestinationHash,
    /// Plaintext to encrypt into one base-MTU DATA packet.
    pub plaintext: &'a [u8],
    /// Current Reticulum protocol clock.
    pub rns_now: MonotonicSeconds,
    /// Current packet-owner monotonic clock.
    pub owner_now: MonotonicMillis,
    /// Deadline for returning or retaining the exact packet owner.
    pub deadline: TxLeaseDeadline,
}

/// Narrow transport-neutral port implemented by the permanent node owner.
///
/// Implementations must retain every owner represented by a preparation or
/// lifecycle observation until the runtime reports the exact acknowledgement.
/// An interface adapter may route the prepared packet to LoRa, Wi-Fi, BLE, USB,
/// or several interfaces without changing this contract.
pub trait SubmissionNodePort {
    /// Prepare one native destination-DATA attempt through the authoritative
    /// interface router.
    fn prepare_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng;

    /// Iterate exact terminal attempts retained by the node owner.
    fn terminal_attempts(&self) -> impl Iterator<Item = TerminalAttempt> + '_;

    /// Iterate recovered packet owners awaiting durable audit and release.
    fn recovered_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_;

    /// Iterate fail-closed quarantines. Quarantines have no release action.
    fn quarantined_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_;

    /// Attempt the exact acknowledgement unlocked by durable projection.
    ///
    /// `Completed` means the named upstream owner was released, `Retryable`
    /// means it is still safely retained, and `Error` is a stable adapter-owned
    /// diagnostic that must fail the durable projector closed.
    fn acknowledge(&mut self, action: AcknowledgementAction) -> AcknowledgementReply;
}

impl<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const DATA_PACKET_BUFFERS: usize,
    const ORDINARY_PACKET_BUFFERS: usize,
    const SLOTS: usize,
    const QUEUE_DEPTH: usize,
> SubmissionNodePort
    for NodeInterfaceSupervisor<
        M,
        P,
        PATHS,
        ANNOUNCES,
        DEDUPLICATION,
        LINKS,
        DATA_PACKET_BUFFERS,
        ORDINARY_PACKET_BUFFERS,
        SLOTS,
        QUEUE_DEPTH,
    >
where
    M: RawMutex + 'static,
    P: TxAuthorizationPolicy,
{
    fn prepare_submission<R>(
        &mut self,
        request: SubmissionPrepareRequest<'_>,
        rng: &mut R,
    ) -> SubmissionPreparationObservation
    where
        R: RngCore + CryptoRng,
    {
        map_preparation_result(self.try_prepare_data(
            DataRouterPrepareRequest {
                destination: request.destination,
                plaintext: request.plaintext,
                rns_now: request.rns_now,
                deadline: request.deadline,
            },
            request.owner_now,
            rng,
        ))
    }

    fn terminal_attempts(&self) -> impl Iterator<Item = TerminalAttempt> + '_ {
        NodeInterfaceSupervisor::terminal_attempts(self)
    }

    fn recovered_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.recovered_data_observations()
    }

    fn quarantined_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.quarantined_data_observations()
    }

    fn acknowledge(&mut self, action: AcknowledgementAction) -> AcknowledgementReply {
        match action.kind() {
            AcknowledgementKind::Terminal(expected) => {
                match self.acknowledge_terminal(expected.handle()) {
                    Ok(actual) if actual == expected => AcknowledgementReply::Completed,
                    Ok(_) => AcknowledgementReply::Error(TERMINAL_ACK_RESULT_MISMATCH),
                    Err(AcknowledgeError::PacketStillBound) => AcknowledgementReply::Retryable,
                    Err(AcknowledgeError::StaleOrUnknown) => {
                        AcknowledgementReply::Error(TERMINAL_ACK_STALE_OR_UNKNOWN)
                    }
                    Err(AcknowledgeError::NotTerminal) => {
                        AcknowledgementReply::Error(TERMINAL_ACK_NOT_TERMINAL)
                    }
                    Err(AcknowledgeError::Invariant) => {
                        AcknowledgementReply::Error(TERMINAL_ACK_INVARIANT)
                    }
                }
            }
            AcknowledgementKind::Recovered(observation) => {
                match self.acknowledge_recovered_data(observation) {
                    Ok(()) => AcknowledgementReply::Completed,
                    Err(DataRecoveryAckError::SlotOutsidePool) => {
                        AcknowledgementReply::Error(RECOVERY_ACK_SLOT_OUTSIDE_POOL)
                    }
                    Err(DataRecoveryAckError::NotRecovered) => {
                        AcknowledgementReply::Error(RECOVERY_ACK_NOT_RECOVERED)
                    }
                    Err(DataRecoveryAckError::ObservationMismatch) => {
                        AcknowledgementReply::Error(RECOVERY_ACK_OBSERVATION_MISMATCH)
                    }
                }
            }
        }
    }
}

const TERMINAL_ACK_STALE_OR_UNKNOWN: u16 = 0x1001;
const TERMINAL_ACK_NOT_TERMINAL: u16 = 0x1002;
const TERMINAL_ACK_INVARIANT: u16 = 0x1003;
const TERMINAL_ACK_RESULT_MISMATCH: u16 = 0x1004;
const RECOVERY_ACK_SLOT_OUTSIDE_POOL: u16 = 0x1101;
const RECOVERY_ACK_NOT_RECOVERED: u16 = 0x1102;
const RECOVERY_ACK_OBSERVATION_MISMATCH: u16 = 0x1103;

fn map_preparation_result(
    result: NodeInterfaceDataPrepareResult,
) -> SubmissionPreparationObservation {
    match result {
        NodeInterfaceDataPrepareResult::Fault(_) => {
            SubmissionPreparationObservation::InternalFailure
        }
        NodeInterfaceDataPrepareResult::Coordinator(result) => match result {
            DataRouterPrepareResult::Prepared(hop) => {
                SubmissionPreparationObservation::Prepared(hop.prepared())
            }
            DataRouterPrepareResult::Rejected { reason, .. } => {
                SubmissionPreparationObservation::Rejected(reason)
            }
            DataRouterPrepareResult::RejectedQuarantined { observation, .. } => {
                SubmissionPreparationObservation::Quarantined(observation)
            }
            DataRouterPrepareResult::NoAvailable => SubmissionPreparationObservation::RetrySameBoot,
            DataRouterPrepareResult::OwnerMismatch | DataRouterPrepareResult::Disabled(_) => {
                SubmissionPreparationObservation::InternalFailure
            }
        },
    }
}

/// Whether the runtime can admit or advance live submissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimePhase {
    /// Complete journal replay is mounted, but interrupted submissions still
    /// require conservative durable finalization.
    Recovering,
    /// Boot recovery is complete and live service may begin.
    Ready,
}

/// One bounded boot-recovery transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryStep {
    /// One replayed submission reached its definitive boot disposition.
    Submission {
        /// Durable submission examined by this step.
        id: SubmissionId,
        /// Replay-safe, already-final, or newly finalized outcome.
        progress: BootRecoveryProgress,
    },
    /// No unreconciled replayed submission remains; service is now open.
    Complete,
    /// Service had already completed boot recovery.
    AlreadyComplete,
}

/// Why a live service operation could not be performed.
#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeError<E> {
    /// Boot recovery has not completed, so live service remains gated.
    Recovering,
    /// Physical durable mutation failed or remains ambiguous.
    Storage(DriveError<E>),
    /// The actor-owned lifecycle projector rejected the operation.
    Projection(ProjectorOperationError),
}

impl<E> From<DriveError<E>> for RuntimeError<E> {
    fn from(error: DriveError<E>) -> Self {
        Self::Storage(error)
    }
}

impl<E> From<ProjectorOperationError> for RuntimeError<E> {
    fn from(error: ProjectorOperationError) -> Self {
        Self::Projection(error)
    }
}

/// Failure from a backend-free runtime control operation.
///
/// Frame observations are deliberately independent of physical storage access:
/// they only update the actor-owned projector and retain any resulting exact
/// persistence request for a later [`SubmissionRuntime::drive_step`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeControlError {
    /// Boot recovery has not completed, so live service remains gated.
    Recovering,
    /// The actor-owned lifecycle projector rejected the operation permanently
    /// or entered a fail-closed fault.
    Projection(ProjectorOperationError),
}

impl From<ProjectorOperationError> for RuntimeControlError {
    fn from(error: ProjectorOperationError) -> Self {
        Self::Projection(error)
    }
}

/// Result of offering one retained complete native-frame observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameOfferProgress {
    /// The observation is durably represented and its source may release it.
    Durable,
    /// The source must retain and offer the identical observation again after
    /// the runtime advances the actor's pending persistence or acknowledgement.
    Retain,
}

/// Wait after an emitted path request before probing destination preparation
/// again. This matches Python LXMF's opportunistic path-request wait.
pub const PATH_DISCOVERY_RESPONSE_WAIT_MS: u64 = 7_000;

/// Minimum separation between fresh path requests for one destination.
///
/// Pinned Rete and Python Reticulum throttle automated requests for twenty
/// seconds, so the sender waits one additional second before its retry.
pub const PATH_DISCOVERY_RETRY_INTERVAL_MS: u64 = 21_000;

/// Maximum number of tagged path requests emitted for one submission in a
/// single boot before `NoPath` becomes terminal.
pub const PATH_DISCOVERY_MAX_REQUESTS: u8 = 2;

/// Exact transport-neutral path-request offer awaiting first router dispatch.
///
/// The runtime does not start response or retry clocks when this value is
/// created. The product owner must return the unchanged offer through
/// [`SubmissionRuntime::acknowledge_path_request_dispatched`] only after one
/// eligible interface has accepted the corresponding packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathDiscoveryOffer {
    id: SubmissionId,
    destination: DestinationHash,
    ordinal: u8,
}

impl PathDiscoveryOffer {
    /// Durable submission whose preparation first requested this shared path.
    pub const fn id(self) -> SubmissionId {
        self.id
    }

    /// Destination shared by every submission waiting on this discovery.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// One-based request ordinal for this destination in the current boot.
    pub const fn ordinal(self) -> u8 {
        self.ordinal
    }
}

/// A router-dispatch acknowledgement did not match the exact pending offer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathDiscoveryAcknowledgeError {
    /// No discovery is currently retained for the offered destination.
    DestinationNotPending,
    /// The destination is pending, but a different offer owns its next edge.
    OfferMismatch,
}

/// One bounded useful transition selected by the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStep {
    /// An exact actor-owned ambiguous mutation was reconciled.
    Pending(PendingProgress),
    /// One exact projector record became durable.
    Persistence(PersistenceProgress),
    /// One durable upstream acknowledgement was attempted and reported.
    Acknowledgement {
        /// Exact action attempted.
        action: AcknowledgementAction,
        /// Adapter disposition reported to the projector.
        reply: AcknowledgementReply,
    },
    /// One terminal attempt was offered to durable projection.
    Terminal {
        /// Exact node terminal observation.
        terminal: TerminalAttempt,
        /// Projector disposition.
        progress: ProjectionProgress,
    },
    /// One recovered owner was offered to durable transport audit.
    Recovered {
        /// Exact recovered-owner observation.
        observation: TxRecoveryObservation,
        /// Projector disposition.
        progress: ProjectionProgress,
    },
    /// One quarantine was offered to durable transport audit.
    Quarantined {
        /// Exact quarantined-owner observation.
        observation: TxRecoveryObservation,
        /// Projector disposition.
        progress: ProjectionProgress,
    },
    /// One durable intent was offered to the permanent node owner.
    Preparation {
        /// Durable submission being prepared.
        id: SubmissionId,
        /// Projector disposition for the node's synchronous result.
        progress: ProjectionProgress,
    },
    /// An unknown destination remains durably `Preparing` while the product
    /// emits one tagged, transport-neutral Reticulum path request.
    PathDiscoveryRequest {
        /// Exact offer retained until the first interface accepts dispatch.
        offer: PathDiscoveryOffer,
        /// Projector disposition retaining the unbound `Preparing` barrier.
        progress: ProjectionProgress,
    },
    /// One queued submission began its durable no-replay barrier.
    PreparationBarrier {
        /// Durable submission entering `Preparing`.
        id: SubmissionId,
        /// Projector disposition, normally an exact persistence handle.
        progress: ProjectionProgress,
    },
    /// No useful submission transition was currently available.
    Idle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathDiscovery {
    destination: DestinationHash,
    phase: PathDiscoveryPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathDiscoveryPhase {
    AwaitingDispatch {
        offer: PathDiscoveryOffer,
        first_request_ms: Option<u64>,
    },
    Waiting {
        first_request_ms: u64,
        next_probe_ms: u64,
        requests_sent: u8,
    },
}

/// Sole portable owner of durable submission state and scheduling phase.
///
/// The physical journal remains outside this value. Operations that can touch
/// NOR borrow a bound journal view for only that call, while projection-only
/// observations retain no backend borrow.
///
/// The permanent node owner is passed mutably to [`Self::drive_step`] so a
/// firmware task can keep node timers, ingress, and ordinary RNS actions in the
/// same coordinator without this crate depending on an executor or product
/// composition.
pub struct SubmissionRuntime<const SUBMISSIONS: usize, const PROJECTED: usize> {
    storage: StorageActor<SUBMISSIONS, PROJECTED>,
    boot_sequence: u64,
    recovery_cursor: u64,
    recovery_exhausted: bool,
    phase: RuntimePhase,
    path_discoveries: [Option<PathDiscovery>; SUBMISSIONS],
}

impl<const SUBMISSIONS: usize, const PROJECTED: usize> SubmissionRuntime<SUBMISSIONS, PROJECTED> {
    /// Mount and completely replay one already-formatted bound journal.
    ///
    /// Live service remains gated until [`Self::recover_boot_step`] reports
    /// [`RecoveryStep::Complete`]. Journal provisioning is deliberately outside
    /// this API because formatting policy is board/product specific.
    pub fn mount<A>(
        access: &mut A,
        first_submission_id: SubmissionId,
        boot_sequence: u64,
    ) -> Result<Self, MountError<A::Error>>
    where
        A: BoundJournalAccess,
    {
        Ok(Self {
            storage: StorageActor::mount(access, first_submission_id)?,
            boot_sequence,
            recovery_cursor: 0,
            recovery_exhausted: false,
            phase: RuntimePhase::Recovering,
            path_discoveries: [None; SUBMISSIONS],
        })
    }

    /// Current boot/service phase.
    pub const fn phase(&self) -> RuntimePhase {
        self.phase
    }

    /// Read-only authoritative durable submission index.
    pub const fn index(&self) -> &SubmissionIndex<SUBMISSIONS> {
        self.storage.index()
    }

    /// Read-only access to the backend-independent mounted storage state.
    pub const fn storage(&self) -> &StorageActor<SUBMISSIONS, PROJECTED> {
        &self.storage
    }

    /// Consume the runtime and recover its backend-independent storage actor.
    pub fn into_storage(self) -> StorageActor<SUBMISSIONS, PROJECTED> {
        self.storage
    }

    /// Conservatively resolve at most one replayed submission.
    pub fn recover_boot_step<A>(
        &mut self,
        access: &mut A,
    ) -> Result<RecoveryStep, RuntimeError<A::Error>>
    where
        A: BoundJournalAccess,
    {
        if self.phase == RuntimePhase::Ready {
            return Ok(RecoveryStep::AlreadyComplete);
        }
        self.storage
            .validate_access(access)
            .map_err(|error| RuntimeError::Storage(DriveError::Binding(error)))?;
        let candidate = (!self.recovery_exhausted).then(|| {
            self.storage
                .index()
                .iter()
                .filter(|submission| submission.accepted().id().get() >= self.recovery_cursor)
                .min_by_key(|submission| submission.accepted().id().get())
                .map(|submission| submission.accepted().id())
        });
        let candidate = candidate.flatten();
        let Some(id) = candidate else {
            self.phase = RuntimePhase::Ready;
            return Ok(RecoveryStep::Complete);
        };
        let progress = self
            .storage
            .finalize_boot_recovery(access, id, self.boot_sequence)?;
        if id.get() == u64::MAX {
            self.recovery_exhausted = true;
        } else {
            self.recovery_cursor = id.get() + 1;
        }
        Ok(RecoveryStep::Submission { id, progress })
    }

    /// Durably accept one authenticated, idempotent submission candidate.
    ///
    /// Physical access is borrowed only through the supplied bound journal.
    pub fn accept<A>(
        &mut self,
        access: &mut A,
        candidate: AcceptanceCandidate,
    ) -> Result<AcceptanceProgress, RuntimeError<A::Error>>
    where
        A: BoundJournalAccess,
    {
        self.ensure_ready()?;
        self.storage
            .accept(access, candidate)
            .map_err(RuntimeError::Storage)
    }

    /// Offer one exact complete native Reticulum frame observation.
    ///
    /// The interface dispatcher must retain the observation while `Retain` is
    /// returned. This includes transient actor serialization, projector-write,
    /// and acknowledgement pressure. Once ordinary runtime steps advance that
    /// retained work and the observation is durably represented, re-offering
    /// the same scalar returns `Durable`; only then may the dispatcher forget
    /// it. Permanent correlation errors and fail-closed faults remain explicit
    /// [`RuntimeControlError`] values. This is independent of any RNode or radio
    /// fragmentation performed after the native-frame boundary.
    pub fn offer_frame(
        &mut self,
        observation: PreparedFrameObservation,
    ) -> Result<FrameOfferProgress, RuntimeControlError> {
        self.ensure_control_ready()?;
        let progress = match self.storage.observe_frame(observation) {
            Ok(progress) => progress,
            Err(error) if frame_offer_must_retry(error) => {
                return Ok(FrameOfferProgress::Retain);
            }
            Err(error) => return Err(error.into()),
        };
        let persistence_pending = self.storage.pending_kind().is_some()
            || self
                .storage
                .projector()
                .pending_persistence()
                .next()
                .is_some();
        if progress == ProjectionProgress::AlreadyObserved && !persistence_pending {
            Ok(FrameOfferProgress::Durable)
        } else {
            Ok(FrameOfferProgress::Retain)
        }
    }

    /// Offer the native observation emitted by a concrete interface dispatcher.
    ///
    /// This is the production counterpart to [`Self::offer_frame`]. It drops
    /// only the selected interface scalar; durable lifecycle state is defined
    /// by the complete native packet identity and attempt correlation.
    pub fn offer_authorized_frame(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> Result<FrameOfferProgress, RuntimeControlError> {
        self.offer_frame(PreparedFrameObservation::from(observation))
    }

    /// Advance at most one useful live submission transition.
    ///
    /// Priority is deliberately durability-first: reconcile an ambiguous
    /// physical mutation, persist one planned record, perform one unlocked
    /// acknowledgement, drain node observations, prepare an already-barriered
    /// intent, then begin one new barrier. The supplied access is validated
    /// before any acknowledgement or node preparation side effect.
    pub fn drive_step<A, N, R>(
        &mut self,
        access: &mut A,
        node: &mut N,
        rns_now: MonotonicSeconds,
        owner_now: MonotonicMillis,
        deadline: TxLeaseDeadline,
        rng: &mut R,
    ) -> Result<RuntimeStep, RuntimeError<A::Error>>
    where
        A: BoundJournalAccess,
        N: SubmissionNodePort,
        R: RngCore + CryptoRng,
    {
        self.ensure_ready()?;
        self.storage
            .validate_access(access)
            .map_err(|error| RuntimeError::Storage(DriveError::Binding(error)))?;

        if self.storage.pending_kind().is_some() {
            return self
                .storage
                .drive_pending(access)
                .map(RuntimeStep::Pending)
                .map_err(RuntimeError::Storage);
        }

        let pending_persistence = self.storage.projector().pending_persistence().next();
        if let Some(request) = pending_persistence {
            return self
                .storage
                .persist_projector(access, request)
                .map(RuntimeStep::Persistence)
                .map_err(RuntimeError::Storage);
        }

        let pending_acknowledgement = self.storage.pending_acknowledgements().next();
        if let Some(action) = pending_acknowledgement {
            let reply = node.acknowledge(action);
            self.storage.report_acknowledgement(action, reply)?;
            return Ok(RuntimeStep::Acknowledgement { action, reply });
        }

        let terminal = node.terminal_attempts().next();
        if let Some(terminal) = terminal {
            let progress = self.storage.observe_terminal(terminal)?;
            return Ok(RuntimeStep::Terminal { terminal, progress });
        }

        let recovered = node.recovered_observations().next();
        if let Some(observation) = recovered {
            let progress = self.storage.observe_recovered(observation)?;
            return Ok(RuntimeStep::Recovered {
                observation,
                progress,
            });
        }

        // Quarantines deliberately remain upstream forever. Skip exact durable
        // duplicates so they cannot starve later preparation work.
        for observation in node.quarantined_observations() {
            let progress = self.storage.observe_quarantined(observation)?;
            if progress != ProjectionProgress::AlreadyObserved {
                return Ok(RuntimeStep::Quarantined {
                    observation,
                    progress,
                });
            }
        }

        let ready = self.storage.index().iter().find_map(|submission| {
            let id = submission.accepted().id();
            let intent = self.storage.ready_intent(id)?;
            let destination = DestinationHash::new(*intent.destination().as_bytes());
            self.path_discovery_due(destination, owner_now.get())
                .then_some((id, intent, destination))
        });
        if let Some((id, intent, destination)) = ready {
            let observation = node.prepare_submission(
                SubmissionPrepareRequest {
                    destination,
                    plaintext: intent.payload(),
                    rns_now,
                    owner_now,
                    deadline,
                },
                rng,
            );
            let (observation, path_offer) =
                self.classify_path_discovery(id, destination, observation, owner_now.get());
            let progress = self.storage.observe_preparation(id, observation)?;
            if let Some(offer) = path_offer {
                return Ok(RuntimeStep::PathDiscoveryRequest { offer, progress });
            }
            return Ok(RuntimeStep::Preparation { id, progress });
        }

        let queued = self
            .storage
            .index()
            .iter()
            .find(|submission| submission.state() == LifecycleState::Queued)
            .map(|submission| submission.accepted().id());
        if let Some(id) = queued {
            let progress = self.storage.begin_preparation(id)?;
            return Ok(RuntimeStep::PreparationBarrier { id, progress });
        }

        Ok(RuntimeStep::Idle)
    }

    fn path_discovery_due(&self, destination: DestinationHash, now_ms: u64) -> bool {
        self.path_discoveries
            .iter()
            .flatten()
            .find(|discovery| discovery.destination == destination)
            .is_none_or(|discovery| match discovery.phase {
                PathDiscoveryPhase::AwaitingDispatch { .. } => false,
                PathDiscoveryPhase::Waiting { next_probe_ms, .. } => now_ms >= next_probe_ms,
            })
    }

    fn classify_path_discovery(
        &mut self,
        id: SubmissionId,
        destination: DestinationHash,
        observation: SubmissionPreparationObservation,
        now_ms: u64,
    ) -> (SubmissionPreparationObservation, Option<PathDiscoveryOffer>) {
        if observation
            != SubmissionPreparationObservation::Rejected(SubmitError::UnknownDestination)
        {
            if !matches!(
                observation,
                SubmissionPreparationObservation::RetrySameBoot
                    | SubmissionPreparationObservation::Rejected(
                        SubmitError::AttemptLedgerFull { .. }
                            | SubmitError::ReceiptTableFull { .. }
                            | SubmitError::ReceiptHashAlreadyTracked
                    )
            ) {
                self.clear_path_discovery(destination);
            }
            return (observation, None);
        }

        let existing = self
            .path_discoveries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.destination == destination));
        let Some(index) = existing else {
            let Some(index) = self.path_discoveries.iter().position(Option::is_none) else {
                return (SubmissionPreparationObservation::InternalFailure, None);
            };
            let offer = PathDiscoveryOffer {
                id,
                destination,
                ordinal: 1,
            };
            self.path_discoveries[index] = Some(PathDiscovery {
                destination,
                phase: PathDiscoveryPhase::AwaitingDispatch {
                    offer,
                    first_request_ms: None,
                },
            });
            return (SubmissionPreparationObservation::RetrySameBoot, Some(offer));
        };

        let discovery = self.path_discoveries[index]
            .as_mut()
            .expect("the located discovery slot must remain occupied");
        match discovery.phase {
            PathDiscoveryPhase::AwaitingDispatch { .. } => {
                (SubmissionPreparationObservation::RetrySameBoot, None)
            }
            PathDiscoveryPhase::Waiting {
                first_request_ms,
                requests_sent,
                ..
            } if requests_sent < PATH_DISCOVERY_MAX_REQUESTS => {
                let retry_not_before =
                    first_request_ms.saturating_add(PATH_DISCOVERY_RETRY_INTERVAL_MS);
                if now_ms < retry_not_before {
                    discovery.phase = PathDiscoveryPhase::Waiting {
                        first_request_ms,
                        next_probe_ms: retry_not_before,
                        requests_sent,
                    };
                    return (SubmissionPreparationObservation::RetrySameBoot, None);
                }
                let ordinal = requests_sent.saturating_add(1);
                let offer = PathDiscoveryOffer {
                    id,
                    destination,
                    ordinal,
                };
                discovery.phase = PathDiscoveryPhase::AwaitingDispatch {
                    offer,
                    first_request_ms: Some(first_request_ms),
                };
                (SubmissionPreparationObservation::RetrySameBoot, Some(offer))
            }
            PathDiscoveryPhase::Waiting { .. } => {
                self.path_discoveries[index] = None;
                (observation, None)
            }
        }
    }

    fn clear_path_discovery(&mut self, destination: DestinationHash) {
        if let Some(index) = self
            .path_discoveries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.destination == destination))
        {
            self.path_discoveries[index] = None;
        }
    }

    /// Start response and retry clocks after the request's first real router
    /// dispatch onto an eligible interface.
    ///
    /// This operation is backend-free. The exact offer remains pending on any
    /// mismatch so a caller can fail closed without losing correlation.
    pub fn acknowledge_path_request_dispatched(
        &mut self,
        offer: PathDiscoveryOffer,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), PathDiscoveryAcknowledgeError> {
        let Some(discovery) = self
            .path_discoveries
            .iter_mut()
            .flatten()
            .find(|discovery| discovery.destination == offer.destination)
        else {
            return Err(PathDiscoveryAcknowledgeError::DestinationNotPending);
        };
        let PathDiscoveryPhase::AwaitingDispatch {
            offer: expected,
            first_request_ms,
        } = discovery.phase
        else {
            return Err(PathDiscoveryAcknowledgeError::OfferMismatch);
        };
        if expected != offer {
            return Err(PathDiscoveryAcknowledgeError::OfferMismatch);
        }
        let dispatched_at = dispatched_at.get();
        discovery.phase = PathDiscoveryPhase::Waiting {
            first_request_ms: first_request_ms.unwrap_or(dispatched_at),
            next_probe_ms: dispatched_at.saturating_add(PATH_DISCOVERY_RESPONSE_WAIT_MS),
            requests_sent: offer.ordinal,
        };
        Ok(())
    }

    fn ensure_ready<E>(&self) -> Result<(), RuntimeError<E>> {
        if self.phase == RuntimePhase::Ready {
            Ok(())
        } else {
            Err(RuntimeError::Recovering)
        }
    }

    fn ensure_control_ready(&self) -> Result<(), RuntimeControlError> {
        if self.phase == RuntimePhase::Ready {
            Ok(())
        } else {
            Err(RuntimeControlError::Recovering)
        }
    }
}

fn frame_offer_must_retry(error: ProjectorOperationError) -> bool {
    matches!(
        error,
        ProjectorOperationError::Busy { .. }
            | ProjectorOperationError::Rejected(
                ProjectorError::WritePending | ProjectorError::AcknowledgementPending
            )
    )
}

#[cfg(test)]
mod tests;
