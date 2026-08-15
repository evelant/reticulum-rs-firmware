//! Fixed-capacity acceptance, replay, and lifecycle index.

use core::mem::MaybeUninit;

use crate::model::{
    Accepted, AuditEntry, AuditEvent, AuthorizationSnapshot, BootRecoveryMarker, FinalDisposition,
    IdempotencyKey, InternalFailure, InterruptedState, JournalEntry, LifecycleState,
    PreparedPacketDetails, PrincipalId, RnsAttemptToken, StateTransition, SubmissionFailure,
    SubmissionId, SubmissionIntent, TransitionError, validate_transition,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LastApplied {
    State(LifecycleState),
    Audit(AuditEvent),
}

/// One accepted submission and its latest reconstructed durable state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexedSubmission {
    accepted: Accepted,
    revision: u64,
    state: LifecycleState,
    prepared_details: Option<PreparedPacketDetails>,
    rns_attempt_token: Option<RnsAttemptToken>,
    transport_audit: Option<AuditEvent>,
    may_have_transmitted: bool,
    preparation_barrier_crossed: bool,
    last_applied: Option<LastApplied>,
}

impl IndexedSubmission {
    fn new(accepted: Accepted) -> Self {
        Self {
            accepted,
            revision: 0,
            state: LifecycleState::Queued,
            prepared_details: None,
            rns_attempt_token: None,
            transport_audit: None,
            may_have_transmitted: false,
            preparation_barrier_crossed: false,
            last_applied: None,
        }
    }

    /// Immutable acceptance record.
    pub const fn accepted(self) -> Accepted {
        self.accepted
    }

    /// Latest applied per-submission revision.
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Latest reconstructed public lifecycle state.
    pub const fn state(self) -> LifecycleState {
        self.state
    }

    /// Last durable prepared-packet metadata observed for this submission.
    ///
    /// This remains available after a later failed final state no longer
    /// exposes packet metadata, allowing recovery and quarantine observations
    /// to be correlated without changing the public final disposition.
    pub const fn prepared_details(self) -> Option<PreparedPacketDetails> {
        self.prepared_details
    }

    /// Durable attempt token established by preparation or transport audit.
    pub const fn rns_attempt_token(self) -> Option<RnsAttemptToken> {
        self.rns_attempt_token
    }

    /// Sole durable transport audit accepted for this submission.
    ///
    /// Generic submissions have one packet attempt. Coherent recovered carrier
    /// owners inside a durable LXMF `Preparing` loop are attempt-volatile and do
    /// not consume this record; quarantine remains durable and fail-closed.
    /// Replay rejects a second persisted transport audit.
    pub const fn transport_audit(self) -> Option<AuditEvent> {
        self.transport_audit
    }

    /// Conservative accumulated transmission uncertainty for the attempt.
    pub const fn may_have_transmitted(self) -> bool {
        self.may_have_transmitted
    }
}

/// Complete owned candidate for principal-scoped acceptance planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptanceCandidate {
    principal: PrincipalId,
    idempotency_key: IdempotencyKey,
    intent: SubmissionIntent,
    authorization: AuthorizationSnapshot,
}

impl AcceptanceCandidate {
    /// Construct one candidate after authentication and semantic validation.
    pub fn new<I>(
        principal: PrincipalId,
        idempotency_key: IdempotencyKey,
        intent: I,
        authorization: AuthorizationSnapshot,
    ) -> Self
    where
        I: Into<SubmissionIntent>,
    {
        Self {
            principal,
            idempotency_key,
            intent: intent.into(),
            authorization,
        }
    }

    /// Authenticated principal owning a successful acceptance.
    pub const fn principal(self) -> PrincipalId {
        self.principal
    }

    /// Principal-scoped idempotency key.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }

    /// Complete fixed-capacity intent content.
    pub const fn intent(self) -> SubmissionIntent {
        self.intent
    }

