//! Permanent, RF-inert supervision for the Reticulum TX owner machines.
//!
//! [`TxSupervisor`] owns one node core, its node-side DATA machine, the scalar
//! permit server, the no-RF dispatcher, and the authorization policy in one
//! non-restartable aggregate. A complete synchronous pass samples the clock
//! separately before lease maintenance and before every clocked machine
//! transition. The async runner performs bounded complete passes, yields under
//! sustained progress, and otherwise races only phase-compatible,
//! cancellation-safe waits against the next absolute deadline.
//!
//! [`NodeInterfaceSupervisor`] is the transport-neutral permanent aggregate. It
//! owns one [`NodeCore`], the authoritative outbound interface router, direct
//! ticket-aware [`DataRouterCoordinator`] and [`OrdinaryRouterCoordinator`]
//! paths, one DATA and ordinary permit service per concrete interface actor,
//! and the shared authorization policy. Its checked constructor consumes the
//! unsplit interface fabric and paired permit proofs before returning
//! common-slot actor capabilities.
//!
//! [`TxSupervisor`] remains the earlier RF-inert aggregate around the legacy
//! no-RF dispatcher and DATA handoff machine. It is retained for focused owner
//! lifecycle validation, but it is not the permanent multi-interface graph.
//!
//! This crate deliberately has no firmware, radio, HAL, device-API, flash, or
//! executor dependency. The aggregate is the intended sole [`NodeCore`] owner;
//! portable RNS ingress accepts only registry-validated interface provenance,
//! while firmware remains responsible for RNode reassembly and for draining
//! every returned protocol action. Durable submission projection and a real
//! radio dispatcher are still separate boundaries.

#![no_std]
#![deny(unsafe_code)]
#![deny(missing_docs)]

mod data_permit;
mod data_router;
mod node_interface;
mod ordinary_permit;
mod ordinary_router;

pub use data_permit::{
    DataPermitServer, DataPermitServerFaultResidueKind, DataPermitServerPhase,
    DataPermitServerStep, DataPermitServerWait,
};

pub use data_router::{
    DataCompletionAcceptError, DataCompletionAcceptFailure, DataPermitAuthorizationError,
    DataPermitAuthorizationFailure, DataPreparedHop, DataRecoveryAckError, DataRouterBuildError,
    DataRouterBuildFailure, DataRouterCompletionProgress, DataRouterConfig, DataRouterCoordinator,
    DataRouterFault, DataRouterFaultResidueKind, DataRouterOwnerMismatch, DataRouterParkedCounts,
    DataRouterParkedKind, DataRouterPrepareRequest, DataRouterPrepareResult, DataRouterStep,
};

pub use node_interface::{
    NodeInterfaceActorPorts, NodeInterfaceAnnounceFlushResult, NodeInterfaceApplicationEventDrain,
    NodeInterfaceCompletionFamily, NodeInterfaceCompletionOrigin, NodeInterfaceCompletionResidue,
    NodeInterfaceDataPrepareResult, NodeInterfaceIngressActionFault,
    NodeInterfaceIngressRecycleFault, NodeInterfaceIngressStep, NodeInterfaceOrdinaryOfferError,
    NodeInterfaceOrdinaryOfferFailure, NodeInterfaceQueuedIngressProcessed,
    NodeInterfaceSupervisor, NodeInterfaceSupervisorBuildError,
    NodeInterfaceSupervisorBuildFailure, NodeInterfaceSupervisorBuildSuccess,
    NodeInterfaceSupervisorFault, NodeInterfaceSupervisorInit, NodeInterfaceSupervisorPass,
    NodeInterfaceSupervisorTransition, NodeInterfaceTerminalIngressActions,
    NodeInterfaceTickAccepted, NodeInterfaceTickActionFailure, NodeInterfaceTickResult,
    RouteDiagnosticsEntry, RouteDiagnosticsSnapshot, RouteExpiryState, RouteInterfaceResolution,
};

pub use ordinary_permit::{
    OrdinaryPermitServer, OrdinaryPermitServerFaultResidueKind, OrdinaryPermitServerPhase,
    OrdinaryPermitServerStep, OrdinaryPermitServerWait,
};

pub use ordinary_router::{
    OrdinaryCompletionAcceptError, OrdinaryCompletionAcceptFailure,
    OrdinaryPermitAuthorizationError, OrdinaryPermitAuthorizationFailure, OrdinaryRouterAdmission,
    OrdinaryRouterBuildError, OrdinaryRouterBuildFailure, OrdinaryRouterBusyReason,
    OrdinaryRouterCompletionObservation, OrdinaryRouterCompletionProgress, OrdinaryRouterConfig,
    OrdinaryRouterCoordinator, OrdinaryRouterFault, OrdinaryRouterFaultResidueKind,
    OrdinaryRouterOfferError, OrdinaryRouterOfferFailure, OrdinaryRouterRejectedActions,
    OrdinaryRouterStep,
};

pub use reticulum_node_core::LxmfMessageLocation;

use core::future::{Future, pending};

use embassy_futures::{
    select::{Either4, select4},
    yield_now,
};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_time::{Duration, Instant, Timer};
use rand_core::{CryptoRng, RngCore};
use reticulum_node_core::{
    AcknowledgeError, AnnounceAdmissionError, AttemptHandle, DestinationHash, InboundProofPolicy,
    MaintenanceReport, MonotonicMillis, MonotonicSeconds, NodeActions, NodeCore,
    PrepareDataRequest, TerminalAttempt, TxAuthorizationCandidate, TxAuthorizationErrorKind,
    TxAuthorizationPolicy, TxLeaseDeadline, TxMaintenanceReport, TxPolicyDecision, TxPolicyDenial,
    TxRecoveryObservation, TxRecoveryRecord,
};
use reticulum_tx_dispatch::{
    NoRfTxDispatcher, NoRfTxDispatcherPhase, NoRfTxDispatcherStep, NoRfTxDispatcherWait,
    NoRfTxMachineSet, NodeTxDataFault, NodeTxDataFaultResidueKind, NodeTxDataMachine,
    NodeTxDataPhase, NodeTxDataStep, NodeTxDataWait, NodeTxParkedCounts, NodeTxPrepareResult,
    NodeTxRecoveryAckError, TxDispatcherFault, TxPermitServer, TxPermitServerPhase,
    TxPermitServerStep, TxPermitServerWait,
};

/// Maximum complete synchronous passes before the permanent runner yields.
pub const MAX_IMMEDIATE_PASSES: usize = 16;

/// Portable source of whole-millisecond monotonic samples.
///
/// Every clock instance, [`TxLeaseDeadline`], and `owner_now` value used with
/// one supervisor must share this clock's epoch and millisecond scale. Mixing
/// boot-relative and wall-clock values, or independently reset epochs, is a
/// caller contract violation.
pub trait TxMonotonicClock {
    /// Return one current monotonic sample.
    fn now(&mut self) -> MonotonicMillis;
}

/// Monotonic source that can also wait for an absolute logical deadline.
///
/// The returned wait future must be cancellation-safe: dropping it before
/// readiness must not consume a one-shot alarm, corrupt the clock, or prevent
/// a later wait for the same or another deadline. The supervisor races this
/// future against channel inputs and drops every losing wait.
pub trait TxAsyncMonotonicClock: TxMonotonicClock {
    /// Future returned by [`Self::wait_until`].
    type WaitUntil<'a>: Future<Output = ()>
    where
        Self: 'a;

