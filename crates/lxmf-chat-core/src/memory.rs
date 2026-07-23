use std::collections::{BTreeMap, BTreeSet};
use std::vec::Vec;

use crate::store::project_outbox_status;
use crate::{
    AcceptanceIds, AcceptanceOutcome, ChatStore, ChatStoreError, Contact, ContactUpsertOutcome,
    DestinationHash, IdempotencyKey, ImageError, InboundCommitOutcome, InboundMessage,
    InboundRecord, MessageId, OutboxCommitOutcome, OutboxId, OutboxMaterial, OutboxRecord,
    OutboxStatus, ReconcileWork, StatusProjectionOutcome, SubmissionId, SubmissionState,
    TimelineEntry, TimelineSequence,
};

/// Current schema of the opaque in-memory restart image.
pub const MEMORY_IMAGE_SCHEMA_VERSION: u16 = 1;

/// Opaque cloneable image used to exercise restart semantics without claiming
/// that the in-memory adapter is production persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryImage {
    schema_version: u16,
    contacts: Vec<Contact>,
    inbound: Vec<InboundRecord>,
    outbox: Vec<OutboxRecord>,
    next_outbox_id: u64,
    next_sequence: u64,
}

impl MemoryImage {
    /// Persisted image schema.
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }
}

/// Executable in-memory reference implementation of [`ChatStore`].
///
/// This type specifies transaction and reconciliation behavior independently
/// of the production SQLite adapter. [`Self::image`] and [`Self::open`] support
/// restart tests but are not a substitute for a durable database transaction
/// log.
#[derive(Clone, Debug)]
pub struct MemoryChatStore {
    contacts: BTreeMap<DestinationHash, Contact>,
    inbound: BTreeMap<MessageId, InboundRecord>,
    outbox: BTreeMap<OutboxId, OutboxRecord>,
    idempotency_index: BTreeMap<IdempotencyKey, OutboxId>,
    submission_index: BTreeMap<SubmissionId, OutboxId>,
    accepted_message_index: BTreeMap<MessageId, OutboxId>,
    next_outbox_id: u64,
    next_sequence: u64,
}

impl Default for MemoryChatStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryChatStore {
    /// Construct an empty store.
    pub const fn new() -> Self {
        Self {
            contacts: BTreeMap::new(),
            inbound: BTreeMap::new(),
            outbox: BTreeMap::new(),
            idempotency_index: BTreeMap::new(),
            submission_index: BTreeMap::new(),
            accepted_message_index: BTreeMap::new(),
            next_outbox_id: 1,
            next_sequence: 1,
        }
    }

    /// Export an opaque restart image.
    pub fn image(&self) -> MemoryImage {
        MemoryImage {
            schema_version: MEMORY_IMAGE_SCHEMA_VERSION,
            contacts: self.contacts.values().cloned().collect(),
            inbound: self.inbound.values().cloned().collect(),
            outbox: self.outbox.values().cloned().collect(),
            next_outbox_id: self.next_outbox_id,
            next_sequence: self.next_sequence,
        }
    }

