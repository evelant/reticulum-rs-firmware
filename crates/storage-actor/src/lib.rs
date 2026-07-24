//! Sole-owner adapter joining the durable model, physical NOR journal, and
//! volatile submission projector.
//!
//! Construction is possible only through [`StorageActor::mount`] or
//! [`StorageActor::mount_into`], so no live request can be served before
//! complete physical scan and semantic replay.
//! The actor retains one exact mutation across ambiguous backend failures and
//! rejects unrelated work until retry establishes a durable conclusion.

#![no_std]
#![deny(unsafe_code)]
#![deny(missing_docs)]

use core::mem::MaybeUninit;

use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use reticulum_node_core::{TerminalAttempt, TxRecoveryObservation};
use reticulum_storage_journal::{
    AppendOutcome, JournalError, JournalState, PARTITION_SIZE, PHYSICAL_FORMAT_VERSION,
    SlotCorruption, append_with_replay_scratch, compact_with_replay_scratch, mount, mount_into,
};
use reticulum_storage_model::{
    AcceptOutcome, AcceptanceCandidate, ApplyError, BootRecoveryDecision, JournalEntry,
    PlanOutcome, PlannedMutation, SubmissionId, SubmissionIndex, SubmissionIntent,
    SubmissionReplay,
};
use reticulum_submission_projector::{
    AcknowledgementAction, AcknowledgementReply, PersistHandle, PersistRequest,
    PersistenceProgress, PersistenceReply, PreparedFrameObservation, ProjectionProgress,
    ProjectorError, SubmissionPreparationObservation, SubmissionProjector,
};
#[cfg(test)]
mod tests;

/// Stable physical identity of one storage device.
///
/// The identifier is product-supplied and must distinguish devices whose
/// address spaces can otherwise expose the same journal range.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StorageDeviceId([u8; 16]);

impl StorageDeviceId {
    /// Construct a device identifier from its exact bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Exact identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Physical provenance and format contract for one journal backend view.
///
/// Offsets are absolute in the containing physical device even though the
/// wrapped NOR backend exposes journal-relative offsets beginning at zero.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JournalBinding {
    device: StorageDeviceId,
    absolute_offset: usize,
    length: usize,
    layout_version: u16,
}

impl JournalBinding {
    /// Construct an exact journal binding.
    pub const fn new(
        device: StorageDeviceId,
        absolute_offset: usize,
        length: usize,
        layout_version: u16,
    ) -> Self {
        Self {
            device,
            absolute_offset,
            length,
            layout_version,
        }
    }

    /// Physical device containing the journal.
    pub const fn device(&self) -> StorageDeviceId {
        self.device
    }

    /// Absolute byte offset in the physical device.
    pub const fn absolute_offset(&self) -> usize {
        self.absolute_offset
    }

    /// Bound journal range length in bytes.
    pub const fn length(&self) -> usize {
        self.length
    }

    /// Physical journal layout version expected in the range.
    pub const fn layout_version(&self) -> u16 {
        self.layout_version
    }
}

/// Binding validation failure detected before any physical journal I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalBindingError {
    /// A later backend names a different physical device.
    DeviceMismatch {
        /// Device retained by the mounted actor.
        expected: StorageDeviceId,
        /// Device presented by the borrowed backend.
        actual: StorageDeviceId,
    },
    /// A later backend names a different absolute journal range.
    RangeMismatch {
        /// Absolute offset retained by the mounted actor.
        expected_absolute_offset: usize,
        /// Length retained by the mounted actor.
        expected_length: usize,
        /// Absolute offset presented by the borrowed backend.
        actual_absolute_offset: usize,
        /// Length presented by the borrowed backend.
        actual_length: usize,
    },
    /// Absolute start plus length cannot be represented.
    RangeOverflow {
        /// Absolute start of the bound range.
        absolute_offset: usize,
        /// Length of the bound range.
        length: usize,
    },
    /// The binding names a different physical format version.
    LayoutVersionMismatch {
        /// Required physical format version.
        expected: u16,
        /// Version presented by the binding.
        actual: u16,
    },
    /// The binding length is not the exact compiled journal partition size.
    LengthMismatch {
        /// Required journal range length.
        expected: usize,
        /// Length presented by the binding.
        actual: usize,
    },
    /// The journal-relative backend capacity differs from the bound range.
    CapacityMismatch {
        /// Capacity required by the binding.
        expected: usize,
        /// Capacity exposed by the backend.
        actual: usize,
    },
    /// The absolute range is incompatible with the backend's I/O geometry.
    AlignmentMismatch {
        /// Absolute start of the bound range.
        absolute_offset: usize,
        /// Length of the bound range.
        length: usize,
        /// Backend read granularity.
        read_size: usize,
        /// Backend program granularity.
        write_size: usize,
        /// Backend erase granularity.
        erase_size: usize,
    },
}

/// A journal-relative NOR backend carrying explicit physical provenance.
///
/// This adapter deliberately delegates offsets unchanged. Range restriction is
/// the responsibility of the wrapped backend; the binding proves which
/// physical range that backend view represents.
pub struct BoundJournal<F> {
    backend: F,
    binding: JournalBinding,
}

impl<F> BoundJournal<F> {
    /// Attach physical provenance to one journal-relative backend.
    pub const fn new(backend: F, binding: JournalBinding) -> Self {
        Self { backend, binding }
    }

    /// Binding carried by this backend view.
    pub const fn binding(&self) -> JournalBinding {
        self.binding
    }

    /// Immutable access to the wrapped backend.
    pub const fn backend(&self) -> &F {
        &self.backend
    }

