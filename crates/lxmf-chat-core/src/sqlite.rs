use core::fmt;
use std::path::Path;
use std::string::String;
use std::vec::Vec;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::store::project_outbox_status;
use crate::{
    AcceptanceIds, AcceptanceOutcome, ChatStore, ChatStoreError, Contact, ContactUpsertOutcome,
    DestinationHash, DeviceBinding, DeviceBindingOutcome, EncodedPacketSha256, IdempotencyKey,
    InboundCommitOutcome, InboundMessage, InboundRecord, MessageId, OutboxCommitOutcome, OutboxId,
    OutboxMaterial, OutboxRecord, OutboxStatus, PacketEvidence, ReconcileWork,
    StatusProjectionOutcome, SubmissionFailure, SubmissionId, SubmissionState, TimelineEntry,
    TimelineSequence, UnixTimestampMillis,
};

/// Current SQLite `PRAGMA user_version` owned by this adapter.
pub const SQLITE_SCHEMA_VERSION: u32 = 2;

const OUTBOX_COLUMNS: &str = "id, sequence, destination, timestamp_unix_ms, idempotency_key, \
                             title, content, submission_id, message_id, status_kind, \
                             failure_kind, packet_len, packet_sha256";

/// SQLite adapter failure without leaking database types into the domain API.
#[derive(Debug)]
pub enum SqliteStoreError {
    /// A database operation failed.
    Database(rusqlite::Error),
    /// Database contents violated the versioned chat schema.
    CorruptData(&'static str),
    /// The database was created by a newer or unsupported schema.
    UnsupportedSchemaVersion(u32),
    /// A valid domain value cannot fit SQLite's signed integer representation.
    ValueOutOfRange(&'static str),
    /// Shared chat-domain semantics rejected the mutation.
    Domain(ChatStoreError),
    /// The database is already bound to a different authenticated device.
    DeviceBindingMismatch {
        /// Identity retained by the database.
        expected: DeviceBinding,
        /// Identity presented by the connected device.
        observed: DeviceBinding,
    },
}

impl fmt::Display for SqliteStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "SQLite operation failed: {error}"),
            Self::CorruptData(field) => write!(formatter, "SQLite chat data is invalid: {field}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "unsupported SQLite chat schema version {version}"
                )
            }
            Self::ValueOutOfRange(field) => {
                write!(formatter, "chat value cannot fit SQLite INTEGER: {field}")
            }
            Self::Domain(error) => write!(formatter, "chat domain mutation failed: {error}"),
            Self::DeviceBindingMismatch { .. } => {
                formatter.write_str("chat database is bound to a different device")
            }
        }
    }
}

impl std::error::Error for SqliteStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::CorruptData(_)
            | Self::UnsupportedSchemaVersion(_)
            | Self::ValueOutOfRange(_)
            | Self::Domain(_)
            | Self::DeviceBindingMismatch { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for SqliteStoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}

impl From<ChatStoreError> for SqliteStoreError {
    fn from(value: ChatStoreError) -> Self {
        Self::Domain(value)
    }
}

/// Transactional SQLite implementation of [`ChatStore`].
///
/// The adapter owns a single connection. Every mutation uses an immediate
/// transaction, including semantic idempotency checks and monotonic counter
/// allocation, so a crash cannot expose an acceptance without both IDs or an
/// outbox identifier without its exact commit-before-send material.
pub struct SqliteChatStore {
    connection: Connection,
}

