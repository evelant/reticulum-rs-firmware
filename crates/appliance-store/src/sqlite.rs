use core::fmt;
use std::collections::BTreeMap;
use std::path::Path;
use std::string::String;
use std::vec::Vec;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::store::{observed_unix_ms, project_conversation_peers, project_outbox_status};
use crate::{
    AcceptanceIds, AcceptanceOutcome, AttemptLocationStamp, ChatStore, ChatStoreError, Contact,
    ContactUpsertOutcome, ConversationPeer, DestinationHash, DeviceBinding, DeviceBindingOutcome,
    EncodedPacketSha256, IdempotencyKey, InboundCommitOutcome, InboundMessage, InboundRecord,
    MAX_MESSAGE_ACTIVITY_EVENTS, MessageActivityEvent, MessageActivityId, MessageActivityKind,
    MessageActivityPage, MessageActivityPageRequest, MessageActivityScope, MessageAttemptNumber,
    MessageId, MessageIngressObservation, MessageInterfaceId, MessageLocation,
    MessageSignalObservation, OutboxCommitOutcome, OutboxId, OutboxMaterial, OutboxRecord,
    OutboxRetryOutcome, OutboxStatus, PacketEvidence, PhoneLocationAuthorization,
    PhoneLocationSample, PhoneLocationSource, PhoneLocationUnavailableReason, ReconcileWork,
    RfTraceAttemptObservation, RfTraceAttemptOutcome, RfTraceBootId, RfTraceEvent, RfTraceEventId,
    RfTraceEventSequence, RfTraceIdentityHash, RfTraceImportBatch, RfTraceImportOutcome,
    RfTraceInboundProofObservation, RfTraceInboundProofStage, RfTraceInterfaceId,
    RfTraceMessageCorrelation, RfTraceObservation, RfTraceObservationKind, RfTracePage,
    RfTracePageRequest, RfTraceProofIngress, RfTraceRadioProfile, RfTraceRouteObservation,
    RfTraceRouteResolution, RfTraceRxObservation, RfTraceScope, RfTraceTxObservation,
    RfTraceTxOutcome, RnsAttemptToken, StatusProjectionOutcome, SubmissionFailure, SubmissionId,
    SubmissionState, TimelineDirection, TimelineEntry, TimelineSequence, UnixTimestampMillis,
};

/// Current SQLite `PRAGMA user_version` owned by this adapter.
pub const SQLITE_SCHEMA_VERSION: u32 = 13;

const INBOUND_COLUMNS: &str = "message_id, sequence, local_destination, source, timestamp_unix_ms, \
                               title, content, ingress_interface, ingress_rssi, ingress_snr, \
                               location_latitude_e6, location_longitude_e6, location_altitude_cm, \
                               location_speed_cm_per_second, location_bearing_centidegrees, \
                               location_accuracy_cm, location_updated_at_unix_seconds, \
                               receiver_location_latitude_e6, receiver_location_longitude_e6, \
                               receiver_location_horizontal_accuracy_mm, \
                               receiver_location_altitude_mm, \
                               receiver_location_vertical_accuracy_mm, \
                               receiver_location_captured_at_unix_ms, \
                               receiver_location_authorization, receiver_location_source, \
                               receiver_location_mocked";
const OUTBOX_COLUMNS: &str = "id, sequence, destination, timestamp_unix_ms, idempotency_key, \
                             title, content, submission_id, message_id, status_kind, \
                             failure_kind, packet_len, packet_sha256, current_attempt, \
                             location_latitude_e6, location_longitude_e6, \
                             location_altitude_cm, location_speed_cm_per_second, \
                             location_bearing_centidegrees, location_accuracy_cm, \
                             location_updated_at_unix_seconds";
const ACTIVITY_COLUMNS: &str = "activity.id, activity.observed_at_unix_ms, \
                               activity.timeline_sequence, activity.peer, activity.direction, \
                               activity.outbox_id, activity.attempt_number, activity.kind, \
                               activity.submission_id, activity.message_id, activity.status_kind, \
                               activity.failure_kind, activity.packet_len, activity.packet_sha256, \
                               activity.location_state, \
                               activity.location_latitude_e6, activity.location_longitude_e6, \
                               activity.location_accuracy_mm, activity.location_altitude_mm, \
                               activity.location_vertical_accuracy_mm, \
                               activity.location_captured_at_unix_ms, \
                               activity.location_authorization, activity.location_source, \
                               activity.location_mocked, activity.location_unavailable_reason, \
                               inbound.ingress_interface, inbound.ingress_rssi, inbound.ingress_snr, \
                               inbound.location_latitude_e6, inbound.location_longitude_e6, \
                               inbound.location_altitude_cm, inbound.location_speed_cm_per_second, \
                               inbound.location_bearing_centidegrees, inbound.location_accuracy_cm, \
                               inbound.location_updated_at_unix_seconds, \
                               inbound.receiver_location_latitude_e6, \
                               inbound.receiver_location_longitude_e6, \
                               inbound.receiver_location_horizontal_accuracy_mm, \
                               inbound.receiver_location_altitude_mm, \
                               inbound.receiver_location_vertical_accuracy_mm, \
                               inbound.receiver_location_captured_at_unix_ms, \
                               inbound.receiver_location_authorization, \
                               inbound.receiver_location_source, inbound.receiver_location_mocked";
const RF_TRACE_COLUMNS: &str = "e.id, e.boot_id, e.imported_at_unix_ms, e.event_sequence, \
                               e.observed_at_us, e.kind, e.interface_id, e.packet_len, \
                               e.packet_sha256, e.attempt_token, e.route_destination, \
                               e.route_next_hop, e.route_hops, e.route_resolution, e.submission_id, \
                               e.tx_outcome, e.planned_frames, e.completed_frames, \
                               e.frame_completed_0_us, e.frame_completed_1_us, e.authorized, \
                               e.rx_rssi, e.rx_snr, e.attempt_outcome, e.proof_interface, \
                               e.proof_rssi, e.proof_snr, e.inbound_stage, \
                               e.inbound_message_id, e.timeline_sequence, e.outbox_id, \
                               e.attempt_number, e.location_state, e.location_latitude_e6, \
                               e.location_longitude_e6, e.location_accuracy_mm, \
                               e.location_altitude_mm, e.location_vertical_accuracy_mm, \
                               e.location_captured_at_unix_ms, e.location_authorization, \
                               e.location_source, e.location_mocked, \
                               e.location_unavailable_reason, b.profile_fingerprint, \
                               b.frequency_hz, b.bandwidth_hz, b.preamble_symbols, \
                               b.requested_power_dbm, b.spreading_factor, \
                               b.coding_rate_denominator, b.explicit_header, b.crc, b.iq_inverted";

