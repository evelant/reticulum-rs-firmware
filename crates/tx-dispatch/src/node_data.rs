//! Persistent node-side ownership return and serialized fan-out handling.

use core::{array, future::poll_fn, mem};

use embassy_sync::blocking_mutex::raw::RawMutex;
use reticulum_node_core::{
    AttemptToken, AvailableBufferError, MonotonicMillis, NodeCore, PacketInterfaceId, PacketSlotId,
    RoutedTxJob, TxCompletion, TxCompletionDisposition, TxCompletionErrorKind, TxCompletionFailure,
    TxLeaseDeadline, TxOwnerScope, TxPacketBuffer, TxQuarantine, TxRecoveryRecord,
};
use reticulum_tx_handoff::TxOwnerReturn;

use crate::NodeTxDataHandoff;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NodeTxDataMode {
    Seeding { parked: usize },
    Running,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AvailableSource {
    Seed,
    Completion,
}

enum ParkCandidate {
    Available {
        buffer: &'static mut TxPacketBuffer,
        source: AvailableSource,
    },
    Recovered {
        buffer: &'static mut TxPacketBuffer,
        record: TxRecoveryRecord,
    },
    Quarantined(TxQuarantine<'static>),
}

impl ParkCandidate {
    fn residue_kind(&self) -> NodeTxDataFaultResidueKind {
        match self {
            Self::Available { .. } => NodeTxDataFaultResidueKind::AvailableBuffer,
            Self::Recovered { .. } => NodeTxDataFaultResidueKind::RecoveredBuffer,
            Self::Quarantined(_) => NodeTxDataFaultResidueKind::Quarantine,
        }
    }

    fn slot<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const POOL_SIZE: usize,
    >(
        &self,
        owner: &NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, POOL_SIZE>,
    ) -> Result<PacketSlotId, NodeTxDataFault> {
        match self {
            Self::Available { buffer, .. } => owner
                .validate_available_buffer(buffer)
                .map_err(NodeTxDataFault::AvailableBuffer),
            Self::Recovered { buffer, record } => {
                let slot = owner
                    .validate_available_buffer(buffer)
                    .map_err(NodeTxDataFault::AvailableBuffer)?;
                if slot == record.slot_id() {
                    Ok(slot)
                } else {
                    Err(NodeTxDataFault::RecoveryRecordMismatch)
                }
            }
            Self::Quarantined(quarantine) => {
                let slot = quarantine.slot_id();
                if slot == quarantine.record().slot_id() {
                    Ok(slot)
                } else {
                    Err(NodeTxDataFault::RecoveryRecordMismatch)
                }
            }
        }
    }
}

enum ParkedOwner {
    Empty,
    Available(&'static mut TxPacketBuffer),
    Recovered {
        buffer: &'static mut TxPacketBuffer,
        record: TxRecoveryRecord,
    },
    Quarantined(TxQuarantine<'static>),
}

enum DisabledOwner {
    Candidate(ParkCandidate),
    CompletionFailure(TxCompletionFailure<'static>),
    UnexpectedCompletion(TxCompletion<'static>),
}

impl DisabledOwner {
    fn kind(&self) -> NodeTxDataFaultResidueKind {
        match self {
            Self::Candidate(candidate) => candidate.residue_kind(),
            Self::CompletionFailure(failure) => {
                let _ = failure.reason();
                NodeTxDataFaultResidueKind::CompletionFailure
            }
            Self::UnexpectedCompletion(completion) => {
                let _ = completion;
                NodeTxDataFaultResidueKind::Completion
            }
        }
    }
}

enum NodeTxDataState {
    Idle,
    Returned(TxOwnerReturn),
    Parking(ParkCandidate),
    Next(RoutedTxJob<'static>),
    Disabled {
        fault: NodeTxDataFault,
        retained: DisabledOwner,
    },
    Poisoned,
}

/// Persistent node-side DATA-owner phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeTxDataPhase {
    /// Registered packet buffers are still being validated and parked.
    Seeding,
    /// All boot seeds are parked and no return or continuation is retained.
    Idle,
    /// One exact handoff return is stored for node-core processing.
    Returned,
    /// One exact terminal owner is stored for per-slot parking.
    Parking,
    /// One serialized continuation job is retained until the job channel has capacity.
    NextPending,
    /// The machine failed closed while retaining every unique owner involved.
    Disabled,
}

/// Copy-only scalar metadata for a retained or queued serialized continuation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeTxQueuedHop {
    slot: PacketSlotId,
    attempt: AttemptToken,
    interface: PacketInterfaceId,
    packet_len: u16,
    deadline: TxLeaseDeadline,
}

impl NodeTxQueuedHop {
    fn from_job(job: &RoutedTxJob<'_>) -> Self {
        Self {
            slot: job.slot_id(),
            attempt: job.attempt(),
            interface: job.interface(),
            packet_len: job.packet_len(),
            deadline: job.deadline(),
        }
    }

    /// Stable packet-buffer slot.
    pub const fn slot_id(self) -> PacketSlotId {
        self.slot
    }

    /// End-to-end attempt correlation token.
    pub const fn attempt(self) -> AttemptToken {
        self.attempt
    }

    /// Interface selected for this serialized hop.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Encoded Reticulum packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// Exact packet-owner lease deadline.
    pub const fn deadline(self) -> TxLeaseDeadline {
        self.deadline
    }
}

/// Fail-closed reason retained by the node-side DATA-owner machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeTxDataFault {
    /// A completion arrived before every registered boot seed was parked.
    CompletionDuringSeeding,
    /// An available buffer arrived after the one-time boot seed phase.
    AvailableAfterSeeding,
    /// Node-core rejected a supposedly reusable packet buffer.
    AvailableBuffer(AvailableBufferError),
    /// A slot identifier does not fit this machine's fixed owner table.
    SlotOutsidePool(PacketSlotId),
    /// A second unique owner claimed an already occupied table slot.
    SlotOccupied(PacketSlotId),
    /// A recovered or quarantined owner disagreed with its scalar recovery record.
    RecoveryRecordMismatch,
    /// Node-core rejected an owning completion without consuming it.
    Completion(TxCompletionErrorKind),
    /// Private mode and transition state disagreed.
    InternalInvariant,
}

/// Kind of exact non-`Copy` value retained after a fail-closed transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeTxDataFaultResidueKind {
    /// A reusable buffer remains retained.
    AvailableBuffer,
    /// A recovered reusable buffer and its record remain retained.
    RecoveredBuffer,
    /// A recovery quarantine remains retained.
    Quarantine,
    /// An unprocessed owning completion remains retained.
    Completion,
    /// A node-core completion failure remains retained with its completion.
    CompletionFailure,
}

/// Kind of owner currently parked in one fixed slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeTxParkedKind {
    /// A reusable packet buffer is available.
    Available,
    /// A reusable buffer awaits exact recovery-record acknowledgement.
    Recovered,
    /// A unique buffer remains fail-closed in recovery quarantine.
    Quarantined,
}

/// Scalar occupancy counts for the fixed owner table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeTxParkedCounts {
    available: usize,
    recovered: usize,
    quarantined: usize,
}

impl NodeTxParkedCounts {
    /// Reusable available-buffer count.
    pub const fn available(self) -> usize {
        self.available
    }

    /// Recovered buffers awaiting exact acknowledgement.
    pub const fn recovered(self) -> usize {
        self.recovered
    }

    /// Fail-closed quarantined-buffer count.
    pub const fn quarantined(self) -> usize {
        self.quarantined
    }

    /// Total occupied owner slots.
    pub const fn occupied(self) -> usize {
        self.available + self.recovered + self.quarantined
    }
}

/// Result of one non-awaiting node-side DATA-owner transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "node-side ownership progress and faults must be handled"]
pub enum NodeTxDataStep {
    /// One owner-preserving internal transition completed.
    Advanced,
    /// No boot seed is queued; the scalar is the number still required.
    NeedSeed(usize),
    /// Bootstrap is complete and no owner return is queued.
    NeedReturn,
    /// The supplied node does not match the owner bound at construction; state
    /// and queued returns remain unchanged.
    OwnerMismatch,
    /// One boot seed was parked; the scalar is the number still required.
    SeedParked {
        /// Stable slot assigned by node-core.
        slot: PacketSlotId,
        /// Number of registered seeds still required.
        remaining: usize,
    },
    /// One final reusable owner was parked.
    AvailableParked(PacketSlotId),
    /// One recovered buffer and its unacknowledged scalar record were parked.
    RecoveredParked(TxRecoveryRecord),
    /// One fail-closed quarantine was parked.
    QuarantinedParked(TxRecoveryRecord),
    /// One serialized continuation was accepted by the job channel.
    NextQueued(NodeTxQueuedHop),
    /// The exact serialized continuation remains retained under pressure.
    NextBackpressured(NodeTxQueuedHop),
    /// The machine failed closed and retained the exact non-`Copy` residue.
    Disabled(NodeTxDataFault),
}

/// Result of the machine's short cancellation-safe progress wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "the caller must resume synchronous node-side ownership stepping"]
pub enum NodeTxDataWait {
    /// One exact owner return was stored before the wait completed.
    ReturnStored,
    /// The job channel reported capacity for the retained serialized continuation.
    JobCapacityReady,
    /// The current phase has no useful channel wait.
    NotWaiting,
    /// The machine was already disabled.
    Disabled(NodeTxDataFault),
}

/// Exact recovered-record acknowledgement failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeTxRecoveryAckError {
    /// The owner machine is fail-closed and permits no table mutation.
    MachineDisabled,
    /// The record's slot does not fit this fixed owner table.
    SlotOutsidePool,
    /// The addressed slot does not contain a recovered owner.
    NotRecovered,
    /// The current recovered record differs from the exact supplied record.
    RecordMismatch,
}

