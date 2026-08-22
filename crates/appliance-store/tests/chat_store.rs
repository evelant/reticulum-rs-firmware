use reticulum_appliance_store::{
    AcceptanceIds, AcceptanceOutcome, AttemptLocationStamp, ChatStore, ChatStoreError, Contact,
    ContactUpsertOutcome, DestinationHash, EncodedPacketSha256, IdempotencyKey,
    InboundCommitOutcome, InboundMessage, MAX_MESSAGE_ACTIVITY_EVENTS, MemoryChatStore,
    MessageActivityKind, MessageActivityPageRequest, MessageActivityScope, MessageId,
    MessageIngressObservation, MessageInterfaceId, MessageLocation, MessageSignalObservation,
    OutboxCommitOutcome, OutboxMaterial, OutboxRetryOutcome, OutboxStatus, PacketEvidence,
    PhoneLocationAuthorization, PhoneLocationSample, PhoneLocationSource,
    PhoneLocationUnavailableReason, ReconcileWork, RfTraceAttemptObservation,
    RfTraceAttemptOutcome, RfTraceBootId, RfTraceEventSequence, RfTraceImportBatch,
    RfTraceInterfaceId, RfTraceObservation, RfTraceObservationKind, RfTracePageRequest,
    RfTraceRadioProfile, RfTraceRouteObservation, RfTraceRouteResolution, RfTraceRxObservation,
    RfTraceScope, RfTraceTxObservation, RfTraceTxOutcome, RnsAttemptToken, StatusProjectionOutcome,
    SubmissionFailure, SubmissionId, SubmissionState, TimelineDirection, UnixTimestampMillis,
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

fn ingress(interface: u8, rssi_dbm: i16, snr_db: i16) -> MessageIngressObservation {
    MessageIngressObservation::new(
        MessageInterfaceId::new([0, 0, 0, 0, 0, 0, 0, interface]),
        Some(MessageSignalObservation::new(rssi_dbm, snr_db)),
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

fn phone_location(latitude_e6: i32, captured_at_unix_ms: u64) -> PhoneLocationSample {
    PhoneLocationSample::new(
        latitude_e6,
        -72_345_678,
        Some(4_250),
        captured_at_unix_ms,
        PhoneLocationAuthorization::Precise,
        PhoneLocationSource::ForegroundStream,
        Some(false),
    )
    .expect("test phone location must be valid")
    .with_altitude(Some(123_456), Some(8_500))
}

fn location(latitude_e6: i32, captured_at_unix_ms: u64) -> AttemptLocationStamp {
    AttemptLocationStamp::Available(phone_location(latitude_e6, captured_at_unix_ms))
}

fn message_location(latitude_e6: i32, updated_at_unix_seconds: u32) -> MessageLocation {
    MessageLocation::new(
        latitude_e6,
        -72_345_678,
        12_345,
        678,
        12_300,
        425,
        updated_at_unix_seconds,
    )
    .expect("test message location must be valid")
}

fn rf_profile(tag: u8) -> RfTraceRadioProfile {
    RfTraceRadioProfile::new(
        [tag; 16],
        915_000_000,
        125_000,
        8,
        22,
        10,
        5,
        true,
        true,
        false,
    )
    .unwrap()
}

fn rf_route(sequence: u64, token: u8, submission: u64) -> RfTraceObservation {
    RfTraceObservation::new(
        RfTraceEventSequence::new(sequence).unwrap(),
        sequence * 1_000,
        RfTraceObservationKind::RouteSelected(RfTraceRouteObservation::new(
            destination(0x91),
            None,
            0,
            RfTraceInterfaceId::new([0, 0, 0, 0, 0, 0, 0, 1]),
            RfTraceRouteResolution::BroadcastReady,
            evidence(0x55),
            RnsAttemptToken::new([token; 32]),
            SubmissionId::new(submission).unwrap(),
        )),
    )
}

fn rf_tx(sequence: u64, token: u8) -> RfTraceObservation {
    RfTraceObservation::new(
        RfTraceEventSequence::new(sequence).unwrap(),
        sequence * 1_000,
        RfTraceObservationKind::DataTx(
            RfTraceTxObservation::new(
                RnsAttemptToken::new([token; 32]),
                RfTraceInterfaceId::new([0, 0, 0, 0, 0, 0, 0, 1]),
                evidence(0x55),
                RfTraceTxOutcome::Transmitted,
                2,
                2,
                [Some(sequence * 1_000 - 200), Some(sequence * 1_000 - 100)],
                true,
                None,
            )
            .unwrap(),
        ),
    )
}

fn rf_terminal(sequence: u64, token: u8) -> RfTraceObservation {
    RfTraceObservation::new(
        RfTraceEventSequence::new(sequence).unwrap(),
        sequence * 1_000,
        RfTraceObservationKind::AttemptTerminal(RfTraceAttemptObservation::new(
            RnsAttemptToken::new([token; 32]),
            RfTraceAttemptOutcome::Delivered,
            None,
        )),
    )
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
fn unknown_inbound_peer_is_queryable_without_becoming_a_contact() {
    let mut store = MemoryChatStore::new();
    let peer = destination(2);
    store
        .commit_inbound(inbound(0x21, 2, 1_000, b"hello from an unknown sender"))
        .unwrap();

    assert!(store.contacts().unwrap().is_empty());
    let conversations = store.conversation_peers().unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].peer(), peer);
    assert_eq!(conversations[0].saved_name(), None);
    assert_eq!(conversations[0].message_count(), 1);
    assert_eq!(conversations[0].inbound_message_count(), 1);
    assert_eq!(
        conversations[0].last_message().unwrap().content(),
        b"hello from an unknown sender"
    );

    store
        .upsert_contact(Contact::new(peer, "Field node"))
        .unwrap();
    store
        .upsert_contact(Contact::new(peer, "Relay by the oak"))
        .unwrap();

    assert_eq!(store.contacts().unwrap().len(), 1);
    let conversations = store.conversation_peers().unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].saved_name(), Some("Relay by the oak"));
    assert_eq!(conversations[0].message_count(), 1);
    assert_eq!(conversations[0].inbound_message_count(), 1);
    assert_eq!(store.conversation_timeline(peer).unwrap().len(), 1);
}

