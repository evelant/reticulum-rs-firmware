#![cfg(feature = "sqlite")]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use reticulum_lxmf_chat_core::{
    AUTOMATIC_OUTBOX_RETRY_LIMIT, AcceptanceIds, AttemptLocationStamp, AutomaticOutboxRetryOutcome,
    ChatStore, Contact, DestinationHash, DeviceBinding, DeviceBindingOutcome, EncodedPacketSha256,
    IdempotencyKey, InboundCommitOutcome, InboundMessage, MessageActivityKind,
    MessageActivityPageRequest, MessageActivityRetryTrigger, MessageActivityScope, MessageId,
    MessageIngressObservation, MessageInterfaceId, MessageLocation, MessageSignalObservation,
    OutboxCommitOutcome, OutboxMaterial, OutboxRetryOutcome, OutboxStatus, PacketEvidence,
    PhoneLocationAuthorization, PhoneLocationSample, PhoneLocationSource,
    PhoneLocationUnavailableReason, ReconcileWork, RfTraceBootId, RfTraceEventSequence,
    RfTraceImportBatch, RfTraceInterfaceId, RfTraceObservation, RfTraceObservationKind,
    RfTracePageRequest, RfTraceRadioProfile, RfTraceRouteObservation, RfTraceRouteResolution,
    RfTraceScope, RfTraceTxObservation, RfTraceTxOutcome, RnsAttemptToken, SQLITE_SCHEMA_VERSION,
    SqliteChatStore, SqliteStoreError, SubmissionFailure, SubmissionId, SubmissionState,
    TimelineDirection, UnixTimestampMillis,
};

static TEST_DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDatabase {
    path: PathBuf,
}

impl TestDatabase {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time must follow Unix epoch")
            .as_nanos();
        let sequence = TEST_DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "reticulum-lxmf-chat-{label}-{}-{nonce}-{sequence}.sqlite3",
            std::process::id()
        ));
        Self { path }
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.path.with_extension("sqlite3-shm"));
        let _ = fs::remove_file(self.path.with_extension("sqlite3-wal"));
    }
}

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
        MessageInterfaceId::new(interface),
        Some(MessageSignalObservation::new(rssi_dbm, snr_db)),
    )
}

fn outbound(key: u8, peer: u8, at: u64, content: &[u8]) -> OutboxMaterial {
    OutboxMaterial::new(
        destination(peer),
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

fn binding(tag: u8) -> DeviceBinding {
    DeviceBinding::new(
        [tag; 16],
        destination(tag.wrapping_add(1)),
        destination(tag.wrapping_add(2)),
    )
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
            RfTraceInterfaceId::new(1),
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
                RfTraceInterfaceId::new(1),
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

#[test]
fn database_binding_is_persistent_and_rejects_a_different_device() {
    let database = TestDatabase::new("device-binding");
    let expected = binding(0x31);
    let observed = binding(0x41);

    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        assert_eq!(store.device_binding().unwrap(), None);
        assert_eq!(
            store.bind_device(expected).unwrap(),
            DeviceBindingOutcome::Bound
        );
        assert_eq!(
            store.bind_device(expected).unwrap(),
            DeviceBindingOutcome::Unchanged
        );
        assert!(matches!(
            store.bind_device(observed),
            Err(SqliteStoreError::DeviceBindingMismatch {
                expected: retained,
                observed: rejected,
            }) if retained == expected && rejected == observed
        ));
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(reopened.device_binding().unwrap(), Some(expected));
}

#[test]
fn unknown_inbound_conversation_survives_contact_add_and_rename() {
    let database = TestDatabase::new("unknown-inbound-conversation");
    let peer = destination(0x51);
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        store
            .commit_inbound(inbound(0x61, 0x51, 2_000, b"durable unknown sender"))
            .unwrap();
        assert!(store.contacts().unwrap().is_empty());
        let conversations = store.conversation_peers().unwrap();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].peer(), peer);
        assert_eq!(conversations[0].saved_name(), None);
        assert_eq!(conversations[0].message_count(), 1);
        assert_eq!(conversations[0].inbound_message_count(), 1);

        store
            .upsert_contact(Contact::new(peer, "Temporary name"))
            .unwrap();
        store
            .upsert_contact(Contact::new(peer, "Hill relay"))
            .unwrap();
        store.close().unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(reopened.contacts().unwrap().len(), 1);
    let conversations = reopened.conversation_peers().unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].saved_name(), Some("Hill relay"));
    assert_eq!(conversations[0].message_count(), 1);
    assert_eq!(conversations[0].inbound_message_count(), 1);
    assert_eq!(
        conversations[0].last_message().unwrap().content(),
        b"durable unknown sender"
    );
    assert_eq!(reopened.conversation_timeline(peer).unwrap().len(), 1);
}

