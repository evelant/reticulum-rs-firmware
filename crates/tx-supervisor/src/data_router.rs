//! Permanent destination-DATA ownership bridge into the interface router.

use core::{
    future::poll_fn,
    mem,
    task::{Context, Poll},
};

use embassy_sync::blocking_mutex::raw::RawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_interface_router::{EligibleInterfaceSetError, OutboundRouter, RouteError};
use reticulum_node_core::{
    AvailableBufferError, DestinationHash, LinkHandle, MonotonicMillis, MonotonicSeconds, NodeCore,
    PacketInterfaceId, PacketSlotId, PrepareDataRequest, PreparedPacket, RoutedTxJob, SubmitError,
    TxAuthorizationCandidate, TxAuthorizationErrorKind, TxAuthorizationFailure,
    TxAuthorizationPolicy, TxCompletion, TxCompletionCode, TxCompletionDisposition,
    TxCompletionErrorKind, TxCompletionFailure, TxLeaseDeadline, TxMaintenanceReport, TxOwnerScope,
    TxPacketBuffer, TxPermitReply, TxPermitRequest, TxPolicyDecision, TxPolicyDenial, TxQuarantine,
    TxRecoveryObservation, TxRecoveryRecord,
};

/// Product policy for DATA hops rejected before they enter an actor queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataRouterConfig {
    route_rejected_code: TxCompletionCode,
    deadline_expired_code: TxCompletionCode,
    fault_drain_code: TxCompletionCode,
}

impl DataRouterConfig {
    /// Construct one DATA coordinator completion-code policy.
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

    /// Completion code for a definitely unpermitted registry rejection.
    pub const fn route_rejected_code(self) -> TxCompletionCode {
        self.route_rejected_code
    }

    /// Completion code for a locally retained job whose deadline expires.
    pub const fn deadline_expired_code(self) -> TxCompletionCode {
        self.deadline_expired_code
    }

    /// Completion code used to drain locally retained jobs after a fault.
    pub const fn fault_drain_code(self) -> TxCompletionCode {
        self.fault_drain_code
    }
}

/// Transport-neutral inputs for one destination-DATA preparation.
///
/// The authoritative enabled-interface snapshot is deliberately absent. The
/// coordinator derives it from the shared [`OutboundRouter`] immediately
/// before calling node-core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataRouterPrepareRequest<'a> {
    /// Complete destination hash for the encrypted DATA packet.
    pub destination: DestinationHash,
    /// Plaintext to encrypt into one base-MTU DATA packet.
    pub plaintext: &'a [u8],
    /// Current Reticulum protocol clock.
    pub rns_now: MonotonicSeconds,
    /// Deadline for returning or retaining the exact packet owner.
    pub deadline: TxLeaseDeadline,
}

#[derive(Clone, Copy)]
enum DataRouterPreparationMode {
    Generic,
    RehydratedOpportunisticLxmf,
    RehydratedDirectLxmf(LinkHandle),
}

/// Scalar identity of one prepared DATA hop retained or routed by the
/// coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataPreparedHop {
    prepared: PreparedPacket,
    interface: PacketInterfaceId,
}

impl DataPreparedHop {
    fn from_job(job: &RoutedTxJob<'_>) -> Self {
        Self {
            prepared: job.prepared(),
            interface: job.interface(),
        }
    }

    fn from_completion(completion: &TxCompletion<'_>) -> Self {
        Self {
            prepared: completion.prepared(),
            interface: completion.interface(),
        }
    }

    /// Complete node-core packet metadata for this DATA attempt.
    pub const fn prepared(self) -> PreparedPacket {
        self.prepared
    }

    /// Stable external packet-buffer slot.
    pub const fn slot_id(self) -> PacketSlotId {
        self.prepared.slot_id()
    }

    /// Selected packet interface for this serialized hop.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Exact packet-owner deadline.
    pub const fn deadline(self) -> TxLeaseDeadline {
        self.prepared.deadline()
    }
}

/// Why permanent DATA coordinator construction failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataRouterBuildError {
    /// At least one external DATA buffer is required.
    ZeroPacketBuffers,
    /// The node has fewer registered buffers than its fixed DATA capacity.
    RegistrationIncomplete {
        /// Configured DATA buffer count.
        expected: usize,
        /// Buffers registered with node-core.
        registered: usize,
    },
    /// Node-core already has active, reserved, or recovery-bound buffers.
    ActiveDispatches {
        /// Active or reserved dispatch records observed at construction.
        active: usize,
    },
    /// One supplied buffer is not reusable by this node incarnation.
    InvalidBuffer {
        /// Position in the supplied construction array.
        input_index: usize,
        /// Node-core validation reason.
        reason: AvailableBufferError,
    },
    /// One validated buffer names a slot outside the fixed coordinator table.
    SlotOutsidePool {
        /// Position in the supplied construction array.
        input_index: usize,
        /// Stable slot reported by node-core.
        slot: PacketSlotId,
        /// Fixed coordinator slot count.
        limit: usize,
    },
    /// Two supplied owners unexpectedly name the same stable slot.
    DuplicateSlot(PacketSlotId),
}

/// Failed construction retaining every supplied static DATA-buffer owner.
#[must_use = "failed construction retains every DATA packet owner"]
pub struct DataRouterBuildFailure<const PACKET_BUFFERS: usize> {
    reason: DataRouterBuildError,
    buffers: [Option<&'static mut TxPacketBuffer>; PACKET_BUFFERS],
    config: DataRouterConfig,
}

impl<const PACKET_BUFFERS: usize> DataRouterBuildFailure<PACKET_BUFFERS> {
    /// Scalar reason permanent construction did not complete.
    pub const fn reason(&self) -> DataRouterBuildError {
        self.reason
    }