impl SqliteChatStore {
    /// Open or create a file-backed chat database and apply the supported
    /// schema migration from version zero.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        Self::from_connection(Connection::open(path)?)
    }

    /// Open an in-memory database with the same schema and transaction paths.
    pub fn open_in_memory() -> Result<Self, SqliteStoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, SqliteStoreError> {
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;\n\
             PRAGMA trusted_schema = OFF;",
        )?;
        initialize_schema(&mut connection)?;
        Ok(Self { connection })
    }

    /// Return the database's current schema version.
    pub fn schema_version(&self) -> Result<u32, SqliteStoreError> {
        let version = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
        Ok(version)
    }

    /// Return the authenticated device identity retained by this database.
    pub fn device_binding(&self) -> Result<Option<DeviceBinding>, SqliteStoreError> {
        self.connection
            .query_row(
                "SELECT device_id, primary_destination, lxmf_delivery_destination\n\
                 FROM device_binding WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(device_id, primary, lxmf)| -> Result<DeviceBinding, SqliteStoreError> {
                    Ok(DeviceBinding::new(
                        array_from_blob(device_id, "device binding device_id")?,
                        DestinationHash::new(array_from_blob(
                            primary,
                            "device binding primary_destination",
                        )?),
                        DestinationHash::new(array_from_blob(
                            lxmf,
                            "device binding lxmf_delivery_destination",
                        )?),
                    ))
                },
            )
            .transpose()
    }

    /// Bind an unbound database to one authenticated device, or verify the
    /// exact existing binding without mutation.
    pub fn bind_device(
        &mut self,
        observed: DeviceBinding,
    ) -> Result<DeviceBindingOutcome, SqliteStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT device_id, primary_destination, lxmf_delivery_destination\n\
                 FROM device_binding WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(device_id, primary, lxmf)| -> Result<DeviceBinding, SqliteStoreError> {
                    Ok(DeviceBinding::new(
                        array_from_blob(device_id, "device binding device_id")?,
                        DestinationHash::new(array_from_blob(
                            primary,
                            "device binding primary_destination",
                        )?),
                        DestinationHash::new(array_from_blob(
                            lxmf,
                            "device binding lxmf_delivery_destination",
                        )?),
                    ))
                },
            )
            .transpose()?;
        if let Some(expected) = existing {
            if expected != observed {
                return Err(SqliteStoreError::DeviceBindingMismatch { expected, observed });
            }
            transaction.commit()?;
            return Ok(DeviceBindingOutcome::Unchanged);
        }
        transaction.execute(
            "INSERT INTO device_binding(\n\
                 singleton, device_id, primary_destination, lxmf_delivery_destination\n\
             ) VALUES (1, ?1, ?2, ?3)",
            params![
                observed.device_id().as_slice(),
                observed.primary_destination().as_bytes().as_slice(),
                observed.lxmf_delivery_destination().as_bytes().as_slice(),
            ],
        )?;
        transaction.commit()?;
        Ok(DeviceBindingOutcome::Bound)
    }

    /// Explicitly close the connection, flushing SQLite-owned resources.
    pub fn close(self) -> Result<(), SqliteStoreError> {
        match self.connection.close() {
            Ok(()) => Ok(()),
            Err((_connection, error)) => Err(SqliteStoreError::Database(error)),
        }
    }
}

