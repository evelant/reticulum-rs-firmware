use core::fmt;

use reticulum_lxmf_chat_core::{
    AcceptanceIds, AcceptanceOutcome, ChatStore, Contact, ContactUpsertOutcome, DestinationHash,
    InboundCommitOutcome, MessageId, OutboxCommitOutcome, OutboxId, OutboxMaterial, ReconcileWork,
    StatusProjectionOutcome, SubmissionState, TimelineEntry,
};

use crate::{InboxCursor, LxmfSession};

/// One committed reconciliation action, or an empty durable outbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileStep {
    /// No unfinished outbox work exists.
    Idle,
    /// Exact committed material was accepted and both device IDs were stored.
    Submitted {
        /// Stable local outbox row.
        outbox_id: OutboxId,
        /// Exact device acceptance identifiers.
        acceptance: AcceptanceIds,
        /// Whether the exact acceptance was newly recorded or already present.
        outcome: AcceptanceOutcome,
    },
    /// One accepted submission's status was durably projected.
    Refreshed {
        /// Stable local outbox row.
        outbox_id: OutboxId,
        /// Exact device acceptance identifiers.
        acceptance: AcceptanceIds,
        /// Device state supplied to the monotonic projector.
        state: SubmissionState,
        /// Result of the durable projection.
        outcome: StatusProjectionOutcome,
    },
}

/// One incremental device-inbox scan action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxStep {
    /// No committed message follows the current session-local cursor.
    EndOfScan,
    /// The summary named an exact message already retained locally, so its
    /// complete wire was not downloaded again.
    AlreadyImported {
        /// Authenticated message identifier.
        message_id: MessageId,
        /// Session-local cursor advanced past this message.
        cursor: InboxCursor,
    },
    /// A complete newly observed message was validated and committed.
    Imported {
        /// Authenticated message identifier.
        message_id: MessageId,
        /// Session-local cursor advanced after the commit.
        cursor: InboxCursor,
        /// Whether the exact semantic message was inserted or found during a
        /// concurrent/idempotent retry.
        outcome: InboundCommitOutcome,
    },
}

/// Failure from either durable local state or the authenticated session.
#[derive(Debug)]
pub enum EngineError<StoreError, SessionError> {
    /// Persistent chat-store operation failed.
    Store(StoreError),
    /// Authenticated device operation failed.
    Session(SessionError),
    /// A session returned complete wire for a different summary identifier.
    InboxMessageIdMismatch {
        /// Identifier selected by the authenticated summary.
        expected: MessageId,
        /// Identifier returned with the complete message.
        observed: MessageId,
    },
}

impl<StoreError: fmt::Display, SessionError: fmt::Display> fmt::Display
    for EngineError<StoreError, SessionError>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(formatter, "chat store failed: {error}"),
            Self::Session(error) => write!(formatter, "device session failed: {error}"),
            Self::InboxMessageIdMismatch { .. } => {
                formatter.write_str("device session returned the wrong inbox message")
            }
        }
    }
}

impl<StoreError, SessionError> std::error::Error for EngineError<StoreError, SessionError>
where
    StoreError: std::error::Error + 'static,
    SessionError: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::InboxMessageIdMismatch { .. } => None,
        }
    }
}

/// Single-owner application engine over one persistent chat store.
pub struct ChatEngine<S> {
    store: S,
    reconcile_offset: usize,
    inbox_after: Option<InboxCursor>,
}

impl<S> ChatEngine<S> {
    /// Construct an engine around the sole store owner.
    pub const fn new(store: S) -> Self {
        Self {
            store,
            reconcile_offset: 0,
            inbox_after: None,
        }
    }

    /// Borrow the underlying store for adapter-specific operations such as
    /// authenticated database binding.
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// Mutably borrow the underlying store for adapter-specific operations.
    pub const fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Recover the sole store owner.
    pub fn into_store(self) -> S {
        self.store
    }

    /// Restart inbox enumeration from the beginning after a new device session.
    pub fn reset_session_scan(&mut self) {
        self.inbox_after = None;
    }
}