#[test]
fn schema_one_database_migrates_to_unbound_current_schema() {
    let database = TestDatabase::new("schema-one-migration");
    SqliteChatStore::open(&database.path)
        .unwrap()
        .close()
        .unwrap();
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE device_binding;\n\
         PRAGMA user_version = 1;",
        )
        .unwrap();
    connection.close().unwrap();

    let store = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
    assert_eq!(store.device_binding().unwrap(), None);
}

#[test]
fn schema_two_database_adds_zeroed_automatic_retry_budget() {
    let database = TestDatabase::new("schema-two-migration");
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        store
            .commit_outbound(outbound(0x41, 0x42, 1_000, b"legacy row"))
            .unwrap();
        store.close().unwrap();
    }
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX message_activity_timeline_idx;\n\
             DROP TABLE message_activity;\n\
             DROP TABLE message_activity_meta;\n\
             DELETE FROM chat_meta WHERE name = 'message_activity_id';\n\
             ALTER TABLE outbox DROP COLUMN current_attempt;\n\
             ALTER TABLE outbox DROP COLUMN automatic_retry_count;\n\
             PRAGMA user_version = 2;",
        )
        .unwrap();
    connection.close().unwrap();

    let store = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
    store.close().unwrap();
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    let default: String = connection
        .query_row(
            "SELECT dflt_value FROM pragma_table_info('outbox')\n\
             WHERE name = 'automatic_retry_count'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(default, "0");
    assert_eq!(
        connection
            .query_row(
                "SELECT automatic_retry_count FROM outbox WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn schema_four_migration_defaults_legacy_ingress_and_restart_preserves_new_observations() {
    let database = TestDatabase::new("schema-four-ingress-migration");
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        assert_eq!(
            store
                .commit_inbound(inbound(0x71, 0x51, 2_100, b"legacy inbound"))
                .unwrap(),
            InboundCommitOutcome::Inserted
        );
        store.close().unwrap();
    }

    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE inbound_messages_v4 (\n\
                 message_id BLOB PRIMARY KEY NOT NULL CHECK (length(message_id) = 32),\n\
                 sequence INTEGER UNIQUE NOT NULL CHECK (sequence > 0),\n\
                 local_destination BLOB NOT NULL CHECK (length(local_destination) = 16),\n\
                 source BLOB NOT NULL CHECK (length(source) = 16),\n\
                 timestamp_unix_ms INTEGER NOT NULL CHECK (timestamp_unix_ms > 0),\n\
                 title BLOB NOT NULL,\n\
                 content BLOB NOT NULL\n\
             );\n\
             INSERT INTO inbound_messages_v4(\n\
                 message_id, sequence, local_destination, source, timestamp_unix_ms, title, content\n\
             ) SELECT\n\
                 message_id, sequence, local_destination, source, timestamp_unix_ms, title, content\n\
             FROM inbound_messages;\n\
             DROP TABLE inbound_messages;\n\
             ALTER TABLE inbound_messages_v4 RENAME TO inbound_messages;\n\
             PRAGMA user_version = 4;",
        )
        .unwrap();
    connection.close().unwrap();

    let first = ingress(7, -109, -2);
    {
        let mut migrated = SqliteChatStore::open(&database.path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
        assert_eq!(
            migrated.conversation_timeline(destination(0x51)).unwrap()[0].ingress_observation(),
            None
        );
        assert_eq!(
            migrated
                .commit_inbound(
                    inbound(0x72, 0x51, 2_200, b"current inbound")
                        .with_ingress_observation(Some(first)),
                )
                .unwrap(),
            InboundCommitOutcome::Inserted
        );
        migrated.close().unwrap();
    }

    let mut reopened = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(
        reopened
            .conversation_timeline(destination(0x51))
            .unwrap()
            .into_iter()
            .find(|entry| entry.message_id() == Some(MessageId::new([0x72; 32])))
            .unwrap()
            .ingress_observation(),
        Some(first)
    );
    assert_eq!(
        reopened
            .commit_inbound(
                inbound(0x72, 0x51, 2_200, b"current inbound")
                    .with_ingress_observation(Some(ingress(8, -72, 11))),
            )
            .unwrap(),
        InboundCommitOutcome::Duplicate
    );
    assert_eq!(
        reopened
            .conversation_timeline(destination(0x51))
            .unwrap()
            .into_iter()
            .find(|entry| entry.message_id() == Some(MessageId::new([0x72; 32])))
            .unwrap()
            .ingress_observation(),
        Some(first)
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
                        if message_id == MessageId::new([0x72; 32])
                )
            })
            .unwrap()
            .ingress_observation(),
        Some(first)
    );

    let late = ingress(9, -116, -7);
    assert_eq!(
        reopened
            .commit_inbound(inbound(0x73, 0x51, 2_300, b"late sqlite evidence"))
            .unwrap(),
        InboundCommitOutcome::Inserted
    );
    assert_eq!(
        reopened
            .commit_inbound(
                inbound(0x73, 0x51, 2_300, b"late sqlite evidence")
                    .with_ingress_observation(Some(late)),
            )
            .unwrap(),
        InboundCommitOutcome::Duplicate
    );
    let late_activity = reopened
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 100).unwrap(),
        )
        .unwrap();
    assert_eq!(
        late_activity
            .events()
            .iter()
            .find(|event| {
                matches!(
                    event.kind(),
                    MessageActivityKind::InboundImported { message_id }
                        if message_id == MessageId::new([0x73; 32])
                )
            })
            .unwrap()
            .ingress_observation(),
        Some(late)
    );
}