    /// Reopen an opaque image and rebuild every uniqueness/correlation index.
    pub fn open(image: MemoryImage) -> Result<Self, ChatStoreError> {
        if image.schema_version != MEMORY_IMAGE_SCHEMA_VERSION {
            return Err(ChatStoreError::InvalidImage(ImageError::SchemaVersion));
        }
        if image.next_outbox_id == 0 || image.next_sequence == 0 {
            return Err(ChatStoreError::InvalidImage(ImageError::InvalidNextCounter));
        }

        let mut store = Self {
            contacts: BTreeMap::new(),
            inbound: BTreeMap::new(),
            outbox: BTreeMap::new(),
            idempotency_index: BTreeMap::new(),
            submission_index: BTreeMap::new(),
            accepted_message_index: BTreeMap::new(),
            next_outbox_id: image.next_outbox_id,
            next_sequence: image.next_sequence,
        };
        let mut sequences = BTreeSet::new();
        let mut greatest_outbox = 0_u64;
        let mut greatest_sequence = 0_u64;

        for contact in image.contacts {
            if store
                .contacts
                .insert(contact.destination(), contact)
                .is_some()
            {
                return Err(ChatStoreError::InvalidImage(ImageError::DuplicateKey));
            }
        }
        for record in image.inbound {
            let sequence = record.sequence().get();
            greatest_sequence = greatest_sequence.max(sequence);
            if !sequences.insert(sequence)
                || store
                    .inbound
                    .insert(record.message().message_id(), record)
                    .is_some()
            {
                return Err(ChatStoreError::InvalidImage(ImageError::DuplicateKey));
            }
        }
        for record in image.outbox {
            let id = record.id();
            let sequence = record.sequence().get();
            greatest_outbox = greatest_outbox.max(id.get());
            greatest_sequence = greatest_sequence.max(sequence);
            if !sequences.insert(sequence) {
                return Err(ChatStoreError::InvalidImage(ImageError::DuplicateKey));
            }
            match (record.acceptance(), record.status()) {
                (None, OutboxStatus::Committed) => {}
                (Some(_), OutboxStatus::Accepted | OutboxStatus::Device(_)) => {}
                _ => {
                    return Err(ChatStoreError::InvalidImage(ImageError::InconsistentOutbox));
                }
            }
            if store
                .idempotency_index
                .insert(record.material().idempotency_key(), id)
                .is_some()
            {
                return Err(ChatStoreError::InvalidImage(ImageError::DuplicateKey));
            }
            if let Some(acceptance) = record.acceptance()
                && (store
                    .submission_index
                    .insert(acceptance.submission_id(), id)
                    .is_some()
                    || store
                        .accepted_message_index
                        .insert(acceptance.message_id(), id)
                        .is_some())
            {
                return Err(ChatStoreError::InvalidImage(ImageError::DuplicateKey));
            }
            if store.outbox.insert(id, record).is_some() {
                return Err(ChatStoreError::InvalidImage(ImageError::DuplicateKey));
            }
        }
        if store.next_outbox_id <= greatest_outbox || store.next_sequence <= greatest_sequence {
            return Err(ChatStoreError::InvalidImage(ImageError::InvalidNextCounter));
        }
        Ok(store)
    }

    fn allocate_outbox_and_sequence(
        &mut self,
    ) -> Result<(OutboxId, TimelineSequence), ChatStoreError> {
        let next_outbox_id = self
            .next_outbox_id
            .checked_add(1)
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        let outbox_id =
            OutboxId::new(self.next_outbox_id).ok_or(ChatStoreError::IdentifierExhausted)?;
        let sequence =
            TimelineSequence::new(self.next_sequence).ok_or(ChatStoreError::IdentifierExhausted)?;
        self.next_outbox_id = next_outbox_id;
        self.next_sequence = next_sequence;
        Ok((outbox_id, sequence))
    }

    fn allocate_sequence(&mut self) -> Result<TimelineSequence, ChatStoreError> {
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        let sequence =
            TimelineSequence::new(self.next_sequence).ok_or(ChatStoreError::IdentifierExhausted)?;
        self.next_sequence = next_sequence;
        Ok(sequence)
    }
}

impl ChatStore for MemoryChatStore {
    type Error = ChatStoreError;

    fn upsert_contact(&mut self, contact: Contact) -> Result<ContactUpsertOutcome, Self::Error> {
        let destination = contact.destination();
        let outcome = match self.contacts.get(&destination) {
            None => ContactUpsertOutcome::Inserted,
            Some(existing) if existing == &contact => ContactUpsertOutcome::Unchanged,
            Some(_) => ContactUpsertOutcome::Updated,
        };
        if outcome != ContactUpsertOutcome::Unchanged {
            self.contacts.insert(destination, contact);
        }
        Ok(outcome)
    }

    fn contact(&self, destination: DestinationHash) -> Result<Option<Contact>, Self::Error> {
        Ok(self.contacts.get(&destination).cloned())
    }

    fn contacts(&self) -> Result<Vec<Contact>, Self::Error> {
        Ok(self.contacts.values().cloned().collect())
    }

    fn contains_inbound(&self, message_id: MessageId) -> Result<bool, Self::Error> {
        Ok(self.inbound.contains_key(&message_id))
    }

    fn commit_inbound(
        &mut self,
        message: InboundMessage,
    ) -> Result<InboundCommitOutcome, Self::Error> {
        let message_id = message.message_id();
        if let Some(existing) = self.inbound.get(&message_id) {
            if existing.message() == &message {
                return Ok(InboundCommitOutcome::Duplicate);
            }
            return Err(ChatStoreError::InboundMessageIdConflict(message_id));
        }
        let sequence = self.allocate_sequence()?;
        self.inbound
            .insert(message_id, InboundRecord { sequence, message });
        Ok(InboundCommitOutcome::Inserted)
    }