    /// Recover all supplied buffers in their original input positions and the
    /// unchanged configuration.
    pub fn into_parts(
        self,
    ) -> (
        [Option<&'static mut TxPacketBuffer>; PACKET_BUFFERS],
        DataRouterConfig,
    ) {
        (self.buffers, self.config)
    }
}

/// Permanent fail-closed reason retained by the DATA coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataRouterFault {
    /// The registry cannot be represented by node-core's compact route set.
    RegistryProfile(EligibleInterfaceSetError),
    /// A supposedly reusable packet buffer failed node-core validation.
    AvailableBuffer(AvailableBufferError),
    /// An owner named a slot outside the fixed DATA pool.
    SlotOutsidePool(PacketSlotId),
    /// An exact owner attempted to enter a slot that was not empty.
    SlotOccupied(PacketSlotId),
    /// A returned owner no longer names the stable slot that released it.
    OwnerSlotMismatch {
        /// Table index from which the exact owner originated.
        expected_index: usize,
        /// Stable slot reported by the returned owner.
        actual: PacketSlotId,
    },
    /// A recovered or quarantined owner disagreed with its observation.
    RecoveryObservationMismatch,
    /// Node-core rejected an owning completion without consuming it.
    Completion(TxCompletionErrorKind),
    /// Private coordinator ownership tables contradicted one another.
    InternalInvariant,
}

/// Kind of exact non-copy owner retained behind a permanent DATA fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataRouterFaultResidueKind {
    /// Reusable DATA buffer.
    AvailableBuffer,
    /// Recovered buffer and its exact observation.
    RecoveredBuffer,
    /// Fail-closed node-core quarantine.
    Quarantine,
    /// Completion-correlation failure retaining the owning completion.
    CompletionFailure,
    /// Completion rejected by a private coordinator invariant.
    Completion,
    /// Routed DATA job.
    Job,
}

enum DataRouterFaultResidue {
    Available(&'static mut TxPacketBuffer),
    Recovered {
        buffer: &'static mut TxPacketBuffer,
        observation: TxRecoveryObservation,
    },
    Quarantine(TxQuarantine<'static>),
    CompletionFailure(TxCompletionFailure<'static>),
    Completion(TxCompletion<'static>),
    Job(RoutedTxJob<'static>),
}

impl DataRouterFaultResidue {
    fn kind(&self) -> DataRouterFaultResidueKind {
        match self {
            Self::Available(buffer) => {
                let _ = buffer.slot_id();
                DataRouterFaultResidueKind::AvailableBuffer
            }
            Self::Recovered {
                buffer,
                observation,
            } => {
                let _ = (buffer.slot_id(), observation);
                DataRouterFaultResidueKind::RecoveredBuffer
            }
            Self::Quarantine(quarantine) => {
                let _ = quarantine.slot_id();
                DataRouterFaultResidueKind::Quarantine
            }
            Self::CompletionFailure(failure) => {
                let _ = failure.reason();
                DataRouterFaultResidueKind::CompletionFailure
            }
            Self::Completion(completion) => {
                let _ = completion.prepared();
                DataRouterFaultResidueKind::Completion
            }
            Self::Job(job) => {
                let _ = job.prepared();
                DataRouterFaultResidueKind::Job
            }
        }
    }
}

enum ParkedDataOwner {
    Empty,
    Available(&'static mut TxPacketBuffer),
    Recovered {
        buffer: &'static mut TxPacketBuffer,
        observation: TxRecoveryObservation,
    },
    Quarantined(TxQuarantine<'static>),
}

/// Kind of exact DATA owner parked in one stable slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataRouterParkedKind {
    /// Reusable buffer available for another DATA preparation.
    Available,
    /// Recovered buffer awaiting exact observation acknowledgement.
    Recovered,
    /// Buffer retained in fail-closed recovery quarantine.
    Quarantined,
}

/// Copy-only occupancy counts for the DATA coordinator's fixed owner table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataRouterParkedCounts {
    available: usize,
    recovered: usize,
    quarantined: usize,
}

impl DataRouterParkedCounts {
    /// Reusable owner count.
    pub const fn available(self) -> usize {
        self.available
    }

    /// Recovered owners awaiting acknowledgement.
    pub const fn recovered(self) -> usize {
        self.recovered
    }

    /// Quarantined owner count.
    pub const fn quarantined(self) -> usize {
        self.quarantined
    }

    /// Total parked owner count.
    pub const fn occupied(self) -> usize {
        self.available + self.recovered + self.quarantined
    }
}

/// Result of one synchronous DATA preparation attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "DATA preparation ownership and rejection must be observed"]
pub enum DataRouterPrepareResult {
    /// One exact prepared owner is retained for transport-neutral routing.
    Prepared(DataPreparedHop),
    /// Node-core rejected preparation and the same buffer was restored.
    Rejected {
        /// Stable restored buffer slot.
        slot: PacketSlotId,
        /// Typed preparation rejection.
        reason: SubmitError,
    },
    /// Preparation failed closed and its quarantine was parked.
    RejectedQuarantined {
        /// Typed preparation rejection.
        reason: SubmitError,
        /// Exact recovery observation for the parked quarantine.
        observation: TxRecoveryObservation,
    },
    /// No reusable DATA buffer is currently parked.
    NoAvailable,
    /// The supplied node is not the incarnation bound at construction.
    OwnerMismatch,
    /// The coordinator is permanently disabled.
    Disabled(DataRouterFault),
}

/// Why an actor completion was not accepted by this DATA coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataCompletionAcceptError {
    /// The completion names a slot outside this product configuration.
    SlotOutsidePool {
        /// Slot carried by the completion owner.
        slot: PacketSlotId,
        /// Fixed coordinator slot count.
        limit: usize,
    },
    /// No routed owner is awaiting this slot's completion.
    NoActiveOwner(PacketSlotId),
    /// Attempt metadata or selected interface does not match the routed owner.
    OwnerMismatch {
        /// Stable slot shared by the expected and supplied bindings.
        slot: PacketSlotId,
        /// Interface expected by the coordinator.
        expected_interface: PacketInterfaceId,
        /// Interface carried by the supplied completion.
        supplied_interface: PacketInterfaceId,
    },
    /// One earlier completion is already retained for reconciliation.
    CompletionPending(PacketSlotId),
    /// The coordinator is disabled and this is not one of its active owners.
    Disabled(DataRouterFault),
}

/// Rejected actor completion retaining the exact DATA packet owner.
#[must_use = "a rejected DATA completion must remain exactly owned"]
pub struct DataCompletionAcceptFailure {
    reason: DataCompletionAcceptError,
    completion: TxCompletion<'static>,
}

impl DataCompletionAcceptFailure {
    /// Scalar reason the completion was not accepted.
    pub const fn reason(&self) -> DataCompletionAcceptError {
        self.reason
    }

    /// Recover the unchanged owning completion.
    pub fn into_completion(self) -> TxCompletion<'static> {
        self.completion
    }
}

/// Successful node-core completion reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataRouterCompletionProgress {
    /// The same DATA attempt is ready for its next serialized interface.
    Next(DataPreparedHop),
    /// The exact buffer returned reusable.
    Available(PacketSlotId),
    /// A late exact owner returned reusable but awaits durable recovery ack.
    Recovered(TxRecoveryObservation),
    /// The exact owner remains fail-closed in per-buffer quarantine.
    Quarantined(TxRecoveryObservation),
}

/// Result of one bounded synchronous DATA coordinator transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "DATA coordinator progress, pressure, and faults must be observed"]
pub enum DataRouterStep {
    /// The supplied node does not match this coordinator's bound owner.
    OwnerMismatch,
    /// Permanent fault; every exact residue remains retained.
    Disabled(DataRouterFault),
    /// One actor completion advanced or parked its exact owner.
    Completion(DataRouterCompletionProgress),
    /// One DATA owner entered its selected interface actor queue.
    Routed(DataPreparedHop),
    /// The selected actor queue was full; the exact owner remains retained.
    RouteBackpressured(DataPreparedHop),
    /// Registry state rejected a hop before actor ownership transfer.
    RouteRejected {
        /// Exact retained DATA hop.
        hop: DataPreparedHop,
        /// Transport-neutral registry reason.
        reason: RouteError,
    },
    /// A locally retained job reached its deadline before actor acceptance.
    PendingJobExpired(DataPreparedHop),
    /// A locally retained owner is being cancelled during fault drain.
    PendingJobCancelledForFault(DataPreparedHop),
    /// No synchronous work is currently observable.
    Idle,
}

impl DataRouterStep {
    /// Whether this transition completed useful synchronous ownership work.
    pub const fn progressed(self) -> bool {
        matches!(
            self,
            Self::Completion(_)
                | Self::Routed(_)
                | Self::RouteRejected { .. }
                | Self::PendingJobExpired(_)
                | Self::PendingJobCancelledForFault(_)
        )
    }
}

/// Exact recovered-observation acknowledgement failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataRecoveryAckError {
    /// Observation names a slot outside this fixed DATA table.
    SlotOutsidePool,
    /// The addressed slot does not contain a recovered owner.
    NotRecovered,
    /// The current recovered observation differs from the supplied value.
    ObservationMismatch,
}

/// Why DATA permit authorization did not produce a reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPermitAuthorizationError {
    /// The supplied node is not the incarnation bound at construction.
    OwnerMismatch,
    /// Node-core rejected the request's owner, generation, interface, or phase.
    Owner(TxAuthorizationErrorKind),
}