#[test]
fn close_and_reopen_preserves_complete_chat_and_reconcile_state() {
    let database = TestDatabase::new("restart");
    let peer = destination(2);
    let (unsubmitted, pending, terminal, pending_acceptance, terminal_acceptance);

    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
        store
            .upsert_contact(Contact::new(peer, "Persistent peer"))
            .unwrap();
        store
            .commit_inbound(inbound(0x11, 2, 1_000, b"persisted inbound"))
            .unwrap();
        assert!(store.contains_inbound(MessageId::new([0x11; 32])).unwrap());

        unsubmitted = store
            .commit_outbound(outbound(0x21, 2, 2_000, b"submit after restart"))
            .unwrap()
            .outbox_id();
        pending = store
            .commit_outbound(outbound(0x22, 2, 3_000, b"refresh after restart"))
            .unwrap()
            .outbox_id();
        terminal = store
            .commit_outbound(outbound(0x23, 2, 4_000, b"already delivered"))
            .unwrap()
            .outbox_id();
        pending_acceptance = acceptance(31, 0x31);
        terminal_acceptance = acceptance(32, 0x32);
        store
            .record_acceptance(pending, pending_acceptance)
            .unwrap();
        store
            .project_submission_status(
                pending_acceptance.submission_id(),
                SubmissionState::AwaitingDelivery(evidence(0x41)),
            )
            .unwrap();
        store
            .record_acceptance(terminal, terminal_acceptance)
            .unwrap();
        store
            .project_submission_status(
                terminal_acceptance.submission_id(),
                SubmissionState::Delivered(evidence(0x42)),
            )
            .unwrap();
        store.close().unwrap();
    }

    {
        let mut reopened = SqliteChatStore::open(&database.path).unwrap();
        assert_eq!(
            reopened.contact(peer).unwrap().unwrap().display_name(),
            "Persistent peer"
        );
        assert_eq!(
            reopened
                .commit_inbound(inbound(0x11, 2, 1_000, b"persisted inbound"))
                .unwrap(),
            InboundCommitOutcome::Duplicate
        );
        assert_eq!(
            reopened.outbox(pending).unwrap().unwrap().acceptance(),
            Some(pending_acceptance)
        );
        assert_eq!(
            reopened.outbox(pending).unwrap().unwrap().status(),
            OutboxStatus::Device(SubmissionState::AwaitingDelivery(evidence(0x41)))
        );
        assert_eq!(
            reopened.outbox(terminal).unwrap().unwrap().status(),
            OutboxStatus::Device(SubmissionState::Delivered(evidence(0x42)))
        );

        let timeline = reopened.conversation_timeline(peer).unwrap();
        assert_eq!(timeline.len(), 4);
        assert_eq!(timeline[0].direction(), TimelineDirection::Inbound);
        assert_eq!(timeline[0].content(), b"persisted inbound");
        assert_eq!(timeline[1].outbox_id(), Some(unsubmitted));
        assert_eq!(timeline[2].outbox_id(), Some(pending));
        assert_eq!(timeline[3].outbox_id(), Some(terminal));

        let work = reopened.reconcile().unwrap();
        assert_eq!(work.len(), 2);
        assert!(matches!(
            &work[0],
            ReconcileWork::Submit { outbox_id, material }
                if *outbox_id == unsubmitted && material.content() == b"submit after restart"
        ));
        assert_eq!(
            work[1],
            ReconcileWork::RefreshStatus {
                outbox_id: pending,
                acceptance: pending_acceptance,
            }
        );

        reopened
            .project_submission_status(
                pending_acceptance.submission_id(),
                SubmissionState::Delivered(evidence(0x41)),
            )
            .unwrap();
        reopened.close().unwrap();
    }

    let reopened_again = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(reopened_again.reconcile().unwrap().len(), 1);
    assert!(matches!(
        reopened_again.outbox(pending).unwrap().unwrap().status(),
        OutboxStatus::Device(SubmissionState::Delivered(_))
    ));
    reopened_again.close().unwrap();
}