/// Persistent fixed-capacity owner-return and serialized-fan-out machine.
///
/// Construction with a nonzero pool begins a one-time seed phase. Exactly
/// `POOL_SIZE` registered available buffers must arrive through the
/// owner-return channel before ordinary completions are accepted. A zero-size
/// profile is vacuously seeded, although [`reticulum_tx_handoff::TxHandoff`]
/// deliberately requires a nonempty product handoff. All reusable, recovered,
/// and quarantined owners remain in an internal per-slot table; public progress
/// values expose scalars only. A future synchronous preparation boundary will
/// consume available table entries without moving raw owners into async
/// futures.
///
/// `TxCompletionDisposition::Next` is never rolled back. It remains in this
/// machine and is retried unchanged until the sole job channel accepts it.
#[must_use = "dropping the node DATA machine abandons channel roles and unique packet owners"]
pub struct NodeTxDataMachine<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    handoff: NodeTxDataHandoff<M, POOL_SIZE>,
    owner_scope: TxOwnerScope,
    mode: NodeTxDataMode,
    state: NodeTxDataState,
    parked: [ParkedOwner; POOL_SIZE],
}

impl<M, const POOL_SIZE: usize> NodeTxDataMachine<M, POOL_SIZE>
where
    M: RawMutex + 'static,
{
    /// Consume the sole node DATA handoff roles, bind one exact node owner,
    /// and begin boot seeding.
    pub fn new<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        handoff: NodeTxDataHandoff<M, POOL_SIZE>,
        owner: &NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, POOL_SIZE>,
    ) -> Self {
        Self {
            handoff,
            owner_scope: owner.tx_owner_scope(),
            mode: if POOL_SIZE == 0 {
                NodeTxDataMode::Running
            } else {
                NodeTxDataMode::Seeding { parked: 0 }
            },
            state: NodeTxDataState::Idle,
            parked: array::from_fn(|_| ParkedOwner::Empty),
        }
    }

    /// Current persistent machine phase.
    pub const fn phase(&self) -> NodeTxDataPhase {
        match self.state {
            NodeTxDataState::Idle => match self.mode {
                NodeTxDataMode::Seeding { .. } => NodeTxDataPhase::Seeding,
                NodeTxDataMode::Running => NodeTxDataPhase::Idle,
            },
            NodeTxDataState::Returned(_) => NodeTxDataPhase::Returned,
            NodeTxDataState::Parking(_) => NodeTxDataPhase::Parking,
            NodeTxDataState::Next(_) => NodeTxDataPhase::NextPending,
            NodeTxDataState::Disabled { .. } | NodeTxDataState::Poisoned => {
                NodeTxDataPhase::Disabled
            }
        }
    }

    /// Number of registered available-buffer seeds still required at boot.
    pub const fn seeds_remaining(&self) -> usize {
        match self.mode {
            NodeTxDataMode::Seeding { parked } => POOL_SIZE - parked,
            NodeTxDataMode::Running => 0,
        }
    }

    /// Whether every fixed packet-buffer owner has been validated and parked.
    pub const fn is_seeded(&self) -> bool {
        matches!(self.mode, NodeTxDataMode::Running)
    }

    /// Retained fail-closed reason, if any.
    pub fn fault(&self) -> Option<NodeTxDataFault> {
        match &self.state {
            NodeTxDataState::Disabled { fault, .. } => Some(*fault),
            NodeTxDataState::Poisoned => Some(NodeTxDataFault::InternalInvariant),
            NodeTxDataState::Idle
            | NodeTxDataState::Returned(_)
            | NodeTxDataState::Parking(_)
            | NodeTxDataState::Next(_) => None,
        }
    }

    /// Kind of exact owning residue retained by a fail-closed transition.
    pub fn fault_residue_kind(&self) -> Option<NodeTxDataFaultResidueKind> {
        match &self.state {
            NodeTxDataState::Disabled { retained, .. } => Some(retained.kind()),
            NodeTxDataState::Idle
            | NodeTxDataState::Returned(_)
            | NodeTxDataState::Parking(_)
            | NodeTxDataState::Next(_)
            | NodeTxDataState::Poisoned => None,
        }
    }

    /// Copy-only occupancy counts for the internal fixed owner table.
    pub fn parked_counts(&self) -> NodeTxParkedCounts {
        let mut counts = NodeTxParkedCounts {
            available: 0,
            recovered: 0,
            quarantined: 0,
        };
        for owner in &self.parked {
            match owner {
                ParkedOwner::Empty => {}
                ParkedOwner::Available(buffer) => {
                    let _ = buffer.slot_id();
                    counts.available += 1;
                }
                ParkedOwner::Recovered { .. } => counts.recovered += 1,
                ParkedOwner::Quarantined(_) => counts.quarantined += 1,
            }
        }
        counts
    }

    /// Copy-only owner kind parked at one stable slot.
    pub fn parked_kind(&self, slot: PacketSlotId) -> Option<NodeTxParkedKind> {
        match self.parked.get(usize::from(slot.get()))? {
            ParkedOwner::Empty => None,
            ParkedOwner::Available(_) => Some(NodeTxParkedKind::Available),
            ParkedOwner::Recovered { .. } => Some(NodeTxParkedKind::Recovered),
            ParkedOwner::Quarantined(_) => Some(NodeTxParkedKind::Quarantined),
        }
    }

    /// Copy the unacknowledged recovery record parked at one slot.
    pub fn recovered_record(&self, slot: PacketSlotId) -> Option<TxRecoveryRecord> {
        match self.parked.get(usize::from(slot.get()))? {
            ParkedOwner::Recovered { record, .. } => Some(*record),
            ParkedOwner::Empty | ParkedOwner::Available(_) | ParkedOwner::Quarantined(_) => None,
        }
    }

    /// Iterate every unacknowledged recovered record in stable slot order.
    pub fn recovered_records(&self) -> impl Iterator<Item = TxRecoveryRecord> + '_ {
        self.parked.iter().filter_map(|owner| match owner {
            ParkedOwner::Recovered { record, .. } => Some(*record),
            ParkedOwner::Empty | ParkedOwner::Available(_) | ParkedOwner::Quarantined(_) => None,
        })
    }

    /// Copy the fail-closed quarantine record parked at one slot.
    pub fn quarantine_record(&self, slot: PacketSlotId) -> Option<TxRecoveryRecord> {
        match self.parked.get(usize::from(slot.get()))? {
            ParkedOwner::Quarantined(quarantine) => Some(quarantine.record()),
            ParkedOwner::Empty | ParkedOwner::Available(_) | ParkedOwner::Recovered { .. } => None,
        }
    }

    /// Iterate every fail-closed quarantine record in stable slot order.
    pub fn quarantine_records(&self) -> impl Iterator<Item = TxRecoveryRecord> + '_ {
        self.parked.iter().filter_map(|owner| match owner {
            ParkedOwner::Quarantined(quarantine) => Some(quarantine.record()),
            ParkedOwner::Empty | ParkedOwner::Available(_) | ParkedOwner::Recovered { .. } => None,
        })
    }

    /// Acknowledge one exact recovered record after durable projection.
    ///
    /// The full generation-scoped record must still match the parked owner;
    /// stale slot-only acknowledgements cannot release a later recovery after
    /// slot reuse.
    pub fn acknowledge_recovered(
        &mut self,
        record: TxRecoveryRecord,
    ) -> Result<(), NodeTxRecoveryAckError> {
        if matches!(
            self.state,
            NodeTxDataState::Disabled { .. } | NodeTxDataState::Poisoned
        ) {
            return Err(NodeTxRecoveryAckError::MachineDisabled);
        }
        let Some(owner) = self.parked.get_mut(usize::from(record.slot_id().get())) else {
            return Err(NodeTxRecoveryAckError::SlotOutsidePool);
        };
        match owner {
            ParkedOwner::Recovered {
                record: current, ..
            } if *current != record => Err(NodeTxRecoveryAckError::RecordMismatch),
            ParkedOwner::Recovered { .. } => {
                let parked = mem::replace(owner, ParkedOwner::Empty);
                let ParkedOwner::Recovered { buffer, .. } = parked else {
                    unreachable!("matched recovered owner changed during synchronous transition")
                };
                *owner = ParkedOwner::Available(buffer);
                Ok(())
            }
            ParkedOwner::Empty | ParkedOwner::Available(_) | ParkedOwner::Quarantined(_) => {
                Err(NodeTxRecoveryAckError::NotRecovered)
            }
        }
    }

    /// Run exactly one non-awaiting ownership transition with a fresh node
    /// clock sample.
    pub fn step<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, POOL_SIZE>,
        now: MonotonicMillis,
    ) -> NodeTxDataStep {
        if owner.tx_owner_scope() != self.owner_scope {
            return NodeTxDataStep::OwnerMismatch;
        }
        let state = mem::replace(&mut self.state, NodeTxDataState::Poisoned);
        match state {
            NodeTxDataState::Idle => self.step_idle(),
            NodeTxDataState::Returned(returned) => self.step_returned(owner, returned, now),
            NodeTxDataState::Parking(candidate) => self.step_parking(owner, candidate),
            NodeTxDataState::Next(job) => self.step_next(job),
            NodeTxDataState::Disabled { fault, retained } => {
                self.state = NodeTxDataState::Disabled { fault, retained };
                NodeTxDataStep::Disabled(fault)
            }
            NodeTxDataState::Poisoned => {
                self.state = NodeTxDataState::Poisoned;
                NodeTxDataStep::Disabled(NodeTxDataFault::InternalInvariant)
            }
        }
    }

    /// Await only channel readiness while all non-`Copy` owners stay in
    /// persistent machine state.
    ///
    /// In an idle phase, cancellation while pending leaves the return queued;
    /// a ready return is assigned to state before this future completes. Under
    /// job pressure, the pending `Next` job never enters the readiness future.
    pub async fn wait_for_progress(&mut self) -> NodeTxDataWait {
        match self.phase() {
            NodeTxDataPhase::Seeding | NodeTxDataPhase::Idle => {
                let returned = self.handoff.returns.receive().await;
                self.state = NodeTxDataState::Returned(returned);
                NodeTxDataWait::ReturnStored
            }
            NodeTxDataPhase::NextPending => {
                poll_fn(|context| self.handoff.jobs.poll_ready_to_send(context)).await;
                NodeTxDataWait::JobCapacityReady
            }
            NodeTxDataPhase::Disabled => match &self.state {
                NodeTxDataState::Disabled { fault, .. } => NodeTxDataWait::Disabled(*fault),
                NodeTxDataState::Poisoned => {
                    NodeTxDataWait::Disabled(NodeTxDataFault::InternalInvariant)
                }
                NodeTxDataState::Idle
                | NodeTxDataState::Returned(_)
                | NodeTxDataState::Parking(_)
                | NodeTxDataState::Next(_) => NodeTxDataWait::NotWaiting,
            },
            NodeTxDataPhase::Returned | NodeTxDataPhase::Parking => NodeTxDataWait::NotWaiting,
        }
    }

    fn step_idle(&mut self) -> NodeTxDataStep {
        match self.handoff.returns.try_receive() {
            Some(returned) => {
                self.state = NodeTxDataState::Returned(returned);
                NodeTxDataStep::Advanced
            }
            None => {
                self.state = NodeTxDataState::Idle;
                match self.mode {
                    NodeTxDataMode::Seeding { parked } => {
                        NodeTxDataStep::NeedSeed(POOL_SIZE - parked)
                    }
                    NodeTxDataMode::Running => NodeTxDataStep::NeedReturn,
                }
            }
        }
    }

    fn step_returned<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, POOL_SIZE>,
        returned: TxOwnerReturn,
        now: MonotonicMillis,
    ) -> NodeTxDataStep {
        match (self.mode, returned) {
            (NodeTxDataMode::Seeding { .. }, TxOwnerReturn::Available(buffer)) => {
                self.state = NodeTxDataState::Parking(ParkCandidate::Available {
                    buffer,
                    source: AvailableSource::Seed,
                });
                NodeTxDataStep::Advanced
            }
            (NodeTxDataMode::Seeding { .. }, TxOwnerReturn::Completion(completion)) => self
                .disable(
                    NodeTxDataFault::CompletionDuringSeeding,
                    DisabledOwner::UnexpectedCompletion(completion),
                ),
            (NodeTxDataMode::Running, TxOwnerReturn::Available(buffer)) => self.disable(
                NodeTxDataFault::AvailableAfterSeeding,
                DisabledOwner::Candidate(ParkCandidate::Available {
                    buffer,
                    source: AvailableSource::Completion,
                }),
            ),
            (NodeTxDataMode::Running, TxOwnerReturn::Completion(completion)) => {
                match owner.complete_tx(completion, now) {
                    Ok(TxCompletionDisposition::Next(job)) => {
                        self.state = NodeTxDataState::Next(job);
                        NodeTxDataStep::Advanced
                    }
                    Ok(TxCompletionDisposition::Available(buffer)) => {
                        self.state = NodeTxDataState::Parking(ParkCandidate::Available {
                            buffer,
                            source: AvailableSource::Completion,
                        });
                        NodeTxDataStep::Advanced
                    }
                    Ok(TxCompletionDisposition::Recovered { buffer, record }) => {
                        self.state =
                            NodeTxDataState::Parking(ParkCandidate::Recovered { buffer, record });
                        NodeTxDataStep::Advanced
                    }
                    Ok(TxCompletionDisposition::Quarantined(quarantine)) => {
                        self.state =
                            NodeTxDataState::Parking(ParkCandidate::Quarantined(quarantine));
                        NodeTxDataStep::Advanced
                    }
                    Err(failure) => {
                        let fault = NodeTxDataFault::Completion(failure.reason());
                        self.disable(fault, DisabledOwner::CompletionFailure(failure))
                    }
                }
            }
        }
    }

    fn step_parking<
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
    >(
        &mut self,
        owner: &NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, POOL_SIZE>,
        candidate: ParkCandidate,
    ) -> NodeTxDataStep {
        let slot = match candidate.slot(owner) {
            Ok(slot) => slot,
            Err(fault) => return self.disable(fault, DisabledOwner::Candidate(candidate)),
        };
        let index = usize::from(slot.get());
        let Some(current) = self.parked.get(index) else {
            return self.disable(
                NodeTxDataFault::SlotOutsidePool(slot),
                DisabledOwner::Candidate(candidate),
            );
        };
        if !matches!(current, ParkedOwner::Empty) {
            return self.disable(
                NodeTxDataFault::SlotOccupied(slot),
                DisabledOwner::Candidate(candidate),
            );
        }

        match candidate {
            ParkCandidate::Available { buffer, source } => {
                if (source == AvailableSource::Seed)
                    != matches!(self.mode, NodeTxDataMode::Seeding { .. })
                {
                    return self.disable(
                        NodeTxDataFault::InternalInvariant,
                        DisabledOwner::Candidate(ParkCandidate::Available { buffer, source }),
                    );
                }
                self.parked[index] = ParkedOwner::Available(buffer);
                self.state = NodeTxDataState::Idle;
                match self.mode {
                    NodeTxDataMode::Seeding { parked } => {
                        let parked = parked + 1;
                        let remaining = POOL_SIZE - parked;
                        self.mode = if remaining == 0 {
                            NodeTxDataMode::Running
                        } else {
                            NodeTxDataMode::Seeding { parked }
                        };
                        NodeTxDataStep::SeedParked { slot, remaining }
                    }
                    NodeTxDataMode::Running => NodeTxDataStep::AvailableParked(slot),
                }
            }
            ParkCandidate::Recovered { buffer, record } => {
                if !matches!(self.mode, NodeTxDataMode::Running) {
                    return self.disable(
                        NodeTxDataFault::InternalInvariant,
                        DisabledOwner::Candidate(ParkCandidate::Recovered { buffer, record }),
                    );
                }
                self.parked[index] = ParkedOwner::Recovered { buffer, record };
                self.state = NodeTxDataState::Idle;
                NodeTxDataStep::RecoveredParked(record)
            }
            ParkCandidate::Quarantined(quarantine) => {
                if !matches!(self.mode, NodeTxDataMode::Running) {
                    return self.disable(
                        NodeTxDataFault::InternalInvariant,
                        DisabledOwner::Candidate(ParkCandidate::Quarantined(quarantine)),
                    );
                }
                let record = quarantine.record();
                self.parked[index] = ParkedOwner::Quarantined(quarantine);
                self.state = NodeTxDataState::Idle;
                NodeTxDataStep::QuarantinedParked(record)
            }
        }
    }

    fn step_next(&mut self, job: RoutedTxJob<'static>) -> NodeTxDataStep {
        let metadata = NodeTxQueuedHop::from_job(&job);
        match self.handoff.jobs.try_send(job) {
            Ok(()) => {
                self.state = NodeTxDataState::Idle;
                NodeTxDataStep::NextQueued(metadata)
            }
            Err(full) => {
                self.state = NodeTxDataState::Next(full.into_inner());
                NodeTxDataStep::NextBackpressured(metadata)
            }
        }
    }

    fn disable(&mut self, fault: NodeTxDataFault, retained: DisabledOwner) -> NodeTxDataStep {
        self.state = NodeTxDataState::Disabled { fault, retained };
        NodeTxDataStep::Disabled(fault)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        future::Future,
        mem::{align_of, size_of},
        pin::pin,
        ptr,
        task::{Context, Poll, Waker},
    };

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
    use rand_core::{CryptoRng, RngCore};
    use reticulum_node_core::{
        DestinationHash, InterfaceSet, MonotonicSeconds, NodeConfig, NodeIdentity, NodeInstanceId,
        PermitResolution, PrepareDataRequest, TxAuthorizationCandidate, TxAuthorizationPolicy,
        TxCompletionCode, TxPolicyDecision, TxRecoveryPriorPhase, TxRecoveryReason,
    };
    use reticulum_tx_handoff::{ChannelFull, DispatcherHandoff, TxHandoff};
    use static_cell::{ConstStaticCell, StaticCell};
    use std::{
        boxed::Box,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Wake,
    };

    use super::*;
    use crate::TxPermitServer;

    type TestNode<const BUFFERS: usize> = NodeCore<4, 2, 8, 2, BUFFERS>;
    type TestMachine<const BUFFERS: usize> = NodeTxDataMachine<NoopRawMutex, BUFFERS>;
    type TestDispatcher<const BUFFERS: usize> = DispatcherHandoff<NoopRawMutex, BUFFERS>;

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

    struct Allow;

    impl TxAuthorizationPolicy for Allow {
        fn authorize(&mut self, _candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            TxPolicyDecision::Authorize
        }
    }

    struct CountingWake(AtomicUsize);

    impl CountingWake {
        fn new() -> Self {
            Self(AtomicUsize::new(0))
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl Wake for CountingWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid")
    }

    fn node<const BUFFERS: usize>(tag: u8, aspect: &str) -> TestNode<BUFFERS> {
        TestNode::new(
            identity(tag),
            "reticulum",
            &[aspect],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .expect("test node must construct")
    }

    fn register_peer<const BUFFERS: usize>(sender: &mut TestNode<BUFFERS>, tag: u8, aspect: &str) {
        sender
            .register_peer(
                &identity(tag),
                "reticulum",
                &[aspect],
                MonotonicSeconds::new(0),
            )
            .expect("test peer must register");
    }

    fn interfaces(ids: &[u8]) -> InterfaceSet {
        ids.iter().fold(InterfaceSet::empty(), |set, id| {
            set.with(PacketInterfaceId::new(*id))
                .expect("test interface must fit profile")
        })
    }

    fn new_machine<const BUFFERS: usize>(
        owner: &TestNode<BUFFERS>,
    ) -> (TestMachine<BUFFERS>, TestDispatcher<BUFFERS>) {
        let storage = Box::leak(Box::new(TxHandoff::<NoopRawMutex, BUFFERS>::new()));
        let (node_ports, dispatcher) = storage.split();
        let (data, _permit) = TxPermitServer::from_node_handoff(node_ports);
        (NodeTxDataMachine::new(data, owner), dispatcher)
    }

    fn must_fit<T>(result: Result<(), ChannelFull<T>>, message: &str) {
        if result.is_err() {
            panic!("{message}");
        }
    }

    fn park_seed<const BUFFERS: usize>(
        machine: &mut TestMachine<BUFFERS>,
        dispatcher: &mut TestDispatcher<BUFFERS>,
        owner: &mut TestNode<BUFFERS>,
        buffer: &'static mut TxPacketBuffer,
        expected_remaining: usize,
    ) -> (PacketSlotId, *const TxPacketBuffer) {
        let pointer = ptr::from_ref(&*buffer);
        let slot = owner
            .register_packet_buffer(buffer)
            .expect("seed registration must fit");
        must_fit(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Available(buffer)),
            "seed return must fit",
        );
        assert_eq!(
            machine.step(owner, MonotonicMillis::new(1)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(machine.phase(), NodeTxDataPhase::Returned);
        assert_eq!(
            machine.step(owner, MonotonicMillis::new(2)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(machine.phase(), NodeTxDataPhase::Parking);
        assert_eq!(
            machine.step(owner, MonotonicMillis::new(3)),
            NodeTxDataStep::SeedParked {
                slot,
                remaining: expected_remaining,
            }
        );
        (slot, pointer)
    }

    fn parked_buffer_pointer<const BUFFERS: usize>(
        machine: &TestMachine<BUFFERS>,
        slot: PacketSlotId,
    ) -> Option<*const TxPacketBuffer> {
        match machine.parked.get(usize::from(slot.get()))? {
            ParkedOwner::Available(buffer) | ParkedOwner::Recovered { buffer, .. } => {
                Some(ptr::from_ref(&**buffer))
            }
            ParkedOwner::Empty | ParkedOwner::Quarantined(_) => None,
        }
    }

    fn take_available<const BUFFERS: usize>(
        machine: &mut TestMachine<BUFFERS>,
        slot: PacketSlotId,
    ) -> &'static mut TxPacketBuffer {
        match mem::replace(
            &mut machine.parked[usize::from(slot.get())],
            ParkedOwner::Empty,
        ) {
            ParkedOwner::Available(buffer) => buffer,
            ParkedOwner::Empty | ParkedOwner::Recovered { .. } | ParkedOwner::Quarantined(_) => {
                panic!("test slot was not available")
            }
        }
    }

    fn prepare<const BUFFERS: usize>(
        sender: &mut TestNode<BUFFERS>,
        buffer: &'static mut TxPacketBuffer,
        destination: DestinationHash,
        enabled_interfaces: InterfaceSet,
        deadline: u64,
        rng: &mut CounterRng,
    ) -> RoutedTxJob<'static> {
        match sender.prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination,
                plaintext: b"node data machine",
                rns_now: MonotonicSeconds::new(1),
                owner_now: MonotonicMillis::new(10),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline)),
                enabled_interfaces,
            },
            rng,
        ) {
            Ok(job) => job,
            Err(failure) => panic!("test preparation failed: {:?}", failure.reason()),
        }
    }

    fn authorized_completion<const BUFFERS: usize>(
        owner: &mut TestNode<BUFFERS>,
        job: RoutedTxJob<'static>,
        now: u64,
        code: u16,
    ) -> TxCompletion<'static> {
        let (pending, request) = job.begin_permit();
        let reply = match owner.authorize_tx(request, MonotonicMillis::new(now), &mut Allow) {
            Ok(reply) => reply,
            Err(failure) => panic!("permit request failed: {:?}", failure.reason()),
        };
        match pending.resolve(reply, MonotonicMillis::new(now)) {
            Ok(PermitResolution::Authorized(authorized)) => {
                authorized.complete(TxCompletionCode::new(code))
            }
            Ok(PermitResolution::Expired(_)) => panic!("test authorization expired"),
            Ok(PermitResolution::Unpermitted(_)) => panic!("allowing policy denied"),
            Err(_) => panic!("permit reply mismatched"),
        }
    }

    fn drive_completion_to_parking<const BUFFERS: usize>(
        machine: &mut TestMachine<BUFFERS>,
        dispatcher: &mut TestDispatcher<BUFFERS>,
        owner: &mut TestNode<BUFFERS>,
        completion: TxCompletion<'static>,
        now: u64,
    ) {
        must_fit(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Completion(completion)),
            "completion return must fit",
        );
        assert_eq!(
            machine.step(owner, MonotonicMillis::new(now)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(
            machine.step(owner, MonotonicMillis::new(now)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(machine.phase(), NodeTxDataPhase::Parking);
    }

    #[test]
    fn boot_seeds_are_validated_and_parked_by_stable_slot() {
        let mut owner = node::<2>(1, "seed-owner");
        let first = Box::leak(Box::new(TxPacketBuffer::new()));
        let second = Box::leak(Box::new(TxPacketBuffer::new()));
        let (mut machine, mut dispatcher) = new_machine::<2>(&owner);

        assert_eq!(machine.seeds_remaining(), 2);
        let (first_slot, first_pointer) =
            park_seed(&mut machine, &mut dispatcher, &mut owner, first, 1);
        let (second_slot, second_pointer) =
            park_seed(&mut machine, &mut dispatcher, &mut owner, second, 0);

        assert!(machine.is_seeded());
        assert_eq!(machine.phase(), NodeTxDataPhase::Idle);
        assert_eq!(machine.parked_counts().available(), 2);
        assert_eq!(machine.parked_counts().occupied(), 2);
        assert_eq!(
            parked_buffer_pointer(&machine, first_slot),
            Some(first_pointer)
        );
        assert_eq!(
            parked_buffer_pointer(&machine, second_slot),
            Some(second_pointer)
        );
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(4)),
            NodeTxDataStep::NeedReturn
        );
    }

    #[test]
    fn foreign_seed_disables_and_retains_the_exact_buffer() {
        let mut owner = node::<1>(2, "machine-owner");
        let mut foreign_owner = node::<1>(3, "foreign-owner");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let pointer = ptr::from_ref(&*buffer);
        foreign_owner.register_packet_buffer(buffer).unwrap();
        let (mut machine, mut dispatcher) = new_machine::<1>(&owner);
        must_fit(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Available(buffer)),
            "foreign seed must fit",
        );

        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(1)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(2)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(3)),
            NodeTxDataStep::Disabled(NodeTxDataFault::AvailableBuffer(
                AvailableBufferError::ForeignOwner
            ))
        );
        assert_eq!(
            machine.fault_residue_kind(),
            Some(NodeTxDataFaultResidueKind::AvailableBuffer)
        );
        let NodeTxDataState::Disabled {
            retained: DisabledOwner::Candidate(ParkCandidate::Available { buffer, .. }),
            ..
        } = &machine.state
        else {
            panic!("foreign seed was not retained")
        };
        assert_eq!(ptr::from_ref(&**buffer), pointer);
    }

    #[test]
    fn bound_owner_rejects_alternating_valid_nodes_before_consuming_return() {
        let mut owner = node::<2>(18, "bound-owner");
        let first = Box::leak(Box::new(TxPacketBuffer::new()));
        let (mut machine, mut dispatcher) = new_machine::<2>(&owner);
        park_seed(&mut machine, &mut dispatcher, &mut owner, first, 1);

        let mut other = node::<2>(19, "alternating-owner");
        let foreign = Box::leak(Box::new(TxPacketBuffer::new()));
        let pointer = ptr::from_ref(&*foreign);
        other.register_packet_buffer(foreign).unwrap();
        must_fit(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Available(foreign)),
            "alternating-owner seed must fit",
        );

        assert_eq!(
            machine.step(&mut other, MonotonicMillis::new(4)),
            NodeTxDataStep::OwnerMismatch
        );
        assert_eq!(machine.phase(), NodeTxDataPhase::Seeding);
        assert_eq!(machine.seeds_remaining(), 1);
        assert_eq!(dispatcher.returns.len(), 1);

        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(5)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(6)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(7)),
            NodeTxDataStep::Disabled(NodeTxDataFault::AvailableBuffer(
                AvailableBufferError::ForeignOwner
            ))
        );
        let NodeTxDataState::Disabled {
            retained: DisabledOwner::Candidate(ParkCandidate::Available { buffer, .. }),
            ..
        } = &machine.state
        else {
            panic!("alternating owner was not retained")
        };
        assert_eq!(ptr::from_ref(&**buffer), pointer);
        assert_eq!(machine.parked_counts().available(), 1);
    }

    #[test]
    fn final_completion_parks_the_same_available_buffer() {
        let mut owner = node::<1>(4, "completion-owner");
        let receiver = node::<0>(5, "completion-receiver");
        register_peer(&mut owner, 5, "completion-receiver");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let (mut machine, mut dispatcher) = new_machine::<1>(&owner);
        let (slot, pointer) = park_seed(&mut machine, &mut dispatcher, &mut owner, buffer, 0);
        let buffer = take_available(&mut machine, slot);
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            interfaces(&[1]),
            1_000,
            &mut CounterRng::default(),
        );
        let completion = job
            .return_unpermitted()
            .complete(TxCompletionCode::new(0x201));

        drive_completion_to_parking(&mut machine, &mut dispatcher, &mut owner, completion, 20);
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(20)),
            NodeTxDataStep::AvailableParked(slot)
        );
        assert_eq!(parked_buffer_pointer(&machine, slot), Some(pointer));
        assert_eq!(machine.parked_counts().available(), 1);
    }

    #[test]
    fn serialized_next_survives_job_pressure_and_readiness_cancellation() {
        let mut owner = node::<1>(6, "fanout-owner");
        let receiver = node::<0>(7, "fanout-receiver");
        register_peer(&mut owner, 7, "fanout-receiver");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let (mut machine, mut dispatcher) = new_machine::<1>(&owner);
        let (slot, pointer) = park_seed(&mut machine, &mut dispatcher, &mut owner, buffer, 0);
        let buffer = take_available(&mut machine, slot);
        let first = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            interfaces(&[1, 2]),
            1_000,
            &mut CounterRng::default(),
        );
        let attempt = first.attempt();
        let deadline = first.deadline();
        let completion = authorized_completion(&mut owner, first, 20, 0x211);

        let mut blocker_owner = node::<1>(8, "blocker-owner");
        let blocker_receiver = node::<0>(9, "blocker-receiver");
        register_peer(&mut blocker_owner, 9, "blocker-receiver");
        let blocker_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        blocker_owner
            .register_packet_buffer(blocker_buffer)
            .unwrap();
        let blocker = prepare(
            &mut blocker_owner,
            blocker_buffer,
            blocker_receiver.destination_hash(),
            interfaces(&[3]),
            1_000,
            &mut CounterRng::default(),
        );
        must_fit(
            machine.handoff.jobs.try_send(blocker),
            "fault-injection blocker must fill job queue",
        );

        must_fit(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Completion(completion)),
            "first-hop completion must fit",
        );
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(21)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(21)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(machine.phase(), NodeTxDataPhase::NextPending);
        let metadata = match machine.step(&mut owner, MonotonicMillis::new(21)) {
            NodeTxDataStep::NextBackpressured(metadata) => metadata,
            other => panic!("Next was not retained under pressure: {other:?}"),
        };
        assert_eq!(metadata.slot_id(), slot);
        assert_eq!(metadata.attempt(), attempt);
        assert_eq!(metadata.interface(), PacketInterfaceId::new(2));
        assert_eq!(metadata.deadline(), deadline);

        let wake = Arc::new(CountingWake::new());
        let waker = Waker::from(Arc::clone(&wake));
        let mut context = Context::from_waker(&waker);
        let blocker = {
            let mut wait = pin!(machine.wait_for_progress());
            assert_eq!(wait.as_mut().poll(&mut context), Poll::Pending);
            let blocker = dispatcher
                .jobs
                .try_receive()
                .expect("blocker must be queued");
            assert_eq!(wake.count(), 1);
            blocker
        };
        assert_eq!(machine.phase(), NodeTxDataPhase::NextPending);
        assert_eq!(
            block_on(machine.wait_for_progress()),
            NodeTxDataWait::JobCapacityReady
        );
        assert_eq!(machine.phase(), NodeTxDataPhase::NextPending);

        let blocker_buffer = match blocker_owner.rollback_queued(blocker, MonotonicMillis::new(22))
        {
            Ok(TxCompletionDisposition::Available(buffer)) => buffer,
            Ok(_) => panic!("fresh blocker did not return available"),
            Err(failure) => panic!("fresh blocker rollback failed: {:?}", failure.reason()),
        };
        assert_eq!(blocker_buffer.slot_id().map(PacketSlotId::get), Some(0));

        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(23)),
            NodeTxDataStep::NextQueued(metadata)
        );
        let next = dispatcher
            .jobs
            .try_receive()
            .expect("Next must be queued once");
        assert_eq!(next.slot_id(), slot);
        assert_eq!(next.attempt(), attempt);
        assert_eq!(next.interface(), PacketInterfaceId::new(2));
        assert_eq!(next.deadline(), deadline);
        assert!(dispatcher.jobs.try_receive().is_none());

        let completion = next
            .return_unpermitted()
            .complete(TxCompletionCode::new(0x212));
        drive_completion_to_parking(&mut machine, &mut dispatcher, &mut owner, completion, 24);
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(24)),
            NodeTxDataStep::AvailableParked(slot)
        );
        assert_eq!(parked_buffer_pointer(&machine, slot), Some(pointer));
    }

    #[test]
    fn wrong_owner_completion_disables_with_the_exact_failure() {
        let mut machine_owner = node::<1>(10, "wrong-machine");
        let seed = Box::leak(Box::new(TxPacketBuffer::new()));
        let (mut machine, mut dispatcher) = new_machine::<1>(&machine_owner);
        park_seed(&mut machine, &mut dispatcher, &mut machine_owner, seed, 0);

        let mut actual_owner = node::<1>(11, "actual-owner");
        let receiver = node::<0>(12, "actual-receiver");
        register_peer(&mut actual_owner, 12, "actual-receiver");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let pointer = ptr::from_ref(&*buffer);
        actual_owner.register_packet_buffer(buffer).unwrap();
        let job = prepare(
            &mut actual_owner,
            buffer,
            receiver.destination_hash(),
            interfaces(&[1]),
            1_000,
            &mut CounterRng::default(),
        );
        let completion = job
            .return_unpermitted()
            .complete(TxCompletionCode::new(0x221));
        must_fit(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Completion(completion)),
            "wrong-owner completion must fit",
        );
        assert_eq!(
            machine.step(&mut machine_owner, MonotonicMillis::new(20)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(
            machine.step(&mut machine_owner, MonotonicMillis::new(20)),
            NodeTxDataStep::Disabled(NodeTxDataFault::Completion(
                TxCompletionErrorKind::WrongOwner
            ))
        );
        assert_eq!(
            machine.fault_residue_kind(),
            Some(NodeTxDataFaultResidueKind::CompletionFailure)
        );

        let state = mem::replace(&mut machine.state, NodeTxDataState::Poisoned);
        let NodeTxDataState::Disabled {
            retained: DisabledOwner::CompletionFailure(failure),
            ..
        } = state
        else {
            panic!("completion failure was not retained")
        };
        let disposition = actual_owner
            .complete_tx(failure.into_completion(), MonotonicMillis::new(20))
            .unwrap_or_else(|failure| {
                panic!(
                    "exact completion was not recoverable: {:?}",
                    failure.reason()
                )
            });
        let returned = match disposition {
            TxCompletionDisposition::Available(buffer) => buffer,
            _ => panic!("unpermitted final completion did not release buffer"),
        };
        assert_eq!(ptr::from_ref(&*returned), pointer);
    }

    #[test]
    fn exact_deadline_recovery_waits_for_generation_scoped_acknowledgement() {
        let mut owner = node::<1>(13, "recovery-owner");
        let receiver = node::<0>(14, "recovery-receiver");
        register_peer(&mut owner, 14, "recovery-receiver");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let (mut machine, mut dispatcher) = new_machine::<1>(&owner);
        let (slot, pointer) = park_seed(&mut machine, &mut dispatcher, &mut owner, buffer, 0);
        let buffer = take_available(&mut machine, slot);
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            interfaces(&[1, 2]),
            100,
            &mut CounterRng::default(),
        );
        let completion = authorized_completion(&mut owner, job, 20, 0x231);
        drive_completion_to_parking(&mut machine, &mut dispatcher, &mut owner, completion, 100);
        let record = match machine.step(&mut owner, MonotonicMillis::new(100)) {
            NodeTxDataStep::RecoveredParked(record) => record,
            other => panic!("deadline owner was not parked recovered: {other:?}"),
        };
        assert_eq!(record.slot_id(), slot);
        assert_eq!(record.reason(), TxRecoveryReason::DeadlineExpired);
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Authorized);
        assert!(record.may_have_transmitted());
        assert_eq!(record.observed_at(), MonotonicMillis::new(100));
        assert_eq!(machine.parked_kind(slot), Some(NodeTxParkedKind::Recovered));
        assert_eq!(machine.recovered_record(slot), Some(record));
        assert_eq!(machine.recovered_records().next(), Some(record));
        assert_eq!(parked_buffer_pointer(&machine, slot), Some(pointer));

        machine
            .acknowledge_recovered(record)
            .expect("exact record must acknowledge");
        assert_eq!(machine.parked_kind(slot), Some(NodeTxParkedKind::Available));
        assert_eq!(machine.recovered_record(slot), None);
        assert_eq!(parked_buffer_pointer(&machine, slot), Some(pointer));
        assert_eq!(
            machine.acknowledge_recovered(record),
            Err(NodeTxRecoveryAckError::NotRecovered)
        );
    }

    #[test]
    fn recovery_fault_parks_quarantine_without_exposing_available_owner() {
        let mut owner = node::<1>(15, "quarantine-owner");
        let receiver = node::<0>(16, "quarantine-receiver");
        register_peer(&mut owner, 16, "quarantine-receiver");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let (mut machine, mut dispatcher) = new_machine::<1>(&owner);
        let (slot, _pointer) = park_seed(&mut machine, &mut dispatcher, &mut owner, buffer, 0);
        let buffer = take_available(&mut machine, slot);
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            interfaces(&[1]),
            1_000,
            &mut CounterRng::default(),
        );
        let code = TxCompletionCode::new(0x241);
        let completion = job.recovery_fault(code);
        drive_completion_to_parking(&mut machine, &mut dispatcher, &mut owner, completion, 20);
        let record = match machine.step(&mut owner, MonotonicMillis::new(20)) {
            NodeTxDataStep::QuarantinedParked(record) => record,
            other => panic!("recovery fault was not quarantined: {other:?}"),
        };
        assert_eq!(record.slot_id(), slot);
        assert_eq!(record.reason(), TxRecoveryReason::CompletionFault(code));
        assert_eq!(
            machine.parked_kind(slot),
            Some(NodeTxParkedKind::Quarantined)
        );
        assert_eq!(machine.quarantine_record(slot), Some(record));
        assert_eq!(machine.quarantine_records().next(), Some(record));
        assert_eq!(machine.parked_counts().available(), 0);
        assert_eq!(machine.parked_counts().quarantined(), 1);
        assert_eq!(parked_buffer_pointer(&machine, slot), None);
    }

    #[test]
    fn cancelled_and_ready_return_waits_store_owners_only_after_ready_poll() {
        let mut owner = node::<1>(17, "wait-owner");
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let pointer = ptr::from_ref(&*buffer);
        let slot = owner.register_packet_buffer(buffer).unwrap();
        let (mut machine, mut dispatcher) = new_machine::<1>(&owner);
        let wake = Arc::new(CountingWake::new());
        let waker = Waker::from(Arc::clone(&wake));
        let mut context = Context::from_waker(&waker);

        {
            let mut wait = pin!(machine.wait_for_progress());
            assert_eq!(wait.as_mut().poll(&mut context), Poll::Pending);
        }
        assert_eq!(machine.phase(), NodeTxDataPhase::Seeding);

        {
            let mut wait = pin!(machine.wait_for_progress());
            assert_eq!(wait.as_mut().poll(&mut context), Poll::Pending);
            must_fit(
                dispatcher
                    .returns
                    .try_send(TxOwnerReturn::Available(buffer)),
                "wait seed must fit",
            );
            assert_eq!(wake.count(), 1);
        }
        assert_eq!(machine.phase(), NodeTxDataPhase::Seeding);
        assert_eq!(dispatcher.returns.len(), 1);

        assert_eq!(
            block_on(machine.wait_for_progress()),
            NodeTxDataWait::ReturnStored
        );
        assert_eq!(machine.phase(), NodeTxDataPhase::Returned);
        assert_eq!(dispatcher.returns.len(), 0);
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(1)),
            NodeTxDataStep::Advanced
        );
        assert_eq!(
            machine.step(&mut owner, MonotonicMillis::new(1)),
            NodeTxDataStep::SeedParked { slot, remaining: 0 }
        );
        assert_eq!(parked_buffer_pointer(&machine, slot), Some(pointer));
    }

    #[test]
    fn production_machine_uses_compact_static_per_slot_owner_metadata() {
        static HANDOFF: ConstStaticCell<TxHandoff<CriticalSectionRawMutex, 1>> =
            ConstStaticCell::new(TxHandoff::new());
        static MACHINE: StaticCell<NodeTxDataMachine<CriticalSectionRawMutex, 1>> =
            StaticCell::new();

        let (node_ports, _dispatcher) = HANDOFF.take().split();
        let (data, _permit) = TxPermitServer::from_node_handoff(node_ports);
        let owner = node::<1>(20, "production-layout-owner");
        let machine = MACHINE.init(NodeTxDataMachine::new(data, &owner));
        assert_eq!(machine.phase(), NodeTxDataPhase::Seeding);

        assert!(size_of::<ParkedOwner>() < size_of::<TxPacketBuffer>());
        let one = size_of::<NodeTxDataMachine<CriticalSectionRawMutex, 1>>();
        let two = size_of::<NodeTxDataMachine<CriticalSectionRawMutex, 2>>();
        assert!(two >= one);
        assert!(two - one <= size_of::<ParkedOwner>() + align_of::<ParkedOwner>());
    }
}
