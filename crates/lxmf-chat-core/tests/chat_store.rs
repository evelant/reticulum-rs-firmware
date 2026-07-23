use reticulum_lxmf_chat_core::{
    AcceptanceIds, AcceptanceOutcome, ChatStore, ChatStoreError, Contact, ContactUpsertOutcome,
    DestinationHash, EncodedPacketSha256, IdempotencyKey, InboundCommitOutcome, InboundMessage,
    MemoryChatStore, MessageId, OutboxCommitOutcome, OutboxMaterial, OutboxStatus, PacketEvidence,
    ReconcileWork, StatusProjectionOutcome, SubmissionFailure, SubmissionId, SubmissionState,
    TimelineDirection, UnixTimestampMillis,
};

fn destination(tag: u8) -> DestinationHash {
    DestinationHash::new([tag; 16])
}

fn timestamp(value: u64) -> UnixTimestampMillis {
    UnixTimestampMillis::new(value).expect("test timestamp must be valid")
}

fn inbound(id: u8, source: u8, at: u64, content: &[u8]) -> InboundMessage {
    InboundMessage::new(
        MessageId::new([id; 32]),
        destination(0xa0),
        destination(source),
        timestamp(at),
        b"inbound".to_vec(),
        content.to_vec(),
    )
}

fn outbound(key: u8, destination_tag: u8, at: u64, content: &[u8]) -> OutboxMaterial {
    OutboxMaterial::new(
        destination(destination_tag),
        timestamp(at),
        IdempotencyKey::new([key; 16]),
        b"outbound".to_vec(),
        content.to_vec(),
    )
}

fn acceptance(submission: u64, message: u8) -> AcceptanceIds {
    AcceptanceIds::new(
        SubmissionId::new(submission).expect("test submission must be valid"),
        MessageId::new([message; 32]),
    )
}

fn evidence(tag: u8) -> PacketEvidence {
    PacketEvidence::new(211, EncodedPacketSha256::new([tag; 32]))
        .expect("test packet evidence must be valid")
}

#[test]
fn contacts_upsert_by_destination_without_duplicate_rows() {
    let mut store = MemoryChatStore::new();
    let peer = destination(1);

    assert_eq!(
        store.upsert_contact(Contact::new(peer, "Alice")).unwrap(),
        ContactUpsertOutcome::Inserted
    );
    assert_eq!(
        store.upsert_contact(Contact::new(peer, "Alice")).unwrap(),
        ContactUpsertOutcome::Unchanged
    );
    assert_eq!(
        store
            .upsert_contact(Contact::new(peer, "Alice (field)"))
            .unwrap(),
        ContactUpsertOutcome::Updated
    );
    assert_eq!(store.contacts().unwrap().len(), 1);
    assert_eq!(
        store.contact(peer).unwrap().unwrap().display_name(),
        "Alice (field)"
    );
}

#[test]
fn inbound_message_id_is_idempotent_only_for_exact_semantics() {
    let mut store = MemoryChatStore::new();
    let message = inbound(7, 2, 1_000, b"hello");

    assert!(!store.contains_inbound(message.message_id()).unwrap());

    assert_eq!(
        store.commit_inbound(message.clone()).unwrap(),
        InboundCommitOutcome::Inserted
    );
    assert!(store.contains_inbound(message.message_id()).unwrap());
    assert_eq!(
        store.commit_inbound(message).unwrap(),
        InboundCommitOutcome::Duplicate
    );
    assert_eq!(
        store
            .commit_inbound(inbound(7, 2, 1_000, b"different"))
            .unwrap_err(),
        ChatStoreError::InboundMessageIdConflict(MessageId::new([7; 32]))
    );
    assert_eq!(
        store.conversation_timeline(destination(2)).unwrap().len(),
        1
    );
}

#[test]
fn outbox_commit_precedes_acceptance_and_retries_exactly() {
    let mut store = MemoryChatStore::new();
    let material = outbound(3, 2, 2_000, b"send me");
    let first = store.commit_outbound(material.clone()).unwrap();
    let id = first.outbox_id();

    assert!(matches!(first, OutboxCommitOutcome::Inserted(_)));
    assert_eq!(
        store.commit_outbound(material.clone()).unwrap(),
        OutboxCommitOutcome::Existing(id)
    );
    assert_eq!(
        store
            .commit_outbound(outbound(3, 2, 2_000, b"changed"))
            .unwrap_err(),
        ChatStoreError::IdempotencyConflict
    );
    assert_eq!(
        store.reconcile().unwrap(),
        vec![ReconcileWork::Submit {
            outbox_id: id,
            material
        }]
    );

    let accepted = acceptance(11, 0x31);
    assert_eq!(
        store.record_acceptance(id, accepted).unwrap(),
        AcceptanceOutcome::Recorded
    );
    assert_eq!(
        store.record_acceptance(id, accepted).unwrap(),
        AcceptanceOutcome::Unchanged
    );
    assert_eq!(
        store.outbox(id).unwrap().unwrap().acceptance(),
        Some(accepted)
    );
    assert_eq!(
        store.reconcile().unwrap(),
        vec![ReconcileWork::RefreshStatus {
            outbox_id: id,
            acceptance: accepted
        }]
    );
}