#[test]
fn sqlite_activity_is_exact_once_attempt_aware_scoped_and_restart_safe() {
    let database = TestDatabase::new("message-activity");
    let peer = destination(0x72);
    let outbox_id;
    let timeline_sequence;
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        let inbound_message = inbound(0x71, 0x72, 10_000, b"activity inbound");
        assert_eq!(
            store.commit_inbound(inbound_message.clone()).unwrap(),
            InboundCommitOutcome::Inserted
        );
        assert_eq!(
            store.commit_inbound(inbound_message).unwrap(),
            InboundCommitOutcome::Duplicate
        );
        let material = outbound(0x73, 0x72, 11_000, b"activity outbound");
        outbox_id = store.commit_outbound(material.clone()).unwrap().outbox_id();
        assert_eq!(
            store.commit_outbound(material).unwrap().outbox_id(),
            outbox_id
        );
        timeline_sequence = store.outbox(outbox_id).unwrap().unwrap().sequence();
        let first = acceptance(0x74, 0x75);
        store.record_acceptance(outbox_id, first).unwrap();
        store.record_acceptance(outbox_id, first).unwrap();
        store
            .project_submission_status(
                first.submission_id(),
                SubmissionState::Failed(SubmissionFailure::NoPath),
            )
            .unwrap();
        store
            .retry_outbox(outbox_id, IdempotencyKey::new([0x76; 16]))
            .unwrap();
        let second = acceptance(0x77, 0x75);
        store.record_acceptance(outbox_id, second).unwrap();
        store
            .project_submission_status(
                second.submission_id(),
                SubmissionState::Delivered(evidence(0x78)),
            )
            .unwrap();

        let page = store
            .message_activity(
                MessageActivityPageRequest::new(MessageActivityScope::All, None, 100).unwrap(),
            )
            .unwrap();
        assert_eq!(page.events().len(), 7);
        assert!(!page.history_incomplete());
        assert!(matches!(
            page.events()[0].kind(),
            MessageActivityKind::OutboundStatus {
                state: SubmissionState::Delivered(packet)
            } if packet == evidence(0x78)
        ));
        assert_eq!(page.events()[0].attempt_number().unwrap().get(), 2);
        assert!(page.events().iter().any(|event| matches!(
            event.kind(),
            MessageActivityKind::OutboundRequeued {
                trigger: MessageActivityRetryTrigger::Manual,
                ..
            }
        )));
        store.close().unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    let scoped = reopened
        .message_activity(
            MessageActivityPageRequest::new(
                MessageActivityScope::Timeline(timeline_sequence),
                None,
                2,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(scoped.events().len(), 2);
    assert!(scoped.next_before().is_some());
    assert!(
        scoped
            .events()
            .iter()
            .all(|event| event.timeline_sequence() == timeline_sequence)
    );
    let row = reopened.outbox(outbox_id).unwrap().unwrap();
    assert_eq!(row.current_attempt().get(), 2);
    let timeline = reopened.conversation_timeline(peer).unwrap();
    let outbound = timeline
        .iter()
        .find(|entry| entry.outbox_id() == Some(outbox_id))
        .unwrap();
    assert_eq!(outbound.current_attempt().unwrap().get(), 2);
    assert_eq!(outbound.packet_evidence(), Some(evidence(0x78)));
}

#[test]
fn sqlite_outbound_attempt_locations_are_exact_once_and_restart_safe() {
    let database = TestDatabase::new("attempt-location");
    let initial_location = location(44_123_456, 10_900);
    let retry_location =
        AttemptLocationStamp::Unavailable(PhoneLocationUnavailableReason::PermissionDenied);
    let outbox_id;
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        let material = outbound(0x79, 0x72, 11_000, b"located activity outbound");
        outbox_id = store
            .commit_outbound_with_location(material.clone(), initial_location)
            .unwrap()
            .outbox_id();
        assert_eq!(
            store
                .commit_outbound_with_location(
                    material,
                    AttemptLocationStamp::Unavailable(
                        PhoneLocationUnavailableReason::ProviderError,
                    ),
                )
                .unwrap(),
            OutboxCommitOutcome::Existing(outbox_id)
        );
        let first = acceptance(0x7a, 0x7b);
        store.record_acceptance(outbox_id, first).unwrap();
        store
            .project_submission_status(
                first.submission_id(),
                SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            )
            .unwrap();
        assert_eq!(
            store
                .retry_outbox_with_location(
                    outbox_id,
                    IdempotencyKey::new([0x7c; 16]),
                    retry_location,
                )
                .unwrap(),
            OutboxRetryOutcome::Requeued(outbox_id)
        );
        store.close().unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
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
        vec![(2, retry_location), (1, initial_location)]
    );
}

#[test]
fn schema_five_migration_marks_legacy_attempt_locations_unknown_and_history_incomplete() {
    let database = TestDatabase::new("schema-five-attempt-location-migration");
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        store
            .commit_outbound(outbound(0x7d, 0x72, 12_000, b"legacy locationless attempt"))
            .unwrap();
        store.close().unwrap();
    }
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX message_activity_timeline_idx;\n\
             CREATE TABLE message_activity_v5 AS\n\
                 SELECT id, observed_at_unix_ms, timeline_sequence, peer, direction, outbox_id,\n\
                        attempt_number, kind, submission_id, message_id, status_kind, failure_kind,\n\
                        packet_len, packet_sha256, retry_trigger\n\
                 FROM message_activity;\n\
             DROP TABLE message_activity;\n\
             ALTER TABLE message_activity_v5 RENAME TO message_activity;\n\
             CREATE INDEX message_activity_timeline_idx\n\
                 ON message_activity(timeline_sequence, id DESC);\n\
             PRAGMA user_version = 5;",
        )
        .unwrap();
    connection.close().unwrap();

    let store = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
    let activity = store
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 100).unwrap(),
        )
        .unwrap();
    assert!(activity.history_incomplete());
    assert_eq!(activity.events().len(), 1);
    assert_eq!(
        activity.events()[0].attempt_location(),
        Some(AttemptLocationStamp::Unavailable(
            PhoneLocationUnavailableReason::NotObserved,
        ))
    );
}