#[test]
fn conversation_peer_union_orders_message_history_before_contact_only_peers() {
    let mut store = MemoryChatStore::new();
    store
        .upsert_contact(Contact::new(destination(1), "Contact only"))
        .unwrap();
    store
        .commit_inbound(inbound(0x31, 2, 2_000, b"older inbound"))
        .unwrap();
    store
        .commit_outbound(outbound(0x32, 3, 3_000, b"newest outbound"))
        .unwrap();

    let conversations = store.conversation_peers().unwrap();
    assert_eq!(
        conversations
            .iter()
            .map(|conversation| conversation.peer())
            .collect::<Vec<_>>(),
        vec![destination(3), destination(2), destination(1)]
    );
    assert_eq!(conversations[0].message_count(), 1);
    assert_eq!(conversations[0].inbound_message_count(), 0);
    assert_eq!(
        conversations[0].last_message().unwrap().direction(),
        TimelineDirection::Outbound
    );
    assert_eq!(conversations[2].message_count(), 0);
    assert!(conversations[2].last_message().is_none());
}

#[test]
fn conversation_peer_provenance_distinguishes_inbound_outbound_and_mixed_history() {
    let mut store = MemoryChatStore::new();
    store
        .commit_outbound(outbound(0x21, 1, 1_000, b"outbound only"))
        .unwrap();
    store
        .commit_inbound(inbound(0x22, 2, 2_000, b"inbound only"))
        .unwrap();
    store
        .commit_outbound(outbound(0x23, 3, 3_000, b"mixed outbound"))
        .unwrap();
    store
        .commit_inbound(inbound(0x24, 3, 4_000, b"mixed inbound"))
        .unwrap();

    let conversations = store.conversation_peers().unwrap();
    let outbound_only = conversations
        .iter()
        .find(|peer| peer.peer() == destination(1))
        .unwrap();
    let inbound_only = conversations
        .iter()
        .find(|peer| peer.peer() == destination(2))
        .unwrap();
    let mixed = conversations
        .iter()
        .find(|peer| peer.peer() == destination(3))
        .unwrap();
    assert_eq!(
        (
            outbound_only.message_count(),
            outbound_only.inbound_message_count(),
            outbound_only.saved_name(),
        ),
        (1, 0, None)
    );
    assert_eq!(
        (
            inbound_only.message_count(),
            inbound_only.inbound_message_count(),
            inbound_only.saved_name(),
        ),
        (1, 1, None)
    );
    assert_eq!(
        (
            mixed.message_count(),
            mixed.inbound_message_count(),
            mixed.saved_name(),
        ),
        (2, 1, None)
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
fn memory_inbound_duplicate_retains_first_available_ingress_observation_across_restart() {
    let mut store = MemoryChatStore::new();
    let first = ingress(1, -102, 8);
    let conflicting_duplicate = ingress(2, -71, 15);
    let message = inbound(0x17, 2, 1_100, b"first arrival").with_ingress_observation(Some(first));

    assert_eq!(
        store.commit_inbound(message.clone()).unwrap(),
        InboundCommitOutcome::Inserted
    );
    assert_eq!(
        store
            .commit_inbound(
                inbound(0x17, 2, 1_100, b"first arrival")
                    .with_ingress_observation(Some(conflicting_duplicate)),
            )
            .unwrap(),
        InboundCommitOutcome::Duplicate
    );
    assert_eq!(
        store.conversation_timeline(destination(2)).unwrap()[0].ingress_observation(),
        Some(first)
    );

    let mut reopened = MemoryChatStore::open(store.image()).unwrap();
    assert_eq!(
        reopened.conversation_timeline(destination(2)).unwrap()[0].ingress_observation(),
        Some(first)
    );

    let first_available = ingress(3, -117, -4);
    assert_eq!(
        reopened
            .commit_inbound(inbound(0x18, 2, 1_200, b"late evidence"))
            .unwrap(),
        InboundCommitOutcome::Inserted
    );
    assert_eq!(
        reopened
            .commit_inbound(
                inbound(0x18, 2, 1_200, b"late evidence")
                    .with_ingress_observation(Some(first_available)),
            )
            .unwrap(),
        InboundCommitOutcome::Duplicate
    );
    assert_eq!(
        reopened
            .conversation_timeline(destination(2))
            .unwrap()
            .into_iter()
            .find(|entry| entry.message_id() == Some(MessageId::new([0x18; 32])))
            .unwrap()
            .ingress_observation(),
        Some(first_available)
    );
    let activity = reopened
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 100).unwrap(),
        )
        .unwrap();
    assert_eq!(
        activity
            .events()
            .iter()
            .find(|event| {
                matches!(
                    event.kind(),
                    MessageActivityKind::InboundImported { message_id }
                        if message_id == MessageId::new([0x18; 32])
                )
            })
            .unwrap()
            .ingress_observation(),
        Some(first_available)
    );
}