/// Failed DATA authorization retaining the exact non-copy request.
#[must_use = "a failed DATA permit request must remain explicitly owned"]
pub struct DataPermitAuthorizationFailure {
    reason: DataPermitAuthorizationError,
    request: TxPermitRequest,
}

impl DataPermitAuthorizationFailure {
    /// Scalar reason authorization did not produce a reply.
    pub const fn reason(&self) -> DataPermitAuthorizationError {
        self.reason
    }

    /// Recover the unchanged permit request.
    pub fn into_request(self) -> TxPermitRequest {
        self.request
    }
}

/// Node-owner mismatch while running DATA lease maintenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataRouterOwnerMismatch;

struct FaultDrainPolicy;

impl TxAuthorizationPolicy for FaultDrainPolicy {
    fn authorize(&mut self, _candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        TxPolicyDecision::Deny(TxPolicyDenial::ResourceUnavailable)
    }
}

/// Permanent fixed-capacity DATA owner and transport-neutral router bridge.
///
/// The coordinator owns static packet buffers, locally pending jobs, and
/// actor-returned completions. It binds but does not own [`NodeCore`]; the
/// future permanent aggregate remains the sole orchestrator allowed to borrow
/// both this coordinator and the node owner.
#[must_use = "dropping the DATA coordinator abandons exact packet owners"]
pub struct DataRouterCoordinator<const PACKET_BUFFERS: usize> {
    owner_scope: TxOwnerScope,
    config: DataRouterConfig,
    parked: [ParkedDataOwner; PACKET_BUFFERS],
    pending_jobs: [Option<RoutedTxJob<'static>>; PACKET_BUFFERS],
    active: [Option<DataPreparedHop>; PACKET_BUFFERS],
    pending_completions: [Option<TxCompletion<'static>>; PACKET_BUFFERS],
    route_cursor: usize,
    completion_cursor: usize,
    fault: Option<DataRouterFault>,
    general_fault_residue: Option<DataRouterFaultResidue>,
    slot_fault_residues: [Option<DataRouterFaultResidue>; PACKET_BUFFERS],
}

impl<const PACKET_BUFFERS: usize> DataRouterCoordinator<PACKET_BUFFERS> {
    /// Opaque node-incarnation binding captured at construction.
    pub const fn owner_scope(&self) -> TxOwnerScope {
        self.owner_scope
    }

    /// Consume one complete array of registered, currently reusable static
    /// packet buffers into stable per-slot ownership.
    #[allow(clippy::result_large_err)]
    pub fn try_new<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        owner: &NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        buffers: [&'static mut TxPacketBuffer; PACKET_BUFFERS],
        config: DataRouterConfig,
    ) -> Result<Self, DataRouterBuildFailure<PACKET_BUFFERS>> {
        let mut buffers = buffers.map(Some);
        let capacities = owner.capacities();
        let early_reason = if PACKET_BUFFERS == 0 {
            Some(DataRouterBuildError::ZeroPacketBuffers)
        } else if capacities.packet_buffers_registered != PACKET_BUFFERS {
            Some(DataRouterBuildError::RegistrationIncomplete {
                expected: PACKET_BUFFERS,
                registered: capacities.packet_buffers_registered,
            })
        } else if capacities.dispatches_used != 0 {
            Some(DataRouterBuildError::ActiveDispatches {
                active: capacities.dispatches_used,
            })
        } else {
            None
        };
        if let Some(reason) = early_reason {
            return Err(DataRouterBuildFailure {
                reason,
                buffers,
                config,
            });
        }

        let mut stable_slots = [None; PACKET_BUFFERS];
        let mut occupied = [false; PACKET_BUFFERS];
        for input_index in 0..PACKET_BUFFERS {
            let buffer = buffers[input_index]
                .as_deref()
                .expect("construction input remains present during validation");
            let slot = match owner.validate_available_buffer(buffer) {
                Ok(slot) => slot,
                Err(reason) => {
                    return Err(DataRouterBuildFailure {
                        reason: DataRouterBuildError::InvalidBuffer {
                            input_index,
                            reason,
                        },
                        buffers,
                        config,
                    });
                }
            };
            let index = usize::from(slot.get());
            if index >= PACKET_BUFFERS {
                return Err(DataRouterBuildFailure {
                    reason: DataRouterBuildError::SlotOutsidePool {
                        input_index,
                        slot,
                        limit: PACKET_BUFFERS,
                    },
                    buffers,
                    config,
                });
            }
            if occupied[index] {
                return Err(DataRouterBuildFailure {
                    reason: DataRouterBuildError::DuplicateSlot(slot),
                    buffers,
                    config,
                });
            }
            occupied[index] = true;
            stable_slots[input_index] = Some(index);
        }

        let mut parked = core::array::from_fn(|_| ParkedDataOwner::Empty);
        for input_index in 0..PACKET_BUFFERS {
            let index = stable_slots[input_index]
                .expect("every validated construction input has one stable slot");
            let buffer = buffers[input_index]
                .take()
                .expect("validated construction input must remain exactly owned");
            parked[index] = ParkedDataOwner::Available(buffer);
        }

        Ok(Self {
            owner_scope: owner.tx_owner_scope(),
            config,
            parked,
            pending_jobs: core::array::from_fn(|_| None),
            active: core::array::from_fn(|_| None),
            pending_completions: core::array::from_fn(|_| None),
            route_cursor: 0,
            completion_cursor: 0,
            fault: None,
            general_fault_residue: None,
            slot_fault_residues: core::array::from_fn(|_| None),
        })
    }

    /// Prepare one destination-DATA packet into the lowest reusable parked
    /// owner and retain the resulting job for the shared interface router.
    ///
    /// Registry eligibility is sampled here, not supplied by the caller. A
    /// semantic node-core rejection restores or quarantines the exact buffer
    /// in its stable slot before this method returns.
    pub fn try_prepare_data<
        R: RngCore + CryptoRng,
        M: RawMutex + 'static,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const SLOTS: usize,
        const QUEUE_DEPTH: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        router: &OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        request: DataRouterPrepareRequest<'_>,
        now: MonotonicMillis,
        rng: &mut R,
    ) -> DataRouterPrepareResult {
        self.try_prepare_data_with(
            owner,
            router,
            request,
            now,
            DataRouterPreparationMode::Generic,
            rng,
        )
    }

    /// Prepare one exact durable LXMF wire message through node-core's
    /// dedicated opportunistic Header-1 path.
    ///
    /// `request.plaintext` is the complete signed wire message, including its
    /// destination prefix. Owner parking and interface routing remain
    /// identical to ordinary DATA preparation.
    pub fn try_prepare_rehydrated_opportunistic_lxmf<
        R: RngCore + CryptoRng,
        M: RawMutex + 'static,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const SLOTS: usize,
        const QUEUE_DEPTH: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        router: &OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        request: DataRouterPrepareRequest<'_>,
        now: MonotonicMillis,
        rng: &mut R,
    ) -> DataRouterPrepareResult {
        self.try_prepare_data_with(
            owner,
            router,
            request,
            now,
            DataRouterPreparationMode::RehydratedOpportunisticLxmf,
            rng,
        )
    }

