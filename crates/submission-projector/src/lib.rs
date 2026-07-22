//! Fixed-capacity projection from volatile Reticulum TX observations into the
//! durable submission journal.
//!
//! The projector implements a persist-before-act protocol. It first exposes an
//! owned [`PersistRequest`], retains that exact request across retryable storage
//! failures, and applies its opaque storage-model plan to the caller's live
//! [`SubmissionIndex`] only after [`PersistenceReply::Applied`] or
//! [`PersistenceReply::AlreadyEquivalent`]. The live index remains the sole
//! durable state and revision authority. Terminal and recovered-owner
//! acknowledgements are retained as exact scalar actions until their caller
//! reports completion.
//!
//! Attempt handles, packet slots, owner deadlines, and packet references are
//! never placed in a [`JournalEntry`]. An [`AttemptHandle`] and [`AttemptToken`]
//! exist here only in volatile fixed-capacity correlation slots. Reconstructing
//! durable state after reboot remains the responsibility of
//! `reticulum-storage-model`; volatile attempts must not be reconstructed.

#![no_std]
#![deny(unsafe_code)]
#![deny(missing_docs)]

use core::{array, mem::MaybeUninit};

use reticulum_node_core::{
    AttemptHandle, AttemptOutcome, AttemptToken, AttemptUnsentReason, AuthorizedFrameObservation,
    PACKET_CAPACITY, PreparedPacket, SubmitError, TerminalAttempt, TxRecoveryObservation,
    TxRecoveryReason,
};
use reticulum_storage_model::{
    ApplyError, AuditEntry, AuditEvent, EncodedPacketSha256, FinalDisposition, InternalFailure,
    JournalEntry, LifecycleState, MAX_ENCODED_PACKET_BYTES, PlanOutcome, PlannedMutation,
    PreparedPacketDetails, RnsAttemptToken, StateTransition, SubmissionFailure, SubmissionId,
    SubmissionIndex, TransportRecoveryReason,
};
const _: () = assert!(
    MAX_ENCODED_PACKET_BYTES == PACKET_CAPACITY,
    "storage-model and node-core encoded packet capacities must match"
);

/// Opaque generation for one exact retained journal write.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PersistHandle(u64);

/// One exact journal append/apply request retained by the projector.
///
/// Before replying [`PersistenceReply::Applied`], the sole storage actor must
/// independently hold the backend's required physical reservation and prove
/// that this exact entry was durably appended once. An ambiguous append is
/// retryable until exact readback can establish `Applied` or
/// `AlreadyEquivalent`. This crate performs no physical capacity accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistRequest {
    handle: PersistHandle,
    planned: PlannedMutation,
}

/// Backend-neutral scalar observation of one complete authorized Reticulum
/// frame.
///
/// Both the current RF-inert inspector and a future guarded radio backend can
/// supply this contract. It owns no packet bytes or references.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedFrameObservation {
    attempt_handle: AttemptHandle,
    attempt: AttemptToken,
    packet_len: usize,
    encoded_packet_sha256: [u8; 32],
}

impl PreparedFrameObservation {
    /// Construct a complete scalar frame observation.
    pub const fn new(
        attempt_handle: AttemptHandle,
        attempt: AttemptToken,
        packet_len: usize,
        encoded_packet_sha256: [u8; 32],
    ) -> Self {
        Self {
            attempt_handle,
            attempt,
            packet_len,
            encoded_packet_sha256,
        }
    }

    /// Generation-checked volatile attempt identity.
    pub const fn attempt_handle(self) -> AttemptHandle {
        self.attempt_handle
    }

    /// RNS proof-correlation token.
    pub const fn attempt(self) -> AttemptToken {
        self.attempt
    }

    /// Complete encoded frame length.
    pub const fn packet_len(self) -> usize {
        self.packet_len
    }

    /// SHA-256 over every complete encoded frame byte.
    pub const fn encoded_packet_sha256(self) -> [u8; 32] {
        self.encoded_packet_sha256
    }
}

impl From<AuthorizedFrameObservation> for PreparedFrameObservation {
    fn from(observation: AuthorizedFrameObservation) -> Self {
        Self::new(
            observation.attempt_handle(),
            observation.attempt(),
            observation.packet_len(),
            *observation.encoded_packet_sha256().as_bytes(),
        )
    }
}

/// Transport-neutral result of asking the permanent node owner to prepare one
/// durable submission.
///
/// This vocabulary deliberately stops at native Reticulum packet metadata. It
/// contains no dispatcher queue, RNode, LoRa, radio, or board state, so every
/// interface supervisor can project the same durable lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionPreparationObservation {
    /// Node-core prepared one exact native packet and retained its attempt.
    Prepared(PreparedPacket),
    /// Same-boot pressure prevented preparation without creating an attempt.
    RetrySameBoot,
    /// Node-core rejected the semantic submission without creating an attempt.
    Rejected(SubmitError),
    /// Preparation failed closed after creating an exact quarantined attempt.
    Quarantined(TxRecoveryObservation),
    /// The permanent owner or its coordinator failed before creating an attempt.
    InternalFailure,
}

impl PersistRequest {
    /// Opaque volatile correlation handle for the storage reply.
    pub const fn handle(self) -> PersistHandle {
        self.handle
    }

    /// Exact immutable journal entry to append and apply.
    pub const fn entry(self) -> JournalEntry {
        self.planned.entry()
    }
}

/// Storage actor's disposition for one exact [`PersistRequest`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceReply {
    /// The exact record was durably appended under the backend reservation and
    /// exactly-once/readback protocol.
    Applied,
    /// Exact backend readback or complete replay proved the record was already committed.
    AlreadyEquivalent,
    /// No durable conclusion is available; retry the identical request.
    Retryable,
    /// Durable state contains contradictory content for this operation.
    Conflict,
    /// Storage failed with a bounded project-owned diagnostic code.
    Error(u16),
}

/// Result of processing one correlated storage reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistenceProgress {
    /// The exact request remains pending and must be retried unchanged.
    RetainedForRetry,
    /// The record is now known durable and its effect was applied locally.
    Committed,
}

/// Exact upstream action unlocked by a durable record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcknowledgementKind {
    /// Acknowledge this exact retained node-core terminal tombstone.
    Terminal(TerminalAttempt),
    /// Release this exact recovered owner observation.
    Recovered(TxRecoveryObservation),
}

/// Retained acknowledgement action with volatile exact-reply correlation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcknowledgementAction {
    generation: u64,
    submission: SubmissionId,
    kind: AcknowledgementKind,
}