fn initialize_schema(connection: &mut Connection) -> Result<(), SqliteStoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = transaction.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
    match version {
        SQLITE_SCHEMA_VERSION => {}
        0 => {
            transaction.execute_batch(
                "CREATE TABLE chat_meta (\n\
                     name TEXT PRIMARY KEY NOT NULL,\n\
                     next_value INTEGER NOT NULL CHECK (next_value > 0)\n\
                 );\n\
                 INSERT INTO chat_meta(name, next_value) VALUES\n\
                     ('outbox_id', 1), ('timeline_sequence', 1);\n\
                 CREATE TABLE contacts (\n\
                     destination BLOB PRIMARY KEY NOT NULL CHECK (length(destination) = 16),\n\
                     display_name TEXT NOT NULL\n\
                 );\n\
                 CREATE TABLE inbound_messages (\n\
                     message_id BLOB PRIMARY KEY NOT NULL CHECK (length(message_id) = 32),\n\
                     sequence INTEGER UNIQUE NOT NULL CHECK (sequence > 0),\n\
                     local_destination BLOB NOT NULL CHECK (length(local_destination) = 16),\n\
                     source BLOB NOT NULL CHECK (length(source) = 16),\n\
                     timestamp_unix_ms INTEGER NOT NULL CHECK (timestamp_unix_ms > 0),\n\
                     title BLOB NOT NULL,\n\
                     content BLOB NOT NULL\n\
                 );\n\
                 CREATE TABLE outbox (\n\
                     id INTEGER PRIMARY KEY NOT NULL CHECK (id > 0),\n\
                     sequence INTEGER UNIQUE NOT NULL CHECK (sequence > 0),\n\
                     destination BLOB NOT NULL CHECK (length(destination) = 16),\n\
                     timestamp_unix_ms INTEGER NOT NULL CHECK (timestamp_unix_ms > 0),\n\
                     idempotency_key BLOB UNIQUE NOT NULL CHECK (length(idempotency_key) = 16),\n\
                     title BLOB NOT NULL,\n\
                     content BLOB NOT NULL,\n\
                     submission_id INTEGER UNIQUE CHECK (submission_id > 0),\n\
                     message_id BLOB UNIQUE CHECK (message_id IS NULL OR length(message_id) = 32),\n\
                     status_kind INTEGER NOT NULL CHECK (status_kind BETWEEN 0 AND 7),\n\
                     failure_kind INTEGER CHECK (failure_kind BETWEEN 0 AND 3),\n\
                     packet_len INTEGER CHECK (packet_len > 0 AND packet_len <= 65535),\n\
                     packet_sha256 BLOB CHECK (packet_sha256 IS NULL OR length(packet_sha256) = 32),\n\
                     CHECK ((status_kind = 0 AND submission_id IS NULL AND message_id IS NULL) OR\n\
                            (status_kind BETWEEN 1 AND 7 AND submission_id IS NOT NULL AND message_id IS NOT NULL)),\n\
                     CHECK ((status_kind IN (4, 5) AND packet_len IS NOT NULL AND packet_sha256 IS NOT NULL) OR\n\
                            (status_kind NOT IN (4, 5) AND packet_len IS NULL AND packet_sha256 IS NULL)),\n\
                     CHECK ((status_kind = 6 AND failure_kind IS NOT NULL) OR\n\
                            (status_kind != 6 AND failure_kind IS NULL))\n\
                 );\n\
                 CREATE TABLE device_binding (\n\
                     singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),\n\
                     device_id BLOB UNIQUE NOT NULL CHECK (length(device_id) = 16),\n\
                     primary_destination BLOB NOT NULL CHECK (length(primary_destination) = 16),\n\
                     lxmf_delivery_destination BLOB NOT NULL CHECK (length(lxmf_delivery_destination) = 16)\n\
                 );\n\
                 PRAGMA user_version = 2;",
            )?;
        }
        1 => {
            transaction.execute_batch(
                "CREATE TABLE device_binding (\n\
                     singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),\n\
                     device_id BLOB UNIQUE NOT NULL CHECK (length(device_id) = 16),\n\
                     primary_destination BLOB NOT NULL CHECK (length(primary_destination) = 16),\n\
                     lxmf_delivery_destination BLOB NOT NULL CHECK (length(lxmf_delivery_destination) = 16)\n\
                 );\n\
                 PRAGMA user_version = 2;",
            )?;
        }
        unsupported => {
            return Err(SqliteStoreError::UnsupportedSchemaVersion(unsupported));
        }
    }
    transaction.commit()?;
    Ok(())
}

fn array_from_blob<const N: usize>(
    bytes: Vec<u8>,
    field: &'static str,
) -> Result<[u8; N], SqliteStoreError> {
    bytes
        .try_into()
        .map_err(|_| SqliteStoreError::CorruptData(field))
}

fn positive_u64(value: i64, field: &'static str) -> Result<u64, SqliteStoreError> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value != 0)
        .ok_or(SqliteStoreError::CorruptData(field))
}

fn sqlite_integer(value: u64, field: &'static str) -> Result<i64, SqliteStoreError> {
    i64::try_from(value).map_err(|_| SqliteStoreError::ValueOutOfRange(field))
}

