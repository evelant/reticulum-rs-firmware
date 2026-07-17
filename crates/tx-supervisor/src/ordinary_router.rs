//! Permanent ordinary-action ownership bridge into the interface router.

use core::{
    future::poll_fn,
    task::{Context, Poll},
};

use embassy_sync::blocking_mutex::raw::RawMutex;
use reticulum_interface_router::{EligibleInterfaceSetError, OutboundRouter, RouteError};
use reticulum_node_core::{
    MonotonicMillis, NodeActions, OrdinaryActionAdmissionError, OrdinaryActionAdmissionFailure,
    OrdinaryActionAdmissionRequest, OrdinaryActionBatch, OrdinaryActionCapacitySnapshot,
    OrdinaryActionOwner, OrdinaryAuthorizationErrorKind, OrdinaryAuthorizationFailure,
    OrdinaryBufferPool, OrdinaryBufferPoolError, OrdinaryCompletionDisposition,
    OrdinaryCompletionError, OrdinaryCompletionFailure, OrdinaryPacketReturnParkFailure,
    OrdinaryPacketSlotId, OrdinaryPreparedPacket, OrdinaryQuarantineReason, OrdinaryTxCompletion,
    OrdinaryTxJob, OrdinaryTxPermitReply, OrdinaryTxPermitRequest, OrdinaryTxQuarantine,
    PacketInterfaceId, TxAuthorizationCandidate, TxAuthorizationPolicy, TxCompletionCode,
    TxLeaseDeadline, TxOwnerScope, TxPolicyDecision, TxPolicyDenial,
};

/// Product policy for ordinary jobs rejected before they enter an actor queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryRouterConfig {
    route_rejected_code: TxCompletionCode,
    deadline_expired_code: TxCompletionCode,
    fault_drain_code: TxCompletionCode,
}

impl OrdinaryRouterConfig {
    /// Construct a policy with the completion code used for a definitely
    /// unpermitted interface hop rejected by the router.
    pub const fn new(
        route_rejected_code: TxCompletionCode,
        deadline_expired_code: TxCompletionCode,
        fault_drain_code: TxCompletionCode,
    ) -> Self {
        Self {
            route_rejected_code,
            deadline_expired_code,
            fault_drain_code,
        }
    }

    /// Completion code used when no interface actor accepted a hop.
    pub const fn route_rejected_code(self) -> TxCompletionCode {
        self.route_rejected_code
    }

    /// Completion code used when a coordinator-owned job reaches its deadline
    /// before an actor accepts it.
    pub const fn deadline_expired_code(self) -> TxCompletionCode {
        self.deadline_expired_code
    }

    /// Completion code used to cancel locally retained owners while draining
    /// after a permanent coordinator fault.
    pub const fn fault_drain_code(self) -> TxCompletionCode {
        self.fault_drain_code
    }
}

/// Deadline policy for one complete native action envelope.
///
/// The coordinator samples its own fresh `step(now)` value and derives the
/// enabled-interface snapshot from the authoritative router at each admission
/// attempt. Callers cannot inject either value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryRouterAdmission {
    deadline: TxLeaseDeadline,
}

impl OrdinaryRouterAdmission {
    /// Construct one envelope deadline.
    pub const fn new(deadline: TxLeaseDeadline) -> Self {
        Self { deadline }
    }

    /// Shared deadline for every packet in this atomic envelope.
    pub const fn deadline(self) -> TxLeaseDeadline {
        self.deadline
    }
}

/// Why an ordinary coordinator could not take permanent ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryRouterBuildError {
    /// At least one packet buffer is required.
    ZeroPacketBuffers,
    /// The owner has fewer registered slots than the configured pool.
    RegistrationIncomplete {
        /// Configured packet-buffer count.
        expected: usize,
        /// Slots registered with the ordinary owner.
        registered: usize,
    },
    /// An owner slot was already active before permanent construction.
    ActiveOwners {
        /// Active slots observed during construction.
        active: usize,
    },
    /// The caller-owned parking table does not contain every registered owner.
    ParkingIncomplete {
        /// Configured packet-buffer count.
        expected: usize,
        /// Exact buffer owners parked in the table.
        parked: usize,
    },
}

/// Failed construction retaining the exact owner, pool, and configuration.
#[must_use = "failed construction retains every ordinary packet owner"]
pub struct OrdinaryRouterBuildFailure<const PACKET_BUFFERS: usize> {
    reason: OrdinaryRouterBuildError,
    owner: OrdinaryActionOwner<PACKET_BUFFERS>,
    pool: OrdinaryBufferPool<'static, PACKET_BUFFERS>,
    config: OrdinaryRouterConfig,
}

impl<const PACKET_BUFFERS: usize> OrdinaryRouterBuildFailure<PACKET_BUFFERS> {
    /// Scalar reason permanent construction did not complete.
    pub const fn reason(&self) -> OrdinaryRouterBuildError {
        self.reason
    }

    /// Recover every unchanged construction input.
    pub fn into_parts(
        self,
    ) -> (
        OrdinaryActionOwner<PACKET_BUFFERS>,
        OrdinaryBufferPool<'static, PACKET_BUFFERS>,
        OrdinaryRouterConfig,
    ) {
        (self.owner, self.pool, self.config)
    }
}

/// Permanent fail-closed reason retained by the ordinary coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryRouterFault {
    /// Atomic admission exposed a non-transient validation or owner fault.
    Admission(OrdinaryActionAdmissionError),
    /// The authoritative registry cannot be represented by node-core's current
    /// compact enabled-interface profile.
    RegistryProfile(EligibleInterfaceSetError),
    /// A returned completion did not correlate with the ordinary owner.
    Completion(OrdinaryCompletionError),
    /// Node-core quarantined a same-generation packet owner.
    Quarantine(OrdinaryQuarantineReason),
    /// A terminal packet owner could not return to its stable parking slot.
    Parking(OrdinaryBufferPoolError),
    /// Private coordinator ownership tables contradicted one another.
    InternalInvariant,
}

/// Kind of exact non-copy owner retained behind a permanent fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryRouterFaultResidueKind {
    /// Original protocol-action envelope and admission request.
    Actions,
    /// Admission failure retaining the original protocol-action envelope.
    AdmissionFailure,
    /// Completion-correlation failure retaining the packet owner.
    CompletionFailure,
    /// Completion rejected by a private coordinator invariant before it could
    /// reach node-core correlation.
    Completion,
    /// Same-generation quarantine retaining the packet owner.
    Quarantine,
    /// Failed parking return retaining the exact available buffer owner.
    ParkingReturn,
    /// A routed packet job that could not safely enter private state.
    Job,
}

enum OrdinaryRouterFaultResidue {
    Actions {
        actions: NodeActions,
        admission: OrdinaryRouterAdmission,
    },
    AdmissionFailure {
        failure: OrdinaryActionAdmissionFailure,
        admission: OrdinaryRouterAdmission,
    },
    CompletionFailure(OrdinaryCompletionFailure<'static>),
    Completion(OrdinaryTxCompletion<'static>),
    Quarantine(OrdinaryTxQuarantine<'static>),
    ParkingReturn(OrdinaryPacketReturnParkFailure<'static>),
    Job(OrdinaryTxJob<'static>),
}

impl OrdinaryRouterFaultResidue {
    fn kind(&self) -> OrdinaryRouterFaultResidueKind {
        match self {
            Self::Actions { actions, admission } => {
                let _ = (actions, admission);
                OrdinaryRouterFaultResidueKind::Actions
            }
            Self::AdmissionFailure { failure, admission } => {
                let _ = (failure, admission);
                OrdinaryRouterFaultResidueKind::AdmissionFailure
            }
            Self::CompletionFailure(failure) => {
                let _ = failure;
                OrdinaryRouterFaultResidueKind::CompletionFailure
            }
            Self::Completion(completion) => {
                let _ = completion;
                OrdinaryRouterFaultResidueKind::Completion
            }
            Self::Quarantine(quarantine) => {
                let _ = quarantine;
                OrdinaryRouterFaultResidueKind::Quarantine
            }
            Self::ParkingReturn(returned) => {
                let _ = returned;
                OrdinaryRouterFaultResidueKind::ParkingReturn
            }
            Self::Job(job) => {
                let _ = job;
                OrdinaryRouterFaultResidueKind::Job
            }
        }
    }
}

/// Existing coordinator output that must be drained before another action
/// envelope can be accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryRouterBusyReason {
    /// An action envelope is waiting for atomic admission.
    PendingActions,
    /// An admitted batch still contains packet or non-packet owners.
    AdmittedBatch,
    /// Events or the unroutable-action count have not been taken by the
    /// runtime.
    NonPacketActions,
    /// A recoverably rejected envelope has not been taken by the runtime.
    RejectedActions,
}

/// Why a protocol-action envelope was not accepted immediately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryRouterOfferError {
    /// The coordinator is permanently disabled.
    Disabled(OrdinaryRouterFault),
    /// An earlier envelope or its non-packet output still occupies the input
    /// boundary.
    Busy(OrdinaryRouterBusyReason),
    /// The complete atomic envelope cannot fit this product's fixed ordinary
    /// packet-owner pool. The unchanged envelope is returned to the caller.
    EnvelopeExceedsPool {
        /// Packets in the offered envelope.
        packet_count: usize,
        /// Fixed ordinary packet-owner limit.
        limit: usize,
    },
}

/// Failed action offer retaining the exact envelope and scalar request.
#[must_use = "an unaccepted protocol-action envelope must be retried or retained"]
pub struct OrdinaryRouterOfferFailure {
    reason: OrdinaryRouterOfferError,
    actions: NodeActions,
    admission: OrdinaryRouterAdmission,
}

impl OrdinaryRouterOfferFailure {
    /// Scalar reason the action envelope was not accepted.
    pub const fn reason(&self) -> OrdinaryRouterOfferError {
        self.reason
    }

    /// Recover the exact action envelope and its unchanged admission request.
    pub fn into_parts(self) -> (NodeActions, OrdinaryRouterAdmission) {
        (self.actions, self.admission)
    }
}

/// Recoverably rejected action envelope retained outside the fixed packet
/// owner pool.
#[must_use = "a rejected protocol-action envelope must be handled explicitly"]
pub struct OrdinaryRouterRejectedActions {
    reason: OrdinaryActionAdmissionError,
    actions: NodeActions,
    admission: OrdinaryRouterAdmission,
}

impl OrdinaryRouterRejectedActions {
    /// Semantic reason the envelope cannot currently enter fixed ownership.
    pub const fn reason(&self) -> OrdinaryActionAdmissionError {
        self.reason
    }

    /// Recover the unchanged envelope and deadline policy.
    pub fn into_parts(self) -> (NodeActions, OrdinaryRouterAdmission) {
        (self.actions, self.admission)
    }
}

/// Why an ordinary permit request could not be authorized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryPermitAuthorizationError {
    /// Node-core rejected the request's owner, generation, interface, or phase.
    Owner(OrdinaryAuthorizationErrorKind),
}

/// Failed ordinary authorization retaining the exact non-copy request.
#[must_use = "a failed ordinary permit request must remain explicitly owned"]
pub struct OrdinaryPermitAuthorizationFailure {
    reason: OrdinaryPermitAuthorizationError,
    request: OrdinaryTxPermitRequest,
}

impl OrdinaryPermitAuthorizationFailure {
    /// Scalar reason authorization did not produce a reply.
    pub const fn reason(&self) -> OrdinaryPermitAuthorizationError {
        self.reason
    }

    /// Recover the unchanged permit request.
    pub fn into_request(self) -> OrdinaryTxPermitRequest {
        self.request
    }
}

struct FaultDrainPolicy;

impl TxAuthorizationPolicy for FaultDrainPolicy {
    fn authorize(&mut self, _candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        TxPolicyDecision::Deny(TxPolicyDenial::ResourceUnavailable)
    }
}

/// Why an actor completion was not accepted by this coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryCompletionAcceptError {
    /// The coordinator is permanently disabled.
    Disabled(OrdinaryRouterFault),
    /// The completion names a slot outside this product configuration.
    SlotOutsidePool {
        /// Slot carried by the completion owner.
        slot: OrdinaryPacketSlotId,
        /// Fixed coordinator slot count.
        limit: usize,
    },
    /// No routed owner is awaiting this slot's completion.
    NoActiveOwner(OrdinaryPacketSlotId),
    /// Slot and generation do not match the exact routed owner.
    OwnerMismatch {
        /// Exact completion binding expected by the coordinator.
        expected: OrdinaryPreparedPacket,
        /// Binding carried by the supplied completion.
        supplied: OrdinaryPreparedPacket,
    },
    /// One earlier completion is already retained for reconciliation.
    CompletionPending(OrdinaryPacketSlotId),
}

/// Rejected actor completion retaining the exact packet owner.
#[must_use = "a rejected ordinary completion must remain exactly owned"]
pub struct OrdinaryCompletionAcceptFailure {
    reason: OrdinaryCompletionAcceptError,
    completion: OrdinaryTxCompletion<'static>,
}

impl OrdinaryCompletionAcceptFailure {
    /// Scalar reason the completion was not accepted.
    pub const fn reason(&self) -> OrdinaryCompletionAcceptError {
        self.reason
    }