impl AcknowledgementAction {
    /// Durable submission whose projection unlocked this action.
    pub const fn submission(self) -> SubmissionId {
        self.submission
    }

    /// Exact scalar to pass to the corresponding node/supervisor acknowledgement.
    pub const fn kind(self) -> AcknowledgementKind {
        self.kind
    }
}

/// Caller's result after attempting one exact upstream acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcknowledgementReply {
    /// The upstream owner accepted the exact acknowledgement.
    Completed,
    /// Ordering still prevents acknowledgement; retain and retry unchanged.
    ///
    /// A node-core `PacketStillBound` result maps to this variant.
    Retryable,
    /// Upstream acknowledgement failed with a bounded diagnostic code.
    Error(u16),
}

/// Non-faulting result of offering a lifecycle or transport observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionProgress {
    /// A new exact journal request is ready for persistence.
    Persist(PersistHandle),
    /// The same observation is already pending or durably represented.
    AlreadyObserved,
    /// A volatile attempt was newly correlated after the preparation barrier.
    AttemptBound,
    /// The observation requires no new durable or volatile action yet.
    NoAction,
}

/// Durable decision for a synchronous node preparation rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreparationRejectionDecision {
    /// No attempt was created and the same durable `Preparing` submission may
    /// retry within this boot after local pressure clears.
    RetrySameBoot,
    /// Commit this conservative final failure without requiring a frame.
    Final(SubmissionFailure),
}

/// Classify one node-core preparation error without collapsing retryable
/// same-boot pressure into a terminal result.
const fn classify_preparation_rejection(reason: SubmitError) -> PreparationRejectionDecision {
    match reason {
        SubmitError::AttemptLedgerFull { .. }
        | SubmitError::ReceiptTableFull { .. }
        | SubmitError::ReceiptHashAlreadyTracked => PreparationRejectionDecision::RetrySameBoot,
        SubmitError::UnknownDestination | SubmitError::NoEligibleInterface { .. } => {
            PreparationRejectionDecision::Final(SubmissionFailure::NoPath)
        }
        SubmitError::PayloadTooLarge { .. } => {
            PreparationRejectionDecision::Final(SubmissionFailure::Rejected)
        }
        SubmitError::UnregisteredPacketBuffer
        | SubmitError::ForeignPacketBuffer
        | SubmitError::PacketBufferBusy
        | SubmitError::PacketBufferInvariant
        | SubmitError::AttemptIdentifierExhausted
        | SubmitError::DispatchIdentifierExhausted
        | SubmitError::HopIdentifierExhausted
        | SubmitError::LeaseDeadlineExpired { .. }
        | SubmitError::InterfaceOutsideProfile { .. }
        | SubmitError::RouteReceiptCancellationFailed
        | SubmitError::Cryptography
        | SubmitError::PacketBuild
        | SubmitError::Invariant => PreparationRejectionDecision::Final(
            SubmissionFailure::Internal(InternalFailure::Unspecified),
        ),
    }
}

/// Recoverable inability to accept a new projection operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectorError {
    /// Every fixed correlation slot is occupied.
    CapacityExhausted,
    /// The named submission is not present in volatile projector state.
    UnknownSubmission,
    /// Another write for the same submission must reach a durable conclusion.
    WritePending,
    /// An unlocked exact acknowledgement must complete before more work.
    AcknowledgementPending,
    /// Volatile attempt binding was requested before `Preparing` became durable.
    PreparationBarrierNotDurable,
    /// A no-attempt preparation result arrived after an attempt was already bound.
    PreparationAlreadyBound,
    /// The projector has latched a fail-closed invariant fault.
    Faulted(ProjectorFault),
}