    /// Mutable access to the wrapped backend.
    pub fn backend_mut(&mut self) -> &mut F {
        &mut self.backend
    }

    /// Consume the adapter and recover the wrapped backend.
    pub fn into_backend(self) -> F {
        self.backend
    }
}

impl<F: ErrorType> ErrorType for BoundJournal<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for BoundJournal<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.backend.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.backend.capacity()
    }
}

impl<F: NorFlash> NorFlash for BoundJournal<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.backend.erase(from, to)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.backend.write(offset, bytes)
    }
}

impl<F: MultiwriteNorFlash> MultiwriteNorFlash for BoundJournal<F> {}

/// NOR access that can prove the exact physical journal range it represents.
pub trait BoundJournalAccess: MultiwriteNorFlash {
    /// Physical device, range, and layout represented by this backend view.
    fn journal_binding(&self) -> JournalBinding;
}

impl<F: MultiwriteNorFlash> BoundJournalAccess for BoundJournal<F> {
    fn journal_binding(&self) -> JournalBinding {
        self.binding
    }
}

/// Kind of exact mutation currently retained after an ambiguous drive result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingKind {
    /// A device-facing acceptance candidate and its exact planned record.
    Acceptance,
    /// An exact conservative boot-finalization record and its boot identity.
    BootRecovery,
    /// An exact request retained by the actor-owned submission projector.
    Projector,
}

/// Bounded, backend-independent fault latched by the actor.
///
/// Once present, all mutation entry points fail closed with the same value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageFault {
    /// The backend geometry is incompatible with the journal format.
    IncompatibleFlash,
    /// The complete partition is erased and has not been explicitly formatted.
    UnformattedErased,
    /// Programmed bytes exist without a valid journal format.
    UnformattedNonErased,
    /// A committed manifest failed validation.
    ManifestCorrupt,
    /// Both manifests claim the same generation.
    ManifestConflict,
    /// A committed physical format version is unsupported.
    UnsupportedPhysicalVersion(u16),
    /// A committed semantic schema version is unsupported.
    UnsupportedSemanticVersion(u16),
    /// Committed geometry disagrees with the compiled journal geometry.
    GeometryMismatch,
    /// One committed record slot is corrupt.
    SlotCorrupt {
        /// Physical slot within the selected bank.
        slot: usize,
        /// Exact bounded corruption class.
        reason: SlotCorruption,
    },
    /// Manifest baseline count or digest is absent from the selected bank.
    BaselineMismatch,
    /// Durable replay found the same logical key more than once.
    DuplicateLogicalKey,
    /// Durable records violate the semantic state machine.
    SemanticReplay(ApplyError),
    /// Durable content contradicts the exact pending logical record.
    LogicalConflict,
    /// Compaction did not make the exact pending append serviceable.
    CompactionDidNotClear,
    /// The selected generation cannot advance without wrapping.
    GenerationExhausted,
    /// Compaction metadata or record copying violated an invariant.
    CompactionInvariant,
    /// Exact program readback differed from the supplied bytes.
    ReadbackMismatch,
    /// Committed acceptances violate the lifetime reservation invariant.
    ReservationInvariant,
    /// An actor-owned recovery decision or durably completed plan was rejected.
    ModelApplyRejected(ApplyError),
    /// The request supplied to the actor is not exactly retained by its projector.
    ProjectorRequestMismatch,
    /// The projector rejected the actor's correlated persistence report.
    ProjectorRejected(ProjectorError),
}

/// Failure while establishing the only live actor state.
#[derive(Debug, Eq, PartialEq)]
pub enum MountError<E> {
    /// Backend provenance, range, layout, or capacity is incompatible.
    Binding(JournalBindingError),
    /// Underlying NOR operation failed before replay completion.
    Backend(E),
    /// Journal or model validation failed with a bounded diagnostic.
    Fault(StorageFault),
}

/// Recoverable or latched failure while driving one retained mutation.
#[derive(Debug, Eq, PartialEq)]
pub enum DriveError<E> {
    /// Borrowed backend does not exactly match the actor's mounted binding.
    ///
    /// This error is non-latching and no physical I/O is attempted.
    Binding(JournalBindingError),
    /// Underlying NOR operation failed with ambiguous completion.
    ///
    /// The exact mutation remains retained and is safe to retry unchanged.
    Backend(E),
    /// A different mutation is already retained and must be resolved first.
    Busy {
        /// Kind of mutation that currently owns the serialization cell.
        pending: PendingKind,
    },
    /// The actor has latched a fail-closed bounded fault.
    Faulted(StorageFault),
}

/// Failure to mutate the actor-owned projector before physical persistence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectorOperationError {
    /// A physical mutation already owns the actor's serialization cell.
    Busy {
        /// Kind of mutation that must reach a durable conclusion first.
        pending: PendingKind,
    },
    /// The actor has latched a fail-closed bounded fault.
    Faulted(StorageFault),
    /// The projector rejected the operation without faulting the actor.
    Rejected(ProjectorError),
}

/// Definitive non-fault result of one durable acceptance request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptanceProgress {
    /// A new submission acceptance is durably represented and live.
    Accepted(SubmissionId),
    /// Exact principal, idempotency key, and content were already accepted.
    Replay(SubmissionId),
    /// The principal-scoped key is already bound to different content.
    IdempotencyConflict {
        /// Existing durable submission protected from mutation.
        existing: SubmissionId,
    },
    /// The fixed live replay index has no free submission slot.
    IndexExhausted,
    /// No later submission identifier can be represented.
    IdentifierExhausted,
    /// The journal cannot reserve a complete schema lifetime for another acceptance.
    JournalCapacityExhausted,
}

