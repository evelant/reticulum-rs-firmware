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
    AcceptanceIds, AttemptLocationStamp, Contact, ConversationPeer, DEVICE_ID_LENGTH,
    DestinationHash, DeviceBinding, EncodedPacketSha256, IdempotencyKey, InboundMessage,
    InboundRecord, InvalidEncodedPacketLength, InvalidMessageActivityPageLimit,
    InvalidRfTraceImportBatch, InvalidRfTracePageLimit, InvalidSubmissionId, InvalidTimestamp,
    MAX_LATITUDE_E6, MAX_LONGITUDE_E6, MAX_MESSAGE_ACTIVITY_EVENTS, MAX_MESSAGE_ACTIVITY_PAGE_SIZE,
    MAX_RF_TRACE_PAGE_SIZE, MAX_UNIX_TIMESTAMP_MILLIS, MIN_LATITUDE_E6, MIN_LONGITUDE_E6,
    MessageActivityEvent, MessageActivityId, MessageActivityKind, MessageActivityPage,
    MessageActivityPageRequest, MessageActivityScope, MessageAttemptNumber, MessageId,
    MessageIngressObservation, MessageInterfaceId, MessageLocation, MessageSignalObservation,
    OutboxId, OutboxMaterial, OutboxRecord, OutboxStatus, PacketEvidence,
    PhoneLocationAuthorization, PhoneLocationSample, PhoneLocationSource,
    PhoneLocationUnavailableReason, ReconcileWork, RfTraceAttemptObservation,
    RfTraceAttemptOutcome, RfTraceBootId, RfTraceEvent, RfTraceEventId, RfTraceEventSequence,
    RfTraceIdentityHash, RfTraceImportBatch, RfTraceInboundProofObservation,
    RfTraceInboundProofStage, RfTraceInterfaceId, RfTraceMessageCorrelation, RfTraceObservation,
    RfTraceObservationKind, RfTracePage, RfTracePageRequest, RfTraceProofIngress,
    RfTraceRadioProfile, RfTraceRouteObservation, RfTraceRouteResolution, RfTraceRxObservation,
    RfTraceScope, RfTraceTxObservation, RfTraceTxOutcome, RnsAttemptToken, SubmissionFailure,
    SubmissionId, SubmissionState, TimelineDirection, TimelineEntry, TimelineSequence,
    UnixTimestampMillis,
};
#[cfg(feature = "sqlite")]
pub use sqlite::{SQLITE_SCHEMA_VERSION, SqliteChatStore, SqliteStoreError};
pub use store::{
    AcceptanceOutcome, ChatStore, ChatStoreError, ContactUpsertOutcome, DeviceBindingOutcome,
    ImageError, InboundCommitOutcome, OutboxCommitOutcome, OutboxRetryOutcome,
    RfTraceImportOutcome, StatusProjectionOutcome,
};