    /// Recover the unchanged owning completion.
    pub fn into_completion(self) -> OrdinaryTxCompletion<'static> {
        self.completion
    }
}

/// Successful completion reconciliation performed by one coordinator step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryRouterCompletionProgress {
    /// The same packet generation is ready for its next serialized interface.
    Next {
        /// Stable owner slot.
        slot: OrdinaryPacketSlotId,
        /// Next selected interface.
        interface: PacketInterfaceId,
    },
    /// The exact packet buffer returned to the reusable pool.
    Returned {
        /// Stable owner slot.
        slot: OrdinaryPacketSlotId,
    },
}

/// Result of one bounded synchronous coordinator transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "coordinator progress, pressure, and retained faults must be observed"]
pub enum OrdinaryRouterStep {
    /// Permanent fault; every exact residue remains inside the coordinator.
    Disabled(OrdinaryRouterFault),
    /// One actor completion advanced or returned its node-core owner.
    Completion(OrdinaryRouterCompletionProgress),
    /// One packet owner entered its selected interface actor queue.
    Routed {
        /// Stable packet-owner slot.
        slot: OrdinaryPacketSlotId,
        /// Selected interface.
        interface: PacketInterfaceId,
    },
    /// The selected actor queue was full; the exact job remains retained.
    RouteBackpressured {
        /// Stable packet-owner slot.
        slot: OrdinaryPacketSlotId,
        /// Selected interface.
        interface: PacketInterfaceId,
    },
    /// Registry state rejected a hop before actor ownership transfer. The hop
    /// is now retained as definitely unpermitted for node-core reconciliation.
    RouteRejected {
        /// Stable packet-owner slot.
        slot: OrdinaryPacketSlotId,
        /// Transport-neutral registry reason.
        reason: RouteError,
    },
    /// A coordinator-owned job reached its deadline before an actor accepted
    /// it and is retained as a local cancellation completion.
    PendingJobExpired {
        /// Stable packet-owner slot.
        slot: OrdinaryPacketSlotId,
    },
    /// A coordinator-owned job is being cancelled so already-moving owners can
    /// drain after a permanent fault disabled fresh work.
    PendingJobCancelledForFault {
        /// Stable packet-owner slot.
        slot: OrdinaryPacketSlotId,
    },
    /// A not-yet-admitted envelope moved into explicit input fault residue so
    /// it cannot be lost or leave a stale deadline wake behind.
    PendingActionsRetainedForFault,
    /// One packet owner moved from an admitted batch into its stable pending
    /// route slot.
    PacketStaged {
        /// Stable packet-owner slot.
        slot: OrdinaryPacketSlotId,
    },
    /// Events and unroutable count are ready for the runtime to take.
    NonPacketActionsReady,
    /// One complete action envelope was admitted atomically.
    ActionsAdmitted {
        /// Number of packet owners admitted from the envelope.
        packet_count: usize,
    },
    /// The retained envelope currently has no eligible interface route. A
    /// later registry change or its deadline can make progress.
    WaitingForInterfaces,
    /// The complete unchanged envelope is ready for explicit caller handling
    /// after a recoverable semantic rejection.
    ActionsRejected {
        /// Typed admission reason retained with the envelope.
        reason: OrdinaryActionAdmissionError,
    },
    /// Atomic admission is waiting for currently active buffers to return.
    AdmissionBackpressured {
        /// Packet owners required by the retained envelope.
        needed: usize,
        /// Packet owners available during this attempt.
        available: usize,
    },
    /// Non-packet output must be drained before this batch can finish.
    OutputBackpressured,
    /// No synchronous work is currently observable.
    Idle,
}

impl OrdinaryRouterStep {
    /// Whether this transition completed useful synchronous work.
    pub const fn progressed(self) -> bool {
        matches!(
            self,
            Self::Completion(_)
                | Self::Routed { .. }
                | Self::RouteRejected { .. }
                | Self::PendingJobExpired { .. }
                | Self::PendingJobCancelledForFault { .. }
                | Self::PendingActionsRetainedForFault
                | Self::PacketStaged { .. }
                | Self::NonPacketActionsReady
                | Self::ActionsAdmitted { .. }
                | Self::ActionsRejected { .. }
        )
    }
}

