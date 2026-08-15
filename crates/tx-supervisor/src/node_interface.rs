//! Permanent transport-neutral node and interface orchestration.

use core::mem::{self, MaybeUninit};

use crate::{
    DataCompletionAcceptError, DataPermitServer, DataPermitServerPhase, DataPermitServerStep,
    DataRecoveryAckError, DataRouterCoordinator, DataRouterFault, DataRouterOwnerMismatch,
    DataRouterParkedCounts, DataRouterPrepareRequest, DataRouterPrepareResult, DataRouterStep,
    OrdinaryCompletionAcceptError, OrdinaryPermitServer, OrdinaryPermitServerPhase,
    OrdinaryPermitServerStep, OrdinaryRouterAdmission, OrdinaryRouterCoordinator,
    OrdinaryRouterDisplacedActions, OrdinaryRouterFault, OrdinaryRouterFaultResidueKind,
    OrdinaryRouterOfferError, OrdinaryRouterOfferFailure, OrdinaryRouterRejectedActions,
    OrdinaryRouterStep,
};
use embassy_sync::blocking_mutex::raw::RawMutex;
use rand_core::{CryptoRng, RngCore};
#[cfg(test)]
use reticulum_interface_router::InterfaceLifecycleState;
use reticulum_interface_router::{
    CompletionRouteError, IngressBufferId, IngressBufferReturnError, IngressBufferReturnFailure,
    IngressRouteError, InterfaceActorHandoff, InterfaceDescriptor, InterfaceFabric, InterfaceLease,
    InterfaceLeaseError, InterfaceLifecycleTransition, InterfaceProperties, InterfaceQueueId,
    InterfaceRegistrationError, InterfaceRegistry, InterfaceTopology, OutboundCompletion,
    OutboundRouter, SealedIngressPacket,
};
use reticulum_node_core::{
    AcknowledgeError, AnnounceAdmissionError, AnnounceEmissionTime, ApplicationEventOfferError,
    ApplicationEventOfferReport, ApplicationEventOwner, ApplicationLinkBinding, AttemptHandle,
    CapacitySnapshot, DestinationHash, EmbeddedNodeMetrics, IngressAnnounceEgress,
    IngressBroadcastPolicy, IngressBroadcastScope, IngressReport, InitiateLinkError, LinkHandle,
    LinkState, LocalDestinationAnnounceTargetError, LxmfMessageLocation, MaintenanceReport,
    MonotonicInstant, MonotonicMillis, MonotonicSeconds, NodeActions, NodeConfig,
    NodeConstructionError, NodeCore, NodeIdentity, NodeInstanceId, OrdinaryActionCapacitySnapshot,
    OrdinaryPreparedPacket, OutboundDispatchInterval, OutboundProtocolToken,
    PacketIngressObservation, PacketInterfaceId, PacketSignalObservation, PathRequestError,
    PrepareBasicLxmfError, PrepareRequestError, PrepareResponseError, PreparedBasicDirectLxmf,
    PreparedBasicLxmf, PreparedPacket, PreparedRequestActions, ProofProbeIdentityAliasError,
    ReceiptCorrelationError, RequestDispatchConfirmation, RequestDispatchError,
    RequestDispatchReconciliation, RequestHandle, RetainedRouteSnapshot, TerminalAttempt,
    TerminalAttempts, TxAuthorizationPolicy, TxLeaseDeadline, TxMaintenanceReport, TxOwnerScope,
    TxRecoveryObservation, TxRoutePlan, TxTarget,
};
use reticulum_tx_handoff::{
    DataPairedPermitHandoff, DispatcherPermitHandoff, OrdinaryDispatcherPermitHandoff,
    OrdinaryPairedPermitHandoff,
};

/// Resolution of one RNS-retained interface binding against current registry state.
///
/// An exact route retains only an interface identifier. Its descriptor is the
/// registry's current descriptor, not proof that the route was learned under
/// that descriptor generation or configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteInterfaceResolution {
    /// RNS retained no exact interface and will use current broadcast fallback.
    BroadcastFallback {
        /// Whether at least one currently eligible interface satisfies the fallback.
        usable: bool,
    },
    /// RNS retained one exact interface identifier.
    Exact {
        /// Exact interface identifier retained with the route.
        interface: PacketInterfaceId,
        /// Current descriptor for that identifier, if still registered.
        current: Option<InterfaceDescriptor>,
        /// Whether the exact target resolves against current interface eligibility.
        usable: bool,
    },
}

impl RouteInterfaceResolution {
    /// Whether the retained target can currently accept a new routed owner.
    pub const fn is_usable(self) -> bool {
        match self {
            Self::BroadcastFallback { usable } | Self::Exact { usable, .. } => usable,
        }
    }

    /// Exact retained interface, or `None` when RNS will use broadcast fallback.
    pub const fn retained_interface(self) -> Option<PacketInterfaceId> {
        match self {
            Self::BroadcastFallback { .. } => None,
            Self::Exact { interface, .. } => Some(interface),
        }
    }

    /// Current authoritative descriptor for an exact retained interface.
    ///
    /// Broadcast fallback can involve multiple interfaces and therefore has no
    /// single descriptor. `None` for an exact route means the retained
    /// identifier is not currently registered.
    pub const fn current_descriptor(self) -> Option<InterfaceDescriptor> {
        match self {
            Self::BroadcastFallback { .. } => None,
            Self::Exact { current, .. } => current,
        }
    }

    /// Current online state of an exact retained interface.
    ///
    /// `None` means either broadcast fallback or a missing current descriptor.
    pub const fn current_online(self) -> Option<bool> {
        match self {
            Self::BroadcastFallback { .. } => None,
            Self::Exact {
                current: Some(descriptor),
                ..
            } => Some(descriptor.is_online()),
            Self::Exact { current: None, .. } => None,
        }
    }

    /// Whether RNS retained transport-neutral broadcast fallback.
    pub const fn is_broadcast_fallback(self) -> bool {
        matches!(self, Self::BroadcastFallback { .. })
    }
}

/// Expiry interpretation of one retained route at a snapshot's monotonic time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteExpiryState {
    /// The route remains within Rete's inclusive expiry boundary.
    Retained {
        /// Whole seconds until the boundary, possibly zero at the last valid sample.
        remaining_seconds: u64,
    },
    /// The route exceeded its boundary but periodic maintenance has not removed it.
    ExpiredPendingMaintenance {
        /// Whole seconds beyond the inclusive expiry boundary.
        overdue_seconds: u64,
    },
    /// Snapshot time preceded the route's retained learning timestamp.
    ClockRegression,
}

/// One retained route enriched with current interface-registry resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteDiagnosticsEntry {
    route: RetainedRouteSnapshot,
    resolution: RouteInterfaceResolution,
}

impl RouteDiagnosticsEntry {
    /// Copy-only native route-table projection.
    pub const fn route(self) -> RetainedRouteSnapshot {
        self.route
    }

    /// Resolution against the current authoritative interface registry.
    pub const fn resolution(self) -> RouteInterfaceResolution {
        self.resolution
    }

    /// Age of the retained route candidate at `captured_at`.
    ///
    /// `None` reports a monotonic-clock regression instead of flattening it to zero.
    pub const fn learned_age_seconds(self, captured_at: MonotonicSeconds) -> Option<u64> {
        captured_at.get().checked_sub(self.route.learned_at().get())
    }

    /// Age of Rete's LRU timestamp at `captured_at`.
    ///
    /// This is not peer activity or a last-heard measurement.
    pub const fn lru_age_seconds(self, captured_at: MonotonicSeconds) -> Option<u64> {
        captured_at
            .get()
            .checked_sub(self.route.last_accessed_at().get())
    }

    /// Interpret the retained route's expiry boundary at `captured_at`.
    pub const fn expiry_state(self, captured_at: MonotonicSeconds) -> RouteExpiryState {
        let Some(age) = self.learned_age_seconds(captured_at) else {
            return RouteExpiryState::ClockRegression;
        };
        let expiry = self.route.expires_after_seconds();
        if age <= expiry {
            RouteExpiryState::Retained {
                remaining_seconds: expiry - age,
            }
        } else {
            RouteExpiryState::ExpiredPendingMaintenance {
                overdue_seconds: age - expiry,
            }
        }
    }
}

/// Caller-owned, allocation-free full route-table diagnostics snapshot.
///
/// Entries are sorted lexicographically by complete destination hash. This is
/// a complete replacement view with no mutation revision: the pinned RNS path
/// table exposes no revision counter. Consumers must not paginate over live
/// native iteration. A future paged API must retain one complete snapshot and
/// assign its own boot incarnation and generation.
pub struct RouteDiagnosticsSnapshot<const PATHS: usize> {
    captured_at: MonotonicSeconds,
    len: usize,
    entries: [Option<RouteDiagnosticsEntry>; PATHS],
}

impl<const PATHS: usize> RouteDiagnosticsSnapshot<PATHS> {
    /// Construct empty caller-owned storage for later snapshot refreshes.
    pub const fn empty() -> Self {
        Self {
            captured_at: MonotonicSeconds::new(0),
            len: 0,
            entries: [None; PATHS],
        }
    }

    /// Monotonic whole-second sample shared by every entry in this snapshot.
    ///
    /// This timestamp is not a mutation revision.
    pub const fn captured_at(&self) -> MonotonicSeconds {
        self.captured_at
    }

    /// Number of complete retained routes copied into this snapshot.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the copied route table was empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Fixed route capacity inherited from the node's `PATHS` profile.
    pub const fn capacity(&self) -> usize {
        PATHS
    }

    /// Iterate complete entries in deterministic destination-hash order.
    pub fn entries(&self) -> impl Iterator<Item = &RouteDiagnosticsEntry> {
        self.entries[..self.len].iter().map(|entry| {
            entry
                .as_ref()
                .expect("the initialized route diagnostics prefix is contiguous")
        })
    }

    fn reset(&mut self, captured_at: MonotonicSeconds) {
        self.captured_at = captured_at;
        self.len = 0;
        self.entries.fill(None);
    }

    fn push(&mut self, entry: RouteDiagnosticsEntry) -> bool {
        let Some(slot) = self.entries.get_mut(self.len) else {
            return false;
        };
        *slot = Some(entry);
        self.len += 1;
        true
    }

    fn sort_by_destination(&mut self) {
        for index in 1..self.len {
            let mut cursor = index;
            while cursor > 0 {
                let out_of_order = {
                    let left = self.entries[cursor - 1]
                        .as_ref()
                        .expect("initialized route entry");
                    let right = self.entries[cursor]
                        .as_ref()
                        .expect("initialized route entry");
                    left.route.destination().as_bytes() > right.route.destination().as_bytes()
                };
                if !out_of_order {
                    break;
                }
                self.entries.swap(cursor - 1, cursor);
                cursor -= 1;
            }
        }
    }
}

impl<const PATHS: usize> Default for RouteDiagnosticsSnapshot<PATHS> {
    fn default() -> Self {
        Self::empty()
    }
}

/// Why permanent node/interface aggregate construction failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceSupervisorBuildError {
    /// A permanent aggregate requires at least one concrete interface slot.
    ZeroInterfaceSlots,
    /// The DATA coordinator belongs to another node incarnation.
    DataOwnerMismatch {
        /// Scope supplied by the node owner.
        node: TxOwnerScope,
        /// Scope retained by the DATA coordinator.
        coordinator: TxOwnerScope,
    },
    /// The ordinary coordinator belongs to another node incarnation.
    OrdinaryOwnerMismatch {
        /// Scope supplied by the node owner.
        node: TxOwnerScope,
        /// Scope retained by the ordinary coordinator.
        coordinator: TxOwnerScope,
    },
}

