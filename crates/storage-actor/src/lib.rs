//! Sole-owner adapter joining the durable model, physical NOR journal, and
//! volatile submission projector.
//!
//! Construction is possible only through [`StorageActor::mount`], so no live
//! request can be served before complete physical scan and semantic replay.
//! The actor retains one exact mutation across ambiguous backend failures and
//! rejects unrelated work until retry establishes a durable conclusion.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use embedded_storage::nor_flash::MultiwriteNorFlash;
use reticulum_storage_journal::{
    AppendOutcome, JournalError, JournalState, SlotCorruption, append, compact, mount,
};
use reticulum_storage_model::{
    AcceptOutcome, AcceptanceCandidate, ApplyError, JournalEntry, PlannedMutation, SubmissionId,
    SubmissionIndex,
};
use reticulum_submission_projector::{
    PersistHandle, PersistRequest, PersistenceProgress, PersistenceReply, ProjectorError,
    SubmissionProjector,
};

#[cfg(test)]
mod tests;

/// Kind of exact mutation currently retained after an ambiguous drive result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingKind {
    /// A device-facing acceptance candidate and its exact planned record.
    Acceptance,
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
    /// A durably completed actor-owned model plan could not be applied.
    ModelApplyRejected(ApplyError),
    /// The request supplied to the actor is not exactly retained by its projector.
    ProjectorRequestMismatch,
    /// The projector rejected the actor's correlated persistence report.
    ProjectorRejected(ProjectorError),
}

/// Failure while establishing the only live actor state.
#[derive(Debug, Eq, PartialEq)]
pub enum MountError<E> {
    /// Underlying NOR operation failed before replay completion.
    Backend(E),
    /// Journal or model validation failed with a bounded diagnostic.
    Fault(StorageFault),
}

/// Recoverable or latched failure while driving one retained mutation.
#[derive(Debug, Eq, PartialEq)]
pub enum DriveError<E> {
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

/// Definitive progress from autonomously reconciling the actor's retained mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingProgress {
    /// No mutation was retained, so no storage operation was required.
    Idle,
    /// One retained acceptance is durable and applied to the live index.
    AcceptanceCommitted(SubmissionId),
    /// One retained projector mutation is durable and applied to the live index.
    ProjectorCommitted,
    /// The journal cannot reserve a complete schema lifetime for the acceptance.
    AcceptanceCapacityExhausted,
}

// The fixed actor cell deliberately owns the complete acceptance plan without
// allocation; the projector variant remains a compact handle into its sole owner.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingMutation {
    Acceptance { planned: PlannedMutation },
    Projector { handle: PersistHandle },
}

/// Target-layout byte size of the actor's actual optional pending-mutation cell.
///
/// This makes the RAM cost of exact ambiguous-write retention directly
/// measurable in host and firmware builds.
pub const PENDING_MUTATION_BYTES: usize = core::mem::size_of::<Option<PendingMutation>>();

// One actor-owned acceptance plan or one compact projector handle must fit.
// Projector requests remain solely in the actor-owned projector.
const _: () = assert!(PENDING_MUTATION_BYTES <= 512);

impl PendingMutation {
    const fn kind(self) -> PendingKind {
        match self {
            Self::Acceptance { .. } => PendingKind::Acceptance,
            Self::Projector { .. } => PendingKind::Projector,
        }
    }
}

/// Mounted sole owner of one journal partition, live index, and submission projector.
pub struct StorageActor<F, const SUBMISSIONS: usize, const PROJECTED: usize> {
    flash: F,
    state: JournalState,
    index: SubmissionIndex<SUBMISSIONS>,
    projector: SubmissionProjector<PROJECTED>,
    first_submission_id: SubmissionId,
    pending: Option<PendingMutation>,
    fault: Option<StorageFault>,
}