    /// Exact authorization facts to persist with a new acceptance.
    pub const fn authorization(self) -> AuthorizationSnapshot {
        self.authorization
    }
}

/// Result of planning a new acceptance or deduplicating one candidate.
// The allocation-free success variant deliberately owns the complete bounded
// intent so the storage actor can append it without borrowing caller memory.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptOutcome {
    /// New validated acceptance requiring independent backend capacity.
    Accepted(PlannedMutation),
    /// The same principal/key/content already identifies this submission.
    Replay(SubmissionId),
    /// The same principal and key were previously applied for different content.
    IdempotencyConflict {
        /// Existing submission protected from mutation.
        existing: SubmissionId,
    },
    /// Fixed in-RAM submission index has no free slot.
    IndexExhausted,
    /// No later submission identifier can be represented.
    IdentifierExhausted,
}

/// Opaque exact journal mutation validated against one live index state.
///
/// Borrow the record for durable encoding with [`Self::entry`], then consume
/// the same value through [`SubmissionIndex::apply_planned`] only after that
/// exact record has been appended successfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlannedMutation {
    entry: JournalEntry,
}

impl PlannedMutation {
    const fn new(entry: JournalEntry) -> Self {
        Self { entry }
    }

    /// Exact immutable record validated by the planner.
    pub const fn entry(&self) -> JournalEntry {
        self.entry
    }
}

/// Fully validated non-mutating plan for one revisioned journal mutation.
// The append variant owns a complete bounded record so no caller borrow can
// change between validation and durable encoding.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanOutcome {
    /// Exact validated record the sole storage actor may append and then apply.
    Append(PlannedMutation),
    /// The exact latest revision is already represented; no append is needed.
    AlreadyEquivalent,
}

/// Result of applying an immutable replay or mutation record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    /// Record advanced reconstructed semantic state.
    Applied,
    /// Exact same record was already the latest applied revision.
    AlreadyEquivalent,
}

/// Failure to validate or apply an acceptance, transition, or audit record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyError {
    /// Referenced submission is absent from the bounded index.
    UnknownSubmission,
    /// Fixed in-RAM submission index has no free slot.
    IndexExhausted,
    /// Submission ID already names a different immutable acceptance.
    SubmissionConflict,
    /// Principal-scoped idempotency key already names different content or ID.
    IdempotencyConflict,
    /// Entry repeats the latest revision with different content.
    RevisionConflict,
    /// Entry revision precedes the latest reconstructed revision.
    StaleRevision,
    /// Entry skipped at least one required revision.
    RevisionGap {
        /// Next exact revision required by the index.
        expected: u64,
        /// Revision supplied by the entry.
        actual: u64,
    },
    /// Latest revision cannot be incremented without wrapping.
    RevisionExhausted,
    /// Lifecycle transition violates the durable state machine.
    IllegalTransition(TransitionError),
    /// Audit observation contradicts current reconstructed lifecycle state.
    AuditStateMismatch,
    /// Prepared metadata or audit token differs from the durable attempt token.
    AttemptTokenMismatch,
    /// The bounded audit kind was already recorded for this submission.
    AuditCardinalityExceeded,
}

/// Boot action derived solely from a completely replayed durable lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootRecoveryDecision {
    /// Queued intent remains replay-safe and should be submitted again.
    ReplayQueued,
    /// An exact durable LXMF message remains a continuing delivery obligation.
    ///
    /// Individual Reticulum packet attempts are boot-volatile. Recovery
    /// prepares a fresh packet for the unchanged signed LXMF wire instead of
    /// terminalizing the logical message because an earlier attempt may have
    /// been interrupted.
    ReplayPendingLxmf,
    /// Replay-unsafe unfinished work must commit this conservative final record.
    FinalizeInterrupted(StateTransition),
    /// Submission was already final before this boot.
    AlreadyFinal,
}