/// Fail-closed invariant or durable-protocol fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectorFault {
    /// A lifecycle operation began from a durable state it cannot safely use.
    UnexpectedDurableState(SubmissionId),
    /// Existing volatile state contradicts a new binding for this submission.
    AttemptBindingConflict(SubmissionId),
    /// An observation names no bound attempt and has no matching token.
    UnknownAttempt,
    /// A known proof token arrived under a different generation-checked handle.
    AttemptGenerationMismatch(SubmissionId),
    /// A known handle arrived with a different proof-correlation token.
    AttemptTokenMismatch(SubmissionId),
    /// Prepared packet length disagrees with the queued attempt metadata.
    PacketLengthMismatch(SubmissionId),
    /// Prepared packet digest disagrees with the queued attempt metadata.
    PacketDigestMismatch(SubmissionId),
    /// A frame observation arrived for a binding without queued packet metadata.
    FrameWithoutQueuedMetadata(SubmissionId),
    /// A repeated/fan-out frame observation changed durable packet metadata.
    PreparedMetadataMismatch(SubmissionId),
    /// A terminal outcome is incompatible with the durable lifecycle state.
    TerminalOutcomeMismatch(SubmissionId),
    /// A second transport audit contradicts the sole durable observation.
    TransportObservationConflict(SubmissionId),
    /// A delivered or timed-out outcome arrived without durable packet metadata.
    TerminalBeforePrepared(SubmissionId),
    /// A frame length cannot be represented by the durable schema.
    PacketLengthOutsideSchema(SubmissionId),
    /// Per-submission revision space is exhausted.
    RevisionExhausted(SubmissionId),
    /// Live storage-model preflight rejected a proposed mutation.
    ModelPlanRejected {
        /// Submission whose mutation was rejected.
        submission: SubmissionId,
        /// Exact semantic validation failure.
        error: ApplyError,
    },
    /// An exact persisted plan could not be applied to the same live index.
    ModelApplyRejected {
        /// Submission whose persisted plan could not apply.
        submission: SubmissionId,
        /// Exact semantic validation failure.
        error: ApplyError,
    },
    /// Volatile operation-generation space is exhausted.
    OperationIdentifierExhausted,
    /// A storage reply did not name the exact currently retained request.
    PersistenceReplyMismatch,
    /// Storage reported contradictory durable content.
    PersistenceConflict(SubmissionId),
    /// Storage failed for a retained request.
    PersistenceFailure {
        /// Submission whose write failed.
        submission: SubmissionId,
        /// Bounded project-owned diagnostic code.
        code: u16,
    },
    /// An acknowledgement reply did not name the exact retained action.
    AcknowledgementReplyMismatch,
    /// Upstream acknowledgement failed rather than reporting retryable ordering.
    AcknowledgementFailure {
        /// Submission whose acknowledgement failed.
        submission: SubmissionId,
        /// Bounded project-owned diagnostic code.
        code: u16,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AttemptBinding {
    handle: AttemptHandle,
    token: AttemptToken,
    expected_packet_len: Option<u16>,
    expected_packet_sha256: Option<[u8; 32]>,
}

impl AttemptBinding {
    const fn from_prepared(prepared: PreparedPacket) -> Self {
        Self {
            handle: prepared.handle(),
            token: prepared.attempt(),
            expected_packet_len: Some(prepared.packet_len()),
            expected_packet_sha256: Some(*prepared.encoded_packet_sha256().as_bytes()),
        }
    }

    const fn from_recovery(observation: TxRecoveryObservation) -> Self {
        Self {
            handle: observation.attempt_handle(),
            token: observation.attempt(),
            expected_packet_len: None,
            expected_packet_sha256: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransportKind {
    Recovered,
    Quarantined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TransportObservation {
    kind: TransportKind,
    observation: TxRecoveryObservation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingRecord {
    Transition {
        request: PersistRequest,
        terminal: Option<TerminalAttempt>,
    },
    Audit {
        request: PersistRequest,
        transport: TransportObservation,
    },
}

impl PendingRecord {
    const fn request(self) -> PersistRequest {
        match self {
            Self::Transition { request, .. } | Self::Audit { request, .. } => request,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StoredAcknowledgement {
    action: AcknowledgementAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SubmissionSlot {
    id: SubmissionId,
    attempt: Option<AttemptBinding>,
    pending: Option<PendingRecord>,
    last_transport: Option<TransportObservation>,
    terminal_acknowledgement: Option<StoredAcknowledgement>,
    recovery_acknowledgement: Option<StoredAcknowledgement>,
    deferred_final: Option<FinalDisposition>,
}

impl SubmissionSlot {
    const fn new(id: SubmissionId) -> Self {
        Self {
            id,
            attempt: None,
            pending: None,
            last_transport: None,
            terminal_acknowledgement: None,
            recovery_acknowledgement: None,
            deferred_final: None,
        }
    }
}

/// Fixed-capacity persist-before-ack lifecycle projector.
///
/// `SUBMISSIONS` bounds only volatile correlations. Durable journal capacity
/// and replay indexing remain separately owned by `reticulum-storage-model`.
/// Each occupied slot can own one complete maximum-size journal mutation, so
/// profile capacities must be selected from target layout and whole-task stack
/// measurements. A future sole-owner runtime may reduce RAM by serializing all
/// submissions through one global pending-write cell; this correctness slice
/// deliberately does not assume that scheduling contract.
/// Final correlations are deliberately retained even after terminal
/// acknowledgement because a recovered owner observation can arrive later.
/// This slice exposes no unsafe automatic retirement: capacity remains bounded
/// until a future sole-owner runtime can provide a proof that terminal,
/// recovered, and quarantine observation sources have all been drained.
pub struct SubmissionProjector<const SUBMISSIONS: usize> {
    slots: [Option<SubmissionSlot>; SUBMISSIONS],
    next_operation_generation: Option<u64>,
    fault: Option<ProjectorFault>,
}

impl<const SUBMISSIONS: usize> SubmissionProjector<SUBMISSIONS> {
    /// Construct an empty volatile projector.
    pub fn new() -> Self {
        Self {
            slots: array::from_fn(|_| None),
            next_operation_generation: Some(1),
            fault: None,
        }
    }

    /// Initialize an empty projector directly in uninitialized caller storage.
    ///
    /// Every fixed-capacity slot is initialized at its final address, so stack
    /// use does not grow with `SUBMISSIONS`. The destination is fully
    /// initialized when the returned mutable reference is exposed.
    ///
    /// The caller must supply genuinely uninitialized storage. Reusing storage
    /// containing a live projector would overwrite it without dropping its
    /// fields.
    #[allow(
        unsafe_code,
        reason = "audited field-wise initialization avoids a capacity-sized projector stack temporary"
    )]
    pub fn initialize_in(
        destination: &mut MaybeUninit<Self>,
    ) -> &mut SubmissionProjector<SUBMISSIONS> {
        const {
            assert!(
                !core::mem::needs_drop::<SubmissionProjector<SUBMISSIONS>>(),
                "field-wise projector placement requires a no-drop projector"
            );
        }
        let projector = destination.as_mut_ptr();
        // SAFETY: `projector` is aligned and uniquely borrowed from an
        // uninitialized `SubmissionProjector`. The array pointer has the
        // address and alignment of its first element; each in-bounds slot is
        // written exactly once, followed by both scalar fields. Only then is a
        // mutable reference to the complete projector created.
        unsafe {
            let slots =
                core::ptr::addr_of_mut!((*projector).slots).cast::<Option<SubmissionSlot>>();
            for slot in 0..SUBMISSIONS {
                slots.add(slot).write(None);
            }
            core::ptr::addr_of_mut!((*projector).next_operation_generation).write(Some(1));
            core::ptr::addr_of_mut!((*projector).fault).write(None);
            &mut *projector
        }
    }

    /// Current fail-closed fault, if one has been latched.
    pub const fn fault(&self) -> Option<ProjectorFault> {
        self.fault
    }

    /// Number of volatile submission correlations currently retained.
    pub fn retained_submissions(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Iterate exact journal requests in stable projector-slot order.
    pub fn pending_persistence(&self) -> impl Iterator<Item = PersistRequest> + '_ {
        self.slots
            .iter()
            .flatten()
            .filter_map(|slot| slot.pending.map(PendingRecord::request))
    }

    /// Copy the exact retained request named by one volatile handle.
    pub fn persistence_request(&self, handle: PersistHandle) -> Option<PersistRequest> {
        self.pending_persistence()
            .find(|request| request.handle() == handle)
    }

    /// Iterate exact unlocked acknowledgements in stable projector-slot order.
    pub fn pending_acknowledgements(&self) -> impl Iterator<Item = AcknowledgementAction> + '_ {
        self.slots.iter().flatten().flat_map(|slot| {
            [
                slot.recovery_acknowledgement.map(|ack| ack.action),
                slot.terminal_acknowledgement.map(|ack| ack.action),
            ]
            .into_iter()
            .flatten()
        })
    }

    /// Authoritative durable lifecycle state from the supplied live index.
    pub fn durable_state<const INDEXED: usize>(
        &self,
        index: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
    ) -> Option<LifecycleState> {
        self.slot_for_id(id)
            .and_then(|_| index.get(id).map(|item| item.state()))
    }

    /// Authoritative durable revision from the supplied live index.
    pub fn durable_revision<const INDEXED: usize>(
        &self,
        index: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
    ) -> Option<u64> {
        self.slot_for_id(id)
            .and_then(|_| index.get(id).map(|item| item.revision()))
    }

    /// Whether the `Queued -> Preparing` barrier is known durable and a node
    /// preparation attempt may now begin.
    pub fn preparation_allowed<const INDEXED: usize>(
        &self,
        index: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
    ) -> bool {
        self.fault.is_none()
            && self.slot_for_id(id).is_some_and(|slot_index| {
                let slot = self.slots[slot_index].unwrap();
                index
                    .get(id)
                    .is_some_and(|item| item.state() == LifecycleState::Preparing)
                    && slot.pending.is_none()
                    && slot.attempt.is_none()
            })
    }

    /// Plan the required no-replay barrier from one durable queued submission.
    ///
    /// Node preparation must not begin until the returned request receives an
    /// `Applied` or `AlreadyEquivalent` reply and [`Self::preparation_allowed`]
    /// becomes true.
    pub fn begin_preparation<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        let indexed = live.get(id).ok_or(ProjectorError::UnknownSubmission)?;
        if let Some(slot_index) = self.slot_for_id(id) {
            let current = self.slots[slot_index].unwrap();
            if let Some(pending) = current.pending {
                return Ok(ProjectionProgress::Persist(pending.request().handle()));
            }
            return if indexed.state() == LifecycleState::Preparing {
                Ok(ProjectionProgress::AlreadyObserved)
            } else {
                Err(self.latch(ProjectorFault::UnexpectedDurableState(id)))
            };
        }
        let Some(slot_index) = self.slots.iter().position(Option::is_none) else {
            return Err(ProjectorError::CapacityExhausted);
        };
        let mut slot = SubmissionSlot::new(id);
        if indexed.state() != LifecycleState::Queued {
            return Err(self.latch(ProjectorFault::UnexpectedDurableState(id)));
        }
        let transition = self.transition(id, indexed.revision(), LifecycleState::Preparing)?;
        let request = self.plan_transition_request(live, transition)?;
        slot.pending = Some(PendingRecord::Transition {
            request,
            terminal: None,
        });
        self.slots[slot_index] = Some(slot);
        Ok(ProjectionProgress::Persist(request.handle()))
    }

    /// Project one transport-neutral permanent-node preparation observation.
    ///
    /// A prepared packet binds only its generation-safe attempt handle, proof
    /// token, encoded length and complete-byte digest. Same-boot pressure keeps
    /// the durable `Preparing` barrier active. Semantic rejection or owner
    /// failure becomes an exact durable final transition, while quarantine is
    /// correlated before its conservative transport audit is planned.
    pub fn observe_preparation<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
        observation: SubmissionPreparationObservation,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        match observation {
            SubmissionPreparationObservation::Prepared(prepared) => {
                self.bind_attempt(live, id, AttemptBinding::from_prepared(prepared))
            }
            SubmissionPreparationObservation::RetrySameBoot => {
                self.ensure_unbound_preparation_context(live, id)?;
                Ok(ProjectionProgress::NoAction)
            }
            SubmissionPreparationObservation::Rejected(reason) => {
                self.observe_preparation_rejected(live, id, reason)
            }
            SubmissionPreparationObservation::Quarantined(observation) => {
                self.bind_attempt(live, id, AttemptBinding::from_recovery(observation))?;
                self.observe_quarantined(live, observation)
            }
            SubmissionPreparationObservation::InternalFailure => {
                self.ensure_unbound_preparation_context(live, id)?;
                self.plan_final(
                    live,
                    id,
                    FinalDisposition::Failed(SubmissionFailure::Internal(
                        InternalFailure::Unspecified,
                    )),
                    None,
                )
            }
        }
    }

    /// Map a node-core rejection reason to a final disposition without
    /// requiring any prepared frame observation.
    fn observe_preparation_rejected<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
        reason: SubmitError,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        self.ensure_unbound_preparation_context(live, id)?;
        match classify_preparation_rejection(reason) {
            PreparationRejectionDecision::RetrySameBoot => Ok(ProjectionProgress::NoAction),
            PreparationRejectionDecision::Final(failure) => {
                self.plan_final(live, id, FinalDisposition::Failed(failure), None)
            }
        }
    }

    /// Project complete encoded packet metadata after the RF sink (or inert
    /// inspector) borrows the exact authorized frame.
    ///
    /// Repeated or fan-out observations are idempotent only when packet length,
    /// full-frame SHA-256, and RNS proof token all match exactly.
    pub fn observe_frame<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        observation: PreparedFrameObservation,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        let slot_index =
            self.correlated_slot(observation.attempt_handle(), observation.attempt())?;
        let id = self.slots[slot_index].unwrap().id;
        let indexed = live
            .get(id)
            .ok_or_else(|| self.latch(ProjectorFault::UnexpectedDurableState(id)))?;
        let packet_len = match u16::try_from(observation.packet_len()) {
            Ok(packet_len) => packet_len,
            Err(_) => {
                return Err(self.latch(ProjectorFault::PacketLengthOutsideSchema(id)));
            }
        };
        let binding = self.slots[slot_index].unwrap().attempt.unwrap();
        let (Some(expected_packet_len), Some(expected_packet_sha256)) =
            (binding.expected_packet_len, binding.expected_packet_sha256)
        else {
            return Err(self.latch(ProjectorFault::FrameWithoutQueuedMetadata(id)));
        };
        if expected_packet_len != packet_len {
            return Err(self.latch(ProjectorFault::PacketLengthMismatch(id)));
        }
        if expected_packet_sha256 != observation.encoded_packet_sha256() {
            return Err(self.latch(ProjectorFault::PacketDigestMismatch(id)));
        }
        let details = PreparedPacketDetails::new(
            expected_packet_len,
            EncodedPacketSha256::new(expected_packet_sha256),
            RnsAttemptToken::new(*binding.token.as_bytes()),
        )
        .map_err(|_| self.latch(ProjectorFault::PacketLengthOutsideSchema(id)))?;
        let current = self.slots[slot_index].unwrap();
        if let Some(pending) = current.pending {
            if pending_prepared_details(pending) == Some(details) {
                return Ok(ProjectionProgress::AlreadyObserved);
            }
            if pending_targets_awaiting(pending) {
                return Err(self.latch(ProjectorFault::PreparedMetadataMismatch(id)));
            }
            return Err(ProjectorError::WritePending);
        }
        match indexed.state() {
            LifecycleState::Preparing => {
                if current.terminal_acknowledgement.is_some()
                    || current.recovery_acknowledgement.is_some()
                {
                    return Err(ProjectorError::AcknowledgementPending);
                }
                let transition = self.transition(
                    id,
                    indexed.revision(),
                    LifecycleState::AwaitingDelivery(details),
                )?;
                let request = self.plan_transition_request(live, transition)?;
                self.slots[slot_index].as_mut().unwrap().pending =
                    Some(PendingRecord::Transition {
                        request,
                        terminal: None,
                    });
                Ok(ProjectionProgress::Persist(request.handle()))
            }
            LifecycleState::AwaitingDelivery(existing) if existing == details => {
                Ok(ProjectionProgress::AlreadyObserved)
            }
            LifecycleState::Final(FinalDisposition::Delivered(existing)) if existing == details => {
                Ok(ProjectionProgress::AlreadyObserved)
            }
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout)) => {
                Ok(ProjectionProgress::AlreadyObserved)
            }
            LifecycleState::AwaitingDelivery(_)
            | LifecycleState::Final(FinalDisposition::Delivered(_)) => {
                Err(self.latch(ProjectorFault::PreparedMetadataMismatch(id)))
            }
            LifecycleState::Queued | LifecycleState::Final(_) => {
                Err(self.latch(ProjectorFault::UnexpectedDurableState(id)))
            }
        }
    }

    /// Project one exact node-core terminal tombstone to a final disposition.
    ///
    /// The returned terminal acknowledgement is not exposed until the final
    /// record is known durable. If the same final record was committed earlier
    /// (for example after synchronous rejection), the exact compatible terminal
    /// is unlocked without writing a second final transition.
    pub fn observe_terminal<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        terminal: TerminalAttempt,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        let index = self.correlated_slot(terminal.handle(), terminal.token())?;
        let current = self.slots[index].unwrap();
        let id = current.id;
        let indexed = live
            .get(id)
            .ok_or_else(|| self.latch(ProjectorFault::UnexpectedDurableState(id)))?;
        if let Some(ack) = current.terminal_acknowledgement {
            return if ack.action.kind == AcknowledgementKind::Terminal(terminal) {
                Ok(ProjectionProgress::AlreadyObserved)
            } else {
                Err(ProjectorError::AcknowledgementPending)
            };
        }
        if let Some(pending) = current.pending {
            if pending_terminal(pending) == Some(terminal) {
                return Ok(ProjectionProgress::AlreadyObserved);
            }
            return Err(ProjectorError::WritePending);
        }
        if matches!(
            indexed.state(),
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(_)))
        ) && indexed.transport_audit().is_some()
        {
            // A durable transport audit plus conservative internal final is
            // authoritative for a quarantined/recovered owner. A receipt can
            // still tombstone later, but rewriting the immutable final would
            // contradict the fail-closed result. The exact correlated
            // terminal is safe to release only after both durable records.
            let action = self.new_acknowledgement(id, AcknowledgementKind::Terminal(terminal))?;
            self.slots[index].as_mut().unwrap().terminal_acknowledgement =
                Some(StoredAcknowledgement { action });
            return Ok(ProjectionProgress::AlreadyObserved);
        }
        let preframe_details = if indexed.state() == LifecycleState::Preparing
            && terminal.outcome() == AttemptOutcome::Delivered
        {
            Some(self.prepared_details_from_binding(id, current.attempt.unwrap())?)
        } else {
            None
        };
        let disposition =
            terminal_disposition(indexed.state(), terminal.outcome(), preframe_details)
                .ok_or_else(|| match terminal.outcome() {
                    AttemptOutcome::Delivered | AttemptOutcome::DeliveryTimeout => {
                        self.latch(ProjectorFault::TerminalBeforePrepared(id))
                    }
                    AttemptOutcome::Unsent(_) => {
                        self.latch(ProjectorFault::TerminalOutcomeMismatch(id))
                    }
                })?;
        if let LifecycleState::Final(existing) = indexed.state() {
            if existing != disposition {
                return Err(self.latch(ProjectorFault::TerminalOutcomeMismatch(id)));
            }
            let action = self.new_acknowledgement(id, AcknowledgementKind::Terminal(terminal))?;
            self.slots[index].as_mut().unwrap().terminal_acknowledgement =
                Some(StoredAcknowledgement { action });
            return Ok(ProjectionProgress::AlreadyObserved);
        }
        self.plan_final(live, id, disposition, Some(terminal))
    }

    /// Project one exact recovered-owner observation to a transport audit.
    ///
    /// The exact observation is retained as an acknowledgement action only
    /// after the audit record is durable.
    pub fn observe_recovered<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        observation: TxRecoveryObservation,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.observe_transport(live, TransportKind::Recovered, observation)
    }

    /// Project one exact fail-closed quarantine observation to a transport
    /// audit. Quarantine has no release acknowledgement action.
    pub fn observe_quarantined<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        observation: TxRecoveryObservation,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        let index = self.correlated_slot(observation.attempt_handle(), observation.attempt())?;
        let id = self.slots[index].unwrap().id;
        let indexed = live
            .get(id)
            .ok_or_else(|| self.latch(ProjectorFault::UnexpectedDurableState(id)))?;
        let requires_final = !indexed.state().is_final();
        let transport = TransportObservation {
            kind: TransportKind::Quarantined,
            observation,
        };
        let durable_event = transport_event(transport);
        let progress = self.observe_transport(live, transport.kind, observation)?;
        if !requires_final {
            return Ok(progress);
        }

        let current = self.slots[index].unwrap();
        if current.pending.and_then(pending_transport) == Some(transport) {
            self.slots[index].as_mut().unwrap().deferred_final = Some(FinalDisposition::Failed(
                SubmissionFailure::Internal(InternalFailure::Unspecified),
            ));
            return Ok(progress);
        }
        if indexed.transport_audit() == Some(durable_event) && current.pending.is_none() {
            return self.plan_final(
                live,
                id,
                FinalDisposition::Failed(SubmissionFailure::Internal(InternalFailure::Unspecified)),
                None,
            );
        }
        Ok(progress)
    }

    /// Apply one exact storage result to its retained request.
    ///
    /// `Applied` is valid only after the backend protocol described on
    /// [`PersistRequest`] has completed. `AlreadyEquivalent` requires exact
    /// readback or replay. Both consume the retained opaque plan through the
    /// same live index before any acknowledgement becomes visible.
    pub fn report_persistence<const INDEXED: usize>(
        &mut self,
        live: &mut SubmissionIndex<INDEXED>,
        request: PersistRequest,
        reply: PersistenceReply,
    ) -> Result<PersistenceProgress, ProjectorError> {
        self.ensure_healthy()?;
        let Some(index) = self.slots.iter().position(|slot| {
            slot.and_then(|slot| slot.pending)
                .is_some_and(|pending| pending.request().handle == request.handle)
        }) else {
            return Err(self.latch(ProjectorFault::PersistenceReplyMismatch));
        };
        let slot = self.slots[index].unwrap();
        let pending = slot.pending.unwrap();
        if pending.request() != request {
            return Err(self.latch(ProjectorFault::PersistenceReplyMismatch));
        }
        match reply {
            PersistenceReply::Retryable => Ok(PersistenceProgress::RetainedForRetry),
            PersistenceReply::Conflict => {
                Err(self.latch(ProjectorFault::PersistenceConflict(slot.id)))
            }
            PersistenceReply::Error(code) => Err(self.latch(ProjectorFault::PersistenceFailure {
                submission: slot.id,
                code,
            })),
            PersistenceReply::Applied | PersistenceReply::AlreadyEquivalent => {
                live.apply_planned(request.planned).map_err(|error| {
                    self.latch(ProjectorFault::ModelApplyRejected {
                        submission: slot.id,
                        error,
                    })
                })?;
                let continue_final = matches!(
                    pending,
                    PendingRecord::Audit {
                        transport: TransportObservation {
                            kind: TransportKind::Quarantined,
                            ..
                        },
                        ..
                    }
                ) && slot.deferred_final.is_some();
                self.commit_pending(index, pending)?;
                if continue_final {
                    let disposition = self.slots[index]
                        .as_mut()
                        .unwrap()
                        .deferred_final
                        .take()
                        .ok_or_else(|| {
                            self.latch(ProjectorFault::UnexpectedDurableState(slot.id))
                        })?;
                    self.plan_final(live, slot.id, disposition, None)?;
                }
                Ok(PersistenceProgress::Committed)
            }
        }
    }

    /// Report the result of one exact retained upstream acknowledgement.
    pub fn report_acknowledgement(
        &mut self,
        action: AcknowledgementAction,
        reply: AcknowledgementReply,
    ) -> Result<(), ProjectorError> {
        self.ensure_healthy()?;
        let Some(index) = self.slots.iter().position(|slot| {
            slot.is_some_and(|slot| {
                slot.terminal_acknowledgement
                    .is_some_and(|stored| stored.action.generation == action.generation)
                    || slot
                        .recovery_acknowledgement
                        .is_some_and(|stored| stored.action.generation == action.generation)
            })
        }) else {
            return Err(self.latch(ProjectorFault::AcknowledgementReplyMismatch));
        };
        let slot = self.slots[index].unwrap();
        let stored = slot
            .terminal_acknowledgement
            .filter(|stored| stored.action.generation == action.generation)
            .or_else(|| {
                slot.recovery_acknowledgement
                    .filter(|stored| stored.action.generation == action.generation)
            })
            .ok_or_else(|| self.latch(ProjectorFault::AcknowledgementReplyMismatch))?;
        if stored.action != action {
            return Err(self.latch(ProjectorFault::AcknowledgementReplyMismatch));
        }
        match reply {
            AcknowledgementReply::Retryable => Ok(()),
            AcknowledgementReply::Error(code) => {
                Err(self.latch(ProjectorFault::AcknowledgementFailure {
                    submission: slot.id,
                    code,
                }))
            }
            AcknowledgementReply::Completed => {
                match action.kind {
                    AcknowledgementKind::Terminal(_) => {
                        self.slots[index].as_mut().unwrap().terminal_acknowledgement = None;
                    }
                    AcknowledgementKind::Recovered(_) => {
                        self.slots[index].as_mut().unwrap().recovery_acknowledgement = None;
                    }
                }
                Ok(())
            }
        }
    }

    fn observe_transport<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        kind: TransportKind,
        observation: TxRecoveryObservation,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        let index = self.correlated_slot(observation.attempt_handle(), observation.attempt())?;
        let current = self.slots[index].unwrap();
        let id = current.id;
        let indexed = live
            .get(id)
            .ok_or_else(|| self.latch(ProjectorFault::UnexpectedDurableState(id)))?;
        let transport = TransportObservation { kind, observation };
        let event = transport_event(transport);
        if current
            .last_transport
            .is_some_and(|existing| existing != transport)
        {
            return Err(self.latch(ProjectorFault::TransportObservationConflict(id)));
        }
        match indexed.transport_audit() {
            Some(existing) if existing == event => {
                return Ok(ProjectionProgress::AlreadyObserved);
            }
            Some(_) => {
                return Err(self.latch(ProjectorFault::TransportObservationConflict(id)));
            }
            None => {}
        }
        if let Some(pending) = current.pending {
            if pending_transport(pending) == Some(transport) {
                return Ok(ProjectionProgress::AlreadyObserved);
            }
            return Err(ProjectorError::WritePending);
        }
        if let Some(ack) = current.recovery_acknowledgement {
            return if ack.action.kind == AcknowledgementKind::Recovered(observation)
                && kind == TransportKind::Recovered
            {
                Ok(ProjectionProgress::AlreadyObserved)
            } else {
                Err(ProjectorError::AcknowledgementPending)
            };
        }
        let revision = self.next_revision(id, indexed.revision())?;
        let audit = AuditEntry::new(id, revision, event)
            .ok_or_else(|| self.latch(ProjectorFault::RevisionExhausted(id)))?;
        let request = self.plan_audit_request(live, audit)?;
        self.slots[index].as_mut().unwrap().pending =
            Some(PendingRecord::Audit { request, transport });
        Ok(ProjectionProgress::Persist(request.handle()))
    }

    fn plan_final<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
        disposition: FinalDisposition,
        terminal: Option<TerminalAttempt>,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        let index = self
            .slot_for_id(id)
            .ok_or(ProjectorError::UnknownSubmission)?;
        let current = self.slots[index].unwrap();
        let indexed = live
            .get(id)
            .ok_or_else(|| self.latch(ProjectorFault::UnexpectedDurableState(id)))?;
        if current.pending.is_some() {
            return Err(ProjectorError::WritePending);
        }
        if current.terminal_acknowledgement.is_some() {
            return Err(ProjectorError::AcknowledgementPending);
        }
        if matches!(
            indexed.state(),
            LifecycleState::Queued | LifecycleState::Final(_)
        ) {
            return Err(self.latch(ProjectorFault::UnexpectedDurableState(id)));
        }
        let transition =
            self.transition(id, indexed.revision(), LifecycleState::Final(disposition))?;
        let request = self.plan_transition_request(live, transition)?;
        self.slots[index].as_mut().unwrap().pending =
            Some(PendingRecord::Transition { request, terminal });
        Ok(ProjectionProgress::Persist(request.handle()))
    }

    fn bind_attempt<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
        binding: AttemptBinding,
    ) -> Result<ProjectionProgress, ProjectorError> {
        self.ensure_healthy()?;
        let index = self
            .slot_for_id(id)
            .ok_or(ProjectorError::UnknownSubmission)?;
        let current = self.slots[index].unwrap();
        if live.get(id).map(|item| item.state()) != Some(LifecycleState::Preparing)
            || current.pending.is_some()
        {
            return Err(ProjectorError::PreparationBarrierNotDurable);
        }
        if current.terminal_acknowledgement.is_some() || current.recovery_acknowledgement.is_some()
        {
            return Err(ProjectorError::AcknowledgementPending);
        }
        if self
            .slots
            .iter()
            .enumerate()
            .filter(|(other_index, _)| *other_index != index)
            .filter_map(|(_, slot)| slot.and_then(|slot| slot.attempt))
            .any(|attempt| attempt.handle == binding.handle)
        {
            return Err(self.latch(ProjectorFault::AttemptBindingConflict(id)));
        }
        match current.attempt {
            None => {
                self.slots[index].as_mut().unwrap().attempt = Some(binding);
                Ok(ProjectionProgress::AttemptBound)
            }
            Some(existing) if existing == binding => Ok(ProjectionProgress::AlreadyObserved),
            Some(existing)
                if existing.token == binding.token && existing.handle != binding.handle =>
            {
                Err(self.latch(ProjectorFault::AttemptGenerationMismatch(id)))
            }
            Some(_) => Err(self.latch(ProjectorFault::AttemptBindingConflict(id))),
        }
    }

    fn ensure_unbound_preparation_context<const INDEXED: usize>(
        &self,
        live: &SubmissionIndex<INDEXED>,
        id: SubmissionId,
    ) -> Result<(), ProjectorError> {
        self.ensure_healthy()?;
        let index = self
            .slot_for_id(id)
            .ok_or(ProjectorError::UnknownSubmission)?;
        let slot = self.slots[index].unwrap();
        if live.get(id).map(|item| item.state()) != Some(LifecycleState::Preparing)
            || slot.pending.is_some()
        {
            return Err(ProjectorError::PreparationBarrierNotDurable);
        }
        if slot.attempt.is_some() {
            return Err(ProjectorError::PreparationAlreadyBound);
        }
        Ok(())
    }

    fn prepared_details_from_binding(
        &mut self,
        id: SubmissionId,
        binding: AttemptBinding,
    ) -> Result<PreparedPacketDetails, ProjectorError> {
        let (Some(packet_len), Some(encoded_packet_sha256)) =
            (binding.expected_packet_len, binding.expected_packet_sha256)
        else {
            return Err(self.latch(ProjectorFault::TerminalBeforePrepared(id)));
        };
        PreparedPacketDetails::new(
            packet_len,
            EncodedPacketSha256::new(encoded_packet_sha256),
            RnsAttemptToken::new(*binding.token.as_bytes()),
        )
        .map_err(|_| self.latch(ProjectorFault::PacketLengthOutsideSchema(id)))
    }

    fn correlated_slot(
        &mut self,
        handle: AttemptHandle,
        token: AttemptToken,
    ) -> Result<usize, ProjectorError> {
        if let Some(index) = self.slots.iter().position(|slot| {
            slot.and_then(|slot| slot.attempt)
                .is_some_and(|attempt| attempt.handle == handle)
        }) {
            let slot = self.slots[index].unwrap();
            if slot.attempt.unwrap().token == token {
                return Ok(index);
            }
            return Err(self.latch(ProjectorFault::AttemptTokenMismatch(slot.id)));
        }
        if let Some(slot) = self.slots.iter().flatten().find(|slot| {
            slot.attempt
                .is_some_and(|attempt| attempt.token == token && attempt.handle != handle)
        }) {
            return Err(self.latch(ProjectorFault::AttemptGenerationMismatch(slot.id)));
        }
        Err(self.latch(ProjectorFault::UnknownAttempt))
    }

    fn transition(
        &mut self,
        id: SubmissionId,
        revision: u64,
        state: LifecycleState,
    ) -> Result<StateTransition, ProjectorError> {
        let next = self.next_revision(id, revision)?;
        StateTransition::new(id, next, state)
            .map_err(|_| self.latch(ProjectorFault::UnexpectedDurableState(id)))
    }

    fn next_revision(&mut self, id: SubmissionId, revision: u64) -> Result<u64, ProjectorError> {
        revision
            .checked_add(1)
            .ok_or_else(|| self.latch(ProjectorFault::RevisionExhausted(id)))
    }

    fn new_request(&mut self, planned: PlannedMutation) -> Result<PersistRequest, ProjectorError> {
        let generation = self.next_operation()?;
        Ok(PersistRequest {
            handle: PersistHandle(generation),
            planned,
        })
    }

    fn plan_transition_request<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        transition: StateTransition,
    ) -> Result<PersistRequest, ProjectorError> {
        match live.plan_transition(transition) {
            Ok(PlanOutcome::Append(planned)) => self.new_request(planned),
            Ok(PlanOutcome::AlreadyEquivalent) => {
                Err(self.latch(ProjectorFault::UnexpectedDurableState(transition.id())))
            }
            Err(error) => Err(self.latch(ProjectorFault::ModelPlanRejected {
                submission: transition.id(),
                error,
            })),
        }
    }

    fn plan_audit_request<const INDEXED: usize>(
        &mut self,
        live: &SubmissionIndex<INDEXED>,
        audit: AuditEntry,
    ) -> Result<PersistRequest, ProjectorError> {
        match live.plan_audit(audit) {
            Ok(PlanOutcome::Append(planned)) => self.new_request(planned),
            Ok(PlanOutcome::AlreadyEquivalent) => {
                Err(self.latch(ProjectorFault::UnexpectedDurableState(audit.id())))
            }
            Err(error) => Err(self.latch(ProjectorFault::ModelPlanRejected {
                submission: audit.id(),
                error,
            })),
        }
    }

    fn new_acknowledgement(
        &mut self,
        submission: SubmissionId,
        kind: AcknowledgementKind,
    ) -> Result<AcknowledgementAction, ProjectorError> {
        Ok(AcknowledgementAction {
            generation: self.next_operation()?,
            submission,
            kind,
        })
    }

    fn next_operation(&mut self) -> Result<u64, ProjectorError> {
        let Some(current) = self.next_operation_generation else {
            return Err(self.latch(ProjectorFault::OperationIdentifierExhausted));
        };
        self.next_operation_generation = current.checked_add(1);
        Ok(current)
    }

    fn commit_pending(
        &mut self,
        index: usize,
        pending: PendingRecord,
    ) -> Result<(), ProjectorError> {
        let mut slot = self.slots[index].unwrap();
        match pending {
            PendingRecord::Transition { request, terminal } => {
                let JournalEntry::StateTransition(transition) = request.entry() else {
                    return Err(self.latch(ProjectorFault::PersistenceReplyMismatch));
                };
                slot.pending = None;
                if transition.state().is_final() {
                    slot.deferred_final = None;
                }
                if let Some(terminal) = terminal {
                    let action =
                        self.new_acknowledgement(slot.id, AcknowledgementKind::Terminal(terminal))?;
                    slot.terminal_acknowledgement = Some(StoredAcknowledgement { action });
                }
                self.slots[index] = Some(slot);
            }
            PendingRecord::Audit { request, transport } => {
                let JournalEntry::Audit(_) = request.entry() else {
                    return Err(self.latch(ProjectorFault::PersistenceReplyMismatch));
                };
                slot.pending = None;
                slot.last_transport = Some(transport);
                if transport.kind == TransportKind::Recovered {
                    let action = self.new_acknowledgement(
                        slot.id,
                        AcknowledgementKind::Recovered(transport.observation),
                    )?;
                    slot.recovery_acknowledgement = Some(StoredAcknowledgement { action });
                }
                self.slots[index] = Some(slot);
            }
        }
        Ok(())
    }

    fn slot_for_id(&self, id: SubmissionId) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.is_some_and(|slot| slot.id == id))
    }

    fn ensure_healthy(&self) -> Result<(), ProjectorError> {
        match self.fault {
            Some(fault) => Err(ProjectorError::Faulted(fault)),
            None => Ok(()),
        }
    }

    fn latch(&mut self, fault: ProjectorFault) -> ProjectorError {
        self.fault = Some(fault);
        ProjectorError::Faulted(fault)
    }
}