#[test]
fn status_projection_never_regresses_or_changes_packet_evidence() {
    let mut store = MemoryChatStore::new();
    let id = store
        .commit_outbound(outbound(4, 2, 3_000, b"status"))
        .unwrap()
        .outbox_id();
    let accepted = acceptance(12, 0x32);
    store.record_acceptance(id, accepted).unwrap();

    assert_eq!(
        store
            .project_submission_status(accepted.submission_id(), SubmissionState::Preparing)
            .unwrap(),
        StatusProjectionOutcome::Advanced
    );
    assert_eq!(
        store
            .project_submission_status(accepted.submission_id(), SubmissionState::Queued)
            .unwrap(),
        StatusProjectionOutcome::IgnoredStale
    );
    assert_eq!(
        store
            .project_submission_status(
                accepted.submission_id(),
                SubmissionState::AwaitingDelivery(evidence(9)),
            )
            .unwrap(),
        StatusProjectionOutcome::Advanced
    );
    assert_eq!(
        store
            .project_submission_status(
                accepted.submission_id(),
                SubmissionState::Delivered(evidence(8)),
            )
            .unwrap_err(),
        ChatStoreError::PacketEvidenceChanged
    );
    assert_eq!(
        store
            .project_submission_status(
                accepted.submission_id(),
                SubmissionState::Delivered(evidence(9)),
            )
            .unwrap(),
        StatusProjectionOutcome::Advanced
    );
    assert_eq!(
        store
            .project_submission_status(
                accepted.submission_id(),
                SubmissionState::Failed(SubmissionFailure::Internal),
            )
            .unwrap_err(),
        ChatStoreError::TerminalStatusConflict
    );
    assert_eq!(store.reconcile().unwrap(), Vec::new());
}

#[test]
fn conversation_timeline_orders_by_timestamp_then_stable_sequence() {
    let mut store = MemoryChatStore::new();
    let peer = destination(5);
    let first_outbound = store
        .commit_outbound(outbound(5, 5, 2_000, b"later"))
        .unwrap()
        .outbox_id();
    store
        .commit_inbound(inbound(0x41, 5, 1_000, b"earlier"))
        .unwrap();
    store
        .commit_inbound(inbound(0x42, 5, 2_000, b"same-time-second"))
        .unwrap();

    let timeline = store.conversation_timeline(peer).unwrap();
    assert_eq!(timeline.len(), 3);
    assert_eq!(timeline[0].content(), b"earlier");
    assert_eq!(timeline[0].direction(), TimelineDirection::Inbound);
    assert_eq!(timeline[1].content(), b"later");
    assert_eq!(timeline[1].outbox_id(), Some(first_outbound));
    assert_eq!(timeline[1].outbox_status(), Some(OutboxStatus::Committed));
    assert_eq!(timeline[2].content(), b"same-time-second");
}

#[test]
fn restart_rebuilds_indexes_and_returns_only_unfinished_work() {
    let mut store = MemoryChatStore::new();
    let unsubmitted = store
        .commit_outbound(outbound(6, 2, 4_000, b"submit after restart"))
        .unwrap()
        .outbox_id();
    let refresh = store
        .commit_outbound(outbound(7, 2, 5_000, b"refresh after restart"))
        .unwrap()
        .outbox_id();
    let terminal = store
        .commit_outbound(outbound(8, 2, 6_000, b"already done"))
        .unwrap()
        .outbox_id();
    let refresh_acceptance = acceptance(21, 0x51);
    let terminal_acceptance = acceptance(22, 0x52);
    store
        .record_acceptance(refresh, refresh_acceptance)
        .unwrap();
    store
        .record_acceptance(terminal, terminal_acceptance)
        .unwrap();
    store
        .project_submission_status(
            terminal_acceptance.submission_id(),
            SubmissionState::Delivered(evidence(0x61)),
        )
        .unwrap();
    store
        .commit_inbound(inbound(0x71, 2, 7_000, b"persisted"))
        .unwrap();

    let reopened = MemoryChatStore::open(store.image()).unwrap();
    let work = reopened.reconcile().unwrap();
    assert_eq!(work.len(), 2);
    assert!(matches!(
        &work[0],
        ReconcileWork::Submit { outbox_id, .. } if *outbox_id == unsubmitted
    ));
    assert_eq!(
        work[1],
        ReconcileWork::RefreshStatus {
            outbox_id: refresh,
            acceptance: refresh_acceptance,
        }
    );
    assert_eq!(
        reopened
            .conversation_timeline(destination(2))
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn acceptance_identifiers_cannot_alias_another_outbox_record() {
    let mut store = MemoryChatStore::new();
    let first = store
        .commit_outbound(outbound(9, 2, 8_000, b"first"))
        .unwrap()
        .outbox_id();
    let second = store
        .commit_outbound(outbound(10, 2, 9_000, b"second"))
        .unwrap()
        .outbox_id();
    store
        .record_acceptance(first, acceptance(31, 0x71))
        .unwrap();

    assert_eq!(
        store
            .record_acceptance(second, acceptance(31, 0x72))
            .unwrap_err(),
        ChatStoreError::AcceptanceIdAlreadyBound
    );
    assert_eq!(
        store
            .record_acceptance(second, acceptance(32, 0x71))
            .unwrap_err(),
        ChatStoreError::AcceptanceIdAlreadyBound
    );
}
