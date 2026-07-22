//! Persistence-facing chat domain semantics for an LXMF client.
//!
//! The domain and storage boundary are deliberately independent of the device
//! API, Reticulum, an async runtime, any database API, and any UI. A caller
//! first durably commits exact [`OutboxMaterial`], then submits that material
//! to a device, then records the returned [`AcceptanceIds`]. After a restart,
//! [`ChatStore::reconcile`] tells the caller which exact records must be
//! resubmitted and which accepted submissions only need a status refresh.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod memory;
mod model;
#[cfg(feature = "sqlite")]
mod sqlite;
mod store;

pub use memory::{MEMORY_IMAGE_SCHEMA_VERSION, MemoryChatStore, MemoryImage};
pub use model::{
    AcceptanceIds, Contact, DestinationHash, EncodedPacketSha256, IdempotencyKey, InboundMessage,
    InboundRecord, InvalidEncodedPacketLength, InvalidSubmissionId, InvalidTimestamp,
    MAX_UNIX_TIMESTAMP_MILLIS, MessageId, OutboxId, OutboxMaterial, OutboxRecord, OutboxStatus,
    PacketEvidence, ReconcileWork, SubmissionFailure, SubmissionId, SubmissionState,
    TimelineDirection, TimelineEntry, TimelineSequence, UnixTimestampMillis,
};
#[cfg(feature = "sqlite")]
pub use sqlite::{SQLITE_SCHEMA_VERSION, SqliteChatStore, SqliteStoreError};
pub use store::{
    AcceptanceOutcome, ChatStore, ChatStoreError, ContactUpsertOutcome, ImageError,
    InboundCommitOutcome, OutboxCommitOutcome, StatusProjectionOutcome,
};