/// Failed construction retaining every supplied non-copy component unchanged.
#[must_use = "failed aggregate construction retains every permanent owner"]
pub struct NodeInterfaceSupervisorBuildFailure<
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
> where
    M: RawMutex + 'static,
{
    reason: NodeInterfaceSupervisorBuildError,
    node: NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
    fabric: &'static mut InterfaceFabric<M, SLOTS, QUEUE_DEPTH>,
    data: DataRouterCoordinator<DATA_PACKET_BUFFERS>,
    ordinary: OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
    data_permits: [DataPairedPermitHandoff<M>; SLOTS],
    ordinary_permits: [OrdinaryPairedPermitHandoff<M>; SLOTS],
    policy: P,
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
>
    NodeInterfaceSupervisorBuildFailure<
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
{
    /// Scalar construction rejection.
    pub const fn reason(&self) -> NodeInterfaceSupervisorBuildError {
        self.reason
    }

    /// Recover every unchanged construction input in argument order.
    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
        &'static mut InterfaceFabric<M, SLOTS, QUEUE_DEPTH>,
        DataRouterCoordinator<DATA_PACKET_BUFFERS>,
        OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
        [DataPairedPermitHandoff<M>; SLOTS],
        [OrdinaryPairedPermitHandoff<M>; SLOTS],
        P,
    ) {
        (
            self.node,
            self.fabric,
            self.data,
            self.ordinary,
            self.data_permits,
            self.ordinary_permits,
            self.policy,
        )
    }
}

#[cfg(test)]
mod tests;

/// Interface-router and permit capabilities permanently associated with one
/// actor slot.
#[must_use = "dropping actor ports abandons its router and permit capabilities"]
pub struct NodeInterfaceActorPorts<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    interface: InterfaceActorHandoff<M, QUEUE_DEPTH>,
    data_permit: DispatcherPermitHandoff<M>,
    ordinary_permit: OrdinaryDispatcherPermitHandoff<M>,
}

impl<M, const QUEUE_DEPTH: usize> NodeInterfaceActorPorts<M, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Fixed interface queue associated with both permit pairs.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.interface.queue_id()
    }

    /// Consume this common-slot group into the concrete actor capabilities.
    pub fn into_parts(
        self,
    ) -> (
        InterfaceActorHandoff<M, QUEUE_DEPTH>,
        DispatcherPermitHandoff<M>,
        OrdinaryDispatcherPermitHandoff<M>,
    ) {
        (self.interface, self.data_permit, self.ordinary_permit)
    }
}

/// Successful checked construction and the actor capabilities split from the
/// same interface fabric and permit stores.
#[must_use = "the permanent supervisor and every actor capability must be retained"]
pub struct NodeInterfaceSupervisorBuildSuccess<
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
> where
    M: RawMutex + 'static,
{
    supervisor: NodeInterfaceSupervisor<
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
    >,
    actors: [NodeInterfaceActorPorts<M, QUEUE_DEPTH>; SLOTS],
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
>
    NodeInterfaceSupervisorBuildSuccess<
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
{
    /// Consume success into the permanent aggregate and per-slot actor ports.
    pub fn into_parts(
        self,
    ) -> (
        NodeInterfaceSupervisor<
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
        >,
        [NodeInterfaceActorPorts<M, QUEUE_DEPTH>; SLOTS],
    ) {
        (self.supervisor, self.actors)
    }
}

/// Partially initialized permanent aggregate whose node already resides at
/// its final caller-owned address.
///
/// This boot-only stage lets a product configure the node, register external
/// packet buffers, and derive the DATA and ordinary coordinators without ever
/// moving the capacity-sized node through a stack frame. The stage must be
/// consumed by [`Self::finish`]; dropping it intentionally leaves the
/// caller-owned destination partially initialized and unavailable for reuse.
#[must_use = "the in-place node must be configured and the supervisor construction finished"]
pub struct NodeInterfaceSupervisorInit<
    'a,
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
> where
    M: RawMutex + 'static,
{
    destination: &'a mut MaybeUninit<
        NodeInterfaceSupervisor<
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
        >,
    >,
}

impl<
    'a,
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
>
    NodeInterfaceSupervisorInit<
        'a,
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
    /// Mutably borrow the completely initialized node field while the rest of
    /// the supervisor remains inaccessible.
    #[allow(
        unsafe_code,
        reason = "the stage is created only after the projected node field is fully initialized and uniquely owns the parent MaybeUninit"
    )]
    pub fn node_mut(
        &mut self,
    ) -> &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS> {
        let supervisor = self.destination.as_mut_ptr();
        // SAFETY: `begin_node_in` creates this stage only after `NodeCore::new_in`
        // initialized this exact field. The stage's exclusive borrow prevents
        // any other access to the parent allocation.
        unsafe { &mut *core::ptr::addr_of_mut!((*supervisor).node) }
    }

    /// Finish every remaining aggregate field directly at its final address.
    ///
    /// Ownership validation precedes fabric splitting and all remaining
    /// writes. An error consumes this boot-only stage and leaves only the node
    /// field initialized; callers must abandon the backing allocation.
    #[allow(
        unsafe_code,
        clippy::too_many_arguments,
        clippy::type_complexity,
        reason = "audited field-wise placement completes the large aggregate without a by-value supervisor temporary"
    )]
    #[inline(never)]
    pub fn finish(
        self,
        fabric: &'static mut InterfaceFabric<M, SLOTS, QUEUE_DEPTH>,
        data: DataRouterCoordinator<DATA_PACKET_BUFFERS>,
        ordinary: OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
        data_permits: [DataPairedPermitHandoff<M>; SLOTS],
        ordinary_permits: [OrdinaryPairedPermitHandoff<M>; SLOTS],
        policy: P,
    ) -> Result<
        (
            &'a mut NodeInterfaceSupervisor<
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
            >,
            [NodeInterfaceActorPorts<M, QUEUE_DEPTH>; SLOTS],
        ),
        NodeInterfaceSupervisorBuildError,
    > {
        let supervisor = self.destination.as_mut_ptr();
        // SAFETY: the stage proves the node field is initialized and uniquely
        // borrowed. No reference to any other parent field is created.
        let node = unsafe { &*core::ptr::addr_of!((*supervisor).node) };
        if let Some(reason) = NodeInterfaceSupervisor::<
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
        >::build_error(node, &data, &ordinary)
        {
            return Err(reason);
        }

        let (router, interface_actors) = fabric.split();
        let mut data_pairs = data_permits.into_iter();
        let mut ordinary_pairs = ordinary_permits.into_iter();
        let mut interface_actors = interface_actors.into_iter();

        // SAFETY: `supervisor` addresses one uniquely borrowed parent whose
        // node field is already initialized. Validation above is the only
        // fallible step. Every other field and every permit array element is
        // written exactly once before the complete reference is exposed.
        let (supervisor, actors) = unsafe {
            let data_servers =
                core::ptr::addr_of_mut!((*supervisor).data_permits).cast::<DataPermitServer<M>>();
            let ordinary_servers = core::ptr::addr_of_mut!((*supervisor).ordinary_permits)
                .cast::<OrdinaryPermitServer<M>>();
            let actors = core::array::from_fn(|index| {
                let (data_node, data_permit) = data_pairs
                    .next()
                    .expect("the DATA permit iterator has exactly SLOTS elements")
                    .into_parts();
                let (ordinary_node, ordinary_permit) = ordinary_pairs
                    .next()
                    .expect("the ordinary permit iterator has exactly SLOTS elements")
                    .into_parts();
                data_servers
                    .add(index)
                    .write(DataPermitServer::new(data_node));
                ordinary_servers
                    .add(index)
                    .write(OrdinaryPermitServer::new(ordinary_node));
                NodeInterfaceActorPorts {
                    interface: interface_actors
                        .next()
                        .expect("the interface actor iterator has exactly SLOTS elements"),
                    data_permit,
                    ordinary_permit,
                }
            });

            core::ptr::addr_of_mut!((*supervisor).router).write(router);
            core::ptr::addr_of_mut!((*supervisor).data).write(data);
            core::ptr::addr_of_mut!((*supervisor).ordinary).write(ordinary);
            core::ptr::addr_of_mut!((*supervisor).policy).write(policy);
            core::ptr::addr_of_mut!((*supervisor).lane_cursor).write(0);
            core::ptr::addr_of_mut!((*supervisor).completion_fault).write(None);
            core::ptr::addr_of_mut!((*supervisor).owner_mismatch_fault).write(false);
            core::ptr::addr_of_mut!((*supervisor).pending_ingress_actions).write(None);
            core::ptr::addr_of_mut!((*supervisor).terminal_ingress_actions).write(None);
            core::ptr::addr_of_mut!((*supervisor).pending_ingress_recycle).write(None);
            core::ptr::addr_of_mut!((*supervisor).terminal_ingress_recycle).write(None);
            (&mut *supervisor, actors)
        };
        Ok((supervisor, actors))
    }
}

/// Packet family of a router-demultiplexed completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceCompletionFamily {
    /// Destination-DATA owner.
    Data,
    /// Allocation-backed ordinary protocol-action owner.
    Ordinary,
}

/// How one exact completion reached node-owner reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceCompletionOrigin {
    /// The completion still named the current registry generation.
    Current,
    /// Router attribution failed, but the exact owner entered explicit node
    /// recovery as required.
    Recovery(CompletionRouteError),
}

/// A coordinator rejected an exact completion after central demultiplexing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceSupervisorFault {
    /// The owned DATA coordinator and node incarnation unexpectedly diverged
    /// after successful construction.
    DataOwnerMismatch,
    /// DATA completion correlation rejected the retained owner.
    DataCompletionRejected(DataCompletionAcceptError),
    /// Ordinary completion correlation rejected the retained owner.
    OrdinaryCompletionRejected(OrdinaryCompletionAcceptError),
}

/// Copy-only identity of the exact completion retained behind an aggregate
/// fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceCompletionResidue {
    /// Destination-DATA completion binding.
    Data {
        /// Exact prepared DATA attempt metadata.
        prepared: PreparedPacket,
        /// Interface selected for this serialized hop.
        interface: PacketInterfaceId,
    },
    /// Ordinary completion binding.
    Ordinary(OrdinaryPreparedPacket),
}

struct CompletionFaultResidue {
    fault: NodeInterfaceSupervisorFault,
    completion: OutboundCompletion,
}

impl CompletionFaultResidue {
    fn binding(&self) -> NodeInterfaceCompletionResidue {
        match &self.completion {
            OutboundCompletion::Data(completion) => NodeInterfaceCompletionResidue::Data {
                prepared: completion.prepared(),
                interface: completion.interface(),
            },
            OutboundCompletion::Ordinary(completion) => {
                NodeInterfaceCompletionResidue::Ordinary(completion.prepared())
            }
        }
    }
}

