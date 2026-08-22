use std::collections::{BTreeMap, BTreeSet};
use std::vec::Vec;

use crate::store::{observed_unix_ms, project_conversation_peers, project_outbox_status};
use crate::{
    AcceptanceIds, AcceptanceOutcome, AttemptLocationStamp, ChatStore, ChatStoreError, Contact,
    ContactUpsertOutcome, ConversationPeer, DestinationHash, IdempotencyKey, ImageError,
    InboundCommitOutcome, InboundMessage, InboundRecord, MAX_MESSAGE_ACTIVITY_EVENTS,
    MessageActivityEvent, MessageActivityId, MessageActivityKind, MessageActivityPage,
    MessageActivityPageRequest, MessageActivityScope, MessageAttemptNumber, MessageId,
    OutboxCommitOutcome, OutboxId, OutboxMaterial, OutboxRecord, OutboxRetryOutcome, OutboxStatus,
    ReconcileWork, RfTraceBootId, RfTraceEvent, RfTraceEventId, RfTraceEventSequence,
    RfTraceImportBatch, RfTraceImportOutcome, RfTraceMessageCorrelation, RfTracePage,
    RfTracePageRequest, RfTraceRadioProfile, RfTraceScope, RnsAttemptToken,
    StatusProjectionOutcome, SubmissionId, SubmissionState, TimelineDirection, TimelineEntry,
    TimelineSequence,
};

/// Current schema of the opaque in-memory restart image.
pub const MEMORY_IMAGE_SCHEMA_VERSION: u16 = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MemoryRfTraceBoot {
    profile: RfTraceRadioProfile,
    history_incomplete: bool,
}

/// Opaque cloneable image used to exercise restart semantics without claiming
/// that the in-memory adapter is production persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryImage {
    schema_version: u16,
    contacts: Vec<Contact>,
    inbound: Vec<InboundRecord>,
    outbox: Vec<OutboxRecord>,
    activity: Vec<MessageActivityEvent>,
    activity_history_incomplete: bool,
    rf_trace_boots: Vec<(RfTraceBootId, MemoryRfTraceBoot)>,
    rf_trace_events: Vec<RfTraceEvent>,
    next_outbox_id: u64,
    next_sequence: u64,
    next_activity_id: u64,
    next_rf_trace_id: u64,
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
    activity: Vec<MessageActivityEvent>,
    activity_history_incomplete: bool,
    rf_trace_boots: BTreeMap<RfTraceBootId, MemoryRfTraceBoot>,
    rf_trace_events: BTreeMap<RfTraceEventId, RfTraceEvent>,
    rf_trace_keys: BTreeMap<(RfTraceBootId, RfTraceEventSequence), RfTraceEventId>,
    next_outbox_id: u64,
    next_sequence: u64,
    next_activity_id: u64,
    next_rf_trace_id: u64,
}