/// Replay-only builder that cannot plan live work or derive boot decisions.
///
/// The caller applies every authenticated backend record in durable order and
/// then consumes this value with [`Self::complete`]. Even an empty backend must
/// take that explicit zero-record completion path. Calling `complete` asserts
/// that the backend, not this semantic model, has established replay EOF.
///
/// Planning and boot recovery are structurally unavailable before completion:
///
/// ```compile_fail
/// use reticulum_storage_model::{SubmissionId, SubmissionReplay};
/// let replay = SubmissionReplay::<1>::new(SubmissionId::new(1));
/// let _ = replay.boot_recovery(SubmissionId::new(1), 1);
/// let _ = replay.plan_transition(todo!());
/// ```
pub struct SubmissionReplay<const SUBMISSIONS: usize> {
    index: SubmissionIndex<SUBMISSIONS>,
    fault: Option<ApplyError>,
}

/// Replay-only builder borrowing an index initialized directly in caller storage.
///
/// This has the same fail-sticky semantic contract as [`SubmissionReplay`], but
/// never owns or returns the potentially large fixed-capacity index by value.
/// It exists for PSRAM-backed products whose CPU stack cannot hold a second
/// transient index while mounting or validating durable journal state.
pub struct SubmissionReplayInPlace<'a, const SUBMISSIONS: usize> {
    index: &'a mut SubmissionIndex<SUBMISSIONS>,
    fault: Option<ApplyError>,
}

impl<const SUBMISSIONS: usize> SubmissionReplay<SUBMISSIONS> {
    /// Begin replay with an explicit first submission identifier.
    pub const fn new(first_id: SubmissionId) -> Self {
        Self {
            index: SubmissionIndex::empty(first_id),
            fault: None,
        }
    }

    /// Begin replay by initializing an index directly in uninitialized caller storage.
    ///
    /// No complete `SubmissionIndex` temporary is constructed. Every slot and
    /// the next-identifier field are initialized at their final addresses
    /// before the returned replay builder can expose them. The destination is
    /// fully initialized immediately on return; callers must still invoke
    /// [`SubmissionReplayInPlace::complete`] only after authenticated backend
    /// EOF before treating it as live state.
    ///
    /// The caller must supply genuinely uninitialized storage or storage whose
    /// only prior use was a failed journal placement attempt documented as
    /// retryable. Reusing storage that contains an exposed live index remains
    /// invalid. The compile-time no-drop invariant below makes failed-attempt
    /// overwrite safe and fails closed if a future index field owns resources.
    #[allow(
        unsafe_code,
        reason = "audited field-wise initialization avoids a capacity-sized stack temporary"
    )]
    pub fn begin_in<'a>(
        destination: &'a mut MaybeUninit<SubmissionIndex<SUBMISSIONS>>,
        first_id: SubmissionId,
    ) -> SubmissionReplayInPlace<'a, SUBMISSIONS> {
        const {
            assert!(
                !core::mem::needs_drop::<SubmissionIndex<SUBMISSIONS>>(),
                "in-place replay retries require a no-drop SubmissionIndex"
            );
        }
        let index = destination.as_mut_ptr();
        // SAFETY: `index` is aligned and uniquely borrowed from an
        // uninitialized `SubmissionIndex`. The array pointer has the address
        // and alignment of its first element; each in-bounds slot is written
        // exactly once (including the zero-capacity case, where the loop is
        // empty), then `next_id` is initialized. Only after every field is
        // initialized is `&mut SubmissionIndex` created and exposed through
        // the replay-only wrapper.
        let index = unsafe {
            let slots = core::ptr::addr_of_mut!((*index).slots).cast::<Option<IndexedSubmission>>();
            for slot in 0..SUBMISSIONS {
                slots.add(slot).write(None);
            }
            core::ptr::addr_of_mut!((*index).next_id).write(Some(first_id));
            &mut *index
        };
        SubmissionReplayInPlace { index, fault: None }
    }

    /// Restart replay in an already initialized caller-owned scratch index.
    ///
    /// Slots are cleared one at a time so the operation's stack use is
    /// independent of `SUBMISSIONS`.
    pub fn restart_in<'a>(
        index: &'a mut SubmissionIndex<SUBMISSIONS>,
        first_id: SubmissionId,
    ) -> SubmissionReplayInPlace<'a, SUBMISSIONS> {
        for slot in &mut index.slots {
            *slot = None;
        }
        index.next_id = Some(first_id);
        SubmissionReplayInPlace { index, fault: None }
    }

    /// Apply one immutable record while reconstructing durable semantic state.
    ///
    /// The first rejected record permanently poisons this replay builder. Later
    /// calls return that same error without applying more records, and
    /// [`Self::complete`] refuses to expose the surviving prefix as live state.
    pub fn apply_entry(&mut self, entry: JournalEntry) -> Result<ApplyOutcome, ApplyError> {
        apply_replay_entry(&mut self.index, &mut self.fault, entry)
    }

    /// Seal a successful replay and expose the live planning and boot-recovery index.
    ///
    /// Returns the first semantic replay error if any decoded durable record was
    /// rejected. A caller must also establish authenticated backend EOF before
    /// invoking this method; the semantic model cannot inspect the backend.
    pub const fn complete(self) -> Result<SubmissionIndex<SUBMISSIONS>, ApplyError> {
        match self.fault {
            Some(error) => Err(error),
            None => Ok(self.index),
        }
    }
}