/// One useful ownership transition completed by a bounded aggregate pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceSupervisorTransition {
    /// A current or recovery-path completion entered its matching coordinator.
    CompletionAccepted {
        /// Completion owner family.
        family: NodeInterfaceCompletionFamily,
        /// Current-generation or explicit recovery path.
        origin: NodeInterfaceCompletionOrigin,
    },
    /// One concrete actor lifecycle report was applied or rejected and its
    /// exact acknowledgement was returned.
    Lifecycle(InterfaceLifecycleTransition),
    /// One DATA coordinator transition progressed or entered a new permanent
    /// fault.
    Data(DataRouterStep),
    /// One ordinary coordinator transition progressed or entered a new
    /// permanent fault.
    Ordinary(OrdinaryRouterStep),
    /// One concrete actor's DATA permit service advanced or entered a new
    /// terminal fault.
    DataPermit {
        /// Stable interface actor slot.
        actor: usize,
        /// Observable permit transition.
        step: DataPermitServerStep,
    },
    /// One concrete actor's ordinary permit service advanced or entered a new
    /// terminal fault.
    OrdinaryPermit {
        /// Stable interface actor slot.
        actor: usize,
        /// Observable permit transition.
        step: OrdinaryPermitServerStep,
    },
    /// A coordinator rejected the exact router completion now retained by the
    /// aggregate.
    Fault(NodeInterfaceSupervisorFault),
    /// No lane could complete a useful synchronous transition.
    Idle,
}

/// Result of one bounded synchronous aggregate pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "maintenance and ownership progress must be observed"]
pub struct NodeInterfaceSupervisorPass {
    maintenance: Result<TxMaintenanceReport, DataRouterOwnerMismatch>,
    transition: NodeInterfaceSupervisorTransition,
}

impl NodeInterfaceSupervisorPass {
    /// DATA owner maintenance performed exactly once at the start of the pass.
    pub const fn maintenance(self) -> Result<TxMaintenanceReport, DataRouterOwnerMismatch> {
        self.maintenance
    }

    /// At most one useful ownership transition selected by the fair scan.
    pub const fn transition(self) -> NodeInterfaceSupervisorTransition {
        self.transition
    }
}

/// Aggregate-level DATA preparation result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "DATA preparation and aggregate disablement must be observed"]
pub enum NodeInterfaceDataPrepareResult {
    /// The aggregate is retaining a rejected completion and accepts no new
    /// outbound owners.
    Fault(NodeInterfaceSupervisorFault),
    /// Result produced by the permanently owned DATA coordinator.
    Coordinator(DataRouterPrepareResult),
}

/// Why an ordinary action envelope was not accepted by the aggregate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceOrdinaryOfferError {
    /// The aggregate is retaining a rejected completion.
    Fault(NodeInterfaceSupervisorFault),
    /// Ordinary coordinator-local offer rejection.
    Coordinator(OrdinaryRouterOfferError),
}

/// Result of draining one ordinary non-packet envelope into the application
/// event owner supplied by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "application-event drain progress and retained pressure must be observed"]
pub enum NodeInterfaceApplicationEventDrain {
    /// The ordinary coordinator has no non-packet envelope ready.
    Idle,
    /// Every event entered caller-owned slots atomically.
    Drained(ApplicationEventOfferReport),
    /// The exact unchanged envelope remains in the ordinary coordinator.
    /// [`ApplicationEventOfferError::Full`] is retryable; all other reasons
    /// require product policy or replacement owner capacity.
    Retained(ApplicationEventOfferError),
}

/// Failed aggregate ordinary offer retaining the exact action envelope.
#[must_use = "an unaccepted ordinary action envelope remains exactly owned"]
pub struct NodeInterfaceOrdinaryOfferFailure {
    reason: NodeInterfaceOrdinaryOfferError,
    actions: NodeActions,
    admission: OrdinaryRouterAdmission,
}

/// Timer maintenance whose returned actions entered the ordinary coordinator.
#[must_use = "timer outcomes and receipt faults must be observed"]
pub struct NodeInterfaceTickAccepted {
    report: MaintenanceReport,
}

impl NodeInterfaceTickAccepted {
    /// Recover the timer report. `report.actions` is empty because the exact
    /// envelope already entered the ordinary coordinator.
    pub fn into_report(self) -> MaintenanceReport {
        self.report
    }
}

/// Timer maintenance completed, but its exact returned action envelope did
/// not enter the ordinary coordinator.
#[must_use = "failed timer action admission retains every returned action"]
pub struct NodeInterfaceTickActionFailure {
    report: MaintenanceReport,
    offer: NodeInterfaceOrdinaryOfferFailure,
}

impl NodeInterfaceTickActionFailure {
    /// Scalar ordinary-action admission reason.
    pub const fn reason(&self) -> NodeInterfaceOrdinaryOfferError {
        self.offer.reason()
    }

    /// Recover the action-free timer report and exact rejected action
    /// envelope.
    pub fn into_parts(self) -> (MaintenanceReport, NodeInterfaceOrdinaryOfferFailure) {
        (self.report, self.offer)
    }
}

/// Result of one protocol timer tick and immediate action admission attempt.
#[must_use = "timer actions and correlation faults require explicit handling"]
// The portable no_std boundary must return the exact allocation-backed action
// owner inline on rejection; boxing would hide the ownership contract.
#[allow(clippy::large_enum_variant)]
pub enum NodeInterfaceTickResult {
    /// Every returned action entered the ordinary coordinator.
    Accepted(NodeInterfaceTickAccepted),
    /// The exact returned actions remain in the failure envelope.
    ActionOfferRejected(NodeInterfaceTickActionFailure),
}

/// Result of flushing queued local announces and immediately admitting their
/// exact action envelope.
#[must_use = "flushed announce actions require explicit admission handling"]
// The portable no_std boundary must return the exact allocation-backed action
// owner inline on rejection; boxing would hide the ownership contract.
#[allow(clippy::large_enum_variant)]
pub enum NodeInterfaceAnnounceFlushResult {
    /// Every flushed announce action entered the ordinary coordinator.
    Accepted,
    /// The exact flushed action envelope remains in this failure.
    ActionOfferRejected(NodeInterfaceOrdinaryOfferFailure),
}

/// Non-retryable reason an ingress-produced action envelope could not enter
/// the ordinary coordinator.
///
/// [`OrdinaryRouterOfferError::Busy`] is deliberately absent: transient
/// pressure remains in the aggregate's retry slot instead of becoming a
/// terminal owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceIngressActionFault {
    /// An aggregate completion/owner fault has stopped new ordinary intake.
    Aggregate(NodeInterfaceSupervisorFault),
    /// The ordinary coordinator is permanently disabled.
    CoordinatorDisabled(OrdinaryRouterFault),
    /// The exact action envelope exceeds the configured fixed packet pool.
    EnvelopeExceedsPool {
        /// Packets retained in the exact action envelope.
        packet_count: usize,
        /// Fixed ordinary packet-owner limit.
        limit: usize,
    },
}

/// Exact ingress-produced action envelope retained behind a non-retryable
/// admission fault.
#[must_use = "terminal ingress actions must be quarantined or recovered explicitly"]
pub struct NodeInterfaceTerminalIngressActions {
    fault: NodeInterfaceIngressActionFault,
    actions: NodeActions,
    admission: OrdinaryRouterAdmission,
}

impl NodeInterfaceTerminalIngressActions {
    /// Copy-only terminal classification for fail-stop policy.
    pub const fn fault(&self) -> NodeInterfaceIngressActionFault {
        self.fault
    }

    /// Recover the exact unchanged action envelope and admission request.
    pub fn into_parts(self) -> (NodeActions, OrdinaryRouterAdmission) {
        (self.actions, self.admission)
    }
}

/// Exact reusable-buffer return failure retained for retry or explicit
/// recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeInterfaceIngressRecycleFault {
    reason: IngressBufferReturnError,
    buffer: IngressBufferId,
}

impl NodeInterfaceIngressRecycleFault {
    /// Typed router rejection for the exact retained sealed buffer.
    pub const fn reason(self) -> IngressBufferReturnError {
        self.reason
    }

    /// Stable queue-local identity of the retained exact buffer.
    pub const fn buffer(self) -> IngressBufferId {
        self.buffer
    }

    /// Whether retrying the exact buffer return can recover from this reason.
    pub const fn is_retryable(self) -> bool {
        matches!(self.reason, IngressBufferReturnError::QueueFull(_))
    }

    /// Whether this reason requires explicit packet quarantine or recovery.
    pub const fn is_terminal(self) -> bool {
        !self.is_retryable()
    }
}

/// Successfully ingested queued packet and its action-free protocol report.
#[must_use = "queued ingress report and retained-action status must be observed"]
pub struct NodeInterfaceQueuedIngressProcessed {
    buffer: IngressBufferId,
    report: IngressReport,
    actions_backpressured: Option<NodeInterfaceOrdinaryOfferError>,
    terminal_action_fault: Option<NodeInterfaceIngressActionFault>,
    recycle_fault: Option<NodeInterfaceIngressRecycleFault>,
}

impl NodeInterfaceQueuedIngressProcessed {
    /// Stable exact buffer identity already recycled or retained for retry.
    pub const fn buffer(&self) -> IngressBufferId {
        self.buffer
    }

    /// Retryable action-admission pressure retained internally, if any.
    ///
    /// This is always [`OrdinaryRouterOfferError::Busy`]. Non-retryable
    /// failures are reported by [`Self::terminal_action_fault`].
    pub const fn actions_backpressured(&self) -> Option<NodeInterfaceOrdinaryOfferError> {
        self.actions_backpressured
    }

    /// Non-retryable action owner retained for explicit recovery, if any.
    pub const fn terminal_action_fault(&self) -> Option<NodeInterfaceIngressActionFault> {
        self.terminal_action_fault
    }

    /// Exact sealed-buffer recycle failure retained internally, if any.
    pub const fn recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        self.recycle_fault
    }

    /// Terminal exact-buffer residue created while processing this packet, if
    /// any.
    pub const fn terminal_recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        match self.recycle_fault {
            Some(fault) if fault.is_terminal() => Some(fault),
            _ => None,
        }
    }

    /// Recover the action-free node ingress report.
    pub fn into_report(self) -> IngressReport {
        self.report
    }
}

/// One bounded queued-ingress transition.
#[must_use = "queued ingress ownership and pressure must be observed"]
// The portable no_std boundary cannot heap-box the allocation-backed ingress
// report; shrinking this variant would otherwise hide or duplicate the exact
// result owner instead of returning it explicitly.
#[allow(clippy::large_enum_variant)]
pub enum NodeInterfaceIngressStep {
    /// A previously retained exact sealed buffer returned to its actor pool.
    RecycleRetried(IngressBufferId),
    /// Exact sealed buffer remains retained for a later recycle retry.
    RecycleBackpressured(NodeInterfaceIngressRecycleFault),
    /// A non-retryable exact sealed buffer remains retained until explicitly
    /// recovered from the aggregate.
    TerminalRecyclePending(NodeInterfaceIngressRecycleFault),
    /// A previously retained protocol-action envelope entered the ordinary
    /// coordinator.
    RetainedActionsAdmitted,
    /// A retained protocol-action envelope remains backpressured.
    ActionsBackpressured(NodeInterfaceOrdinaryOfferError),
    /// A non-retryable action envelope remains retained until explicitly
    /// recovered from the aggregate.
    TerminalActionsPending(NodeInterfaceIngressActionFault),
    /// Router validation rejected one sealed packet and its exact buffer was
    /// recycled or retained as reported.
    RouteRejected {
        /// Router provenance/origin rejection.
        reason: IngressRouteError,
        /// Stable exact buffer identity.
        buffer: IngressBufferId,
        /// Recycle fault retained internally, if exact return failed.
        recycle_fault: Option<NodeInterfaceIngressRecycleFault>,
    },
    /// Node receipt correlation rejected a packet after synchronous byte
    /// consumption; its exact buffer was recycled or retained as reported.
    CorrelationRejected {
        /// Fixed-ledger correlation reason.
        reason: ReceiptCorrelationError,
        /// Stable exact buffer identity.
        buffer: IngressBufferId,
        /// Recycle fault retained internally, if exact return failed.
        recycle_fault: Option<NodeInterfaceIngressRecycleFault>,
    },
    /// Node processing completed; exact action and recycle pressure are
    /// retained inside the aggregate and reported by copy-only status.
    Processed(NodeInterfaceQueuedIngressProcessed),
    /// No queued packet or retained ingress work was observable.
    Idle,
}