impl<S: ChatStore> ChatEngine<S> {
    /// Insert or update one contact.
    pub fn upsert_contact(&mut self, contact: Contact) -> Result<ContactUpsertOutcome, S::Error> {
        self.store.upsert_contact(contact)
    }

    /// Return all contacts in deterministic destination order.
    pub fn contacts(&self) -> Result<Vec<Contact>, S::Error> {
        self.store.contacts()
    }

    /// Return one stable conversation timeline.
    pub fn timeline(&self, peer: DestinationHash) -> Result<Vec<TimelineEntry>, S::Error> {
        self.store.conversation_timeline(peer)
    }

    /// Commit exact outbound material without requiring a live device session.
    pub fn enqueue_send(
        &mut self,
        material: OutboxMaterial,
    ) -> Result<OutboxCommitOutcome, S::Error> {
        self.store.commit_outbound(material)
    }

    /// Perform at most one submit or status request and its corresponding
    /// durable local mutation. Work selection rotates so one long-lived
    /// nonterminal submission cannot starve later outbox rows.
    pub fn reconcile_step<D: LxmfSession + ?Sized>(
        &mut self,
        session: &mut D,
    ) -> Result<ReconcileStep, EngineError<S::Error, D::Error>> {
        let mut work = self.store.reconcile().map_err(EngineError::Store)?;
        if work.is_empty() {
            self.reconcile_offset = 0;
            return Ok(ReconcileStep::Idle);
        }
        let index = self.reconcile_offset % work.len();
        let item = work.remove(index);
        let result = match item {
            ReconcileWork::Submit {
                outbox_id,
                material,
            } => {
                let acceptance = session.submit(&material).map_err(EngineError::Session)?;
                let outcome = self
                    .store
                    .record_acceptance(outbox_id, acceptance)
                    .map_err(EngineError::Store)?;
                ReconcileStep::Submitted {
                    outbox_id,
                    acceptance,
                    outcome,
                }
            }
            ReconcileWork::RefreshStatus {
                outbox_id,
                acceptance,
            } => {
                let state = session
                    .submission_status(acceptance.submission_id())
                    .map_err(EngineError::Session)?;
                let outcome = self
                    .store
                    .project_submission_status(acceptance.submission_id(), state)
                    .map_err(EngineError::Store)?;
                ReconcileStep::Refreshed {
                    outbox_id,
                    acceptance,
                    state,
                    outcome,
                }
            }
        };
        self.reconcile_offset = (index + 1) % work.len().saturating_add(1);
        Ok(result)
    }

    /// Advance one committed inbox summary. Known IDs skip complete wire reads;
    /// unknown messages advance the cursor only after a successful local commit.
    pub fn inbox_step<D: LxmfSession + ?Sized>(
        &mut self,
        session: &mut D,
    ) -> Result<InboxStep, EngineError<S::Error, D::Error>> {
        let Some(summary) = session
            .next_inbox(self.inbox_after)
            .map_err(EngineError::Session)?
        else {
            return Ok(InboxStep::EndOfScan);
        };
        if self
            .store
            .contains_inbound(summary.message_id())
            .map_err(EngineError::Store)?
        {
            self.inbox_after = Some(summary.cursor());
            return Ok(InboxStep::AlreadyImported {
                message_id: summary.message_id(),
                cursor: summary.cursor(),
            });
        }
        let message = session.read_inbox(summary).map_err(EngineError::Session)?;
        if message.message_id() != summary.message_id() {
            return Err(EngineError::InboxMessageIdMismatch {
                expected: summary.message_id(),
                observed: message.message_id(),
            });
        }
        let message_id = message.message_id();
        let outcome = self
            .store
            .commit_inbound(message)
            .map_err(EngineError::Store)?;
        self.inbox_after = Some(summary.cursor());
        Ok(InboxStep::Imported {
            message_id,
            cursor: summary.cursor(),
            outcome,
        })
    }
}