/// SQLite adapter failure without leaking database types into the domain API.
#[derive(Debug)]
pub enum SqliteStoreError {
    /// A database operation failed.
    Database(rusqlite::Error),
    /// Database contents violated the versioned chat schema.
    CorruptData(&'static str),
    /// The database does not use the current schema.
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

#[derive(Clone, Copy)]
enum RetryMutation {
    Requeued,
    AlreadyPending,
}

impl SqliteChatStore {
    /// Open a current file-backed chat database or create one in an empty file.
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

    /// Common terminal-row replacement transaction.
    fn rearm_outbox(
        &mut self,
        outbox_id: OutboxId,
        idempotency_key: IdempotencyKey,
        location: AttemptLocationStamp,
    ) -> Result<RetryMutation, SqliteStoreError> {
        let id = sqlite_integer(outbox_id.get(), "outbox id")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE id = ?1");
        let record = transaction
            .query_row(&sql, [id], raw_outbox)
            .optional()?
            .map(decode_outbox)
            .transpose()?
            .ok_or(ChatStoreError::OutboxNotFound(outbox_id))?;
        match record.status() {
            OutboxStatus::Committed
            | OutboxStatus::Accepted
            | OutboxStatus::Device(
                SubmissionState::Queued
                | SubmissionState::Preparing
                | SubmissionState::AwaitingDelivery(_),
            ) => {
                transaction.commit()?;
                return Ok(RetryMutation::AlreadyPending);
            }
            OutboxStatus::Device(SubmissionState::Failed(failure)) if failure.is_retryable() => {}
            OutboxStatus::Device(
                SubmissionState::Delivered(_)
                | SubmissionState::ApplicationDelivered
                | SubmissionState::Failed(_)
                | SubmissionState::Cancelled,
            ) => return Err(ChatStoreError::OutboxNotRetryable(outbox_id).into()),
        }
        if record.material().idempotency_key() == idempotency_key {
            return Err(ChatStoreError::RetryIdempotencyKeyUnchanged(outbox_id).into());
        }
        let conflict = transaction
            .query_row(
                "SELECT id FROM outbox WHERE idempotency_key = ?1 AND id != ?2 LIMIT 1",
                params![idempotency_key.as_bytes().as_slice(), id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if conflict.is_some() {
            return Err(ChatStoreError::IdempotencyConflict.into());
        }
        let next_attempt = record
            .current_attempt()
            .checked_next()
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        let updated = transaction.execute(
            "UPDATE outbox\n\
                 SET idempotency_key = ?1, submission_id = NULL, message_id = NULL,\n\
                     status_kind = 0, failure_kind = NULL, packet_len = NULL,\n\
                     packet_sha256 = NULL, current_attempt = ?2\n\
                 WHERE id = ?3 AND status_kind = 6",
            params![
                idempotency_key.as_bytes().as_slice(),
                i64::from(next_attempt.get()),
                id,
            ],
        )?;
        if updated != 1 {
            return Err(SqliteStoreError::CorruptData("outbox retry update"));
        }
        record_message_activity(
            &transaction,
            PendingMessageActivity {
                observed_at_unix_ms: observed_unix_ms(),
                timeline_sequence: record.sequence(),
                peer: record.material().destination(),
                direction: TimelineDirection::Outbound,
                outbox_id: Some(outbox_id),
                attempt_number: Some(next_attempt),
                kind: MessageActivityKind::OutboundRequeued { location },
            },
        )?;
        transaction.commit()?;
        Ok(RetryMutation::Requeued)
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
    if version == SQLITE_SCHEMA_VERSION {
        transaction.commit()?;
        return Ok(());
    }
    if version != 0 {
        return Err(SqliteStoreError::UnsupportedSchemaVersion(version));
    }
    let application_objects = transaction.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get::<_, u32>(0),
    )?;
    if application_objects != 0 {
        return Err(SqliteStoreError::UnsupportedSchemaVersion(0));
    }
    transaction.execute_batch(
        "CREATE TABLE chat_meta (\n\
                     name TEXT PRIMARY KEY NOT NULL,\n\
                     next_value INTEGER NOT NULL CHECK (next_value > 0)\n\
                 );\n\
                 INSERT INTO chat_meta(name, next_value) VALUES\n\
                     ('outbox_id', 1), ('timeline_sequence', 1), ('message_activity_id', 1);\n\
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
                     content BLOB NOT NULL,\n\
                     ingress_interface BLOB CHECK (ingress_interface IS NULL OR length(ingress_interface) = 8),\n\
                     ingress_rssi INTEGER CHECK (ingress_rssi BETWEEN -32768 AND 32767),\n\
                     ingress_snr INTEGER CHECK (ingress_snr BETWEEN -32768 AND 32767),\n\
                     location_latitude_e6 INTEGER CHECK (location_latitude_e6 BETWEEN -90000000 AND 90000000),\n\
                     location_longitude_e6 INTEGER CHECK (location_longitude_e6 BETWEEN -180000000 AND 180000000),\n\
                     location_altitude_cm INTEGER CHECK (location_altitude_cm BETWEEN -2147483648 AND 2147483647),\n\
                     location_speed_cm_per_second INTEGER CHECK (location_speed_cm_per_second BETWEEN 0 AND 4294967295),\n\
                     location_bearing_centidegrees INTEGER CHECK (location_bearing_centidegrees BETWEEN -2147483648 AND 2147483647),\n\
                     location_accuracy_cm INTEGER CHECK (location_accuracy_cm BETWEEN 0 AND 65535),\n\
                     location_updated_at_unix_seconds INTEGER CHECK (location_updated_at_unix_seconds BETWEEN 0 AND 4294967295),\n\
                     receiver_location_latitude_e6 INTEGER CHECK (receiver_location_latitude_e6 BETWEEN -90000000 AND 90000000),\n\
                     receiver_location_longitude_e6 INTEGER CHECK (receiver_location_longitude_e6 BETWEEN -180000000 AND 180000000),\n\
                     receiver_location_horizontal_accuracy_mm INTEGER CHECK (receiver_location_horizontal_accuracy_mm BETWEEN 0 AND 4294967295),\n\
                     receiver_location_altitude_mm INTEGER CHECK (receiver_location_altitude_mm BETWEEN -2147483648 AND 2147483647),\n\
                     receiver_location_vertical_accuracy_mm INTEGER CHECK (receiver_location_vertical_accuracy_mm BETWEEN 0 AND 4294967295),\n\
                     receiver_location_captured_at_unix_ms INTEGER CHECK (receiver_location_captured_at_unix_ms >= 0),\n\
                     receiver_location_authorization INTEGER CHECK (receiver_location_authorization BETWEEN 0 AND 2),\n\
                     receiver_location_source INTEGER CHECK (receiver_location_source IN (0, 1)),\n\
                     receiver_location_mocked INTEGER CHECK (receiver_location_mocked IN (0, 1)),\n\
                     CHECK ((ingress_interface IS NULL AND ingress_rssi IS NULL AND ingress_snr IS NULL) OR\n\
                            (ingress_interface IS NOT NULL AND\n\
                             ((ingress_rssi IS NULL AND ingress_snr IS NULL) OR\n\
                              (ingress_rssi IS NOT NULL AND ingress_snr IS NOT NULL))))\n\
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
                     status_kind INTEGER NOT NULL CHECK (status_kind BETWEEN 0 AND 8),\n\
                     failure_kind INTEGER CHECK (failure_kind BETWEEN 0 AND 3),\n\
                     packet_len INTEGER CHECK (packet_len > 0 AND packet_len <= 65535),\n\
                     packet_sha256 BLOB CHECK (packet_sha256 IS NULL OR length(packet_sha256) = 32),\n\
                     current_attempt INTEGER NOT NULL DEFAULT 1\n\
                         CHECK (current_attempt BETWEEN 1 AND 4294967295),\n\
                     location_latitude_e6 INTEGER CHECK (location_latitude_e6 BETWEEN -90000000 AND 90000000),\n\
                     location_longitude_e6 INTEGER CHECK (location_longitude_e6 BETWEEN -180000000 AND 180000000),\n\
                     location_altitude_cm INTEGER CHECK (location_altitude_cm BETWEEN -2147483648 AND 2147483647),\n\
                     location_speed_cm_per_second INTEGER CHECK (location_speed_cm_per_second BETWEEN 0 AND 4294967295),\n\
                     location_bearing_centidegrees INTEGER CHECK (location_bearing_centidegrees BETWEEN -2147483648 AND 2147483647),\n\
                     location_accuracy_cm INTEGER CHECK (location_accuracy_cm BETWEEN 0 AND 65535),\n\
                     location_updated_at_unix_seconds INTEGER CHECK (location_updated_at_unix_seconds BETWEEN 0 AND 4294967295),\n\
                     CHECK ((status_kind = 0 AND submission_id IS NULL AND message_id IS NULL) OR\n\
                            (status_kind BETWEEN 1 AND 8 AND submission_id IS NOT NULL AND message_id IS NOT NULL)),\n\
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
                 CREATE TABLE message_activity_meta (\n\
                     singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),\n\
                     history_incomplete INTEGER NOT NULL CHECK (history_incomplete IN (0, 1))\n\
                 );\n\
                 INSERT INTO message_activity_meta(singleton, history_incomplete) VALUES (1, 0);\n\
                 CREATE TABLE message_activity (\n\
                     id INTEGER PRIMARY KEY NOT NULL CHECK (id > 0),\n\
                     observed_at_unix_ms INTEGER CHECK (observed_at_unix_ms >= 0),\n\
                     timeline_sequence INTEGER NOT NULL CHECK (timeline_sequence > 0),\n\
                     peer BLOB NOT NULL CHECK (length(peer) = 16),\n\
                     direction INTEGER NOT NULL CHECK (direction IN (0, 1)),\n\
                     outbox_id INTEGER CHECK (outbox_id > 0),\n\
                     attempt_number INTEGER CHECK (attempt_number BETWEEN 1 AND 4294967295),\n\
                     kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 4),\n\
                     submission_id INTEGER CHECK (submission_id > 0),\n\
                     message_id BLOB CHECK (message_id IS NULL OR length(message_id) = 32),\n\
                     status_kind INTEGER CHECK (status_kind BETWEEN 2 AND 8),\n\
                     failure_kind INTEGER CHECK (failure_kind BETWEEN 0 AND 3),\n\
                     packet_len INTEGER CHECK (packet_len > 0 AND packet_len <= 65535),\n\
                     packet_sha256 BLOB CHECK (packet_sha256 IS NULL OR length(packet_sha256) = 32),\n\
                     location_state INTEGER CHECK (location_state IN (0, 1)),\n\
                     location_latitude_e6 INTEGER CHECK (location_latitude_e6 BETWEEN -90000000 AND 90000000),\n\
                     location_longitude_e6 INTEGER CHECK (location_longitude_e6 BETWEEN -180000000 AND 180000000),\n\
                     location_accuracy_mm INTEGER CHECK (location_accuracy_mm BETWEEN 0 AND 4294967295),\n\
                     location_altitude_mm INTEGER CHECK (location_altitude_mm BETWEEN -2147483648 AND 2147483647),\n\
                     location_vertical_accuracy_mm INTEGER CHECK (location_vertical_accuracy_mm BETWEEN 0 AND 4294967295),\n\
                     location_captured_at_unix_ms INTEGER CHECK (location_captured_at_unix_ms >= 0),\n\
                     location_authorization INTEGER CHECK (location_authorization BETWEEN 0 AND 2),\n\
                     location_source INTEGER CHECK (location_source IN (0, 1)),\n\
                     location_mocked INTEGER CHECK (location_mocked IN (0, 1)),\n\
                     location_unavailable_reason INTEGER CHECK (location_unavailable_reason BETWEEN 0 AND 6),\n\
                     CHECK ((direction = 0 AND outbox_id IS NULL AND attempt_number IS NULL) OR\n\
                            (direction = 1 AND outbox_id IS NOT NULL AND attempt_number IS NOT NULL)),\n\
                     CHECK ((kind = 0 AND direction = 0 AND message_id IS NOT NULL AND\n\
                             submission_id IS NULL AND status_kind IS NULL AND\n\
                             failure_kind IS NULL AND packet_len IS NULL AND packet_sha256 IS NULL) OR\n\
                            (kind = 1 AND direction = 1 AND submission_id IS NULL AND\n\
                             message_id IS NULL AND status_kind IS NULL AND\n\
                             failure_kind IS NULL AND packet_len IS NULL AND packet_sha256 IS NULL) OR\n\
                            (kind = 2 AND direction = 1 AND submission_id IS NOT NULL AND\n\
                             message_id IS NOT NULL AND status_kind IS NULL AND\n\
                             failure_kind IS NULL AND packet_len IS NULL AND packet_sha256 IS NULL) OR\n\
                            (kind = 3 AND direction = 1 AND submission_id IS NULL AND\n\
                             message_id IS NULL AND status_kind IS NOT NULL) OR\n\
                            (kind = 4 AND direction = 1 AND submission_id IS NULL AND\n\
                             message_id IS NULL AND status_kind IS NULL AND\n\
                             failure_kind IS NULL AND packet_len IS NULL AND packet_sha256 IS NULL)),\n\
                     CHECK ((status_kind IN (4, 5) AND packet_len IS NOT NULL AND packet_sha256 IS NOT NULL) OR\n\
                            (status_kind NOT IN (4, 5) AND packet_len IS NULL AND packet_sha256 IS NULL)),\n\
                     CHECK ((status_kind = 6 AND failure_kind IS NOT NULL) OR\n\
                            (status_kind != 6 AND failure_kind IS NULL)),\n\
                     CHECK (((kind IN (1, 4)) AND location_state IS NOT NULL) OR\n\
                            ((kind NOT IN (1, 4)) AND location_state IS NULL)),\n\
                     CHECK ((location_state = 0 AND location_latitude_e6 IS NOT NULL AND\n\
                             location_longitude_e6 IS NOT NULL AND\n\
                             location_captured_at_unix_ms IS NOT NULL AND\n\
                             location_authorization IS NOT NULL AND location_source IS NOT NULL AND\n\
                             location_unavailable_reason IS NULL) OR\n\
                            (location_state = 1 AND location_latitude_e6 IS NULL AND\n\
                             location_longitude_e6 IS NULL AND location_accuracy_mm IS NULL AND\n\
                             location_altitude_mm IS NULL AND location_vertical_accuracy_mm IS NULL AND\n\
                             location_captured_at_unix_ms IS NULL AND\n\
                             location_authorization IS NULL AND location_source IS NULL AND\n\
                             location_mocked IS NULL AND location_unavailable_reason IS NOT NULL) OR\n\
                            (location_state IS NULL AND location_latitude_e6 IS NULL AND\n\
                             location_longitude_e6 IS NULL AND location_accuracy_mm IS NULL AND\n\
                             location_altitude_mm IS NULL AND location_vertical_accuracy_mm IS NULL AND\n\
                             location_captured_at_unix_ms IS NULL AND\n\
                             location_authorization IS NULL AND location_source IS NULL AND\n\
                             location_mocked IS NULL AND location_unavailable_reason IS NULL))\n\
                 );\n\
                 CREATE INDEX message_activity_timeline_idx\n\
                     ON message_activity(timeline_sequence, id DESC);",
    )?;
    create_rf_trace_schema(&transaction)?;
    transaction.pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn create_rf_trace_schema(transaction: &Transaction<'_>) -> Result<(), SqliteStoreError> {
    transaction.execute_batch(
        "INSERT OR IGNORE INTO chat_meta(name, next_value) VALUES ('rf_trace_id', 1);\n\
         CREATE TABLE IF NOT EXISTS rf_trace_boots (\n\
             boot_id BLOB PRIMARY KEY NOT NULL CHECK (length(boot_id) = 8),\n\
             profile_fingerprint BLOB NOT NULL CHECK (length(profile_fingerprint) = 16),\n\
             frequency_hz INTEGER NOT NULL CHECK (frequency_hz > 0),\n\
             bandwidth_hz INTEGER NOT NULL CHECK (bandwidth_hz > 0),\n\
             preamble_symbols INTEGER NOT NULL CHECK (preamble_symbols > 0 AND preamble_symbols <= 65535),\n\
             requested_power_dbm INTEGER NOT NULL CHECK (requested_power_dbm BETWEEN -32768 AND 32767),\n\
             spreading_factor INTEGER NOT NULL CHECK (spreading_factor BETWEEN 1 AND 255),\n\
             coding_rate_denominator INTEGER NOT NULL CHECK (coding_rate_denominator BETWEEN 1 AND 255),\n\
             explicit_header INTEGER NOT NULL CHECK (explicit_header IN (0, 1)),\n\
             crc INTEGER NOT NULL CHECK (crc IN (0, 1)),\n\
             iq_inverted INTEGER NOT NULL CHECK (iq_inverted IN (0, 1)),\n\
             history_incomplete INTEGER NOT NULL CHECK (history_incomplete IN (0, 1))\n\
         );\n\
         CREATE TABLE IF NOT EXISTS rf_trace_events (\n\
             id INTEGER PRIMARY KEY NOT NULL CHECK (id > 0),\n\
             boot_id BLOB NOT NULL CHECK (length(boot_id) = 8)\n\
                 REFERENCES rf_trace_boots(boot_id),\n\
             imported_at_unix_ms INTEGER NOT NULL CHECK (imported_at_unix_ms >= 0),\n\
             event_sequence BLOB NOT NULL CHECK (length(event_sequence) = 8),\n\
             observed_at_us BLOB NOT NULL CHECK (length(observed_at_us) = 8),\n\
             kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 3),\n\
             interface_id BLOB CHECK (interface_id IS NULL OR length(interface_id) = 8),\n\
             packet_len INTEGER CHECK (packet_len BETWEEN 1 AND 65535),\n\
             packet_sha256 BLOB CHECK (packet_sha256 IS NULL OR length(packet_sha256) = 32),\n\
             attempt_token BLOB CHECK (attempt_token IS NULL OR length(attempt_token) = 32),\n\
             route_destination BLOB CHECK (route_destination IS NULL OR length(route_destination) = 16),\n\
             route_next_hop BLOB CHECK (route_next_hop IS NULL OR length(route_next_hop) = 16),\n\
             route_hops INTEGER CHECK (route_hops BETWEEN 0 AND 255),\n\
             route_resolution INTEGER CHECK (route_resolution BETWEEN 0 AND 4),\n\
             submission_id BLOB CHECK (submission_id IS NULL OR length(submission_id) = 8),\n\
             tx_outcome INTEGER CHECK (tx_outcome BETWEEN 0 AND 15),\n\
             planned_frames INTEGER CHECK (planned_frames BETWEEN 1 AND 2),\n\
             completed_frames INTEGER CHECK (completed_frames BETWEEN 0 AND 2),\n\
             frame_completed_0_us BLOB CHECK (frame_completed_0_us IS NULL OR length(frame_completed_0_us) = 8),\n\
             frame_completed_1_us BLOB CHECK (frame_completed_1_us IS NULL OR length(frame_completed_1_us) = 8),\n\
             authorized INTEGER CHECK (authorized IN (0, 1)),\n\
             rx_rssi INTEGER CHECK (rx_rssi BETWEEN -32768 AND 32767),\n\
             rx_snr INTEGER CHECK (rx_snr BETWEEN -32768 AND 32767),\n\
             attempt_outcome INTEGER CHECK (attempt_outcome BETWEEN 0 AND 2),\n\
             proof_interface BLOB CHECK (proof_interface IS NULL OR length(proof_interface) = 8),\n\
             proof_rssi INTEGER CHECK (proof_rssi BETWEEN -32768 AND 32767),\n\
             proof_snr INTEGER CHECK (proof_snr BETWEEN -32768 AND 32767),\n\
             inbound_stage INTEGER CHECK (inbound_stage BETWEEN 0 AND 6),\n\
             inbound_message_id BLOB \
                 CHECK (inbound_message_id IS NULL OR length(inbound_message_id) = 32),\n\
             timeline_sequence INTEGER CHECK (timeline_sequence > 0),\n\
             outbox_id INTEGER CHECK (outbox_id > 0),\n\
             attempt_number INTEGER CHECK (attempt_number BETWEEN 1 AND 4294967295),\n\
             location_state INTEGER CHECK (location_state IN (0, 1)),\n\
             location_latitude_e6 INTEGER CHECK (location_latitude_e6 BETWEEN -90000000 AND 90000000),\n\
             location_longitude_e6 INTEGER CHECK (location_longitude_e6 BETWEEN -180000000 AND 180000000),\n\
             location_accuracy_mm INTEGER CHECK (location_accuracy_mm BETWEEN 0 AND 4294967295),\n\
             location_altitude_mm INTEGER CHECK (location_altitude_mm BETWEEN -2147483648 AND 2147483647),\n\
             location_vertical_accuracy_mm INTEGER CHECK (location_vertical_accuracy_mm BETWEEN 0 AND 4294967295),\n\
             location_captured_at_unix_ms INTEGER CHECK (location_captured_at_unix_ms >= 0),\n\
             location_authorization INTEGER CHECK (location_authorization BETWEEN 0 AND 2),\n\
             location_source INTEGER CHECK (location_source IN (0, 1)),\n\
             location_mocked INTEGER CHECK (location_mocked IN (0, 1)),\n\
             location_unavailable_reason INTEGER CHECK (location_unavailable_reason BETWEEN 0 AND 6),\n\
             UNIQUE(boot_id, event_sequence),\n\
             CHECK ((timeline_sequence IS NULL AND outbox_id IS NULL AND attempt_number IS NULL AND location_state IS NULL) OR\n\
                    (timeline_sequence IS NOT NULL AND outbox_id IS NOT NULL AND attempt_number IS NOT NULL AND location_state IS NOT NULL)),\n\
             CHECK ((proof_rssi IS NULL AND proof_snr IS NULL) OR\n\
                    (proof_rssi IS NOT NULL AND proof_snr IS NOT NULL)),\n\
             CHECK ((completed_frames >= 1) = (frame_completed_0_us IS NOT NULL)),\n\
             CHECK ((completed_frames >= 2) = (frame_completed_1_us IS NOT NULL))\n\
         );\n\
         CREATE INDEX IF NOT EXISTS rf_trace_timeline_idx\n\
             ON rf_trace_events(timeline_sequence, id DESC);\n\
         CREATE INDEX IF NOT EXISTS rf_trace_packet_idx\n\
             ON rf_trace_events(packet_sha256);\n\
         CREATE INDEX IF NOT EXISTS rf_trace_token_idx\n\
             ON rf_trace_events(attempt_token);",
    )?;
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

fn u64_blob(value: u64) -> [u8; 8] {
    value.to_be_bytes()
}

fn u64_from_blob(bytes: Vec<u8>, field: &'static str) -> Result<u64, SqliteStoreError> {
    Ok(u64::from_be_bytes(array_from_blob(bytes, field)?))
}

fn sqlite_bool(value: bool) -> i64 {
    i64::from(value)
}

fn bool_from_integer(value: i64, field: &'static str) -> Result<bool, SqliteStoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SqliteStoreError::CorruptData(field)),
    }
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
        ingress_interface: row.get(7)?,
        ingress_rssi: row.get(8)?,
        ingress_snr: row.get(9)?,
        location_latitude_e6: row.get(10)?,
        location_longitude_e6: row.get(11)?,
        location_altitude_cm: row.get(12)?,
        location_speed_cm_per_second: row.get(13)?,
        location_bearing_centidegrees: row.get(14)?,
        location_accuracy_cm: row.get(15)?,
        location_updated_at_unix_seconds: row.get(16)?,
        receiver_location_latitude_e6: row.get(17)?,
        receiver_location_longitude_e6: row.get(18)?,
        receiver_location_horizontal_accuracy_mm: row.get(19)?,
        receiver_location_altitude_mm: row.get(20)?,
        receiver_location_vertical_accuracy_mm: row.get(21)?,
        receiver_location_captured_at_unix_ms: row.get(22)?,
        receiver_location_authorization: row.get(23)?,
        receiver_location_source: row.get(24)?,
        receiver_location_mocked: row.get(25)?,
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
    ingress_interface: Option<Vec<u8>>,
    ingress_rssi: Option<i64>,
    ingress_snr: Option<i64>,
    location_latitude_e6: Option<i64>,
    location_longitude_e6: Option<i64>,
    location_altitude_cm: Option<i64>,
    location_speed_cm_per_second: Option<i64>,
    location_bearing_centidegrees: Option<i64>,
    location_accuracy_cm: Option<i64>,
    location_updated_at_unix_seconds: Option<i64>,
    receiver_location_latitude_e6: Option<i64>,
    receiver_location_longitude_e6: Option<i64>,
    receiver_location_horizontal_accuracy_mm: Option<i64>,
    receiver_location_altitude_mm: Option<i64>,
    receiver_location_vertical_accuracy_mm: Option<i64>,
    receiver_location_captured_at_unix_ms: Option<i64>,
    receiver_location_authorization: Option<i64>,
    receiver_location_source: Option<i64>,
    receiver_location_mocked: Option<i64>,
}

fn decode_inbound(raw: RawInbound) -> Result<InboundRecord, SqliteStoreError> {
    let sequence = TimelineSequence::new(positive_u64(raw.sequence, "inbound sequence")?)
        .ok_or(SqliteStoreError::CorruptData("inbound sequence"))?;
    let timestamp =
        UnixTimestampMillis::new(positive_u64(raw.timestamp_unix_ms, "inbound timestamp")?)
            .map_err(|_| SqliteStoreError::CorruptData("inbound timestamp"))?;
    let ingress = match (raw.ingress_interface, raw.ingress_rssi, raw.ingress_snr) {
        (None, None, None) => None,
        (Some(interface), None, None) => Some(MessageIngressObservation::new(
            MessageInterfaceId::new(array_from_blob(interface, "inbound ingress_interface")?),
            None,
        )),
        (Some(interface), Some(rssi), Some(snr)) => Some(MessageIngressObservation::new(
            MessageInterfaceId::new(array_from_blob(interface, "inbound ingress_interface")?),
            Some(MessageSignalObservation::new(
                i16::try_from(rssi)
                    .map_err(|_| SqliteStoreError::CorruptData("inbound ingress_rssi"))?,
                i16::try_from(snr)
                    .map_err(|_| SqliteStoreError::CorruptData("inbound ingress_snr"))?,
            )),
        )),
        _ => {
            return Err(SqliteStoreError::CorruptData("inbound ingress observation"));
        }
    };
    let location = decode_message_location(
        raw.location_latitude_e6,
        raw.location_longitude_e6,
        raw.location_altitude_cm,
        raw.location_speed_cm_per_second,
        raw.location_bearing_centidegrees,
        raw.location_accuracy_cm,
        raw.location_updated_at_unix_seconds,
    )?;
    let receiver_location = decode_phone_location(
        raw.receiver_location_latitude_e6,
        raw.receiver_location_longitude_e6,
        raw.receiver_location_horizontal_accuracy_mm,
        raw.receiver_location_altitude_mm,
        raw.receiver_location_vertical_accuracy_mm,
        raw.receiver_location_captured_at_unix_ms,
        raw.receiver_location_authorization,
        raw.receiver_location_source,
        raw.receiver_location_mocked,
        "inbound receiver location",
    )?;
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
        )
        .with_location(location)
        .with_ingress_observation(ingress),
        receiver_location,
    })
}