impl<F, const SUBMISSIONS: usize, const PROJECTED: usize> StorageActor<F, SUBMISSIONS, PROJECTED>
where
    F: MultiwriteNorFlash,
{
    /// Scan the complete physical journal and finish semantic replay before
    /// exposing a live actor.
    pub fn mount(
        mut flash: F,
        first_submission_id: SubmissionId,
    ) -> Result<Self, MountError<F::Error>> {
        let mounted =
            mount::<SUBMISSIONS, F>(&mut flash, first_submission_id).map_err(map_mount_error)?;
        let state = mounted.state();
        let index = mounted.into_index();
        Ok(Self {
            flash,
            state,
            index,
            projector: SubmissionProjector::new(),
            first_submission_id,
            pending: None,
            fault: None,
        })
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
    /// use embedded_storage::nor_flash::MultiwriteNorFlash;
    /// use reticulum_storage_actor::StorageActor;
    /// use reticulum_submission_projector::SubmissionProjector;
    ///
    /// fn replace_projector<F: MultiwriteNorFlash>(actor: &mut StorageActor<F, 1, 1>) {
    ///     let _ = mem::replace(actor.projector(), SubmissionProjector::new());
    /// }
    /// ```
    pub const fn projector(&self) -> &SubmissionProjector<PROJECTED> {
        &self.projector
    }

    /// Plan the durable no-replay barrier for one queued submission.
    ///
    /// The projector remains actor-owned; callers receive only the bounded
    /// progress value and must route any returned persistence request through
    /// [`Self::persist_projector`].
    pub fn begin_preparation(
        &mut self,
        id: SubmissionId,
    ) -> Result<reticulum_submission_projector::ProjectionProgress, ProjectorOperationError> {
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
        match self.projector.begin_preparation(&self.index, id) {
            Ok(progress) => Ok(progress),
            Err(error @ ProjectorError::Faulted(_)) => Err(self.latch_projector_operation(error)),
            Err(error) => Err(ProjectorOperationError::Rejected(error)),
        }
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

    /// Consume the actor and recover the owned backend for tests or recovery.
    pub fn into_flash(self) -> F {
        self.flash
    }

    /// Durably accept one exact authenticated candidate.
    ///
    /// A backend error retains the exact plan. Retrying the same candidate
    /// reconciles by journal readback; different work receives [`DriveError::Busy`].
    pub fn accept(
        &mut self,
        candidate: AcceptanceCandidate,
    ) -> Result<AcceptanceProgress, DriveError<F::Error>> {
        self.ensure_healthy()?;

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

        match self.drive_pending()? {
            PendingProgress::AcceptanceCommitted(id) => Ok(AcceptanceProgress::Accepted(id)),
            PendingProgress::AcceptanceCapacityExhausted => {
                Ok(AcceptanceProgress::JournalCapacityExhausted)
            }
            PendingProgress::Idle | PendingProgress::ProjectorCommitted => {
                Err(self.latch(StorageFault::LogicalConflict))
            }
        }
    }

    /// Drive one exact projector request through durable append and correlated
    /// live-index application.
    ///
    /// The actor retains only the request handle; the exact request remains in
    /// its sole projector and is resolved again before every flash mutation.
    pub fn persist_projector(
        &mut self,
        request: PersistRequest,
    ) -> Result<PersistenceProgress, DriveError<F::Error>> {
        self.ensure_healthy()?;
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

        match self.drive_pending()? {
            PendingProgress::ProjectorCommitted => Ok(PersistenceProgress::Committed),
            PendingProgress::Idle
            | PendingProgress::AcceptanceCommitted(_)
            | PendingProgress::AcceptanceCapacityExhausted => {
                Err(self.latch(StorageFault::ProjectorRequestMismatch))
            }
        }
    }

    /// Autonomously reconcile the exact mutation retained after backend ambiguity.
    ///
    /// No caller-owned candidate, request, or projector is needed. A further
    /// backend error leaves the same actor-owned state retained for another call.
    pub fn drive_pending(&mut self) -> Result<PendingProgress, DriveError<F::Error>> {
        self.ensure_healthy()?;
        match self.pending {
            None => Ok(PendingProgress::Idle),
            Some(PendingMutation::Acceptance { planned }) => {
                let id = match planned.entry() {
                    JournalEntry::Accepted(accepted) => accepted.id(),
                    _ => return Err(self.latch(StorageFault::LogicalConflict)),
                };
                match self.drive_append(planned.entry())? {
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
            Some(PendingMutation::Projector { handle }) => {
                self.ensure_projector_healthy()?;
                let Some(request) = self.projector.persistence_request(handle) else {
                    return Err(self.latch(StorageFault::ProjectorRequestMismatch));
                };
                match self.drive_append(request.entry()) {
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

    fn drive_append(
        &mut self,
        entry: JournalEntry,
    ) -> Result<PhysicalProgress, DriveError<F::Error>> {
        let first = self.first_submission_id;
        match append::<SUBMISSIONS, F>(&mut self.flash, first, entry) {
            Ok(outcome) => return Ok(self.observe_append(outcome)),
            Err(JournalError::Backend(error)) => return Err(DriveError::Backend(error)),
            Err(JournalError::AcceptanceCapacityExhausted) => {
                return Ok(PhysicalProgress::AcceptanceCapacityExhausted);
            }
            Err(JournalError::NeedsCompaction | JournalError::CompactionPending) => {}
            Err(error) => return Err(self.latch(map_journal_fault(error))),
        }

        self.state = match compact::<SUBMISSIONS, F>(&mut self.flash, first) {
            Ok(state) => state,
            Err(JournalError::Backend(error)) => return Err(DriveError::Backend(error)),
            Err(error) => return Err(self.latch(map_journal_fault(error))),
        };

        match append::<SUBMISSIONS, F>(&mut self.flash, first, entry) {
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

    fn ensure_healthy(&self) -> Result<(), DriveError<F::Error>> {
        match self.fault {
            Some(fault) => Err(DriveError::Faulted(fault)),
            None => Ok(()),
        }
    }

    fn ensure_projector_healthy(&mut self) -> Result<(), DriveError<F::Error>> {
        match self.projector.fault() {
            Some(fault) => Err(self.latch(StorageFault::ProjectorRejected(
                ProjectorError::Faulted(fault),
            ))),
            None => Ok(()),
        }
    }

    fn latch(&mut self, fault: StorageFault) -> DriveError<F::Error> {
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