    /// Wait until the supplied absolute monotonic-millisecond deadline.
    ///
    /// Early wakes are allowed. The supervisor always samples the clock again
    /// before making a transition.
    fn wait_until(&mut self, deadline: MonotonicMillis) -> Self::WaitUntil<'_>;
}

/// Convert a logical millisecond deadline to the first Embassy instant that
/// is not earlier than it.
///
/// `Instant::from_millis` rounds down for tick rates that do not divide 1 kHz.
/// Converting through the rounding-up `Duration` constructor preserves the
/// safety side of an owner deadline. Values outside the tick range saturate.
pub fn embassy_instant_for_millis(deadline: MonotonicMillis) -> Instant {
    Duration::try_from_millis(deadline.get())
        .map(|duration| Instant::from_ticks(duration.as_ticks()))
        .unwrap_or(Instant::MAX)
}

/// Adapter for the platform Embassy time driver.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EmbassyMonotonicClock;

impl TxMonotonicClock for EmbassyMonotonicClock {
    fn now(&mut self) -> MonotonicMillis {
        MonotonicMillis::new(Instant::now().as_millis())
    }
}

impl TxAsyncMonotonicClock for EmbassyMonotonicClock {
    type WaitUntil<'a> = Timer;

    fn wait_until(&mut self, deadline: MonotonicMillis) -> Self::WaitUntil<'_> {
        Timer::at(embassy_instant_for_millis(deadline))
    }
}

/// Initial fail-closed policy: validate candidates, but authorize no RF use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RfInertTxPolicy;

impl TxAuthorizationPolicy for RfInertTxPolicy {
    fn authorize(&mut self, _candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        TxPolicyDecision::Deny(TxPolicyDenial::ResourceUnavailable)
    }
}

/// Observed monotonic-clock regression.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxClockRegression {
    previous: MonotonicMillis,
    observed: MonotonicMillis,
}

impl TxClockRegression {
    /// Last accepted sample.
    pub const fn previous(self) -> MonotonicMillis {
        self.previous
    }

    /// Regressing sample that stopped all clocked transitions.
    pub const fn observed(self) -> MonotonicMillis {
        self.observed
    }
}

/// Node-side DATA-machine supervisor fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxSupervisorDataFault {
    /// The machine failed closed for the contained reason.
    Machine(NodeTxDataFault),
    /// The machine rejected the node supplied by its owning supervisor.
    OwnerMismatch,
}

/// Permit-service supervisor fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxSupervisorPermitFault {
    /// Node-core rejected a retained permit request.
    Authorization(TxAuthorizationErrorKind),
    /// The permit machine reported an impossible private state.
    InternalInvariant,
}

/// Complete scalar fault snapshot retained by the supervisor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TxSupervisorFaults {
    clock: Option<TxClockRegression>,
    data: Option<TxSupervisorDataFault>,
    permit: Option<TxSupervisorPermitFault>,
    dispatcher: Option<TxDispatcherFault>,
}

impl TxSupervisorFaults {
    /// Whether any permanent supervisor fault has been observed.
    pub const fn is_faulted(self) -> bool {
        self.clock.is_some()
            || self.data.is_some()
            || self.permit.is_some()
            || self.dispatcher.is_some()
    }

    /// Retained monotonic regression, if clocked transitions have stopped.
    pub const fn clock(self) -> Option<TxClockRegression> {
        self.clock
    }

    /// Retained node-side DATA fault.
    pub const fn data(self) -> Option<TxSupervisorDataFault> {
        self.data
    }

    /// Retained permit-service fault.
    pub const fn permit(self) -> Option<TxSupervisorPermitFault> {
        self.permit
    }

    /// Retained dispatcher fault.
    pub const fn dispatcher(self) -> Option<TxDispatcherFault> {
        self.dispatcher
    }
}

/// Result of one complete, bounded-fairness supervisor pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "supervisor progress and retained faults must be observed"]
pub struct TxSupervisorPass {
    maintenance: Option<TxMaintenanceReport>,
    data: Option<NodeTxDataStep>,
    permit: Option<TxPermitServerStep>,
    dispatcher: Option<NoRfTxDispatcherStep>,
    samples: u8,
    progressed: bool,
    faults: TxSupervisorFaults,
}

impl TxSupervisorPass {
    /// Packet-owner lease-maintenance result, absent after a clock stop.
    pub const fn maintenance(self) -> Option<TxMaintenanceReport> {
        self.maintenance
    }

    /// Node-side DATA-machine transition, if sampled.
    pub const fn data(self) -> Option<NodeTxDataStep> {
        self.data
    }

    /// Permit transition, omitted once any fault disables new authorization.
    pub const fn permit(self) -> Option<TxPermitServerStep> {
        self.permit
    }

    /// Dispatcher transition, if sampled.
    pub const fn dispatcher(self) -> Option<NoRfTxDispatcherStep> {
        self.dispatcher
    }

    /// Number of separately checked clock samples used by this pass.
    pub const fn clock_samples(self) -> u8 {
        self.samples
    }

    /// Whether at least one lane completed useful synchronous work.
    pub const fn progressed(self) -> bool {
        self.progressed
    }

    /// Complete retained fault snapshot after this pass.
    pub const fn faults(self) -> TxSupervisorFaults {
        self.faults
    }
}

/// Error returned when a permanent fault forbids fresh DATA preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxSupervisorUnavailable {
    faults: TxSupervisorFaults,
}

impl TxSupervisorUnavailable {
    /// Complete reason fresh work is disabled.
    pub const fn faults(self) -> TxSupervisorFaults {
        self.faults
    }
}

/// Legacy RF-inert aggregate for the earlier no-RF DATA ownership machines.
///
/// This type remains for focused lifecycle validation. New permanent runtimes
/// use [`NodeInterfaceSupervisor`], which owns the authoritative router,
/// ordinary-action path, and per-interface permit services. Dropping either
/// aggregate is not an ownership recovery mechanism.
#[must_use = "dropping the supervisor abandons unique node and packet owners"]
pub struct TxSupervisor<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const POOL_SIZE: usize,
> where
    M: RawMutex + 'static,
{
    owner: NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, POOL_SIZE>,
    data: NodeTxDataMachine<M, POOL_SIZE>,
    permit: TxPermitServer<M>,
    dispatcher: NoRfTxDispatcher<M, POOL_SIZE>,
    policy: P,
    last_now: Option<MonotonicMillis>,
    faults: TxSupervisorFaults,
}