/// Definitive non-fault result of recovering one replayed submission at boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootRecoveryProgress {
    /// Queued intent remains replay-safe and may be submitted again.
    ReplayQueued,
    /// The replayed submission was already durably final.
    AlreadyFinal,
    /// Replay-unsafe interrupted work is now durably final.
    Finalized,
}

/// Definitive progress from autonomously reconciling the actor's retained mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingProgress {
    /// No mutation was retained, so no storage operation was required.
    Idle,
    /// One retained acceptance is durable and applied to the live index.
    AcceptanceCommitted(SubmissionId),
    /// One retained conservative boot-finalization is durable and applied.
    BootRecoveryFinalized(SubmissionId),
    /// One retained projector mutation is durable and applied to the live index.
    ProjectorCommitted,
    /// The journal cannot reserve a complete schema lifetime for the acceptance.
    AcceptanceCapacityExhausted,
}

// The fixed actor cell deliberately owns a complete acceptance or boot plan
// without allocation; the projector variant is a compact handle into its owner.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingMutation {
    Acceptance {
        planned: PlannedMutation,
    },
    BootRecovery {
        id: SubmissionId,
        boot_sequence: u64,
        planned: PlannedMutation,
    },
    Projector {
        handle: PersistHandle,
    },
}

/// Target-layout byte size of the actor's actual optional pending-mutation cell.
///
/// This makes the RAM cost of exact ambiguous-write retention directly
/// measurable in host and firmware builds.
pub const PENDING_MUTATION_BYTES: usize = core::mem::size_of::<Option<PendingMutation>>();

// One actor-owned acceptance/boot plan or one compact projector handle must
// fit. Projector requests remain solely in the actor-owned projector.
const _: () = assert!(PENDING_MUTATION_BYTES <= 544);

impl PendingMutation {
    const fn kind(self) -> PendingKind {
        match self {
            Self::Acceptance { .. } => PendingKind::Acceptance,
            Self::BootRecovery { .. } => PendingKind::BootRecovery,
            Self::Projector { .. } => PendingKind::Projector,
        }
    }
}

/// Mounted semantic owner of one bound journal, live index, and submission projector.
///
/// Physical storage is borrowed only for individual operations. The actor
/// retains the exact binding established at mount and rejects every different
/// backend before I/O.
pub struct StorageActor<const SUBMISSIONS: usize, const PROJECTED: usize> {
    binding: JournalBinding,
    state: JournalState,
    index: SubmissionIndex<SUBMISSIONS>,
    replay_scratch: SubmissionIndex<SUBMISSIONS>,
    projector: SubmissionProjector<PROJECTED>,
    first_submission_id: SubmissionId,
    pending: Option<PendingMutation>,
    fault: Option<StorageFault>,
}

impl<const SUBMISSIONS: usize, const PROJECTED: usize> StorageActor<SUBMISSIONS, PROJECTED> {
    /// Scan the complete physical journal and finish semantic replay before
    /// exposing a live actor.
    ///
    /// This remains an out-of-line placement boundary so a caller embedding
    /// the actor in a larger runtime does not merge both large return slots
    /// into one compiler-emitted stack frame.
    #[inline(never)]
    pub fn mount<A: BoundJournalAccess>(
        access: &mut A,
        first_submission_id: SubmissionId,
    ) -> Result<Self, MountError<A::Error>> {
        validate_binding::<A>(access).map_err(MountError::Binding)?;
        let binding = access.journal_binding();
        let mounted =
            mount::<SUBMISSIONS, A>(access, first_submission_id).map_err(map_mount_error)?;
        let state = mounted.state();
        let index = mounted.into_index();
        let replay_scratch = SubmissionReplay::<SUBMISSIONS>::new(first_submission_id)
            .complete()
            .expect("a fresh replay builder cannot be faulted");
        Ok(Self {
            binding,
            state,
            index,
            replay_scratch,
            projector: SubmissionProjector::new(),
            first_submission_id,
            pending: None,
            fault: None,
        })
    }

