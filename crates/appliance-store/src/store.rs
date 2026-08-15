use core::fmt;

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
use std::vec::Vec;

use crate::{
    AcceptanceIds, AttemptLocationStamp, Contact, ConversationPeer, DestinationHash,
    InboundMessage, InboundRecord, MessageActivityPage, MessageActivityPageRequest, MessageId,
    OutboxId, OutboxMaterial, OutboxRecord, OutboxStatus, ReconcileWork, RfTraceBootId,
    RfTraceEventSequence, RfTraceImportBatch, RfTracePage, RfTracePageRequest, RnsAttemptToken,
    SubmissionId, SubmissionState, TimelineEntry,
};

/// Result of binding an unbound database to one authenticated device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceBindingOutcome {
    /// The database was previously unbound and now retains the supplied identity.
    Bound,
    /// The database already retained the exact supplied identity.
    Unchanged,
}

/// Result of inserting or updating a contact keyed by destination hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContactUpsertOutcome {
    /// A new destination was inserted.
    Inserted,
    /// Existing user-facing metadata changed.
    Updated,
    /// The exact contact was already present.
    Unchanged,
}

/// Result of committing one inbound message under message-ID deduplication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboundCommitOutcome {
    /// A new authenticated message was inserted.
    Inserted,
    /// The exact authenticated message was already present.
    Duplicate,
}

/// Result of commit-before-send outbox admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxCommitOutcome {
    /// A new durable outbox row was inserted.
    Inserted(OutboxId),
    /// The exact material and idempotency key were already committed.
    Existing(OutboxId),
}

/// Result of creating a replacement device submission for an existing row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxRetryOutcome {
    /// A terminal device submission was cleared and the same row was requeued.
    Requeued(OutboxId),
    /// The row already has unfinished work, so no replacement was added.
    AlreadyPending(OutboxId),
}

impl OutboxRetryOutcome {
    /// Stable outbox identifier retained by either outcome.
    pub const fn outbox_id(self) -> OutboxId {
        match self {
            Self::Requeued(id) | Self::AlreadyPending(id) => id,
        }
    }
}

impl OutboxCommitOutcome {
    /// Stable outbox identifier for either idempotent outcome.
    pub const fn outbox_id(self) -> OutboxId {
        match self {
            Self::Inserted(id) | Self::Existing(id) => id,
        }
    }
}

/// Result of recording the device's acceptance identifier pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptanceOutcome {
    /// Acceptance was added and the record advanced to accepted.
    Recorded,
    /// The exact pair was already recorded.
    Unchanged,
}

/// Result of projecting a possibly repeated or stale device status response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusProjectionOutcome {
    /// Durable state advanced.
    Advanced,
    /// The exact status was already durable.
    Unchanged,
    /// An older nonterminal observation was ignored without regressing state.
    IgnoredStale,
}

/// Result of atomically importing one bounded board RF trace page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceImportOutcome {
    inserted: usize,
    existing: usize,
    correlations_added: usize,
}

impl RfTraceImportOutcome {
    pub(crate) const fn new(inserted: usize, existing: usize, correlations_added: usize) -> Self {
        Self {
            inserted,
            existing,
            correlations_added,
        }
    }

    /// Newly inserted board observations.
    pub const fn inserted(self) -> usize {
        self.inserted
    }

    /// Exact already-durable observations encountered during replay.
    pub const fn existing(self) -> usize {
        self.existing
    }

    /// Previously uncorrelated rows enriched from immutable acceptance activity.
    pub const fn correlations_added(self) -> usize {
        self.correlations_added
    }
}

/// Invalid opaque in-memory image detected while rebuilding indexes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageError {
    /// Image schema does not match this implementation.
    SchemaVersion,
    /// A destination, message, outbox, idempotency key, acceptance ID,
    /// timeline sequence, or activity ID appeared more than once.
    DuplicateKey,
    /// An outbox row's acceptance and status contradicted one another.
    InconsistentOutbox,
    /// Retained activity referenced an impossible row or exceeded its bound.
    InconsistentActivity,
    /// RF trace boot, event, or message-correlation state was inconsistent.
    InconsistentRfTrace,
    /// A persisted next-ID counter was zero or did not exceed retained values.
    InvalidNextCounter,
}