#[allow(clippy::type_complexity)]
fn encode_message_location(
    location: Option<MessageLocation>,
) -> (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
) {
    match location {
        None => (None, None, None, None, None, None, None),
        Some(location) => (
            Some(i64::from(location.latitude_e6())),
            Some(i64::from(location.longitude_e6())),
            Some(i64::from(location.altitude_cm())),
            Some(i64::from(location.speed_cm_per_second())),
            Some(i64::from(location.bearing_centidegrees())),
            Some(i64::from(location.accuracy_cm())),
            Some(i64::from(location.updated_at_unix_seconds())),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_message_location(
    latitude_e6: Option<i64>,
    longitude_e6: Option<i64>,
    altitude_cm: Option<i64>,
    speed_cm_per_second: Option<i64>,
    bearing_centidegrees: Option<i64>,
    accuracy_cm: Option<i64>,
    updated_at_unix_seconds: Option<i64>,
) -> Result<Option<MessageLocation>, SqliteStoreError> {
    match (
        latitude_e6,
        longitude_e6,
        altitude_cm,
        speed_cm_per_second,
        bearing_centidegrees,
        accuracy_cm,
        updated_at_unix_seconds,
    ) {
        (None, None, None, None, None, None, None) => Ok(None),
        (
            Some(latitude_e6),
            Some(longitude_e6),
            Some(altitude_cm),
            Some(speed_cm_per_second),
            Some(bearing_centidegrees),
            Some(accuracy_cm),
            Some(updated_at_unix_seconds),
        ) => MessageLocation::new(
            i32::try_from(latitude_e6)
                .map_err(|_| SqliteStoreError::CorruptData("message location latitude"))?,
            i32::try_from(longitude_e6)
                .map_err(|_| SqliteStoreError::CorruptData("message location longitude"))?,
            i32::try_from(altitude_cm)
                .map_err(|_| SqliteStoreError::CorruptData("message location altitude"))?,
            u32::try_from(speed_cm_per_second)
                .map_err(|_| SqliteStoreError::CorruptData("message location speed"))?,
            i32::try_from(bearing_centidegrees)
                .map_err(|_| SqliteStoreError::CorruptData("message location bearing"))?,
            u16::try_from(accuracy_cm)
                .map_err(|_| SqliteStoreError::CorruptData("message location accuracy"))?,
            u32::try_from(updated_at_unix_seconds)
                .map_err(|_| SqliteStoreError::CorruptData("message location updated_at"))?,
        )
        .map(Some)
        .ok_or(SqliteStoreError::CorruptData(
            "message location coordinates",
        )),
        _ => Err(SqliteStoreError::CorruptData(
            "message location completeness",
        )),
    }
}

fn encode_ingress_observation(
    ingress: Option<MessageIngressObservation>,
) -> (Option<Vec<u8>>, Option<i64>, Option<i64>) {
    match ingress {
        None => (None, None, None),
        Some(ingress) => match ingress.signal() {
            None => (Some(ingress.interface().as_bytes().to_vec()), None, None),
            Some(signal) => (
                Some(ingress.interface().as_bytes().to_vec()),
                Some(i64::from(signal.rssi_dbm())),
                Some(i64::from(signal.snr_db())),
            ),
        },
    }
}

#[derive(Clone, Copy)]
struct EncodedPhoneLocation {
    latitude_e6: Option<i64>,
    longitude_e6: Option<i64>,
    horizontal_accuracy_mm: Option<i64>,
    altitude_mm: Option<i64>,
    vertical_accuracy_mm: Option<i64>,
    captured_at_unix_ms: Option<i64>,
    authorization: Option<i64>,
    source: Option<i64>,
    mocked: Option<i64>,
}

fn encode_phone_location(
    location: Option<PhoneLocationSample>,
    capture_field: &'static str,
) -> Result<EncodedPhoneLocation, SqliteStoreError> {
    let Some(location) = location else {
        return Ok(EncodedPhoneLocation {
            latitude_e6: None,
            longitude_e6: None,
            horizontal_accuracy_mm: None,
            altitude_mm: None,
            vertical_accuracy_mm: None,
            captured_at_unix_ms: None,
            authorization: None,
            source: None,
            mocked: None,
        });
    };
    Ok(EncodedPhoneLocation {
        latitude_e6: Some(i64::from(location.latitude_e6())),
        longitude_e6: Some(i64::from(location.longitude_e6())),
        horizontal_accuracy_mm: location.horizontal_accuracy_mm().map(i64::from),
        altitude_mm: location.altitude_mm().map(i64::from),
        vertical_accuracy_mm: location.vertical_accuracy_mm().map(i64::from),
        captured_at_unix_ms: Some(sqlite_integer(
            location.captured_at_unix_ms(),
            capture_field,
        )?),
        authorization: Some(match location.authorization() {
            PhoneLocationAuthorization::Precise => 0,
            PhoneLocationAuthorization::Approximate => 1,
            PhoneLocationAuthorization::Unknown => 2,
        }),
        source: Some(match location.source() {
            PhoneLocationSource::ForegroundStream => 0,
            PhoneLocationSource::LastKnown => 1,
        }),
        mocked: location.mocked().map(i64::from),
    })
}

#[allow(clippy::too_many_arguments)]
fn decode_phone_location(
    latitude_e6: Option<i64>,
    longitude_e6: Option<i64>,
    horizontal_accuracy_mm: Option<i64>,
    altitude_mm: Option<i64>,
    vertical_accuracy_mm: Option<i64>,
    captured_at_unix_ms: Option<i64>,
    authorization: Option<i64>,
    source: Option<i64>,
    mocked: Option<i64>,
    field: &'static str,
) -> Result<Option<PhoneLocationSample>, SqliteStoreError> {
    let absent = latitude_e6.is_none()
        && longitude_e6.is_none()
        && horizontal_accuracy_mm.is_none()
        && altitude_mm.is_none()
        && vertical_accuracy_mm.is_none()
        && captured_at_unix_ms.is_none()
        && authorization.is_none()
        && source.is_none()
        && mocked.is_none();
    if absent {
        return Ok(None);
    }
    let latitude_e6 = i32::try_from(latitude_e6.ok_or(SqliteStoreError::CorruptData(field))?)
        .map_err(|_| SqliteStoreError::CorruptData(field))?;
    let longitude_e6 = i32::try_from(longitude_e6.ok_or(SqliteStoreError::CorruptData(field))?)
        .map_err(|_| SqliteStoreError::CorruptData(field))?;
    let horizontal_accuracy_mm = horizontal_accuracy_mm
        .map(|value| u32::try_from(value).map_err(|_| SqliteStoreError::CorruptData(field)))
        .transpose()?;
    let altitude_mm = altitude_mm
        .map(|value| i32::try_from(value).map_err(|_| SqliteStoreError::CorruptData(field)))
        .transpose()?;
    let vertical_accuracy_mm = vertical_accuracy_mm
        .map(|value| u32::try_from(value).map_err(|_| SqliteStoreError::CorruptData(field)))
        .transpose()?;
    let captured_at_unix_ms =
        u64::try_from(captured_at_unix_ms.ok_or(SqliteStoreError::CorruptData(field))?)
            .map_err(|_| SqliteStoreError::CorruptData(field))?;
    let authorization = match authorization {
        Some(0) => PhoneLocationAuthorization::Precise,
        Some(1) => PhoneLocationAuthorization::Approximate,
        Some(2) => PhoneLocationAuthorization::Unknown,
        _ => return Err(SqliteStoreError::CorruptData(field)),
    };
    let source = match source {
        Some(0) => PhoneLocationSource::ForegroundStream,
        Some(1) => PhoneLocationSource::LastKnown,
        _ => return Err(SqliteStoreError::CorruptData(field)),
    };
    let mocked = match mocked {
        None => None,
        Some(0) => Some(false),
        Some(1) => Some(true),
        Some(_) => return Err(SqliteStoreError::CorruptData(field)),
    };
    PhoneLocationSample::new(
        latitude_e6,
        longitude_e6,
        horizontal_accuracy_mm,
        captured_at_unix_ms,
        authorization,
        source,
        mocked,
    )
    .map(|sample| sample.with_altitude(altitude_mm, vertical_accuracy_mm))
    .map(Some)
    .ok_or(SqliteStoreError::CorruptData(field))
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
    current_attempt: i64,
    location_latitude_e6: Option<i64>,
    location_longitude_e6: Option<i64>,
    location_altitude_cm: Option<i64>,
    location_speed_cm_per_second: Option<i64>,
    location_bearing_centidegrees: Option<i64>,
    location_accuracy_cm: Option<i64>,
    location_updated_at_unix_seconds: Option<i64>,
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
        current_attempt: row.get(13)?,
        location_latitude_e6: row.get(14)?,
        location_longitude_e6: row.get(15)?,
        location_altitude_cm: row.get(16)?,
        location_speed_cm_per_second: row.get(17)?,
        location_bearing_centidegrees: row.get(18)?,
        location_accuracy_cm: row.get(19)?,
        location_updated_at_unix_seconds: row.get(20)?,
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
    let current_attempt = u32::try_from(raw.current_attempt)
        .ok()
        .and_then(MessageAttemptNumber::new)
        .ok_or(SqliteStoreError::CorruptData("outbox current_attempt"))?;
    let location = decode_message_location(
        raw.location_latitude_e6,
        raw.location_longitude_e6,
        raw.location_altitude_cm,
        raw.location_speed_cm_per_second,
        raw.location_bearing_centidegrees,
        raw.location_accuracy_cm,
        raw.location_updated_at_unix_seconds,
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
        )
        .with_location(location),
        acceptance,
        status,
        current_attempt,
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
        8 if failure.is_none() && evidence.is_none() => {
            OutboxStatus::Device(SubmissionState::ApplicationDelivered)
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
        OutboxStatus::Device(SubmissionState::ApplicationDelivered) => (8, None, None, None),
    }
}

struct RawActivity {
    id: i64,
    observed_at_unix_ms: Option<i64>,
    timeline_sequence: i64,
    peer: Vec<u8>,
    direction: i64,
    outbox_id: Option<i64>,
    attempt_number: Option<i64>,
    kind: i64,
    submission_id: Option<i64>,
    message_id: Option<Vec<u8>>,
    status_kind: Option<i64>,
    failure_kind: Option<i64>,
    packet_len: Option<i64>,
    packet_sha256: Option<Vec<u8>>,
    location_state: Option<i64>,
    location_latitude_e6: Option<i64>,
    location_longitude_e6: Option<i64>,
    location_accuracy_mm: Option<i64>,
    location_altitude_mm: Option<i64>,
    location_vertical_accuracy_mm: Option<i64>,
    location_captured_at_unix_ms: Option<i64>,
    location_authorization: Option<i64>,
    location_source: Option<i64>,
    location_mocked: Option<i64>,
    location_unavailable_reason: Option<i64>,
    ingress_interface: Option<Vec<u8>>,
    ingress_rssi: Option<i64>,
    ingress_snr: Option<i64>,
    message_location_latitude_e6: Option<i64>,
    message_location_longitude_e6: Option<i64>,
    message_location_altitude_cm: Option<i64>,
    message_location_speed_cm_per_second: Option<i64>,
    message_location_bearing_centidegrees: Option<i64>,
    message_location_accuracy_cm: Option<i64>,
    message_location_updated_at_unix_seconds: Option<i64>,
    receiver_location_latitude_e6: Option<i64>,
    receiver_location_longitude_e6: Option<i64>,
    receiver_location_horizontal_accuracy_mm: Option<i64>,
    receiver_location_altitude_mm: Option<i64>,
    receiver_location_vertical_accuracy_mm: Option<i64>,
    receiver_location_captured_at_unix_ms: Option<i64>,
    receiver_location_authorization: Option<i64>,
    receiver_location_source: Option<i64>,
    receiver_location_mocked: Option<i64>,
}

fn raw_activity(row: &Row<'_>) -> rusqlite::Result<RawActivity> {
    Ok(RawActivity {
        id: row.get(0)?,
        observed_at_unix_ms: row.get(1)?,
        timeline_sequence: row.get(2)?,
        peer: row.get(3)?,
        direction: row.get(4)?,
        outbox_id: row.get(5)?,
        attempt_number: row.get(6)?,
        kind: row.get(7)?,
        submission_id: row.get(8)?,
        message_id: row.get(9)?,
        status_kind: row.get(10)?,
        failure_kind: row.get(11)?,
        packet_len: row.get(12)?,
        packet_sha256: row.get(13)?,
        location_state: row.get(14)?,
        location_latitude_e6: row.get(15)?,
        location_longitude_e6: row.get(16)?,
        location_accuracy_mm: row.get(17)?,
        location_altitude_mm: row.get(18)?,
        location_vertical_accuracy_mm: row.get(19)?,
        location_captured_at_unix_ms: row.get(20)?,
        location_authorization: row.get(21)?,
        location_source: row.get(22)?,
        location_mocked: row.get(23)?,
        location_unavailable_reason: row.get(24)?,
        ingress_interface: row.get(25)?,
        ingress_rssi: row.get(26)?,
        ingress_snr: row.get(27)?,
        message_location_latitude_e6: row.get(28)?,
        message_location_longitude_e6: row.get(29)?,
        message_location_altitude_cm: row.get(30)?,
        message_location_speed_cm_per_second: row.get(31)?,
        message_location_bearing_centidegrees: row.get(32)?,
        message_location_accuracy_cm: row.get(33)?,
        message_location_updated_at_unix_seconds: row.get(34)?,
        receiver_location_latitude_e6: row.get(35)?,
        receiver_location_longitude_e6: row.get(36)?,
        receiver_location_horizontal_accuracy_mm: row.get(37)?,
        receiver_location_altitude_mm: row.get(38)?,
        receiver_location_vertical_accuracy_mm: row.get(39)?,
        receiver_location_captured_at_unix_ms: row.get(40)?,
        receiver_location_authorization: row.get(41)?,
        receiver_location_source: row.get(42)?,
        receiver_location_mocked: row.get(43)?,
    })
}

fn decode_attempt_location(
    raw: &RawActivity,
) -> Result<Option<AttemptLocationStamp>, SqliteStoreError> {
    let no_sample_fields = raw.location_latitude_e6.is_none()
        && raw.location_longitude_e6.is_none()
        && raw.location_accuracy_mm.is_none()
        && raw.location_altitude_mm.is_none()
        && raw.location_vertical_accuracy_mm.is_none()
        && raw.location_captured_at_unix_ms.is_none()
        && raw.location_authorization.is_none()
        && raw.location_source.is_none()
        && raw.location_mocked.is_none();
    match raw.location_state {
        None if no_sample_fields && raw.location_unavailable_reason.is_none() => Ok(None),
        Some(0) if raw.location_unavailable_reason.is_none() => {
            let sample = decode_phone_location(
                raw.location_latitude_e6,
                raw.location_longitude_e6,
                raw.location_accuracy_mm,
                raw.location_altitude_mm,
                raw.location_vertical_accuracy_mm,
                raw.location_captured_at_unix_ms,
                raw.location_authorization,
                raw.location_source,
                raw.location_mocked,
                "message activity phone location sample",
            )?
            .ok_or(SqliteStoreError::CorruptData(
                "message activity phone location sample",
            ))?;
            Ok(Some(AttemptLocationStamp::Available(sample)))
        }
        Some(1) if no_sample_fields => {
            let reason = match raw.location_unavailable_reason {
                Some(0) => PhoneLocationUnavailableReason::NotObserved,
                Some(1) => PhoneLocationUnavailableReason::TelemetryDisabled,
                Some(2) => PhoneLocationUnavailableReason::PermissionDenied,
                Some(3) => PhoneLocationUnavailableReason::ServicesDisabled,
                Some(4) => PhoneLocationUnavailableReason::PlatformUnavailable,
                Some(5) => PhoneLocationUnavailableReason::NoFixYet,
                Some(6) => PhoneLocationUnavailableReason::ProviderError,
                _ => {
                    return Err(SqliteStoreError::CorruptData(
                        "message activity location unavailable reason",
                    ));
                }
            };
            Ok(Some(AttemptLocationStamp::Unavailable(reason)))
        }
        Some(_) | None => Err(SqliteStoreError::CorruptData(
            "message activity location stamp",
        )),
    }
}

fn decode_activity(raw: RawActivity) -> Result<MessageActivityEvent, SqliteStoreError> {
    let attempt_location = decode_attempt_location(&raw)?;
    let message_location = decode_message_location(
        raw.message_location_latitude_e6,
        raw.message_location_longitude_e6,
        raw.message_location_altitude_cm,
        raw.message_location_speed_cm_per_second,
        raw.message_location_bearing_centidegrees,
        raw.message_location_accuracy_cm,
        raw.message_location_updated_at_unix_seconds,
    )?;
    let receiver_location = decode_phone_location(
        raw.receiver_location_latitude_e6,
        raw.receiver_location_longitude_e6,
        raw.receiver_location_horizontal_accuracy_mm,
        raw.receiver_location_altitude_mm,
        raw.receiver_location_vertical_accuracy_mm,
        raw.receiver_location_captured_at_unix_ms,
        raw.receiver_location_authorization,
        raw.receiver_location_source,
        raw.receiver_location_mocked,
        "message activity receiver location",
    )?;
    let ingress_observation = match (raw.ingress_interface, raw.ingress_rssi, raw.ingress_snr) {
        (None, None, None) => None,
        (Some(interface), None, None) => Some(MessageIngressObservation::new(
            MessageInterfaceId::new(array_from_blob(
                interface,
                "message activity ingress_interface",
            )?),
            None,
        )),
        (Some(interface), Some(rssi), Some(snr)) => Some(MessageIngressObservation::new(
            MessageInterfaceId::new(array_from_blob(
                interface,
                "message activity ingress_interface",
            )?),
            Some(MessageSignalObservation::new(
                i16::try_from(rssi)
                    .map_err(|_| SqliteStoreError::CorruptData("message activity ingress_rssi"))?,
                i16::try_from(snr)
                    .map_err(|_| SqliteStoreError::CorruptData("message activity ingress_snr"))?,
            )),
        )),
        _ => {
            return Err(SqliteStoreError::CorruptData(
                "message activity ingress observation",
            ));
        }
    };
    let id = MessageActivityId::new(positive_u64(raw.id, "message activity id")?)
        .ok_or(SqliteStoreError::CorruptData("message activity id"))?;
    let observed_at_unix_ms = raw
        .observed_at_unix_ms
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| SqliteStoreError::CorruptData("message activity observed_at_unix_ms"))
        })
        .transpose()?;
    let timeline_sequence = TimelineSequence::new(positive_u64(
        raw.timeline_sequence,
        "message activity timeline_sequence",
    )?)
    .ok_or(SqliteStoreError::CorruptData(
        "message activity timeline_sequence",
    ))?;
    let peer = DestinationHash::new(array_from_blob(raw.peer, "message activity peer")?);
    let direction = match raw.direction {
        0 => TimelineDirection::Inbound,
        1 => TimelineDirection::Outbound,
        _ => return Err(SqliteStoreError::CorruptData("message activity direction")),
    };
    let outbox_id = raw
        .outbox_id
        .map(|value| {
            OutboxId::new(positive_u64(value, "message activity outbox_id")?)
                .ok_or(SqliteStoreError::CorruptData("message activity outbox_id"))
        })
        .transpose()?;
    let attempt_number = raw
        .attempt_number
        .map(|value| {
            u32::try_from(value)
                .ok()
                .and_then(MessageAttemptNumber::new)
                .ok_or(SqliteStoreError::CorruptData(
                    "message activity attempt_number",
                ))
        })
        .transpose()?;
    match direction {
        TimelineDirection::Inbound if outbox_id.is_none() && attempt_number.is_none() => {}
        TimelineDirection::Outbound if outbox_id.is_some() && attempt_number.is_some() => {}
        TimelineDirection::Inbound | TimelineDirection::Outbound => {
            return Err(SqliteStoreError::CorruptData(
                "message activity direction references",
            ));
        }
    }

    let kind = match raw.kind {
        0 if direction == TimelineDirection::Inbound
            && raw.submission_id.is_none()
            && raw.status_kind.is_none()
            && raw.failure_kind.is_none()
            && raw.packet_len.is_none()
            && raw.packet_sha256.is_none() =>
        {
            MessageActivityKind::InboundImported {
                message_id: MessageId::new(array_from_blob(
                    raw.message_id.ok_or(SqliteStoreError::CorruptData(
                        "message activity inbound message_id",
                    ))?,
                    "message activity inbound message_id",
                )?),
            }
        }
        1 if direction == TimelineDirection::Outbound
            && raw.submission_id.is_none()
            && raw.message_id.is_none()
            && raw.status_kind.is_none()
            && raw.failure_kind.is_none()
            && raw.packet_len.is_none()
            && raw.packet_sha256.is_none() =>
        {
            MessageActivityKind::OutboundQueued {
                location: attempt_location.ok_or(SqliteStoreError::CorruptData(
                    "message activity queued location stamp",
                ))?,
            }
        }
        2 if direction == TimelineDirection::Outbound
            && raw.status_kind.is_none()
            && raw.failure_kind.is_none()
            && raw.packet_len.is_none()
            && raw.packet_sha256.is_none() =>
        {
            MessageActivityKind::OutboundAccepted {
                acceptance: AcceptanceIds::new(
                    SubmissionId::new(positive_u64(
                        raw.submission_id.ok_or(SqliteStoreError::CorruptData(
                            "message activity acceptance submission_id",
                        ))?,
                        "message activity acceptance submission_id",
                    )?)
                    .map_err(|_| {
                        SqliteStoreError::CorruptData("message activity acceptance submission_id")
                    })?,
                    MessageId::new(array_from_blob(
                        raw.message_id.ok_or(SqliteStoreError::CorruptData(
                            "message activity acceptance message_id",
                        ))?,
                        "message activity acceptance message_id",
                    )?),
                ),
            }
        }
        3 if direction == TimelineDirection::Outbound
            && raw.submission_id.is_none()
            && raw.message_id.is_none() =>
        {
            let status = decode_status(
                raw.status_kind.ok_or(SqliteStoreError::CorruptData(
                    "message activity status_kind",
                ))?,
                raw.failure_kind,
                raw.packet_len,
                raw.packet_sha256,
            )?;
            let OutboxStatus::Device(state) = status else {
                return Err(SqliteStoreError::CorruptData(
                    "message activity device status",
                ));
            };
            MessageActivityKind::OutboundStatus { state }
        }
        4 if direction == TimelineDirection::Outbound
            && raw.submission_id.is_none()
            && raw.message_id.is_none()
            && raw.status_kind.is_none()
            && raw.failure_kind.is_none()
            && raw.packet_len.is_none()
            && raw.packet_sha256.is_none() =>
        {
            MessageActivityKind::OutboundRequeued {
                location: attempt_location.ok_or(SqliteStoreError::CorruptData(
                    "message activity requeued location stamp",
                ))?,
            }
        }
        _ => return Err(SqliteStoreError::CorruptData("message activity kind")),
    };

    if !matches!(raw.kind, 1 | 4) && attempt_location.is_some() {
        return Err(SqliteStoreError::CorruptData(
            "message activity unexpected location stamp",
        ));
    }

    Ok(MessageActivityEvent {
        id,
        observed_at_unix_ms,
        timeline_sequence,
        peer,
        direction,
        outbox_id,
        attempt_number,
        ingress_observation,
        message_location,
        receiver_location,
        kind,
    })
}