#[test]
fn schema_three_migration_is_honest_about_missing_activity_history() {
    let database = TestDatabase::new("schema-three-activity-migration");
    let outbox_id;
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        outbox_id = store
            .commit_outbound(outbound(0x81, 0x82, 12_000, b"legacy outbound"))
            .unwrap()
            .outbox_id();
        for retry in 0..2_u8 {
            let accepted = acceptance(u64::from(0x90 + retry), 0x84);
            store.record_acceptance(outbox_id, accepted).unwrap();
            store
                .project_submission_status(
                    accepted.submission_id(),
                    SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
                )
                .unwrap();
            assert_eq!(
                store
                    .retry_outbox_automatically(outbox_id, IdempotencyKey::new([0x85 + retry; 16]),)
                    .unwrap(),
                AutomaticOutboxRetryOutcome::Requeued(outbox_id)
            );
        }
        store.close().unwrap();
    }
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX message_activity_timeline_idx;\n\
             DROP TABLE message_activity;\n\
             DROP TABLE message_activity_meta;\n\
             DELETE FROM chat_meta WHERE name = 'message_activity_id';\n\
             ALTER TABLE outbox DROP COLUMN current_attempt;\n\
             PRAGMA user_version = 3;",
        )
        .unwrap();
    connection.close().unwrap();

    let mut store = SqliteChatStore::open(&database.path).unwrap();
    let migrated = store
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 100).unwrap(),
        )
        .unwrap();
    assert!(migrated.events().is_empty());
    assert!(migrated.history_incomplete());
    assert_eq!(
        store
            .outbox(outbox_id)
            .unwrap()
            .unwrap()
            .current_attempt()
            .get(),
        3
    );
    assert_eq!(
        store
            .outbox(outbox_id)
            .unwrap()
            .unwrap()
            .automatic_retry_count(),
        2
    );

    let accepted = acceptance(0x83, 0x84);
    store.record_acceptance(outbox_id, accepted).unwrap();
    let after = store
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 100).unwrap(),
        )
        .unwrap();
    assert_eq!(after.events().len(), 1);
    assert!(after.history_incomplete());
    assert!(matches!(
        after.events()[0].kind(),
        MessageActivityKind::OutboundAccepted {
            acceptance: observed
        } if observed == accepted
    ));
    assert_eq!(after.events()[0].attempt_number().unwrap().get(), 3);
}

#[test]
fn unsupported_schema_version_is_rejected_without_mutation() {
    let database = TestDatabase::new("future-schema");
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .pragma_update(None, "user_version", 99_u32)
        .unwrap();
    connection.close().unwrap();

    assert!(matches!(
        SqliteChatStore::open(&database.path),
        Err(SqliteStoreError::UnsupportedSchemaVersion(99))
    ));
}

