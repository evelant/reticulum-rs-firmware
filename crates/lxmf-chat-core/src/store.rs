use core::fmt;

use std::vec::Vec;

use crate::{
    AcceptanceIds, Contact, DestinationHash, InboundMessage, MessageId, OutboxId, OutboxMaterial,
    OutboxRecord, OutboxStatus, ReconcileWork, SubmissionId, SubmissionState, TimelineEntry,
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

/// Invalid opaque in-memory image detected while rebuilding indexes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageError {
    /// Image schema does not match this implementation.
    SchemaVersion,
    /// A destination, message, outbox, idempotency key, acceptance ID, or
    /// timeline sequence appeared more than once.
    DuplicateKey,
    /// An outbox row's acceptance and status contradicted one another.
    InconsistentOutbox,
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
    /// A submission or message ID is already assigned to another outbox row.
    AcceptanceIdAlreadyBound,
    /// Awaiting and delivered packet evidence disagreed.
    PacketEvidenceChanged,
    /// A durable terminal state was contradicted by a different terminal.
    TerminalStatusConflict,
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
            Self::AcceptanceIdAlreadyBound => {
                formatter.write_str("acceptance identifier is already bound")
            }
            Self::PacketEvidenceChanged => {
                formatter.write_str("device packet evidence changed across status projection")
            }
            Self::TerminalStatusConflict => {
                formatter.write_str("device status contradicts a durable terminal state")
            }
            Self::IdentifierExhausted => formatter.write_str("local identifier space exhausted"),
            Self::InvalidImage(_) => formatter.write_str("in-memory persistence image is invalid"),
        }
    }
}

/// Persistent chat storage boundary.
///
/// Implementations must make each mutation atomic. In particular,
/// [`Self::commit_outbound`] must commit all exact retry material before it
/// returns an identifier, and [`Self::record_acceptance`] must commit both
/// acceptance IDs together. Adapters must not leak storage rows or connection
/// types into the domain model.
pub trait ChatStore {
    /// Adapter-specific failure type.
    type Error;

    /// Insert or update one contact by destination hash.
    fn upsert_contact(&mut self, contact: Contact) -> Result<ContactUpsertOutcome, Self::Error>;

    /// Return one contact by destination hash.
    fn contact(&self, destination: DestinationHash) -> Result<Option<Contact>, Self::Error>;

    /// Return all contacts in deterministic destination order.
    fn contacts(&self) -> Result<Vec<Contact>, Self::Error>;

    /// Report whether an authenticated inbound message ID is already retained.
    fn contains_inbound(&self, message_id: MessageId) -> Result<bool, Self::Error>;

    /// Commit one inbound message, deduplicating strictly by LXMF message ID.
    fn commit_inbound(
        &mut self,
        message: InboundMessage,
    ) -> Result<InboundCommitOutcome, Self::Error>;

    /// Commit exact outbound material before any device send attempt.
    fn commit_outbound(
        &mut self,
        material: OutboxMaterial,
    ) -> Result<OutboxCommitOutcome, Self::Error>;

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

    /// Query exact work needed after restart or device reconnect.
    fn reconcile(&self) -> Result<Vec<ReconcileWork>, Self::Error>;
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