struct RawRfTrace {
    id: i64,
    boot_id: Vec<u8>,
    imported_at_unix_ms: i64,
    event_sequence: Vec<u8>,
    observed_at_us: Vec<u8>,
    kind: i64,
    interface_id: Option<Vec<u8>>,
    packet_len: Option<i64>,
    packet_sha256: Option<Vec<u8>>,
    attempt_token: Option<Vec<u8>>,
    route_destination: Option<Vec<u8>>,
    route_next_hop: Option<Vec<u8>>,
    route_hops: Option<i64>,
    route_resolution: Option<i64>,
    submission_id: Option<Vec<u8>>,
    tx_outcome: Option<i64>,
    planned_frames: Option<i64>,
    completed_frames: Option<i64>,
    frame_completed_0_us: Option<Vec<u8>>,
    frame_completed_1_us: Option<Vec<u8>>,
    authorized: Option<i64>,
    rx_rssi: Option<i64>,
    rx_snr: Option<i64>,
    attempt_outcome: Option<i64>,
    proof_interface: Option<Vec<u8>>,
    proof_rssi: Option<i64>,
    proof_snr: Option<i64>,
    inbound_stage: Option<i64>,
    inbound_message_id: Option<Vec<u8>>,
    timeline_sequence: Option<i64>,
    outbox_id: Option<i64>,
    attempt_number: Option<i64>,
    location_state: Option<i64>,
    location_latitude_e6: Option<i64>,
    location_longitude_e6: Option<i64>,
    location_accuracy_mm: Option<i64>,
    location_altitude_mm: Option<i64>,
    location_vertical_accuracy_mm: Option<i64>,
    location_captured_at_unix_ms: Option<i64>,
    location_authorization: Option<i64>,
    location_source: Option<i64>,
    location_mocked: Option<i64>,
    location_unavailable_reason: Option<i64>,
    profile_fingerprint: Vec<u8>,
    frequency_hz: i64,
    bandwidth_hz: i64,
    preamble_symbols: i64,
    requested_power_dbm: i64,
    spreading_factor: i64,
    coding_rate_denominator: i64,
    explicit_header: i64,
    crc: i64,
    iq_inverted: i64,
}