impl NodeInterfaceIngressStep {
    /// Terminal exact-buffer status reported by this transition, including an
    /// initial processed or rejected ingress result.
    pub const fn terminal_recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        let fault = match self {
            Self::TerminalRecyclePending(fault) => Some(*fault),
            Self::RouteRejected { recycle_fault, .. }
            | Self::CorrelationRejected { recycle_fault, .. } => *recycle_fault,
            Self::Processed(processed) => processed.recycle_fault(),
            _ => None,
        };
        match fault {
            Some(fault) if fault.is_terminal() => Some(fault),
            _ => None,
        }
    }
}

struct PendingIngressRecycle {
    status: NodeInterfaceIngressRecycleFault,
    packet: SealedIngressPacket,
}

enum IngressRecycleRetention {
    Retryable(NodeInterfaceIngressRecycleFault),
    Terminal(NodeInterfaceIngressRecycleFault),
}

enum IngressActionRetention {
    Retryable(NodeInterfaceOrdinaryOfferError),
    Terminal(NodeInterfaceIngressActionFault),
}

impl NodeInterfaceOrdinaryOfferFailure {
    /// Scalar rejection reason.
    pub const fn reason(&self) -> NodeInterfaceOrdinaryOfferError {
        self.reason
    }

    /// Recover the unchanged action envelope and admission policy.
    pub fn into_parts(self) -> (NodeActions, OrdinaryRouterAdmission) {
        (self.actions, self.admission)
    }
}