#[test]
fn memory_inbound_receiver_location_is_first_import_only_and_query_projected() {
    let mut store = MemoryChatStore::new();
    let sender_location = message_location(43_123_456, 1_784_000_001);
    let receiver_location = phone_location(44_654_321, 1_784_000_001_250);
    let conflicting_duplicate = phone_location(45_000_000, 1_784_000_009_999);
    let message =
        inbound(0x19, 2, 1_300, b"located reception").with_location(Some(sender_location));

    assert_eq!(
        store
            .commit_inbound_with_receiver_location(message.clone(), Some(receiver_location))
            .unwrap(),
        InboundCommitOutcome::Inserted
    );
    assert_eq!(
        store
            .commit_inbound_with_receiver_location(message, Some(conflicting_duplicate))
            .unwrap(),
        InboundCommitOutcome::Duplicate
    );

    let timeline = store.conversation_timeline(destination(2)).unwrap();
    assert_eq!(timeline[0].location(), Some(sender_location));
    assert_eq!(timeline[0].receiver_location(), Some(receiver_location));
    let activity = store
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 10).unwrap(),
        )
        .unwrap();
    assert_eq!(
        activity.events()[0].message_location(),
        Some(sender_location)
    );
    assert_eq!(
        activity.events()[0].receiver_location(),
        Some(receiver_location)
    );

    let mut reopened = MemoryChatStore::open(store.image()).unwrap();
    assert_eq!(
        reopened.conversation_timeline(destination(2)).unwrap()[0].receiver_location(),
        Some(receiver_location)
    );
    let without_fix = inbound(0x1a, 2, 1_400, b"no receiver fix");
    reopened.commit_inbound(without_fix.clone()).unwrap();
    reopened
        .commit_inbound_with_receiver_location(without_fix, Some(conflicting_duplicate))
        .unwrap();
    assert!(
        reopened
            .conversation_timeline(destination(2))
            .unwrap()
            .iter()
            .find(|entry| entry.message_id() == Some(MessageId::new([0x1a; 32])))
            .unwrap()
            .receiver_location()
            .is_none()
    );
}

