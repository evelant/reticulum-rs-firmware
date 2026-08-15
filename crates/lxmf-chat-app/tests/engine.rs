use std::collections::BTreeMap;

use reticulum_lxmf_chat_app::{
    ChatEngine, EngineError, InboxCursor, InboxStep, InboxSummary, LxmfSession, ReconcileStep,
};
use reticulum_lxmf_chat_core::{
    AcceptanceIds, ChatStore, DestinationHash, DeviceBinding, IdempotencyKey, InboundMessage,
    MemoryChatStore, MessageId, OutboxMaterial, OutboxStatus, PhoneLocationAuthorization,
    PhoneLocationSample, PhoneLocationSource, SubmissionId, SubmissionState, UnixTimestampMillis,
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
    acknowledgements: Vec<InboxCursor>,
    fail_ack_once: bool,
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
    type Error = &'static str;

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

    fn inbox_status(&mut self) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        let latest = self.inbox.last().map(|(summary, _)| {
            reticulum_device_api::LxmfMessageHandle::new(summary.cursor().get()).unwrap()
        });
        let acknowledged = self
            .acknowledgements
            .last()
            .map(|cursor| reticulum_device_api::LxmfMessageHandle::new(cursor.get()).unwrap());
        Ok(reticulum_device_api::LxmfMailboxStatus::new(latest, acknowledged).unwrap())
    }

    fn acknowledge_inbox_through(
        &mut self,
        through: InboxCursor,
    ) -> Result<reticulum_device_api::LxmfMailboxStatus, Self::Error> {
        if self.fail_ack_once {
            self.fail_ack_once = false;
            return Err("transient acknowledgement failure");
        }
        self.acknowledgements.push(through);
        self.inbox_status()
    }

    fn next_nearby_peer(
        &mut self,
        _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
    ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
        unreachable!("the chat engine does not perform nearby-peer discovery")
    }

    fn nomad_fetch_start(
        &mut self,
        _request: reticulum_device_api::NomadFetchStartRequest<'_>,
    ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
        unreachable!("the chat engine does not perform NomadNet fetches")
    }

    fn nomad_fetch_poll(
        &mut self,
        _id: reticulum_device_api::NomadFetchId,
    ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
        unreachable!("the chat engine does not perform NomadNet fetches")
    }

    fn reticulum_probe_start(
        &mut self,
        _request: reticulum_device_api::ProbeStartRequest,
    ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
        unreachable!("the chat engine does not perform Reticulum probes")
    }

    fn reticulum_probe_poll(
        &mut self,
        _id: reticulum_device_api::ProbeId,
    ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
        unreachable!("the chat engine does not perform Reticulum probes")
    }

    fn network_config_get(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
        unreachable!("the chat engine does not manage network configuration")
    }

    fn network_config_mutate(
        &mut self,
        _request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
    ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
        unreachable!("the chat engine does not manage network configuration")
    }

    fn network_status(
        &mut self,
    ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
        unreachable!("the chat engine does not read network status")
    }

    fn manual_service_announce(
        &mut self,
    ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
        unreachable!("the chat engine does not request service announces")
    }

    fn node_diagnostics(
        &mut self,
    ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
        unreachable!("the chat engine does not request node diagnostics")
    }

    fn route_diagnostics_page(
        &mut self,
        _request: reticulum_device_api::RouteDiagnosticsRequest,
    ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
        unreachable!("the chat engine does not request route diagnostics")
    }

    fn radio_trace_page(
        &mut self,
        _request: reticulum_device_api::RadioTracePageRequest,
    ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
        unreachable!("the chat engine does not request radio traces")
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
fn exact_new_outbox_can_precede_unrelated_status_refreshes() {
    let mut engine = ChatEngine::new(MemoryChatStore::new());
    let first = engine.enqueue_send(material(1)).unwrap().outbox_id();
    let second = engine.enqueue_send(material(2)).unwrap().outbox_id();
    let mut session = FakeSession {
        next_submission: 1,
        ..FakeSession::default()
    };

    assert!(matches!(
        engine.reconcile_step(&mut session).unwrap(),
        ReconcileStep::Submitted { outbox_id, .. } if outbox_id == first
    ));
    assert!(matches!(
        engine.reconcile_step(&mut session).unwrap(),
        ReconcileStep::Submitted { outbox_id, .. } if outbox_id == second
    ));

    let urgent = engine.enqueue_send(material(3)).unwrap().outbox_id();
    assert!(matches!(
        engine
            .reconcile_outbox_step(&mut session, urgent)
            .unwrap(),
        ReconcileStep::Submitted { outbox_id, .. } if outbox_id == urgent
    ));
    assert_eq!(
        session.submitted,
        vec![material(1), material(2), material(3)]
    );
}

#[test]
fn exact_priority_does_not_reset_the_ordinary_fairness_cursor() {
    let mut engine = ChatEngine::new(MemoryChatStore::new());
    let first = engine.enqueue_send(material(1)).unwrap().outbox_id();
    let second = engine.enqueue_send(material(2)).unwrap().outbox_id();
    let urgent = engine.enqueue_send(material(3)).unwrap().outbox_id();
    let mut session = FakeSession {
        next_submission: 1,
        ..FakeSession::default()
    };

    assert!(matches!(
        engine.reconcile_step(&mut session).unwrap(),
        ReconcileStep::Submitted { outbox_id, .. } if outbox_id == first
    ));
    assert!(matches!(
        engine
            .reconcile_outbox_step(&mut session, urgent)
            .unwrap(),
        ReconcileStep::Submitted { outbox_id, .. } if outbox_id == urgent
    ));
    assert!(matches!(
        engine
            .reconcile_step_avoiding(&mut session, urgent)
            .unwrap(),
        ReconcileStep::Submitted { outbox_id, .. } if outbox_id == second
    ));
}

#[test]
fn avoiding_a_hot_row_falls_back_to_it_only_when_no_other_work_exists() {
    let mut engine = ChatEngine::new(MemoryChatStore::new());
    let hot = engine.enqueue_send(material(1)).unwrap().outbox_id();
    let other = engine.enqueue_send(material(2)).unwrap().outbox_id();
    let mut session = FakeSession {
        next_submission: 1,
        ..FakeSession::default()
    };

    assert!(matches!(
        engine
            .reconcile_step_avoiding(&mut session, hot)
            .unwrap(),
        ReconcileStep::Submitted { outbox_id, .. } if outbox_id == other
    ));
    session
        .statuses
        .insert(SubmissionId::new(1).unwrap(), SubmissionState::Cancelled);
    assert!(matches!(
        engine
            .reconcile_step_avoiding(&mut session, hot)
            .unwrap(),
        ReconcileStep::Refreshed { outbox_id, .. } if outbox_id == other
    ));
    assert!(matches!(
        engine
            .reconcile_step_avoiding(&mut session, hot)
            .unwrap(),
        ReconcileStep::Submitted { outbox_id, .. } if outbox_id == hot
    ));
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
        InboxStep::Acknowledged {
            cursor: second.0.cursor()
        }
    );
    assert_eq!(session.acknowledgements, vec![second.0.cursor()]);
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
fn inbox_import_atomically_retains_the_receiver_phone_fix() {
    let item = inbound(1, 0x23);
    let receiver = PhoneLocationSample::new(
        42_357_111,
        -71_061_924,
        Some(8_250),
        1_785_084_000_999,
        PhoneLocationAuthorization::Precise,
        PhoneLocationSource::ForegroundStream,
        Some(false),
    )
    .unwrap()
    .with_altitude(Some(17_234), Some(12_500));
    let mut engine = ChatEngine::new(MemoryChatStore::new());
    let mut session = FakeSession::with_inbox(vec![item]);

    assert!(matches!(
        engine
            .inbox_step_with_receiver_location(&mut session, Some(receiver))
            .unwrap(),
        InboxStep::Imported { .. }
    ));
    let timeline = engine
        .store()
        .conversation_timeline(destination(0x23))
        .unwrap();
    assert_eq!(timeline[0].receiver_location(), Some(receiver));
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

#[test]
fn inbox_acknowledgement_batches_and_retries_after_local_commit() {
    let first = inbound(1, 0x41);
    let second = inbound(2, 0x42);
    let mut engine = ChatEngine::new(MemoryChatStore::new());
    let mut session = FakeSession::with_inbox(vec![first, second.clone()]);

    assert!(matches!(
        engine.inbox_step(&mut session).unwrap(),
        InboxStep::Imported { .. }
    ));
    assert!(matches!(
        engine.inbox_step(&mut session).unwrap(),
        InboxStep::Imported { .. }
    ));
    assert_eq!(
        engine.pending_inbox_acknowledgement(),
        Some(second.0.cursor())
    );

    session.fail_ack_once = true;
    assert!(matches!(
        engine.inbox_step(&mut session),
        Err(EngineError::Session("transient acknowledgement failure"))
    ));
    assert_eq!(
        engine.pending_inbox_acknowledgement(),
        Some(second.0.cursor())
    );
    assert!(session.acknowledgements.is_empty());

    assert_eq!(
        engine.inbox_step(&mut session).unwrap(),
        InboxStep::Acknowledged {
            cursor: second.0.cursor()
        }
    );
    assert_eq!(session.acknowledgements, vec![second.0.cursor()]);
    assert_eq!(engine.pending_inbox_acknowledgement(), None);
}