impl<const SUBMISSIONS: usize> Default for SubmissionProjector<SUBMISSIONS> {
    fn default() -> Self {
        Self::new()
    }
}

fn pending_prepared_details(pending: PendingRecord) -> Option<PreparedPacketDetails> {
    match pending {
        PendingRecord::Transition { request, .. } => match request.entry() {
            JournalEntry::StateTransition(transition) => match transition.state() {
                LifecycleState::AwaitingDelivery(details) => Some(details),
                LifecycleState::Queued | LifecycleState::Preparing | LifecycleState::Final(_) => {
                    None
                }
            },
            JournalEntry::Accepted(_) | JournalEntry::Audit(_) => None,
        },
        PendingRecord::Audit { .. } => None,
    }
}

fn pending_targets_awaiting(pending: PendingRecord) -> bool {
    pending_prepared_details(pending).is_some()
}

fn pending_terminal(pending: PendingRecord) -> Option<TerminalAttempt> {
    match pending {
        PendingRecord::Transition { terminal, .. } => terminal,
        PendingRecord::Audit { .. } => None,
    }
}

fn pending_transport(pending: PendingRecord) -> Option<TransportObservation> {
    match pending {
        PendingRecord::Audit { transport, .. } => Some(transport),
        PendingRecord::Transition { .. } => None,
    }
}

