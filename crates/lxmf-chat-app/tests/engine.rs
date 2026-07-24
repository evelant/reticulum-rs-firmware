use std::collections::BTreeMap;
use std::convert::Infallible;

use reticulum_lxmf_chat_app::{
    ChatEngine, EngineError, InboxCursor, InboxStep, InboxSummary, LxmfSession, ReconcileStep,
};
use reticulum_lxmf_chat_core::{
    AcceptanceIds, ChatStore, DestinationHash, DeviceBinding, IdempotencyKey, InboundMessage,
    MemoryChatStore, MessageId, OutboxMaterial, OutboxStatus, SubmissionId, SubmissionState,
    UnixTimestampMillis,
};

fn destination(tag: u8) -> DestinationHash {
    DestinationHash::new([tag; 16])
}

fn material(tag: u8) -> OutboxMaterial {
    OutboxMaterial::new(
        destination(tag),
        UnixTimestampMillis::new(1_000 + u64::from(tag)).unwrap(),
        IdempotencyKey::new([tag; 16]),
        b"title".to_vec(),
        vec![tag],
    )
}

fn inbound(handle: u64, tag: u8) -> (InboxSummary, InboundMessage) {
    let message_id = MessageId::new([tag; 32]);
    (
        InboxSummary::new(InboxCursor::new(handle).unwrap(), message_id),
        InboundMessage::new(
            message_id,
            destination(0xa0),
            destination(tag),
            UnixTimestampMillis::new(2_000 + u64::from(tag)).unwrap(),
            b"inbound".to_vec(),
            vec![tag],
        ),
    )
}

#[derive(Default)]
struct FakeSession {
    submitted: Vec<OutboxMaterial>,
    statuses: BTreeMap<SubmissionId, SubmissionState>,
    inbox: Vec<(InboxSummary, InboundMessage)>,
    reads: usize,
    next_submission: u64,
    mismatched_read: bool,
}

impl FakeSession {
    fn with_inbox(inbox: Vec<(InboxSummary, InboundMessage)>) -> Self {
        Self {
            inbox,
            next_submission: 1,
            ..Self::default()
        }
    }
}

impl LxmfSession for FakeSession {
    type Error = Infallible;

    fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
        Ok(DeviceBinding::new(
            [0x11; 16],
            destination(0x12),
            destination(0x13),
        ))
    }

    fn submit(&mut self, material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
        self.submitted.push(material.clone());
        let id = SubmissionId::new(self.next_submission.max(1)).unwrap();
        self.next_submission = id.get() + 1;
        self.statuses.insert(id, SubmissionState::Queued);
        Ok(AcceptanceIds::new(id, MessageId::new([id.get() as u8; 32])))
    }

    fn submission_status(&mut self, id: SubmissionId) -> Result<SubmissionState, Self::Error> {
        Ok(*self.statuses.get(&id).unwrap())
    }

    fn next_inbox(
        &mut self,
        after: Option<InboxCursor>,
    ) -> Result<Option<InboxSummary>, Self::Error> {
        let after = after.map_or(0, InboxCursor::get);
        Ok(self
            .inbox
            .iter()
            .map(|(summary, _)| *summary)
            .find(|summary| summary.cursor().get() > after))
    }

    fn read_inbox(&mut self, summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
        self.reads += 1;
        let mut message = self
            .inbox
            .iter()
            .find(|(candidate, _)| *candidate == summary)
            .unwrap()
            .1
            .clone();
        if self.mismatched_read {
            let (_, replacement) = inbound(99, 0xee);
            message = replacement;
        }
        Ok(message)
    }

    fn next_nearby_peer(
        &mut self,
        _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
        unreachable!("the chat engine does not perform nearby-peer discovery")
    }

    fn is_usable(&self) -> bool {
        true
    }
}

#[test]
fn offline_commits_are_submitted_fairly_and_status_is_projected() {
    let mut engine = ChatEngine::new(MemoryChatStore::new());
    let first = engine.enqueue_send(material(1)).unwrap().outbox_id();
    let second = engine.enqueue_send(material(2)).unwrap().outbox_id();
    let mut session = FakeSession {
        next_submission: 1,
        ..FakeSession::default()
    };

    let ReconcileStep::Submitted {
        outbox_id: first_submitted,
        acceptance: first_acceptance,
        ..
    } = engine.reconcile_step(&mut session).unwrap()
    else {
        panic!("expected first submission");
    };
    assert_eq!(first_submitted, first);

    let ReconcileStep::Submitted {
        outbox_id: second_submitted,
        ..
    } = engine.reconcile_step(&mut session).unwrap()
    else {
        panic!("expected round-robin second submission");
    };
    assert_eq!(second_submitted, second);
    assert_eq!(session.submitted, vec![material(1), material(2)]);

    session
        .statuses
        .insert(first_acceptance.submission_id(), SubmissionState::Preparing);
    let ReconcileStep::Refreshed {
        outbox_id, state, ..
    } = engine.reconcile_step(&mut session).unwrap()
    else {
        panic!("expected a status refresh");
    };
    assert_eq!(outbox_id, first);
    assert_eq!(state, SubmissionState::Preparing);
    assert_eq!(
        engine.store().outbox(first).unwrap().unwrap().status(),
        OutboxStatus::Device(SubmissionState::Preparing)
    );
}

#[test]
fn incremental_inbox_scan_skips_known_wire_and_rescans_safely() {
    let first = inbound(1, 0x21);
    let second = inbound(2, 0x22);
    let mut store = MemoryChatStore::new();
    store.commit_inbound(first.1.clone()).unwrap();
    let mut engine = ChatEngine::new(store);
    let mut session = FakeSession::with_inbox(vec![first.clone(), second.clone()]);

    assert!(matches!(
        engine.inbox_step(&mut session).unwrap(),
        InboxStep::AlreadyImported { message_id, .. } if message_id == first.0.message_id()
    ));
    assert_eq!(session.reads, 0);
    assert!(matches!(
        engine.inbox_step(&mut session).unwrap(),
        InboxStep::Imported { message_id, .. } if message_id == second.0.message_id()
    ));
    assert_eq!(session.reads, 1);
    assert_eq!(
        engine.inbox_step(&mut session).unwrap(),
        InboxStep::EndOfScan
    );

    engine.reset_session_scan();
    assert!(matches!(
        engine.inbox_step(&mut session).unwrap(),
        InboxStep::AlreadyImported { message_id, .. } if message_id == first.0.message_id()
    ));
    assert_eq!(session.reads, 1);
}

#[test]
fn inbox_cursor_advances_only_for_the_selected_message() {
    let item = inbound(1, 0x31);
    let mut engine = ChatEngine::new(MemoryChatStore::new());
    let mut session = FakeSession::with_inbox(vec![item]);
    session.mismatched_read = true;

    assert!(matches!(
        engine.inbox_step(&mut session),
        Err(EngineError::InboxMessageIdMismatch { .. })
    ));
    assert_eq!(
        engine
            .store()
            .conversation_timeline(destination(0xee))
            .unwrap(),
        vec![]
    );
}
