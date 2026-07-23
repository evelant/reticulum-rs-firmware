#![cfg(feature = "sqlite")]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use reticulum_lxmf_chat_core::{
    AcceptanceIds, ChatStore, Contact, DestinationHash, DeviceBinding, DeviceBindingOutcome,
    EncodedPacketSha256, IdempotencyKey, InboundCommitOutcome, InboundMessage, MessageId,
    OutboxMaterial, OutboxStatus, PacketEvidence, ReconcileWork, SQLITE_SCHEMA_VERSION,
    SqliteChatStore, SqliteStoreError, SubmissionId, SubmissionState, TimelineDirection,
    UnixTimestampMillis,
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

fn binding(tag: u8) -> DeviceBinding {
    DeviceBinding::new(
        [tag; 16],
        destination(tag.wrapping_add(1)),
        destination(tag.wrapping_add(2)),
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
fn schema_one_database_migrates_to_unbound_schema_two() {
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