/// Permanent synchronous owner of one node, the shared interface router, both
/// outbound coordinator families, per-actor permit services, and product
/// authorization policy.
///
/// The aggregate exposes narrow identity, submission, and status methods only.
/// Concrete interface actors retain their router and permit capabilities
/// outside this value and remain free to implement any transport.
#[must_use = "dropping the permanent aggregate abandons exact outbound owners"]
pub struct NodeInterfaceSupervisor<
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
> where
    M: RawMutex + 'static,
{
    node: NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
    router: OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
    data: DataRouterCoordinator<DATA_PACKET_BUFFERS>,
    ordinary: OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
    data_permits: [DataPermitServer<M>; SLOTS],
    ordinary_permits: [OrdinaryPermitServer<M>; SLOTS],
    policy: P,
    lane_cursor: usize,
    completion_fault: Option<CompletionFaultResidue>,
    owner_mismatch_fault: bool,
    pending_ingress_actions: Option<NodeInterfaceOrdinaryOfferFailure>,
    terminal_ingress_actions: Option<NodeInterfaceTerminalIngressActions>,
    pending_ingress_recycle: Option<PendingIngressRecycle>,
    terminal_ingress_recycle: Option<PendingIngressRecycle>,
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
>
    NodeInterfaceSupervisor<
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
    fn build_error(
        node: &NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
        data: &DataRouterCoordinator<DATA_PACKET_BUFFERS>,
        ordinary: &OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
    ) -> Option<NodeInterfaceSupervisorBuildError> {
        let node_scope = node.tx_owner_scope();
        if SLOTS == 0 {
            Some(NodeInterfaceSupervisorBuildError::ZeroInterfaceSlots)
        } else if data.owner_scope() != node_scope {
            Some(NodeInterfaceSupervisorBuildError::DataOwnerMismatch {
                node: node_scope,
                coordinator: data.owner_scope(),
            })
        } else if ordinary.owner_scope() != node_scope {
            Some(NodeInterfaceSupervisorBuildError::OrdinaryOwnerMismatch {
                node: node_scope,
                coordinator: ordinary.owner_scope(),
            })
        } else {
            None
        }
    }

    /// Begin constructing this aggregate by initializing its node directly in
    /// the final caller-owned destination.
    ///
    /// The returned stage exposes only that initialized node until
    /// [`NodeInterfaceSupervisorInit::finish`] completes every other field.
    /// If node validation fails, the destination remains uninitialized and may
    /// be reused for another attempt.
    #[allow(
        unsafe_code,
        clippy::too_many_arguments,
        reason = "audited projection delegates initialization of the final node field to node-core's in-place constructor"
    )]
    #[inline(never)]
    pub fn begin_node_in<'a>(
        destination: &'a mut MaybeUninit<Self>,
        identity: NodeIdentity,
        app_name: &str,
        aspects: &[&str],
        instance: NodeInstanceId,
        config: NodeConfig,
    ) -> Result<
        NodeInterfaceSupervisorInit<
            'a,
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
        >,
        NodeConstructionError,
    > {
        let supervisor = destination.as_mut_ptr();
        // SAFETY: `destination` is uniquely borrowed and uninitialized. A
        // projected `MaybeUninit<NodeCore>` has the exact node field's address
        // and layout without creating a reference to an uninitialized node.
        let node_destination = unsafe {
            &mut *core::ptr::addr_of_mut!((*supervisor).node).cast::<MaybeUninit<
                    NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
                >>()
        };
        NodeCore::new_in(
            node_destination,
            identity,
            app_name,
            aspects,
            instance,
            config,
        )?;
        Ok(NodeInterfaceSupervisorInit { destination })
    }

    /// Consume every permanent component after validating common node
    /// ownership. Every failure returns every non-copy input unchanged.
    #[allow(clippy::too_many_arguments, clippy::result_large_err)]
    pub fn try_new(
        node: NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
        fabric: &'static mut InterfaceFabric<M, SLOTS, QUEUE_DEPTH>,
        data: DataRouterCoordinator<DATA_PACKET_BUFFERS>,
        ordinary: OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
        data_permits: [DataPairedPermitHandoff<M>; SLOTS],
        ordinary_permits: [OrdinaryPairedPermitHandoff<M>; SLOTS],
        policy: P,
    ) -> Result<
        NodeInterfaceSupervisorBuildSuccess<
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
        >,
        NodeInterfaceSupervisorBuildFailure<
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
        >,
    > {
        if let Some(reason) = Self::build_error(&node, &data, &ordinary) {
            return Err(NodeInterfaceSupervisorBuildFailure {
                reason,
                node,
                fabric,
                data,
                ordinary,
                data_permits,
                ordinary_permits,
                policy,
            });
        }

        let (router, interface_actors) = fabric.split();
        let mut data_servers = core::array::from_fn(|_| None);
        let mut ordinary_servers = core::array::from_fn(|_| None);
        let mut data_pairs = data_permits.map(Some);
        let mut ordinary_pairs = ordinary_permits.map(Some);
        let mut interface_actors = interface_actors.map(Some);
        let actors = core::array::from_fn(|index| {
            let (data_node, data_permit) = data_pairs[index]
                .take()
                .expect("each validated DATA pair is split exactly once")
                .into_parts();
            let (ordinary_node, ordinary_permit) = ordinary_pairs[index]
                .take()
                .expect("each validated ordinary pair is split exactly once")
                .into_parts();
            data_servers[index] = Some(DataPermitServer::new(data_node));
            ordinary_servers[index] = Some(OrdinaryPermitServer::new(ordinary_node));
            NodeInterfaceActorPorts {
                interface: interface_actors[index]
                    .take()
                    .expect("each interface actor is moved exactly once"),
                data_permit,
                ordinary_permit,
            }
        });
        let data_permits = data_servers
            .map(|server| server.expect("every interface slot receives one DATA permit server"));
        let ordinary_permits = ordinary_servers.map(|server| {
            server.expect("every interface slot receives one ordinary permit server")
        });

        Ok(NodeInterfaceSupervisorBuildSuccess {
            supervisor: Self {
                node,
                router,
                data,
                ordinary,
                data_permits,
                ordinary_permits,
                policy,
                lane_cursor: 0,
                completion_fault: None,
                owner_mismatch_fault: false,
                pending_ingress_actions: None,
                terminal_ingress_actions: None,
                pending_ingress_recycle: None,
                terminal_ingress_recycle: None,
            },
            actors,
        })
    }

    /// Primary local Reticulum destination owned by this aggregate.
    pub fn destination_hash(&self) -> DestinationHash {
        self.node.destination_hash()
    }

    /// Compose one basic LXMF carrier with the node-owned identity and exact
    /// registered `lxmf.delivery` source.
    ///
    /// This read-only protocol operation does not acquire an interface,
    /// prepare an RNS packet, or consume a delivery receipt. The caller must
    /// durably accept the returned carrier before handing it and the opaque
    /// result to the dedicated opportunistic-LXMF submission path.
    pub fn prepare_basic_lxmf_into(
        &self,
        destination: &DestinationHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        output: &mut [u8],
    ) -> Result<PreparedBasicLxmf, PrepareBasicLxmfError> {
        self.node
            .prepare_basic_lxmf_into(destination, timestamp_unix_ms, title, content, output)
    }

    /// Compose one signed opportunistic LXMF carrier with a location snapshot.
    pub fn prepare_basic_lxmf_with_location_into(
        &self,
        destination: &DestinationHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        location: LxmfMessageLocation,
        output: &mut [u8],
    ) -> Result<PreparedBasicLxmf, PrepareBasicLxmfError> {
        self.node.prepare_basic_lxmf_with_location_into(
            destination,
            timestamp_unix_ms,
            title,
            content,
            location,
            output,
        )
    }

    /// Compose one complete direct-LXMF wire message with the node-owned
    /// identity and exact registered `lxmf.delivery` source.
    ///
    /// This read-only operation does not establish a Link, prepare a packet,
    /// or consume receipt capacity. The caller must durably accept the exact
    /// initialized wire prefix before beginning the direct-Link transaction.
    pub fn prepare_basic_direct_lxmf_into(
        &self,
        destination: &DestinationHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        output: &mut [u8],
    ) -> Result<PreparedBasicDirectLxmf, PrepareBasicLxmfError> {
        self.node.prepare_basic_direct_lxmf_into(
            destination,
            timestamp_unix_ms,
            title,
            content,
            output,
        )
    }

    /// Compose one complete signed direct-LXMF wire message with a location snapshot.
    pub fn prepare_basic_direct_lxmf_with_location_into(
        &self,
        destination: &DestinationHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        location: LxmfMessageLocation,
        output: &mut [u8],
    ) -> Result<PreparedBasicDirectLxmf, PrepareBasicLxmfError> {
        self.node.prepare_basic_direct_lxmf_with_location_into(
            destination,
            timestamp_unix_ms,
            title,
            content,
            location,
            output,
        )
    }

    /// Copy a public key learned for one announced Reticulum destination.
    ///
    /// The permanent aggregate retains sole mutable node ownership. This
    /// fixed-size read-only query lets an application worker verify an LXMF
    /// source without exposing the inner node or borrowing native storage.
    pub fn recall_identity(&self, destination: &DestinationHash) -> Option<[u8; 64]> {
        self.node.recall_identity(destination)
    }

    /// Derive the canonical remote `rnstransport.probe` destination from any
    /// announce-known destination owned by that retained identity.
    pub fn proof_probe_destination_for(
        &self,
        announced_destination: &DestinationHash,
    ) -> Option<DestinationHash> {
        self.node.proof_probe_destination_for(announced_destination)
    }

    /// Alias an authenticated retained peer identity under its canonical
    /// `rnstransport.probe` destination without creating a path.
    pub fn prepare_proof_probe_destination_for(
        &mut self,
        announced_destination: &DestinationHash,
    ) -> Result<DestinationHash, ProofProbeIdentityAliasError> {
        self.node
            .prepare_proof_probe_destination_for(announced_destination)
    }

    /// Recall an announce-learned identity only for its exact
    /// identity-derived `lxmf.delivery` destination.
    pub fn recall_lxmf_delivery_identity(&self, destination: &DestinationHash) -> Option<[u8; 64]> {
        self.node.recall_lxmf_delivery_identity(destination)
    }

    /// Register one stable interface identity offline in a vacant actor slot.
    ///
    /// Only that slot's lifecycle capability can make the interface eligible
    /// by completing a generation-bound Ready exchange.
    pub fn register_interface(
        &mut self,
        queue: InterfaceQueueId,
        interface: PacketInterfaceId,
        properties: InterfaceProperties,
    ) -> Result<InterfaceDescriptor, InterfaceRegistrationError> {
        self.router.register(queue, interface, properties, false)
    }

    /// Administratively exclude one current interface from newly routed work.
    ///
    /// This disable-only surface cannot bypass actor-owned Ready reporting.
    pub fn disable_interface(
        &mut self,
        lease: InterfaceLease,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.router.set_online(lease, false)
    }

    /// Confirm the bounded synchronous egress-handoff interval for one
    /// token-bearing ordinary protocol packet.
    ///
    /// This edge is successful interface/router acceptance, not physical
    /// transmission completion. The first matching serialized fan-out hop
    /// wins; callers use the `first_dispatch` field on the corresponding
    /// [`OrdinaryRouterStep::Routed`] transition to distinguish a rejected
    /// first confirmation from an expected false result on a later hop.
    pub fn confirm_outbound_protocol(
        &mut self,
        token: OutboundProtocolToken,
        interface: PacketInterfaceId,
        interval: OutboundDispatchInterval,
    ) -> bool {
        self.node
            .confirm_outbound_protocol(token, interface, interval)
    }

    /// Test-only generation change used to exercise stale completion recovery.
    ///
    /// Production code cannot safely reuse a generation while native RNS path
    /// state still refers only to the stable interface ID.
    #[cfg(test)]
    fn reconfigure_interface(
        &mut self,
        lease: InterfaceLease,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.router.reconfigure(lease, properties, online)
    }

    /// Prepare one destination-DATA packet using the router's authoritative
    /// eligible-interface snapshot.
    pub fn try_prepare_data<R>(
        &mut self,
        request: DataRouterPrepareRequest<'_>,
        now: MonotonicMillis,
        rng: &mut R,
    ) -> NodeInterfaceDataPrepareResult
    where
        R: RngCore + CryptoRng,
    {
        if let Some(fault) = self.fault() {
            return NodeInterfaceDataPrepareResult::Fault(fault);
        }
        NodeInterfaceDataPrepareResult::Coordinator(self.data.try_prepare_data(
            &mut self.node,
            &self.router,
            request,
            now,
            rng,
        ))
    }

    /// Prepare one exact durable LXMF wire message through the dedicated
    /// opportunistic Header-1 destination-DATA path.
    pub fn try_prepare_rehydrated_opportunistic_lxmf<R>(
        &mut self,
        request: DataRouterPrepareRequest<'_>,
        now: MonotonicMillis,
        rng: &mut R,
    ) -> NodeInterfaceDataPrepareResult
    where
        R: RngCore + CryptoRng,
    {
        if let Some(fault) = self.fault() {
            return NodeInterfaceDataPrepareResult::Fault(fault);
        }
        NodeInterfaceDataPrepareResult::Coordinator(
            self.data.try_prepare_rehydrated_opportunistic_lxmf(
                &mut self.node,
                &self.router,
                request,
                now,
                rng,
            ),
        )
    }

    /// Prepare one exact durable LXMF wire message as application DATA on an
    /// already active, product-retained Link.
    ///
    /// The node revalidates the opaque Link handle, message destination, Link
    /// state, and bound interface. The DATA coordinator retains the resulting
    /// exact packet owner through the same routing, completion, and recovery
    /// lifecycle as opportunistic destination DATA.
    pub fn try_prepare_rehydrated_direct_lxmf<R>(
        &mut self,
        link: LinkHandle,
        request: DataRouterPrepareRequest<'_>,
        now: MonotonicMillis,
        rng: &mut R,
    ) -> NodeInterfaceDataPrepareResult
    where
        R: RngCore + CryptoRng,
    {
        if let Some(fault) = self.fault() {
            return NodeInterfaceDataPrepareResult::Fault(fault);
        }
        NodeInterfaceDataPrepareResult::Coordinator(self.data.try_prepare_rehydrated_direct_lxmf(
            &mut self.node,
            &self.router,
            link,
            request,
            now,
            rng,
        ))
    }

    /// Offer one complete native ordinary-action envelope.
    // The allocation-backed action envelope must remain inline on failure;
    // this portable boundary cannot box or discard its exact owner.
    #[allow(clippy::result_large_err)]
    pub fn try_offer_actions(
        &mut self,
        actions: NodeActions,
        admission: OrdinaryRouterAdmission,
    ) -> Result<(), NodeInterfaceOrdinaryOfferFailure> {
        if let Some(fault) = self.fault() {
            return Err(NodeInterfaceOrdinaryOfferFailure {
                reason: NodeInterfaceOrdinaryOfferError::Fault(fault),
                actions,
                admission,
            });
        }
        self.ordinary.try_offer_actions(actions, admission).map_err(
            |failure: OrdinaryRouterOfferFailure| {
                let reason = failure.reason();
                let (actions, admission) = failure.into_parts();
                NodeInterfaceOrdinaryOfferFailure {
                    reason: NodeInterfaceOrdinaryOfferError::Coordinator(reason),
                    actions,
                    admission,
                }
            },
        )
    }

    /// Offer one critical ordinary envelope ahead of an earlier unadmitted input.
    ///
    /// Admitted batches and actor-owned packets remain non-preemptible. The
    /// caller must retain the optional exact displaced envelope before
    /// performing any other coordinator transition.
    #[allow(clippy::result_large_err)]
    pub fn try_offer_priority_actions(
        &mut self,
        actions: NodeActions,
        admission: OrdinaryRouterAdmission,
    ) -> Result<Option<OrdinaryRouterDisplacedActions>, NodeInterfaceOrdinaryOfferFailure> {
        if let Some(fault) = self.fault() {
            return Err(NodeInterfaceOrdinaryOfferFailure {
                reason: NodeInterfaceOrdinaryOfferError::Fault(fault),
                actions,
                admission,
            });
        }
        self.ordinary
            .try_offer_priority_actions(actions, admission)
            .map_err(|failure: OrdinaryRouterOfferFailure| {
                let reason = failure.reason();
                let (actions, admission) = failure.into_parts();
                NodeInterfaceOrdinaryOfferFailure {
                    reason: NodeInterfaceOrdinaryOfferError::Coordinator(reason),
                    actions,
                    admission,
                }
            })
    }

    /// Process at most one router-observed sealed ingress packet.
    ///
    /// A retryable exact buffer-return failure is retried first. A terminal
    /// buffer residue instead gates ingress until firmware explicitly takes
    /// its exact packet owner. Retained action work follows; only when all are
    /// clear can another actor packet be dequeued. Packet bytes are consumed
    /// synchronously, then the exact sealed owner is recycled before action
    /// admission is attempted, so ordinary backpressure cannot starve the
    /// actor's fixed RX pool.
    /// Permanent node tasks should fairly interleave this bounded lane with
    /// [`Self::step`].
    pub fn step_ingress<R>(
        &mut self,
        rns_now: MonotonicSeconds,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceIngressStep
    where
        R: RngCore + CryptoRng,
    {
        self.step_ingress_at(
            rns_now,
            MonotonicInstant::from_secs(rns_now.get()),
            admission,
            rng,
        )
    }

    /// Precise Link-clock variant of [`Self::step_ingress`].
    pub fn step_ingress_at<R>(
        &mut self,
        rns_now: MonotonicSeconds,
        link_now: MonotonicInstant,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceIngressStep
    where
        R: RngCore + CryptoRng,
    {
        if let Some(pending) = self.pending_ingress_recycle.take() {
            let buffer = pending.status.buffer;
            return match self.router.try_return_ingress_buffer(pending.packet) {
                Ok(()) => NodeInterfaceIngressStep::RecycleRetried(buffer),
                Err(failure) => match self.retain_ingress_recycle_failure(failure) {
                    IngressRecycleRetention::Retryable(status) => {
                        NodeInterfaceIngressStep::RecycleBackpressured(status)
                    }
                    IngressRecycleRetention::Terminal(status) => {
                        NodeInterfaceIngressStep::TerminalRecyclePending(status)
                    }
                },
            };
        }

        if let Some(terminal) = self.terminal_ingress_recycle.as_ref() {
            return NodeInterfaceIngressStep::TerminalRecyclePending(terminal.status);
        }

        if let Some(terminal) = self.terminal_ingress_actions.as_ref() {
            return NodeInterfaceIngressStep::TerminalActionsPending(terminal.fault());
        }

        if let Some(failure) = self.pending_ingress_actions.take() {
            let (actions, retained_admission) = failure.into_parts();
            return match self.try_offer_actions(actions, retained_admission) {
                Ok(()) => NodeInterfaceIngressStep::RetainedActionsAdmitted,
                Err(failure) => match self.retain_ingress_action_failure(failure) {
                    IngressActionRetention::Retryable(reason) => {
                        NodeInterfaceIngressStep::ActionsBackpressured(reason)
                    }
                    IngressActionRetention::Terminal(fault) => {
                        NodeInterfaceIngressStep::TerminalActionsPending(fault)
                    }
                },
            };
        }

        match self.router.try_receive_ingress() {
            Ok(Some(ingress)) => {
                let interface = ingress.interface();
                let signal = ingress.signal();
                let properties = ingress.descriptor().properties();
                let broadcast_scope = match properties.topology() {
                    InterfaceTopology::SharedMedium => IngressBroadcastScope::SharedMedium,
                    InterfaceTopology::PointToPoint => IngressBroadcastScope::PointToPoint,
                };
                let eligible = self.router.eligible_interfaces();
                let allowed = self.router.announce_egress_interfaces(interface);
                let default_egress =
                    eligible
                        .ok()
                        .and_then(|eligible| match properties.topology() {
                            InterfaceTopology::SharedMedium => Some(eligible),
                            InterfaceTopology::PointToPoint => eligible.without(interface),
                        });
                let mut broadcast_policy = IngressBroadcastPolicy::new(broadcast_scope);
                match (allowed, default_egress) {
                    (Ok(allowed), Some(default_egress)) if allowed == default_egress => {}
                    (Ok(allowed), _) => {
                        broadcast_policy = broadcast_policy
                            .with_announce_egress(IngressAnnounceEgress::from_bits(allowed.bits()));
                    }
                    (Err(_), _) => {
                        // A stale/out-of-profile registry cannot safely widen
                        // announce propagation. Suppress only this exact
                        // ingress-derived announce; ordinary traffic keeps its
                        // existing route contract and registry diagnostics.
                        broadcast_policy =
                            broadcast_policy.with_announce_egress(IngressAnnounceEgress::empty());
                    }
                }
                let packet = ingress.into_packet();
                let ingress_observation = PacketIngressObservation::remote(
                    interface,
                    signal.map(|signal| {
                        PacketSignalObservation::new(signal.rssi_dbm(), signal.snr_db())
                    }),
                );
                let result = self.node.ingest_at_with_broadcast_policy_observed(
                    packet.as_ref(),
                    rns_now,
                    link_now,
                    ingress_observation,
                    broadcast_policy,
                    rng,
                );
                self.finish_queued_ingress(result, packet, admission)
            }
            Ok(None) => NodeInterfaceIngressStep::Idle,
            Err(failure) => {
                let reason = failure.reason();
                let packet = failure.into_packet();
                let buffer = packet.id();
                let recycle_fault = self.recycle_ingress_packet(packet);
                NodeInterfaceIngressStep::RouteRejected {
                    reason,
                    buffer,
                    recycle_fault,
                }
            }
        }
    }

    fn finish_queued_ingress(
        &mut self,
        result: Result<IngressReport, ReceiptCorrelationError>,
        packet: SealedIngressPacket,
        admission: OrdinaryRouterAdmission,
    ) -> NodeInterfaceIngressStep {
        let buffer = packet.id();
        let recycle_fault = self.recycle_ingress_packet(packet);
        match result {
            Err(reason) => NodeInterfaceIngressStep::CorrelationRejected {
                reason,
                buffer,
                recycle_fault,
            },
            Ok(mut report) => {
                let actions = mem::take(&mut report.actions);
                let (actions_backpressured, terminal_action_fault) =
                    if Self::actions_are_empty(&actions) {
                        (None, None)
                    } else {
                        match self.try_offer_actions(actions, admission) {
                            Ok(()) => (None, None),
                            Err(failure) => match self.retain_ingress_action_failure(failure) {
                                IngressActionRetention::Retryable(reason) => (Some(reason), None),
                                IngressActionRetention::Terminal(fault) => (None, Some(fault)),
                            },
                        }
                    };
                NodeInterfaceIngressStep::Processed(NodeInterfaceQueuedIngressProcessed {
                    buffer,
                    report,
                    actions_backpressured,
                    terminal_action_fault,
                    recycle_fault,
                })
            }
        }
    }

    fn retain_ingress_action_failure(
        &mut self,
        failure: NodeInterfaceOrdinaryOfferFailure,
    ) -> IngressActionRetention {
        match failure.reason() {
            reason @ NodeInterfaceOrdinaryOfferError::Coordinator(
                OrdinaryRouterOfferError::Busy(_),
            ) => {
                debug_assert!(self.pending_ingress_actions.is_none());
                debug_assert!(self.terminal_ingress_actions.is_none());
                self.pending_ingress_actions = Some(failure);
                IngressActionRetention::Retryable(reason)
            }
            NodeInterfaceOrdinaryOfferError::Fault(fault) => self.retain_terminal_ingress_actions(
                NodeInterfaceIngressActionFault::Aggregate(fault),
                failure,
            ),
            NodeInterfaceOrdinaryOfferError::Coordinator(OrdinaryRouterOfferError::Disabled(
                fault,
            )) => self.retain_terminal_ingress_actions(
                NodeInterfaceIngressActionFault::CoordinatorDisabled(fault),
                failure,
            ),
            NodeInterfaceOrdinaryOfferError::Coordinator(
                OrdinaryRouterOfferError::EnvelopeExceedsPool {
                    packet_count,
                    limit,
                },
            ) => self.retain_terminal_ingress_actions(
                NodeInterfaceIngressActionFault::EnvelopeExceedsPool {
                    packet_count,
                    limit,
                },
                failure,
            ),
        }
    }

    fn retain_terminal_ingress_actions(
        &mut self,
        fault: NodeInterfaceIngressActionFault,
        failure: NodeInterfaceOrdinaryOfferFailure,
    ) -> IngressActionRetention {
        debug_assert!(self.pending_ingress_actions.is_none());
        debug_assert!(self.terminal_ingress_actions.is_none());
        let (actions, admission) = failure.into_parts();
        self.terminal_ingress_actions = Some(NodeInterfaceTerminalIngressActions {
            fault,
            actions,
            admission,
        });
        IngressActionRetention::Terminal(fault)
    }

    fn recycle_ingress_packet(
        &mut self,
        packet: SealedIngressPacket,
    ) -> Option<NodeInterfaceIngressRecycleFault> {
        match self.router.try_return_ingress_buffer(packet) {
            Ok(()) => None,
            Err(failure) => {
                let status = match self.retain_ingress_recycle_failure(failure) {
                    IngressRecycleRetention::Retryable(status)
                    | IngressRecycleRetention::Terminal(status) => status,
                };
                Some(status)
            }
        }
    }

    fn retain_ingress_recycle_failure(
        &mut self,
        failure: IngressBufferReturnFailure,
    ) -> IngressRecycleRetention {
        let reason = failure.reason();
        self.retain_ingress_recycle(reason, failure.into_packet())
    }

    fn retain_ingress_recycle(
        &mut self,
        reason: IngressBufferReturnError,
        packet: SealedIngressPacket,
    ) -> IngressRecycleRetention {
        let status = NodeInterfaceIngressRecycleFault {
            reason,
            buffer: packet.id(),
        };
        let retained = PendingIngressRecycle { status, packet };
        if status.is_retryable() {
            debug_assert!(self.pending_ingress_recycle.is_none());
            debug_assert!(self.terminal_ingress_recycle.is_none());
            self.pending_ingress_recycle = Some(retained);
            IngressRecycleRetention::Retryable(status)
        } else {
            debug_assert!(self.pending_ingress_recycle.is_none());
            debug_assert!(self.terminal_ingress_recycle.is_none());
            self.terminal_ingress_recycle = Some(retained);
            IngressRecycleRetention::Terminal(status)
        }
    }

    fn actions_are_empty(actions: &NodeActions) -> bool {
        actions.is_empty()
    }

    /// Run one protocol timer tick and immediately offer every returned action
    /// to the ordinary coordinator.
    pub fn tick<R>(
        &mut self,
        now: MonotonicSeconds,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceTickResult
    where
        R: RngCore + CryptoRng,
    {
        self.tick_at(now, MonotonicInstant::from_secs(now.get()), admission, rng)
    }

    /// Precise Link-clock variant of [`Self::tick`].
    pub fn tick_at<R>(
        &mut self,
        now: MonotonicSeconds,
        link_now: MonotonicInstant,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceTickResult
    where
        R: RngCore + CryptoRng,
    {
        let mut report = self.node.tick_at(now, link_now, rng);
        let actions = mem::take(&mut report.actions);
        if Self::actions_are_empty(&actions) {
            return NodeInterfaceTickResult::Accepted(NodeInterfaceTickAccepted { report });
        }
        match self.try_offer_actions(actions, admission) {
            Ok(()) => NodeInterfaceTickResult::Accepted(NodeInterfaceTickAccepted { report }),
            Err(offer) => {
                NodeInterfaceTickResult::ActionOfferRejected(NodeInterfaceTickActionFailure {
                    report,
                    offer,
                })
            }
        }
    }

    /// Bind one local announce destination and its native retransmits to one interface.
    pub fn set_local_announce_interface_target(
        &mut self,
        destination: &DestinationHash,
        interface: PacketInterfaceId,
    ) -> Result<(), LocalDestinationAnnounceTargetError> {
        self.node
            .set_local_announce_interface_target(destination, interface)
    }

    /// Queue one signed local announce in the owned protocol node.
    pub fn queue_announce<R>(
        &mut self,
        app_data: Option<&[u8]>,
        emitted_at: AnnounceEmissionTime,
        rng: &mut R,
    ) -> Result<(), AnnounceAdmissionError>
    where
        R: RngCore + CryptoRng,
    {
        self.node.queue_announce(app_data, emitted_at, rng)
    }

    /// Queue one signed local announce for a registered destination in the
    /// owned protocol node.
    pub fn queue_announce_for<R>(
        &mut self,
        destination: &DestinationHash,
        app_data: Option<&[u8]>,
        emitted_at: AnnounceEmissionTime,
        rng: &mut R,
    ) -> Result<(), AnnounceAdmissionError>
    where
        R: RngCore + CryptoRng,
    {
        self.node
            .queue_announce_for(destination, app_data, emitted_at, rng)
    }

    /// Flush every ready local announce and immediately offer the exact action
    /// envelope to the ordinary coordinator.
    pub fn flush_announces<R>(
        &mut self,
        now: MonotonicSeconds,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceAnnounceFlushResult
    where
        R: RngCore,
    {
        let actions = self.node.flush_announces(now, rng);
        if Self::actions_are_empty(&actions) {
            return NodeInterfaceAnnounceFlushResult::Accepted;
        }
        match self.try_offer_actions(actions, admission) {
            Ok(()) => NodeInterfaceAnnounceFlushResult::Accepted,
            Err(failure) => NodeInterfaceAnnounceFlushResult::ActionOfferRejected(failure),
        }
    }

    /// Build one tagged Reticulum path request as a transport-neutral ordinary
    /// action envelope. Admission remains a separate exact-owner operation so
    /// callers can retain the unchanged request under router backpressure.
    pub fn request_path<R>(
        &self,
        destination: &DestinationHash,
        rng: &mut R,
    ) -> Result<NodeActions, PathRequestError>
    where
        R: RngCore + CryptoRng,
    {
        self.node.request_path(destination, rng)
    }

    /// Whether the native protocol owner currently retains a usable identity
    /// and route entry for one destination.
    ///
    /// This read-only preflight lets delivery policy request a path before
    /// attempting Link construction; the authoritative Link initiator still
    /// revalidates the route when it mutates protocol state.
    pub fn has_path(&self, destination: &DestinationHash) -> bool {
        self.node.has_path(destination)
    }

    /// Remove one exact retained native path without changing identity or Link
    /// ownership.
    ///
    /// The durable submission runtime uses this only for destination-DATA
    /// receipt timeouts. Direct Link-DATA timeouts retain their separate Link
    /// retirement transaction.
    pub fn remove_retained_path(&mut self, destination: &DestinationHash) -> bool {
        self.node.remove_retained_path(destination)
    }

    /// Whether one retained native path resolves against the authoritative
    /// currently-online interface snapshot.
    ///
    /// A direct path without an exact ingress binding remains usable when at
    /// least one interface is online; a path bound to an offline interface is
    /// retained but cannot start a new Link yet.
    pub fn has_usable_path(&self, destination: &DestinationHash) -> bool {
        let Some(target) = self.node.retained_path_target(destination) else {
            return false;
        };
        self.router
            .eligible_interfaces()
            .ok()
            .and_then(|eligible| TxRoutePlan::resolve(target, eligible).ok())
            .is_some()
    }

    /// Replace caller-owned storage with a complete route diagnostics snapshot.
    ///
    /// Every native route is copied under one immutable node borrow, enriched
    /// against one current interface-registry view, and then sorted by complete
    /// destination hash. The operation allocates nothing and copies at most the
    /// node's `PATHS` const-generic capacity.
    ///
    /// `captured_at` is an observation time, not a route-table revision.
    pub fn snapshot_route_diagnostics(
        &self,
        captured_at: MonotonicSeconds,
        snapshot: &mut RouteDiagnosticsSnapshot<PATHS>,
    ) {
        snapshot.reset(captured_at);
        let eligible = self.router.eligible_interfaces().ok();
        let registry = self.router.registry();
        let visited = self.node.visit_retained_routes(|route| {
            let target = match route.received_on() {
                Some(interface) => TxTarget::Only(interface),
                None => TxTarget::All,
            };
            let usable = eligible
                .and_then(|interfaces| TxRoutePlan::resolve(target, interfaces).ok())
                .is_some();
            let resolution = match route.received_on() {
                Some(interface) => RouteInterfaceResolution::Exact {
                    interface,
                    current: registry.descriptor(interface),
                    usable,
                },
                None => RouteInterfaceResolution::BroadcastFallback { usable },
            };
            let inserted = snapshot.push(RouteDiagnosticsEntry { route, resolution });
            debug_assert!(
                inserted,
                "native route count cannot exceed matching PATHS snapshot"
            );
        });
        debug_assert_eq!(visited, snapshot.len);
        debug_assert_eq!(visited, self.node.retained_route_count());
        snapshot.sort_by_destination();
    }

    /// Number of routes currently retained by the native RNS path table.
    pub fn retained_route_count(&self) -> usize {
        self.node.retained_route_count()
    }

    /// Hop count retained with one known destination path.
    pub fn retained_path_hops(&self, destination: &DestinationHash) -> Option<u8> {
        self.node.retained_path_hops(destination)
    }

    /// Next-hop transport identity retained for one known destination path.
    pub fn retained_path_next_hop(&self, destination: &DestinationHash) -> Option<[u8; 16]> {
        self.node.retained_path_next_hop(destination)
    }

    /// Conservative first-hop serialization allowance for the retained route.
    ///
    /// Reticulum adds one full interface-MTU transmission time to its base
    /// Link-establishment window when bitrate metadata is available. Before a
    /// serialized fan-out has selected its first successful interface, use the
    /// slowest advertised candidate so a rejected earlier hop cannot shorten
    /// the deadline for a later transport. Unknown bitrate follows Reticulum's
    /// zero-additional-time fallback.
    pub fn retained_path_first_hop_serialization_ms(
        &self,
        destination: &DestinationHash,
    ) -> Option<u64> {
        let target = self.node.retained_path_target(destination)?;
        let eligible = self.router.eligible_interfaces().ok()?;
        let candidates = TxRoutePlan::resolve(target, eligible)
            .ok()?
            .remaining()
            .bits();
        let mut maximum_ms = 0_u64;
        for index in 0_u8..64 {
            if candidates & (1_u64 << index) == 0 {
                continue;
            }
            let descriptor = self
                .router
                .registry()
                .descriptor(PacketInterfaceId::new(index))?;
            let Some(bitrate) = descriptor.advertised_bitrate() else {
                continue;
            };
            let packet_bits = u64::from(descriptor.logical_mtu().get()) * 8;
            let bits_per_second = u64::from(bitrate.get());
            let serialization_ms = packet_bits
                .saturating_mul(1_000)
                .saturating_add(bits_per_second - 1)
                / bits_per_second;
            maximum_ms = maximum_ms.max(serialization_ms);
        }
        Some(maximum_ms)
    }

    /// Whether the shared native owned-Link table can currently retain one
    /// additional locally initiated or inbound responder session.
    ///
    /// This is a scheduler hint only; [`Self::initiate_link`] remains the
    /// authoritative, transactional admission boundary.
    pub fn can_initiate_link(&self) -> bool {
        self.node.can_initiate_link()
    }

    /// Initiate one product-owned Link and return its exact ordinary action
    /// envelope plus an opaque handle for later state observation and DATA.
    ///
    /// The caller must preserve the returned action envelope until it is
    /// durably admitted or explicitly rejected. Link state remains owned by
    /// the aggregate and is accessible only through [`Self::link_state`].
    pub fn initiate_link<R>(
        &mut self,
        destination: &DestinationHash,
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> Result<(NodeActions, LinkHandle), InitiateLinkError>
    where
        R: RngCore + CryptoRng,
    {
        self.node.initiate_link(destination, now, rng)
    }

    /// Prepare one anonymous direct Link request under permanent node ownership.
    ///
    /// An outer error is an aggregate fault observed before RNG or native node
    /// mutation. The inner result is the node preparation outcome. On success,
    /// the returned owner keeps the exact ordinary action envelope paired with
    /// only a copyable request handle; native dispatch authorities remain in
    /// the aggregate.
    // The exact aggregate fault is copy-only but includes coordinator
    // correlation detail; boxing is unavailable at this no_std boundary.
    #[allow(clippy::result_large_err)]
    pub fn prepare_anonymous_request<R>(
        &mut self,
        link: LinkHandle,
        path: &str,
        requested_at_secs: f64,
        rng: &mut R,
    ) -> Result<Result<PreparedRequestActions, PrepareRequestError>, NodeInterfaceSupervisorFault>
    where
        R: RngCore + CryptoRng,
    {
        if let Some(fault) = self.fault() {
            return Err(fault);
        }
        Ok(self
            .node
            .prepare_anonymous_request(link, path, requested_at_secs, rng))
    }

    /// Prepare one direct response for an exact event-carried request binding.
    ///
    /// An outer error is an aggregate fault observed before node allocation,
    /// entropy consumption or native Link mutation. The inner result is the
    /// product-owned response-preparation outcome. Success owns exactly one
    /// ordinary packet action and no receipt, token or cancellation authority.
    // The exact aggregate fault is copy-only but includes coordinator
    // correlation detail; boxing is unavailable at this no_std boundary.
    #[allow(clippy::result_large_err)]
    pub fn prepare_response_actions<R>(
        &mut self,
        binding: &ApplicationLinkBinding,
        request: &[u8; 16],
        data: &[u8],
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> Result<Result<NodeActions, PrepareResponseError>, NodeInterfaceSupervisorFault>
    where
        R: RngCore + CryptoRng,
    {
        if let Some(fault) = self.fault() {
            return Err(fault);
        }
        Ok(self
            .node
            .prepare_response_actions(binding, request, data, now, rng))
    }

    /// Apply one ordinary-router dispatch decision to an exact request.
    ///
    /// This authority transition deliberately remains available after an
    /// aggregate fault: a prepared request may already have reached an
    /// interface before the unrelated fault was observed, and its native
    /// owner must still be discharged exactly.
    pub fn confirm_request_dispatch(
        &mut self,
        handle: RequestHandle,
        sent_at: MonotonicSeconds,
        first_dispatch: bool,
    ) -> Result<RequestDispatchConfirmation, RequestDispatchError> {
        self.node
            .confirm_request_dispatch(handle, sent_at, first_dispatch)
    }

    /// Reclaim one exact request from every native dispatch owner.
    ///
    /// This phase-agnostic operation remains available after an aggregate
    /// fault and is reserved for invariant recovery. Every result guarantees
    /// that no exact native request authority remains.
    pub fn reconcile_request_dispatch(
        &mut self,
        handle: RequestHandle,
    ) -> RequestDispatchReconciliation {
        self.node.reconcile_request_dispatch(handle)
    }

    /// Cancel only an exact request that still awaits first dispatch.
    pub fn cancel_prepared_request(
        &mut self,
        handle: RequestHandle,
    ) -> Result<(), RequestDispatchError> {
        self.node.cancel_prepared_request(handle)
    }

    /// Cancel only an exact request whose first dispatch started its timeout.
    pub fn cancel_confirmed_request(
        &mut self,
        handle: RequestHandle,
    ) -> Result<(), RequestDispatchError> {
        self.node.cancel_confirmed_request(handle)
    }

    /// Observe the current state of one opaque product-retained Link.
    pub fn link_state(&self, link: LinkHandle) -> Option<LinkState> {
        self.node.link_state(link)
    }

    /// Whether one active Link is bound to an interface that is currently
    /// eligible in the authoritative product registry.
    ///
    /// An active Link on an offline interface remains retained for possible
    /// later reuse but is not compatible with the current `Auto` send.
    pub fn link_is_usable(&self, link: LinkHandle) -> bool {
        if self.node.link_state(link) != Some(LinkState::Active) {
            return false;
        }
        let Some(interface) = self.node.link_interface(link) else {
            return false;
        };
        self.router
            .eligible_interfaces()
            .is_ok_and(|eligible| eligible.contains(interface))
    }

    /// Whether one exact Link has an active or terminal DATA attempt that has
    /// not yet completed its durable acknowledgement lifecycle.
    pub fn link_has_unacknowledged_attempt(&self, link: LinkHandle) -> bool {
        self.node.link_has_unacknowledged_attempt(link)
    }

    /// Abort a Link only while native state proves it is not established.
    ///
    /// Returns `false` for an unknown, active, stale, or already closed Link;
    /// the caller cannot use this narrow recovery surface to tear down a
    /// reusable established session.
    pub fn abort_unestablished_link(&mut self, link: LinkHandle) -> bool {
        self.node.abort_unestablished_link(link)
    }

    /// Close one exact active or stale product Link and return its ordinary
    /// authenticated teardown actions.
    ///
    /// Local Link state is removed before this method returns. The caller must
    /// retain every non-empty action envelope until ordinary-router admission
    /// succeeds or enters the existing fail-closed recovery path.
    pub fn close_link<R>(&mut self, link: LinkHandle, rng: &mut R) -> NodeActions
    where
        R: RngCore + CryptoRng,
    {
        self.node.close_link(link, rng)
    }

    /// Atomically drain one ready non-packet action envelope into a
    /// caller-provided, transport-neutral application-event owner.
    ///
    /// If the owner is full or the complete batch cannot fit, the ordinary
    /// coordinator immediately recovers the exact unchanged `NodeActions`
    /// allocation. Existing packet routes, completions, and lifecycle work
    /// remain independently progressable through [`Self::step`].
    pub fn drain_application_events(
        &mut self,
        owner: &mut ApplicationEventOwner<'_>,
    ) -> NodeInterfaceApplicationEventDrain {
        match self.ordinary.drain_application_events(owner) {
            Ok(Some(report)) => NodeInterfaceApplicationEventDrain::Drained(report),
            Ok(None) => NodeInterfaceApplicationEventDrain::Idle,
            Err(reason) => NodeInterfaceApplicationEventDrain::Retained(reason),
        }
    }

    /// Whether an ordinary envelope still owns application events upstream of
    /// the product event consumer.
    pub fn ordinary_application_events_pending(&self) -> bool {
        self.ordinary.application_events_pending()
    }

    /// Kind of exact ordinary input or packet residue retained by a permanent
    /// coordinator fault.
    pub fn ordinary_fault_residue_kind(&self) -> Option<OrdinaryRouterFaultResidueKind> {
        self.ordinary.fault_residue_kind()
    }

    /// Take one recoverably rejected ordinary envelope.
    pub fn take_rejected_actions(&mut self) -> Option<OrdinaryRouterRejectedActions> {
        self.ordinary.take_rejected_actions()
    }

    /// Perform DATA owner maintenance once, apply at most one pending actor
    /// lifecycle request as a pre-routing barrier, then select at most one
    /// useful transition by bounded round-robin scan across orchestration
    /// lanes.
    ///
    /// Idle, pressured, or disabled lanes never stop the scan. In particular,
    /// a coordinator-local fault does not prevent an already-issued actor
    /// request from reaching that coordinator's forced-denial authorization
    /// path through its permit server.
    pub fn step(&mut self, now: MonotonicMillis) -> NodeInterfaceSupervisorPass {
        let owner_was_mismatched = self.owner_mismatch_fault;
        let maintenance = self.data.maintain_tx(&mut self.node, now);
        if maintenance.is_err() {
            self.owner_mismatch_fault = true;
            if !owner_was_mismatched {
                return NodeInterfaceSupervisorPass {
                    maintenance,
                    transition: NodeInterfaceSupervisorTransition::Fault(
                        NodeInterfaceSupervisorFault::DataOwnerMismatch,
                    ),
                };
            }
        }

        if let Some(lifecycle) = self.router.try_process_lifecycle() {
            return NodeInterfaceSupervisorPass {
                maintenance,
                transition: NodeInterfaceSupervisorTransition::Lifecycle(lifecycle),
            };
        }

        if maintenance.is_err() {
            return NodeInterfaceSupervisorPass {
                maintenance,
                transition: NodeInterfaceSupervisorTransition::Fault(
                    NodeInterfaceSupervisorFault::DataOwnerMismatch,
                ),
            };
        }

        let lane_count = 3 + (2 * SLOTS);
        for offset in 0..lane_count {
            let lane = (self.lane_cursor + offset) % lane_count;
            let transition = match lane {
                0 => self.step_completion_lane(),
                1 => {
                    let fault_before = self.data.fault();
                    let step = self.data.step(&mut self.node, &mut self.router, now);
                    if matches!(step, DataRouterStep::OwnerMismatch) {
                        self.owner_mismatch_fault = true;
                        Some(NodeInterfaceSupervisorTransition::Fault(
                            NodeInterfaceSupervisorFault::DataOwnerMismatch,
                        ))
                    } else {
                        (step.progressed()
                            || (fault_before.is_none() && self.data.fault().is_some()))
                        .then_some(NodeInterfaceSupervisorTransition::Data(step))
                    }
                }
                2 => {
                    let fault_before = self.ordinary.fault();
                    let step = self.ordinary.step(&mut self.router, now);
                    (step.progressed()
                        || (fault_before.is_none() && self.ordinary.fault().is_some()))
                    .then_some(NodeInterfaceSupervisorTransition::Ordinary(step))
                }
                lane if lane < 3 + SLOTS => {
                    let actor = lane - 3;
                    let phase_before = self.data_permits[actor].phase();
                    let step = self.data_permits[actor].step(
                        &mut self.data,
                        &mut self.node,
                        now,
                        &mut self.policy,
                    );
                    (matches!(step, DataPermitServerStep::Advanced)
                        || (phase_before != DataPermitServerPhase::Disabled
                            && matches!(
                                step,
                                DataPermitServerStep::Disabled(_)
                                    | DataPermitServerStep::InternalInvariant
                            )))
                    .then_some(NodeInterfaceSupervisorTransition::DataPermit { actor, step })
                }
                lane => {
                    let actor = lane - 3 - SLOTS;
                    let phase_before = self.ordinary_permits[actor].phase();
                    let step = self.ordinary_permits[actor].step(
                        &mut self.ordinary,
                        now,
                        &mut self.policy,
                    );
                    (matches!(step, OrdinaryPermitServerStep::Advanced)
                        || (phase_before != OrdinaryPermitServerPhase::Disabled
                            && matches!(
                                step,
                                OrdinaryPermitServerStep::Disabled(_)
                                    | OrdinaryPermitServerStep::InternalInvariant
                            )))
                    .then_some(NodeInterfaceSupervisorTransition::OrdinaryPermit { actor, step })
                }
            };
            if let Some(transition) = transition {
                self.lane_cursor = (lane + 1) % lane_count;
                return NodeInterfaceSupervisorPass {
                    maintenance,
                    transition,
                };
            }
        }
        self.lane_cursor = (self.lane_cursor + 1) % lane_count;
        NodeInterfaceSupervisorPass {
            maintenance,
            transition: NodeInterfaceSupervisorTransition::Idle,
        }
    }

    /// Aggregate completion-correlation fault, if one exact owner was
    /// rejected and retained.
    pub fn fault(&self) -> Option<NodeInterfaceSupervisorFault> {
        self.owner_mismatch_fault
            .then_some(NodeInterfaceSupervisorFault::DataOwnerMismatch)
            .or_else(|| self.completion_fault.as_ref().map(|residue| residue.fault))
    }

    /// Copy-only binding of the exact completion retained behind the aggregate
    /// fault.
    pub fn completion_fault_residue(&self) -> Option<NodeInterfaceCompletionResidue> {
        self.completion_fault
            .as_ref()
            .map(CompletionFaultResidue::binding)
    }

    /// Read scalar node-owner occupancy.
    pub fn node_capacities(&self) -> CapacitySnapshot {
        self.node.capacities()
    }

    /// Copy the complete allocation-free RNS telemetry and capacity snapshot.
    pub fn node_metrics(&self) -> EmbeddedNodeMetrics {
        self.node.metrics()
    }

    /// Immutable authoritative interface registry.
    pub const fn interface_registry(&self) -> &InterfaceRegistry<SLOTS> {
        self.router.registry()
    }

    /// DATA coordinator-local permanent fault.
    pub const fn data_fault(&self) -> Option<DataRouterFault> {
        self.data.fault()
    }

    /// Ordinary coordinator-local permanent fault.
    pub const fn ordinary_fault(&self) -> Option<OrdinaryRouterFault> {
        self.ordinary.fault()
    }

    /// Copy-only DATA parked-owner occupancy.
    pub fn data_parked_counts(&self) -> DataRouterParkedCounts {
        self.data.parked_counts()
    }

    /// Iterate terminal DATA attempts retained by the sole node owner until a
    /// durable application projection acknowledges them.
    pub fn terminal_attempts(&self) -> TerminalAttempts<'_> {
        self.node.terminal_attempts()
    }

    /// Release one exact terminal tombstone after durable application state
    /// has been committed.
    pub fn acknowledge_terminal(
        &mut self,
        handle: AttemptHandle,
    ) -> Result<TerminalAttempt, AcknowledgeError> {
        self.node.acknowledge_terminal(handle)
    }

    /// Iterate generation-safe recovered DATA-owner observations awaiting a
    /// durable transport audit and exact release acknowledgement.
    pub fn recovered_data_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.data.recovered_observations()
    }

    /// Iterate generation-safe fail-closed DATA quarantines without exposing
    /// their retained packet owners.
    pub fn quarantined_data_observations(
        &self,
    ) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.data.quarantine_observations()
    }

    /// Release one exact recovered DATA owner after its transport audit is
    /// durable. Quarantined owners deliberately have no corresponding release.
    pub fn acknowledge_recovered_data(
        &mut self,
        observation: TxRecoveryObservation,
    ) -> Result<(), DataRecoveryAckError> {
        self.data.acknowledge_recovered(observation)
    }

    /// Copy-only ordinary owner occupancy.
    pub fn ordinary_capacities(&self) -> OrdinaryActionCapacitySnapshot {
        self.ordinary.capacities()
    }

    /// Number of ordinary packet buffers currently parked for reuse.
    pub fn ordinary_parked_count(&self) -> usize {
        self.ordinary.parked_count()
    }

    /// Phase of one concrete actor's DATA permit service.
    pub fn data_permit_phase(&self, actor: usize) -> Option<DataPermitServerPhase> {
        self.data_permits.get(actor).map(DataPermitServer::phase)
    }

    /// Phase of one concrete actor's ordinary permit service.
    pub fn ordinary_permit_phase(&self, actor: usize) -> Option<OrdinaryPermitServerPhase> {
        self.ordinary_permits
            .get(actor)
            .map(OrdinaryPermitServer::phase)
    }

    /// Whether one exact ingress-produced action envelope is retained for a
    /// retryable busy admission.
    pub const fn ingress_actions_pending(&self) -> bool {
        self.pending_ingress_actions.is_some()
    }

    /// Copy-only non-retryable ingress-action status, if an exact envelope is
    /// retained for explicit recovery.
    pub fn terminal_ingress_action_fault(&self) -> Option<NodeInterfaceIngressActionFault> {
        self.terminal_ingress_actions
            .as_ref()
            .map(NodeInterfaceTerminalIngressActions::fault)
    }

    /// Take the exact ingress-produced action envelope retained behind a
    /// non-retryable admission fault.
    pub fn take_terminal_ingress_actions(&mut self) -> Option<NodeInterfaceTerminalIngressActions> {
        self.terminal_ingress_actions.take()
    }

    /// Copy-only status of an exact sealed ingress buffer retained for a
    /// retryable queue-full recycle.
    pub fn ingress_recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        self.pending_ingress_recycle
            .as_ref()
            .map(|pending| pending.status)
    }

    /// Copy-only terminal status of an exact sealed ingress buffer retained
    /// for explicit quarantine or recovery.
    pub fn terminal_ingress_recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        self.terminal_ingress_recycle
            .as_ref()
            .map(|terminal| terminal.status)
    }

    /// Take the exact sealed ingress packet and its terminal recycle status.
    pub fn take_terminal_ingress_buffer(
        &mut self,
    ) -> Option<(NodeInterfaceIngressRecycleFault, SealedIngressPacket)> {
        self.terminal_ingress_recycle
            .take()
            .map(|terminal| (terminal.status, terminal.packet))
    }

    /// Shared authorization policy as an immutable status surface.
    pub const fn policy(&self) -> &P {
        &self.policy
    }

    /// Earliest locally known DATA or ordinary owner deadline.
    pub fn next_deadline(&self) -> Option<TxLeaseDeadline> {
        [
            self.node.next_tx_deadline(),
            self.data.next_deadline(),
            self.ordinary.next_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn step_completion_lane(&mut self) -> Option<NodeInterfaceSupervisorTransition> {
        if self.completion_fault.is_some() {
            return None;
        }
        match self.router.try_receive_completion() {
            Ok(Some(completion)) => {
                self.accept_completion(completion, NodeInterfaceCompletionOrigin::Current)
            }
            Ok(None) => None,
            Err(failure) => {
                let recovery = failure.into_node_recovery();
                let origin = NodeInterfaceCompletionOrigin::Recovery(recovery.reason());
                self.accept_completion(recovery.into_outbound(), origin)
            }
        }
    }

    fn accept_completion(
        &mut self,
        completion: OutboundCompletion,
        origin: NodeInterfaceCompletionOrigin,
    ) -> Option<NodeInterfaceSupervisorTransition> {
        match completion {
            OutboundCompletion::Data(completion) => {
                match self.data.try_accept_completion(completion) {
                    Ok(()) => Some(NodeInterfaceSupervisorTransition::CompletionAccepted {
                        family: NodeInterfaceCompletionFamily::Data,
                        origin,
                    }),
                    Err(failure) => {
                        let fault =
                            NodeInterfaceSupervisorFault::DataCompletionRejected(failure.reason());
                        self.completion_fault = Some(CompletionFaultResidue {
                            fault,
                            completion: OutboundCompletion::Data(failure.into_completion()),
                        });
                        Some(NodeInterfaceSupervisorTransition::Fault(fault))
                    }
                }
            }
            OutboundCompletion::Ordinary(completion) => {
                match self.ordinary.try_accept_completion(completion) {
                    Ok(()) => Some(NodeInterfaceSupervisorTransition::CompletionAccepted {
                        family: NodeInterfaceCompletionFamily::Ordinary,
                        origin,
                    }),
                    Err(failure) => {
                        let fault = NodeInterfaceSupervisorFault::OrdinaryCompletionRejected(
                            failure.reason(),
                        );
                        self.completion_fault = Some(CompletionFaultResidue {
                            fault,
                            completion: OutboundCompletion::Ordinary(failure.into_completion()),
                        });
                        Some(NodeInterfaceSupervisorTransition::Fault(fault))
                    }
                }
            }
        }
    }
}