/// Domain failure shared by all storage adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatStoreError {
    /// The same LXMF message ID was supplied with different semantic bytes.
    InboundMessageIdConflict(MessageId),
    /// An idempotency key was reused for different outbound material.
    IdempotencyConflict,
    /// The named local outbox row does not exist.
    OutboxNotFound(OutboxId),
    /// The named accepted device submission does not exist.
    SubmissionNotFound(SubmissionId),
    /// One outbox row was assigned a different acceptance pair.
    AcceptanceConflict(OutboxId),
    /// The named outbox row is delivered, cancelled, or explicitly rejected.
    OutboxNotRetryable(OutboxId),
    /// A retry reused the device-API key that names the terminal attempt.
    RetryIdempotencyKeyUnchanged(OutboxId),
    /// A submission or message ID is already assigned to another outbox row.
    AcceptanceIdAlreadyBound,
    /// Awaiting and delivered packet evidence disagreed.
    PacketEvidenceChanged,
    /// A durable terminal state was contradicted by a different terminal.
    TerminalStatusConflict,
    /// One boot identifier was reused with a different immutable radio profile.
    RfTraceBootProfileConflict(RfTraceBootId),
    /// One boot-local sequence was reused for different RF evidence.
    RfTraceEventConflict {
        /// Trace-producing boot.
        boot_id: RfTraceBootId,
        /// Conflicting boot-local event sequence.
        event_sequence: RfTraceEventSequence,
    },
    /// One hop-invariant token resolved to different immutable attempts.
    RfTraceAttemptTokenConflict(RnsAttemptToken),
    /// A local monotonically allocated identifier cannot advance safely.
    IdentifierExhausted,
    /// An opaque in-memory image failed restart validation.
    InvalidImage(ImageError),
}

impl fmt::Display for ChatStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InboundMessageIdConflict(_) => {
                formatter.write_str("LXMF message ID conflicts with retained inbound material")
            }
            Self::IdempotencyConflict => {
                formatter.write_str("idempotency key conflicts with retained outbox material")
            }
            Self::OutboxNotFound(_) => formatter.write_str("outbox record not found"),
            Self::SubmissionNotFound(_) => formatter.write_str("submission not found"),
            Self::AcceptanceConflict(_) => {
                formatter.write_str("outbox acceptance identifiers conflict")
            }
            Self::OutboxNotRetryable(_) => formatter.write_str("outbox record is not retryable"),
            Self::RetryIdempotencyKeyUnchanged(_) => {
                formatter.write_str("outbox retry requires a fresh idempotency key")
            }
            Self::AcceptanceIdAlreadyBound => {
                formatter.write_str("acceptance identifier is already bound")
            }
            Self::PacketEvidenceChanged => {
                formatter.write_str("device packet evidence changed across status projection")
            }
            Self::TerminalStatusConflict => {
                formatter.write_str("device status contradicts a durable terminal state")
            }
            Self::RfTraceBootProfileConflict(_) => {
                formatter.write_str("RF trace boot profile conflicts with retained metadata")
            }
            Self::RfTraceEventConflict { .. } => {
                formatter.write_str("RF trace sequence conflicts with retained packet evidence")
            }
            Self::RfTraceAttemptTokenConflict(_) => formatter
                .write_str("RF trace attempt token conflicts with a retained message attempt"),
            Self::IdentifierExhausted => formatter.write_str("local identifier space exhausted"),
            Self::InvalidImage(_) => formatter.write_str("in-memory persistence image is invalid"),
        }
    }
}

/// Persistent chat storage boundary.
///
/// Implementations must make each mutation atomic. In particular,
/// [`Self::commit_outbound`] must commit all exact message and API-attempt
/// material before it returns an identifier, and [`Self::record_acceptance`]
/// must commit both acceptance IDs together. Adapters must not leak storage
/// rows or connection types into the domain model.
pub trait ChatStore {
    /// Adapter-specific failure type.
    type Error;

    /// Insert or update one contact by destination hash.
    fn upsert_contact(&mut self, contact: Contact) -> Result<ContactUpsertOutcome, Self::Error>;

    /// Return one contact by destination hash.
    fn contact(&self, destination: DestinationHash) -> Result<Option<Contact>, Self::Error>;

    /// Return all contacts in deterministic destination order.
    fn contacts(&self) -> Result<Vec<Contact>, Self::Error>;

    /// Return the union of saved contacts and peers present in message history.
    ///
    /// Peers are ordered by most-recent message first, followed by contacts
    /// without messages in destination order. An authenticated sender does not
    /// become a contact merely by appearing in this projection.
    fn conversation_peers(&self) -> Result<Vec<ConversationPeer>, Self::Error>;

