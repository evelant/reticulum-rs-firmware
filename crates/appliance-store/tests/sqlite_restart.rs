#![cfg(feature = "sqlite")]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use reticulum_appliance_store::{
    AcceptanceIds, AttemptLocationStamp, ChatStore, Contact, DestinationHash, DeviceBinding,
    DeviceBindingOutcome, EncodedPacketSha256, IdempotencyKey, InboundCommitOutcome,
    InboundMessage, MessageActivityKind, MessageActivityPageRequest, MessageActivityScope,
    MessageId, OutboxCommitOutcome, OutboxMaterial, OutboxRetryOutcome, OutboxStatus,
    PacketEvidence, PhoneLocationAuthorization, PhoneLocationSample, PhoneLocationSource,
    PhoneLocationUnavailableReason, ReconcileWork, RfTraceBootId, RfTraceEventSequence,
    RfTraceImportBatch, RfTraceInboundProofObservation, RfTraceInboundProofStage,
    RfTraceInterfaceId, RfTraceObservation, RfTraceObservationKind, RfTracePageRequest,
    RfTraceRadioProfile, RfTraceRouteObservation, RfTraceRouteResolution, RfTraceScope,
    RfTraceTxObservation, RfTraceTxOutcome, RnsAttemptToken, SQLITE_SCHEMA_VERSION,
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
            "reticulum-appliance-{label}-{}-{nonce}-{sequence}.sqlite3",
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

fn rf_inbound_proof(sequence: u64, token: u8) -> RfTraceObservation {
    RfTraceObservation::new(
        RfTraceEventSequence::new(sequence).unwrap(),
        sequence * 1_000,
        RfTraceObservationKind::InboundProof(
            RfTraceInboundProofObservation::new(
                RnsAttemptToken::new([token; 32]),
                RfTraceInboundProofStage::PhysicalTxFailed,
                Some(MessageId::new([0xc1; 32])),
                Some(evidence(0xc2)),
                Some(RfTraceInterfaceId::new(1)),
                Some((-104, 7)),
                Some(RfTraceTxOutcome::TxFault),
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
fn non_current_schema_is_rejected_without_mutation() {
    let database = TestDatabase::new("non-current-schema");
    {
        let connection = rusqlite::Connection::open(&database.path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE contacts (destination BLOB PRIMARY KEY, display_name TEXT NOT NULL);\n\
                 INSERT INTO contacts(destination, display_name) VALUES (x'01010101010101010101010101010101', 'obsolete');\n\
                 CREATE TABLE outbox (obsolete INTEGER);\n\
                 CREATE TABLE message_activity (obsolete INTEGER);",
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION - 2)
            .unwrap();
    }

    assert!(matches!(
        SqliteChatStore::open(&database.path),
        Err(SqliteStoreError::UnsupportedSchemaVersion(version))
            if version == SQLITE_SCHEMA_VERSION - 2
    ));

    let connection = rusqlite::Connection::open(&database.path).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        SQLITE_SCHEMA_VERSION - 2
    );
    assert_eq!(
        connection
            .query_row("SELECT display_name FROM contacts", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "obsolete"
    );
    let mut statement = connection.prepare("PRAGMA table_info(outbox)").unwrap();
    let outbox_columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(outbox_columns, vec!["obsolete"]);
}

#[test]
fn schema_ten_rf_trace_rows_migrate_to_eleven_without_data_loss() {
    let database = TestDatabase::new("schema-ten-rf-trace-migration");
    let expected = rf_tx(1, 0xa1);
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        let batch = RfTraceImportBatch::new(
            RfTraceBootId::new(0xa2),
            rf_profile(0xa3),
            2_000,
            false,
            vec![expected],
        )
        .unwrap();
        assert_eq!(store.import_rf_trace_batch(batch).unwrap().inserted(), 1);
        store.close().unwrap();
    }

    {
        let connection = rusqlite::Connection::open(&database.path).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE rf_trace_events DROP COLUMN inbound_message_id;\n\
                 ALTER TABLE rf_trace_events DROP COLUMN inbound_stage;",
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", 10_u32)
            .unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);
    let page = reopened
        .rf_trace(RfTracePageRequest::new(RfTraceScope::All, None, 10).unwrap())
        .unwrap();
    assert_eq!(page.events().len(), 1);
    assert_eq!(page.events()[0].observation(), expected);

    let connection = rusqlite::Connection::open(&database.path).unwrap();
    let mut statement = connection
        .prepare("PRAGMA table_info(rf_trace_events)")
        .unwrap();
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(columns.iter().any(|name| name == "inbound_stage"));
    assert!(columns.iter().any(|name| name == "inbound_message_id"));
}

#[test]
fn unversioned_nonempty_database_is_rejected_without_mutation() {
    let database = TestDatabase::new("unversioned-nonempty");
    {
        let connection = rusqlite::Connection::open(&database.path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE unrelated (value TEXT NOT NULL);\n\
                 INSERT INTO unrelated(value) VALUES ('preserved');",
            )
            .unwrap();
    }

    assert!(matches!(
        SqliteChatStore::open(&database.path),
        Err(SqliteStoreError::UnsupportedSchemaVersion(0))
    ));
    let connection = rusqlite::Connection::open(&database.path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT value FROM unrelated", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "preserved"
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
        assert!(
            page.events()
                .iter()
                .any(|event| matches!(event.kind(), MessageActivityKind::OutboundRequeued { .. }))
        );
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

#[test]
fn sqlite_rf_trace_restart_preserves_inbound_proof_lifecycle_evidence() {
    let database = TestDatabase::new("rf-trace-inbound-proof");
    {
        let mut store = SqliteChatStore::open(&database.path).unwrap();
        let batch = RfTraceImportBatch::new(
            RfTraceBootId::new(0xf1),
            rf_profile(0xf2),
            6_000,
            false,
            vec![rf_inbound_proof(1, 0xf3)],
        )
        .unwrap();
        assert_eq!(store.import_rf_trace_batch(batch).unwrap().inserted(), 1);
        store.close().unwrap();
    }

    let reopened = SqliteChatStore::open(&database.path).unwrap();
    let page = reopened
        .rf_trace(RfTracePageRequest::new(RfTraceScope::All, None, 10).unwrap())
        .unwrap();
    let [event] = page.events() else {
        panic!("one inbound proof event must survive restart");
    };
    let RfTraceObservationKind::InboundProof(proof) = event.observation().kind() else {
        panic!("the persisted event must retain its inbound proof kind");
    };
    assert_eq!(proof.stage(), RfTraceInboundProofStage::PhysicalTxFailed);
    assert_eq!(proof.message_id(), Some(MessageId::new([0xc1; 32])));
    assert_eq!(proof.packet_evidence(), Some(evidence(0xc2)));
    assert_eq!(proof.interface(), Some(RfTraceInterfaceId::new(1)));
    assert_eq!(proof.signal(), Some((-104, 7)));
    assert_eq!(proof.dispatch_outcome(), Some(RfTraceTxOutcome::TxFault));
}