#[test]
fn retried_outbox_row_survives_sqlite_restart_without_a_duplicate_bubble() {
    let database = TestDatabase::new("retry");
    let original = outbound(0x61, 2, 13_000, b"eventual sqlite");
    let id;
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        id = store.commit_outbound(original.clone()).unwrap().outbox_id();
        let accepted = acceptance(61, 0xa1);
        store.record_acceptance(id, accepted).unwrap();
        store
            .project_submission_status(
                accepted.submission_id(),
                SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            )
            .unwrap();
        assert_eq!(
            store
                .retry_outbox(id, IdempotencyKey::new([0x62; 16]))
                .unwrap(),
            OutboxRetryOutcome::Requeued(id)
        );
        store.close().unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    let row = reopened.outbox(id).unwrap().unwrap();
    assert_eq!(row.status(), OutboxStatus::Committed);
    assert_eq!(row.material().timestamp(), original.timestamp());
    assert_eq!(row.material().content(), original.content());
    assert_eq!(
        reopened
            .conversation_timeline(destination(2))
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        reopened.reconcile().unwrap().as_slice(),
        [ReconcileWork::Submit {
            outbox_id,
            material
        }] if *outbox_id == id
            && material.timestamp() == original.timestamp()
            && material.content() == original.content()
    ));
}

#[test]
fn automatic_retry_exhaustion_survives_sqlite_restart_without_blocking_manual_retry() {
    let database = TestDatabase::new("automatic-retry-budget");
    let original = outbound(0x71, 2, 14_000, b"bounded sqlite");
    let id;
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        id = store.commit_outbound(original).unwrap().outbox_id();
        for automatic_retry in 0..AUTOMATIC_OUTBOX_RETRY_LIMIT {
            let accepted = acceptance(u64::from(90 + automatic_retry), 0xb1);
            store.record_acceptance(id, accepted).unwrap();
            store
                .project_submission_status(
                    accepted.submission_id(),
                    SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
                )
                .unwrap();
            assert_eq!(
                store
                    .retry_outbox_automatically(
                        id,
                        IdempotencyKey::new([0x72 + automatic_retry; 16]),
                    )
                    .unwrap(),
                AutomaticOutboxRetryOutcome::Requeued(id)
            );
        }
        let final_acceptance = acceptance(99, 0xb1);
        store.record_acceptance(id, final_acceptance).unwrap();
        store
            .project_submission_status(
                final_acceptance.submission_id(),
                SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            )
            .unwrap();
        store.close().unwrap();
    }

    let mut reopened = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(
        reopened
            .outbox(id)
            .unwrap()
            .unwrap()
            .automatic_retry_count(),
        AUTOMATIC_OUTBOX_RETRY_LIMIT
    );
    assert!(reopened.retryable_outbox().unwrap().is_empty());
    let before = reopened.outbox(id).unwrap().unwrap();
    assert_eq!(
        reopened
            .retry_outbox_automatically(id, IdempotencyKey::new([0x76; 16]))
            .unwrap(),
        AutomaticOutboxRetryOutcome::BudgetExhausted(id)
    );
    assert_eq!(reopened.outbox(id).unwrap().unwrap(), before);
    assert_eq!(
        reopened
            .retry_outbox(id, IdempotencyKey::new([0x77; 16]))
            .unwrap(),
        OutboxRetryOutcome::Requeued(id)
    );
    assert_eq!(
        reopened
            .outbox(id)
            .unwrap()
            .unwrap()
            .automatic_retry_count(),
        AUTOMATIC_OUTBOX_RETRY_LIMIT
    );
}

#[test]
fn schema_six_migrates_additively_without_rebuilding_message_activity() {
    let database = TestDatabase::new("schema-six-rf-trace-migration");
    let retained_location = location(43_123_456, 2_000);
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        store
            .commit_outbound_with_location(
                outbound(0xf1, 0xf2, 1_000, b"retained v6 activity"),
                retained_location,
            )
            .unwrap();
        store.close().unwrap();
    }
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX rf_trace_token_idx;\n\
             DROP INDEX rf_trace_packet_idx;\n\
             DROP INDEX rf_trace_timeline_idx;\n\
             DROP TABLE rf_trace_events;\n\
             DROP TABLE rf_trace_boots;\n\
             DELETE FROM chat_meta WHERE name = 'rf_trace_id';\n\
             PRAGMA user_version = 6;",
        )
        .unwrap();
    connection.close().unwrap();

    let store = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
    let activity = store
        .message_activity(
            MessageActivityPageRequest::new(MessageActivityScope::All, None, 10).unwrap(),
        )
        .unwrap();
    assert_eq!(activity.events().len(), 1);
    assert_eq!(
        activity.events()[0].attempt_location(),
        Some(retained_location)
    );
    assert!(
        store
            .rf_trace(RfTracePageRequest::new(RfTraceScope::All, None, 10).unwrap())
            .unwrap()
            .events()
            .is_empty()
    );
}