impl<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const POOL_SIZE: usize,
> TxSupervisor<M, P, PATHS, ANNOUNCES, DEDUPLICATION, LINKS, POOL_SIZE>
where
    M: RawMutex + 'static,
    P: TxAuthorizationPolicy,
{
    /// Bind one exact node owner to its DATA machine and consume every other
    /// persistent TX component into the permanent aggregate.
    pub fn new(
        owner: NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, POOL_SIZE>,
        machines: NoRfTxMachineSet<M, POOL_SIZE>,
        policy: P,
    ) -> Self {
        let (data_handoff, permit, dispatcher) = machines.into_parts();
        let data = NodeTxDataMachine::new(data_handoff, &owner);
        Self {
            owner,
            data,
            permit,
            dispatcher,
            policy,
            last_now: None,
            faults: TxSupervisorFaults::default(),
        }
    }

    /// Complete retained fault snapshot.
    pub const fn faults(&self) -> TxSupervisorFaults {
        self.faults
    }

    /// Current node-side DATA phase.
    pub const fn data_phase(&self) -> NodeTxDataPhase {
        self.data.phase()
    }

    /// Current permit-service phase.
    pub const fn permit_phase(&self) -> TxPermitServerPhase {
        self.permit.phase()
    }

    /// Current dispatcher phase.
    pub fn dispatcher_phase(&self) -> NoRfTxDispatcherPhase {
        self.dispatcher.phase()
    }

    /// Primary local Reticulum destination owned by this aggregate.
    pub fn destination_hash(&self) -> DestinationHash {
        self.owner.destination_hash()
    }

    /// Configure automatic delivery proofs on the aggregate's sole node owner.
    ///
    /// Proof packets returned by later ingress calls remain ordinary protocol
    /// actions. The permanent runtime must retain and drain them through its
    /// bounded radio handoff; this method does not bypass authorization or
    /// transmit directly.
    pub fn set_inbound_proof_policy(&mut self, policy: InboundProofPolicy) {
        self.owner.set_inbound_proof_policy(policy);
    }

    /// Admit one signed local announce into the sole node owner's bounded queue.
    pub fn queue_announce<R: RngCore + CryptoRng>(
        &mut self,
        app_data: Option<&[u8]>,
        emitted_at: reticulum_node_core::AnnounceEmissionTime,
        rng: &mut R,
    ) -> Result<(), AnnounceAdmissionError> {
        self.owner.queue_announce(app_data, emitted_at, rng)
    }

    /// Flush ready local announces into the ordinary protocol-action envelope.
    ///
    /// The caller owns every returned action and must not silently discard one
    /// because the downstream radio handoff is full.
    pub fn flush_announces<R: RngCore>(
        &mut self,
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> NodeActions {
        self.owner.flush_announces(now, rng)
    }

    /// Run RNS timer maintenance through the sole node owner.
    ///
    /// This uses whole protocol seconds, independent from the millisecond clock
    /// supplied to TX-owner maintenance and permit deadlines.
    pub fn tick_rns<R: RngCore + CryptoRng>(
        &mut self,
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> MaintenanceReport {
        self.owner.tick(now, rng)
    }

    /// Scalar fixed-owner-table occupancy.
    pub fn parked_counts(&self) -> NodeTxParkedCounts {
        self.data.parked_counts()
    }

    /// Kind of exact owner retained inside a permanently disabled DATA machine.
    pub fn data_fault_residue_kind(&self) -> Option<NodeTxDataFaultResidueKind> {
        self.data.fault_residue_kind()
    }

    /// Correlated recovery observation trapped inside a permanently disabled
    /// DATA machine, when the residue had already reached recovery.
    ///
    /// This owner is not in the acknowledgeable recovered-owner table. A
    /// durable projector must therefore classify the observation as
    /// fail-closed quarantine, even when [`Self::data_fault_residue_kind`]
    /// reports [`NodeTxDataFaultResidueKind::RecoveredBuffer`]. The original
    /// recovery reason remains useful evidence; [`Self::faults`] separately
    /// retains the secondary DATA-machine fault that prevented safe parking.
    pub fn data_fault_quarantine_observation(&self) -> Option<TxRecoveryObservation> {
        match self.data.fault_residue_kind() {
            Some(
                NodeTxDataFaultResidueKind::RecoveredBuffer
                | NodeTxDataFaultResidueKind::Quarantine,
            ) => self.data.fault_recovery_observation(),
            Some(
                NodeTxDataFaultResidueKind::AvailableBuffer
                | NodeTxDataFaultResidueKind::Completion
                | NodeTxDataFaultResidueKind::CompletionFailure
                | NodeTxDataFaultResidueKind::RollbackFailure,
            )
            | None => None,
        }
    }

    /// Iterate recovered records that remain parked and unacknowledged.
    pub fn recovered_records(&self) -> impl Iterator<Item = TxRecoveryRecord> + '_ {
        self.data.recovered_records()
    }

    /// Iterate correlated recovered owners that remain parked and
    /// unacknowledged.
    pub fn recovered_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.data.recovered_observations()
    }

    /// Iterate fail-closed quarantine records retained by the DATA machine.
    pub fn quarantine_records(&self) -> impl Iterator<Item = TxRecoveryRecord> + '_ {
        self.data.quarantine_records()
    }

    /// Iterate correlated fail-closed quarantine observations retained by the
    /// DATA machine.
    pub fn quarantine_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.data.quarantine_observations()
    }

    /// Iterate terminal DATA attempts that remain retained by node-core.
    pub fn terminal_attempts(&self) -> impl Iterator<Item = TerminalAttempt> + '_ {
        self.owner.terminal_attempts()
    }

    /// Release one terminal attempt only after its final disposition is
    /// durable and its packet owner is no longer bound.
    pub fn acknowledge_terminal(
        &mut self,
        handle: AttemptHandle,
    ) -> Result<TerminalAttempt, AcknowledgeError> {
        self.owner.acknowledge_terminal(handle)
    }

    /// Release one exact recovered owner after its correlated recovery
    /// observation is durable.
    ///
    /// A recovery already parked before a later DATA-machine fault remains
    /// acknowledgeable; the machine keeps that later fault and stays disabled.
    pub fn acknowledge_recovered(
        &mut self,
        observation: TxRecoveryObservation,
    ) -> Result<(), NodeTxRecoveryAckError> {
        self.data.acknowledge_recovered(observation)
    }

    /// Earliest absolute wake required by lease maintenance or an outstanding
    /// permit-exchange recovery grace period.
    pub fn next_wake(&self) -> Option<MonotonicMillis> {
        let owner = self.owner.next_tx_deadline().map(TxLeaseDeadline::instant);
        let grace = match self.dispatcher.phase() {
            NoRfTxDispatcherPhase::PermitSend | NoRfTxDispatcherPhase::PermitWait => {
                self.dispatcher.grace_deadline()
            }
            NoRfTxDispatcherPhase::Idle
            | NoRfTxDispatcherPhase::Job
            | NoRfTxDispatcherPhase::PermitReply
            | NoRfTxDispatcherPhase::Authorized
            | NoRfTxDispatcherPhase::Expired
            | NoRfTxDispatcherPhase::Unpermitted
            | NoRfTxDispatcherPhase::Return
            | NoRfTxDispatcherPhase::Disabled => None,
        };
        match (owner, grace) {
            (Some(owner), Some(grace)) => Some(core::cmp::min(owner, grace)),
            (Some(owner), None) => Some(owner),
            (None, Some(grace)) => Some(grace),
            (None, None) => None,
        }
    }

    /// Prepare one fresh DATA packet with a newly sampled owner clock and try
    /// its initial handoff synchronously.
    ///
    /// The supplied `owner_now` field is replaced with this supervisor's
    /// checked sample. Once any permanent fault is retained, no new work is
    /// accepted.
    pub fn try_prepare_and_submit_data<C, R>(
        &mut self,
        clock: &mut C,
        mut request: PrepareDataRequest<'_>,
        rng: &mut R,
    ) -> Result<NodeTxPrepareResult, TxSupervisorUnavailable>
    where
        C: TxMonotonicClock,
        R: RngCore + CryptoRng,
    {
        if self.faults.is_faulted() {
            return Err(TxSupervisorUnavailable {
                faults: self.faults,
            });
        }
        let Some(now) = self.sample(clock) else {
            return Err(TxSupervisorUnavailable {
                faults: self.faults,
            });
        };
        request.owner_now = now;
        let result = self
            .data
            .try_prepare_and_submit_data(&mut self.owner, request, rng);
        self.observe_data_prepare(result);
        Ok(result)
    }

    /// Run one complete synchronous pass.
    ///
    /// Clock samples are never shared between calls: maintenance, DATA,
    /// permit, and dispatcher each receive a separate checked sample. A clock
    /// regression stops the remainder of the pass and every later clocked
    /// transition. Other faults disable policy authorization and fresh work,
    /// while DATA and dispatcher stepping continues so already-moving exact
    /// owners can return to retained storage where their APIs permit it.
    pub fn run_one_pass<C>(&mut self, clock: &mut C) -> TxSupervisorPass
    where
        C: TxMonotonicClock,
    {
        let mut pass = TxSupervisorPass {
            maintenance: None,
            data: None,
            permit: None,
            dispatcher: None,
            samples: 0,
            progressed: false,
            faults: self.faults,
        };
        if self.faults.clock.is_some() {
            return pass;
        }

        let Some(now) = self.sample_counted(clock, &mut pass.samples) else {
            pass.faults = self.faults;
            return pass;
        };
        let maintenance = self.owner.maintain_tx(now);
        pass.progressed |= maintenance.newly_recovery_required > 0;
        pass.maintenance = Some(maintenance);

        let Some(now) = self.sample_counted(clock, &mut pass.samples) else {
            pass.faults = self.faults;
            return pass;
        };
        let data = self.data.step(&mut self.owner, now);
        pass.progressed |= data_progressed(data);
        pass.data = Some(data);
        self.observe_data_step(data);

        if !self.faults.is_faulted() {
            let Some(now) = self.sample_counted(clock, &mut pass.samples) else {
                pass.faults = self.faults;
                return pass;
            };
            let permit = self.permit.step(&mut self.owner, now, &mut self.policy);
            pass.progressed |= permit_progressed(permit);
            pass.permit = Some(permit);
            self.observe_permit_step(permit);
        }

        let Some(now) = self.sample_counted(clock, &mut pass.samples) else {
            pass.faults = self.faults;
            return pass;
        };
        let dispatcher_before = self.dispatcher.phase();
        let dispatcher = self.dispatcher.step(now);
        pass.progressed |= dispatcher_progressed(dispatcher_before, dispatcher);
        pass.dispatcher = Some(dispatcher);
        self.observe_dispatcher_step(dispatcher);

        pass.faults = self.faults;
        pass
    }

    /// Await one phase-compatible TX-machine input or the next absolute deadline.
    ///
    /// Call this only after a complete pass reports no progress. Dispatcher
    /// input is polled before the timer, so an already-observable permit reply
    /// wins an exact grace-deadline tie. Losing short waits are safe to cancel;
    /// all unique owners remain in channels or persistent machine state.
    ///
    /// A permanent node task may race this future against an independently
    /// owned RX-frame or RNS-timer future. Cancelling this future cannot discard
    /// an owner: channel receives and the supplied deadline wait are required to
    /// be cancellation-safe, and state changes occur only after a selected wait
    /// completes.
    pub async fn wait_for_work<C>(&mut self, clock: &mut C)
    where
        C: TxAsyncMonotonicClock,
    {
        if self.faults.clock.is_some() {
            pending::<()>().await;
            return;
        }

        let data_waitable = matches!(
            self.data.phase(),
            NodeTxDataPhase::Seeding | NodeTxDataPhase::Idle | NodeTxDataPhase::NextPending
        );
        let permit_waitable =
            !self.faults.is_faulted() && matches!(self.permit.phase(), TxPermitServerPhase::Idle);
        let dispatcher_waitable = matches!(
            self.dispatcher.phase(),
            NoRfTxDispatcherPhase::Idle | NoRfTxDispatcherPhase::PermitWait
        );
        let deadline = self.next_wake();

        let wake = select4(
            wait_dispatcher(&mut self.dispatcher, dispatcher_waitable),
            wait_permit(&mut self.permit, permit_waitable),
            wait_data(&mut self.data, data_waitable),
            wait_deadline(clock, deadline),
        )
        .await;
        match wake {
            Either4::First(NoRfTxDispatcherWait::Disabled(fault)) => {
                self.faults.dispatcher.get_or_insert(fault);
            }
            Either4::Second(TxPermitServerWait::Disabled(fault)) => {
                self.faults
                    .permit
                    .get_or_insert(TxSupervisorPermitFault::Authorization(fault));
            }
            Either4::Second(TxPermitServerWait::InternalInvariant) => {
                self.faults
                    .permit
                    .get_or_insert(TxSupervisorPermitFault::InternalInvariant);
            }
            Either4::Third(NodeTxDataWait::Disabled(fault)) => {
                self.faults
                    .data
                    .get_or_insert(TxSupervisorDataFault::Machine(fault));
            }
            Either4::First(
                NoRfTxDispatcherWait::JobStored
                | NoRfTxDispatcherWait::PermitReplyStored
                | NoRfTxDispatcherWait::NotWaiting,
            )
            | Either4::Second(TxPermitServerWait::RequestStored | TxPermitServerWait::NotWaiting)
            | Either4::Third(
                NodeTxDataWait::ReturnStored
                | NodeTxDataWait::JobCapacityReady
                | NodeTxDataWait::NotWaiting,
            )
            | Either4::Fourth(()) => {}
        }
        if let Some(fault) = self.data.fault() {
            self.faults
                .data
                .get_or_insert(TxSupervisorDataFault::Machine(fault));
        }
        if let Some(fault) = self.permit.fault() {
            self.faults
                .permit
                .get_or_insert(TxSupervisorPermitFault::Authorization(fault));
        }
        if let Some(fault) = self.dispatcher.fault() {
            self.faults.dispatcher.get_or_insert(fault);
        }
    }

    /// Run the permanent supervisor forever.
    ///
    /// The aggregate is borrowed for the task's complete lifetime. Sustained
    /// synchronous progress is bounded to [`MAX_IMMEDIATE_PASSES`] complete
    /// passes before yielding to the executor.
    pub async fn run<C>(&'static mut self, mut clock: C) -> !
    where
        C: TxAsyncMonotonicClock + 'static,
    {
        loop {
            let mut quiescent = false;
            for _ in 0..MAX_IMMEDIATE_PASSES {
                let pass = self.run_one_pass(&mut clock);
                if self.faults.clock.is_some() {
                    pending::<()>().await;
                }
                if !pass.progressed() {
                    quiescent = true;
                    break;
                }
            }
            if quiescent {
                self.wait_for_work(&mut clock).await;
                // An async clock may wake early or immediately. Yield after
                // every selected wake so a conforming timer cannot create an
                // executor-starving quiescent loop.
                yield_now().await;
            } else {
                yield_now().await;
            }
        }
    }

    fn sample<C>(&mut self, clock: &mut C) -> Option<MonotonicMillis>
    where
        C: TxMonotonicClock,
    {
        if self.faults.clock.is_some() {
            return None;
        }
        let observed = clock.now();
        if let Some(previous) = self.last_now
            && observed < previous
        {
            self.faults.clock = Some(TxClockRegression { previous, observed });
            return None;
        }
        self.last_now = Some(observed);
        Some(observed)
    }

    fn sample_counted<C>(&mut self, clock: &mut C, samples: &mut u8) -> Option<MonotonicMillis>
    where
        C: TxMonotonicClock,
    {
        *samples = samples.saturating_add(1);
        self.sample(clock)
    }

    fn observe_data_prepare(&mut self, result: NodeTxPrepareResult) {
        match result {
            NodeTxPrepareResult::OwnerMismatch => {
                self.faults
                    .data
                    .get_or_insert(TxSupervisorDataFault::OwnerMismatch);
            }
            NodeTxPrepareResult::Disabled(fault) => {
                self.faults
                    .data
                    .get_or_insert(TxSupervisorDataFault::Machine(fault));
            }
            NodeTxPrepareResult::Queued(_)
            | NodeTxPrepareResult::QueueBackpressured
            | NodeTxPrepareResult::RollbackPending(_)
            | NodeTxPrepareResult::Rejected { .. }
            | NodeTxPrepareResult::RejectedQuarantined { .. }
            | NodeTxPrepareResult::NoAvailable
            | NodeTxPrepareResult::ProgressRequired(_) => {}
        }
    }

    fn observe_data_step(&mut self, step: NodeTxDataStep) {
        match step {
            NodeTxDataStep::OwnerMismatch => {
                self.faults
                    .data
                    .get_or_insert(TxSupervisorDataFault::OwnerMismatch);
            }
            NodeTxDataStep::Disabled(fault) => {
                self.faults
                    .data
                    .get_or_insert(TxSupervisorDataFault::Machine(fault));
            }
            NodeTxDataStep::Advanced
            | NodeTxDataStep::NeedSeed(_)
            | NodeTxDataStep::NeedReturn
            | NodeTxDataStep::SeedParked { .. }
            | NodeTxDataStep::AvailableParked(_)
            | NodeTxDataStep::RecoveredParked(_)
            | NodeTxDataStep::QuarantinedParked(_)
            | NodeTxDataStep::NextQueued(_)
            | NodeTxDataStep::NextBackpressured(_)
            | NodeTxDataStep::FreshRollbackHandled(_) => {}
        }
    }

    fn observe_permit_step(&mut self, step: TxPermitServerStep) {
        match step {
            TxPermitServerStep::Disabled(fault) => {
                self.faults
                    .permit
                    .get_or_insert(TxSupervisorPermitFault::Authorization(fault));
            }
            TxPermitServerStep::InternalInvariant => {
                self.faults
                    .permit
                    .get_or_insert(TxSupervisorPermitFault::InternalInvariant);
            }
            TxPermitServerStep::Advanced
            | TxPermitServerStep::NeedRequest
            | TxPermitServerStep::ReplyBackpressured => {}
        }
    }

    fn observe_dispatcher_step(&mut self, step: NoRfTxDispatcherStep) {
        if let NoRfTxDispatcherStep::Disabled(fault) = step {
            self.faults.dispatcher.get_or_insert(fault);
        } else if let Some(fault) = self.dispatcher.fault() {
            self.faults.dispatcher.get_or_insert(fault);
        }
    }
}