#[derive(Clone, Copy)]
enum RetryMutation {
    Requeued,
    AlreadyPending,
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
            activity: Vec::new(),
            activity_history_incomplete: false,
            rf_trace_boots: BTreeMap::new(),
            rf_trace_events: BTreeMap::new(),
            rf_trace_keys: BTreeMap::new(),
            next_outbox_id: 1,
            next_sequence: 1,
            next_activity_id: 1,
            next_rf_trace_id: 1,
        }
    }

    /// Export an opaque restart image.
    pub fn image(&self) -> MemoryImage {
        MemoryImage {
            schema_version: MEMORY_IMAGE_SCHEMA_VERSION,
            contacts: self.contacts.values().cloned().collect(),
            inbound: self.inbound.values().cloned().collect(),
            outbox: self.outbox.values().cloned().collect(),
            activity: self.activity.clone(),
            activity_history_incomplete: self.activity_history_incomplete,
            rf_trace_boots: self
                .rf_trace_boots
                .iter()
                .map(|(boot_id, boot)| (*boot_id, *boot))
                .collect(),
            rf_trace_events: self.rf_trace_events.values().copied().collect(),
            next_outbox_id: self.next_outbox_id,
            next_sequence: self.next_sequence,
            next_activity_id: self.next_activity_id,
            next_rf_trace_id: self.next_rf_trace_id,
        }
    }

    /// Reopen an opaque image and rebuild every uniqueness/correlation index.
    pub fn open(image: MemoryImage) -> Result<Self, ChatStoreError> {
        if image.schema_version != MEMORY_IMAGE_SCHEMA_VERSION {
            return Err(ChatStoreError::InvalidImage(ImageError::SchemaVersion));
        }
        if image.next_outbox_id == 0
            || image.next_sequence == 0
            || image.next_activity_id == 0
            || image.next_rf_trace_id == 0
        {
            return Err(ChatStoreError::InvalidImage(ImageError::InvalidNextCounter));
        }

        let mut store = Self {
            contacts: BTreeMap::new(),
            inbound: BTreeMap::new(),
            outbox: BTreeMap::new(),
            idempotency_index: BTreeMap::new(),
            submission_index: BTreeMap::new(),
            accepted_message_index: BTreeMap::new(),
            activity: Vec::new(),
            activity_history_incomplete: image.activity_history_incomplete,
            rf_trace_boots: BTreeMap::new(),
            rf_trace_events: BTreeMap::new(),
            rf_trace_keys: BTreeMap::new(),
            next_outbox_id: image.next_outbox_id,
            next_sequence: image.next_sequence,
            next_activity_id: image.next_activity_id,
            next_rf_trace_id: image.next_rf_trace_id,
        };
        let mut sequences = BTreeSet::new();
        let mut activity_ids = BTreeSet::new();
        let mut greatest_outbox = 0_u64;
        let mut greatest_sequence = 0_u64;
        let mut greatest_activity = 0_u64;
        let mut greatest_rf_trace = 0_u64;

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
        for event in image.activity {
            let id = event.id().get();
            greatest_activity = greatest_activity.max(id);
            if !activity_ids.insert(id) {
                return Err(ChatStoreError::InvalidImage(ImageError::DuplicateKey));
            }
            if event.timeline_sequence().get() >= store.next_sequence
                || event
                    .outbox_id()
                    .is_some_and(|outbox_id| outbox_id.get() >= store.next_outbox_id)
            {
                return Err(ChatStoreError::InvalidImage(
                    ImageError::InconsistentActivity,
                ));
            }
            store.activity.push(event);
        }
        store.activity.sort_by_key(|event| event.id().get());
        if store.activity.len() > MAX_MESSAGE_ACTIVITY_EVENTS {
            return Err(ChatStoreError::InvalidImage(
                ImageError::InconsistentActivity,
            ));
        }

        for (boot_id, boot) in image.rf_trace_boots {
            if store.rf_trace_boots.insert(boot_id, boot).is_some() {
                return Err(ChatStoreError::InvalidImage(ImageError::DuplicateKey));
            }
        }
        for event in image.rf_trace_events {
            let id = event.id();
            greatest_rf_trace = greatest_rf_trace.max(id.get());
            let Some(boot) = store.rf_trace_boots.get(&event.boot_id()) else {
                return Err(ChatStoreError::InvalidImage(
                    ImageError::InconsistentRfTrace,
                ));
            };
            if boot.profile != event.profile()
                || event.imported_at_unix_ms() > crate::MAX_UNIX_TIMESTAMP_MILLIS
                || store
                    .rf_trace_keys
                    .insert((event.boot_id(), event.observation().event_sequence()), id)
                    .is_some()
                || store.rf_trace_events.insert(id, event).is_some()
            {
                return Err(ChatStoreError::InvalidImage(
                    ImageError::InconsistentRfTrace,
                ));
            }
        }
        if !store.valid_persisted_rf_trace_correlations() {
            return Err(ChatStoreError::InvalidImage(
                ImageError::InconsistentRfTrace,
            ));
        }
        if store.next_outbox_id <= greatest_outbox
            || store.next_sequence <= greatest_sequence
            || store.next_activity_id <= greatest_activity
            || store.next_rf_trace_id <= greatest_rf_trace
        {
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

    fn reserve_activity_id(&self) -> Result<(MessageActivityId, u64), ChatStoreError> {
        let next_activity_id = self
            .next_activity_id
            .checked_add(1)
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        let activity_id = MessageActivityId::new(self.next_activity_id)
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        Ok((activity_id, next_activity_id))
    }

    fn append_activity(&mut self, event: MessageActivityEvent, next_activity_id: u64) {
        self.next_activity_id = next_activity_id;
        self.activity.push(event);
        if self.activity.len() > MAX_MESSAGE_ACTIVITY_EVENTS {
            let overflow = self.activity.len() - MAX_MESSAGE_ACTIVITY_EVENTS;
            self.activity.drain(..overflow);
            self.activity_history_incomplete = true;
        }
    }

    fn rf_trace_correlation(
        activity: &[MessageActivityEvent],
        submission_id: SubmissionId,
    ) -> Result<Option<RfTraceMessageCorrelation>, ChatStoreError> {
        let mut correlation = None;
        for event in activity {
            let MessageActivityKind::OutboundAccepted { acceptance } = event.kind() else {
                continue;
            };
            if acceptance.submission_id() != submission_id {
                continue;
            }
            let (Some(outbox_id), Some(attempt_number)) =
                (event.outbox_id(), event.attempt_number())
            else {
                return Err(ChatStoreError::InvalidImage(
                    ImageError::InconsistentActivity,
                ));
            };
            let mut attempt_location = None;
            for attempt_event in activity {
                if attempt_event.outbox_id() != Some(outbox_id)
                    || attempt_event.attempt_number() != Some(attempt_number)
                {
                    continue;
                }
                let Some(location) = attempt_event.attempt_location() else {
                    continue;
                };
                if attempt_location.is_some_and(|existing| existing != location) {
                    return Err(ChatStoreError::InvalidImage(
                        ImageError::InconsistentActivity,
                    ));
                }
                attempt_location = Some(location);
            }
            let Some(attempt_location) = attempt_location else {
                // Older retained activity may straddle the bounded-history
                // cutoff. Do not claim a partial correlation without its
                // attempt location.
                continue;
            };
            let candidate = RfTraceMessageCorrelation::new(
                event.timeline_sequence(),
                outbox_id,
                attempt_number,
                attempt_location,
            );
            if correlation.is_some_and(|existing| existing != candidate) {
                return Err(ChatStoreError::InvalidImage(
                    ImageError::InconsistentActivity,
                ));
            }
            correlation = Some(candidate);
        }
        Ok(correlation)
    }

    fn valid_persisted_rf_trace_correlations(&self) -> bool {
        let mut tokens = BTreeMap::<RnsAttemptToken, RfTraceMessageCorrelation>::new();
        for event in self.rf_trace_events.values() {
            let Some(correlation) = event.message_correlation() else {
                continue;
            };
            let Some(token) = event.observation().rns_attempt_token() else {
                return false;
            };
            let Some(outbox) = self.outbox.get(&correlation.outbox_id()) else {
                return false;
            };
            if outbox.sequence() != correlation.timeline_sequence()
                || correlation.attempt_number() > outbox.current_attempt()
                || tokens
                    .insert(token, correlation)
                    .is_some_and(|existing| existing != correlation)
            {
                return false;
            }
            if let Some(submission_id) = event.observation().submission_id() {
                let Ok(current) = Self::rf_trace_correlation(&self.activity, submission_id) else {
                    return false;
                };
                if current.is_some_and(|current| current != correlation) {
                    return false;
                }
            }
        }
        true
    }

    fn seed_token_correlation(
        tokens: &mut BTreeMap<RnsAttemptToken, RfTraceMessageCorrelation>,
        token: RnsAttemptToken,
        correlation: RfTraceMessageCorrelation,
    ) -> Result<(), ChatStoreError> {
        if tokens
            .insert(token, correlation)
            .is_some_and(|existing| existing != correlation)
        {
            return Err(ChatStoreError::RfTraceAttemptTokenConflict(token));
        }
        Ok(())
    }

    fn reserve_rf_trace_id(&self) -> Result<(RfTraceEventId, u64), ChatStoreError> {
        let next_rf_trace_id = self
            .next_rf_trace_id
            .checked_add(1)
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        let id = RfTraceEventId::new(self.next_rf_trace_id)
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        Ok((id, next_rf_trace_id))
    }

    fn import_rf_trace_batch_inner(
        &mut self,
        batch: RfTraceImportBatch,
    ) -> Result<RfTraceImportOutcome, ChatStoreError> {
        let boot_id = batch.boot_id();
        let profile = batch.profile();
        if self
            .rf_trace_boots
            .get(&boot_id)
            .is_some_and(|boot| boot.profile != profile)
        {
            return Err(ChatStoreError::RfTraceBootProfileConflict(boot_id));
        }

        // Preflight the complete page before assigning any local identifiers.
        for observation in batch.observations() {
            let key = (boot_id, observation.event_sequence());
            if let Some(id) = self.rf_trace_keys.get(&key) {
                let existing = self
                    .rf_trace_events
                    .get(id)
                    .expect("the RF trace replay index names a retained event");
                if existing.observation() != *observation {
                    return Err(ChatStoreError::RfTraceEventConflict {
                        boot_id,
                        event_sequence: observation.event_sequence(),
                    });
                }
            }
        }

        let mut token_correlations = BTreeMap::new();
        for event in self.rf_trace_events.values() {
            if let (Some(token), Some(correlation)) = (
                event.observation().rns_attempt_token(),
                event.message_correlation(),
            ) {
                Self::seed_token_correlation(&mut token_correlations, token, correlation)?;
            }
        }
        // Resolve every direct submission seed before importing the page so a
        // later terminal record can correlate even when its route was in an
        // earlier page (or before a process restart).
        for observation in self
            .rf_trace_events
            .values()
            .map(|event| event.observation())
            .chain(batch.observations().iter().copied())
        {
            let (Some(submission_id), Some(token)) =
                (observation.submission_id(), observation.rns_attempt_token())
            else {
                continue;
            };
            if let Some(correlation) = Self::rf_trace_correlation(&self.activity, submission_id)? {
                Self::seed_token_correlation(&mut token_correlations, token, correlation)?;
            }
        }

        let mut inserted = 0;
        let mut existing_count = 0;
        let mut correlations_added = 0;
        for observation in batch.observations() {
            let key = (boot_id, observation.event_sequence());
            let correlation = observation
                .rns_attempt_token()
                .and_then(|token| token_correlations.get(&token).copied());
            if let Some(id) = self.rf_trace_keys.get(&key).copied() {
                existing_count += 1;
                let event = self
                    .rf_trace_events
                    .get_mut(&id)
                    .expect("the RF trace replay index names a retained event");
                match (event.correlation, correlation) {
                    (None, Some(correlation)) => {
                        event.correlation = Some(correlation);
                        correlations_added += 1;
                    }
                    (Some(existing), Some(current)) if existing != current => {
                        return Err(ChatStoreError::InvalidImage(
                            ImageError::InconsistentRfTrace,
                        ));
                    }
                    _ => {}
                }
                continue;
            }

            let (id, next_id) = self.reserve_rf_trace_id()?;
            let event = RfTraceEvent {
                id,
                boot_id,
                profile,
                imported_at_unix_ms: batch.imported_at_unix_ms(),
                observation: *observation,
                correlation,
            };
            self.rf_trace_events.insert(id, event);
            self.rf_trace_keys.insert(key, id);
            self.next_rf_trace_id = next_id;
            inserted += 1;
        }

        // A route seed may arrive after a terminal page. Enrich every retained
        // event sharing its unambiguous token without rewriting raw evidence.
        for event in self.rf_trace_events.values_mut() {
            let Some(token) = event.observation().rns_attempt_token() else {
                continue;
            };
            let Some(correlation) = token_correlations.get(&token).copied() else {
                continue;
            };
            match event.correlation {
                None => {
                    event.correlation = Some(correlation);
                    correlations_added += 1;
                }
                Some(existing) if existing != correlation => {
                    return Err(ChatStoreError::RfTraceAttemptTokenConflict(token));
                }
                Some(_) => {}
            }
        }

        let has_sequence_gap = {
            let mut expected = 1_u64;
            let mut gap = false;
            for (candidate_boot, sequence) in self.rf_trace_keys.keys() {
                if *candidate_boot != boot_id {
                    continue;
                }
                if sequence.get() != expected {
                    gap = true;
                    break;
                }
                let Some(next) = expected.checked_add(1) else {
                    break;
                };
                expected = next;
            }
            gap
        };
        let boot = self
            .rf_trace_boots
            .entry(boot_id)
            .or_insert(MemoryRfTraceBoot {
                profile,
                history_incomplete: false,
            });
        boot.history_incomplete |= batch.history_incomplete() || has_sequence_gap;
        Ok(RfTraceImportOutcome::new(
            inserted,
            existing_count,
            correlations_added,
        ))
    }

    /// Common terminal-row replacement mutation.
    fn rearm_outbox(
        &mut self,
        outbox_id: OutboxId,
        idempotency_key: IdempotencyKey,
        location: AttemptLocationStamp,
    ) -> Result<RetryMutation, ChatStoreError> {
        let record = self
            .outbox
            .get(&outbox_id)
            .ok_or(ChatStoreError::OutboxNotFound(outbox_id))?;
        match record.status() {
            OutboxStatus::Committed
            | OutboxStatus::Accepted
            | OutboxStatus::Device(
                SubmissionState::Queued
                | SubmissionState::Preparing
                | SubmissionState::AwaitingDelivery(_),
            ) => return Ok(RetryMutation::AlreadyPending),
            OutboxStatus::Device(SubmissionState::Failed(failure)) if failure.is_retryable() => {}
            OutboxStatus::Device(
                SubmissionState::Delivered(_)
                | SubmissionState::ApplicationDelivered
                | SubmissionState::Failed(_)
                | SubmissionState::Cancelled,
            ) => return Err(ChatStoreError::OutboxNotRetryable(outbox_id)),
        }
        let old_key = record.material().idempotency_key();
        if old_key == idempotency_key {
            return Err(ChatStoreError::RetryIdempotencyKeyUnchanged(outbox_id));
        }
        if self
            .idempotency_index
            .get(&idempotency_key)
            .is_some_and(|existing| *existing != outbox_id)
        {
            return Err(ChatStoreError::IdempotencyConflict);
        }
        let next_attempt = record
            .current_attempt()
            .checked_next()
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        let (activity_id, next_activity_id) = self.reserve_activity_id()?;
        let sequence = record.sequence();
        let peer = record.material().destination();
        let acceptance = record
            .acceptance()
            .expect("a device terminal status always has acceptance identifiers");
        self.idempotency_index.remove(&old_key);
        self.submission_index.remove(&acceptance.submission_id());
        self.accepted_message_index.remove(&acceptance.message_id());
        let record = self
            .outbox
            .get_mut(&outbox_id)
            .expect("outbox existence was checked");
        record.material.replace_idempotency_key(idempotency_key);
        record.acceptance = None;
        record.status = OutboxStatus::Committed;
        record.current_attempt = next_attempt;
        self.idempotency_index.insert(idempotency_key, outbox_id);
        self.append_activity(
            MessageActivityEvent {
                id: activity_id,
                observed_at_unix_ms: observed_unix_ms(),
                timeline_sequence: sequence,
                peer,
                direction: TimelineDirection::Outbound,
                outbox_id: Some(outbox_id),
                attempt_number: Some(next_attempt),
                ingress_observation: None,
                message_location: None,
                receiver_location: None,
                kind: MessageActivityKind::OutboundRequeued { location },
            },
            next_activity_id,
        );
        Ok(RetryMutation::Requeued)
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

    fn conversation_peers(&self) -> Result<Vec<ConversationPeer>, Self::Error> {
        Ok(project_conversation_peers(
            self.contacts.values(),
            self.inbound.values(),
            self.outbox.values(),
        ))
    }

    fn contains_inbound(&self, message_id: MessageId) -> Result<bool, Self::Error> {
        Ok(self.inbound.contains_key(&message_id))
    }

    fn commit_inbound_with_receiver_location(
        &mut self,
        message: InboundMessage,
        receiver_location: Option<crate::PhoneLocationSample>,
    ) -> Result<InboundCommitOutcome, Self::Error> {
        let message_id = message.message_id();
        if let Some(existing) = self.inbound.get_mut(&message_id) {
            if existing.message().same_authenticated_message(&message) {
                existing
                    .message
                    .retain_ingress_if_absent(message.ingress_observation());
                existing
                    .message
                    .retain_location_if_absent(message.location());
                return Ok(InboundCommitOutcome::Duplicate);
            }
            return Err(ChatStoreError::InboundMessageIdConflict(message_id));
        }
        let (activity_id, next_activity_id) = self.reserve_activity_id()?;
        let sequence = self.allocate_sequence()?;
        let peer = message.source();
        self.inbound.insert(
            message_id,
            InboundRecord {
                sequence,
                message,
                receiver_location,
            },
        );
        self.append_activity(
            MessageActivityEvent {
                id: activity_id,
                observed_at_unix_ms: observed_unix_ms(),
                timeline_sequence: sequence,
                peer,
                direction: TimelineDirection::Inbound,
                outbox_id: None,
                attempt_number: None,
                ingress_observation: None,
                message_location: None,
                receiver_location: None,
                kind: MessageActivityKind::InboundImported { message_id },
            },
            next_activity_id,
        );
        Ok(InboundCommitOutcome::Inserted)
    }

    fn commit_outbound_with_location(
        &mut self,
        material: OutboxMaterial,
        location: AttemptLocationStamp,
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
        let (activity_id, next_activity_id) = self.reserve_activity_id()?;
        let (id, sequence) = self.allocate_outbox_and_sequence()?;
        let key = material.idempotency_key();
        let peer = material.destination();
        let current_attempt = MessageAttemptNumber::first();
        self.outbox.insert(
            id,
            OutboxRecord {
                id,
                sequence,
                material,
                acceptance: None,
                status: OutboxStatus::Committed,
                current_attempt,
            },
        );
        self.idempotency_index.insert(key, id);
        self.append_activity(
            MessageActivityEvent {
                id: activity_id,
                observed_at_unix_ms: observed_unix_ms(),
                timeline_sequence: sequence,
                peer,
                direction: TimelineDirection::Outbound,
                outbox_id: Some(id),
                attempt_number: Some(current_attempt),
                ingress_observation: None,
                message_location: None,
                receiver_location: None,
                kind: MessageActivityKind::OutboundQueued { location },
            },
            next_activity_id,
        );
        Ok(OutboxCommitOutcome::Inserted(id))
    }

    fn retry_outbox_with_location(
        &mut self,
        outbox_id: OutboxId,
        idempotency_key: IdempotencyKey,
        location: AttemptLocationStamp,
    ) -> Result<OutboxRetryOutcome, Self::Error> {
        match self.rearm_outbox(outbox_id, idempotency_key, location)? {
            RetryMutation::Requeued => Ok(OutboxRetryOutcome::Requeued(outbox_id)),
            RetryMutation::AlreadyPending => Ok(OutboxRetryOutcome::AlreadyPending(outbox_id)),
        }
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
        let (activity_id, next_activity_id) = self.reserve_activity_id()?;
        let record = self
            .outbox
            .get(&outbox_id)
            .expect("outbox existence was checked");
        let sequence = record.sequence();
        let peer = record.material().destination();
        let attempt_number = record.current_attempt();
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
        self.append_activity(
            MessageActivityEvent {
                id: activity_id,
                observed_at_unix_ms: observed_unix_ms(),
                timeline_sequence: sequence,
                peer,
                direction: TimelineDirection::Outbound,
                outbox_id: Some(outbox_id),
                attempt_number: Some(attempt_number),
                ingress_observation: None,
                message_location: None,
                receiver_location: None,
                kind: MessageActivityKind::OutboundAccepted { acceptance },
            },
            next_activity_id,
        );
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
            .get(&outbox_id)
            .expect("submission index must reference an outbox row");
        if matches!(record.status, OutboxStatus::Committed) {
            return Err(ChatStoreError::SubmissionNotFound(submission_id));
        }
        let (status, outcome) = project_outbox_status(record.status, state)?;
        if outcome == StatusProjectionOutcome::Advanced {
            let (activity_id, next_activity_id) = self.reserve_activity_id()?;
            let sequence = record.sequence();
            let peer = record.material().destination();
            let attempt_number = record.current_attempt();
            self.outbox
                .get_mut(&outbox_id)
                .expect("submission index must reference an outbox row")
                .status = status;
            self.append_activity(
                MessageActivityEvent {
                    id: activity_id,
                    observed_at_unix_ms: observed_unix_ms(),
                    timeline_sequence: sequence,
                    peer,
                    direction: TimelineDirection::Outbound,
                    outbox_id: Some(outbox_id),
                    attempt_number: Some(attempt_number),
                    ingress_observation: None,
                    message_location: None,
                    receiver_location: None,
                    kind: MessageActivityKind::OutboundStatus { state },
                },
                next_activity_id,
            );
        }
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

    fn message_activity(
        &self,
        request: MessageActivityPageRequest,
    ) -> Result<MessageActivityPage, Self::Error> {
        let mut events: Vec<_> = self
            .activity
            .iter()
            .rev()
            .filter(|event| {
                request.before().is_none_or(|before| event.id() < before)
                    && match request.scope() {
                        MessageActivityScope::All => true,
                        MessageActivityScope::Timeline(sequence) => {
                            event.timeline_sequence() == sequence
                        }
                    }
            })
            .take(request.limit() + 1)
            .copied()
            .map(|event| {
                let inbound = match event.kind() {
                    MessageActivityKind::InboundImported { message_id } => {
                        self.inbound.get(&message_id)
                    }
                    MessageActivityKind::OutboundQueued { .. }
                    | MessageActivityKind::OutboundAccepted { .. }
                    | MessageActivityKind::OutboundStatus { .. }
                    | MessageActivityKind::OutboundRequeued { .. } => None,
                };
                event
                    .with_ingress_observation(
                        inbound.and_then(|record| record.message().ingress_observation()),
                    )
                    .with_inbound_locations(
                        inbound.and_then(|record| record.message().location()),
                        inbound.and_then(InboundRecord::receiver_location),
                    )
            })
            .collect();
        let has_more = events.len() > request.limit();
        events.truncate(request.limit());
        let next_before = has_more.then(|| {
            events
                .last()
                .expect("a non-empty bounded page has a last event")
                .id()
        });
        Ok(MessageActivityPage {
            events,
            next_before,
            history_incomplete: self.activity_history_incomplete,
        })
    }

    fn import_rf_trace_batch(
        &mut self,
        batch: RfTraceImportBatch,
    ) -> Result<RfTraceImportOutcome, Self::Error> {
        let mut staged = self.clone();
        let outcome = staged.import_rf_trace_batch_inner(batch)?;
        *self = staged;
        Ok(outcome)
    }

    fn rf_trace(&self, request: RfTracePageRequest) -> Result<RfTracePage, Self::Error> {
        let mut events: Vec<_> = self
            .rf_trace_events
            .values()
            .rev()
            .filter(|event| {
                request.before().is_none_or(|before| event.id() < before)
                    && match request.scope() {
                        RfTraceScope::All => true,
                        RfTraceScope::Timeline(sequence) => event
                            .message_correlation()
                            .is_some_and(|correlation| correlation.timeline_sequence() == sequence),
                    }
            })
            .take(request.limit() + 1)
            .copied()
            .collect();
        let has_more = events.len() > request.limit();
        events.truncate(request.limit());
        let next_before = has_more.then(|| {
            events
                .last()
                .expect("a non-empty bounded RF trace page has a last event")
                .id()
        });
        Ok(RfTracePage {
            events,
            next_before,
            history_incomplete: self
                .rf_trace_boots
                .values()
                .any(|boot| boot.history_incomplete),
        })
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