fn allocate_counter(
    transaction: &Transaction<'_>,
    name: &'static str,
) -> Result<u64, SqliteStoreError> {
    let current = transaction.query_row(
        "SELECT next_value FROM chat_meta WHERE name = ?1",
        [name],
        |row| row.get::<_, i64>(0),
    )?;
    let current = positive_u64(current, "chat_meta.next_value")?;
    let next = current
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or(ChatStoreError::IdentifierExhausted)?;
    let updated = transaction.execute(
        "UPDATE chat_meta SET next_value = ?1 WHERE name = ?2 AND next_value = ?3",
        params![
            sqlite_integer(next, "chat_meta.next_value")?,
            name,
            sqlite_integer(current, "chat_meta.next_value")?
        ],
    )?;
    if updated != 1 {
        return Err(SqliteStoreError::CorruptData("chat_meta counter"));
    }
    Ok(current)
}

fn raw_inbound(row: &Row<'_>) -> rusqlite::Result<RawInbound> {
    Ok(RawInbound {
        message_id: row.get(0)?,
        sequence: row.get(1)?,
        local_destination: row.get(2)?,
        source: row.get(3)?,
        timestamp_unix_ms: row.get(4)?,
        title: row.get(5)?,
        content: row.get(6)?,
    })
}

struct RawInbound {
    message_id: Vec<u8>,
    sequence: i64,
    local_destination: Vec<u8>,
    source: Vec<u8>,
    timestamp_unix_ms: i64,
    title: Vec<u8>,
    content: Vec<u8>,
}

fn decode_inbound(raw: RawInbound) -> Result<InboundRecord, SqliteStoreError> {
    let sequence = TimelineSequence::new(positive_u64(raw.sequence, "inbound sequence")?)
        .ok_or(SqliteStoreError::CorruptData("inbound sequence"))?;
    let timestamp =
        UnixTimestampMillis::new(positive_u64(raw.timestamp_unix_ms, "inbound timestamp")?)
            .map_err(|_| SqliteStoreError::CorruptData("inbound timestamp"))?;
    Ok(InboundRecord {
        sequence,
        message: InboundMessage::new(
            MessageId::new(array_from_blob(raw.message_id, "inbound message_id")?),
            DestinationHash::new(array_from_blob(
                raw.local_destination,
                "inbound local_destination",
            )?),
            DestinationHash::new(array_from_blob(raw.source, "inbound source")?),
            timestamp,
            raw.title,
            raw.content,
        ),
    })
}

struct RawOutbox {
    id: i64,
    sequence: i64,
    destination: Vec<u8>,
    timestamp_unix_ms: i64,
    idempotency_key: Vec<u8>,
    title: Vec<u8>,
    content: Vec<u8>,
    submission_id: Option<i64>,
    message_id: Option<Vec<u8>>,
    status_kind: i64,
    failure_kind: Option<i64>,
    packet_len: Option<i64>,
    packet_sha256: Option<Vec<u8>>,
}

fn raw_outbox(row: &Row<'_>) -> rusqlite::Result<RawOutbox> {
    Ok(RawOutbox {
        id: row.get(0)?,
        sequence: row.get(1)?,
        destination: row.get(2)?,
        timestamp_unix_ms: row.get(3)?,
        idempotency_key: row.get(4)?,
        title: row.get(5)?,
        content: row.get(6)?,
        submission_id: row.get(7)?,
        message_id: row.get(8)?,
        status_kind: row.get(9)?,
        failure_kind: row.get(10)?,
        packet_len: row.get(11)?,
        packet_sha256: row.get(12)?,
    })
}