fn data_progressed(step: NodeTxDataStep) -> bool {
    matches!(
        step,
        NodeTxDataStep::Advanced
            | NodeTxDataStep::SeedParked { .. }
            | NodeTxDataStep::AvailableParked(_)
            | NodeTxDataStep::RecoveredParked(_)
            | NodeTxDataStep::QuarantinedParked(_)
            | NodeTxDataStep::NextQueued(_)
            | NodeTxDataStep::FreshRollbackHandled(_)
    )
}

fn permit_progressed(step: TxPermitServerStep) -> bool {
    matches!(step, TxPermitServerStep::Advanced)
}

fn dispatcher_progressed(before: NoRfTxDispatcherPhase, step: NoRfTxDispatcherStep) -> bool {
    matches!(
        step,
        NoRfTxDispatcherStep::Advanced | NoRfTxDispatcherStep::Inspected(_)
    ) || (before != NoRfTxDispatcherPhase::Disabled
        && matches!(step, NoRfTxDispatcherStep::Disabled(_)))
}

async fn wait_data<M, const POOL_SIZE: usize>(
    machine: &mut NodeTxDataMachine<M, POOL_SIZE>,
    enabled: bool,
) -> NodeTxDataWait
where
    M: RawMutex + 'static,
{
    if enabled {
        machine.wait_for_progress().await
    } else {
        pending::<NodeTxDataWait>().await
    }
}

