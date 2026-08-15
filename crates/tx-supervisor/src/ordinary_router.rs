//! Permanent ordinary-action ownership bridge into the interface router.

use core::{
    future::poll_fn,
    task::{Context, Poll},
};

use embassy_sync::blocking_mutex::raw::RawMutex;
use reticulum_interface_router::{EligibleInterfaceSetError, OutboundRouter, RouteError};
use reticulum_node_core::{
    ApplicationEventOfferError, ApplicationEventOfferReport, ApplicationEventOwner,
    MonotonicMillis, NodeActions, OrdinaryActionAdmissionError, OrdinaryActionAdmissionFailure,
    OrdinaryActionAdmissionRequest, OrdinaryActionBatch, OrdinaryActionCapacitySnapshot,
    OrdinaryActionOwner, OrdinaryAuthorizationErrorKind, OrdinaryAuthorizationFailure,
    OrdinaryBufferPool, OrdinaryBufferPoolError, OrdinaryCompletionDisposition,
    OrdinaryCompletionError, OrdinaryCompletionFailure, OrdinaryDispatchToken,
    OrdinaryPacketReturnParkFailure, OrdinaryPacketSlotId, OrdinaryPreparedPacket,
    OrdinaryQuarantineReason, OrdinaryTransmissionOutcome, OrdinaryTxCompletion, OrdinaryTxJob,
    OrdinaryTxPermitReply, OrdinaryTxPermitRequest, OrdinaryTxQuarantine, OutboundProtocolToken,
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

/// Transport-neutral observation retained from one concrete actor completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryRouterCompletionObservation {
    token: OrdinaryDispatchToken,
    interface: PacketInterfaceId,
    transmission: OrdinaryTransmissionOutcome,
}

impl OrdinaryRouterCompletionObservation {
    /// Exact packet-buffer generation returned by the concrete actor.
    pub const fn token(self) -> OrdinaryDispatchToken {
        self.token
    }

    /// Interface that owned this completed serialized hop.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Concrete actor's terminal transmission observation.
    pub const fn transmission(self) -> OrdinaryTransmissionOutcome {
        self.transmission
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
        /// Exact completed hop that caused this fan-out advance.
        completed: OrdinaryRouterCompletionObservation,
    },
    /// The exact packet buffer returned to the reusable pool.
    Returned {
        /// Stable owner slot.
        slot: OrdinaryPacketSlotId,
        /// Exact completed final hop returned to the reusable pool.
        completed: OrdinaryRouterCompletionObservation,
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
        /// Exact slot-generation correlation carried through the actor.
        token: OrdinaryDispatchToken,
        /// Selected interface.
        interface: PacketInterfaceId,
        /// Opaque LINKREQUEST or LRPROOF timing token, when present.
        protocol_token: Option<OutboundProtocolToken>,
        /// Whether this is the packet generation's first successful router
        /// dispatch. A false confirmation is terminal for this hop, but is
        /// expected after a previously confirmed serialized fan-out hop.
        first_dispatch: bool,
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
        /// Exact slot generation staged for later router or actor completion.
        token: OrdinaryDispatchToken,
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
    dispatch_accepted: [bool; PACKET_BUFFERS],
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
            dispatch_accepted: [false; PACKET_BUFFERS],
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
    #[allow(
        clippy::result_large_err,
        reason = "offer failure must preserve the exact action envelope and admission owner"
    )]
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

    pub(crate) fn drain_application_events(
        &mut self,
        owner: &mut ApplicationEventOwner<'_>,
    ) -> Result<Option<ApplicationEventOfferReport>, ApplicationEventOfferError> {
        let Some(actions) = self.non_packet_actions.take() else {
            return Ok(None);
        };
        match owner.try_offer_actions(actions) {
            Ok(report) => Ok(Some(report)),
            Err(failure) => {
                let reason = failure.reason();
                self.non_packet_actions = Some(failure.into_actions());
                Err(reason)
            }
        }
    }

    /// Take one recoverably rejected unchanged action envelope.
    pub fn take_rejected_actions(&mut self) -> Option<OrdinaryRouterRejectedActions> {
        self.rejected_actions.take()
    }

    /// Whether one retained ordinary envelope still contains application events.
    ///
    /// This spans every pre-consumer ownership phase so a product timeout cannot
    /// overtake a native terminal event merely because that event has not yet
    /// reached the caller-provided application-event slots.
    pub fn application_events_pending(&self) -> bool {
        let has_events = |actions: &NodeActions| !actions.events.is_empty();
        self.pending_actions
            .as_ref()
            .is_some_and(|(actions, _)| has_events(actions))
            || self
                .batch
                .as_ref()
                .is_some_and(|batch| has_events(batch.non_packet_actions()))
            || self.non_packet_actions.as_ref().is_some_and(has_events)
            || self
                .rejected_actions
                .as_ref()
                .is_some_and(|rejected| has_events(&rejected.actions))
            || self.input_fault_residue.as_ref().is_some_and(|residue| {
                match residue {
                    OrdinaryRouterFaultResidue::Actions { actions, .. } => has_events(actions),
                    // The admission failure owns an opaque action envelope.
                    // Conservatively preserve terminal-event precedence after
                    // an aggregate fault instead of guessing that it is empty.
                    OrdinaryRouterFaultResidue::AdmissionFailure { .. } => true,
                    OrdinaryRouterFaultResidue::CompletionFailure(_)
                    | OrdinaryRouterFaultResidue::Completion(_)
                    | OrdinaryRouterFaultResidue::Quarantine(_)
                    | OrdinaryRouterFaultResidue::ParkingReturn(_)
                    | OrdinaryRouterFaultResidue::Job(_) => false,
                }
            })
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
        let completed = OrdinaryRouterCompletionObservation {
            token: prepared.dispatch_token(),
            interface: prepared.interface(),
            transmission: completion.transmission_outcome(),
        };
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
                    OrdinaryRouterCompletionProgress::Next {
                        slot,
                        interface,
                        completed,
                    },
                ))
            }
            Ok(OrdinaryCompletionDisposition::Returned(returned)) => {
                self.active[index] = None;
                let slot = returned.prepared().slot_id();
                match self.pool.park_return(returned) {
                    Ok(_) => {
                        self.dispatch_accepted[index] = false;
                        Some(OrdinaryRouterStep::Completion(
                            OrdinaryRouterCompletionProgress::Returned { slot, completed },
                        ))
                    }
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
        let token = job.prepared().dispatch_token();
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
                let first_dispatch = !self.dispatch_accepted[index];
                self.dispatch_accepted[index] = true;
                Some(OrdinaryRouterStep::Routed {
                    slot,
                    token,
                    interface,
                    protocol_token: prepared.protocol_token(),
                    first_dispatch,
                })
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
        let token = job.prepared().dispatch_token();
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
            || self.dispatch_accepted[index]
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
        Some(OrdinaryRouterStep::PacketStaged { slot, token })
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
mod tests;