fn decode_outbox(raw: RawOutbox) -> Result<OutboxRecord, SqliteStoreError> {
    let id = OutboxId::new(positive_u64(raw.id, "outbox id")?)
        .ok_or(SqliteStoreError::CorruptData("outbox id"))?;
    let sequence = TimelineSequence::new(positive_u64(raw.sequence, "outbox sequence")?)
        .ok_or(SqliteStoreError::CorruptData("outbox sequence"))?;
    let timestamp =
        UnixTimestampMillis::new(positive_u64(raw.timestamp_unix_ms, "outbox timestamp")?)
            .map_err(|_| SqliteStoreError::CorruptData("outbox timestamp"))?;
    let acceptance = match (raw.submission_id, raw.message_id) {
        (None, None) => None,
        (Some(submission_id), Some(message_id)) => Some(AcceptanceIds::new(
            SubmissionId::new(positive_u64(submission_id, "outbox submission_id")?)
                .map_err(|_| SqliteStoreError::CorruptData("outbox submission_id"))?,
            MessageId::new(array_from_blob(message_id, "outbox message_id")?),
        )),
        _ => return Err(SqliteStoreError::CorruptData("outbox acceptance pair")),
    };
    let status = decode_status(
        raw.status_kind,
        raw.failure_kind,
        raw.packet_len,
        raw.packet_sha256,
    )?;
    match (acceptance, status) {
        (None, OutboxStatus::Committed) => {}
        (Some(_), OutboxStatus::Accepted | OutboxStatus::Device(_)) => {}
        _ => return Err(SqliteStoreError::CorruptData("outbox acceptance/status")),
    }
    Ok(OutboxRecord {
        id,
        sequence,
        material: OutboxMaterial::new(
            DestinationHash::new(array_from_blob(raw.destination, "outbox destination")?),
            timestamp,
            IdempotencyKey::new(array_from_blob(
                raw.idempotency_key,
                "outbox idempotency_key",
            )?),
            raw.title,
            raw.content,
        ),
        acceptance,
        status,
    })
}

fn decode_status(
    kind: i64,
    failure: Option<i64>,
    packet_len: Option<i64>,
    packet_sha256: Option<Vec<u8>>,
) -> Result<OutboxStatus, SqliteStoreError> {
    let evidence = match (packet_len, packet_sha256) {
        (None, None) => None,
        (Some(length), Some(digest)) => {
            let length = u16::try_from(length)
                .ok()
                .filter(|length| *length != 0)
                .ok_or(SqliteStoreError::CorruptData("outbox packet_len"))?;
            Some(
                PacketEvidence::new(
                    length,
                    EncodedPacketSha256::new(array_from_blob(digest, "outbox packet_sha256")?),
                )
                .map_err(|_| SqliteStoreError::CorruptData("outbox packet evidence"))?,
            )
        }
        _ => return Err(SqliteStoreError::CorruptData("outbox packet evidence pair")),
    };
    let status = match kind {
        0 if failure.is_none() && evidence.is_none() => OutboxStatus::Committed,
        1 if failure.is_none() && evidence.is_none() => OutboxStatus::Accepted,
        2 if failure.is_none() && evidence.is_none() => {
            OutboxStatus::Device(SubmissionState::Queued)
        }
        3 if failure.is_none() && evidence.is_none() => {
            OutboxStatus::Device(SubmissionState::Preparing)
        }
        4 if failure.is_none() => OutboxStatus::Device(SubmissionState::AwaitingDelivery(
            evidence.ok_or(SqliteStoreError::CorruptData("awaiting packet evidence"))?,
        )),
        5 if failure.is_none() => OutboxStatus::Device(SubmissionState::Delivered(
            evidence.ok_or(SqliteStoreError::CorruptData("delivered packet evidence"))?,
        )),
        6 if evidence.is_none() => OutboxStatus::Device(SubmissionState::Failed(match failure {
            Some(0) => SubmissionFailure::NoPath,
            Some(1) => SubmissionFailure::DeliveryTimeout,
            Some(2) => SubmissionFailure::DownstreamRejection,
            Some(3) => SubmissionFailure::Internal,
            _ => return Err(SqliteStoreError::CorruptData("outbox failure_kind")),
        })),
        7 if failure.is_none() && evidence.is_none() => {
            OutboxStatus::Device(SubmissionState::Cancelled)
        }
        _ => return Err(SqliteStoreError::CorruptData("outbox status")),
    };
    Ok(status)
}