    /// Report whether an authenticated inbound message ID is already retained.
    fn contains_inbound(&self, message_id: MessageId) -> Result<bool, Self::Error>;

    /// Commit one inbound message, deduplicating strictly by LXMF message ID.
    fn commit_inbound(
        &mut self,
        message: InboundMessage,
    ) -> Result<InboundCommitOutcome, Self::Error> {
        self.commit_inbound_with_receiver_location(message, None)
    }

    /// Commit one inbound message and the receiver phone's current fix in one
    /// atomic mutation.
    ///
    /// The receiver fix belongs only to the first insertion. Implementations
    /// must not fabricate or backfill it when the message is already present.
    fn commit_inbound_with_receiver_location(
        &mut self,
        message: InboundMessage,
        receiver_location: Option<crate::PhoneLocationSample>,
    ) -> Result<InboundCommitOutcome, Self::Error>;

    /// Commit exact outbound material before any device send attempt.
    fn commit_outbound(
        &mut self,
        material: OutboxMaterial,
    ) -> Result<OutboxCommitOutcome, Self::Error> {
        self.commit_outbound_with_location(material, AttemptLocationStamp::not_observed())
    }

    /// Commit exact outbound material and its initial app-submission location
    /// stamp in one atomic mutation.
    fn commit_outbound_with_location(
        &mut self,
        material: OutboxMaterial,
        location: AttemptLocationStamp,
    ) -> Result<OutboxCommitOutcome, Self::Error>;

    /// Create a replacement submission for a retryable terminal row using a
    /// fresh device request key.
    ///
    /// This is not a carrier attempt inside the board-owned delivery loop.
    /// Implementations preserve the row identifier, timeline sequence, signed
    /// LXMF material, and message identity while clearing only the previous
    /// device acceptance and lifecycle projection.
    fn retry_outbox(
        &mut self,
        outbox_id: OutboxId,
        idempotency_key: crate::IdempotencyKey,
    ) -> Result<OutboxRetryOutcome, Self::Error> {
        self.retry_outbox_with_location(
            outbox_id,
            idempotency_key,
            AttemptLocationStamp::not_observed(),
        )
    }

    /// Create a replacement terminal-row submission and atomically retain the
    /// phone location state for that app-created submission.
    fn retry_outbox_with_location(
        &mut self,
        outbox_id: OutboxId,
        idempotency_key: crate::IdempotencyKey,
        location: AttemptLocationStamp,
    ) -> Result<OutboxRetryOutcome, Self::Error>;

    /// Atomically attach the device acceptance identifier pair.
    fn record_acceptance(
        &mut self,
        outbox_id: OutboxId,
        acceptance: AcceptanceIds,
    ) -> Result<AcceptanceOutcome, Self::Error>;

    /// Durably project a device status response selected by submission ID.
    fn project_submission_status(
        &mut self,
        submission_id: SubmissionId,
        state: SubmissionState,
    ) -> Result<StatusProjectionOutcome, Self::Error>;

    /// Return one complete local outbox row.
    fn outbox(&self, outbox_id: OutboxId) -> Result<Option<OutboxRecord>, Self::Error>;

    /// Return a stable timestamp-ordered timeline for one peer.
    fn conversation_timeline(
        &self,
        peer: DestinationHash,
    ) -> Result<Vec<TimelineEntry>, Self::Error>;

    /// Return one bounded newest-first page of immutable message activity.
    fn message_activity(
        &self,
        request: MessageActivityPageRequest,
    ) -> Result<MessageActivityPage, Self::Error>;

    /// Atomically import one bounded packet-keyed RF trace page.
    ///
    /// `(boot_id, event_sequence)` is the replay key. Exact duplicates are
    /// no-ops except that a now-known immutable submission acceptance may fill
    /// a previously absent message correlation. A conflicting replay fails
    /// without partial mutation.
    fn import_rf_trace_batch(
        &mut self,
        batch: RfTraceImportBatch,
    ) -> Result<RfTraceImportOutcome, Self::Error>;

    /// Return one bounded newest-first page of durable RF trace events.
    fn rf_trace(&self, request: RfTracePageRequest) -> Result<RfTracePage, Self::Error>;

    /// Query exact work needed after restart or device reconnect.
    fn reconcile(&self) -> Result<Vec<ReconcileWork>, Self::Error>;
}

pub(crate) fn observed_unix_ms() -> Option<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    u64::try_from(millis).ok()
}