    /// Mount directly into caller-provided uninitialized storage.
    ///
    /// This placement form avoids returning the complete actor by value. It is
    /// intended for products that keep a comparatively large durable index in
    /// external memory. `destination` contains a fully initialized actor only
    /// when `Ok` is returned. On error it must still be treated as
    /// uninitialized, although an internal no-drop placement field may already
    /// have been written; the same storage may be supplied to an exact retry.
    ///
    /// The caller must supply genuinely uninitialized storage or the same
    /// storage after this method returned an error. Calling this method with a
    /// destination that contains an exposed live actor remains invalid. The
    /// compile-time no-drop invariants make failed-attempt overwrite safe and
    /// fail closed if any future placement field owns resources.
    #[allow(
        unsafe_code,
        reason = "audited raw-field projection initializes both capacity-sized indexes at their final caller-owned addresses"
    )]
    #[inline(never)]
    pub fn mount_into<'a, A: BoundJournalAccess>(
        destination: &'a mut MaybeUninit<Self>,
        access: &mut A,
        first_submission_id: SubmissionId,
    ) -> Result<&'a mut Self, MountError<A::Error>> {
        const {
            assert!(
                !core::mem::needs_drop::<SubmissionIndex<SUBMISSIONS>>(),
                "actor placement retries require a no-drop live and scratch index"
            );
            assert!(
                !core::mem::needs_drop::<SubmissionProjector<PROJECTED>>(),
                "actor placement retries require a no-drop projector"
            );
            assert!(
                !core::mem::needs_drop::<StorageActor<SUBMISSIONS, PROJECTED>>(),
                "actor placement retries require a no-drop actor"
            );
        }
        validate_binding::<A>(access).map_err(MountError::Binding)?;
        let binding = access.journal_binding();
        let actor = destination.as_mut_ptr();
        // SAFETY: `actor` is aligned, uniquely borrowed, and points to an
        // uninitialized `StorageActor`. `MaybeUninit<T>` has `T`'s layout, so
        // projecting each index field as `MaybeUninit<SubmissionIndex<_>>`
        // creates no reference to an uninitialized index. Journal replay
        // initializes the live index directly; a fresh replay initializes the
        // independent retry scratch directly. No capacity-sized value is
        // returned or moved through this frame.
        let (state, scratch_destination, projector_destination) = unsafe {
            let index_destination = &mut *core::ptr::addr_of_mut!((*actor).index)
                .cast::<MaybeUninit<SubmissionIndex<SUBMISSIONS>>>();
            let state = mount_into(index_destination, access, first_submission_id)
                .map_err(map_mount_error)?;
            let scratch_destination = &mut *core::ptr::addr_of_mut!((*actor).replay_scratch)
                .cast::<MaybeUninit<SubmissionIndex<SUBMISSIONS>>>();
            let projector_destination = &mut *core::ptr::addr_of_mut!((*actor).projector)
                .cast::<MaybeUninit<SubmissionProjector<PROJECTED>>>(
            );
            (state, scratch_destination, projector_destination)
        };
        SubmissionReplay::<SUBMISSIONS>::begin_in(scratch_destination, first_submission_id)
            .complete()
            .expect("a fresh in-place replay builder cannot be faulted");
        SubmissionProjector::initialize_in(projector_destination);

        // SAFETY: both indexes and the projector are fully initialized above.
        // Every remaining write is infallible and targets one distinct field
        // exactly once, after which the complete actor may be exposed.
        unsafe {
            core::ptr::addr_of_mut!((*actor).binding).write(binding);
            core::ptr::addr_of_mut!((*actor).state).write(state);
            core::ptr::addr_of_mut!((*actor).first_submission_id).write(first_submission_id);
            core::ptr::addr_of_mut!((*actor).pending).write(None);
            core::ptr::addr_of_mut!((*actor).fault).write(None);
            Ok(&mut *actor)
        }
    }

    /// Exact physical journal binding established by mount.
    pub const fn binding(&self) -> JournalBinding {
        self.binding
    }

    /// Validate a borrowed backend against the mounted physical binding.
    ///
    /// Coordinators can call this before node-side effects in a compound step.
    /// Failure is non-latching and performs no physical journal I/O.
    pub fn validate_access<A: BoundJournalAccess>(
        &self,
        access: &A,
    ) -> Result<(), JournalBindingError> {
        validate_exact_binding(self.binding, access.journal_binding())
            .and_then(|()| validate_binding::<A>(access))
    }

    /// Last completely established physical journal state.
    ///
    /// This may lag programmed flash while an ambiguous backend result remains
    /// pending; exact retry reconciles it before unrelated work is admitted.
    pub const fn state(&self) -> JournalState {
        self.state
    }

    /// Live semantic index reconstructed at mount and advanced only after durability.
    ///
    /// The index intentionally lags an ambiguous physical append until exact
    /// retry proves `Appended` or `AlreadyEquivalent`.
    pub const fn index(&self) -> &SubmissionIndex<SUBMISSIONS> {
        &self.index
    }

    /// Immutable access to the actor-owned volatile submission projector.
    ///
    /// Mutable access is intentionally unavailable, so safe callers cannot
    /// replace, extract, or report persistence directly to the sole projector.
    ///
    /// ```compile_fail
    /// use core::mem;
    /// use reticulum_storage_actor::StorageActor;
    /// use reticulum_submission_projector::SubmissionProjector;
    ///
    /// fn replace_projector(actor: &mut StorageActor<1, 1>) {
    ///     let _ = mem::replace(actor.projector(), SubmissionProjector::new());
    /// }
    /// ```
    pub const fn projector(&self) -> &SubmissionProjector<PROJECTED> {
        &self.projector
    }

    /// Copy the durable transport-neutral intent only when preparation may begin.
    ///
    /// Readiness requires the `Queued -> Preparing` barrier to be durably
    /// committed, no attempt to have been bound, and no actor or projector
    /// fault or pending mutation. Returning an owned bounded intent lets a
    /// coordinator call the permanent node owner without borrowing storage
    /// across that call.
    pub fn ready_intent(&self, id: SubmissionId) -> Option<SubmissionIntent> {
        if self.fault.is_some()
            || self.pending.is_some()
            || !self.projector.preparation_allowed(&self.index, id)
        {
            return None;
        }
        self.index
            .get(id)
            .map(|submission| submission.accepted().intent())
    }

    /// Plan the durable no-replay barrier for one queued submission.
    ///
    /// The projector remains actor-owned; callers receive only the bounded
    /// progress value and must route any returned persistence request through
    /// [`Self::persist_projector`].
    pub fn begin_preparation(
        &mut self,
        id: SubmissionId,
    ) -> Result<ProjectionProgress, ProjectorOperationError> {
        self.ensure_projector_operation()?;
        let result = self.projector.begin_preparation(&self.index, id);
        self.map_projector_operation(result)
    }

    /// Project one transport-neutral permanent-node preparation observation.
    ///
    /// This is the product integration seam for every interface supervisor.
    pub fn observe_preparation(
        &mut self,
        id: SubmissionId,
        observation: SubmissionPreparationObservation,
    ) -> Result<ProjectionProgress, ProjectorOperationError> {
        self.ensure_projector_operation()?;
        let result = self
            .projector
            .observe_preparation(&self.index, id, observation);
        self.map_projector_operation(result)
    }

    /// Project scalar metadata for one complete authorized Reticulum frame.
    ///
    /// Packet bytes and owners remain outside storage; the projector retains
    /// only its bounded attempt correlation and any exact journal request.
    pub fn observe_frame(
        &mut self,
        observation: PreparedFrameObservation,
    ) -> Result<ProjectionProgress, ProjectorOperationError> {
        self.ensure_projector_operation()?;
        let result = self.projector.observe_frame(&self.index, observation);
        self.map_projector_operation(result)
    }

    /// Project one exact node-core terminal tombstone.
    ///
    /// The corresponding upstream acknowledgement remains invisible until the
    /// final lifecycle record is durably committed through this actor.
    pub fn observe_terminal(
        &mut self,
        terminal: TerminalAttempt,
    ) -> Result<ProjectionProgress, ProjectorOperationError> {
        self.ensure_projector_operation()?;
        let result = self.projector.observe_terminal(&self.index, terminal);
        self.map_projector_operation(result)
    }

    /// Project one exact recovered packet-owner observation.
    ///
    /// Its release acknowledgement is exposed only after the transport audit
    /// is durable.
    pub fn observe_recovered(
        &mut self,
        observation: TxRecoveryObservation,
    ) -> Result<ProjectionProgress, ProjectorOperationError> {
        self.ensure_projector_operation()?;
        let result = self.projector.observe_recovered(&self.index, observation);
        self.map_projector_operation(result)
    }

    /// Project one exact fail-closed packet-owner quarantine observation.
    ///
    /// Quarantine has no release acknowledgement; the projector may stage a
    /// following conservative final record after its audit becomes durable.
    pub fn observe_quarantined(
        &mut self,
        observation: TxRecoveryObservation,
    ) -> Result<ProjectionProgress, ProjectorOperationError> {
        self.ensure_projector_operation()?;
        let result = self.projector.observe_quarantined(&self.index, observation);
        self.map_projector_operation(result)
    }

    /// Iterate exact upstream acknowledgement actions unlocked by durable
    /// projector records, in stable projector-slot order.
    pub fn pending_acknowledgements(&self) -> impl Iterator<Item = AcknowledgementAction> + '_ {
        self.projector.pending_acknowledgements()
    }

    /// Report the result of one exact retained upstream acknowledgement.
    ///
    /// A retryable reply leaves the same action retained. Completion releases
    /// only the exactly correlated action; mismatch or upstream failure is
    /// fail-closed by the actor.
    pub fn report_acknowledgement(
        &mut self,
        action: AcknowledgementAction,
        reply: AcknowledgementReply,
    ) -> Result<(), ProjectorOperationError> {
        self.ensure_projector_operation()?;
        let result = self.projector.report_acknowledgement(action, reply);
        self.map_projector_operation(result)
    }

    /// Current fail-closed actor fault, if any.
    pub const fn fault(&self) -> Option<StorageFault> {
        self.fault
    }

    /// Kind of exact mutation retained across an ambiguous backend result.
    pub const fn pending_kind(&self) -> Option<PendingKind> {
        match self.pending {
            Some(pending) => Some(pending.kind()),
            None => None,
        }
    }

    /// Durably accept one exact authenticated candidate.
    ///
    /// A backend error retains the exact plan. Retrying the same candidate
    /// reconciles by journal readback; different work receives [`DriveError::Busy`].
    pub fn accept<A: BoundJournalAccess>(
        &mut self,
        access: &mut A,
        candidate: AcceptanceCandidate,
    ) -> Result<AcceptanceProgress, DriveError<A::Error>> {
        self.ensure_healthy()?;
        self.ensure_binding(access)?;

        match self.pending {
            Some(PendingMutation::Acceptance { planned }) => {
                match self.index.plan_accept(candidate) {
                    AcceptOutcome::Accepted(replanned) if replanned == planned => {}
                    _ => {
                        return Err(DriveError::Busy {
                            pending: PendingKind::Acceptance,
                        });
                    }
                }
            }
            Some(pending) => {
                return Err(DriveError::Busy {
                    pending: pending.kind(),
                });
            }
            None => match self.index.plan_accept(candidate) {
                AcceptOutcome::Accepted(planned) => {
                    self.pending = Some(PendingMutation::Acceptance { planned });
                }
                AcceptOutcome::Replay(id) => return Ok(AcceptanceProgress::Replay(id)),
                AcceptOutcome::IdempotencyConflict { existing } => {
                    return Ok(AcceptanceProgress::IdempotencyConflict { existing });
                }
                AcceptOutcome::IndexExhausted => return Ok(AcceptanceProgress::IndexExhausted),
                AcceptOutcome::IdentifierExhausted => {
                    return Ok(AcceptanceProgress::IdentifierExhausted);
                }
            },
        };

        match self.drive_pending(access)? {
            PendingProgress::AcceptanceCommitted(id) => Ok(AcceptanceProgress::Accepted(id)),
            PendingProgress::AcceptanceCapacityExhausted => {
                Ok(AcceptanceProgress::JournalCapacityExhausted)
            }
            PendingProgress::Idle
            | PendingProgress::BootRecoveryFinalized(_)
            | PendingProgress::ProjectorCommitted => Err(self.latch(StorageFault::LogicalConflict)),
        }
    }

    /// Resolve one completely replayed submission before services are enabled.
    ///
    /// Queued work and already-final work require no physical mutation and
    /// return immediately. Preparing or awaiting-delivery work is converted to
    /// the model's conservative interrupted-by-reset final state, then becomes
    /// visible in the live index only after its exact planned record is
    /// durably appended or proved already equivalent.
    ///
    /// A backend error retains the complete plan together with `id` and
    /// `boot_sequence`. An exact retry or [`Self::drive_pending`] reconciles
    /// that same record; any different identifier or boot sequence receives
    /// [`DriveError::Busy`] until the retained operation is resolved.
    pub fn finalize_boot_recovery<A: BoundJournalAccess>(
        &mut self,
        access: &mut A,
        id: SubmissionId,
        boot_sequence: u64,
    ) -> Result<BootRecoveryProgress, DriveError<A::Error>> {
        self.ensure_healthy()?;
        self.ensure_binding(access)?;

        match self.pending {
            Some(PendingMutation::BootRecovery {
                id: pending_id,
                boot_sequence: pending_boot_sequence,
                ..
            }) if pending_id == id && pending_boot_sequence == boot_sequence => {}
            Some(pending) => {
                return Err(DriveError::Busy {
                    pending: pending.kind(),
                });
            }
            None => {
                let decision = self
                    .index
                    .boot_recovery(id, boot_sequence)
                    .map_err(|error| self.latch(StorageFault::ModelApplyRejected(error)))?;
                let transition = match decision {
                    BootRecoveryDecision::ReplayQueued => {
                        return Ok(BootRecoveryProgress::ReplayQueued);
                    }
                    BootRecoveryDecision::AlreadyFinal => {
                        return Ok(BootRecoveryProgress::AlreadyFinal);
                    }
                    BootRecoveryDecision::FinalizeInterrupted(transition) => transition,
                };
                let planned = match self
                    .index
                    .plan_transition(transition)
                    .map_err(|error| self.latch(StorageFault::ModelApplyRejected(error)))?
                {
                    PlanOutcome::Append(planned) => planned,
                    PlanOutcome::AlreadyEquivalent => {
                        return Err(self.latch(StorageFault::LogicalConflict));
                    }
                };
                self.pending = Some(PendingMutation::BootRecovery {
                    id,
                    boot_sequence,
                    planned,
                });
            }
        }

        match self.drive_pending(access)? {
            PendingProgress::BootRecoveryFinalized(finalized) if finalized == id => {
                Ok(BootRecoveryProgress::Finalized)
            }
            PendingProgress::Idle
            | PendingProgress::AcceptanceCommitted(_)
            | PendingProgress::BootRecoveryFinalized(_)
            | PendingProgress::ProjectorCommitted
            | PendingProgress::AcceptanceCapacityExhausted => {
                Err(self.latch(StorageFault::LogicalConflict))
            }
        }
    }

    /// Drive one exact projector request through durable append and correlated
    /// live-index application.
    ///
    /// The actor retains only the request handle; the exact request remains in
    /// its sole projector and is resolved again before every flash mutation.
    pub fn persist_projector<A: BoundJournalAccess>(
        &mut self,
        access: &mut A,
        request: PersistRequest,
    ) -> Result<PersistenceProgress, DriveError<A::Error>> {
        self.ensure_healthy()?;
        self.ensure_binding(access)?;
        self.ensure_projector_healthy()?;
        if self.projector.persistence_request(request.handle()) != Some(request) {
            return Err(self.latch(StorageFault::ProjectorRequestMismatch));
        }
        match self.pending {
            Some(PendingMutation::Projector { handle }) if handle == request.handle() => {}
            Some(pending) => {
                return Err(DriveError::Busy {
                    pending: pending.kind(),
                });
            }
            None => {
                self.pending = Some(PendingMutation::Projector {
                    handle: request.handle(),
                });
            }
        }

        match self.drive_pending(access)? {
            PendingProgress::ProjectorCommitted => Ok(PersistenceProgress::Committed),
            PendingProgress::Idle
            | PendingProgress::AcceptanceCommitted(_)
            | PendingProgress::BootRecoveryFinalized(_)
            | PendingProgress::AcceptanceCapacityExhausted => {
                Err(self.latch(StorageFault::ProjectorRequestMismatch))
            }
        }
    }

    /// Autonomously reconcile the exact mutation retained after backend ambiguity.
    ///
    /// No caller-owned candidate, request, or projector is needed. A further
    /// backend error leaves the same actor-owned state retained for another call.
    pub fn drive_pending<A: BoundJournalAccess>(
        &mut self,
        access: &mut A,
    ) -> Result<PendingProgress, DriveError<A::Error>> {
        self.ensure_healthy()?;
        self.ensure_binding(access)?;
        match self.pending {
            None => Ok(PendingProgress::Idle),
            Some(PendingMutation::Acceptance { planned }) => {
                let id = match planned.entry() {
                    JournalEntry::Accepted(accepted) => accepted.id(),
                    _ => return Err(self.latch(StorageFault::LogicalConflict)),
                };
                match self.drive_append(access, planned.entry())? {
                    PhysicalProgress::Durable(_) => {
                        if let Err(error) = self.index.apply_planned(planned) {
                            return Err(self.latch(StorageFault::ModelApplyRejected(error)));
                        }
                        self.pending = None;
                        Ok(PendingProgress::AcceptanceCommitted(id))
                    }
                    PhysicalProgress::AcceptanceCapacityExhausted => {
                        self.pending = None;
                        Ok(PendingProgress::AcceptanceCapacityExhausted)
                    }
                }
            }
            Some(PendingMutation::BootRecovery { id, planned, .. }) => {
                match self.drive_append(access, planned.entry())? {
                    PhysicalProgress::Durable(_) => {
                        if let Err(error) = self.index.apply_planned(planned) {
                            return Err(self.latch(StorageFault::ModelApplyRejected(error)));
                        }
                        self.pending = None;
                        Ok(PendingProgress::BootRecoveryFinalized(id))
                    }
                    PhysicalProgress::AcceptanceCapacityExhausted => {
                        Err(self.latch(StorageFault::ReservationInvariant))
                    }
                }
            }
            Some(PendingMutation::Projector { handle }) => {
                self.ensure_projector_healthy()?;
                let Some(request) = self.projector.persistence_request(handle) else {
                    return Err(self.latch(StorageFault::ProjectorRequestMismatch));
                };
                match self.drive_append(access, request.entry()) {
                    Err(DriveError::Backend(error)) => {
                        match self.projector.report_persistence(
                            &mut self.index,
                            request,
                            PersistenceReply::Retryable,
                        ) {
                            Ok(PersistenceProgress::RetainedForRetry) => {
                                Err(DriveError::Backend(error))
                            }
                            Ok(PersistenceProgress::Committed) => {
                                Err(self.latch(StorageFault::ProjectorRequestMismatch))
                            }
                            Err(projector_error) => {
                                Err(self.latch(StorageFault::ProjectorRejected(projector_error)))
                            }
                        }
                    }
                    Err(error) => Err(error),
                    Ok(PhysicalProgress::AcceptanceCapacityExhausted) => {
                        Err(self.latch(StorageFault::ReservationInvariant))
                    }
                    Ok(PhysicalProgress::Durable(reply)) => {
                        let progress = self
                            .projector
                            .report_persistence(&mut self.index, request, reply)
                            .map_err(|error| self.latch(StorageFault::ProjectorRejected(error)))?;
                        if progress != PersistenceProgress::Committed {
                            return Err(self.latch(StorageFault::ProjectorRequestMismatch));
                        }
                        self.pending = None;
                        Ok(PendingProgress::ProjectorCommitted)
                    }
                }
            }
        }
    }

    fn drive_append<A: BoundJournalAccess>(
        &mut self,
        access: &mut A,
        entry: JournalEntry,
    ) -> Result<PhysicalProgress, DriveError<A::Error>> {
        self.ensure_binding(access)?;
        let first = self.first_submission_id;
        match append_with_replay_scratch::<SUBMISSIONS, A>(
            access,
            first,
            entry,
            &mut self.replay_scratch,
        ) {
            Ok(outcome) => return Ok(self.observe_append(outcome)),
            Err(JournalError::Backend(error)) => return Err(DriveError::Backend(error)),
            Err(JournalError::AcceptanceCapacityExhausted) => {
                return Ok(PhysicalProgress::AcceptanceCapacityExhausted);
            }
            Err(JournalError::NeedsCompaction | JournalError::CompactionPending) => {}
            Err(error) => return Err(self.latch(map_journal_fault(error))),
        }

        self.ensure_binding(access)?;
        self.state = match compact_with_replay_scratch::<SUBMISSIONS, A>(
            access,
            first,
            &mut self.replay_scratch,
        ) {
            Ok(state) => state,
            Err(JournalError::Backend(error)) => return Err(DriveError::Backend(error)),
            Err(error) => return Err(self.latch(map_journal_fault(error))),
        };

        self.ensure_binding(access)?;
        match append_with_replay_scratch::<SUBMISSIONS, A>(
            access,
            first,
            entry,
            &mut self.replay_scratch,
        ) {
            Ok(outcome) => Ok(self.observe_append(outcome)),
            Err(JournalError::Backend(error)) => Err(DriveError::Backend(error)),
            Err(JournalError::AcceptanceCapacityExhausted) => {
                Ok(PhysicalProgress::AcceptanceCapacityExhausted)
            }
            Err(JournalError::NeedsCompaction | JournalError::CompactionPending) => {
                Err(self.latch(StorageFault::CompactionDidNotClear))
            }
            Err(error) => Err(self.latch(map_journal_fault(error))),
        }
    }

    fn observe_append(&mut self, outcome: AppendOutcome) -> PhysicalProgress {
        match outcome {
            AppendOutcome::Appended(state) => {
                self.state = state;
                PhysicalProgress::Durable(PersistenceReply::Applied)
            }
            AppendOutcome::AlreadyEquivalent(state) => {
                self.state = state;
                PhysicalProgress::Durable(PersistenceReply::AlreadyEquivalent)
            }
        }
    }

    fn ensure_healthy<E>(&self) -> Result<(), DriveError<E>> {
        match self.fault {
            Some(fault) => Err(DriveError::Faulted(fault)),
            None => Ok(()),
        }
    }

    fn ensure_projector_healthy<E>(&mut self) -> Result<(), DriveError<E>> {
        match self.projector.fault() {
            Some(fault) => Err(self.latch(StorageFault::ProjectorRejected(
                ProjectorError::Faulted(fault),
            ))),
            None => Ok(()),
        }
    }

    fn ensure_projector_operation(&mut self) -> Result<(), ProjectorOperationError> {
        if let Some(fault) = self.fault {
            return Err(ProjectorOperationError::Faulted(fault));
        }
        if let Some(pending) = self.pending {
            return Err(ProjectorOperationError::Busy {
                pending: pending.kind(),
            });
        }
        if let Some(projector_fault) = self.projector.fault() {
            return Err(self.latch_projector_operation(ProjectorError::Faulted(projector_fault)));
        }
        Ok(())
    }

    fn map_projector_operation<T>(
        &mut self,
        result: Result<T, ProjectorError>,
    ) -> Result<T, ProjectorOperationError> {
        match result {
            Ok(value) => Ok(value),
            Err(error @ ProjectorError::Faulted(_)) => Err(self.latch_projector_operation(error)),
            Err(error) => Err(ProjectorOperationError::Rejected(error)),
        }
    }

    fn ensure_binding<A: BoundJournalAccess>(
        &self,
        access: &A,
    ) -> Result<(), DriveError<A::Error>> {
        self.validate_access(access).map_err(DriveError::Binding)
    }

    fn latch<E>(&mut self, fault: StorageFault) -> DriveError<E> {
        let fault = *self.fault.get_or_insert(fault);
        DriveError::Faulted(fault)
    }

    fn latch_projector_operation(&mut self, error: ProjectorError) -> ProjectorOperationError {
        let fault = *self
            .fault
            .get_or_insert(StorageFault::ProjectorRejected(error));
        ProjectorOperationError::Faulted(fault)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhysicalProgress {
    Durable(PersistenceReply),
    AcceptanceCapacityExhausted,
}

fn validate_exact_binding(
    expected: JournalBinding,
    actual: JournalBinding,
) -> Result<(), JournalBindingError> {
    if actual.device != expected.device {
        return Err(JournalBindingError::DeviceMismatch {
            expected: expected.device,
            actual: actual.device,
        });
    }
    if actual.absolute_offset != expected.absolute_offset || actual.length != expected.length {
        return Err(JournalBindingError::RangeMismatch {
            expected_absolute_offset: expected.absolute_offset,
            expected_length: expected.length,
            actual_absolute_offset: actual.absolute_offset,
            actual_length: actual.length,
        });
    }
    if actual.layout_version != expected.layout_version {
        return Err(JournalBindingError::LayoutVersionMismatch {
            expected: expected.layout_version,
            actual: actual.layout_version,
        });
    }
    Ok(())
}

fn validate_binding<A: BoundJournalAccess>(access: &A) -> Result<(), JournalBindingError> {
    let binding = access.journal_binding();
    if binding.layout_version != PHYSICAL_FORMAT_VERSION {
        return Err(JournalBindingError::LayoutVersionMismatch {
            expected: PHYSICAL_FORMAT_VERSION,
            actual: binding.layout_version,
        });
    }
    if binding.length != PARTITION_SIZE {
        return Err(JournalBindingError::LengthMismatch {
            expected: PARTITION_SIZE,
            actual: binding.length,
        });
    }
    if binding
        .absolute_offset
        .checked_add(binding.length)
        .is_none()
    {
        return Err(JournalBindingError::RangeOverflow {
            absolute_offset: binding.absolute_offset,
            length: binding.length,
        });
    }

    let read_size = <A as ReadNorFlash>::READ_SIZE;
    let write_size = <A as NorFlash>::WRITE_SIZE;
    let erase_size = <A as NorFlash>::ERASE_SIZE;
    let aligned =
        |value: usize, alignment: usize| alignment != 0 && value.is_multiple_of(alignment);
    if !aligned(binding.absolute_offset, read_size)
        || !aligned(binding.length, read_size)
        || !aligned(binding.absolute_offset, write_size)
        || !aligned(binding.length, write_size)
        || !aligned(binding.absolute_offset, erase_size)
        || !aligned(binding.length, erase_size)
    {
        return Err(JournalBindingError::AlignmentMismatch {
            absolute_offset: binding.absolute_offset,
            length: binding.length,
            read_size,
            write_size,
            erase_size,
        });
    }
    if access.capacity() != binding.length {
        return Err(JournalBindingError::CapacityMismatch {
            expected: binding.length,
            actual: access.capacity(),
        });
    }
    Ok(())
}

fn map_mount_error<E>(error: JournalError<E>) -> MountError<E> {
    match error {
        JournalError::Backend(error) => MountError::Backend(error),
        error => MountError::Fault(map_journal_fault(error)),
    }
}

fn map_journal_fault<E>(error: JournalError<E>) -> StorageFault {
    match error {
        JournalError::Backend(_) => unreachable_backend_mapping(),
        JournalError::IncompatibleFlash => StorageFault::IncompatibleFlash,
        JournalError::UnformattedErased => StorageFault::UnformattedErased,
        JournalError::UnformattedNonErased => StorageFault::UnformattedNonErased,
        JournalError::ManifestCorrupt => StorageFault::ManifestCorrupt,
        JournalError::ManifestConflict => StorageFault::ManifestConflict,
        JournalError::UnsupportedPhysicalVersion(version) => {
            StorageFault::UnsupportedPhysicalVersion(version)
        }
        JournalError::UnsupportedSemanticVersion(version) => {
            StorageFault::UnsupportedSemanticVersion(version)
        }
        JournalError::GeometryMismatch => StorageFault::GeometryMismatch,
        JournalError::SlotCorrupt { slot, reason } => StorageFault::SlotCorrupt { slot, reason },
        JournalError::BaselineMismatch => StorageFault::BaselineMismatch,
        JournalError::DuplicateLogicalKey => StorageFault::DuplicateLogicalKey,
        JournalError::SemanticReplay(error) => StorageFault::SemanticReplay(error),
        JournalError::LogicalConflict => StorageFault::LogicalConflict,
        JournalError::NeedsCompaction | JournalError::CompactionPending => {
            StorageFault::CompactionDidNotClear
        }
        JournalError::GenerationExhausted => StorageFault::GenerationExhausted,
        JournalError::CompactionInvariant => StorageFault::CompactionInvariant,
        JournalError::AcceptanceCapacityExhausted => StorageFault::ReservationInvariant,
        JournalError::ReadbackMismatch => StorageFault::ReadbackMismatch,
        JournalError::ReservationInvariant => StorageFault::ReservationInvariant,
    }
}

fn unreachable_backend_mapping() -> ! {
    unreachable!("backend errors are handled before bounded fault mapping")
}