impl<'a, const SUBMISSIONS: usize> SubmissionReplayInPlace<'a, SUBMISSIONS> {
    /// Apply one immutable record while reconstructing durable semantic state.
    ///
    /// The first rejected record permanently poisons this replay builder, just
    /// as it does for [`SubmissionReplay::apply_entry`].
    pub fn apply_entry(&mut self, entry: JournalEntry) -> Result<ApplyOutcome, ApplyError> {
        apply_replay_entry(self.index, &mut self.fault, entry)
    }

    /// Seal successful replay and return the caller-owned live index in place.
    ///
    /// Returns the first semantic replay error if any decoded durable record
    /// was rejected. The caller must establish authenticated backend EOF before
    /// invoking this method.
    pub fn complete(self) -> Result<&'a mut SubmissionIndex<SUBMISSIONS>, ApplyError> {
        match self.fault {
            Some(error) => Err(error),
            None => Ok(self.index),
        }
    }
}

fn apply_replay_entry<const SUBMISSIONS: usize>(
    index: &mut SubmissionIndex<SUBMISSIONS>,
    fault: &mut Option<ApplyError>,
    entry: JournalEntry,
) -> Result<ApplyOutcome, ApplyError> {
    if let Some(error) = *fault {
        return Err(error);
    }
    match index.apply_entry_internal(entry) {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            *fault = Some(error);
            Err(error)
        }
    }
}