fn transport_event(transport: TransportObservation) -> AuditEvent {
    let observation = transport.observation;
    let rns_attempt_token = RnsAttemptToken::new(*observation.attempt().as_bytes());
    match transport.kind {
        TransportKind::Recovered => AuditEvent::TransportRecovered {
            rns_attempt_token,
            may_have_transmitted: observation.record().may_have_transmitted(),
            reason: recovery_reason(observation.record().reason()),
        },
        TransportKind::Quarantined => AuditEvent::TransportQuarantined {
            rns_attempt_token,
            may_have_transmitted: observation.record().may_have_transmitted(),
            reason: recovery_reason(observation.record().reason()),
        },
    }
}

fn terminal_disposition(
    state: LifecycleState,
    outcome: AttemptOutcome,
    preframe_details: Option<PreparedPacketDetails>,
) -> Option<FinalDisposition> {
    match outcome {
        AttemptOutcome::Delivered => match state {
            LifecycleState::AwaitingDelivery(details)
            | LifecycleState::Final(FinalDisposition::Delivered(details)) => {
                Some(FinalDisposition::Delivered(details))
            }
            LifecycleState::Preparing => preframe_details.map(FinalDisposition::Delivered),
            LifecycleState::Queued | LifecycleState::Final(_) => None,
        },
        AttemptOutcome::DeliveryTimeout => match state {
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout)) => {
                Some(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
            }
            LifecycleState::Preparing | LifecycleState::AwaitingDelivery(_) => {
                Some(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout))
            }
            LifecycleState::Queued | LifecycleState::Final(_) => None,
        },
        AttemptOutcome::Unsent(reason) => {
            let failure = match reason {
                AttemptUnsentReason::PolicyDenied(_)
                | AttemptUnsentReason::PermitDeadlineExpired
                | AttemptUnsentReason::Unpermitted(_) => SubmissionFailure::Rejected,
                AttemptUnsentReason::QueueRollback | AttemptUnsentReason::RecoveryRequired => {
                    SubmissionFailure::Internal(InternalFailure::Unspecified)
                }
            };
            match state {
                LifecycleState::Preparing => Some(FinalDisposition::Failed(failure)),
                LifecycleState::AwaitingDelivery(_)
                    if matches!(failure, SubmissionFailure::Internal(_)) =>
                {
                    Some(FinalDisposition::Failed(failure))
                }
                LifecycleState::Final(FinalDisposition::Failed(existing))
                    if existing == failure =>
                {
                    Some(FinalDisposition::Failed(failure))
                }
                LifecycleState::Queued
                | LifecycleState::AwaitingDelivery(_)
                | LifecycleState::Final(_) => None,
            }
        }
    }
}

fn recovery_reason(reason: TxRecoveryReason) -> TransportRecoveryReason {
    match reason {
        TxRecoveryReason::DeadlineExpired => TransportRecoveryReason::DeadlineExpired,
        TxRecoveryReason::CompletionFault(code) => {
            TransportRecoveryReason::CompletionFault(code.get())
        }
        TxRecoveryReason::ReceiptCancellationFailed => {
            TransportRecoveryReason::ReceiptCancellationFailed
        }
        TxRecoveryReason::HopIdentifierExhausted => TransportRecoveryReason::HopIdentifierExhausted,
        TxRecoveryReason::Invariant => TransportRecoveryReason::Invariant,
    }
}

#[cfg(test)]
mod tests;