fn raw_rf_trace(row: &Row<'_>) -> rusqlite::Result<RawRfTrace> {
    Ok(RawRfTrace {
        id: row.get(0)?,
        boot_id: row.get(1)?,
        imported_at_unix_ms: row.get(2)?,
        event_sequence: row.get(3)?,
        observed_at_us: row.get(4)?,
        kind: row.get(5)?,
        interface_id: row.get(6)?,
        packet_len: row.get(7)?,
        packet_sha256: row.get(8)?,
        attempt_token: row.get(9)?,
        route_destination: row.get(10)?,
        route_next_hop: row.get(11)?,
        route_hops: row.get(12)?,
        route_resolution: row.get(13)?,
        submission_id: row.get(14)?,
        tx_outcome: row.get(15)?,
        planned_frames: row.get(16)?,
        completed_frames: row.get(17)?,
        frame_completed_0_us: row.get(18)?,
        frame_completed_1_us: row.get(19)?,
        authorized: row.get(20)?,
        rx_rssi: row.get(21)?,
        rx_snr: row.get(22)?,
        attempt_outcome: row.get(23)?,
        proof_interface: row.get(24)?,
        proof_rssi: row.get(25)?,
        proof_snr: row.get(26)?,
        inbound_stage: row.get(27)?,
        inbound_message_id: row.get(28)?,
        timeline_sequence: row.get(29)?,
        outbox_id: row.get(30)?,
        attempt_number: row.get(31)?,
        location_state: row.get(32)?,
        location_latitude_e6: row.get(33)?,
        location_longitude_e6: row.get(34)?,
        location_accuracy_mm: row.get(35)?,
        location_altitude_mm: row.get(36)?,
        location_vertical_accuracy_mm: row.get(37)?,
        location_captured_at_unix_ms: row.get(38)?,
        location_authorization: row.get(39)?,
        location_source: row.get(40)?,
        location_mocked: row.get(41)?,
        location_unavailable_reason: row.get(42)?,
        profile_fingerprint: row.get(43)?,
        frequency_hz: row.get(44)?,
        bandwidth_hz: row.get(45)?,
        preamble_symbols: row.get(46)?,
        requested_power_dbm: row.get(47)?,
        spreading_factor: row.get(48)?,
        coding_rate_denominator: row.get(49)?,
        explicit_header: row.get(50)?,
        crc: row.get(51)?,
        iq_inverted: row.get(52)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn decode_rf_attempt_location(
    state: Option<i64>,
    latitude_e6: Option<i64>,
    longitude_e6: Option<i64>,
    accuracy_mm: Option<i64>,
    altitude_mm: Option<i64>,
    vertical_accuracy_mm: Option<i64>,
    captured_at_unix_ms: Option<i64>,
    authorization: Option<i64>,
    source: Option<i64>,
    mocked: Option<i64>,
    unavailable_reason: Option<i64>,
) -> Result<Option<AttemptLocationStamp>, SqliteStoreError> {
    let no_sample_fields = latitude_e6.is_none()
        && longitude_e6.is_none()
        && accuracy_mm.is_none()
        && altitude_mm.is_none()
        && vertical_accuracy_mm.is_none()
        && captured_at_unix_ms.is_none()
        && authorization.is_none()
        && source.is_none()
        && mocked.is_none();
    match state {
        None if no_sample_fields && unavailable_reason.is_none() => Ok(None),
        Some(0) if unavailable_reason.is_none() => {
            let sample = decode_phone_location(
                latitude_e6,
                longitude_e6,
                accuracy_mm,
                altitude_mm,
                vertical_accuracy_mm,
                captured_at_unix_ms,
                authorization,
                source,
                mocked,
                "RF trace phone location sample",
            )?
            .ok_or(SqliteStoreError::CorruptData(
                "RF trace phone location sample",
            ))?;
            Ok(Some(AttemptLocationStamp::Available(sample)))
        }
        Some(1) if no_sample_fields => Ok(Some(AttemptLocationStamp::Unavailable(
            match unavailable_reason {
                Some(0) => PhoneLocationUnavailableReason::NotObserved,
                Some(1) => PhoneLocationUnavailableReason::TelemetryDisabled,
                Some(2) => PhoneLocationUnavailableReason::PermissionDenied,
                Some(3) => PhoneLocationUnavailableReason::ServicesDisabled,
                Some(4) => PhoneLocationUnavailableReason::PlatformUnavailable,
                Some(5) => PhoneLocationUnavailableReason::NoFixYet,
                Some(6) => PhoneLocationUnavailableReason::ProviderError,
                _ => {
                    return Err(SqliteStoreError::CorruptData(
                        "RF trace location unavailable reason",
                    ));
                }
            },
        ))),
        Some(_) | None => Err(SqliteStoreError::CorruptData("RF trace location stamp")),
    }
}

fn decode_rf_profile(raw: &RawRfTrace) -> Result<RfTraceRadioProfile, SqliteStoreError> {
    RfTraceRadioProfile::new(
        array_from_blob(
            raw.profile_fingerprint.clone(),
            "RF trace profile fingerprint",
        )?,
        u32::try_from(raw.frequency_hz)
            .map_err(|_| SqliteStoreError::CorruptData("RF trace frequency"))?,
        u32::try_from(raw.bandwidth_hz)
            .map_err(|_| SqliteStoreError::CorruptData("RF trace bandwidth"))?,
        u16::try_from(raw.preamble_symbols)
            .map_err(|_| SqliteStoreError::CorruptData("RF trace preamble"))?,
        i16::try_from(raw.requested_power_dbm)
            .map_err(|_| SqliteStoreError::CorruptData("RF trace power"))?,
        u8::try_from(raw.spreading_factor)
            .map_err(|_| SqliteStoreError::CorruptData("RF trace spreading factor"))?,
        u8::try_from(raw.coding_rate_denominator)
            .map_err(|_| SqliteStoreError::CorruptData("RF trace coding rate"))?,
        bool_from_integer(raw.explicit_header, "RF trace explicit header")?,
        bool_from_integer(raw.crc, "RF trace CRC")?,
        bool_from_integer(raw.iq_inverted, "RF trace IQ polarity")?,
    )
    .ok_or(SqliteStoreError::CorruptData("RF trace profile"))
}

fn decode_rf_tx_outcome(value: i64) -> Result<RfTraceTxOutcome, SqliteStoreError> {
    Ok(match value {
        0 => RfTraceTxOutcome::Transmitted,
        1 => RfTraceTxOutcome::AccessRejected,
        2 => RfTraceTxOutcome::PermitDenied,
        3 => RfTraceTxOutcome::AuthorizationExpired,
        4 => RfTraceTxOutcome::PostGrantAccessRejected,
        5 => RfTraceTxOutcome::AirtimeRejected,
        6 => RfTraceTxOutcome::DeadlineConversionOverflow,
        7 => RfTraceTxOutcome::RadioInactive,
        8 => RfTraceTxOutcome::InterfaceConfigurationMismatch,
        9 => RfTraceTxOutcome::RadioConfigurationChangedBeforePermit,
        10 => RfTraceTxOutcome::RadioConfigurationChangedAfterPermit,
        11 => RfTraceTxOutcome::CadFault,
        12 => RfTraceTxOutcome::TxFault,
        13 => RfTraceTxOutcome::ControlPlaneRecovery,
        14 => RfTraceTxOutcome::FrameInvariantRecovery,
        15 => RfTraceTxOutcome::CancelledRadioOperation,
        _ => return Err(SqliteStoreError::CorruptData("RF trace TX outcome")),
    })
}

fn decode_rf_route_resolution(value: i64) -> Result<RfTraceRouteResolution, SqliteStoreError> {
    Ok(match value {
        0 => RfTraceRouteResolution::ExactReady,
        1 => RfTraceRouteResolution::ExactOffline,
        2 => RfTraceRouteResolution::ExactMissing,
        3 => RfTraceRouteResolution::BroadcastReady,
        4 => RfTraceRouteResolution::BroadcastUnavailable,
        _ => return Err(SqliteStoreError::CorruptData("RF trace route resolution")),
    })
}

fn decode_rf_attempt_outcome(value: i64) -> Result<RfTraceAttemptOutcome, SqliteStoreError> {
    Ok(match value {
        0 => RfTraceAttemptOutcome::Delivered,
        1 => RfTraceAttemptOutcome::DeliveryTimeout,
        2 => RfTraceAttemptOutcome::Unsent,
        _ => return Err(SqliteStoreError::CorruptData("RF trace attempt outcome")),
    })
}

fn decode_rf_inbound_proof_stage(value: i64) -> Result<RfTraceInboundProofStage, SqliteStoreError> {
    Ok(match value {
        0 => RfTraceInboundProofStage::DataLogicalRx,
        4 => RfTraceInboundProofStage::OrdinaryQueued,
        5 => RfTraceInboundProofStage::PhysicalTxDone,
        6 => RfTraceInboundProofStage::PhysicalTxFailed,
        _ => {
            return Err(SqliteStoreError::CorruptData(
                "RF trace inbound proof stage",
            ));
        }
    })
}

fn decode_rf_packet(
    raw: &RawRfTrace,
) -> Result<(RfTraceInterfaceId, PacketEvidence), SqliteStoreError> {
    let interface = RfTraceInterfaceId::new(array_from_blob(
        raw.interface_id
            .clone()
            .ok_or(SqliteStoreError::CorruptData("RF trace interface"))?,
        "RF trace interface",
    )?);
    let length = u16::try_from(
        raw.packet_len
            .ok_or(SqliteStoreError::CorruptData("RF trace packet length"))?,
    )
    .map_err(|_| SqliteStoreError::CorruptData("RF trace packet length"))?;
    let evidence = PacketEvidence::new(
        length,
        EncodedPacketSha256::new(array_from_blob(
            raw.packet_sha256
                .clone()
                .ok_or(SqliteStoreError::CorruptData("RF trace packet digest"))?,
            "RF trace packet digest",
        )?),
    )
    .map_err(|_| SqliteStoreError::CorruptData("RF trace packet evidence"))?;
    Ok((interface, evidence))
}

fn decode_rf_token(raw: &RawRfTrace) -> Result<Option<RnsAttemptToken>, SqliteStoreError> {
    raw.attempt_token
        .clone()
        .map(|bytes| {
            Ok(RnsAttemptToken::new(array_from_blob(
                bytes,
                "RF trace attempt token",
            )?))
        })
        .transpose()
}

fn decode_rf_optional_packet(raw: &RawRfTrace) -> Result<Option<PacketEvidence>, SqliteStoreError> {
    match (raw.packet_len, raw.packet_sha256.clone()) {
        (None, None) => Ok(None),
        (Some(length), Some(sha256)) => Ok(Some(
            PacketEvidence::new(
                u16::try_from(length)
                    .map_err(|_| SqliteStoreError::CorruptData("RF trace packet length"))?,
                EncodedPacketSha256::new(array_from_blob(sha256, "RF trace packet digest")?),
            )
            .map_err(|_| SqliteStoreError::CorruptData("RF trace packet evidence"))?,
        )),
        _ => Err(SqliteStoreError::CorruptData(
            "RF trace partial packet evidence",
        )),
    }
}

fn decode_rf_trace(raw: RawRfTrace) -> Result<RfTraceEvent, SqliteStoreError> {
    let id = RfTraceEventId::new(positive_u64(raw.id, "RF trace id")?)
        .ok_or(SqliteStoreError::CorruptData("RF trace id"))?;
    let boot_id = RfTraceBootId::new(u64_from_blob(raw.boot_id.clone(), "RF trace boot id")?);
    let profile = decode_rf_profile(&raw)?;
    let imported_at_unix_ms = u64::try_from(raw.imported_at_unix_ms)
        .ok()
        .filter(|value| *value <= crate::MAX_UNIX_TIMESTAMP_MILLIS)
        .ok_or(SqliteStoreError::CorruptData("RF trace imported timestamp"))?;
    let event_sequence = RfTraceEventSequence::new(u64_from_blob(
        raw.event_sequence.clone(),
        "RF trace event sequence",
    )?)
    .ok_or(SqliteStoreError::CorruptData("RF trace event sequence"))?;
    let observed_at_us = u64_from_blob(raw.observed_at_us.clone(), "RF trace observed time")?;
    let token = decode_rf_token(&raw)?;
    let kind = match raw.kind {
        0 => {
            let (interface, packet) = decode_rf_packet(&raw)?;
            let token = token.ok_or(SqliteStoreError::CorruptData("RF trace TX token"))?;
            let planned = u8::try_from(
                raw.planned_frames
                    .ok_or(SqliteStoreError::CorruptData("RF trace planned frames"))?,
            )
            .map_err(|_| SqliteStoreError::CorruptData("RF trace planned frames"))?;
            let completed = u8::try_from(
                raw.completed_frames
                    .ok_or(SqliteStoreError::CorruptData("RF trace completed frames"))?,
            )
            .map_err(|_| SqliteStoreError::CorruptData("RF trace completed frames"))?;
            let frame_completed_at_us = [
                raw.frame_completed_0_us
                    .clone()
                    .map(|value| u64_from_blob(value, "RF trace first frame completion"))
                    .transpose()?,
                raw.frame_completed_1_us
                    .clone()
                    .map(|value| u64_from_blob(value, "RF trace second frame completion"))
                    .transpose()?,
            ];
            let submission_id = raw
                .submission_id
                .clone()
                .map(|value| {
                    SubmissionId::new(u64_from_blob(value, "RF trace submission id")?)
                        .map_err(|_| SqliteStoreError::CorruptData("RF trace submission id"))
                })
                .transpose()?;
            RfTraceObservationKind::DataTx(
                RfTraceTxObservation::new(
                    token,
                    interface,
                    packet,
                    decode_rf_tx_outcome(
                        raw.tx_outcome
                            .ok_or(SqliteStoreError::CorruptData("RF trace TX outcome"))?,
                    )?,
                    planned,
                    completed,
                    frame_completed_at_us,
                    bool_from_integer(
                        raw.authorized
                            .ok_or(SqliteStoreError::CorruptData("RF trace authorized flag"))?,
                        "RF trace authorized flag",
                    )?,
                    submission_id,
                )
                .ok_or(SqliteStoreError::CorruptData("RF trace TX evidence"))?,
            )
        }
        1 => {
            let (interface, packet) = decode_rf_packet(&raw)?;
            RfTraceObservationKind::LogicalRx(RfTraceRxObservation::new(
                interface,
                packet,
                token,
                i16::try_from(
                    raw.rx_rssi
                        .ok_or(SqliteStoreError::CorruptData("RF trace RX RSSI"))?,
                )
                .map_err(|_| SqliteStoreError::CorruptData("RF trace RX RSSI"))?,
                i16::try_from(
                    raw.rx_snr
                        .ok_or(SqliteStoreError::CorruptData("RF trace RX SNR"))?,
                )
                .map_err(|_| SqliteStoreError::CorruptData("RF trace RX SNR"))?,
            ))
        }
        2 => {
            let (interface, packet) = decode_rf_packet(&raw)?;
            RfTraceObservationKind::RouteSelected(RfTraceRouteObservation::new(
                DestinationHash::new(array_from_blob(
                    raw.route_destination
                        .clone()
                        .ok_or(SqliteStoreError::CorruptData("RF trace route destination"))?,
                    "RF trace route destination",
                )?),
                raw.route_next_hop
                    .clone()
                    .map(|value| {
                        Ok::<_, SqliteStoreError>(RfTraceIdentityHash::new(array_from_blob(
                            value,
                            "RF trace route next hop",
                        )?))
                    })
                    .transpose()?,
                u8::try_from(
                    raw.route_hops
                        .ok_or(SqliteStoreError::CorruptData("RF trace route hops"))?,
                )
                .map_err(|_| SqliteStoreError::CorruptData("RF trace route hops"))?,
                interface,
                decode_rf_route_resolution(
                    raw.route_resolution
                        .ok_or(SqliteStoreError::CorruptData("RF trace route resolution"))?,
                )?,
                packet,
                token.ok_or(SqliteStoreError::CorruptData("RF trace route token"))?,
                SubmissionId::new(u64_from_blob(
                    raw.submission_id
                        .clone()
                        .ok_or(SqliteStoreError::CorruptData("RF trace route submission"))?,
                    "RF trace route submission",
                )?)
                .map_err(|_| SqliteStoreError::CorruptData("RF trace route submission"))?,
            ))
        }
        3 if raw.inbound_stage.is_some() => {
            if raw.attempt_outcome.is_some()
                || raw.proof_interface.is_some()
                || raw.proof_rssi.is_some()
                || raw.proof_snr.is_some()
            {
                return Err(SqliteStoreError::CorruptData(
                    "RF trace inbound proof mixed terminal evidence",
                ));
            }
            let interface = raw
                .interface_id
                .clone()
                .map(|interface| {
                    array_from_blob(interface, "RF trace proof interface")
                        .map(RfTraceInterfaceId::new)
                })
                .transpose()?;
            let signal = match (raw.rx_rssi, raw.rx_snr) {
                (None, None) => None,
                (Some(rssi), Some(snr)) => Some((
                    i16::try_from(rssi)
                        .map_err(|_| SqliteStoreError::CorruptData("RF trace proof RSSI"))?,
                    i16::try_from(snr)
                        .map_err(|_| SqliteStoreError::CorruptData("RF trace proof SNR"))?,
                )),
                _ => {
                    return Err(SqliteStoreError::CorruptData(
                        "RF trace partial inbound proof signal",
                    ));
                }
            };
            RfTraceObservationKind::InboundProof(
                RfTraceInboundProofObservation::new(
                    token.ok_or(SqliteStoreError::CorruptData(
                        "RF trace inbound proof token",
                    ))?,
                    decode_rf_inbound_proof_stage(raw.inbound_stage.ok_or(
                        SqliteStoreError::CorruptData("RF trace inbound proof stage"),
                    )?)?,
                    raw.inbound_message_id
                        .clone()
                        .map(|message_id| {
                            Ok::<_, SqliteStoreError>(MessageId::new(array_from_blob(
                                message_id,
                                "RF trace inbound message id",
                            )?))
                        })
                        .transpose()?,
                    decode_rf_optional_packet(&raw)?,
                    interface,
                    signal,
                    raw.tx_outcome.map(decode_rf_tx_outcome).transpose()?,
                )
                .ok_or(SqliteStoreError::CorruptData(
                    "RF trace inbound proof evidence",
                ))?,
            )
        }
        3 => {
            let proof_ingress = match (raw.proof_interface, raw.proof_rssi, raw.proof_snr) {
                (None, None, None) => None,
                (Some(interface), None, None) => Some(RfTraceProofIngress::new(
                    RfTraceInterfaceId::new(array_from_blob(
                        interface,
                        "RF trace proof interface",
                    )?),
                    None,
                )),
                (Some(interface), Some(rssi), Some(snr)) => Some(RfTraceProofIngress::new(
                    RfTraceInterfaceId::new(array_from_blob(
                        interface,
                        "RF trace proof interface",
                    )?),
                    Some((
                        i16::try_from(rssi)
                            .map_err(|_| SqliteStoreError::CorruptData("RF trace proof RSSI"))?,
                        i16::try_from(snr)
                            .map_err(|_| SqliteStoreError::CorruptData("RF trace proof SNR"))?,
                    )),
                )),
                _ => {
                    return Err(SqliteStoreError::CorruptData(
                        "RF trace proof ingress evidence",
                    ));
                }
            };
            RfTraceObservationKind::AttemptTerminal(RfTraceAttemptObservation::new(
                token.ok_or(SqliteStoreError::CorruptData("RF trace terminal token"))?,
                decode_rf_attempt_outcome(
                    raw.attempt_outcome
                        .ok_or(SqliteStoreError::CorruptData("RF trace attempt outcome"))?,
                )?,
                proof_ingress,
            ))
        }
        _ => return Err(SqliteStoreError::CorruptData("RF trace kind")),
    };
    let attempt_location = decode_rf_attempt_location(
        raw.location_state,
        raw.location_latitude_e6,
        raw.location_longitude_e6,
        raw.location_accuracy_mm,
        raw.location_altitude_mm,
        raw.location_vertical_accuracy_mm,
        raw.location_captured_at_unix_ms,
        raw.location_authorization,
        raw.location_source,
        raw.location_mocked,
        raw.location_unavailable_reason,
    )?;
    let correlation = match (
        raw.timeline_sequence,
        raw.outbox_id,
        raw.attempt_number,
        attempt_location,
    ) {
        (None, None, None, None) => None,
        (Some(timeline), Some(outbox), Some(attempt), Some(location)) => {
            Some(RfTraceMessageCorrelation::new(
                TimelineSequence::new(positive_u64(timeline, "RF trace timeline sequence")?)
                    .ok_or(SqliteStoreError::CorruptData("RF trace timeline sequence"))?,
                OutboxId::new(positive_u64(outbox, "RF trace outbox id")?)
                    .ok_or(SqliteStoreError::CorruptData("RF trace outbox id"))?,
                u32::try_from(attempt)
                    .ok()
                    .and_then(MessageAttemptNumber::new)
                    .ok_or(SqliteStoreError::CorruptData("RF trace attempt number"))?,
                location,
            ))
        }
        _ => {
            return Err(SqliteStoreError::CorruptData(
                "RF trace message correlation",
            ));
        }
    };
    Ok(RfTraceEvent {
        id,
        boot_id,
        profile,
        imported_at_unix_ms,
        observation: RfTraceObservation::new(event_sequence, observed_at_us, kind),
        correlation,
    })
}

struct EncodedRfObservation {
    kind: i64,
    interface_id: Option<Vec<u8>>,
    packet_len: Option<i64>,
    packet_sha256: Option<Vec<u8>>,
    attempt_token: Option<Vec<u8>>,
    route_destination: Option<Vec<u8>>,
    route_next_hop: Option<Vec<u8>>,
    route_hops: Option<i64>,
    route_resolution: Option<i64>,
    submission_id: Option<Vec<u8>>,
    tx_outcome: Option<i64>,
    planned_frames: Option<i64>,
    completed_frames: Option<i64>,
    frame_completed_0_us: Option<Vec<u8>>,
    frame_completed_1_us: Option<Vec<u8>>,
    authorized: Option<i64>,
    rx_rssi: Option<i64>,
    rx_snr: Option<i64>,
    attempt_outcome: Option<i64>,
    proof_interface: Option<Vec<u8>>,
    proof_rssi: Option<i64>,
    proof_snr: Option<i64>,
    inbound_stage: Option<i64>,
    inbound_message_id: Option<Vec<u8>>,
}

fn encode_rf_tx_outcome(outcome: RfTraceTxOutcome) -> i64 {
    match outcome {
        RfTraceTxOutcome::Transmitted => 0,
        RfTraceTxOutcome::AccessRejected => 1,
        RfTraceTxOutcome::PermitDenied => 2,
        RfTraceTxOutcome::AuthorizationExpired => 3,
        RfTraceTxOutcome::PostGrantAccessRejected => 4,
        RfTraceTxOutcome::AirtimeRejected => 5,
        RfTraceTxOutcome::DeadlineConversionOverflow => 6,
        RfTraceTxOutcome::RadioInactive => 7,
        RfTraceTxOutcome::InterfaceConfigurationMismatch => 8,
        RfTraceTxOutcome::RadioConfigurationChangedBeforePermit => 9,
        RfTraceTxOutcome::RadioConfigurationChangedAfterPermit => 10,
        RfTraceTxOutcome::CadFault => 11,
        RfTraceTxOutcome::TxFault => 12,
        RfTraceTxOutcome::ControlPlaneRecovery => 13,
        RfTraceTxOutcome::FrameInvariantRecovery => 14,
        RfTraceTxOutcome::CancelledRadioOperation => 15,
    }
}

fn encode_rf_route_resolution(resolution: RfTraceRouteResolution) -> i64 {
    match resolution {
        RfTraceRouteResolution::ExactReady => 0,
        RfTraceRouteResolution::ExactOffline => 1,
        RfTraceRouteResolution::ExactMissing => 2,
        RfTraceRouteResolution::BroadcastReady => 3,
        RfTraceRouteResolution::BroadcastUnavailable => 4,
    }
}

fn encode_rf_attempt_outcome(outcome: RfTraceAttemptOutcome) -> i64 {
    match outcome {
        RfTraceAttemptOutcome::Delivered => 0,
        RfTraceAttemptOutcome::DeliveryTimeout => 1,
        RfTraceAttemptOutcome::Unsent => 2,
    }
}

fn encode_rf_inbound_proof_stage(stage: RfTraceInboundProofStage) -> i64 {
    match stage {
        RfTraceInboundProofStage::DataLogicalRx => 0,
        RfTraceInboundProofStage::OrdinaryQueued => 4,
        RfTraceInboundProofStage::PhysicalTxDone => 5,
        RfTraceInboundProofStage::PhysicalTxFailed => 6,
    }
}

fn encode_rf_observation(observation: RfTraceObservation) -> EncodedRfObservation {
    let mut encoded = EncodedRfObservation {
        kind: 0,
        interface_id: None,
        packet_len: None,
        packet_sha256: None,
        attempt_token: None,
        route_destination: None,
        route_next_hop: None,
        route_hops: None,
        route_resolution: None,
        submission_id: None,
        tx_outcome: None,
        planned_frames: None,
        completed_frames: None,
        frame_completed_0_us: None,
        frame_completed_1_us: None,
        authorized: None,
        rx_rssi: None,
        rx_snr: None,
        attempt_outcome: None,
        proof_interface: None,
        proof_rssi: None,
        proof_snr: None,
        inbound_stage: None,
        inbound_message_id: None,
    };
    match observation.kind() {
        RfTraceObservationKind::DataTx(tx) => {
            encoded.kind = 0;
            encoded.interface_id = Some(tx.interface().as_bytes().to_vec());
            encoded.packet_len = Some(i64::from(tx.packet_evidence().encoded_packet_len()));
            encoded.packet_sha256 = Some(
                tx.packet_evidence()
                    .encoded_packet_sha256()
                    .as_bytes()
                    .to_vec(),
            );
            encoded.attempt_token = Some(tx.rns_attempt_token().as_bytes().to_vec());
            encoded.submission_id = tx
                .submission_id()
                .map(|id| u64_blob(id.get()).as_slice().to_vec());
            encoded.tx_outcome = Some(encode_rf_tx_outcome(tx.outcome()));
            encoded.planned_frames = Some(i64::from(tx.planned_physical_frames()));
            encoded.completed_frames = Some(i64::from(tx.completed_physical_frames()));
            let frame_times = tx.frame_completed_at_us();
            encoded.frame_completed_0_us =
                frame_times[0].map(|value| u64_blob(value).as_slice().to_vec());
            encoded.frame_completed_1_us =
                frame_times[1].map(|value| u64_blob(value).as_slice().to_vec());
            encoded.authorized = Some(sqlite_bool(tx.authorized_frame_observed()));
        }
        RfTraceObservationKind::LogicalRx(rx) => {
            encoded.kind = 1;
            encoded.interface_id = Some(rx.interface().as_bytes().to_vec());
            encoded.packet_len = Some(i64::from(rx.packet_evidence().encoded_packet_len()));
            encoded.packet_sha256 = Some(
                rx.packet_evidence()
                    .encoded_packet_sha256()
                    .as_bytes()
                    .to_vec(),
            );
            encoded.attempt_token = rx.rns_packet_hash().map(|token| token.as_bytes().to_vec());
            encoded.rx_rssi = Some(i64::from(rx.rssi_dbm()));
            encoded.rx_snr = Some(i64::from(rx.snr_db()));
        }
        RfTraceObservationKind::RouteSelected(route) => {
            encoded.kind = 2;
            encoded.interface_id = Some(route.selected_interface().as_bytes().to_vec());
            encoded.packet_len = Some(i64::from(route.packet_evidence().encoded_packet_len()));
            encoded.packet_sha256 = Some(
                route
                    .packet_evidence()
                    .encoded_packet_sha256()
                    .as_bytes()
                    .to_vec(),
            );
            encoded.attempt_token = Some(route.rns_attempt_token().as_bytes().to_vec());
            encoded.route_destination = Some(route.destination().as_bytes().to_vec());
            encoded.route_next_hop = route.next_hop().map(|hash| hash.as_bytes().to_vec());
            encoded.route_hops = Some(i64::from(route.hops()));
            encoded.route_resolution = Some(encode_rf_route_resolution(route.resolution()));
            encoded.submission_id = Some(u64_blob(route.submission_id().get()).as_slice().to_vec());
        }
        RfTraceObservationKind::AttemptTerminal(terminal) => {
            encoded.kind = 3;
            encoded.attempt_token = Some(terminal.rns_attempt_token().as_bytes().to_vec());
            encoded.attempt_outcome = Some(encode_rf_attempt_outcome(terminal.outcome()));
            if let Some(proof) = terminal.proof_ingress() {
                encoded.proof_interface = Some(proof.interface().as_bytes().to_vec());
                if let Some((rssi, snr)) = proof.signal() {
                    encoded.proof_rssi = Some(i64::from(rssi));
                    encoded.proof_snr = Some(i64::from(snr));
                }
            }
        }
        RfTraceObservationKind::InboundProof(proof) => {
            // Kind 3 is the persisted proof-event family. `inbound_stage`
            // distinguishes receiver lifecycle events from attempt terminals.
            encoded.kind = 3;
            encoded.attempt_token = Some(proof.rns_attempt_token().as_bytes().to_vec());
            encoded.inbound_stage = Some(encode_rf_inbound_proof_stage(proof.stage()));
            encoded.inbound_message_id = proof
                .message_id()
                .map(|message_id| message_id.as_bytes().to_vec());
            if let Some(packet) = proof.packet_evidence() {
                encoded.packet_len = Some(i64::from(packet.encoded_packet_len()));
                encoded.packet_sha256 = Some(packet.encoded_packet_sha256().as_bytes().to_vec());
            }
            encoded.interface_id = proof
                .interface()
                .map(|interface| interface.as_bytes().to_vec());
            if let Some((rssi, snr)) = proof.signal() {
                encoded.rx_rssi = Some(i64::from(rssi));
                encoded.rx_snr = Some(i64::from(snr));
            }
            encoded.tx_outcome = proof.dispatch_outcome().map(encode_rf_tx_outcome);
        }
    }
    encoded
}

struct EncodedRfCorrelation {
    timeline_sequence: Option<i64>,
    outbox_id: Option<i64>,
    attempt_number: Option<i64>,
    location: EncodedAttemptLocation,
}

fn encode_rf_correlation(
    correlation: Option<RfTraceMessageCorrelation>,
) -> Result<EncodedRfCorrelation, SqliteStoreError> {
    match correlation {
        None => Ok(EncodedRfCorrelation {
            timeline_sequence: None,
            outbox_id: None,
            attempt_number: None,
            location: encode_attempt_location(None)?,
        }),
        Some(correlation) => Ok(EncodedRfCorrelation {
            timeline_sequence: Some(sqlite_integer(
                correlation.timeline_sequence().get(),
                "RF trace timeline sequence",
            )?),
            outbox_id: Some(sqlite_integer(
                correlation.outbox_id().get(),
                "RF trace outbox id",
            )?),
            attempt_number: Some(i64::from(correlation.attempt_number().get())),
            location: encode_attempt_location(Some(correlation.attempt_location()))?,
        }),
    }
}

fn ensure_rf_trace_boot(
    transaction: &Transaction<'_>,
    boot_id: RfTraceBootId,
    profile: RfTraceRadioProfile,
) -> Result<(), SqliteStoreError> {
    let boot_blob = u64_blob(boot_id.get());
    let existing = transaction
        .query_row(
            "SELECT profile_fingerprint, frequency_hz, bandwidth_hz, preamble_symbols,\n\
                    requested_power_dbm, spreading_factor, coding_rate_denominator,\n\
                    explicit_header, crc, iq_inverted\n\
             FROM rf_trace_boots WHERE boot_id = ?1",
            [boot_blob.as_slice()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()?;
    if let Some((fingerprint, frequency, bandwidth, preamble, power, sf, cr, header, crc, iq)) =
        existing
    {
        let existing = RfTraceRadioProfile::new(
            array_from_blob(fingerprint, "RF trace profile fingerprint")?,
            u32::try_from(frequency)
                .map_err(|_| SqliteStoreError::CorruptData("RF trace frequency"))?,
            u32::try_from(bandwidth)
                .map_err(|_| SqliteStoreError::CorruptData("RF trace bandwidth"))?,
            u16::try_from(preamble)
                .map_err(|_| SqliteStoreError::CorruptData("RF trace preamble"))?,
            i16::try_from(power).map_err(|_| SqliteStoreError::CorruptData("RF trace power"))?,
            u8::try_from(sf)
                .map_err(|_| SqliteStoreError::CorruptData("RF trace spreading factor"))?,
            u8::try_from(cr).map_err(|_| SqliteStoreError::CorruptData("RF trace coding rate"))?,
            bool_from_integer(header, "RF trace explicit header")?,
            bool_from_integer(crc, "RF trace CRC")?,
            bool_from_integer(iq, "RF trace IQ polarity")?,
        )
        .ok_or(SqliteStoreError::CorruptData("RF trace profile"))?;
        if existing != profile {
            return Err(ChatStoreError::RfTraceBootProfileConflict(boot_id).into());
        }
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO rf_trace_boots(\n\
             boot_id, profile_fingerprint, frequency_hz, bandwidth_hz, preamble_symbols,\n\
             requested_power_dbm, spreading_factor, coding_rate_denominator, explicit_header,\n\
             crc, iq_inverted, history_incomplete\n\
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
        params![
            boot_blob.as_slice(),
            profile.fingerprint().as_slice(),
            i64::from(profile.frequency_hz()),
            i64::from(profile.bandwidth_hz()),
            i64::from(profile.preamble_symbols()),
            i64::from(profile.requested_power_dbm()),
            i64::from(profile.spreading_factor()),
            i64::from(profile.coding_rate_denominator()),
            sqlite_bool(profile.explicit_header()),
            sqlite_bool(profile.crc()),
            sqlite_bool(profile.iq_inverted()),
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_rf_correlation_columns(
    timeline_sequence: i64,
    outbox_id: i64,
    attempt_number: i64,
    location_state: Option<i64>,
    location_latitude_e6: Option<i64>,
    location_longitude_e6: Option<i64>,
    location_accuracy_mm: Option<i64>,
    location_altitude_mm: Option<i64>,
    location_vertical_accuracy_mm: Option<i64>,
    location_captured_at_unix_ms: Option<i64>,
    location_authorization: Option<i64>,
    location_source: Option<i64>,
    location_mocked: Option<i64>,
    location_unavailable_reason: Option<i64>,
) -> Result<RfTraceMessageCorrelation, SqliteStoreError> {
    let location = decode_rf_attempt_location(
        location_state,
        location_latitude_e6,
        location_longitude_e6,
        location_accuracy_mm,
        location_altitude_mm,
        location_vertical_accuracy_mm,
        location_captured_at_unix_ms,
        location_authorization,
        location_source,
        location_mocked,
        location_unavailable_reason,
    )?
    .ok_or(SqliteStoreError::CorruptData(
        "RF trace correlation location",
    ))?;
    Ok(RfTraceMessageCorrelation::new(
        TimelineSequence::new(positive_u64(
            timeline_sequence,
            "RF trace timeline sequence",
        )?)
        .ok_or(SqliteStoreError::CorruptData("RF trace timeline sequence"))?,
        OutboxId::new(positive_u64(outbox_id, "RF trace outbox id")?)
            .ok_or(SqliteStoreError::CorruptData("RF trace outbox id"))?,
        u32::try_from(attempt_number)
            .ok()
            .and_then(MessageAttemptNumber::new)
            .ok_or(SqliteStoreError::CorruptData("RF trace attempt number"))?,
        location,
    ))
}

fn rf_trace_correlation_for_submission(
    transaction: &Transaction<'_>,
    submission_id: SubmissionId,
) -> Result<Option<RfTraceMessageCorrelation>, SqliteStoreError> {
    let Ok(submission_id) = sqlite_integer(submission_id.get(), "RF trace submission id") else {
        return Ok(None);
    };
    let mut statement = transaction.prepare(
        "SELECT accepted.timeline_sequence, accepted.outbox_id, accepted.attempt_number,\n\
                begin.location_state, begin.location_latitude_e6, begin.location_longitude_e6,\n\
                begin.location_accuracy_mm, begin.location_altitude_mm,\n\
                begin.location_vertical_accuracy_mm, begin.location_captured_at_unix_ms,\n\
                begin.location_authorization, begin.location_source, begin.location_mocked,\n\
                begin.location_unavailable_reason\n\
         FROM message_activity AS accepted\n\
         JOIN message_activity AS begin\n\
           ON begin.outbox_id = accepted.outbox_id\n\
          AND begin.attempt_number = accepted.attempt_number\n\
          AND begin.kind IN (1, 4)\n\
         WHERE accepted.kind = 2 AND accepted.submission_id = ?1",
    )?;
    let mut rows = statement.query([submission_id])?;
    let mut result = None;
    while let Some(row) = rows.next()? {
        let candidate = decode_rf_correlation_columns(
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
            row.get(10)?,
            row.get(11)?,
            row.get(12)?,
            row.get(13)?,
        )?;
        if result.is_some_and(|existing| existing != candidate) {
            return Err(SqliteStoreError::CorruptData(
                "RF trace submission correlation",
            ));
        }
        result = Some(candidate);
    }
    Ok(result)
}

fn seed_rf_token_correlation(
    correlations: &mut BTreeMap<RnsAttemptToken, RfTraceMessageCorrelation>,
    token: RnsAttemptToken,
    correlation: RfTraceMessageCorrelation,
) -> Result<(), SqliteStoreError> {
    if correlations
        .insert(token, correlation)
        .is_some_and(|existing| existing != correlation)
    {
        return Err(ChatStoreError::RfTraceAttemptTokenConflict(token).into());
    }
    Ok(())
}

fn load_rf_token_correlations(
    transaction: &Transaction<'_>,
    token: RnsAttemptToken,
    correlations: &mut BTreeMap<RnsAttemptToken, RfTraceMessageCorrelation>,
) -> Result<(), SqliteStoreError> {
    let mut statement = transaction.prepare(
        "SELECT timeline_sequence, outbox_id, attempt_number, location_state,\n\
                location_latitude_e6, location_longitude_e6, location_accuracy_mm,\n\
                location_altitude_mm, location_vertical_accuracy_mm,\n\
                location_captured_at_unix_ms, location_authorization, location_source,\n\
                location_mocked, location_unavailable_reason\n\
         FROM rf_trace_events\n\
         WHERE attempt_token = ?1 AND timeline_sequence IS NOT NULL",
    )?;
    let mut rows = statement.query([token.as_bytes().as_slice()])?;
    while let Some(row) = rows.next()? {
        let correlation = decode_rf_correlation_columns(
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
            row.get(10)?,
            row.get(11)?,
            row.get(12)?,
            row.get(13)?,
        )?;
        seed_rf_token_correlation(correlations, token, correlation)?;
    }
    drop(rows);
    drop(statement);

    let mut statement = transaction.prepare(
        "SELECT submission_id FROM rf_trace_events\n\
         WHERE attempt_token = ?1 AND submission_id IS NOT NULL",
    )?;
    let mut rows = statement.query([token.as_bytes().as_slice()])?;
    while let Some(row) = rows.next()? {
        let submission_id =
            SubmissionId::new(u64_from_blob(row.get(0)?, "RF trace submission id")?)
                .map_err(|_| SqliteStoreError::CorruptData("RF trace submission id"))?;
        if let Some(correlation) = rf_trace_correlation_for_submission(transaction, submission_id)?
        {
            seed_rf_token_correlation(correlations, token, correlation)?;
        }
    }
    Ok(())
}

fn load_rf_trace_by_replay_key(
    transaction: &Transaction<'_>,
    boot_id: RfTraceBootId,
    sequence: RfTraceEventSequence,
) -> Result<Option<RfTraceEvent>, SqliteStoreError> {
    let sql = format!(
        "SELECT {RF_TRACE_COLUMNS} FROM rf_trace_events AS e\n\
         JOIN rf_trace_boots AS b ON b.boot_id = e.boot_id\n\
         WHERE e.boot_id = ?1 AND e.event_sequence = ?2"
    );
    transaction
        .query_row(
            &sql,
            params![
                u64_blob(boot_id.get()).as_slice(),
                u64_blob(sequence.get()).as_slice(),
            ],
            raw_rf_trace,
        )
        .optional()?
        .map(decode_rf_trace)
        .transpose()
}

fn insert_rf_trace_event(
    transaction: &Transaction<'_>,
    boot_id: RfTraceBootId,
    profile: RfTraceRadioProfile,
    imported_at_unix_ms: u64,
    observation: RfTraceObservation,
    correlation: Option<RfTraceMessageCorrelation>,
) -> Result<RfTraceEventId, SqliteStoreError> {
    let id = allocate_counter(transaction, "rf_trace_id")?;
    let encoded = encode_rf_observation(observation);
    let correlation_columns = encode_rf_correlation(correlation)?;
    let location = correlation_columns.location;
    transaction.execute(
        "INSERT INTO rf_trace_events(\n\
             id, boot_id, imported_at_unix_ms, event_sequence, observed_at_us, kind,\n\
             interface_id, packet_len, packet_sha256, attempt_token, route_destination,\n\
             route_next_hop, route_hops, route_resolution, submission_id, tx_outcome,\n\
             planned_frames, completed_frames, frame_completed_0_us, frame_completed_1_us,\n\
             authorized, rx_rssi, rx_snr, attempt_outcome, proof_interface, proof_rssi,\n\
             proof_snr, inbound_stage, inbound_message_id, timeline_sequence, outbox_id,\n\
             attempt_number, location_state,\n\
             location_latitude_e6, location_longitude_e6, location_accuracy_mm,\n\
             location_altitude_mm, location_vertical_accuracy_mm,\n\
             location_captured_at_unix_ms, location_authorization, location_source,\n\
             location_mocked, location_unavailable_reason\n\
         ) VALUES (\n\
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,\n\
             ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,\n\
             ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43\n\
         )",
        params![
            sqlite_integer(id, "RF trace id")?,
            u64_blob(boot_id.get()).as_slice(),
            sqlite_integer(imported_at_unix_ms, "RF trace imported timestamp")?,
            u64_blob(observation.event_sequence().get()).as_slice(),
            u64_blob(observation.observed_at_us()).as_slice(),
            encoded.kind,
            encoded.interface_id,
            encoded.packet_len,
            encoded.packet_sha256,
            encoded.attempt_token,
            encoded.route_destination,
            encoded.route_next_hop,
            encoded.route_hops,
            encoded.route_resolution,
            encoded.submission_id,
            encoded.tx_outcome,
            encoded.planned_frames,
            encoded.completed_frames,
            encoded.frame_completed_0_us,
            encoded.frame_completed_1_us,
            encoded.authorized,
            encoded.rx_rssi,
            encoded.rx_snr,
            encoded.attempt_outcome,
            encoded.proof_interface,
            encoded.proof_rssi,
            encoded.proof_snr,
            encoded.inbound_stage,
            encoded.inbound_message_id,
            correlation_columns.timeline_sequence,
            correlation_columns.outbox_id,
            correlation_columns.attempt_number,
            location.state,
            location.latitude_e6,
            location.longitude_e6,
            location.accuracy_mm,
            location.altitude_mm,
            location.vertical_accuracy_mm,
            location.captured_at_unix_ms,
            location.authorization,
            location.source,
            location.mocked,
            location.unavailable_reason,
        ],
    )?;
    let id = RfTraceEventId::new(id).ok_or(ChatStoreError::IdentifierExhausted)?;
    let _ = profile;
    Ok(id)
}

fn backfill_rf_trace_correlation(
    transaction: &Transaction<'_>,
    token: RnsAttemptToken,
    correlation: RfTraceMessageCorrelation,
) -> Result<usize, SqliteStoreError> {
    let encoded = encode_rf_correlation(Some(correlation))?;
    let location = encoded.location;
    let changed = transaction.execute(
        "UPDATE rf_trace_events SET\n\
             timeline_sequence = ?2, outbox_id = ?3, attempt_number = ?4,\n\
             location_state = ?5, location_latitude_e6 = ?6, location_longitude_e6 = ?7,\n\
             location_accuracy_mm = ?8, location_altitude_mm = ?9,\n\
             location_vertical_accuracy_mm = ?10, location_captured_at_unix_ms = ?11,\n\
             location_authorization = ?12, location_source = ?13, location_mocked = ?14,\n\
             location_unavailable_reason = ?15\n\
         WHERE attempt_token = ?1 AND timeline_sequence IS NULL",
        params![
            token.as_bytes().as_slice(),
            encoded.timeline_sequence,
            encoded.outbox_id,
            encoded.attempt_number,
            location.state,
            location.latitude_e6,
            location.longitude_e6,
            location.accuracy_mm,
            location.altitude_mm,
            location.vertical_accuracy_mm,
            location.captured_at_unix_ms,
            location.authorization,
            location.source,
            location.mocked,
            location.unavailable_reason,
        ],
    )?;
    Ok(changed)
}

fn update_rf_trace_boot_history(
    transaction: &Transaction<'_>,
    boot_id: RfTraceBootId,
    producer_history_incomplete: bool,
) -> Result<(), SqliteStoreError> {
    let boot_blob = u64_blob(boot_id.get());
    let mut statement = transaction.prepare(
        "SELECT event_sequence FROM rf_trace_events\n\
         WHERE boot_id = ?1 ORDER BY event_sequence ASC",
    )?;
    let mut rows = statement.query([boot_blob.as_slice()])?;
    let mut expected = 1_u64;
    let mut gap = false;
    while let Some(row) = rows.next()? {
        let sequence = u64_from_blob(row.get(0)?, "RF trace event sequence")?;
        if sequence != expected {
            gap = true;
            break;
        }
        let Some(next) = expected.checked_add(1) else {
            break;
        };
        expected = next;
    }
    drop(rows);
    drop(statement);
    if producer_history_incomplete || gap {
        let changed = transaction.execute(
            "UPDATE rf_trace_boots SET history_incomplete = 1 WHERE boot_id = ?1",
            [boot_blob.as_slice()],
        )?;
        if changed != 1 {
            return Err(SqliteStoreError::CorruptData(
                "RF trace boot history update",
            ));
        }
    }
    Ok(())
}

struct PendingMessageActivity {
    observed_at_unix_ms: Option<u64>,
    timeline_sequence: TimelineSequence,
    peer: DestinationHash,
    direction: TimelineDirection,
    outbox_id: Option<OutboxId>,
    attempt_number: Option<MessageAttemptNumber>,
    kind: MessageActivityKind,
}

struct EncodedAttemptLocation {
    state: Option<i64>,
    latitude_e6: Option<i64>,
    longitude_e6: Option<i64>,
    accuracy_mm: Option<i64>,
    altitude_mm: Option<i64>,
    vertical_accuracy_mm: Option<i64>,
    captured_at_unix_ms: Option<i64>,
    authorization: Option<i64>,
    source: Option<i64>,
    mocked: Option<i64>,
    unavailable_reason: Option<i64>,
}

fn encode_attempt_location(
    location: Option<AttemptLocationStamp>,
) -> Result<EncodedAttemptLocation, SqliteStoreError> {
    match location {
        None => Ok(EncodedAttemptLocation {
            state: None,
            latitude_e6: None,
            longitude_e6: None,
            accuracy_mm: None,
            altitude_mm: None,
            vertical_accuracy_mm: None,
            captured_at_unix_ms: None,
            authorization: None,
            source: None,
            mocked: None,
            unavailable_reason: None,
        }),
        Some(AttemptLocationStamp::Available(sample)) => {
            let encoded =
                encode_phone_location(Some(sample), "message activity location capture time")?;
            Ok(EncodedAttemptLocation {
                state: Some(0),
                latitude_e6: encoded.latitude_e6,
                longitude_e6: encoded.longitude_e6,
                accuracy_mm: encoded.horizontal_accuracy_mm,
                altitude_mm: encoded.altitude_mm,
                vertical_accuracy_mm: encoded.vertical_accuracy_mm,
                captured_at_unix_ms: encoded.captured_at_unix_ms,
                authorization: encoded.authorization,
                source: encoded.source,
                mocked: encoded.mocked,
                unavailable_reason: None,
            })
        }
        Some(AttemptLocationStamp::Unavailable(reason)) => Ok(EncodedAttemptLocation {
            state: Some(1),
            latitude_e6: None,
            longitude_e6: None,
            accuracy_mm: None,
            altitude_mm: None,
            vertical_accuracy_mm: None,
            captured_at_unix_ms: None,
            authorization: None,
            source: None,
            mocked: None,
            unavailable_reason: Some(match reason {
                PhoneLocationUnavailableReason::NotObserved => 0,
                PhoneLocationUnavailableReason::TelemetryDisabled => 1,
                PhoneLocationUnavailableReason::PermissionDenied => 2,
                PhoneLocationUnavailableReason::ServicesDisabled => 3,
                PhoneLocationUnavailableReason::PlatformUnavailable => 4,
                PhoneLocationUnavailableReason::NoFixYet => 5,
                PhoneLocationUnavailableReason::ProviderError => 6,
            }),
        }),
    }
}

fn record_message_activity(
    transaction: &Transaction<'_>,
    activity: PendingMessageActivity,
) -> Result<(), SqliteStoreError> {
    let id = allocate_counter(transaction, "message_activity_id")?;
    let (
        kind_value,
        submission_id,
        message_id,
        status_kind,
        failure_kind,
        packet_len,
        packet_sha256,
        attempt_location,
    ) = match activity.kind {
        MessageActivityKind::InboundImported { message_id } => (
            0_i64,
            None,
            Some(message_id.as_bytes().to_vec()),
            None,
            None,
            None,
            None,
            None,
        ),
        MessageActivityKind::OutboundQueued { location } => {
            (1, None, None, None, None, None, None, Some(location))
        }
        MessageActivityKind::OutboundAccepted { acceptance } => (
            2,
            Some(sqlite_integer(
                acceptance.submission_id().get(),
                "message activity submission_id",
            )?),
            Some(acceptance.message_id().as_bytes().to_vec()),
            None,
            None,
            None,
            None,
            None,
        ),
        MessageActivityKind::OutboundStatus { state } => {
            let (status, failure, packet_len, packet_sha256) =
                encode_status(OutboxStatus::Device(state));
            (
                3,
                None,
                None,
                Some(status),
                failure,
                packet_len,
                packet_sha256,
                None,
            )
        }
        MessageActivityKind::OutboundRequeued { location } => {
            (4, None, None, None, None, None, None, Some(location))
        }
    };
    let location = encode_attempt_location(attempt_location)?;
    transaction.execute(
        "INSERT INTO message_activity(\n\
             id, observed_at_unix_ms, timeline_sequence, peer, direction, outbox_id,\n\
             attempt_number, kind, submission_id, message_id, status_kind, failure_kind,\n\
             packet_len, packet_sha256, location_state, location_latitude_e6,\n\
             location_longitude_e6, location_accuracy_mm, location_altitude_mm,\n\
             location_vertical_accuracy_mm, location_captured_at_unix_ms,\n\
             location_authorization, location_source, location_mocked,\n\
             location_unavailable_reason\n\
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,\n\
                  ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
        params![
            sqlite_integer(id, "message activity id")?,
            activity
                .observed_at_unix_ms
                .map(|value| sqlite_integer(value, "message activity observed_at_unix_ms"))
                .transpose()?,
            sqlite_integer(
                activity.timeline_sequence.get(),
                "message activity timeline_sequence"
            )?,
            activity.peer.as_bytes().as_slice(),
            match activity.direction {
                TimelineDirection::Inbound => 0_i64,
                TimelineDirection::Outbound => 1_i64,
            },
            activity
                .outbox_id
                .map(|value| sqlite_integer(value.get(), "message activity outbox_id"))
                .transpose()?,
            activity.attempt_number.map(|value| i64::from(value.get())),
            kind_value,
            submission_id,
            message_id,
            status_kind,
            failure_kind,
            packet_len,
            packet_sha256,
            location.state,
            location.latitude_e6,
            location.longitude_e6,
            location.accuracy_mm,
            location.altitude_mm,
            location.vertical_accuracy_mm,
            location.captured_at_unix_ms,
            location.authorization,
            location.source,
            location.mocked,
            location.unavailable_reason,
        ],
    )?;
    let deleted = transaction.execute(
        "DELETE FROM message_activity\n\
         WHERE id <= (\n\
             SELECT id FROM message_activity ORDER BY id DESC LIMIT 1 OFFSET ?1\n\
         )",
        [i64::try_from(MAX_MESSAGE_ACTIVITY_EVENTS)
            .map_err(|_| SqliteStoreError::ValueOutOfRange("message activity retention"))?],
    )?;
    if deleted != 0 {
        transaction.execute(
            "UPDATE message_activity_meta SET history_incomplete = 1 WHERE singleton = 1",
            [],
        )?;
    }
    Ok(())
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

    fn conversation_peers(&self) -> Result<Vec<ConversationPeer>, Self::Error> {
        let contacts = self.contacts()?;
        let mut inbound = Vec::new();
        {
            let sql = format!("SELECT {INBOUND_COLUMNS} FROM inbound_messages");
            let mut statement = self.connection.prepare(&sql)?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                inbound.push(decode_inbound(raw_inbound(row)?)?);
            }
        }
        let mut outbox = Vec::new();
        {
            let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox");
            let mut statement = self.connection.prepare(&sql)?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                outbox.push(decode_outbox(raw_outbox(row)?)?);
            }
        }
        Ok(project_conversation_peers(
            contacts.iter(),
            inbound.iter(),
            outbox.iter(),
        ))
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

    fn commit_inbound_with_receiver_location(
        &mut self,
        message: InboundMessage,
        receiver_location: Option<PhoneLocationSample>,
    ) -> Result<InboundCommitOutcome, Self::Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sql = format!("SELECT {INBOUND_COLUMNS} FROM inbound_messages WHERE message_id = ?1");
        let existing = transaction
            .query_row(
                &sql,
                [message.message_id().as_bytes().as_slice()],
                raw_inbound,
            )
            .optional()?;
        if let Some(existing) = existing {
            let existing = decode_inbound(existing)?;
            if existing.message().same_authenticated_message(&message) {
                if existing.message().ingress_observation().is_none()
                    && message.ingress_observation().is_some()
                {
                    let (interface, rssi, snr) =
                        encode_ingress_observation(message.ingress_observation());
                    transaction.execute(
                        "UPDATE inbound_messages\n\
                         SET ingress_interface = ?2, ingress_rssi = ?3, ingress_snr = ?4\n\
                         WHERE message_id = ?1 AND ingress_interface IS NULL",
                        params![
                            message.message_id().as_bytes().as_slice(),
                            interface,
                            rssi,
                            snr,
                        ],
                    )?;
                }
                if existing.message().location().is_none() && message.location().is_some() {
                    let (latitude, longitude, altitude, speed, bearing, accuracy, updated_at) =
                        encode_message_location(message.location());
                    transaction.execute(
                        "UPDATE inbound_messages\n\
                         SET location_latitude_e6 = ?2, location_longitude_e6 = ?3,\n\
                             location_altitude_cm = ?4, location_speed_cm_per_second = ?5,\n\
                             location_bearing_centidegrees = ?6, location_accuracy_cm = ?7,\n\
                             location_updated_at_unix_seconds = ?8\n\
                         WHERE message_id = ?1 AND location_latitude_e6 IS NULL",
                        params![
                            message.message_id().as_bytes().as_slice(),
                            latitude,
                            longitude,
                            altitude,
                            speed,
                            bearing,
                            accuracy,
                            updated_at,
                        ],
                    )?;
                }
                transaction.commit()?;
                return Ok(InboundCommitOutcome::Duplicate);
            }
            return Err(ChatStoreError::InboundMessageIdConflict(message.message_id()).into());
        }
        let sequence = allocate_counter(&transaction, "timeline_sequence")?;
        let (ingress_interface, ingress_rssi, ingress_snr) =
            encode_ingress_observation(message.ingress_observation());
        let (latitude, longitude, altitude, speed, bearing, accuracy, updated_at) =
            encode_message_location(message.location());
        let receiver =
            encode_phone_location(receiver_location, "inbound receiver location capture time")?;
        transaction.execute(
            "INSERT INTO inbound_messages(\n\
                 message_id, sequence, local_destination, source, timestamp_unix_ms, title, content,\n\
                 ingress_interface, ingress_rssi, ingress_snr, location_latitude_e6,\n\
                 location_longitude_e6, location_altitude_cm, location_speed_cm_per_second,\n\
                 location_bearing_centidegrees, location_accuracy_cm,\n\
                 location_updated_at_unix_seconds, receiver_location_latitude_e6,\n\
                 receiver_location_longitude_e6, receiver_location_horizontal_accuracy_mm,\n\
                 receiver_location_altitude_mm, receiver_location_vertical_accuracy_mm,\n\
                 receiver_location_captured_at_unix_ms, receiver_location_authorization,\n\
                 receiver_location_source, receiver_location_mocked\n\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,\n\
                       ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
            params![
                message.message_id().as_bytes().as_slice(),
                sqlite_integer(sequence, "inbound sequence")?,
                message.local_destination().as_bytes().as_slice(),
                message.source().as_bytes().as_slice(),
                sqlite_integer(message.timestamp().get(), "inbound timestamp")?,
                message.title(),
                message.content(),
                ingress_interface,
                ingress_rssi,
                ingress_snr,
                latitude,
                longitude,
                altitude,
                speed,
                bearing,
                accuracy,
                updated_at,
                receiver.latitude_e6,
                receiver.longitude_e6,
                receiver.horizontal_accuracy_mm,
                receiver.altitude_mm,
                receiver.vertical_accuracy_mm,
                receiver.captured_at_unix_ms,
                receiver.authorization,
                receiver.source,
                receiver.mocked,
            ],
        )?;
        record_message_activity(
            &transaction,
            PendingMessageActivity {
                observed_at_unix_ms: observed_unix_ms(),
                timeline_sequence: TimelineSequence::new(sequence)
                    .ok_or(ChatStoreError::IdentifierExhausted)?,
                peer: message.source(),
                direction: TimelineDirection::Inbound,
                outbox_id: None,
                attempt_number: None,
                kind: MessageActivityKind::InboundImported {
                    message_id: message.message_id(),
                },
            },
        )?;
        transaction.commit()?;
        Ok(InboundCommitOutcome::Inserted)
    }

    fn commit_outbound_with_location(
        &mut self,
        material: OutboxMaterial,
        location: AttemptLocationStamp,
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
        let (latitude, longitude, altitude, speed, bearing, accuracy, updated_at) =
            encode_message_location(material.location());
        transaction.execute(
            "INSERT INTO outbox(\n\
                 id, sequence, destination, timestamp_unix_ms, idempotency_key, title, content,\n\
                 submission_id, message_id, status_kind, failure_kind, packet_len, packet_sha256,\n\
                 current_attempt, location_latitude_e6, location_longitude_e6,\n\
                 location_altitude_cm, location_speed_cm_per_second,\n\
                 location_bearing_centidegrees, location_accuracy_cm,\n\
                 location_updated_at_unix_seconds\n\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 0, NULL, NULL, NULL, 1,\n\
                       ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                sqlite_integer(id, "outbox id")?,
                sqlite_integer(sequence, "outbox sequence")?,
                material.destination().as_bytes().as_slice(),
                sqlite_integer(material.timestamp().get(), "outbox timestamp")?,
                material.idempotency_key().as_bytes().as_slice(),
                material.title(),
                material.content(),
                latitude,
                longitude,
                altitude,
                speed,
                bearing,
                accuracy,
                updated_at,
            ],
        )?;
        let outbox_id = OutboxId::new(id).ok_or(ChatStoreError::IdentifierExhausted)?;
        record_message_activity(
            &transaction,
            PendingMessageActivity {
                observed_at_unix_ms: observed_unix_ms(),
                timeline_sequence: TimelineSequence::new(sequence)
                    .ok_or(ChatStoreError::IdentifierExhausted)?,
                peer: material.destination(),
                direction: TimelineDirection::Outbound,
                outbox_id: Some(outbox_id),
                attempt_number: Some(MessageAttemptNumber::first()),
                kind: MessageActivityKind::OutboundQueued { location },
            },
        )?;
        transaction.commit()?;
        Ok(OutboxCommitOutcome::Inserted(outbox_id))
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
        record_message_activity(
            &transaction,
            PendingMessageActivity {
                observed_at_unix_ms: observed_unix_ms(),
                timeline_sequence: record.sequence(),
                peer: record.material().destination(),
                direction: TimelineDirection::Outbound,
                outbox_id: Some(outbox_id),
                attempt_number: Some(record.current_attempt()),
                kind: MessageActivityKind::OutboundAccepted { acceptance },
            },
        )?;
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
            record_message_activity(
                &transaction,
                PendingMessageActivity {
                    observed_at_unix_ms: observed_unix_ms(),
                    timeline_sequence: record.sequence(),
                    peer: record.material().destination(),
                    direction: TimelineDirection::Outbound,
                    outbox_id: Some(record.id()),
                    attempt_number: Some(record.current_attempt()),
                    kind: MessageActivityKind::OutboundStatus { state },
                },
            )?;
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
            let sql = format!("SELECT {INBOUND_COLUMNS} FROM inbound_messages WHERE source = ?1");
            let mut statement = self.connection.prepare(&sql)?;
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

    fn message_activity(
        &self,
        request: MessageActivityPageRequest,
    ) -> Result<MessageActivityPage, Self::Error> {
        let before = request
            .before()
            .map(|id| sqlite_integer(id.get(), "message activity before cursor"))
            .transpose()?;
        let timeline_sequence = match request.scope() {
            MessageActivityScope::All => None,
            MessageActivityScope::Timeline(sequence) => Some(sqlite_integer(
                sequence.get(),
                "message activity timeline sequence",
            )?),
        };
        let query_limit = request
            .limit()
            .checked_add(1)
            .ok_or(ChatStoreError::IdentifierExhausted)?;
        let query_limit = i64::try_from(query_limit)
            .map_err(|_| SqliteStoreError::ValueOutOfRange("message activity page limit"))?;
        let sql = format!(
            "SELECT {ACTIVITY_COLUMNS} FROM message_activity AS activity\n\
             LEFT JOIN inbound_messages AS inbound\n\
                 ON activity.direction = 0 AND inbound.sequence = activity.timeline_sequence\n\
             WHERE (?1 IS NULL OR activity.id < ?1)\n\
                 AND (?2 IS NULL OR activity.timeline_sequence = ?2)\n\
             ORDER BY activity.id DESC LIMIT ?3"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let mut rows = statement.query(params![before, timeline_sequence, query_limit])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            events.push(decode_activity(raw_activity(row)?)?);
        }
        let has_more = events.len() > request.limit();
        events.truncate(request.limit());
        let next_before = has_more.then(|| {
            events
                .last()
                .expect("a non-empty bounded page has a last event")
                .id()
        });
        let history_incomplete = self.connection.query_row(
            "SELECT history_incomplete FROM message_activity_meta WHERE singleton = 1",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let history_incomplete = match history_incomplete {
            0 => false,
            1 => true,
            _ => {
                return Err(SqliteStoreError::CorruptData(
                    "message activity history_incomplete",
                ));
            }
        };
        Ok(MessageActivityPage {
            events,
            next_before,
            history_incomplete,
        })
    }

    fn import_rf_trace_batch(
        &mut self,
        batch: RfTraceImportBatch,
    ) -> Result<RfTraceImportOutcome, Self::Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_rf_trace_boot(&transaction, batch.boot_id(), batch.profile())?;

        let mut existing_count = 0_usize;
        for observation in batch.observations() {
            if let Some(existing) = load_rf_trace_by_replay_key(
                &transaction,
                batch.boot_id(),
                observation.event_sequence(),
            )? {
                if existing.observation() != *observation {
                    return Err(ChatStoreError::RfTraceEventConflict {
                        boot_id: batch.boot_id(),
                        event_sequence: observation.event_sequence(),
                    }
                    .into());
                }
                existing_count += 1;
            }
        }

        let mut token_correlations = BTreeMap::new();
        let mut tokens = std::collections::BTreeSet::new();
        for observation in batch.observations() {
            if let Some(token) = observation.rns_attempt_token() {
                tokens.insert(token);
            }
        }
        for token in &tokens {
            load_rf_token_correlations(&transaction, *token, &mut token_correlations)?;
        }
        for observation in batch.observations() {
            let (Some(token), Some(submission_id)) =
                (observation.rns_attempt_token(), observation.submission_id())
            else {
                continue;
            };
            if let Some(correlation) =
                rf_trace_correlation_for_submission(&transaction, submission_id)?
            {
                seed_rf_token_correlation(&mut token_correlations, token, correlation)?;
            }
        }

        let mut inserted = 0_usize;
        for observation in batch.observations() {
            if load_rf_trace_by_replay_key(
                &transaction,
                batch.boot_id(),
                observation.event_sequence(),
            )?
            .is_some()
            {
                continue;
            }
            let correlation = observation
                .rns_attempt_token()
                .and_then(|token| token_correlations.get(&token).copied());
            insert_rf_trace_event(
                &transaction,
                batch.boot_id(),
                batch.profile(),
                batch.imported_at_unix_ms(),
                *observation,
                correlation,
            )?;
            inserted += 1;
        }

        let mut correlations_added = 0_usize;
        for (token, correlation) in token_correlations {
            correlations_added = correlations_added
                .checked_add(backfill_rf_trace_correlation(
                    &transaction,
                    token,
                    correlation,
                )?)
                .ok_or(ChatStoreError::IdentifierExhausted)?;
        }
        update_rf_trace_boot_history(&transaction, batch.boot_id(), batch.history_incomplete())?;
        transaction.commit()?;
        Ok(RfTraceImportOutcome::new(
            inserted,
            existing_count,
            correlations_added,
        ))
    }

    fn rf_trace(&self, request: RfTracePageRequest) -> Result<RfTracePage, Self::Error> {
        let before = request
            .before()
            .map(|id| sqlite_integer(id.get(), "RF trace before cursor"))
            .transpose()?;
        let timeline_sequence = match request.scope() {
            RfTraceScope::All => None,
            RfTraceScope::Timeline(sequence) => Some(sqlite_integer(
                sequence.get(),
                "RF trace timeline sequence",
            )?),
        };
        let query_limit = i64::try_from(
            request
                .limit()
                .checked_add(1)
                .ok_or(ChatStoreError::IdentifierExhausted)?,
        )
        .map_err(|_| SqliteStoreError::ValueOutOfRange("RF trace page limit"))?;
        let sql = format!(
            "SELECT {RF_TRACE_COLUMNS} FROM rf_trace_events AS e\n\
             JOIN rf_trace_boots AS b ON b.boot_id = e.boot_id\n\
             WHERE (?1 IS NULL OR e.id < ?1)\n\
               AND (?2 IS NULL OR e.timeline_sequence = ?2)\n\
             ORDER BY e.id DESC LIMIT ?3"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let mut rows = statement.query(params![before, timeline_sequence, query_limit])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            events.push(decode_rf_trace(raw_rf_trace(row)?)?);
        }
        let has_more = events.len() > request.limit();
        events.truncate(request.limit());
        let next_before = has_more.then(|| {
            events
                .last()
                .expect("a non-empty bounded RF trace page has a last event")
                .id()
        });
        let history_incomplete = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM rf_trace_boots WHERE history_incomplete = 1)",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(RfTracePage {
            events,
            next_before,
            history_incomplete: bool_from_integer(
                history_incomplete,
                "RF trace history incomplete",
            )?,
        })
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