/// Live bounded semantic index produced only after explicit replay completion.
///
/// This model deliberately performs no physical journal-capacity accounting or
/// reservation. Before publishing acceptance or appending any `Append` plan,
/// the sole storage actor must independently establish backend durability,
/// quota, retention, compaction, and required future-record reservation.
pub struct SubmissionIndex<const SUBMISSIONS: usize> {
    slots: [Option<IndexedSubmission>; SUBMISSIONS],
    next_id: Option<SubmissionId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationPreflight {
    AlreadyEquivalent,
    Apply { slot: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcceptancePreflight {
    AlreadyEquivalent,
    Apply { slot: usize },
}

impl<const SUBMISSIONS: usize> SubmissionIndex<SUBMISSIONS> {
    const fn empty(first_id: SubmissionId) -> Self {
        Self {
            slots: [None; SUBMISSIONS],
            next_id: Some(first_id),
        }
    }

    /// Next identifier that a new successful acceptance would consume.
    pub const fn next_id(&self) -> Option<SubmissionId> {
        self.next_id
    }

    /// Look up one submission without exposing mutable index storage.
    pub fn get(&self, id: SubmissionId) -> Option<IndexedSubmission> {
        self.slots
            .iter()
            .flatten()
            .copied()
            .find(|submission| submission.accepted.id() == id)
    }

    /// Look up one submission only when it belongs to the supplied principal.
    pub fn get_owned(&self, principal: PrincipalId, id: SubmissionId) -> Option<IndexedSubmission> {
        self.slots.iter().flatten().copied().find(|submission| {
            let id_matches = submission.accepted.id() == id;
            let principal_matches = submission.accepted.principal() == principal;
            id_matches & principal_matches
        })
    }

    /// Look up only the public lifecycle state of a principal-owned submission.
    ///
    /// Missing and foreign identifiers both scan the complete fixed slot table
    /// and return `None`; the large durable intent is not copied into a status
    /// request's stack frame.
    pub fn get_owned_state(
        &self,
        principal: PrincipalId,
        id: SubmissionId,
    ) -> Option<LifecycleState> {
        self.slots
            .iter()
            .flatten()
            .find(|submission| {
                let id_matches = submission.accepted.id() == id;
                let principal_matches = submission.accepted.principal() == principal;
                id_matches & principal_matches
            })
            .map(|submission| submission.state)
    }

    /// Iterate submissions in stable fixed-slot order.
    pub fn iter(&self) -> impl Iterator<Item = IndexedSubmission> + '_ {
        self.slots.iter().flatten().copied()
    }

    /// Plan acceptance without consuming an identifier or mutating the index.
    ///
    /// An `Accepted` result establishes only request, ID, and index feasibility.
    /// It is not a physical backend reservation. The sole storage actor must first
    /// obtain whatever append and future-terminal guarantees its backend
    /// contract requires, append the returned plan's exact record, retain and
    /// consume that plan through [`Self::apply_planned`], and only then publish
    /// acceptance. If a commit reply is lost, backend
    /// readback/replay applies the record before replanning returns `Replay`.
    pub fn plan_accept(&self, candidate: AcceptanceCandidate) -> AcceptOutcome {
        let principal = candidate.principal();
        let idempotency_key = candidate.idempotency_key();
        let intent = candidate.intent();
        if let Some(existing) = self.slots.iter().flatten().find(|submission| {
            submission.accepted.principal() == principal
                && submission.accepted.idempotency_key() == idempotency_key
        }) {
            return if existing.accepted.intent() == intent {
                AcceptOutcome::Replay(existing.accepted.id())
            } else {
                AcceptOutcome::IdempotencyConflict {
                    existing: existing.accepted.id(),
                }
            };
        }

        if !self.slots.iter().any(Option::is_none) {
            return AcceptOutcome::IndexExhausted;
        }
        let Some(id) = self.next_id else {
            return AcceptOutcome::IdentifierExhausted;
        };

        AcceptOutcome::Accepted(PlannedMutation::new(JournalEntry::Accepted(Accepted::new(
            id,
            principal,
            idempotency_key,
            intent,
            candidate.authorization(),
        ))))
    }

    /// Validate a transition fully without mutating semantic state.
    ///
    /// Under sole-actor serialization, applying the exact returned `Append`
    /// entry immediately after its successful backend append cannot discover a
    /// new model validation failure.
    pub fn plan_transition(&self, transition: StateTransition) -> Result<PlanOutcome, ApplyError> {
        match self.preflight_transition(transition)? {
            MutationPreflight::AlreadyEquivalent => Ok(PlanOutcome::AlreadyEquivalent),
            MutationPreflight::Apply { .. } => Ok(PlanOutcome::Append(PlannedMutation::new(
                JournalEntry::StateTransition(transition),
            ))),
        }
    }

    /// Validate an audit fully without mutating semantic state.
    ///
    /// Under sole-actor serialization, applying the exact returned `Append`
    /// entry immediately after its successful backend append cannot discover a
    /// new model validation failure.
    pub fn plan_audit(&self, audit: AuditEntry) -> Result<PlanOutcome, ApplyError> {
        match self.preflight_audit(audit)? {
            MutationPreflight::AlreadyEquivalent => Ok(PlanOutcome::AlreadyEquivalent),
            MutationPreflight::Apply { .. } => Ok(PlanOutcome::Append(PlannedMutation::new(
                JournalEntry::Audit(audit),
            ))),
        }
    }

    /// Apply the exact opaque plan after its record was appended successfully.
    pub fn apply_planned(&mut self, planned: PlannedMutation) -> Result<ApplyOutcome, ApplyError> {
        self.apply_entry_internal(planned.entry)
    }

    fn apply_entry_internal(&mut self, entry: JournalEntry) -> Result<ApplyOutcome, ApplyError> {
        match entry {
            JournalEntry::Accepted(accepted) => self.apply_accepted(accepted),
            JournalEntry::StateTransition(transition) => self.apply_transition(transition),
            JournalEntry::Audit(audit) => self.apply_audit(audit),
        }
    }

    fn apply_accepted(&mut self, accepted: Accepted) -> Result<ApplyOutcome, ApplyError> {
        match self.preflight_accepted(accepted)? {
            AcceptancePreflight::AlreadyEquivalent => Ok(ApplyOutcome::AlreadyEquivalent),
            AcceptancePreflight::Apply { slot } => {
                self.slots[slot] = Some(IndexedSubmission::new(accepted));
                self.observe_applied_id(accepted.id());
                Ok(ApplyOutcome::Applied)
            }
        }
    }

    fn apply_transition(
        &mut self,
        transition: StateTransition,
    ) -> Result<ApplyOutcome, ApplyError> {
        let MutationPreflight::Apply { slot } = self.preflight_transition(transition)? else {
            return Ok(ApplyOutcome::AlreadyEquivalent);
        };
        let mut current = self.slots[slot].ok_or(ApplyError::UnknownSubmission)?;

        match transition.state() {
            LifecycleState::AwaitingDelivery(details) => {
                current.preparation_barrier_crossed = true;
                current.prepared_details = Some(details);
                current.rns_attempt_token = Some(details.rns_attempt_token());
                // Awaiting-delivery metadata follows authorization of the
                // frame boundary, so transmission is conservatively possible.
                current.may_have_transmitted = true;
            }
            LifecycleState::Final(FinalDisposition::Delivered(details)) => {
                current.preparation_barrier_crossed = true;
                current.prepared_details = Some(details);
                current.rns_attempt_token = Some(details.rns_attempt_token());
                // A proof can terminal the attempt before any frame
                // observation is persisted. Delivery itself still requires
                // conservative possible-transmission accounting.
                current.may_have_transmitted = true;
            }
            LifecycleState::Preparing => {
                current.preparation_barrier_crossed = true;
            }
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
                if current.preparation_barrier_crossed =>
            {
                // A timeout observed before prepared-frame metadata becomes
                // durable may race authorization. Without that ordering
                // proof, replay must preserve possible transmission.
                current.may_have_transmitted = true;
            }
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
                InternalFailure::InterruptedByReset(_),
            ))) if current.preparation_barrier_crossed => {
                // Reset can follow authorization but precede the durable
                // prepared-frame record. The exact interruption marker closes
                // replay, while the aggregate remains conservatively unsure.
                current.may_have_transmitted = true;
            }
            LifecycleState::Queued
            | LifecycleState::Final(FinalDisposition::Failed(_) | FinalDisposition::Cancelled) => {}
        }
        current.revision = transition.revision();
        current.state = transition.state();
        current.last_applied = Some(LastApplied::State(transition.state()));
        self.slots[slot] = Some(current);
        Ok(ApplyOutcome::Applied)
    }

    fn apply_audit(&mut self, audit: AuditEntry) -> Result<ApplyOutcome, ApplyError> {
        let MutationPreflight::Apply { slot } = self.preflight_audit(audit)? else {
            return Ok(ApplyOutcome::AlreadyEquivalent);
        };
        let mut current = self.slots[slot].ok_or(ApplyError::UnknownSubmission)?;

        let (rns_attempt_token, may_have_transmitted) = transport_observation(audit.event());
        current.rns_attempt_token = Some(rns_attempt_token);
        current.may_have_transmitted |= may_have_transmitted;
        current.transport_audit = Some(audit.event());
        current.revision = audit.revision();
        current.last_applied = Some(LastApplied::Audit(audit.event()));
        self.slots[slot] = Some(current);
        Ok(ApplyOutcome::Applied)
    }

    /// Derive reboot handling after complete durable replay.
    pub fn boot_recovery(
        &self,
        id: SubmissionId,
        boot_sequence: u64,
    ) -> Result<BootRecoveryDecision, ApplyError> {
        let current = self.get(id).ok_or(ApplyError::UnknownSubmission)?;
        match current.state {
            LifecycleState::Queued => Ok(BootRecoveryDecision::ReplayQueued),
            LifecycleState::Preparing
                if matches!(current.accepted.intent(), SubmissionIntent::LxmfMessage(_))
                    && !matches!(
                        current.transport_audit,
                        Some(AuditEvent::TransportQuarantined { .. })
                    ) =>
            {
                Ok(BootRecoveryDecision::ReplayPendingLxmf)
            }
            LifecycleState::Preparing | LifecycleState::AwaitingDelivery(_) => {
                let interrupted_state = match current.state {
                    LifecycleState::Preparing => InterruptedState::Preparing,
                    LifecycleState::AwaitingDelivery(_) => InterruptedState::AwaitingDelivery,
                    LifecycleState::Queued | LifecycleState::Final(_) => {
                        return Err(ApplyError::AuditStateMismatch);
                    }
                };
                let revision = current
                    .revision
                    .checked_add(1)
                    .ok_or(ApplyError::RevisionExhausted)?;
                let marker = BootRecoveryMarker::new(boot_sequence, interrupted_state);
                let transition = StateTransition::new(
                    id,
                    revision,
                    LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
                        InternalFailure::InterruptedByReset(marker),
                    ))),
                )
                .map_err(ApplyError::IllegalTransition)?;
                Ok(BootRecoveryDecision::FinalizeInterrupted(transition))
            }
            LifecycleState::Final(_) => Ok(BootRecoveryDecision::AlreadyFinal),
        }
    }

    fn preflight_accepted(&self, accepted: Accepted) -> Result<AcceptancePreflight, ApplyError> {
        if let Some(existing) = self.get(accepted.id()) {
            return if existing.accepted == accepted {
                Ok(AcceptancePreflight::AlreadyEquivalent)
            } else {
                Err(ApplyError::SubmissionConflict)
            };
        }
        if self.slots.iter().flatten().any(|existing| {
            existing.accepted.principal() == accepted.principal()
                && existing.accepted.idempotency_key() == accepted.idempotency_key()
        }) {
            return Err(ApplyError::IdempotencyConflict);
        }
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return Err(ApplyError::IndexExhausted);
        };
        Ok(AcceptancePreflight::Apply { slot })
    }

    fn preflight_transition(
        &self,
        transition: StateTransition,
    ) -> Result<MutationPreflight, ApplyError> {
        let slot = self
            .slot_for(transition.id())
            .ok_or(ApplyError::UnknownSubmission)?;
        let current = self.slots[slot].ok_or(ApplyError::UnknownSubmission)?;
        if compare_revision(
            current.revision,
            transition.revision(),
            current.last_applied == Some(LastApplied::State(transition.state())),
        )? == ApplyOutcome::AlreadyEquivalent
        {
            return Ok(MutationPreflight::AlreadyEquivalent);
        }
        validate_transition(current.state, transition.state())
            .map_err(ApplyError::IllegalTransition)?;
        let proposed_attempt_token = match transition.state() {
            LifecycleState::AwaitingDelivery(details)
            | LifecycleState::Final(FinalDisposition::Delivered(details)) => {
                Some(details.rns_attempt_token())
            }
            LifecycleState::Queued
            | LifecycleState::Preparing
            | LifecycleState::Final(FinalDisposition::Failed(_) | FinalDisposition::Cancelled) => {
                None
            }
        };
        if proposed_attempt_token.is_some_and(|proposed| {
            current
                .rns_attempt_token
                .is_some_and(|existing| existing != proposed)
        }) {
            return Err(ApplyError::AttemptTokenMismatch);
        }

        Ok(MutationPreflight::Apply { slot })
    }

    fn preflight_audit(&self, audit: AuditEntry) -> Result<MutationPreflight, ApplyError> {
        let slot = self
            .slot_for(audit.id())
            .ok_or(ApplyError::UnknownSubmission)?;
        let current = self.slots[slot].ok_or(ApplyError::UnknownSubmission)?;
        if compare_revision(
            current.revision,
            audit.revision(),
            current.last_applied == Some(LastApplied::Audit(audit.event())),
        )? == ApplyOutcome::AlreadyEquivalent
        {
            return Ok(MutationPreflight::AlreadyEquivalent);
        }
        validate_audit(current, audit.event())?;
        Ok(MutationPreflight::Apply { slot })
    }

    fn slot_for(&self, id: SubmissionId) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.is_some_and(|submission| submission.accepted.id() == id))
    }

    fn observe_applied_id(&mut self, id: SubmissionId) {
        self.next_id = match self.next_id {
            Some(next) if next.get() > id.get() => Some(next),
            Some(_) => id.get().checked_add(1).map(SubmissionId::new),
            None => None,
        };
    }
}