#[test]
fn schema_eight_adds_nullable_message_locations_and_restart_preserves_new_snapshots() {
    let database = TestDatabase::new("schema-seven-message-location-migration");
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        store
            .commit_inbound(inbound(0xe1, 0xe2, 2_100, b"legacy inbound"))
            .unwrap();
        store
            .commit_outbound(outbound(0xe3, 0xe2, 2_200, b"legacy outbound"))
            .unwrap();
        store.close().unwrap();
    }
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE inbound_messages DROP COLUMN location_latitude_e6;\n\
             ALTER TABLE inbound_messages DROP COLUMN location_longitude_e6;\n\
             ALTER TABLE inbound_messages DROP COLUMN location_altitude_cm;\n\
             ALTER TABLE inbound_messages DROP COLUMN location_speed_cm_per_second;\n\
             ALTER TABLE inbound_messages DROP COLUMN location_bearing_centidegrees;\n\
             ALTER TABLE inbound_messages DROP COLUMN location_accuracy_cm;\n\
             ALTER TABLE inbound_messages DROP COLUMN location_updated_at_unix_seconds;\n\
             ALTER TABLE outbox DROP COLUMN location_latitude_e6;\n\
             ALTER TABLE outbox DROP COLUMN location_longitude_e6;\n\
             ALTER TABLE outbox DROP COLUMN location_altitude_cm;\n\
             ALTER TABLE outbox DROP COLUMN location_speed_cm_per_second;\n\
             ALTER TABLE outbox DROP COLUMN location_bearing_centidegrees;\n\
             ALTER TABLE outbox DROP COLUMN location_accuracy_cm;\n\
             ALTER TABLE outbox DROP COLUMN location_updated_at_unix_seconds;\n\
             PRAGMA user_version = 7;",
        )
        .unwrap();
    connection.close().unwrap();

    let inbound_location = message_location(43_123_456, 1_784_000_001);
    let outbound_location = message_location(44_654_321, 1_784_000_002);
    let outbox_id;
    {
        let mut migrated = SqliteChatStore::open(&database.path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
        let legacy = migrated.conversation_timeline(destination(0xe2)).unwrap();
        assert_eq!(legacy.len(), 2);
        assert!(legacy.iter().all(|entry| entry.location().is_none()));

        migrated
            .commit_inbound(
                inbound(0xe4, 0xe2, 2_300, b"located inbound")
                    .with_location(Some(inbound_location)),
            )
            .unwrap();
        outbox_id = migrated
            .commit_outbound(
                outbound(0xe5, 0xe2, 2_400, b"located outbound")
                    .with_location(Some(outbound_location)),
            )
            .unwrap()
            .outbox_id();
        let first = acceptance(0xe5, 0xe6);
        migrated.record_acceptance(outbox_id, first).unwrap();
        migrated
            .project_submission_status(
                first.submission_id(),
                SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            )
            .unwrap();
        migrated
            .retry_outbox(outbox_id, IdempotencyKey::new([0xe6; 16]))
            .unwrap();
        migrated.close().unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    let timeline = reopened.conversation_timeline(destination(0xe2)).unwrap();
    assert!(
        timeline
            .iter()
            .any(|entry| entry.location() == Some(inbound_location))
    );
    assert!(
        timeline
            .iter()
            .any(|entry| entry.location() == Some(outbound_location))
    );
    assert_eq!(
        reopened
            .outbox(outbox_id)
            .unwrap()
            .unwrap()
            .material()
            .location(),
        Some(outbound_location)
    );
    assert!(reopened.reconcile().unwrap().iter().any(|work| matches!(
        work,
        ReconcileWork::Submit { material, .. }
            if material.location() == Some(outbound_location)
    )));
}

#[test]
fn schema_nine_adds_receiver_locations_and_restart_preserves_first_import_fix() {
    let database = TestDatabase::new("schema-nine-receiver-location-migration");
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        store
            .commit_inbound(inbound(0xd1, 0xd2, 3_100, b"legacy inbound"))
            .unwrap();
        store.close().unwrap();
    }
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE inbound_messages DROP COLUMN receiver_location_latitude_e6;\n\
             ALTER TABLE inbound_messages DROP COLUMN receiver_location_longitude_e6;\n\
             ALTER TABLE inbound_messages DROP COLUMN receiver_location_horizontal_accuracy_mm;\n\
             ALTER TABLE inbound_messages DROP COLUMN receiver_location_altitude_mm;\n\
             ALTER TABLE inbound_messages DROP COLUMN receiver_location_vertical_accuracy_mm;\n\
             ALTER TABLE inbound_messages DROP COLUMN receiver_location_captured_at_unix_ms;\n\
             ALTER TABLE inbound_messages DROP COLUMN receiver_location_authorization;\n\
             ALTER TABLE inbound_messages DROP COLUMN receiver_location_source;\n\
             ALTER TABLE inbound_messages DROP COLUMN receiver_location_mocked;\n\
             PRAGMA user_version = 8;",
        )
        .unwrap();
    connection.close().unwrap();

    let sender_location = message_location(43_123_456, 1_784_000_003);
    let receiver_location = phone_location(44_654_321, 1_784_000_003_250);
    let conflicting_duplicate = phone_location(45_000_000, 1_784_000_009_999);
    {
        let mut migrated = SqliteChatStore::open(&database.path).unwrap();
        assert_eq!(migrated.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
        assert!(
            migrated.conversation_timeline(destination(0xd2)).unwrap()[0]
                .receiver_location()
                .is_none()
        );
        let message =
            inbound(0xd3, 0xd2, 3_200, b"located reception").with_location(Some(sender_location));
        assert_eq!(
            migrated
                .commit_inbound_with_receiver_location(message.clone(), Some(receiver_location),)
                .unwrap(),
            InboundCommitOutcome::Inserted
        );
        assert_eq!(
            migrated
                .commit_inbound_with_receiver_location(message, Some(conflicting_duplicate))
                .unwrap(),
            InboundCommitOutcome::Duplicate
        );
        migrated.close().unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    let entry = reopened
        .conversation_timeline(destination(0xd2))
        .unwrap()
        .into_iter()
        .find(|entry| entry.message_id() == Some(MessageId::new([0xd3; 32])))
        .unwrap();
    assert_eq!(entry.location(), Some(sender_location));
    assert_eq!(entry.receiver_location(), Some(receiver_location));
    let activity = reopened
        .message_activity(
            MessageActivityPageRequest::new(
                MessageActivityScope::Timeline(entry.sequence()),
                None,
                10,
            )
            .unwrap(),
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
}

#[test]
fn sqlite_rf_trace_round_trip_duplicate_and_restart_preserve_correlation() {
    let database = TestDatabase::new("rf-trace-restart");
    let retained_location = location(42_765_432, 4_000);
    let timeline_sequence;
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
        let outbox_id = store
            .commit_outbound_with_location(
                outbound(0xf3, 0x91, 3_000, b"durable trace"),
                retained_location,
            )
            .unwrap()
            .outbox_id();
        store
            .record_acceptance(outbox_id, acceptance(900, 0xf4))
            .unwrap();
        timeline_sequence = store.outbox(outbox_id).unwrap().unwrap().sequence();
        let batch = RfTraceImportBatch::new(
            RfTraceBootId::new(u64::MAX),
            rf_profile(0xf5),
            5_000,
            false,
            vec![rf_route(1, 0xb1, 900), rf_tx(2, 0xb1)],
        )
        .unwrap();
        let inserted = store.import_rf_trace_batch(batch.clone()).unwrap();
        assert_eq!((inserted.inserted(), inserted.existing()), (2, 0));
        let duplicate = store.import_rf_trace_batch(batch).unwrap();
        assert_eq!((duplicate.inserted(), duplicate.existing()), (0, 2));
        store.close().unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    let page = reopened
        .rf_trace(
            RfTracePageRequest::new(RfTraceScope::Timeline(timeline_sequence), None, 10).unwrap(),
        )
        .unwrap();
    assert_eq!(page.events().len(), 2);
    assert!(!page.history_incomplete());
    for event in page.events() {
        assert_eq!(event.boot_id(), RfTraceBootId::new(u64::MAX));
        let correlation = event.message_correlation().unwrap();
        assert_eq!(correlation.timeline_sequence(), timeline_sequence);
        assert_eq!(correlation.attempt_number().get(), 1);
        assert_eq!(correlation.attempt_location(), retained_location);
    }
    let tx = page
        .events()
        .iter()
        .find_map(|event| match event.observation().kind() {
            RfTraceObservationKind::DataTx(tx) => Some(tx),
            _ => None,
        })
        .unwrap();
    assert_eq!(tx.frame_completed_at_us(), [Some(1_800), Some(1_900)]);
}