/// Permanent ordinary-action owner and transport-neutral router bridge.
///
/// The coordinator never interprets LoRa, BLE, Wi-Fi, USB, or radio state. It
/// owns complete native Reticulum packet buffers and routes each serialized
/// fan-out hop to the interface selected by node-core. A concrete interface
/// actor owns authorization and transmission after accepting the ticket-bound
/// job.
#[must_use = "dropping the coordinator abandons ordinary packet and action owners"]
pub struct OrdinaryRouterCoordinator<const PACKET_BUFFERS: usize> {
    owner_scope: TxOwnerScope,
    owner: OrdinaryActionOwner<PACKET_BUFFERS>,
    pool: OrdinaryBufferPool<'static, PACKET_BUFFERS>,
    config: OrdinaryRouterConfig,
    pending_actions: Option<(NodeActions, OrdinaryRouterAdmission)>,
    batch: Option<OrdinaryActionBatch<'static, PACKET_BUFFERS>>,
    batch_non_packet_taken: bool,
    non_packet_actions: Option<NodeActions>,
    rejected_actions: Option<OrdinaryRouterRejectedActions>,
    pending_jobs: [Option<OrdinaryTxJob<'static>>; PACKET_BUFFERS],
    active: [Option<OrdinaryPreparedPacket>; PACKET_BUFFERS],
    pending_completions: [Option<OrdinaryTxCompletion<'static>>; PACKET_BUFFERS],
    route_cursor: usize,
    completion_cursor: usize,
    fault: Option<OrdinaryRouterFault>,
    input_fault_residue: Option<OrdinaryRouterFaultResidue>,
    slot_fault_residues: [Option<OrdinaryRouterFaultResidue>; PACKET_BUFFERS],
}

impl<const PACKET_BUFFERS: usize> OrdinaryRouterCoordinator<PACKET_BUFFERS> {
    /// Consume one fully registered, fully parked ordinary owner into a
    /// permanent coordinator.
    pub fn try_new(
        owner: OrdinaryActionOwner<PACKET_BUFFERS>,
        pool: OrdinaryBufferPool<'static, PACKET_BUFFERS>,
        config: OrdinaryRouterConfig,
    ) -> Result<Self, OrdinaryRouterBuildFailure<PACKET_BUFFERS>> {
        let capacities = owner.capacities();
        let reason = if PACKET_BUFFERS == 0 {
            Some(OrdinaryRouterBuildError::ZeroPacketBuffers)
        } else if capacities.registered != PACKET_BUFFERS {
            Some(OrdinaryRouterBuildError::RegistrationIncomplete {
                expected: PACKET_BUFFERS,
                registered: capacities.registered,
            })
        } else if capacities.active != 0 {
            Some(OrdinaryRouterBuildError::ActiveOwners {
                active: capacities.active,
            })
        } else if pool.parked_count() != PACKET_BUFFERS {
            Some(OrdinaryRouterBuildError::ParkingIncomplete {
                expected: PACKET_BUFFERS,
                parked: pool.parked_count(),
            })
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(OrdinaryRouterBuildFailure {
                reason,
                owner,
                pool,
                config,
            });
        }

        let owner_scope = owner.owner_scope();
        Ok(Self {
            owner_scope,
            owner,
            pool,
            config,
            pending_actions: None,
            batch: None,
            batch_non_packet_taken: false,
            non_packet_actions: None,
            rejected_actions: None,
            pending_jobs: core::array::from_fn(|_| None),
            active: core::array::from_fn(|_| None),
            pending_completions: core::array::from_fn(|_| None),
            route_cursor: 0,
            completion_cursor: 0,
            fault: None,
            input_fault_residue: None,
            slot_fault_residues: core::array::from_fn(|_| None),
        })
    }

    /// Opaque node-incarnation binding captured at construction.
    pub const fn owner_scope(&self) -> TxOwnerScope {
        self.owner_scope
    }

    /// Offer one complete native protocol-action envelope.
    ///
    /// Active actor jobs do not block this boundary. If too few packet buffers
    /// are currently free, [`Self::step`] retains the envelope and reports
    /// admission backpressure until exact owners return.
    pub fn try_offer_actions(
        &mut self,
        actions: NodeActions,
        admission: OrdinaryRouterAdmission,
    ) -> Result<(), OrdinaryRouterOfferFailure> {
        if let Some(fault) = self.fault {
            return Err(OrdinaryRouterOfferFailure {
                reason: OrdinaryRouterOfferError::Disabled(fault),
                actions,
                admission,
            });
        }
        let busy = if self.pending_actions.is_some() {
            Some(OrdinaryRouterBusyReason::PendingActions)
        } else if self.batch.is_some() {
            Some(OrdinaryRouterBusyReason::AdmittedBatch)
        } else if self.non_packet_actions.is_some() {
            Some(OrdinaryRouterBusyReason::NonPacketActions)
        } else if self.rejected_actions.is_some() {
            Some(OrdinaryRouterBusyReason::RejectedActions)
        } else {
            None
        };
        if let Some(busy) = busy {
            return Err(OrdinaryRouterOfferFailure {
                reason: OrdinaryRouterOfferError::Busy(busy),
                actions,
                admission,
            });
        }
        let packet_count = actions.packets.len();
        if packet_count > PACKET_BUFFERS {
            return Err(OrdinaryRouterOfferFailure {
                reason: OrdinaryRouterOfferError::EnvelopeExceedsPool {
                    packet_count,
                    limit: PACKET_BUFFERS,
                },
                actions,
                admission,
            });
        }
        self.pending_actions = Some((actions, admission));
        Ok(())
    }

    /// Accept one ordinary completion already demultiplexed by the shared
    /// outbound router.
    ///
    /// DATA completions never enter this API. Invalid or duplicate ordinary
    /// completions are returned unchanged so a higher aggregate can retain the
    /// conflicting owner without corrupting this coordinator.
    // The exact non-Copy packet owner must remain inline on every rejection;
    // heap allocation is not available on this portable boundary.
    #[allow(clippy::result_large_err)]
    pub fn try_accept_completion(
        &mut self,
        completion: OrdinaryTxCompletion<'static>,
    ) -> Result<(), OrdinaryCompletionAcceptFailure> {
        let prepared = completion.prepared();
        let slot = prepared.slot_id();
        let index = usize::from(slot.get());
        let Some(active) = self.active.get(index).copied().flatten() else {
            let reason = if let Some(fault) = self.fault {
                OrdinaryCompletionAcceptError::Disabled(fault)
            } else if index >= PACKET_BUFFERS {
                OrdinaryCompletionAcceptError::SlotOutsidePool {
                    slot,
                    limit: PACKET_BUFFERS,
                }
            } else {
                OrdinaryCompletionAcceptError::NoActiveOwner(slot)
            };
            return Err(OrdinaryCompletionAcceptFailure { reason, completion });
        };
        if active != prepared {
            return Err(OrdinaryCompletionAcceptFailure {
                reason: OrdinaryCompletionAcceptError::OwnerMismatch {
                    expected: active,
                    supplied: prepared,
                },
                completion,
            });
        }
        if self.pending_completions[index].is_some() {
            return Err(OrdinaryCompletionAcceptFailure {
                reason: OrdinaryCompletionAcceptError::CompletionPending(slot),
                completion,
            });
        }
        self.pending_completions[index] = Some(completion);
        Ok(())
    }

    /// Validate and authorize one transport-neutral ordinary permit request.
    ///
    /// A concrete actor obtains the request from `OrdinaryTxJob::begin_permit`
    /// and retains its pending packet owner until this exact reply returns.
    /// Channel pressure and request/reply scheduling belong to the surrounding
    /// permit service; this method is the sole node-owner authorization edge.
    /// After a permanent fault, a still-valid request is resolved through a
    /// forced resource-unavailable denial so an already-issued actor owner can
    /// drain without exposing bytes.
    #[allow(clippy::result_large_err)]
    pub fn authorize_tx<P>(
        &mut self,
        request: OrdinaryTxPermitRequest,
        now: MonotonicMillis,
        policy: &mut P,
    ) -> Result<OrdinaryTxPermitReply, OrdinaryPermitAuthorizationFailure>
    where
        P: TxAuthorizationPolicy,
    {
        let result = if self.fault.is_some() {
            self.owner.authorize_tx(request, now, &mut FaultDrainPolicy)
        } else {
            self.owner.authorize_tx(request, now, policy)
        };
        result.map_err(
            |failure: OrdinaryAuthorizationFailure| OrdinaryPermitAuthorizationFailure {
                reason: OrdinaryPermitAuthorizationError::Owner(failure.reason()),
                request: failure.into_request(),
            },
        )
    }

    /// Run at most one ownership-changing transition against the shared
    /// interface router.
    pub fn step<M, const SLOTS: usize, const QUEUE_DEPTH: usize>(
        &mut self,
        router: &mut OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        now: MonotonicMillis,
    ) -> OrdinaryRouterStep
    where
        M: RawMutex + 'static,
    {
        if let Some(step) = self.reconcile_one_completion(now) {
            return step;
        }
        if let Some(fault) = self.fault {
            if self.retain_pending_actions_for_fault() {
                return OrdinaryRouterStep::PendingActionsRetainedForFault;
            }
            if let Some(step) = self.cancel_one_pending_for_fault() {
                return step;
            }
            if let Some(step) = self.drain_batch_once()
                && step.progressed()
            {
                return step;
            }
            return OrdinaryRouterStep::Disabled(fault);
        }

        let mut pressure = None;
        if let Some(step) = self.route_one_job(router, now) {
            if step.progressed() {
                return step;
            }
            pressure = Some(step);
        }
        if let Some(step) = self.drain_batch_once() {
            if step.progressed() {
                return step;
            }
            pressure.get_or_insert(step);
        }
        if let Some(step) = self.admit_once(router, now) {
            if step.progressed() || matches!(step, OrdinaryRouterStep::Disabled(_)) {
                return step;
            }
            pressure.get_or_insert(step);
        }
        pressure.unwrap_or(OrdinaryRouterStep::Idle)
    }

    /// Take events and the unroutable-action count from the most recently
    /// admitted envelope. The packet vector is empty.
    pub fn take_non_packet_actions(&mut self) -> Option<NodeActions> {
        self.non_packet_actions.take()
    }

    /// Take one recoverably rejected unchanged action envelope.
    pub fn take_rejected_actions(&mut self) -> Option<OrdinaryRouterRejectedActions> {
        self.rejected_actions.take()
    }

    /// Complete retained fault, if this coordinator is permanently disabled.
    pub const fn fault(&self) -> Option<OrdinaryRouterFault> {
        self.fault
    }

    /// Kind of exact non-copy owner retained behind a permanent fault.
    pub fn fault_residue_kind(&self) -> Option<OrdinaryRouterFaultResidueKind> {
        self.input_fault_residue
            .as_ref()
            .map(OrdinaryRouterFaultResidue::kind)
            .or_else(|| {
                self.slot_fault_residues
                    .iter()
                    .find_map(|residue| residue.as_ref().map(OrdinaryRouterFaultResidue::kind))
            })
    }

    /// Number of exact retained fault residues across input and packet slots.
    pub fn fault_residue_count(&self) -> usize {
        usize::from(self.input_fault_residue.is_some())
            + self
                .slot_fault_residues
                .iter()
                .filter(|residue| residue.is_some())
                .count()
    }

    /// Kind of exact residue retained for one stable packet slot.
    pub fn slot_fault_residue_kind(
        &self,
        slot: OrdinaryPacketSlotId,
    ) -> Option<OrdinaryRouterFaultResidueKind> {
        self.slot_fault_residues
            .get(usize::from(slot.get()))
            .and_then(Option::as_ref)
            .map(OrdinaryRouterFaultResidue::kind)
    }

    /// Scalar owner occupancy, independent from DATA receipt capacity.
    pub fn capacities(&self) -> OrdinaryActionCapacitySnapshot {
        self.owner.capacities()
    }

    /// Number of exact ordinary buffers currently parked for reuse.
    pub fn parked_count(&self) -> usize {
        self.pool.parked_count()
    }

    /// Number of actor-accepted packet owners awaiting completions.
    pub fn active_count(&self) -> usize {
        self.active.iter().filter(|entry| entry.is_some()).count()
    }

    /// Earliest deadline on work still owned locally by this coordinator.
    ///
    /// Actor-accepted jobs are deliberately excluded: only the concrete actor
    /// can complete or cancel those owners, and repeatedly waking this task
    /// cannot advance them.
    pub fn next_deadline(&self) -> Option<TxLeaseDeadline> {
        let pending_actions = self
            .pending_actions
            .as_ref()
            .map(|(_, admission)| admission.deadline());
        self.pending_jobs
            .iter()
            .filter_map(|job| job.as_ref().map(OrdinaryTxJob::deadline))
            .fold(pending_actions, |earliest, deadline| match earliest {
                Some(earliest) if earliest <= deadline => Some(earliest),
                Some(_) | None => Some(deadline),
            })
    }

    /// Poll every locally retained route job for advisory actor-queue
    /// capacity.
    ///
    /// A ready result means a subsequent [`Self::step`] can either route a job
    /// or observe a changed registry rejection. A pending poll only registers
    /// wakers; it does not reserve capacity or move any exact owner.
    pub fn poll_route_progress<M, const SLOTS: usize, const QUEUE_DEPTH: usize>(
        &mut self,
        router: &mut OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        context: &mut Context<'_>,
    ) -> Poll<()>
    where
        M: RawMutex + 'static,
    {
        if self.fault.is_some() && self.pending_jobs.iter().any(Option::is_some) {
            return Poll::Ready(());
        }
        for job in self.pending_jobs.iter().filter_map(Option::as_ref) {
            if router
                .poll_route_capacity(job.interface(), job.packet_len(), context)
                .is_ready()
            {
                return Poll::Ready(());
            }
        }
        Poll::Pending
    }

    /// Wait cancellation-safely until a retained route can be retried.
    pub async fn wait_route_progress<M, const SLOTS: usize, const QUEUE_DEPTH: usize>(
        &mut self,
        router: &mut OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
    ) where
        M: RawMutex + 'static,
    {
        poll_fn(|context| self.poll_route_progress(router, context)).await
    }

    fn reconcile_one_completion(&mut self, now: MonotonicMillis) -> Option<OrdinaryRouterStep> {
        let index = self.next_completion_index()?;
        let completion = self.pending_completions[index]
            .take()
            .expect("selected completion slot must remain occupied");
        let prepared = completion.prepared();
        if self.active[index] != Some(prepared) {
            self.set_slot_fault(
                index,
                OrdinaryRouterFault::InternalInvariant,
                OrdinaryRouterFaultResidue::Completion(completion),
            );
            return Some(OrdinaryRouterStep::Disabled(
                OrdinaryRouterFault::InternalInvariant,
            ));
        }

        match self.owner.complete_tx(completion, now) {
            Err(failure) => {
                let fault = OrdinaryRouterFault::Completion(failure.reason());
                self.set_slot_fault(
                    index,
                    fault,
                    OrdinaryRouterFaultResidue::CompletionFailure(failure),
                );
                Some(OrdinaryRouterStep::Disabled(fault))
            }
            Ok(OrdinaryCompletionDisposition::Next(job)) => {
                self.active[index] = None;
                let slot = job.slot_id();
                let interface = job.interface();
                if self.pending_jobs[index].is_some() {
                    self.set_slot_fault(
                        index,
                        OrdinaryRouterFault::InternalInvariant,
                        OrdinaryRouterFaultResidue::Job(job),
                    );
                    return Some(OrdinaryRouterStep::Disabled(
                        OrdinaryRouterFault::InternalInvariant,
                    ));
                }
                self.pending_jobs[index] = Some(job);
                Some(OrdinaryRouterStep::Completion(
                    OrdinaryRouterCompletionProgress::Next { slot, interface },
                ))
            }
            Ok(OrdinaryCompletionDisposition::Returned(returned)) => {
                self.active[index] = None;
                let slot = returned.prepared().slot_id();
                match self.pool.park_return(returned) {
                    Ok(_) => Some(OrdinaryRouterStep::Completion(
                        OrdinaryRouterCompletionProgress::Returned { slot },
                    )),
                    Err(failure) => {
                        let fault = OrdinaryRouterFault::Parking(failure.reason());
                        self.set_slot_fault(
                            index,
                            fault,
                            OrdinaryRouterFaultResidue::ParkingReturn(failure),
                        );
                        Some(OrdinaryRouterStep::Disabled(fault))
                    }
                }
            }
            Ok(OrdinaryCompletionDisposition::Quarantined(quarantine)) => {
                let fault = OrdinaryRouterFault::Quarantine(quarantine.reason());
                self.set_slot_fault(
                    index,
                    fault,
                    OrdinaryRouterFaultResidue::Quarantine(quarantine),
                );
                Some(OrdinaryRouterStep::Disabled(fault))
            }
        }
    }

    fn route_one_job<M, const SLOTS: usize, const QUEUE_DEPTH: usize>(
        &mut self,
        router: &mut OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        now: MonotonicMillis,
    ) -> Option<OrdinaryRouterStep>
    where
        M: RawMutex + 'static,
    {
        let index = self.next_job_index()?;
        let job = self.pending_jobs[index]
            .take()
            .expect("selected route slot must remain occupied");
        let slot = job.slot_id();
        let interface = job.interface();
        let prepared = job.prepared();
        if now >= job.deadline().instant() {
            if self.active[index].is_some() || self.pending_completions[index].is_some() {
                self.set_slot_fault(
                    index,
                    OrdinaryRouterFault::InternalInvariant,
                    OrdinaryRouterFaultResidue::Job(job),
                );
                return Some(OrdinaryRouterStep::Disabled(
                    OrdinaryRouterFault::InternalInvariant,
                ));
            }
            self.active[index] = Some(prepared);
            self.pending_completions[index] = Some(job.cancel(self.config.deadline_expired_code));
            return Some(OrdinaryRouterStep::PendingJobExpired { slot });
        }
        match router.try_route_ordinary(job) {
            Ok(_) => {
                self.active[index] = Some(prepared);
                Some(OrdinaryRouterStep::Routed { slot, interface })
            }
            Err(failure) => {
                let reason = failure.reason();
                let job = failure.into_job();
                if matches!(reason, RouteError::QueueFull(_)) {
                    self.pending_jobs[index] = Some(job);
                    return Some(OrdinaryRouterStep::RouteBackpressured { slot, interface });
                }
                if self.active[index].is_some() || self.pending_completions[index].is_some() {
                    self.set_slot_fault(
                        index,
                        OrdinaryRouterFault::InternalInvariant,
                        OrdinaryRouterFaultResidue::Job(job),
                    );
                    return Some(OrdinaryRouterStep::Disabled(
                        OrdinaryRouterFault::InternalInvariant,
                    ));
                }
                self.active[index] = Some(prepared);
                self.pending_completions[index] = Some(
                    job.return_unpermitted()
                        .complete(self.config.route_rejected_code),
                );
                Some(OrdinaryRouterStep::RouteRejected { slot, reason })
            }
        }
    }

    fn cancel_one_pending_for_fault(&mut self) -> Option<OrdinaryRouterStep> {
        let index = self.next_job_index()?;
        let job = self.pending_jobs[index]
            .take()
            .expect("selected route slot must remain occupied");
        let slot = job.slot_id();
        let prepared = job.prepared();
        if self.active[index].is_some() || self.pending_completions[index].is_some() {
            self.set_slot_fault(
                index,
                OrdinaryRouterFault::InternalInvariant,
                OrdinaryRouterFaultResidue::Job(job),
            );
            return Some(OrdinaryRouterStep::Disabled(
                OrdinaryRouterFault::InternalInvariant,
            ));
        }
        self.active[index] = Some(prepared);
        self.pending_completions[index] = Some(job.cancel(self.config.fault_drain_code));
        Some(OrdinaryRouterStep::PendingJobCancelledForFault { slot })
    }

    fn retain_pending_actions_for_fault(&mut self) -> bool {
        let Some((actions, admission)) = self.pending_actions.take() else {
            return false;
        };
        let fault = self
            .fault
            .expect("pending input is retained for fault only after fault entry");
        self.set_input_fault(
            fault,
            OrdinaryRouterFaultResidue::Actions { actions, admission },
        );
        true
    }

    fn drain_batch_once(&mut self) -> Option<OrdinaryRouterStep> {
        let batch = self.batch.as_mut()?;
        if !self.batch_non_packet_taken {
            if self.non_packet_actions.is_some() {
                return Some(OrdinaryRouterStep::OutputBackpressured);
            }
            self.non_packet_actions = Some(batch.take_non_packet_actions());
            self.batch_non_packet_taken = true;
            if batch.remaining_packet_count() == 0 {
                self.batch = None;
            }
            return Some(OrdinaryRouterStep::NonPacketActionsReady);
        }

        let Some(job) = batch.take_next_packet() else {
            self.batch = None;
            return None;
        };
        let slot = job.slot_id();
        let index = usize::from(slot.get());
        if index >= PACKET_BUFFERS {
            self.set_input_fault(
                OrdinaryRouterFault::InternalInvariant,
                OrdinaryRouterFaultResidue::Job(job),
            );
            return Some(OrdinaryRouterStep::Disabled(
                OrdinaryRouterFault::InternalInvariant,
            ));
        }
        if self.pending_jobs[index].is_some()
            || self.active[index].is_some()
            || self.pending_completions[index].is_some()
        {
            self.set_slot_fault(
                index,
                OrdinaryRouterFault::InternalInvariant,
                OrdinaryRouterFaultResidue::Job(job),
            );
            return Some(OrdinaryRouterStep::Disabled(
                OrdinaryRouterFault::InternalInvariant,
            ));
        }
        self.pending_jobs[index] = Some(job);
        if batch.remaining_packet_count() == 0 {
            self.batch = None;
        }
        Some(OrdinaryRouterStep::PacketStaged { slot })
    }

    fn admit_once<M, const SLOTS: usize, const QUEUE_DEPTH: usize>(
        &mut self,
        router: &OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        now: MonotonicMillis,
    ) -> Option<OrdinaryRouterStep>
    where
        M: RawMutex + 'static,
    {
        let (actions, admission) = self.pending_actions.take()?;
        if now >= admission.deadline().instant() {
            let reason = OrdinaryActionAdmissionError::DeadlineExpired {
                now,
                deadline: admission.deadline(),
            };
            self.rejected_actions = Some(OrdinaryRouterRejectedActions {
                reason,
                actions,
                admission,
            });
            return Some(OrdinaryRouterStep::ActionsRejected { reason });
        }
        let enabled_interfaces = match router.eligible_interfaces() {
            Ok(interfaces) => interfaces,
            Err(reason) => {
                let fault = OrdinaryRouterFault::RegistryProfile(reason);
                self.set_input_fault(
                    fault,
                    OrdinaryRouterFaultResidue::Actions { actions, admission },
                );
                return Some(OrdinaryRouterStep::Disabled(fault));
            }
        };
        let request = OrdinaryActionAdmissionRequest {
            owner_now: now,
            deadline: admission.deadline(),
            enabled_interfaces,
        };
        match self.owner.admit_from_pool(actions, &mut self.pool, request) {
            Ok(batch) => {
                let packet_count = batch.packet_count();
                self.batch = Some(batch);
                self.batch_non_packet_taken = false;
                Some(OrdinaryRouterStep::ActionsAdmitted { packet_count })
            }
            Err(failure) => {
                let reason = failure.reason();
                let pressure = match reason {
                    OrdinaryActionAdmissionError::PoolFull { needed, available }
                    | OrdinaryActionAdmissionError::MissingBufferOwners {
                        needed,
                        supplied: available,
                    } if needed <= PACKET_BUFFERS => Some((needed, available)),
                    _ => None,
                };
                if let Some((needed, available)) = pressure {
                    self.pending_actions = Some((failure.into_actions(), admission));
                    return Some(OrdinaryRouterStep::AdmissionBackpressured { needed, available });
                }
                if matches!(
                    reason,
                    OrdinaryActionAdmissionError::NoEligibleInterface { .. }
                ) {
                    self.pending_actions = Some((failure.into_actions(), admission));
                    return Some(OrdinaryRouterStep::WaitingForInterfaces);
                }
                if matches!(
                    reason,
                    OrdinaryActionAdmissionError::PacketTooLarge { .. }
                        | OrdinaryActionAdmissionError::DeadlineExpired { .. }
                        | OrdinaryActionAdmissionError::InterfaceOutsideProfile { .. }
                ) {
                    self.rejected_actions = Some(OrdinaryRouterRejectedActions {
                        reason,
                        actions: failure.into_actions(),
                        admission,
                    });
                    return Some(OrdinaryRouterStep::ActionsRejected { reason });
                }
                let fault = OrdinaryRouterFault::Admission(reason);
                self.set_input_fault(
                    fault,
                    OrdinaryRouterFaultResidue::AdmissionFailure { failure, admission },
                );
                Some(OrdinaryRouterStep::Disabled(fault))
            }
        }
    }

    fn next_completion_index(&mut self) -> Option<usize> {
        for offset in 0..PACKET_BUFFERS {
            let index = (self.completion_cursor + offset) % PACKET_BUFFERS;
            if self.pending_completions[index].is_some() {
                self.completion_cursor = (index + 1) % PACKET_BUFFERS;
                return Some(index);
            }
        }
        None
    }

    fn next_job_index(&mut self) -> Option<usize> {
        for offset in 0..PACKET_BUFFERS {
            let index = (self.route_cursor + offset) % PACKET_BUFFERS;
            if self.pending_jobs[index].is_some() {
                self.route_cursor = (index + 1) % PACKET_BUFFERS;
                return Some(index);
            }
        }
        None
    }

    fn set_input_fault(&mut self, fault: OrdinaryRouterFault, residue: OrdinaryRouterFaultResidue) {
        self.fault.get_or_insert(fault);
        assert!(
            self.input_fault_residue.is_none(),
            "one input envelope cannot produce two retained fault residues"
        );
        self.input_fault_residue = Some(residue);
    }

    fn set_slot_fault(
        &mut self,
        index: usize,
        fault: OrdinaryRouterFault,
        residue: OrdinaryRouterFaultResidue,
    ) {
        self.fault.get_or_insert(fault);
        let slot = self
            .slot_fault_residues
            .get_mut(index)
            .expect("a coordinator-owned packet slot must fit its residue table");
        assert!(
            slot.is_none(),
            "one stable packet owner cannot produce two retained fault residues"
        );
        self.active[index] = None;
        *slot = Some(residue);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rand_core::{CryptoRng, RngCore};
    use reticulum_interface_router::{
        InterfaceConfigId, InterfaceCost, InterfaceDescriptor, InterfaceFabric,
        InterfaceProperties, InterfaceTxJob, LogicalMtu, OutboundCompletion,
    };
    use reticulum_node_core::{
        AnnounceEmissionTime, MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId,
        OrdinaryPacketBuffer, OrdinaryPermitResolution, PACKET_CAPACITY, TxAuthorizationCandidate,
        TxPermitRequirements, TxPermitReservation, TxPermitResourceId, TxPolicyDecision,
    };
    use reticulum_rns_rete::{
        EmbeddedNode, EmbeddedNodeConfig, InboundProofPolicy, InterfaceId as RnsInterfaceId,
    };
    use std::boxed::Box;

    use super::*;

    type TestNode = NodeCore<8, 4, 8, 8, 0>;

    #[derive(Default)]
    struct CounterRng(u8);

    impl RngCore for CounterRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for byte in destination {
                self.0 = self.0.wrapping_add(1);
                *byte = self.0;
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for CounterRng {}

    fn node(tag: u8) -> TestNode {
        TestNode::new(
            NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid"),
            "reticulum",
            &["ordinary-router"],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .expect("test node must construct")
    }

    fn coordinator<const BUFFERS: usize>(
        node: &mut TestNode,
    ) -> OrdinaryRouterCoordinator<BUFFERS> {
        let mut owner = node
            .take_ordinary_action_owner::<BUFFERS>()
            .expect("ordinary owner must be issued once");
        let mut pool = OrdinaryBufferPool::new();
        for _ in 0..BUFFERS {
            owner
                .register_and_park(&mut pool, Box::leak(Box::new(OrdinaryPacketBuffer::new())))
                .unwrap_or_else(|failure| {
                    panic!("ordinary buffer must register: {:?}", failure.reason())
                });
        }
        OrdinaryRouterCoordinator::try_new(
            owner,
            pool,
            OrdinaryRouterConfig::new(
                TxCompletionCode::new(0x401),
                TxCompletionCode::new(0x409),
                TxCompletionCode::new(0x40a),
            ),
        )
        .unwrap_or_else(|failure| panic!("coordinator must build: {:?}", failure.reason()))
    }

    fn properties(config: u32) -> InterfaceProperties {
        InterfaceProperties::new(
            LogicalMtu::try_new(500).expect("test MTU must be nonzero"),
            InterfaceConfigId::new(config),
            None,
            InterfaceCost::new(0),
        )
    }

    fn configure_router<const DEPTH: usize>() -> (
        OutboundRouter<NoopRawMutex, 2, DEPTH>,
        [reticulum_interface_router::InterfaceActorHandoff<NoopRawMutex, DEPTH>; 2],
        [InterfaceDescriptor; 2],
    ) {
        let fabric = Box::leak(Box::new(InterfaceFabric::new()));
        let (mut router, actors) = fabric.split();
        let first = router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                properties(2),
                true,
            )
            .expect("first interface must register");
        let second = router
            .register(
                actors[1].queue_id(),
                PacketInterfaceId::new(9),
                properties(9),
                true,
            )
            .expect("second interface must register");
        (router, actors, [first, second])
    }

    fn announce_actions(node: &mut TestNode, now: u64, rng: &mut CounterRng) -> NodeActions {
        node.queue_announce(None, AnnounceEmissionTime::new(now).unwrap(), rng)
            .expect("bounded announce queue must accept one item");
        let actions = node.flush_announces(MonotonicSeconds::new(now), rng);
        assert_eq!(actions.packets.len(), 1);
        actions
    }

    fn standalone_announce_actions(tag: u8, rng: &mut CounterRng) -> NodeActions {
        let mut source = node(tag);
        announce_actions(&mut source, 10, rng)
    }

    fn rns_node(tag: u8) -> EmbeddedNode<4, 4, 8, 2> {
        EmbeddedNode::new(
            reticulum_rns_rete::identity_from_private_key(&[tag; 64])
                .expect("RNS test identity must be valid"),
            "reticulum",
            &["ordinary-router-source-only"],
            EmbeddedNodeConfig::endpoint(),
        )
        .expect("RNS test node must construct")
    }

    fn source_only_actions(source_interface: u8, rng: &mut CounterRng) -> NodeActions {
        let mut sender = rns_node(71);
        let mut receiver = rns_node(72);
        receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
        receiver
            .queue_announce(None, 1, rng)
            .expect("receiver announce must queue");
        let announce = receiver.flush_announces(1, rng);
        sender.ingest(announce[0].bytes(), 1, RnsInterfaceId(2), rng);
        let mut encoded = [0u8; PACKET_CAPACITY];
        let prepared = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"ordinary router source-only proof",
                2,
                rng,
                &mut encoded,
            )
            .expect("test DATA packet must prepare");
        receiver
            .ingest(
                &encoded[..usize::from(prepared.packet_len())],
                2,
                RnsInterfaceId(source_interface),
                rng,
            )
            .actions
    }

    fn admission(_now: u64) -> OrdinaryRouterAdmission {
        OrdinaryRouterAdmission::new(TxLeaseDeadline::new(MonotonicMillis::new(100_000)))
    }

    fn route_one_announce<const BUFFERS: usize, const SLOTS: usize, const DEPTH: usize>(
        coordinator: &mut OrdinaryRouterCoordinator<BUFFERS>,
        router: &mut OutboundRouter<NoopRawMutex, SLOTS, DEPTH>,
        actions: NodeActions,
    ) {
        coordinator
            .try_offer_actions(actions, admission(10))
            .unwrap_or_else(|failure| panic!("actions must be accepted: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::ActionsAdmitted { packet_count: 1 }
        );
        assert_eq!(
            coordinator.step(router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::NonPacketActionsReady
        );
        let output = coordinator
            .take_non_packet_actions()
            .expect("non-packet output must remain explicit");
        assert!(output.events.is_empty());
        assert!(output.packets.is_empty());
        assert_eq!(output.unroutable_packets, 0);
        assert!(matches!(
            coordinator.step(router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::PacketStaged { .. }
        ));
        assert!(matches!(
            coordinator.step(router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::Routed {
                interface,
                ..
            } if interface == PacketInterfaceId::new(2)
        ));
    }

    struct Allow;

    impl TxAuthorizationPolicy for Allow {
        fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            TxPolicyDecision::Authorize(
                TxPermitReservation::try_new(
                    candidate.requirements.resource(),
                    candidate.requirements.required_units(),
                )
                .expect("test policy must cover valid nonzero requirements"),
            )
        }
    }

    #[test]
    fn all_fanout_survives_stale_router_lease_and_returns_exact_pool_owner() {
        let mut node = node(11);
        let mut coordinator = coordinator::<1>(&mut node);
        let (mut router, mut actors, descriptors) = configure_router::<1>();
        let mut rng = CounterRng::default();
        let actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, actions);

        let InterfaceTxJob::Ordinary(first) = actors[0]
            .try_receive_job()
            .expect("first interface must receive the All hop")
        else {
            panic!("ordinary route changed owner family")
        };
        let (first_ticket, first) = first.into_parts();
        let expected_slot = first.slot_id();
        let completion = first_ticket
            .complete(
                first
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(0x402)),
            )
            .unwrap_or_else(|_| panic!("ticket must bind the exact first-hop owner"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("first completion must fit"));

        router
            .reconfigure(descriptors[0].lease(), properties(22), true)
            .expect("test reconfiguration must supersede the first lease");
        let recovery = router
            .try_receive_completion()
            .expect_err("superseded completion must use explicit recovery")
            .into_node_recovery();
        let OutboundCompletion::Ordinary(first_completion) = recovery.into_outbound() else {
            panic!("stale ordinary completion changed owner family")
        };
        coordinator
            .try_accept_completion(first_completion)
            .unwrap_or_else(|failure| {
                panic!(
                    "stale-lease recovery must reconcile: {:?}",
                    failure.reason()
                )
            });
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Next {
                slot: expected_slot,
                interface: PacketInterfaceId::new(9),
            })
        );
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Routed {
                slot: expected_slot,
                interface: PacketInterfaceId::new(9),
            }
        );

        let InterfaceTxJob::Ordinary(second) = actors[1]
            .try_receive_job()
            .expect("second interface must receive serialized Next")
        else {
            panic!("ordinary Next changed owner family")
        };
        let (second_ticket, second) = second.into_parts();
        assert_eq!(second.slot_id(), expected_slot);
        let completion = second_ticket
            .complete(
                second
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(0x403)),
            )
            .unwrap_or_else(|_| panic!("ticket must bind the exact second-hop owner"));
        actors[1]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("second completion must fit"));
        let Some(OutboundCompletion::Ordinary(second_completion)) = router
            .try_receive_completion()
            .expect("current second lease must route completion")
        else {
            panic!("second ordinary completion was not returned")
        };
        coordinator
            .try_accept_completion(second_completion)
            .unwrap_or_else(|failure| {
                panic!("second completion must be accepted: {:?}", failure.reason())
            });
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
                slot: expected_slot,
            })
        );
        assert_eq!(coordinator.parked_count(), 1);
        assert_eq!(coordinator.active_count(), 0);
        assert_eq!(coordinator.capacities().active, 0);
        assert_eq!(coordinator.next_deadline(), None);
        assert_eq!(coordinator.fault(), None);
    }

    #[test]
    fn active_owner_backpressures_and_then_admits_the_exact_next_envelope() {
        let mut node = node(21);
        let mut coordinator = coordinator::<1>(&mut node);
        let (mut router, mut actors, _) = configure_router::<1>();
        let mut rng = CounterRng::default();
        let first_actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, first_actions);

        let second_actions = standalone_announce_actions(22, &mut rng);
        coordinator
            .try_offer_actions(second_actions, admission(20))
            .unwrap_or_else(|failure| {
                panic!("empty input boundary rejected: {:?}", failure.reason())
            });
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::AdmissionBackpressured {
                needed: 1,
                available: 0,
            }
        );

        let InterfaceTxJob::Ordinary(first) = actors[0]
            .try_receive_job()
            .expect("first active owner must remain in the actor queue")
        else {
            panic!("ordinary route changed owner family")
        };
        let (ticket, first) = first.into_parts();
        let completion = ticket
            .complete(first.cancel(TxCompletionCode::new(0x404)))
            .unwrap_or_else(|_| panic!("first ticket must remain exact"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("first completion must fit"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("current completion must route")
        else {
            panic!("ordinary completion changed owner family")
        };
        coordinator
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| panic!("completion must fit: {:?}", failure.reason()));
        assert!(matches!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { .. })
        ));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::ActionsAdmitted { packet_count: 1 }
        );
        assert_eq!(coordinator.fault(), None);
    }

    #[test]
    fn busy_offer_and_incomplete_build_return_every_input_unchanged() {
        let mut first_node = node(31);
        let mut coordinator = coordinator::<1>(&mut first_node);
        let mut rng = CounterRng::default();
        let first = announce_actions(&mut first_node, 10, &mut rng);
        coordinator
            .try_offer_actions(first, admission(10))
            .unwrap_or_else(|failure| panic!("first offer failed: {:?}", failure.reason()));
        let second = standalone_announce_actions(33, &mut rng);
        let pointer = second.packets.as_ptr();
        let failure = coordinator
            .try_offer_actions(second, admission(20))
            .expect_err("pending input must reject a second envelope");
        assert_eq!(
            failure.reason(),
            OrdinaryRouterOfferError::Busy(OrdinaryRouterBusyReason::PendingActions)
        );
        let (second, request) = failure.into_parts();
        assert_eq!(second.packets.as_ptr(), pointer);
        assert_eq!(request, admission(20));

        let mut other = node(32);
        let mut owner = other
            .take_ordinary_action_owner::<1>()
            .expect("ordinary owner must be issued");
        let buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
        owner
            .register_packet_buffer(buffer)
            .expect("buffer must register without parking");
        let failure = OrdinaryRouterCoordinator::try_new(
            owner,
            OrdinaryBufferPool::new(),
            OrdinaryRouterConfig::new(
                TxCompletionCode::new(1),
                TxCompletionCode::new(2),
                TxCompletionCode::new(3),
            ),
        )
        .err()
        .expect("missing parked owner must fail construction");
        assert_eq!(
            failure.reason(),
            OrdinaryRouterBuildError::ParkingIncomplete {
                expected: 1,
                parked: 0,
            }
        );
        let (_owner, pool, config) = failure.into_parts();
        assert_eq!(pool.parked_count(), 0);
        assert_eq!(config.route_rejected_code(), TxCompletionCode::new(1));
        assert_eq!(config.deadline_expired_code(), TxCompletionCode::new(2));
        assert_eq!(config.fault_drain_code(), TxCompletionCode::new(3));
    }

    #[test]
    fn full_actor_queue_retains_the_second_job_until_capacity_returns() {
        let mut node = node(41);
        let mut coordinator = coordinator::<2>(&mut node);
        let (mut router, mut actors, _) = configure_router::<1>();
        let mut rng = CounterRng::default();
        let mut actions = standalone_announce_actions(42, &mut rng);
        let mut second = standalone_announce_actions(43, &mut rng);
        actions.packets.append(&mut second.packets);
        assert_eq!(actions.packets.len(), 2);

        coordinator
            .try_offer_actions(actions, admission(10))
            .unwrap_or_else(|failure| panic!("two-packet batch failed: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::ActionsAdmitted { packet_count: 2 }
        );
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::NonPacketActionsReady
        );
        let _ = coordinator
            .take_non_packet_actions()
            .expect("non-packet output must remain explicit");
        assert!(matches!(
            coordinator.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::PacketStaged { .. }
        ));
        assert!(matches!(
            coordinator.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::Routed { .. }
        ));
        assert!(matches!(
            coordinator.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::PacketStaged { .. }
        ));
        let pressure = coordinator.step(&mut router, MonotonicMillis::new(10));
        assert!(matches!(
            pressure,
            OrdinaryRouterStep::RouteBackpressured {
                interface,
                ..
            } if interface == PacketInterfaceId::new(2)
        ));
        assert!(!pressure.progressed());

        let InterfaceTxJob::Ordinary(first) = actors[0]
            .try_receive_job()
            .expect("first queued owner must remain exact")
        else {
            panic!("first owner changed family")
        };
        assert!(matches!(
            coordinator.step(&mut router, MonotonicMillis::new(11)),
            OrdinaryRouterStep::Routed { .. }
        ));
        let InterfaceTxJob::Ordinary(second) = actors[0]
            .try_receive_job()
            .expect("retained second owner must route after capacity returns")
        else {
            panic!("second owner changed family")
        };
        assert_eq!(
            first.context().lease().interface(),
            PacketInterfaceId::new(2)
        );
        assert_eq!(
            second.context().lease().interface(),
            PacketInterfaceId::new(2)
        );
        let (first_ticket, first) = first.into_parts();
        let (second_ticket, second) = second.into_parts();
        let first_slot = first.slot_id();
        let second_slot = second.slot_id();
        assert_ne!(first_slot, second_slot);
        assert!(first.generation() < second.generation());

        let first_completion = first_ticket
            .complete(first.cancel(TxCompletionCode::new(0x405)))
            .unwrap_or_else(|_| panic!("first ticket must remain exact"));
        actors[0]
            .try_send_completion(first_completion)
            .unwrap_or_else(|_| panic!("first completion must fit"));
        let Some(OutboundCompletion::Ordinary(first_completion)) = router
            .try_receive_completion()
            .expect("first current completion must route")
        else {
            panic!("first completion changed family")
        };
        coordinator
            .try_accept_completion(first_completion)
            .unwrap_or_else(|failure| panic!("first completion must fit: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
                slot: first_slot,
            })
        );

        let second_completion = second_ticket
            .complete(second.cancel(TxCompletionCode::new(0x406)))
            .unwrap_or_else(|_| panic!("second ticket must remain exact"));
        actors[0]
            .try_send_completion(second_completion)
            .unwrap_or_else(|_| panic!("second completion must fit"));
        let Some(OutboundCompletion::Ordinary(second_completion)) = router
            .try_receive_completion()
            .expect("second current completion must route")
        else {
            panic!("second completion changed family")
        };
        coordinator
            .try_accept_completion(second_completion)
            .unwrap_or_else(|failure| panic!("second completion must fit: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
                slot: second_slot,
            })
        );
        assert_eq!(coordinator.parked_count(), 2);
        assert_eq!(coordinator.active_count(), 0);
    }

    #[test]
    fn full_lora_queue_does_not_starve_an_independent_other_interface_packet() {
        let (mut router, mut actors, _) = configure_router::<1>();
        let mut rng = CounterRng::default();

        let mut blocker_node = node(44);
        let mut blocker = coordinator::<1>(&mut blocker_node);
        let blocker_actions = announce_actions(&mut blocker_node, 10, &mut rng);
        route_one_announce(&mut blocker, &mut router, blocker_actions);
        assert_eq!(actors[0].pending_jobs(), 1);

        let mut primary_node = node(45);
        let mut primary = coordinator::<2>(&mut primary_node);
        let mut actions = standalone_announce_actions(46, &mut rng);
        let mut only_second = source_only_actions(9, &mut rng);
        assert_eq!(only_second.packets.len(), 1);
        actions.packets.append(&mut only_second.packets);
        primary
            .try_offer_actions(actions, admission(10))
            .unwrap_or_else(|failure| panic!("mixed batch failed: {:?}", failure.reason()));
        assert_eq!(
            primary.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::ActionsAdmitted { packet_count: 2 }
        );
        assert_eq!(
            primary.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::NonPacketActionsReady
        );
        let _ = primary
            .take_non_packet_actions()
            .expect("mixed batch output must remain explicit");
        assert!(matches!(
            primary.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::PacketStaged { .. }
        ));

        // The All packet selects interface 2, whose queue is full. The same
        // bounded step must continue and stage the independent Only(9) owner
        // instead of returning pressure and starving another transport.
        assert!(matches!(
            primary.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::PacketStaged { .. }
        ));
        let second_route = primary.step(&mut router, MonotonicMillis::new(10));
        assert!(matches!(
            second_route,
            OrdinaryRouterStep::Routed {
                interface,
                ..
            } if interface == PacketInterfaceId::new(9)
        ));
        let InterfaceTxJob::Ordinary(primary_second) = actors[1]
            .try_receive_job()
            .expect("independent interface must receive its owner")
        else {
            panic!("independent owner changed family")
        };
        let (ticket, primary_second) = primary_second.into_parts();
        let second_slot = primary_second.slot_id();
        let completion = ticket
            .complete(primary_second.cancel(TxCompletionCode::new(0x40e)))
            .unwrap_or_else(|_| panic!("independent ticket must remain exact"));
        actors[1]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("independent completion must fit"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("independent completion must route")
        else {
            panic!("independent completion changed family")
        };
        primary
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| {
                panic!("independent completion failed: {:?}", failure.reason())
            });
        assert_eq!(
            primary.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
                slot: second_slot,
            })
        );

        let InterfaceTxJob::Ordinary(blocking_job) = actors[0]
            .try_receive_job()
            .expect("blocker owner must remain exact")
        else {
            panic!("blocker owner changed family")
        };
        let (ticket, blocking_job) = blocking_job.into_parts();
        let blocking_slot = blocking_job.slot_id();
        let completion = ticket
            .complete(blocking_job.cancel(TxCompletionCode::new(0x40f)))
            .unwrap_or_else(|_| panic!("blocker ticket must remain exact"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("blocker completion must fit"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("blocker completion must route")
        else {
            panic!("blocker completion changed family")
        };
        blocker
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| panic!("blocker completion failed: {:?}", failure.reason()));
        assert_eq!(
            blocker.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
                slot: blocking_slot,
            })
        );

        let first_route = primary.step(&mut router, MonotonicMillis::new(20));
        assert!(matches!(
            first_route,
            OrdinaryRouterStep::Routed {
                interface,
                ..
            } if interface == PacketInterfaceId::new(2)
        ));
        let InterfaceTxJob::Ordinary(primary_first) = actors[0]
            .try_receive_job()
            .expect("previously pressured LoRa owner must route later")
        else {
            panic!("LoRa owner changed family")
        };
        let (ticket, primary_first) = primary_first.into_parts();
        let first_slot = primary_first.slot_id();
        let completion = ticket
            .complete(primary_first.cancel(TxCompletionCode::new(0x410)))
            .unwrap_or_else(|_| panic!("LoRa ticket must remain exact"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("LoRa completion must fit"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("LoRa completion must route")
        else {
            panic!("LoRa completion changed family")
        };
        primary
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| panic!("LoRa completion failed: {:?}", failure.reason()));
        assert_eq!(
            primary.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
                slot: first_slot,
            })
        );
        assert_eq!(primary.parked_count(), 2);
        assert_eq!(blocker.parked_count(), 1);
        assert_eq!(primary.fault(), None);
        assert_eq!(blocker.fault(), None);
    }

    #[test]
    fn oversized_atomic_envelope_is_returned_without_poisoning_active_work() {
        let mut node = node(51);
        let mut coordinator = coordinator::<1>(&mut node);
        let mut rng = CounterRng::default();
        let mut actions = standalone_announce_actions(52, &mut rng);
        let mut second = standalone_announce_actions(53, &mut rng);
        actions.packets.append(&mut second.packets);
        let pointer = actions.packets.as_ptr();
        let failure = coordinator
            .try_offer_actions(actions, admission(10))
            .expect_err("oversized atomic input must return unchanged");
        assert_eq!(
            failure.reason(),
            OrdinaryRouterOfferError::EnvelopeExceedsPool {
                packet_count: 2,
                limit: 1,
            }
        );
        let (actions, returned_admission) = failure.into_parts();
        assert_eq!(actions.packets.as_ptr(), pointer);
        assert_eq!(returned_admission, admission(10));
        assert_eq!(coordinator.fault(), None);
        assert_eq!(coordinator.parked_count(), 1);
    }

    #[test]
    fn permit_authorization_exposes_bytes_and_returns_through_the_real_ticket() {
        let mut node = node(54);
        let mut coordinator = coordinator::<1>(&mut node);
        let (mut router, mut actors, _) = configure_router::<1>();
        let mut rng = CounterRng::default();
        let actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, actions);

        let InterfaceTxJob::Ordinary(job) = actors[0]
            .try_receive_job()
            .expect("actor must receive the routed owner")
        else {
            panic!("ordinary owner changed family")
        };
        let (ticket, job) = job.into_parts();
        let slot = job.slot_id();
        let requirements = TxPermitRequirements::try_new(TxPermitResourceId::new([0x54; 16]), 1)
            .expect("test requirements must be nonzero");
        let (pending, request) = job.begin_permit(requirements);
        let reply = coordinator
            .authorize_tx(request, MonotonicMillis::new(20), &mut Allow)
            .unwrap_or_else(|failure| {
                panic!("permit request must authorize: {:?}", failure.reason())
            });
        let mut authorized = match pending
            .resolve(reply, MonotonicMillis::new(20))
            .unwrap_or_else(|_| panic!("permit reply must match its exact pending owner"))
        {
            OrdinaryPermitResolution::Authorized(authorized) => authorized,
            OrdinaryPermitResolution::Expired(_) => panic!("fresh grant unexpectedly expired"),
            OrdinaryPermitResolution::Unpermitted(_) => panic!("allow policy denied"),
        };
        {
            let frame = authorized
                .frame(MonotonicMillis::new(20))
                .expect("authorized owner must expose its complete native frame once");
            assert!(!frame.bytes().is_empty());
            assert_eq!(frame.prepared().slot_id(), slot);
        }
        let completion = ticket
            .complete(authorized.cancel(TxCompletionCode::new(0x40b)))
            .unwrap_or_else(|_| panic!("authorized completion must match its ticket"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("authorized completion must fit"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("authorized completion lease must remain current")
        else {
            panic!("authorized completion changed family")
        };
        coordinator
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| {
                panic!(
                    "authorized completion must reconcile: {:?}",
                    failure.reason()
                )
            });
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { slot })
        );
        assert_eq!(coordinator.parked_count(), 1);
        assert_eq!(coordinator.fault(), None);
    }

    #[test]
    fn locally_retained_job_cancels_at_its_fresh_deadline() {
        let mut node = node(55);
        let mut coordinator = coordinator::<1>(&mut node);
        let (mut router, mut actors, _) = configure_router::<1>();
        let mut rng = CounterRng::default();
        let actions = announce_actions(&mut node, 10, &mut rng);
        let deadline = TxLeaseDeadline::new(MonotonicMillis::new(15));
        coordinator
            .try_offer_actions(actions, OrdinaryRouterAdmission::new(deadline))
            .unwrap_or_else(|failure| panic!("actions failed: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::ActionsAdmitted { packet_count: 1 }
        );
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(10)),
            OrdinaryRouterStep::NonPacketActionsReady
        );
        let _ = coordinator
            .take_non_packet_actions()
            .expect("non-packet output must remain explicit");
        let OrdinaryRouterStep::PacketStaged { slot } =
            coordinator.step(&mut router, MonotonicMillis::new(10))
        else {
            panic!("admitted owner was not staged")
        };
        assert_eq!(coordinator.next_deadline(), Some(deadline));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(15)),
            OrdinaryRouterStep::PendingJobExpired { slot }
        );
        assert!(actors[0].try_receive_job().is_none());
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(15)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { slot })
        );
        assert_eq!(coordinator.parked_count(), 1);
        assert_eq!(coordinator.next_deadline(), None);
    }

    #[test]
    fn backpressured_envelope_uses_step_time_and_is_rejected_after_deadline() {
        let mut node = node(56);
        let mut coordinator = coordinator::<1>(&mut node);
        let (mut router, mut actors, _) = configure_router::<1>();
        let mut rng = CounterRng::default();
        let first_actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, first_actions);

        let second_actions = standalone_announce_actions(57, &mut rng);
        let second_pointer = second_actions.packets.as_ptr();
        let deadline = TxLeaseDeadline::new(MonotonicMillis::new(25));
        coordinator
            .try_offer_actions(second_actions, OrdinaryRouterAdmission::new(deadline))
            .unwrap_or_else(|failure| panic!("second actions failed: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::AdmissionBackpressured {
                needed: 1,
                available: 0,
            }
        );

        let InterfaceTxJob::Ordinary(first) = actors[0]
            .try_receive_job()
            .expect("active first owner must remain queued")
        else {
            panic!("first owner changed family")
        };
        let (ticket, first) = first.into_parts();
        let completion = ticket
            .complete(first.cancel(TxCompletionCode::new(0x40c)))
            .unwrap_or_else(|_| panic!("first ticket must remain exact"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("first completion must fit"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("first completion must route")
        else {
            panic!("first completion changed family")
        };
        coordinator
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| panic!("first completion failed: {:?}", failure.reason()));
        assert!(matches!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { .. })
        ));
        let reason = OrdinaryActionAdmissionError::DeadlineExpired {
            now: MonotonicMillis::new(30),
            deadline,
        };
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::ActionsRejected { reason }
        );
        let rejected = coordinator
            .take_rejected_actions()
            .expect("expired envelope must remain explicit output");
        assert_eq!(rejected.reason(), reason);
        let (actions, returned_admission) = rejected.into_parts();
        assert_eq!(actions.packets.as_ptr(), second_pointer);
        assert_eq!(returned_admission.deadline(), deadline);
        assert_eq!(coordinator.fault(), None);
        assert_eq!(coordinator.parked_count(), 1);
    }

    #[test]
    fn registry_fault_stops_intake_but_still_drains_an_active_actor_owner() {
        let mut node = node(58);
        let mut coordinator = coordinator::<2>(&mut node);
        let fabric = Box::leak(Box::new(InterfaceFabric::<NoopRawMutex, 3, 1>::new()));
        let (mut router, mut actors) = fabric.split();
        router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                properties(2),
                true,
            )
            .expect("first interface must register");
        router
            .register(
                actors[1].queue_id(),
                PacketInterfaceId::new(9),
                properties(9),
                true,
            )
            .expect("second interface must register");
        let mut rng = CounterRng::default();
        let first_actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, first_actions);

        router
            .register(
                actors[2].queue_id(),
                PacketInterfaceId::new(64),
                properties(64),
                true,
            )
            .expect("out-of-profile identity remains registrable");
        let second_actions = standalone_announce_actions(59, &mut rng);
        coordinator
            .try_offer_actions(second_actions, admission(20))
            .unwrap_or_else(|failure| panic!("second actions failed: {:?}", failure.reason()));
        let step = coordinator.step(&mut router, MonotonicMillis::new(20));
        assert!(matches!(
            step,
            OrdinaryRouterStep::Disabled(OrdinaryRouterFault::RegistryProfile(
                EligibleInterfaceSetError::InterfaceOutsideProfile(_)
            ))
        ));
        assert_eq!(coordinator.active_count(), 1);
        assert_eq!(coordinator.fault_residue_count(), 1);

        let InterfaceTxJob::Ordinary(first) = actors[0]
            .try_receive_job()
            .expect("active owner must remain in the actor queue")
        else {
            panic!("active owner changed family")
        };
        let (ticket, first) = first.into_parts();
        let slot = first.slot_id();
        let requirements = TxPermitRequirements::try_new(TxPermitResourceId::new([0x58; 16]), 1)
            .expect("fault-drain requirements must be nonzero");
        let (pending, request) = first.begin_permit(requirements);
        let reply = coordinator
            .authorize_tx(request, MonotonicMillis::new(25), &mut Allow)
            .unwrap_or_else(|failure| {
                panic!(
                    "fault-drain request must receive a denial: {:?}",
                    failure.reason()
                )
            });
        let first = match pending
            .resolve(reply, MonotonicMillis::new(25))
            .unwrap_or_else(|_| panic!("fault-drain denial must match its pending owner"))
        {
            OrdinaryPermitResolution::Unpermitted(owner) => owner,
            OrdinaryPermitResolution::Authorized(_) => {
                panic!("faulted coordinator exposed a fresh authorization")
            }
            OrdinaryPermitResolution::Expired(_) => panic!("fault-drain request expired"),
        };
        let completion = ticket
            .complete(first.cancel(TxCompletionCode::new(0x40d)))
            .unwrap_or_else(|_| panic!("active ticket must remain exact"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("active completion must fit"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("active completion must route despite registry profile fault")
        else {
            panic!("active completion changed family")
        };
        coordinator
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| {
                panic!(
                    "faulted coordinator must drain active owner: {:?}",
                    failure.reason()
                )
            });
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { slot })
        );
        assert_eq!(coordinator.active_count(), 0);
        assert_eq!(coordinator.parked_count(), 2);
        assert_eq!(coordinator.fault_residue_count(), 1);
        assert!(matches!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Disabled(OrdinaryRouterFault::RegistryProfile(_))
        ));
    }

    #[test]
    fn actor_quarantine_retains_an_already_offered_envelope_as_fault_residue() {
        let mut node = node(60);
        let mut coordinator = coordinator::<2>(&mut node);
        let (mut router, mut actors, _) = configure_router::<1>();
        let mut rng = CounterRng::default();
        let first_actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, first_actions);

        let second_actions = standalone_announce_actions(61, &mut rng);
        coordinator
            .try_offer_actions(second_actions, admission(20))
            .unwrap_or_else(|failure| panic!("second actions failed: {:?}", failure.reason()));
        assert!(coordinator.next_deadline().is_some());

        let InterfaceTxJob::Ordinary(first) = actors[0]
            .try_receive_job()
            .expect("active owner must remain in the actor queue")
        else {
            panic!("active owner changed family")
        };
        let (ticket, first) = first.into_parts();
        let slot = first.slot_id();
        let recovery_code = TxCompletionCode::new(0x410);
        let completion = ticket
            .complete(first.recovery_fault(recovery_code))
            .unwrap_or_else(|_| panic!("active ticket must remain exact"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("active completion must fit"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("active completion must route")
        else {
            panic!("active completion changed family")
        };
        coordinator
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| panic!("active completion failed: {:?}", failure.reason()));

        let fault =
            OrdinaryRouterFault::Quarantine(OrdinaryQuarantineReason::RecoveryFault(recovery_code));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Disabled(fault)
        );
        assert_eq!(coordinator.fault_residue_count(), 1);
        assert_eq!(
            coordinator.slot_fault_residue_kind(slot),
            Some(OrdinaryRouterFaultResidueKind::Quarantine)
        );
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::PendingActionsRetainedForFault
        );
        assert_eq!(coordinator.fault(), Some(fault));
        assert_eq!(coordinator.fault_residue_count(), 2);
        assert_eq!(coordinator.next_deadline(), None);
        assert_eq!(coordinator.active_count(), 0);
        assert_eq!(coordinator.parked_count(), 1);
    }

    #[test]
    fn crossed_completion_is_rejected_unchanged_and_both_owners_reconcile() {
        let mut first_node = node(61);
        let mut first_coordinator = coordinator::<1>(&mut first_node);
        let (mut first_router, mut first_actors, _) = configure_router::<1>();
        let mut first_rng = CounterRng::default();
        let first_actions = announce_actions(&mut first_node, 10, &mut first_rng);
        route_one_announce(&mut first_coordinator, &mut first_router, first_actions);

        let mut second_node = node(62);
        let mut second_coordinator = coordinator::<1>(&mut second_node);
        let (mut second_router, mut second_actors, _) = configure_router::<1>();
        let mut second_rng = CounterRng(100);
        let second_actions = announce_actions(&mut second_node, 10, &mut second_rng);
        route_one_announce(&mut second_coordinator, &mut second_router, second_actions);

        let InterfaceTxJob::Ordinary(first) = first_actors[0]
            .try_receive_job()
            .expect("first actor must hold one owner")
        else {
            panic!("first owner changed family")
        };
        let (first_ticket, first) = first.into_parts();
        let first_prepared = first.prepared();
        let first_completion = first_ticket
            .complete(first.cancel(TxCompletionCode::new(0x407)))
            .unwrap_or_else(|_| panic!("first ticket must remain exact"));
        first_actors[0]
            .try_send_completion(first_completion)
            .unwrap_or_else(|_| panic!("first completion must fit"));
        let Some(OutboundCompletion::Ordinary(first_completion)) = first_router
            .try_receive_completion()
            .expect("first completion must route")
        else {
            panic!("first completion changed family")
        };

        let InterfaceTxJob::Ordinary(second) = second_actors[0]
            .try_receive_job()
            .expect("second actor must hold one owner")
        else {
            panic!("second owner changed family")
        };
        let (second_ticket, second) = second.into_parts();
        let second_prepared = second.prepared();
        let second_completion = second_ticket
            .complete(second.cancel(TxCompletionCode::new(0x408)))
            .unwrap_or_else(|_| panic!("second ticket must remain exact"));
        second_actors[0]
            .try_send_completion(second_completion)
            .unwrap_or_else(|_| panic!("second completion must fit"));
        let Some(OutboundCompletion::Ordinary(second_completion)) = second_router
            .try_receive_completion()
            .expect("second completion must route")
        else {
            panic!("second completion changed family")
        };

        let crossed = second_coordinator
            .try_accept_completion(first_completion)
            .expect_err("foreign owner must be rejected unchanged");
        assert_eq!(
            crossed.reason(),
            OrdinaryCompletionAcceptError::OwnerMismatch {
                expected: second_prepared,
                supplied: first_prepared,
            }
        );
        first_coordinator
            .try_accept_completion(crossed.into_completion())
            .unwrap_or_else(|failure| {
                panic!("first owner must still reconcile: {:?}", failure.reason())
            });
        second_coordinator
            .try_accept_completion(second_completion)
            .unwrap_or_else(|failure| {
                panic!("second owner must still reconcile: {:?}", failure.reason())
            });
        assert!(matches!(
            first_coordinator.step(&mut first_router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { .. })
        ));
        assert!(matches!(
            second_coordinator.step(&mut second_router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { .. })
        ));
        assert_eq!(first_coordinator.parked_count(), 1);
        assert_eq!(second_coordinator.parked_count(), 1);
        assert_eq!(first_coordinator.fault(), None);
        assert_eq!(second_coordinator.fault(), None);
    }
}