fn encode_status(status: OutboxStatus) -> (i64, Option<i64>, Option<i64>, Option<Vec<u8>>) {
    match status {
        OutboxStatus::Committed => (0, None, None, None),
        OutboxStatus::Accepted => (1, None, None, None),
        OutboxStatus::Device(SubmissionState::Queued) => (2, None, None, None),
        OutboxStatus::Device(SubmissionState::Preparing) => (3, None, None, None),
        OutboxStatus::Device(SubmissionState::AwaitingDelivery(evidence)) => (
            4,
            None,
            Some(i64::from(evidence.encoded_packet_len())),
            Some(evidence.encoded_packet_sha256().as_bytes().to_vec()),
        ),
        OutboxStatus::Device(SubmissionState::Delivered(evidence)) => (
            5,
            None,
            Some(i64::from(evidence.encoded_packet_len())),
            Some(evidence.encoded_packet_sha256().as_bytes().to_vec()),
        ),
        OutboxStatus::Device(SubmissionState::Failed(failure)) => (
            6,
            Some(match failure {
                SubmissionFailure::NoPath => 0,
                SubmissionFailure::DeliveryTimeout => 1,
                SubmissionFailure::DownstreamRejection => 2,
                SubmissionFailure::Internal => 3,
            }),
            None,
            None,
        ),
        OutboxStatus::Device(SubmissionState::Cancelled) => (7, None, None, None),
    }
}

fn query_outbox_by_id(
    connection: &Connection,
    outbox_id: OutboxId,
) -> Result<Option<OutboxRecord>, SqliteStoreError> {
    let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE id = ?1");
    let raw = connection
        .query_row(
            &sql,
            [sqlite_integer(outbox_id.get(), "outbox id")?],
            raw_outbox,
        )
        .optional()?;
    raw.map(decode_outbox).transpose()
}

impl ChatStore for SqliteChatStore {
    type Error = SqliteStoreError;

    fn upsert_contact(&mut self, contact: Contact) -> Result<ContactUpsertOutcome, Self::Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT display_name FROM contacts WHERE destination = ?1",
                [contact.destination().as_bytes().as_slice()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let outcome = match existing.as_deref() {
            None => ContactUpsertOutcome::Inserted,
            Some(name) if name == contact.display_name() => ContactUpsertOutcome::Unchanged,
            Some(_) => ContactUpsertOutcome::Updated,
        };
        if outcome != ContactUpsertOutcome::Unchanged {
            transaction.execute(
                "INSERT INTO contacts(destination, display_name) VALUES (?1, ?2)\n\
                 ON CONFLICT(destination) DO UPDATE SET display_name = excluded.display_name",
                params![
                    contact.destination().as_bytes().as_slice(),
                    contact.display_name()
                ],
            )?;
        }
        transaction.commit()?;
        Ok(outcome)
    }