fn compare_revision(
    current: u64,
    supplied: u64,
    equivalent: bool,
) -> Result<ApplyOutcome, ApplyError> {
    if supplied == current {
        return if equivalent {
            Ok(ApplyOutcome::AlreadyEquivalent)
        } else {
            Err(ApplyError::RevisionConflict)
        };
    }
    if supplied < current {
        return Err(ApplyError::StaleRevision);
    }
    let expected = current
        .checked_add(1)
        .ok_or(ApplyError::RevisionExhausted)?;
    if supplied != expected {
        return Err(ApplyError::RevisionGap {
            expected,
            actual: supplied,
        });
    }
    Ok(ApplyOutcome::Applied)
}

fn validate_audit(submission: IndexedSubmission, event: AuditEvent) -> Result<(), ApplyError> {
    match event {
        // Transport observations may race a proof/timeout projection and are
        // legal after final state. Queued has not crossed the preparation
        // barrier and therefore cannot name a transport attempt.
        AuditEvent::TransportRecovered {
            rns_attempt_token, ..
        }
        | AuditEvent::TransportQuarantined {
            rns_attempt_token, ..
        } => {
            if submission.transport_audit.is_some() {
                return Err(ApplyError::AuditCardinalityExceeded);
            }
            if matches!(submission.state, LifecycleState::Queued)
                || !submission.preparation_barrier_crossed
            {
                return Err(ApplyError::AuditStateMismatch);
            }
            if submission
                .rns_attempt_token
                .is_some_and(|token| token != rns_attempt_token)
            {
                return Err(ApplyError::AttemptTokenMismatch);
            }
            Ok(())
        }
    }
}

const fn transport_observation(event: AuditEvent) -> (RnsAttemptToken, bool) {
    match event {
        AuditEvent::TransportRecovered {
            rns_attempt_token,
            may_have_transmitted,
            ..
        }
        | AuditEvent::TransportQuarantined {
            rns_attempt_token,
            may_have_transmitted,
            ..
        } => (rns_attempt_token, may_have_transmitted),
    }
}