struct ConversationPeerAccumulator {
    saved_name: Option<String>,
    message_count: usize,
    inbound_message_count: usize,
    last_message: Option<TimelineEntry>,
}

impl ConversationPeerAccumulator {
    fn contact(contact: &Contact) -> Self {
        Self {
            saved_name: Some(contact.display_name().to_owned()),
            message_count: 0,
            inbound_message_count: 0,
            last_message: None,
        }
    }

    const fn message_only() -> Self {
        Self {
            saved_name: None,
            message_count: 0,
            inbound_message_count: 0,
            last_message: None,
        }
    }

    fn observe(&mut self, message: TimelineEntry, inbound: bool) {
        self.message_count = self.message_count.saturating_add(1);
        if inbound {
            self.inbound_message_count = self.inbound_message_count.saturating_add(1);
        }
        let replaces_last = self.last_message.as_ref().is_none_or(|last| {
            (message.timestamp(), message.sequence()) > (last.timestamp(), last.sequence())
        });
        if replaces_last {
            self.last_message = Some(message);
        }
    }
}

pub(crate) fn project_conversation_peers<'a>(
    contacts: impl Iterator<Item = &'a Contact>,
    inbound: impl Iterator<Item = &'a InboundRecord>,
    outbox: impl Iterator<Item = &'a OutboxRecord>,
) -> Vec<ConversationPeer> {
    let mut peers = BTreeMap::<DestinationHash, ConversationPeerAccumulator>::new();
    for contact in contacts {
        peers.insert(
            contact.destination(),
            ConversationPeerAccumulator::contact(contact),
        );
    }
    for record in inbound {
        peers
            .entry(record.message().source())
            .or_insert_with(ConversationPeerAccumulator::message_only)
            .observe(TimelineEntry::inbound(record), true);
    }
    for record in outbox {
        peers
            .entry(record.material().destination())
            .or_insert_with(ConversationPeerAccumulator::message_only)
            .observe(TimelineEntry::outbound(record), false);
    }

    let mut result: Vec<_> = peers
        .into_iter()
        .map(|(peer, accumulator)| {
            ConversationPeer::new(
                peer,
                accumulator.saved_name,
                accumulator.message_count,
                accumulator.inbound_message_count,
                accumulator.last_message,
            )
        })
        .collect();
    result.sort_by(|left, right| {
        match (left.last_message(), right.last_message()) {
            (Some(left_last), Some(right_last)) => (right_last.timestamp(), right_last.sequence())
                .cmp(&(left_last.timestamp(), left_last.sequence())),
            (Some(_), None) => core::cmp::Ordering::Less,
            (None, Some(_)) => core::cmp::Ordering::Greater,
            (None, None) => left.peer().cmp(&right.peer()),
        }
        .then_with(|| left.peer().cmp(&right.peer()))
    });
    result
}

pub(crate) fn project_outbox_status(
    current: OutboxStatus,
    state: SubmissionState,
) -> Result<(OutboxStatus, StatusProjectionOutcome), ChatStoreError> {
    let current = match current {
        OutboxStatus::Committed => return Err(ChatStoreError::TerminalStatusConflict),
        OutboxStatus::Accepted => {
            return Ok((
                OutboxStatus::Device(state),
                StatusProjectionOutcome::Advanced,
            ));
        }
        OutboxStatus::Device(current) => current,
    };
    if current == state {
        return Ok((
            OutboxStatus::Device(current),
            StatusProjectionOutcome::Unchanged,
        ));
    }
    if current.is_terminal() {
        return if state.is_terminal() {
            Err(ChatStoreError::TerminalStatusConflict)
        } else {
            Ok((
                OutboxStatus::Device(current),
                StatusProjectionOutcome::IgnoredStale,
            ))
        };
    }
    if state.progression_rank() < current.progression_rank() {
        return Ok((
            OutboxStatus::Device(current),
            StatusProjectionOutcome::IgnoredStale,
        ));
    }
    if current.progression_rank() == state.progression_rank() {
        return Err(ChatStoreError::PacketEvidenceChanged);
    }
    if let (Some(current_evidence), Some(next_evidence)) =
        (current.packet_evidence(), state.packet_evidence())
        && current_evidence != next_evidence
    {
        return Err(ChatStoreError::PacketEvidenceChanged);
    }
    Ok((
        OutboxStatus::Device(state),
        StatusProjectionOutcome::Advanced,
    ))
}