    fn contact(&self, destination: DestinationHash) -> Result<Option<Contact>, Self::Error> {
        let name = self
            .connection
            .query_row(
                "SELECT display_name FROM contacts WHERE destination = ?1",
                [destination.as_bytes().as_slice()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(name.map(|name| Contact::new(destination, name)))
    }

    fn contacts(&self) -> Result<Vec<Contact>, Self::Error> {
        let mut statement = self
            .connection
            .prepare("SELECT destination, display_name FROM contacts ORDER BY destination ASC")?;
        let mut rows = statement.query([])?;
        let mut contacts = Vec::new();
        while let Some(row) = rows.next()? {
            let destination: Vec<u8> = row.get(0)?;
            let name: String = row.get(1)?;
            contacts.push(Contact::new(
                DestinationHash::new(array_from_blob(destination, "contact destination")?),
                name,
            ));
        }
        Ok(contacts)
    }

    fn contains_inbound(&self, message_id: MessageId) -> Result<bool, Self::Error> {
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM inbound_messages WHERE message_id = ?1",
                [message_id.as_bytes().as_slice()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    fn commit_inbound(
        &mut self,
        message: InboundMessage,
    ) -> Result<InboundCommitOutcome, Self::Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT message_id, sequence, local_destination, source, timestamp_unix_ms, title, content\n\
                 FROM inbound_messages WHERE message_id = ?1",
                [message.message_id().as_bytes().as_slice()],
                raw_inbound,
            )
            .optional()?;
        if let Some(existing) = existing {
            let existing = decode_inbound(existing)?;
            if existing.message() == &message {
                transaction.commit()?;
                return Ok(InboundCommitOutcome::Duplicate);
            }
            return Err(ChatStoreError::InboundMessageIdConflict(message.message_id()).into());
        }
        let sequence = allocate_counter(&transaction, "timeline_sequence")?;
        transaction.execute(
            "INSERT INTO inbound_messages(\n\
                 message_id, sequence, local_destination, source, timestamp_unix_ms, title, content\n\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                message.message_id().as_bytes().as_slice(),
                sqlite_integer(sequence, "inbound sequence")?,
                message.local_destination().as_bytes().as_slice(),
                message.source().as_bytes().as_slice(),
                sqlite_integer(message.timestamp().get(), "inbound timestamp")?,
                message.title(),
                message.content(),
            ],
        )?;
        transaction.commit()?;
        Ok(InboundCommitOutcome::Inserted)
    }

    fn commit_outbound(
        &mut self,
        material: OutboxMaterial,
    ) -> Result<OutboxCommitOutcome, Self::Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE idempotency_key = ?1");
        let existing = transaction
            .query_row(
                &sql,
                [material.idempotency_key().as_bytes().as_slice()],
                raw_outbox,
            )
            .optional()?;
        if let Some(existing) = existing {
            let existing = decode_outbox(existing)?;
            if existing.material() == &material {
                let id = existing.id();
                transaction.commit()?;
                return Ok(OutboxCommitOutcome::Existing(id));
            }
            return Err(ChatStoreError::IdempotencyConflict.into());
        }
        let id = allocate_counter(&transaction, "outbox_id")?;
        let sequence = allocate_counter(&transaction, "timeline_sequence")?;
        transaction.execute(
            "INSERT INTO outbox(\n\
                 id, sequence, destination, timestamp_unix_ms, idempotency_key, title, content,\n\
                 submission_id, message_id, status_kind, failure_kind, packet_len, packet_sha256\n\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 0, NULL, NULL, NULL)",
            params![
                sqlite_integer(id, "outbox id")?,
                sqlite_integer(sequence, "outbox sequence")?,
                material.destination().as_bytes().as_slice(),
                sqlite_integer(material.timestamp().get(), "outbox timestamp")?,
                material.idempotency_key().as_bytes().as_slice(),
                material.title(),
                material.content(),
            ],
        )?;
        transaction.commit()?;
        Ok(OutboxCommitOutcome::Inserted(
            OutboxId::new(id).ok_or(ChatStoreError::IdentifierExhausted)?,
        ))
    }