    fn commit_outbound(
        &mut self,
        material: OutboxMaterial,
    ) -> Result<OutboxCommitOutcome, Self::Error> {
        if let Some(existing_id) = self
            .idempotency_index
            .get(&material.idempotency_key())
            .copied()
        {
            let existing = self
                .outbox
                .get(&existing_id)
                .expect("idempotency index must reference an outbox row");
            if existing.material() == &material {
                return Ok(OutboxCommitOutcome::Existing(existing_id));
            }
            return Err(ChatStoreError::IdempotencyConflict);
        }
        let (id, sequence) = self.allocate_outbox_and_sequence()?;
        let key = material.idempotency_key();
        self.outbox.insert(
            id,
            OutboxRecord {
                id,
                sequence,
                material,
                acceptance: None,
                status: OutboxStatus::Committed,
            },
        );
        self.idempotency_index.insert(key, id);
        Ok(OutboxCommitOutcome::Inserted(id))
    }

    fn record_acceptance(
        &mut self,
        outbox_id: OutboxId,
        acceptance: AcceptanceIds,
    ) -> Result<AcceptanceOutcome, Self::Error> {
        let existing_acceptance = self
            .outbox
            .get(&outbox_id)
            .ok_or(ChatStoreError::OutboxNotFound(outbox_id))?
            .acceptance();
        if let Some(existing) = existing_acceptance {
            return if existing == acceptance {
                Ok(AcceptanceOutcome::Unchanged)
            } else {
                Err(ChatStoreError::AcceptanceConflict(outbox_id))
            };
        }
        if self
            .submission_index
            .get(&acceptance.submission_id())
            .is_some_and(|existing| *existing != outbox_id)
            || self
                .accepted_message_index
                .get(&acceptance.message_id())
                .is_some_and(|existing| *existing != outbox_id)
        {
            return Err(ChatStoreError::AcceptanceIdAlreadyBound);
        }
        let record = self
            .outbox
            .get_mut(&outbox_id)
            .expect("outbox existence was checked");
        record.acceptance = Some(acceptance);
        record.status = OutboxStatus::Accepted;
        self.submission_index
            .insert(acceptance.submission_id(), outbox_id);
        self.accepted_message_index
            .insert(acceptance.message_id(), outbox_id);
        Ok(AcceptanceOutcome::Recorded)
    }

    fn project_submission_status(
        &mut self,
        submission_id: SubmissionId,
        state: SubmissionState,
    ) -> Result<StatusProjectionOutcome, Self::Error> {
        let outbox_id = self
            .submission_index
            .get(&submission_id)
            .copied()
            .ok_or(ChatStoreError::SubmissionNotFound(submission_id))?;
        let record = self
            .outbox
            .get_mut(&outbox_id)
            .expect("submission index must reference an outbox row");
        if matches!(record.status, OutboxStatus::Committed) {
            return Err(ChatStoreError::SubmissionNotFound(submission_id));
        }
        let (status, outcome) = project_outbox_status(record.status, state)?;
        record.status = status;
        Ok(outcome)
    }

    fn outbox(&self, outbox_id: OutboxId) -> Result<Option<OutboxRecord>, Self::Error> {
        Ok(self.outbox.get(&outbox_id).cloned())
    }

    fn conversation_timeline(
        &self,
        peer: DestinationHash,
    ) -> Result<Vec<TimelineEntry>, Self::Error> {
        let mut timeline = Vec::new();
        timeline.extend(
            self.inbound
                .values()
                .filter(|record| record.message().source() == peer)
                .map(TimelineEntry::inbound),
        );
        timeline.extend(
            self.outbox
                .values()
                .filter(|record| record.material().destination() == peer)
                .map(TimelineEntry::outbound),
        );
        timeline.sort_by_key(|entry| (entry.timestamp().get(), entry.sequence().get()));
        Ok(timeline)
    }

    fn reconcile(&self) -> Result<Vec<ReconcileWork>, Self::Error> {
        let mut work = Vec::new();
        for record in self.outbox.values() {
            match record.acceptance() {
                None => work.push(ReconcileWork::Submit {
                    outbox_id: record.id(),
                    material: record.material().clone(),
                }),
                Some(acceptance) if !record.status().is_terminal() => {
                    work.push(ReconcileWork::RefreshStatus {
                        outbox_id: record.id(),
                        acceptance,
                    });
                }
                Some(_) => {}
            }
        }
        Ok(work)
    }
}