#[test]
fn message_locations_validate_and_memory_persists_inbound_outbound_and_retry_material() {
    assert!(MessageLocation::new(-90_000_000, 180_000_000, 0, 0, 0, 0, 0).is_some());
    assert!(MessageLocation::new(90_000_001, 0, 0, 0, 0, 0, 0).is_none());
    assert!(MessageLocation::new(0, -180_000_001, 0, 0, 0, 0, 0).is_none());

    let inbound_location = message_location(43_123_456, 1_784_000_001);
    let outbound_location = message_location(44_654_321, 1_784_000_002);
    let mut store = MemoryChatStore::new();
    let inbound_message =
        inbound(0x19, 2, 1_300, b"located inbound").with_location(Some(inbound_location));
    store.commit_inbound(inbound_message).unwrap();
    let outbound_material =
        outbound(0x43, 2, 10_100, b"located outbound").with_location(Some(outbound_location));
    let outbox_id = store
        .commit_outbound(outbound_material.clone())
        .unwrap()
        .outbox_id();
    let first = acceptance(43, 0x83);
    store.record_acceptance(outbox_id, first).unwrap();
    store
        .project_submission_status(
            first.submission_id(),
            SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        )
        .unwrap();
    store
        .retry_outbox(outbox_id, IdempotencyKey::new([0x44; 16]))
        .unwrap();

    let reopened = MemoryChatStore::open(store.image()).unwrap();
    let timeline = reopened.conversation_timeline(destination(2)).unwrap();
    assert_eq!(timeline[0].location(), Some(inbound_location));
    assert_eq!(timeline[1].location(), Some(outbound_location));
    assert_eq!(
        reopened
            .outbox(outbox_id)
            .unwrap()
            .unwrap()
            .material()
            .location(),
        Some(outbound_location)
    );
    assert!(matches!(
        reopened.reconcile().unwrap().as_slice(),
        [ReconcileWork::Submit { material, .. }]
            if material.location() == Some(outbound_location)
    ));

    let late_location = message_location(45_000_000, 1_784_000_003);
    store = reopened;
    store
        .commit_inbound(inbound(0x1a, 2, 1_400, b"location backfill"))
        .unwrap();
    assert_eq!(
        store
            .commit_inbound(
                inbound(0x1a, 2, 1_400, b"location backfill").with_location(Some(late_location)),
            )
            .unwrap(),
        InboundCommitOutcome::Duplicate
    );
    assert_eq!(
        store
            .conversation_timeline(destination(2))
            .unwrap()
            .into_iter()
            .find(|entry| entry.message_id() == Some(MessageId::new([0x1a; 32])))
            .unwrap()
            .location(),
        Some(late_location)
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

#[test]
fn retry_rearms_the_same_timeline_row_with_exact_semantic_material() {
    let mut store = MemoryChatStore::new();
    let original = outbound(0x41, 2, 10_000, b"eventual");
    let id = store.commit_outbound(original.clone()).unwrap().outbox_id();
    let first_acceptance = acceptance(41, 0x81);
    store.record_acceptance(id, first_acceptance).unwrap();
    store
        .project_submission_status(
            first_acceptance.submission_id(),
            SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        )
        .unwrap();

    assert_eq!(
        store
            .retry_outbox(id, IdempotencyKey::new([0x42; 16]))
            .unwrap(),
        OutboxRetryOutcome::Requeued(id)
    );
    let row = store.outbox(id).unwrap().unwrap();
    assert_eq!(row.id(), id);
    assert_eq!(row.status(), OutboxStatus::Committed);
    assert_eq!(row.acceptance(), None);
    assert_eq!(row.material().destination(), original.destination());
    assert_eq!(row.material().timestamp(), original.timestamp());
    assert_eq!(row.material().title(), original.title());
    assert_eq!(row.material().content(), original.content());
    assert_eq!(
        row.material().idempotency_key(),
        IdempotencyKey::new([0x42; 16])
    );
    assert_eq!(
        store.conversation_timeline(destination(2)).unwrap().len(),
        1
    );
    assert!(matches!(
        store.reconcile().unwrap().as_slice(),
        [ReconcileWork::Submit {
            outbox_id,
            material
        }] if *outbox_id == id
            && material.timestamp() == original.timestamp()
            && material.content() == original.content()
    ));

    let second_acceptance = acceptance(42, 0x81);
    store.record_acceptance(id, second_acceptance).unwrap();
    assert_eq!(
        store.outbox(id).unwrap().unwrap().acceptance(),
        Some(second_acceptance)
    );
}

#[test]
fn retry_refuses_unchanged_keys_and_non_retryable_terminal_states() {
    let mut store = MemoryChatStore::new();
    let failed = store
        .commit_outbound(outbound(0x51, 2, 11_000, b"failed"))
        .unwrap()
        .outbox_id();
    let failed_acceptance = acceptance(51, 0x91);
    store.record_acceptance(failed, failed_acceptance).unwrap();
    store
        .project_submission_status(
            failed_acceptance.submission_id(),
            SubmissionState::Failed(SubmissionFailure::NoPath),
        )
        .unwrap();
    assert_eq!(
        store
            .retry_outbox(failed, IdempotencyKey::new([0x51; 16]))
            .unwrap_err(),
        ChatStoreError::RetryIdempotencyKeyUnchanged(failed)
    );

    let rejected = store
        .commit_outbound(outbound(0x52, 2, 12_000, b"rejected"))
        .unwrap()
        .outbox_id();
    let rejected_acceptance = acceptance(52, 0x92);
    store
        .record_acceptance(rejected, rejected_acceptance)
        .unwrap();
    store
        .project_submission_status(
            rejected_acceptance.submission_id(),
            SubmissionState::Failed(SubmissionFailure::DownstreamRejection),
        )
        .unwrap();
    assert_eq!(
        store
            .retry_outbox(rejected, IdempotencyKey::new([0x53; 16]))
            .unwrap_err(),
        ChatStoreError::OutboxNotRetryable(rejected)
    );
}

#[test]
fn outbound_attempt_locations_are_atomic_exact_once_and_restart_safe() {
    let mut store = MemoryChatStore::new();
    let material = outbound(0xc1, 2, 19_000, b"location-stamped attempts");
    let initial_location = location(44_123_456, 18_900);
    let outbox_id = store
        .commit_outbound_with_location(material.clone(), initial_location)
        .unwrap()
        .outbox_id();

    assert_eq!(
        store
            .commit_outbound_with_location(
                material,
                AttemptLocationStamp::Unavailable(
                    PhoneLocationUnavailableReason::PermissionDenied,
                ),
            )
            .unwrap(),
        OutboxCommitOutcome::Existing(outbox_id)
    );

    let first_acceptance = acceptance(0xc2, 0xc3);
    store
        .record_acceptance(outbox_id, first_acceptance)
        .unwrap();
    store
        .project_submission_status(
            first_acceptance.submission_id(),
            SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        )
        .unwrap();
    let manual_location =
        AttemptLocationStamp::Unavailable(PhoneLocationUnavailableReason::ServicesDisabled);
    assert_eq!(
        store
            .retry_outbox_with_location(
                outbox_id,
                IdempotencyKey::new([0xc4; 16]),
                manual_location,
            )
            .unwrap(),
        OutboxRetryOutcome::Requeued(outbox_id)
    );
    assert_eq!(
        store
            .retry_outbox_with_location(
                outbox_id,
                IdempotencyKey::new([0xc5; 16]),
                location(45_000_000, 19_100),
            )
            .unwrap(),
        OutboxRetryOutcome::AlreadyPending(outbox_id)
    );

    let second_acceptance = acceptance(0xc5, 0xc6);
    store
        .record_acceptance(outbox_id, second_acceptance)
        .unwrap();
    store
        .project_submission_status(
            second_acceptance.submission_id(),
            SubmissionState::Failed(SubmissionFailure::NoPath),
        )
        .unwrap();
    let second_retry_location = location(46_234_567, 19_200);
    assert_eq!(
        store
            .retry_outbox_with_location(
                outbox_id,
                IdempotencyKey::new([0xc7; 16]),
                second_retry_location,
            )
            .unwrap(),
        OutboxRetryOutcome::Requeued(outbox_id)
    );

    let reopened = MemoryChatStore::open(store.image()).unwrap();
    let attempt_events = reopened
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 100).unwrap(),
        )
        .unwrap()
        .events()
        .iter()
        .filter_map(|event| {
            event
                .attempt_location()
                .map(|location| (event.attempt_number().unwrap().get(), location))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        attempt_events,
        vec![
            (3, second_retry_location),
            (2, manual_location),
            (1, initial_location),
        ]
    );
}

#[test]
fn message_activity_is_exact_once_attempt_aware_scoped_and_restart_safe() {
    let mut store = MemoryChatStore::new();
    let peer = destination(2);
    let inbound_message = inbound(0xd1, 2, 20_000, b"inbound activity");
    assert_eq!(
        store.commit_inbound(inbound_message.clone()).unwrap(),
        InboundCommitOutcome::Inserted
    );
    assert_eq!(
        store.commit_inbound(inbound_message).unwrap(),
        InboundCommitOutcome::Duplicate
    );

    let material = outbound(0xd2, 2, 21_000, b"outbound activity");
    let outbox_id = store.commit_outbound(material.clone()).unwrap().outbox_id();
    assert_eq!(
        store.commit_outbound(material).unwrap(),
        OutboxCommitOutcome::Existing(outbox_id)
    );
    let first_acceptance = acceptance(0xd3, 0xd4);
    assert_eq!(
        store
            .record_acceptance(outbox_id, first_acceptance)
            .unwrap(),
        AcceptanceOutcome::Recorded
    );
    assert_eq!(
        store
            .record_acceptance(outbox_id, first_acceptance)
            .unwrap(),
        AcceptanceOutcome::Unchanged
    );
    assert_eq!(
        store
            .project_submission_status(
                first_acceptance.submission_id(),
                SubmissionState::Preparing,
            )
            .unwrap(),
        StatusProjectionOutcome::Advanced
    );
    assert_eq!(
        store
            .project_submission_status(first_acceptance.submission_id(), SubmissionState::Queued)
            .unwrap(),
        StatusProjectionOutcome::IgnoredStale
    );
    assert_eq!(
        store
            .project_submission_status(
                first_acceptance.submission_id(),
                SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            )
            .unwrap(),
        StatusProjectionOutcome::Advanced
    );
    assert_eq!(
        store
            .retry_outbox(outbox_id, IdempotencyKey::new([0xd5; 16]))
            .unwrap(),
        OutboxRetryOutcome::Requeued(outbox_id)
    );
    assert_eq!(
        store
            .retry_outbox(outbox_id, IdempotencyKey::new([0xd6; 16]))
            .unwrap(),
        OutboxRetryOutcome::AlreadyPending(outbox_id)
    );
    let second_acceptance = acceptance(0xd6, 0xd4);
    store
        .record_acceptance(outbox_id, second_acceptance)
        .unwrap();
    store
        .project_submission_status(
            second_acceptance.submission_id(),
            SubmissionState::Delivered(evidence(0xd7)),
        )
        .unwrap();

    let first_page = store
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 3).unwrap(),
        )
        .unwrap();
    assert_eq!(first_page.events().len(), 3);
    assert!(!first_page.history_incomplete());
    assert!(first_page.next_before().is_some());
    assert!(matches!(
        first_page.events()[0].kind(),
        MessageActivityKind::OutboundStatus {
            state: SubmissionState::Delivered(packet)
        } if packet == evidence(0xd7)
    ));
    assert_eq!(first_page.events()[0].attempt_number().unwrap().get(), 2);
    assert!(matches!(
        first_page.events()[2].kind(),
        MessageActivityKind::OutboundRequeued { .. }
    ));
    assert_eq!(first_page.events()[2].attempt_number().unwrap().get(), 2);

    let second_page = store
        .message_activity(
            MessageActivityPageRequest::new(
                MessageActivityScope::All,
                first_page.next_before(),
                100,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(second_page.events().len(), 5);
    assert!(second_page.next_before().is_none());

    let outbound_sequence = store
        .conversation_timeline(peer)
        .unwrap()
        .into_iter()
        .find(|entry| entry.direction() == TimelineDirection::Outbound)
        .unwrap()
        .sequence();
    let scoped = store
        .message_activity(
            MessageActivityPageRequest::new(
                MessageActivityScope::Timeline(outbound_sequence),
                None,
                100,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(scoped.events().len(), 7);
    assert!(
        scoped
            .events()
            .iter()
            .all(|event| event.timeline_sequence() == outbound_sequence)
    );

    let timeline = store.conversation_timeline(peer).unwrap();
    let outbound = timeline
        .iter()
        .find(|entry| entry.direction() == TimelineDirection::Outbound)
        .unwrap();
    assert_eq!(outbound.current_attempt().unwrap().get(), 2);
    assert_eq!(
        outbound.submission_id(),
        Some(second_acceptance.submission_id())
    );
    assert_eq!(outbound.packet_evidence(), Some(evidence(0xd7)));

    let reopened = MemoryChatStore::open(store.image()).unwrap();
    assert_eq!(
        reopened
            .message_activity(
                MessageActivityPageRequest::new(MessageActivityScope::All, None, 100).unwrap()
            )
            .unwrap()
            .events()
            .len(),
        8
    );
}

#[test]
fn message_activity_retention_is_bounded_and_reports_incomplete_history() {
    let mut store = MemoryChatStore::new();
    for index in 0..=MAX_MESSAGE_ACTIVITY_EVENTS {
        let mut id = [0_u8; 32];
        id[..8].copy_from_slice(&(index as u64).to_be_bytes());
        store
            .commit_inbound(InboundMessage::new(
                MessageId::new(id),
                destination(0xa0),
                destination(0xee),
                timestamp(30_000),
                Vec::new(),
                Vec::new(),
            ))
            .unwrap();
    }

    let mut before = None;
    let mut retained = 0;
    let mut oldest_id = None;
    loop {
        let page = store
            .message_activity(
                MessageActivityPageRequest::new(MessageActivityScope::All, before, 100).unwrap(),
            )
            .unwrap();
        assert!(page.history_incomplete());
        retained += page.events().len();
        oldest_id = page
            .events()
            .last()
            .map(|event| event.id().get())
            .or(oldest_id);
        match page.next_before() {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    assert_eq!(retained, MAX_MESSAGE_ACTIVITY_EVENTS);
    assert_eq!(oldest_id, Some(2));
}

#[test]
fn rf_trace_import_is_atomic_idempotent_correlated_and_restart_safe() {
    let mut store = MemoryChatStore::new();
    let first_location = location(44_111_111, 5_000);
    let outbox_id = store
        .commit_outbound_with_location(outbound(0xe1, 0x91, 4_000, b"trace me"), first_location)
        .unwrap()
        .outbox_id();
    store
        .record_acceptance(outbox_id, acceptance(700, 0xe2))
        .unwrap();
    let timeline_sequence = store.outbox(outbox_id).unwrap().unwrap().sequence();

    let rx = RfTraceObservation::new(
        RfTraceEventSequence::new(4).unwrap(),
        4_000,
        RfTraceObservationKind::LogicalRx(RfTraceRxObservation::new(
            RfTraceInterfaceId::new([0, 0, 0, 0, 0, 0, 0, 1]),
            evidence(0x55),
            Some(RnsAttemptToken::new([0xa1; 32])),
            -91,
            7,
        )),
    );
    let observations = vec![
        rf_route(1, 0xa1, 700),
        rf_tx(2, 0xa1),
        rf_terminal(3, 0xa1),
        rx,
        RfTraceObservation::new(
            RfTraceEventSequence::new(5).unwrap(),
            5_000,
            RfTraceObservationKind::LogicalRx(RfTraceRxObservation::new(
                RfTraceInterfaceId::new([0, 0, 0, 0, 0, 0, 0, 1]),
                evidence(0x55),
                None,
                -94,
                5,
            )),
        ),
    ];
    let batch = RfTraceImportBatch::new(
        RfTraceBootId::new(u64::MAX),
        rf_profile(1),
        6_000,
        false,
        observations.clone(),
    )
    .unwrap();
    let outcome = store.import_rf_trace_batch(batch.clone()).unwrap();
    assert_eq!((outcome.inserted(), outcome.existing()), (5, 0));

    let page = store
        .rf_trace(RfTracePageRequest::new(RfTraceScope::All, None, 100).unwrap())
        .unwrap();
    assert_eq!(page.events().len(), 5);
    assert!(!page.history_incomplete());
    assert_eq!(
        page.events()
            .iter()
            .filter(|event| event.message_correlation().is_some())
            .count(),
        4
    );
    let correlated = page
        .events()
        .iter()
        .find_map(|event| event.message_correlation())
        .unwrap();
    assert_eq!(correlated.timeline_sequence(), timeline_sequence);
    assert_eq!(correlated.attempt_location(), first_location);

    let duplicate = store.import_rf_trace_batch(batch).unwrap();
    assert_eq!((duplicate.inserted(), duplicate.existing()), (0, 5));

    let conflicting = RfTraceImportBatch::new(
        RfTraceBootId::new(u64::MAX),
        rf_profile(1),
        7_000,
        false,
        vec![rf_tx(1, 0xa1)],
    )
    .unwrap();
    assert!(matches!(
        store.import_rf_trace_batch(conflicting),
        Err(ChatStoreError::RfTraceEventConflict { .. })
    ));
    assert_eq!(
        store
            .rf_trace(RfTracePageRequest::new(RfTraceScope::All, None, 100).unwrap())
            .unwrap()
            .events()
            .len(),
        5
    );
    assert!(matches!(
        store.import_rf_trace_batch(
            RfTraceImportBatch::new(
                RfTraceBootId::new(u64::MAX),
                rf_profile(2),
                8_000,
                false,
                Vec::new(),
            )
            .unwrap()
        ),
        Err(ChatStoreError::RfTraceBootProfileConflict(_))
    ));

    store
        .project_submission_status(
            SubmissionId::new(700).unwrap(),
            SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
        )
        .unwrap();
    let retry_location = location(45_222_222, 9_000);
    store
        .retry_outbox_with_location(outbox_id, IdempotencyKey::new([0xe3; 16]), retry_location)
        .unwrap();
    store
        .record_acceptance(outbox_id, acceptance(701, 0xe4))
        .unwrap();
    store
        .import_rf_trace_batch(
            RfTraceImportBatch::new(
                RfTraceBootId::new(2),
                rf_profile(1),
                10_000,
                false,
                vec![rf_route(1, 0xa2, 701), rf_tx(2, 0xa2)],
            )
            .unwrap(),
        )
        .unwrap();
    let scoped = store
        .rf_trace(
            RfTracePageRequest::new(RfTraceScope::Timeline(timeline_sequence), None, 100).unwrap(),
        )
        .unwrap();
    assert_eq!(scoped.events().len(), 6);
    assert!(scoped.events().iter().any(|event| {
        event.message_correlation().is_some_and(|correlation| {
            correlation.attempt_number().get() == 2
                && correlation.attempt_location() == retry_location
        })
    }));

    let reopened = MemoryChatStore::open(store.image()).unwrap();
    assert_eq!(
        reopened
            .rf_trace(RfTracePageRequest::new(RfTraceScope::All, None, 100).unwrap())
            .unwrap()
            .events()
            .len(),
        7
    );
}