    fn record_acceptance(
        &mut self,
        outbox_id: OutboxId,
        acceptance: AcceptanceIds,
    ) -> Result<AcceptanceOutcome, Self::Error> {
        let submission_id = sqlite_integer(acceptance.submission_id().get(), "submission id")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE id = ?1");
        let record = transaction
            .query_row(
                &sql,
                [sqlite_integer(outbox_id.get(), "outbox id")?],
                raw_outbox,
            )
            .optional()?
            .map(decode_outbox)
            .transpose()?
            .ok_or(ChatStoreError::OutboxNotFound(outbox_id))?;
        if let Some(existing) = record.acceptance() {
            if existing == acceptance {
                transaction.commit()?;
                return Ok(AcceptanceOutcome::Unchanged);
            }
            return Err(ChatStoreError::AcceptanceConflict(outbox_id).into());
        }
        let conflict = transaction
            .query_row(
                "SELECT id FROM outbox\n\
                 WHERE (submission_id = ?1 OR message_id = ?2) AND id != ?3 LIMIT 1",
                params![
                    submission_id,
                    acceptance.message_id().as_bytes().as_slice(),
                    sqlite_integer(outbox_id.get(), "outbox id")?
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if conflict.is_some() {
            return Err(ChatStoreError::AcceptanceIdAlreadyBound.into());
        }
        let updated = transaction.execute(
            "UPDATE outbox SET submission_id = ?1, message_id = ?2, status_kind = 1\n\
             WHERE id = ?3 AND submission_id IS NULL AND message_id IS NULL AND status_kind = 0",
            params![
                submission_id,
                acceptance.message_id().as_bytes().as_slice(),
                sqlite_integer(outbox_id.get(), "outbox id")?
            ],
        )?;
        if updated != 1 {
            return Err(SqliteStoreError::CorruptData("outbox acceptance update"));
        }
        transaction.commit()?;
        Ok(AcceptanceOutcome::Recorded)
    }

    fn project_submission_status(
        &mut self,
        submission_id: SubmissionId,
        state: SubmissionState,
    ) -> Result<StatusProjectionOutcome, Self::Error> {
        let submission = sqlite_integer(submission_id.get(), "submission id")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE submission_id = ?1");
        let record = transaction
            .query_row(&sql, [submission], raw_outbox)
            .optional()?
            .map(decode_outbox)
            .transpose()?
            .ok_or(ChatStoreError::SubmissionNotFound(submission_id))?;
        let (next, outcome) = project_outbox_status(record.status(), state)?;
        if outcome == StatusProjectionOutcome::Advanced {
            let (kind, failure, packet_len, packet_sha256) = encode_status(next);
            let updated = transaction.execute(
                "UPDATE outbox SET status_kind = ?1, failure_kind = ?2, packet_len = ?3, packet_sha256 = ?4\n\
                 WHERE submission_id = ?5",
                params![kind, failure, packet_len, packet_sha256, submission],
            )?;
            if updated != 1 {
                return Err(SqliteStoreError::CorruptData("outbox status update"));
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    fn outbox(&self, outbox_id: OutboxId) -> Result<Option<OutboxRecord>, Self::Error> {
        query_outbox_by_id(&self.connection, outbox_id)
    }

    fn conversation_timeline(
        &self,
        peer: DestinationHash,
    ) -> Result<Vec<TimelineEntry>, Self::Error> {
        let mut timeline = Vec::new();
        {
            let mut statement = self.connection.prepare(
                "SELECT message_id, sequence, local_destination, source, timestamp_unix_ms, title, content\n\
                 FROM inbound_messages WHERE source = ?1",
            )?;
            let mut rows = statement.query([peer.as_bytes().as_slice()])?;
            while let Some(row) = rows.next()? {
                timeline.push(TimelineEntry::inbound(&decode_inbound(raw_inbound(row)?)?));
            }
        }
        {
            let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE destination = ?1");
            let mut statement = self.connection.prepare(&sql)?;
            let mut rows = statement.query([peer.as_bytes().as_slice()])?;
            while let Some(row) = rows.next()? {
                timeline.push(TimelineEntry::outbound(&decode_outbox(raw_outbox(row)?)?));
            }
        }
        timeline.sort_by_key(|entry| (entry.timestamp().get(), entry.sequence().get()));
        Ok(timeline)
    }

    fn reconcile(&self) -> Result<Vec<ReconcileWork>, Self::Error> {
        let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox ORDER BY id ASC");
        let mut statement = self.connection.prepare(&sql)?;
        let mut rows = statement.query([])?;
        let mut work = Vec::new();
        while let Some(row) = rows.next()? {
            let record = decode_outbox(raw_outbox(row)?)?;
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