async fn wait_permit<M>(machine: &mut TxPermitServer<M>, enabled: bool) -> TxPermitServerWait
where
    M: RawMutex + 'static,
{
    if enabled {
        machine.wait_for_request().await
    } else {
        pending::<TxPermitServerWait>().await
    }
}

async fn wait_dispatcher<M, const POOL_SIZE: usize>(
    machine: &mut NoRfTxDispatcher<M, POOL_SIZE>,
    enabled: bool,
) -> NoRfTxDispatcherWait
where
    M: RawMutex + 'static,
{
    if enabled {
        machine.wait_for_input().await
    } else {
        pending::<NoRfTxDispatcherWait>().await
    }
}

async fn wait_deadline<C>(clock: &mut C, deadline: Option<MonotonicMillis>)
where
    C: TxAsyncMonotonicClock,
{
    if let Some(deadline) = deadline {
        clock.wait_until(deadline).await;
    } else {
        pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        future::{Future, Pending, Ready, ready},
        pin::pin,
        task::{Context, Poll, Waker},
    };

    use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
    use reticulum_node_core::{
        AttemptOutcome, AttemptToken, AttemptUnsentReason, DestinationHash, InterfaceSet,
        MonotonicSeconds, NodeConfig, NodeIdentity, NodeInstanceId, PacketInterfaceId,
        TxCompletionCode, TxPacketBuffer, TxPermitReservation,
    };
    use reticulum_tx_dispatch::{NoRfTxDispatcherConfig, TxDispatcherCompletionCodes};
    use reticulum_tx_handoff::TxHandoff;
    use static_cell::{ConstStaticCell, StaticCell};
    use std::{boxed::Box, vec, vec::Vec};

    use super::*;

    type TestNode = NodeCore<4, 2, 8, 2, 1>;
    type TestSupervisor<P> = TxSupervisor<NoopRawMutex, P, 4, 2, 8, 2, 1>;
    type ProductionSupervisor =
        TxSupervisor<CriticalSectionRawMutex, RfInertTxPolicy, 4, 2, 8, 2, 1>;

    static STATIC_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());
    static STATIC_BUFFER: StaticCell<TxPacketBuffer> = StaticCell::new();
    static STATIC_SUPERVISOR: StaticCell<TestSupervisor<RfInertTxPolicy>> = StaticCell::new();
    static PRODUCTION_SUPERVISOR: StaticCell<ProductionSupervisor> = StaticCell::new();

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

    struct RecordingDeny {
        calls: usize,
        candidate: Option<TxAuthorizationCandidate>,
    }

    impl TxAuthorizationPolicy for RecordingDeny {
        fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            self.calls += 1;
            self.candidate = Some(candidate);
            TxPolicyDecision::Deny(TxPolicyDenial::ResourceUnavailable)
        }
    }

    struct RecordingAllow {
        calls: usize,
    }

    impl TxAuthorizationPolicy for RecordingAllow {
        fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            self.calls += 1;
            TxPolicyDecision::Authorize(
                TxPermitReservation::try_new(
                    candidate.requirements.resource(),
                    candidate.requirements.required_units(),
                )
                .expect("test policy must mirror valid requirements"),
            )
        }
    }

    #[derive(Default)]
    struct ManualClock {
        now: u64,
        calls: usize,
    }

    impl ManualClock {
        fn at(now: u64) -> Self {
            Self { now, calls: 0 }
        }
    }

    impl TxMonotonicClock for ManualClock {
        fn now(&mut self) -> MonotonicMillis {
            self.calls += 1;
            MonotonicMillis::new(self.now)
        }
    }

    impl TxAsyncMonotonicClock for ManualClock {
        type WaitUntil<'a> = Pending<()>;

        fn wait_until(&mut self, _deadline: MonotonicMillis) -> Self::WaitUntil<'_> {
            pending()
        }
    }

    struct ReadyTimerClock(ManualClock);

    impl TxMonotonicClock for ReadyTimerClock {
        fn now(&mut self) -> MonotonicMillis {
            self.0.now()
        }
    }

    impl TxAsyncMonotonicClock for ReadyTimerClock {
        type WaitUntil<'a> = Ready<()>;

        fn wait_until(&mut self, _deadline: MonotonicMillis) -> Self::WaitUntil<'_> {
            ready(())
        }
    }

    struct ScriptClock<'a> {
        samples: &'a [u64],
        next: usize,
    }

    impl TxMonotonicClock for ScriptClock<'_> {
        fn now(&mut self) -> MonotonicMillis {
            let value = self.samples[self.next];
            self.next += 1;
            MonotonicMillis::new(value)
        }
    }

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid")
    }

    fn proof_for(receiver_tag: u8, attempt: AttemptToken) -> Vec<u8> {
        let identity = reticulum_rns_rete::identity_from_private_key(&[receiver_tag; 64]).unwrap();
        let signature = identity.sign(attempt.as_bytes()).unwrap();
        let mut proof = vec![0u8; 19 + 32 + 64];
        proof[0] = 0x03;
        proof[2..18].copy_from_slice(&attempt.as_bytes()[..16]);
        proof[19..51].copy_from_slice(attempt.as_bytes());
        proof[51..].copy_from_slice(&signature);
        proof
    }

    fn node(tag: u8, aspect: &str) -> TestNode {
        TestNode::new(
            identity(tag),
            "reticulum",
            &[aspect],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .expect("test node must construct")
    }

    fn config() -> NoRfTxDispatcherConfig {
        NoRfTxDispatcherConfig::new(
            25,
            TxDispatcherCompletionCodes::new(
                TxCompletionCode::new(0x301),
                TxCompletionCode::new(0x302),
                TxCompletionCode::new(0x303),
                TxCompletionCode::new(0x304),
                TxCompletionCode::new(0x305),
            ),
        )
    }

    fn build_supervisor<P>(policy: P, tag: u8) -> (TestSupervisor<P>, DestinationHash)
    where
        P: TxAuthorizationPolicy,
    {
        let peer_aspect = "supervisor-peer";
        let destination = NodeCore::<4, 2, 8, 2, 1>::new(
            identity(tag.wrapping_add(1)),
            "reticulum",
            &[peer_aspect],
            NodeInstanceId::new([tag.wrapping_add(0x40); 16]),
            NodeConfig::endpoint(),
        )
        .expect("peer node must construct")
        .destination_hash();
        let peer = identity(tag.wrapping_add(1));

        let mut owner = node(tag, "supervisor-owner");
        owner
            .register_peer(&peer, "reticulum", &[peer_aspect], MonotonicSeconds::new(0))
            .expect("peer must register");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        owner
            .register_packet_buffer(buffer)
            .expect("packet buffer must register");

        let handoff = Box::leak(Box::new(TxHandoff::<NoopRawMutex, 1>::new()));
        let mut handoff = handoff.split_paired();
        handoff
            .try_seed_available(buffer)
            .unwrap_or_else(|_| panic!("seed must fit the owner-return channel"));
        let machines = NoRfTxMachineSet::try_new(handoff, config())
            .unwrap_or_else(|_| panic!("fully seeded handoff must build"));
        (TxSupervisor::new(owner, machines, policy), destination)
    }

    fn request(destination: DestinationHash, deadline: u64) -> PrepareDataRequest<'static> {
        PrepareDataRequest {
            destination,
            plaintext: b"permanent supervisor test payload",
            rns_now: MonotonicSeconds::new(1),
            owner_now: MonotonicMillis::new(0),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline)),
            enabled_interfaces: InterfaceSet::empty()
                .with(PacketInterfaceId::new(1))
                .expect("test interface must fit"),
        }
    }

    fn drive_to_quiescence<P>(supervisor: &mut TestSupervisor<P>, clock: &mut ManualClock) -> usize
    where
        P: TxAuthorizationPolicy,
    {
        for passes in 1..=64 {
            if !supervisor.run_one_pass(clock).progressed() {
                return passes;
            }
        }
        panic!("supervisor did not quiesce within the bounded test budget")
    }

    fn seed<P>(supervisor: &mut TestSupervisor<P>, clock: &mut ManualClock)
    where
        P: TxAuthorizationPolicy,
    {
        assert!(drive_to_quiescence(supervisor, clock) <= 4);
        assert_eq!(supervisor.parked_counts().available(), 1);
        assert_eq!(supervisor.data_phase(), NodeTxDataPhase::Idle);
    }

    #[test]
    fn complete_pass_samples_each_clocked_lane_separately() {
        let (mut supervisor, _) = build_supervisor(RfInertTxPolicy, 10);
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);
        let before = clock.calls;

        let pass = supervisor.run_one_pass(&mut clock);

        assert!(!pass.progressed());
        assert_eq!(pass.clock_samples(), 4);
        assert_eq!(clock.calls - before, 4);
        assert!(pass.maintenance().is_some());
        assert!(pass.data().is_some());
        assert!(pass.permit().is_some());
        assert!(pass.dispatcher().is_some());
        assert!(!pass.faults().is_faulted());
    }

    #[test]
    fn rf_inert_policy_completes_owner_lifecycle_without_authorization() {
        let (mut supervisor, destination) = build_supervisor(
            RecordingDeny {
                calls: 0,
                candidate: None,
            },
            20,
        );
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);

        let queued = supervisor
            .try_prepare_and_submit_data(
                &mut clock,
                request(destination, 10_000),
                &mut CounterRng::default(),
            )
            .expect("healthy supervisor must accept preparation");
        let queued = match queued {
            NodeTxPrepareResult::Queued(queued) => queued,
            other => panic!("fresh preparation was not queued: {other:?}"),
        };
        assert!(drive_to_quiescence(&mut supervisor, &mut clock) <= 16);

        assert_eq!(supervisor.policy.calls, 1);
        assert_eq!(
            supervisor
                .policy
                .candidate
                .expect("called policy must retain its candidate")
                .now,
            MonotonicMillis::new(1_000)
        );
        assert_eq!(supervisor.parked_counts().available(), 1);
        assert_eq!(supervisor.parked_counts().recovered(), 0);
        assert_eq!(supervisor.permit_phase(), TxPermitServerPhase::Idle);
        assert_eq!(supervisor.dispatcher_phase(), NoRfTxDispatcherPhase::Idle);
        assert_eq!(supervisor.next_wake(), None);
        assert!(!supervisor.faults().is_faulted());
        let terminal = supervisor
            .terminal_attempts()
            .next()
            .expect("policy denial must retain an unsent terminal");
        assert_eq!(terminal.handle(), queued.attempt_handle());
        assert_eq!(terminal.token(), queued.attempt());
        assert_eq!(
            terminal.outcome(),
            AttemptOutcome::Unsent(AttemptUnsentReason::PolicyDenied(
                TxPolicyDenial::ResourceUnavailable
            ))
        );
        assert_eq!(
            supervisor
                .acknowledge_terminal(terminal.handle())
                .expect("returned unsent owner must permit durable terminal acknowledgement"),
            terminal
        );
        assert_eq!(supervisor.terminal_attempts().count(), 0);
    }

    #[test]
    fn terminal_facade_withholds_acknowledgement_until_owner_returns() {
        let (mut supervisor, destination) = build_supervisor(RecordingAllow { calls: 0 }, 25);
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);
        let mut rng = CounterRng::default();
        let queued = match supervisor
            .try_prepare_and_submit_data(&mut clock, request(destination, 100_000), &mut rng)
            .expect("preparation must succeed")
        {
            NodeTxPrepareResult::Queued(queued) => queued,
            other => panic!("fresh preparation was not queued: {other:?}"),
        };

        supervisor
            .owner
            .ingest(
                &proof_for(26, queued.attempt()),
                MonotonicSeconds::new(33),
                PacketInterfaceId::new(1),
                &mut rng,
            )
            .unwrap();
        let terminal = supervisor
            .terminal_attempts()
            .next()
            .expect("timeout must be visible through the facade");
        assert_eq!(terminal.handle(), queued.attempt_handle());
        assert_eq!(terminal.outcome(), AttemptOutcome::Delivered);
        assert_eq!(
            supervisor.acknowledge_terminal(terminal.handle()),
            Err(AcknowledgeError::PacketStillBound)
        );

        assert!(drive_to_quiescence(&mut supervisor, &mut clock) <= 16);
        assert_eq!(supervisor.policy.calls, 0);
        assert_eq!(supervisor.parked_counts().available(), 1);
        assert_eq!(
            supervisor
                .acknowledge_terminal(terminal.handle())
                .expect("returned owner must make the durable terminal acknowledgeable"),
            terminal
        );
    }

    #[test]
    fn exact_owner_deadline_recovers_and_retains_unacknowledged_owner() {
        let (mut supervisor, destination) = build_supervisor(RfInertTxPolicy, 30);
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);
        let queued = match supervisor
            .try_prepare_and_submit_data(
                &mut clock,
                request(destination, 1_100),
                &mut CounterRng::default(),
            )
            .expect("preparation must succeed")
        {
            NodeTxPrepareResult::Queued(queued) => queued,
            other => panic!("fresh preparation was not queued: {other:?}"),
        };

        assert!(supervisor.run_one_pass(&mut clock).progressed());
        assert_eq!(supervisor.next_wake(), Some(MonotonicMillis::new(1_100)));
        clock.now = 1_100;
        let deadline_pass = supervisor.run_one_pass(&mut clock);
        assert_eq!(
            deadline_pass.maintenance(),
            Some(TxMaintenanceReport {
                newly_recovery_required: 1,
            })
        );
        assert!(deadline_pass.progressed());
        assert!(drive_to_quiescence(&mut supervisor, &mut clock) <= 8);

        assert_eq!(supervisor.parked_counts().available(), 0);
        assert_eq!(supervisor.parked_counts().recovered(), 1);
        assert_eq!(supervisor.recovered_records().count(), 1);
        let observation = supervisor
            .recovered_observations()
            .next()
            .expect("recovered owner must retain correlated observation");
        assert_eq!(observation.attempt_handle(), queued.attempt_handle());
        assert_eq!(observation.attempt(), queued.attempt());
        assert_eq!(supervisor.next_wake(), None);
        assert!(!supervisor.faults().is_faulted());
        supervisor
            .acknowledge_recovered(observation)
            .expect("supervisor must forward exact recovered acknowledgement");
        assert_eq!(supervisor.parked_counts().available(), 1);
        assert_eq!(supervisor.recovered_observations().count(), 0);
    }

    #[test]
    fn clock_regression_stops_remaining_and_future_transitions() {
        let (mut supervisor, destination) = build_supervisor(RfInertTxPolicy, 40);
        let samples = [100, 101, 99];
        let mut clock = ScriptClock {
            samples: &samples,
            next: 0,
        };

        let pass = supervisor.run_one_pass(&mut clock);
        let regression = pass
            .faults()
            .clock()
            .expect("regressing sample must be retained");
        assert_eq!(regression.previous(), MonotonicMillis::new(101));
        assert_eq!(regression.observed(), MonotonicMillis::new(99));
        assert_eq!(pass.clock_samples(), 3);
        assert!(pass.maintenance().is_some());
        assert!(pass.data().is_some());
        assert!(pass.permit().is_none());
        assert!(pass.dispatcher().is_none());

        let stopped = supervisor.run_one_pass(&mut clock);
        assert_eq!(stopped.clock_samples(), 0);
        assert_eq!(clock.next, 3);
        assert!(
            supervisor
                .try_prepare_and_submit_data(
                    &mut clock,
                    request(destination, 1_000),
                    &mut CounterRng::default(),
                )
                .is_err()
        );
        assert_eq!(clock.next, 3);
    }

    #[test]
    fn fresh_permit_sample_crossing_deadline_never_calls_policy_or_inspects() {
        let (mut supervisor, destination) = build_supervisor(RecordingAllow { calls: 0 }, 45);
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);
        assert!(matches!(
            supervisor
                .try_prepare_and_submit_data(
                    &mut clock,
                    request(destination, 1_100),
                    &mut CounterRng::default(),
                )
                .expect("preparation must succeed"),
            NodeTxPrepareResult::Queued(_)
        ));
        for _ in 0..3 {
            assert_eq!(
                supervisor.dispatcher.step(MonotonicMillis::new(1_000)),
                NoRfTxDispatcherStep::Advanced
            );
        }
        assert_eq!(
            supervisor.permit.step(
                &mut supervisor.owner,
                MonotonicMillis::new(1_000),
                &mut supervisor.policy
            ),
            TxPermitServerStep::Advanced
        );
        assert_eq!(supervisor.permit_phase(), TxPermitServerPhase::Request);

        let samples = [1_099, 1_099, 1_100, 1_100];
        let mut crossing = ScriptClock {
            samples: &samples,
            next: 0,
        };
        let pass = supervisor.run_one_pass(&mut crossing);

        assert_eq!(crossing.next, 4);
        assert_eq!(
            pass.maintenance(),
            Some(TxMaintenanceReport {
                newly_recovery_required: 0,
            })
        );
        assert_eq!(pass.permit(), Some(TxPermitServerStep::Advanced));
        assert_eq!(
            pass.dispatcher(),
            Some(NoRfTxDispatcherStep::NeedPermitReply {
                grace_deadline: MonotonicMillis::new(1_125),
            })
        );
        assert_eq!(supervisor.policy.calls, 0);
        assert_eq!(
            supervisor.dispatcher_phase(),
            NoRfTxDispatcherPhase::PermitWait
        );
        assert_eq!(supervisor.next_wake(), Some(MonotonicMillis::new(1_125)));

        let mut at_deadline = ManualClock::at(1_100);
        drive_to_quiescence(&mut supervisor, &mut at_deadline);
        assert_eq!(supervisor.policy.calls, 0);
        assert_eq!(supervisor.parked_counts().recovered(), 1);
        assert_eq!(supervisor.recovered_records().count(), 1);
    }

    #[test]
    fn cancelling_combined_wait_preserves_later_progress() {
        let (mut supervisor, destination) = build_supervisor(RfInertTxPolicy, 50);
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);

        {
            let mut wait = pin!(supervisor.wait_for_work(&mut clock));
            let mut context = Context::from_waker(Waker::noop());
            assert_eq!(wait.as_mut().poll(&mut context), Poll::Pending);
        }

        assert!(matches!(
            supervisor
                .try_prepare_and_submit_data(
                    &mut clock,
                    request(destination, 10_000),
                    &mut CounterRng::default(),
                )
                .expect("cancelled short wait must not fault the aggregate"),
            NodeTxPrepareResult::Queued(_)
        ));
        drive_to_quiescence(&mut supervisor, &mut clock);
        assert_eq!(supervisor.parked_counts().available(), 1);
        assert!(!supervisor.faults().is_faulted());
    }

    #[test]
    fn observable_reply_wins_a_simultaneously_ready_timer() {
        let (mut supervisor, destination) = build_supervisor(RecordingAllow { calls: 0 }, 55);
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);
        assert!(matches!(
            supervisor
                .try_prepare_and_submit_data(
                    &mut clock,
                    request(destination, 1_100),
                    &mut CounterRng::default(),
                )
                .expect("preparation must succeed"),
            NodeTxPrepareResult::Queued(_)
        ));

        assert_eq!(
            supervisor.dispatcher.step(MonotonicMillis::new(1_000)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            supervisor.dispatcher.step(MonotonicMillis::new(1_000)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            supervisor.dispatcher.step(MonotonicMillis::new(1_000)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            supervisor.permit.step(
                &mut supervisor.owner,
                MonotonicMillis::new(1_000),
                &mut supervisor.policy
            ),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            supervisor.permit.step(
                &mut supervisor.owner,
                MonotonicMillis::new(1_000),
                &mut supervisor.policy
            ),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            supervisor.permit.step(
                &mut supervisor.owner,
                MonotonicMillis::new(1_000),
                &mut supervisor.policy
            ),
            TxPermitServerStep::Advanced
        );
        assert_eq!(supervisor.policy.calls, 1);
        assert_eq!(
            supervisor.dispatcher_phase(),
            NoRfTxDispatcherPhase::PermitWait
        );

        let mut ready_clock = ReadyTimerClock(ManualClock::at(1_100));
        embassy_futures::block_on(supervisor.wait_for_work(&mut ready_clock));
        assert_eq!(
            supervisor.dispatcher_phase(),
            NoRfTxDispatcherPhase::PermitReply
        );
    }

    #[test]
    fn dispatcher_fault_stops_policy_and_drains_exact_owner_to_quarantine() {
        let (mut supervisor, destination) = build_supervisor(RecordingAllow { calls: 0 }, 57);
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);
        let queued = match supervisor
            .try_prepare_and_submit_data(
                &mut clock,
                request(destination, 1_100),
                &mut CounterRng::default(),
            )
            .expect("preparation must succeed")
        {
            NodeTxPrepareResult::Queued(queued) => queued,
            other => panic!("fresh preparation was not queued: {other:?}"),
        };
        for _ in 0..3 {
            assert_eq!(
                supervisor.dispatcher.step(MonotonicMillis::new(1_000)),
                NoRfTxDispatcherStep::Advanced
            );
        }
        assert_eq!(
            supervisor.dispatcher_phase(),
            NoRfTxDispatcherPhase::PermitWait
        );
        assert_eq!(supervisor.next_wake(), Some(MonotonicMillis::new(1_100)));

        clock.now = 1_100;
        let deadline = supervisor.run_one_pass(&mut clock);
        assert_eq!(
            deadline.maintenance(),
            Some(TxMaintenanceReport {
                newly_recovery_required: 1,
            })
        );
        assert_eq!(
            deadline.dispatcher(),
            Some(NoRfTxDispatcherStep::NeedPermitReply {
                grace_deadline: MonotonicMillis::new(1_125),
            })
        );
        assert!(!supervisor.faults().is_faulted());
        assert_eq!(supervisor.policy.calls, 0);
        assert_eq!(supervisor.next_wake(), Some(MonotonicMillis::new(1_125)));

        clock.now = 1_125;
        let faulting = supervisor.run_one_pass(&mut clock);
        assert!(faulting.progressed());
        assert_eq!(
            supervisor.faults().dispatcher(),
            Some(TxDispatcherFault::PermitReplyGraceExpired)
        );
        assert_eq!(supervisor.policy.calls, 0);
        drive_to_quiescence(&mut supervisor, &mut clock);

        assert_eq!(supervisor.policy.calls, 0);
        assert_eq!(supervisor.permit_phase(), TxPermitServerPhase::Reply);
        assert_eq!(
            supervisor.dispatcher_phase(),
            NoRfTxDispatcherPhase::Disabled
        );
        assert_eq!(supervisor.parked_counts().available(), 0);
        assert_eq!(supervisor.parked_counts().recovered(), 0);
        assert_eq!(supervisor.parked_counts().quarantined(), 1);
        assert_eq!(supervisor.quarantine_records().count(), 1);
        let observation = supervisor
            .quarantine_observations()
            .next()
            .expect("quarantine must remain correlated to its attempt");
        assert_eq!(observation.attempt_handle(), queued.attempt_handle());
        assert_eq!(observation.attempt(), queued.attempt());
    }

    #[test]
    fn embassy_deadline_conversion_never_rounds_earlier_and_saturates() {
        let logical = MonotonicMillis::new(1_234);
        let converted = embassy_instant_for_millis(logical);
        assert!(converted.as_millis() >= logical.get());
        assert_eq!(
            embassy_instant_for_millis(MonotonicMillis::new(u64::MAX)),
            Instant::MAX
        );
    }

    #[test]
    fn incomplete_machine_set_returns_common_origin_roles_for_seeding() {
        let mut owner = node(59, "retry-seeding-owner");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        owner
            .register_packet_buffer(buffer)
            .expect("retry-seeding buffer must register");
        let storage = Box::leak(Box::new(TxHandoff::<NoopRawMutex, 1>::new()));
        let error = NoRfTxMachineSet::try_new(storage.split_paired(), config())
            .err()
            .expect("an unseeded role set must be returned");
        assert_eq!(error.seeds_remaining(), 1);

        let (mut handoff, config) = error.into_parts();
        handoff
            .try_seed_available(buffer)
            .unwrap_or_else(|_| panic!("recovered seed capability must remain usable"));
        let machines = NoRfTxMachineSet::try_new(handoff, config)
            .unwrap_or_else(|_| panic!("fully seeded retry must build"));
        let mut supervisor = TxSupervisor::new(owner, machines, RfInertTxPolicy);
        let mut clock = ManualClock::at(1_000);
        seed(&mut supervisor, &mut clock);
        assert_eq!(supervisor.parked_counts().available(), 1);
    }

    #[test]
    fn complete_aggregate_fits_permanent_production_mutex_static_storage() {
        let mut owner = node(60, "production-static-owner");
        let buffer = STATIC_BUFFER.init(TxPacketBuffer::new());
        owner
            .register_packet_buffer(buffer)
            .expect("production-static buffer must register");
        let mut handoff = STATIC_HANDOFF.take().split_paired();
        handoff
            .try_seed_available(buffer)
            .unwrap_or_else(|_| panic!("production-static seed must fit"));
        let machines = NoRfTxMachineSet::try_new(handoff, config())
            .unwrap_or_else(|_| panic!("production-static handoff must be fully seeded"));
        let supervisor =
            STATIC_SUPERVISOR.init(TxSupervisor::new(owner, machines, RfInertTxPolicy));

        assert_eq!(supervisor.data_phase(), NodeTxDataPhase::Seeding);
        assert!(!supervisor.faults().is_faulted());
        let _: &'static StaticCell<ProductionSupervisor> = &PRODUCTION_SUPERVISOR;
    }
}