    /// Prepare one exact durable LXMF wire message as application DATA on an
    /// already active, product-retained Link.
    ///
    /// `request.plaintext` is the complete signed wire message, including its
    /// destination prefix. Node-core revalidates that prefix against the
    /// Link-bound destination and derives the exact bound interface. The
    /// resulting packet uses the same parked owner, authoritative interface
    /// router, completion ledger, and recovery path as every other DATA job.
    pub fn try_prepare_rehydrated_direct_lxmf<
        R: RngCore + CryptoRng,
        M: RawMutex + 'static,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const SLOTS: usize,
        const QUEUE_DEPTH: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        router: &OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        link: LinkHandle,
        request: DataRouterPrepareRequest<'_>,
        now: MonotonicMillis,
        rng: &mut R,
    ) -> DataRouterPrepareResult {
        self.try_prepare_data_with(
            owner,
            router,
            request,
            now,
            DataRouterPreparationMode::RehydratedDirectLxmf(link),
            rng,
        )
    }

    fn try_prepare_data_with<
        R: RngCore + CryptoRng,
        M: RawMutex + 'static,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const SLOTS: usize,
        const QUEUE_DEPTH: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        router: &OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        request: DataRouterPrepareRequest<'_>,
        now: MonotonicMillis,
        mode: DataRouterPreparationMode,
        rng: &mut R,
    ) -> DataRouterPrepareResult {
        if owner.tx_owner_scope() != self.owner_scope {
            return DataRouterPrepareResult::OwnerMismatch;
        }
        if let Some(fault) = self.fault {
            return DataRouterPrepareResult::Disabled(fault);
        }
        let enabled_interfaces = match router.eligible_interfaces() {
            Ok(interfaces) => interfaces,
            Err(reason) => {
                let fault = DataRouterFault::RegistryProfile(reason);
                self.fault = Some(fault);
                return DataRouterPrepareResult::Disabled(fault);
            }
        };
        let Some(index) = self
            .parked
            .iter()
            .position(|entry| matches!(entry, ParkedDataOwner::Available(_)))
        else {
            return DataRouterPrepareResult::NoAvailable;
        };
        if self.pending_jobs[index].is_some()
            || self.active[index].is_some()
            || self.pending_completions[index].is_some()
            || self.slot_fault_residues[index].is_some()
        {
            let fault = DataRouterFault::InternalInvariant;
            self.fault = Some(fault);
            return DataRouterPrepareResult::Disabled(fault);
        }

        let parked = mem::replace(&mut self.parked[index], ParkedDataOwner::Empty);
        let ParkedDataOwner::Available(buffer) = parked else {
            unreachable!("selected reusable DATA owner changed during one synchronous call")
        };
        let expected_slot = match owner.validate_available_buffer(buffer) {
            Ok(slot) if usize::from(slot.get()) == index => slot,
            Ok(actual) => {
                let fault = DataRouterFault::OwnerSlotMismatch {
                    expected_index: index,
                    actual,
                };
                self.set_slot_fault(index, fault, DataRouterFaultResidue::Available(buffer));
                return DataRouterPrepareResult::Disabled(fault);
            }
            Err(reason) => {
                let fault = DataRouterFault::AvailableBuffer(reason);
                self.set_slot_fault(index, fault, DataRouterFaultResidue::Available(buffer));
                return DataRouterPrepareResult::Disabled(fault);
            }
        };

        let node_request = PrepareDataRequest {
            destination: request.destination,
            plaintext: request.plaintext,
            rns_now: request.rns_now,
            owner_now: now,
            deadline: request.deadline,
            enabled_interfaces,
        };
        let prepared = match mode {
            DataRouterPreparationMode::Generic => {
                owner.prepare_data_into_slot(buffer, node_request, rng)
            }
            DataRouterPreparationMode::RehydratedOpportunisticLxmf => {
                owner.prepare_rehydrated_opportunistic_lxmf_into_slot(buffer, node_request, rng)
            }
            DataRouterPreparationMode::RehydratedDirectLxmf(link) => {
                owner.prepare_rehydrated_direct_lxmf_into_slot(buffer, link, node_request, rng)
            }
        };
        match prepared {
            Ok(job) => {
                let hop = DataPreparedHop::from_job(&job);
                let actual = hop.slot_id();
                if actual != expected_slot || usize::from(actual.get()) != index {
                    let fault = DataRouterFault::OwnerSlotMismatch {
                        expected_index: index,
                        actual,
                    };
                    self.set_slot_fault(index, fault, DataRouterFaultResidue::Job(job));
                    return DataRouterPrepareResult::Disabled(fault);
                }
                self.pending_jobs[index] = Some(job);
                DataRouterPrepareResult::Prepared(hop)
            }
            Err(failure) => {
                let reason = failure.reason();
                match failure.into_buffer() {
                    Ok(buffer) => {
                        let actual = match owner.validate_available_buffer(buffer) {
                            Ok(slot) => slot,
                            Err(error) => {
                                let fault = DataRouterFault::AvailableBuffer(error);
                                self.set_slot_fault(
                                    index,
                                    fault,
                                    DataRouterFaultResidue::Available(buffer),
                                );
                                return DataRouterPrepareResult::Disabled(fault);
                            }
                        };
                        if actual != expected_slot || usize::from(actual.get()) != index {
                            let fault = DataRouterFault::OwnerSlotMismatch {
                                expected_index: index,
                                actual,
                            };
                            self.set_slot_fault(
                                index,
                                fault,
                                DataRouterFaultResidue::Available(buffer),
                            );
                            return DataRouterPrepareResult::Disabled(fault);
                        }
                        self.parked[index] = ParkedDataOwner::Available(buffer);
                        DataRouterPrepareResult::Rejected {
                            slot: actual,
                            reason,
                        }
                    }
                    Err(quarantine) => {
                        let actual = quarantine.slot_id();
                        let observation = quarantine.observation();
                        if actual != expected_slot || usize::from(actual.get()) != index {
                            let fault = DataRouterFault::OwnerSlotMismatch {
                                expected_index: index,
                                actual,
                            };
                            self.set_slot_fault(
                                index,
                                fault,
                                DataRouterFaultResidue::Quarantine(quarantine),
                            );
                            return DataRouterPrepareResult::Disabled(fault);
                        }
                        if observation.record().slot_id() != actual {
                            let fault = DataRouterFault::RecoveryObservationMismatch;
                            self.set_slot_fault(
                                index,
                                fault,
                                DataRouterFaultResidue::Quarantine(quarantine),
                            );
                            return DataRouterPrepareResult::Disabled(fault);
                        }
                        self.parked[index] = ParkedDataOwner::Quarantined(quarantine);
                        DataRouterPrepareResult::RejectedQuarantined {
                            reason,
                            observation,
                        }
                    }
                }
            }
        }
    }

    /// Accept one DATA completion already demultiplexed by the shared router.
    ///
    /// Invalid, duplicate, or crossed completions are returned unchanged so a
    /// higher aggregate can retain the conflicting owner explicitly.
    #[allow(clippy::result_large_err)]
    pub fn try_accept_completion(
        &mut self,
        completion: TxCompletion<'static>,
    ) -> Result<(), DataCompletionAcceptFailure> {
        let supplied = DataPreparedHop::from_completion(&completion);
        let slot = supplied.slot_id();
        let index = usize::from(slot.get());
        let Some(expected) = self.active.get(index).copied().flatten() else {
            let reason = if index >= PACKET_BUFFERS {
                DataCompletionAcceptError::SlotOutsidePool {
                    slot,
                    limit: PACKET_BUFFERS,
                }
            } else if let Some(fault) = self.fault {
                DataCompletionAcceptError::Disabled(fault)
            } else {
                DataCompletionAcceptError::NoActiveOwner(slot)
            };
            return Err(DataCompletionAcceptFailure { reason, completion });
        };
        if expected != supplied {
            return Err(DataCompletionAcceptFailure {
                reason: DataCompletionAcceptError::OwnerMismatch {
                    slot,
                    expected_interface: expected.interface(),
                    supplied_interface: supplied.interface(),
                },
                completion,
            });
        }
        if self.pending_completions[index].is_some() {
            return Err(DataCompletionAcceptFailure {
                reason: DataCompletionAcceptError::CompletionPending(slot),
                completion,
            });
        }
        self.pending_completions[index] = Some(completion);
        Ok(())
    }

    /// Validate and authorize one transport-neutral DATA permit request.
    ///
    /// After a permanent coordinator fault, a still-valid actor request is
    /// resolved through forced resource-unavailable denial so an issued owner
    /// can drain without exposing bytes.
    #[allow(clippy::result_large_err)]
    pub fn authorize_tx<
        P,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        request: TxPermitRequest,
        now: MonotonicMillis,
        policy: &mut P,
    ) -> Result<TxPermitReply, DataPermitAuthorizationFailure>
    where
        P: TxAuthorizationPolicy,
    {
        if owner.tx_owner_scope() != self.owner_scope {
            return Err(DataPermitAuthorizationFailure {
                reason: DataPermitAuthorizationError::OwnerMismatch,
                request,
            });
        }
        let result = if self.fault.is_some() {
            owner.authorize_tx(request, now, &mut FaultDrainPolicy)
        } else {
            owner.authorize_tx(request, now, policy)
        };
        result.map_err(
            |failure: TxAuthorizationFailure| DataPermitAuthorizationFailure {
                reason: DataPermitAuthorizationError::Owner(failure.reason()),
                request: failure.into_request(),
            },
        )
    }

    /// Run node-core packet-owner deadline maintenance for the bound owner.
    pub fn maintain_tx<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        &self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        now: MonotonicMillis,
    ) -> Result<TxMaintenanceReport, DataRouterOwnerMismatch> {
        if owner.tx_owner_scope() != self.owner_scope {
            return Err(DataRouterOwnerMismatch);
        }
        Ok(owner.maintain_tx(now))
    }

    /// Run at most one ownership-changing transition against the shared
    /// interface router.
    pub fn step<
        M: RawMutex + 'static,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const SLOTS: usize,
        const QUEUE_DEPTH: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        router: &mut OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        now: MonotonicMillis,
    ) -> DataRouterStep {
        if owner.tx_owner_scope() != self.owner_scope {
            return DataRouterStep::OwnerMismatch;
        }
        if let Some(step) = self.reconcile_one_completion(owner, now) {
            return step;
        }
        if let Some(fault) = self.fault {
            if let Some(step) = self.cancel_one_pending_for_fault() {
                return step;
            }
            return DataRouterStep::Disabled(fault);
        }
        self.route_one_job(router, now)
            .unwrap_or(DataRouterStep::Idle)
    }

    /// Retained permanent coordinator fault, if any.
    pub const fn fault(&self) -> Option<DataRouterFault> {
        self.fault
    }

    /// Copy-only occupancy of the fixed parked-owner table.
    pub fn parked_counts(&self) -> DataRouterParkedCounts {
        let mut counts = DataRouterParkedCounts {
            available: 0,
            recovered: 0,
            quarantined: 0,
        };
        for owner in &self.parked {
            match owner {
                ParkedDataOwner::Empty => {}
                ParkedDataOwner::Available(buffer) => {
                    let _ = buffer.slot_id();
                    counts.available += 1;
                }
                ParkedDataOwner::Recovered { .. } => counts.recovered += 1,
                ParkedDataOwner::Quarantined(_) => counts.quarantined += 1,
            }
        }
        counts
    }

    /// Copy-only owner kind parked at one stable DATA slot.
    pub fn parked_kind(&self, slot: PacketSlotId) -> Option<DataRouterParkedKind> {
        match self.parked.get(usize::from(slot.get()))? {
            ParkedDataOwner::Empty => None,
            ParkedDataOwner::Available(_) => Some(DataRouterParkedKind::Available),
            ParkedDataOwner::Recovered { .. } => Some(DataRouterParkedKind::Recovered),
            ParkedDataOwner::Quarantined(_) => Some(DataRouterParkedKind::Quarantined),
        }
    }

    /// Number of locally retained jobs not yet accepted by an interface actor.
    pub fn pending_job_count(&self) -> usize {
        self.pending_jobs
            .iter()
            .filter(|entry| entry.is_some())
            .count()
    }

    /// Number of DATA slots retaining an exact completion-correlation binding.
    ///
    /// This includes actor-accepted owners and locally synthesized
    /// route-rejection, deadline, or fault-drain completions awaiting
    /// reconciliation.
    pub fn completion_correlated_count(&self) -> usize {
        self.active.iter().filter(|entry| entry.is_some()).count()
    }

    /// Earliest deadline on a DATA job still locally owned by this
    /// coordinator.
    ///
    /// Actor-owned deadlines remain available through node-core's
    /// `next_tx_deadline`; the future aggregate must combine both sources.
    pub fn next_deadline(&self) -> Option<TxLeaseDeadline> {
        self.pending_jobs
            .iter()
            .filter_map(|job| job.as_ref().map(RoutedTxJob::deadline))
            .min()
    }

    /// Kind of the first exact owner retained behind a permanent fault.
    pub fn fault_residue_kind(&self) -> Option<DataRouterFaultResidueKind> {
        self.general_fault_residue
            .as_ref()
            .map(DataRouterFaultResidue::kind)
            .or_else(|| {
                self.slot_fault_residues
                    .iter()
                    .find_map(|residue| residue.as_ref().map(DataRouterFaultResidue::kind))
            })
    }

    /// Number of exact non-copy residues retained after permanent faults.
    pub fn fault_residue_count(&self) -> usize {
        usize::from(self.general_fault_residue.is_some())
            + self
                .slot_fault_residues
                .iter()
                .filter(|residue| residue.is_some())
                .count()
    }

    /// Kind of exact fault residue retained for one stable DATA slot.
    pub fn slot_fault_residue_kind(
        &self,
        slot: PacketSlotId,
    ) -> Option<DataRouterFaultResidueKind> {
        self.slot_fault_residues
            .get(usize::from(slot.get()))
            .and_then(Option::as_ref)
            .map(DataRouterFaultResidue::kind)
    }

    /// Copy the unacknowledged recovery record parked at one slot.
    pub fn recovered_record(&self, slot: PacketSlotId) -> Option<TxRecoveryRecord> {
        match self.parked.get(usize::from(slot.get()))? {
            ParkedDataOwner::Recovered { observation, .. } => Some(observation.record()),
            ParkedDataOwner::Empty
            | ParkedDataOwner::Available(_)
            | ParkedDataOwner::Quarantined(_) => None,
        }
    }

    /// Copy the generation-safe recovery observation parked at one slot.
    pub fn recovered_observation(&self, slot: PacketSlotId) -> Option<TxRecoveryObservation> {
        match self.parked.get(usize::from(slot.get()))? {
            ParkedDataOwner::Recovered { observation, .. } => Some(*observation),
            ParkedDataOwner::Empty
            | ParkedDataOwner::Available(_)
            | ParkedDataOwner::Quarantined(_) => None,
        }
    }

    /// Iterate all unacknowledged recovered observations in stable slot order.
    pub fn recovered_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.parked.iter().filter_map(|owner| match owner {
            ParkedDataOwner::Recovered { observation, .. } => Some(*observation),
            ParkedDataOwner::Empty
            | ParkedDataOwner::Available(_)
            | ParkedDataOwner::Quarantined(_) => None,
        })
    }

    /// Copy the fail-closed quarantine record parked at one slot.
    pub fn quarantine_record(&self, slot: PacketSlotId) -> Option<TxRecoveryRecord> {
        match self.parked.get(usize::from(slot.get()))? {
            ParkedDataOwner::Quarantined(quarantine) => Some(quarantine.record()),
            ParkedDataOwner::Empty
            | ParkedDataOwner::Available(_)
            | ParkedDataOwner::Recovered { .. } => None,
        }
    }

    /// Copy the generation-safe quarantine observation parked at one slot.
    pub fn quarantine_observation(&self, slot: PacketSlotId) -> Option<TxRecoveryObservation> {
        match self.parked.get(usize::from(slot.get()))? {
            ParkedDataOwner::Quarantined(quarantine) => Some(quarantine.observation()),
            ParkedDataOwner::Empty
            | ParkedDataOwner::Available(_)
            | ParkedDataOwner::Recovered { .. } => None,
        }
    }

    /// Iterate every fail-closed quarantine observation in stable packet-slot
    /// order without exposing its retained owner.
    pub fn quarantine_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.parked.iter().filter_map(|owner| match owner {
            ParkedDataOwner::Quarantined(quarantine) => Some(quarantine.observation()),
            ParkedDataOwner::Empty
            | ParkedDataOwner::Available(_)
            | ParkedDataOwner::Recovered { .. } => None,
        })
    }

    /// Acknowledge one exact recovered observation after durable projection.
    pub fn acknowledge_recovered(
        &mut self,
        observation: TxRecoveryObservation,
    ) -> Result<(), DataRecoveryAckError> {
        let Some(owner) = self
            .parked
            .get_mut(usize::from(observation.record().slot_id().get()))
        else {
            return Err(DataRecoveryAckError::SlotOutsidePool);
        };
        match owner {
            ParkedDataOwner::Recovered {
                observation: current,
                ..
            } if *current != observation => Err(DataRecoveryAckError::ObservationMismatch),
            ParkedDataOwner::Recovered { .. } => {
                let parked = mem::replace(owner, ParkedDataOwner::Empty);
                let ParkedDataOwner::Recovered { buffer, .. } = parked else {
                    unreachable!("matched recovered DATA owner changed synchronously")
                };
                *owner = ParkedDataOwner::Available(buffer);
                Ok(())
            }
            ParkedDataOwner::Empty
            | ParkedDataOwner::Available(_)
            | ParkedDataOwner::Quarantined(_) => Err(DataRecoveryAckError::NotRecovered),
        }
    }

    /// Poll every locally retained route job for advisory actor-queue
    /// capacity.
    ///
    /// Readiness never reserves capacity or moves an exact DATA owner.
    pub fn poll_route_progress<
        M: RawMutex + 'static,
        const SLOTS: usize,
        const QUEUE_DEPTH: usize,
    >(
        &mut self,
        router: &mut OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        context: &mut Context<'_>,
    ) -> Poll<()> {
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

    /// Wait cancellation-safely until at least one retained DATA route can be
    /// retried.
    pub async fn wait_route_progress<
        M: RawMutex + 'static,
        const SLOTS: usize,
        const QUEUE_DEPTH: usize,
    >(
        &mut self,
        router: &mut OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
    ) {
        poll_fn(|context| self.poll_route_progress(router, context)).await
    }

    fn reconcile_one_completion<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        now: MonotonicMillis,
    ) -> Option<DataRouterStep> {
        let index = self.next_completion_index()?;
        let completion = self.pending_completions[index]
            .take()
            .expect("selected DATA completion slot must remain occupied");
        let supplied = DataPreparedHop::from_completion(&completion);
        if self.active[index] != Some(supplied) {
            let fault = DataRouterFault::InternalInvariant;
            self.set_slot_fault(index, fault, DataRouterFaultResidue::Completion(completion));
            return Some(DataRouterStep::Disabled(fault));
        }

        match owner.complete_tx(completion, now) {
            Err(failure) => {
                let fault = DataRouterFault::Completion(failure.reason());
                self.set_slot_fault(
                    index,
                    fault,
                    DataRouterFaultResidue::CompletionFailure(failure),
                );
                Some(DataRouterStep::Disabled(fault))
            }
            Ok(TxCompletionDisposition::Next(job)) => {
                self.active[index] = None;
                let hop = DataPreparedHop::from_job(&job);
                if usize::from(hop.slot_id().get()) != index
                    || self.pending_jobs[index].is_some()
                    || !matches!(self.parked[index], ParkedDataOwner::Empty)
                {
                    let fault = DataRouterFault::InternalInvariant;
                    self.set_slot_fault(index, fault, DataRouterFaultResidue::Job(job));
                    return Some(DataRouterStep::Disabled(fault));
                }
                self.pending_jobs[index] = Some(job);
                Some(DataRouterStep::Completion(
                    DataRouterCompletionProgress::Next(hop),
                ))
            }
            Ok(TxCompletionDisposition::Available(buffer)) => {
                self.active[index] = None;
                Some(self.park_available(owner, index, buffer))
            }
            Ok(TxCompletionDisposition::Recovered {
                buffer,
                observation,
            }) => {
                self.active[index] = None;
                Some(self.park_recovered(owner, index, buffer, observation))
            }
            Ok(TxCompletionDisposition::Quarantined(quarantine)) => {
                self.active[index] = None;
                Some(self.park_quarantine(index, quarantine))
            }
        }
    }

    fn route_one_job<M: RawMutex + 'static, const SLOTS: usize, const QUEUE_DEPTH: usize>(
        &mut self,
        router: &mut OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        now: MonotonicMillis,
    ) -> Option<DataRouterStep> {
        let index = self.next_job_index()?;
        let job = self.pending_jobs[index]
            .take()
            .expect("selected DATA route slot must remain occupied");
        let hop = DataPreparedHop::from_job(&job);
        if usize::from(hop.slot_id().get()) != index
            || self.active[index].is_some()
            || self.pending_completions[index].is_some()
            || !matches!(self.parked[index], ParkedDataOwner::Empty)
        {
            let fault = DataRouterFault::InternalInvariant;
            self.set_slot_fault(index, fault, DataRouterFaultResidue::Job(job));
            return Some(DataRouterStep::Disabled(fault));
        }
        if now >= hop.deadline().instant() {
            self.active[index] = Some(hop);
            self.pending_completions[index] = Some(
                job.return_unpermitted()
                    .complete(self.config.deadline_expired_code),
            );
            return Some(DataRouterStep::PendingJobExpired(hop));
        }

        match router.try_route_data(job) {
            Ok(_) => {
                self.active[index] = Some(hop);
                Some(DataRouterStep::Routed(hop))
            }
            Err(failure) => {
                let reason = failure.reason();
                let job = failure.into_job();
                if matches!(reason, RouteError::QueueFull(_)) {
                    self.pending_jobs[index] = Some(job);
                    return Some(DataRouterStep::RouteBackpressured(hop));
                }
                self.active[index] = Some(hop);
                self.pending_completions[index] = Some(
                    job.return_unpermitted()
                        .complete(self.config.route_rejected_code),
                );
                Some(DataRouterStep::RouteRejected { hop, reason })
            }
        }
    }

    fn cancel_one_pending_for_fault(&mut self) -> Option<DataRouterStep> {
        let index = self.next_job_index()?;
        let job = self.pending_jobs[index]
            .take()
            .expect("selected DATA fault-drain slot must remain occupied");
        let hop = DataPreparedHop::from_job(&job);
        if usize::from(hop.slot_id().get()) != index
            || self.active[index].is_some()
            || self.pending_completions[index].is_some()
            || !matches!(self.parked[index], ParkedDataOwner::Empty)
        {
            let fault = DataRouterFault::InternalInvariant;
            self.set_slot_fault(index, fault, DataRouterFaultResidue::Job(job));
            return Some(DataRouterStep::Disabled(fault));
        }
        self.active[index] = Some(hop);
        self.pending_completions[index] = Some(
            job.return_unpermitted()
                .complete(self.config.fault_drain_code),
        );
        Some(DataRouterStep::PendingJobCancelledForFault(hop))
    }

    fn park_available<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        &mut self,
        owner: &NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        index: usize,
        buffer: &'static mut TxPacketBuffer,
    ) -> DataRouterStep {
        let slot = match owner.validate_available_buffer(buffer) {
            Ok(slot) => slot,
            Err(reason) => {
                let fault = DataRouterFault::AvailableBuffer(reason);
                self.set_slot_fault(index, fault, DataRouterFaultResidue::Available(buffer));
                return DataRouterStep::Disabled(fault);
            }
        };
        if usize::from(slot.get()) != index {
            let fault = DataRouterFault::OwnerSlotMismatch {
                expected_index: index,
                actual: slot,
            };
            self.set_slot_fault(index, fault, DataRouterFaultResidue::Available(buffer));
            return DataRouterStep::Disabled(fault);
        }
        if !matches!(self.parked[index], ParkedDataOwner::Empty)
            || self.pending_jobs[index].is_some()
            || self.pending_completions[index].is_some()
        {
            let fault = DataRouterFault::SlotOccupied(slot);
            self.set_slot_fault(index, fault, DataRouterFaultResidue::Available(buffer));
            return DataRouterStep::Disabled(fault);
        }
        self.parked[index] = ParkedDataOwner::Available(buffer);
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(slot))
    }

    fn park_recovered<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        &mut self,
        owner: &NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        index: usize,
        buffer: &'static mut TxPacketBuffer,
        observation: TxRecoveryObservation,
    ) -> DataRouterStep {
        let slot = match owner.validate_available_buffer(buffer) {
            Ok(slot) => slot,
            Err(reason) => {
                let fault = DataRouterFault::AvailableBuffer(reason);
                self.set_slot_fault(
                    index,
                    fault,
                    DataRouterFaultResidue::Recovered {
                        buffer,
                        observation,
                    },
                );
                return DataRouterStep::Disabled(fault);
            }
        };
        if usize::from(slot.get()) != index {
            let fault = DataRouterFault::OwnerSlotMismatch {
                expected_index: index,
                actual: slot,
            };
            self.set_slot_fault(
                index,
                fault,
                DataRouterFaultResidue::Recovered {
                    buffer,
                    observation,
                },
            );
            return DataRouterStep::Disabled(fault);
        }
        if observation.record().slot_id() != slot {
            let fault = DataRouterFault::RecoveryObservationMismatch;
            self.set_slot_fault(
                index,
                fault,
                DataRouterFaultResidue::Recovered {
                    buffer,
                    observation,
                },
            );
            return DataRouterStep::Disabled(fault);
        }
        if !matches!(self.parked[index], ParkedDataOwner::Empty)
            || self.pending_jobs[index].is_some()
            || self.pending_completions[index].is_some()
        {
            let fault = DataRouterFault::SlotOccupied(slot);
            self.set_slot_fault(
                index,
                fault,
                DataRouterFaultResidue::Recovered {
                    buffer,
                    observation,
                },
            );
            return DataRouterStep::Disabled(fault);
        }
        self.parked[index] = ParkedDataOwner::Recovered {
            buffer,
            observation,
        };
        DataRouterStep::Completion(DataRouterCompletionProgress::Recovered(observation))
    }

    fn park_quarantine(
        &mut self,
        index: usize,
        quarantine: TxQuarantine<'static>,
    ) -> DataRouterStep {
        let slot = quarantine.slot_id();
        let observation = quarantine.observation();
        if usize::from(slot.get()) != index {
            let fault = DataRouterFault::OwnerSlotMismatch {
                expected_index: index,
                actual: slot,
            };
            self.set_slot_fault(index, fault, DataRouterFaultResidue::Quarantine(quarantine));
            return DataRouterStep::Disabled(fault);
        }
        if observation.record().slot_id() != slot {
            let fault = DataRouterFault::RecoveryObservationMismatch;
            self.set_slot_fault(index, fault, DataRouterFaultResidue::Quarantine(quarantine));
            return DataRouterStep::Disabled(fault);
        }
        if !matches!(self.parked[index], ParkedDataOwner::Empty)
            || self.pending_jobs[index].is_some()
            || self.pending_completions[index].is_some()
        {
            let fault = DataRouterFault::SlotOccupied(slot);
            self.set_slot_fault(index, fault, DataRouterFaultResidue::Quarantine(quarantine));
            return DataRouterStep::Disabled(fault);
        }
        self.parked[index] = ParkedDataOwner::Quarantined(quarantine);
        DataRouterStep::Completion(DataRouterCompletionProgress::Quarantined(observation))
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

    fn set_slot_fault(
        &mut self,
        index: usize,
        fault: DataRouterFault,
        residue: DataRouterFaultResidue,
    ) {
        self.fault.get_or_insert(fault);
        let Some(slot) = self.slot_fault_residues.get_mut(index) else {
            self.set_general_fault(fault, residue);
            return;
        };
        assert!(
            slot.is_none(),
            "one stable DATA packet owner cannot produce two fault residues"
        );
        self.active[index] = None;
        *slot = Some(residue);
    }

    fn set_general_fault(&mut self, fault: DataRouterFault, residue: DataRouterFaultResidue) {
        self.fault.get_or_insert(fault);
        assert!(
            self.general_fault_residue.is_none(),
            "one unindexed DATA owner cannot produce two fault residues"
        );
        self.general_fault_residue = Some(residue);
    }
}

#[cfg(test)]
mod tests;
