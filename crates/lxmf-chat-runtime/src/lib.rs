//! Transport-neutral durable LXMF chat actor.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use reticulum_device_api::{
    ApiErrorCode, MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES,
};
use reticulum_device_client::ClientError;
use reticulum_lxmf_chat_app::{
    ChatEngine, DeviceSessionError, EngineError, InboxStep, LxmfSession, ReconcileStep,
};
use reticulum_lxmf_chat_core::{
    ChatStore, Contact, ContactUpsertOutcome, DestinationHash, DeviceBinding, IdempotencyKey,
    InboundCommitOutcome, OutboxCommitOutcome, OutboxId, OutboxMaterial, OutboxStatus,
    SqliteChatStore, SqliteStoreError, SubmissionFailure, SubmissionState,
    TimelineDirection as CoreTimelineDirection, TimelineEntry, UnixTimestampMillis,
};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::{oneshot, watch};
use ts_rs::TS;

const COMMAND_CAPACITY: usize = 32;
const COMMAND_WAIT: Duration = Duration::from_millis(50);

/// Largest UTF-8 contact display name accepted by every client adapter.
pub const MAX_CONTACT_NAME_BYTES: usize = 256;

/// Largest integer JSON clients may represent without precision loss.
pub const MAX_JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// A JSON number constrained to JavaScript's lossless integer range.
#[derive(TS)]
#[ts(type = "number")]
pub struct JsonSafeInteger;

/// Serialize a `u64` only when JavaScript can represent it exactly.
pub fn serialize_json_safe_u64<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value > MAX_JSON_SAFE_INTEGER {
        return Err(serde::ser::Error::custom(
            "integer exceeds the JSON safe-integer contract",
        ));
    }
    serializer.serialize_u64(*value)
}

/// Deserialize a `u64` only when JavaScript can represent it exactly.
pub fn deserialize_json_safe_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    if value > MAX_JSON_SAFE_INTEGER {
        return Err(D::Error::custom(
            "integer exceeds the JSON safe-integer contract",
        ));
    }
    Ok(value)
}

fn serialize_optional_json_safe_u64<S>(
    value: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => serialize_json_safe_u64(value, serializer),
        None => serializer.serialize_none(),
    }
}

fn serialize_json_safe_usize<S>(value: &usize, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let value = u64::try_from(*value)
        .map_err(|_| serde::ser::Error::custom("integer exceeds the JSON u64 range"))?;
    serialize_json_safe_u64(&value, serializer)
}

/// Invalid app-facing chat request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientRequestError {
    /// A contact name exceeds the shared UTF-8 byte limit.
    ContactNameTooLong,
    /// A destination is not an exact 16-byte hexadecimal hash.
    InvalidDestination,
    /// An LXMF title exceeds the device API's basic-message limit.
    TitleTooLong,
    /// LXMF content exceeds the device API's basic-message limit.
    ContentTooLong,
    /// A timestamp is outside the exact LXMF/JavaScript range.
    InvalidTimestamp,
    /// An idempotency key is not an exact 16-byte hexadecimal value.
    InvalidIdempotencyKey,
}

impl fmt::Display for ClientRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ContactNameTooLong => "contact name is too long",
            Self::InvalidDestination => {
                "destination must contain exactly 32 hexadecimal characters"
            }
            Self::TitleTooLong => "message title exceeds device limit",
            Self::ContentTooLong => "message content exceeds device limit",
            Self::InvalidTimestamp => "timestamp is outside the LXMF client range",
            Self::InvalidIdempotencyKey => {
                "idempotency key must contain exactly 32 hexadecimal characters"
            }
        })
    }
}

impl std::error::Error for ClientRequestError {}

/// Parse one complete lowercase-or-uppercase hexadecimal LXMF destination.
pub fn parse_destination(value: &str) -> Result<DestinationHash, ClientRequestError> {
    parse_hex::<16>(value)
        .map(DestinationHash::new)
        .ok_or(ClientRequestError::InvalidDestination)
}

fn parse_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    let mut bytes = [0_u8; N];
    hex::decode_to_slice(value, &mut bytes).ok()?;
    Some(bytes)
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
#[allow(missing_docs)]
pub struct ContactRequest {
    name: String,
}

impl ContactRequest {
    /// Construct one contact request.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// Validate this request and combine it with its path destination.
    pub fn into_contact(self, destination: &str) -> Result<Contact, ClientRequestError> {
        if self.name.len() > MAX_CONTACT_NAME_BYTES {
            return Err(ClientRequestError::ContactNameTooLong);
        }
        Ok(Contact::new(parse_destination(destination)?, self.name))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum MutationOutcome {
    Inserted,
    Updated,
    Unchanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct MutationResponse {
    outcome: MutationOutcome,
}

impl MutationResponse {
    /// Durable mutation result.
    pub const fn outcome(self) -> MutationOutcome {
        self.outcome
    }
}

impl From<ContactUpsertOutcome> for MutationResponse {
    fn from(outcome: ContactUpsertOutcome) -> Self {
        Self {
            outcome: match outcome {
                ContactUpsertOutcome::Inserted => MutationOutcome::Inserted,
                ContactUpsertOutcome::Updated => MutationOutcome::Updated,
                ContactUpsertOutcome::Unchanged => MutationOutcome::Unchanged,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
#[allow(missing_docs)]
pub struct SendRequest {
    destination: String,
    #[serde(deserialize_with = "deserialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    timestamp_ms: u64,
    idempotency_key: String,
    title: String,
    content: String,
}

impl SendRequest {
    /// Validate and project this request into exact retry-stable outbox data.
    pub fn into_material(self) -> Result<OutboxMaterial, ClientRequestError> {
        if self.title.len() > MAX_LXMF_BASIC_TITLE_BYTES {
            return Err(ClientRequestError::TitleTooLong);
        }
        if self.content.len() > MAX_LXMF_BASIC_CONTENT_BYTES {
            return Err(ClientRequestError::ContentTooLong);
        }
        let destination = parse_destination(&self.destination)?;
        let timestamp = UnixTimestampMillis::new(self.timestamp_ms)
            .map_err(|_| ClientRequestError::InvalidTimestamp)?;
        let idempotency_key = parse_hex::<16>(&self.idempotency_key)
            .map(IdempotencyKey::new)
            .ok_or(ClientRequestError::InvalidIdempotencyKey)?;
        Ok(OutboxMaterial::new(
            destination,
            timestamp,
            idempotency_key,
            self.title.into_bytes(),
            self.content.into_bytes(),
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum SendOutcome {
    Inserted,
    Existing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct SendResponse {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    outbox_id: u64,
    outcome: SendOutcome,
}

impl SendResponse {
    /// Durable local outbox identifier.
    pub const fn outbox_id(self) -> u64 {
        self.outbox_id
    }

    /// Whether the row was inserted or already existed.
    pub const fn outcome(self) -> SendOutcome {
        self.outcome
    }
}

impl From<OutboxCommitOutcome> for SendResponse {
    fn from(outcome: OutboxCommitOutcome) -> Self {
        match outcome {
            OutboxCommitOutcome::Inserted(id) => Self {
                outbox_id: id.get(),
                outcome: SendOutcome::Inserted,
            },
            OutboxCommitOutcome::Existing(id) => Self {
                outbox_id: id.get(),
                outcome: SendOutcome::Existing,
            },
        }
    }
}

type BoxedSession = Box<dyn LxmfSession<Error = DeviceSessionError> + Send>;

/// Physical or local-network bearer used for one authenticated session.
///
/// The host service implements [`Self::UsbSerial`] and
/// [`Self::BluetoothLowEnergy`]. The remaining variants deliberately reserve
/// the runtime vocabulary for USB OTG, Wi-Fi, and later adapters without
/// implying that they are already available.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionTransport {
    /// Host-side USB serial/JTAG connection.
    UsbSerial,
    /// Direct USB OTG connection owned by a native client.
    UsbOtg,
    /// Bluetooth Low Energy connection owned by a native client.
    BluetoothLowEnergy,
    /// Local Wi-Fi connection owned by a native or web client.
    Wifi,
    /// A future bearer identified by a stable implementation label.
    Other(String),
}

/// Transport-neutral labels published for an authenticated connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionMetadata {
    transport: ConnectionTransport,
    endpoint: String,
    device_label: String,
}

impl ConnectionMetadata {
    /// Describe one connection.
    pub fn new(
        transport: ConnectionTransport,
        endpoint: impl Into<String>,
        device_label: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            endpoint: endpoint.into(),
            device_label: device_label.into(),
        }
    }

    /// Describe the currently implemented host USB serial bearer.
    pub fn usb_serial(endpoint: impl Into<String>, usb_serial: impl Into<String>) -> Self {
        Self::new(ConnectionTransport::UsbSerial, endpoint, usb_serial)
    }

    /// Bearer used by the connection.
    pub const fn transport(&self) -> &ConnectionTransport {
        &self.transport
    }

    /// Bearer-specific endpoint label.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Stable user-visible device label.
    pub fn device_label(&self) -> &str {
        &self.device_label
    }
}

/// One authenticated device session returned by a transport connector.
///
/// The optional lease is retained for exactly as long as the authenticated
/// session is active.
pub struct ConnectedSession {
    session: BoxedSession,
    metadata: ConnectionMetadata,
    connection_lease: Option<Box<dyn Send>>,
}

impl ConnectedSession {
    /// Wrap an authenticated LXMF session and its connection metadata.
    pub fn new<S>(session: S, metadata: ConnectionMetadata) -> Self
    where
        S: LxmfSession<Error = DeviceSessionError> + Send + 'static,
    {
        Self {
            session: Box::new(session),
            metadata,
            connection_lease: None,
        }
    }

    /// Retain an opaque transport ownership guard with this session.
    pub fn with_connection_lease<L>(mut self, lease: L) -> Self
    where
        L: Send + 'static,
    {
        self.connection_lease = Some(Box::new(lease));
        self
    }
}

/// Classified failure to establish an authenticated transport session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectFailure {
    /// A transient condition should use bounded exponential retry.
    Retryable(String),
    /// The selected bearer has no connector implementation in this build.
    Unavailable {
        /// Bearer that cannot currently be opened.
        transport: ConnectionTransport,
        /// Actionable implementation or platform limitation.
        reason: String,
    },
    /// A local invariant or configuration error needs operator action.
    Permanent(String),
}

impl ConnectFailure {
    /// Construct a transient connection failure.
    pub fn retryable(reason: impl Into<String>) -> Self {
        Self::Retryable(reason.into())
    }

    /// Construct a build/platform availability failure.
    pub fn unavailable(transport: ConnectionTransport, reason: impl Into<String>) -> Self {
        Self::Unavailable {
            transport,
            reason: reason.into(),
        }
    }

    /// Construct a permanent connection failure.
    pub fn permanent(reason: impl Into<String>) -> Self {
        Self::Permanent(reason.into())
    }
}

impl fmt::Display for ConnectFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retryable(reason) | Self::Permanent(reason) => formatter.write_str(reason),
            Self::Unavailable { reason, .. } => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for ConnectFailure {}

/// Synchronous factory for authenticated LXMF sessions.
pub trait Connector: Send + 'static {
    /// Connect and authenticate with an explicit retryability classification.
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure>;
}

/// Timing and path policy for one appliance actor.
#[derive(Clone, Debug)]
pub struct ApplianceConfig {
    database: PathBuf,
    reconnect_initial: Duration,
    reconnect_maximum: Duration,
    operation_gap: Duration,
    inbox_poll_interval: Duration,
    status_poll_interval: Duration,
    capacity_backoff: Duration,
}

impl ApplianceConfig {
    /// Construct the default developer-alpha policy.
    pub fn new(database: PathBuf) -> Self {
        Self {
            database,
            reconnect_initial: Duration::from_secs(1),
            reconnect_maximum: Duration::from_secs(30),
            operation_gap: Duration::from_millis(100),
            inbox_poll_interval: Duration::from_secs(2),
            status_poll_interval: Duration::from_secs(1),
            capacity_backoff: Duration::from_secs(30),
        }
    }

    /// Chat SQLite path owned exclusively by the service actor.
    pub fn database(&self) -> &PathBuf {
        &self.database
    }
}

/// Public connection lifecycle exposed to the UI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    /// Actor has opened the database but has not attempted its connector.
    Starting,
    /// No authenticated bearer is currently usable.
    Disconnected,
    /// The selected bearer is being opened and authenticated.
    Connecting,
    /// This build has no implementation for the selected bearer.
    Unavailable {
        /// Bearer reserved for a future connector implementation.
        transport: ConnectionTransport,
    },
    /// The registered board is authenticated and background work may run.
    Ready {
        /// Bearer used by this authenticated session.
        transport: ConnectionTransport,
        /// Bearer-specific endpoint label.
        endpoint: String,
        /// Stable user-visible device label.
        device_label: String,
    },
    /// A retryable failure is waiting for bounded exponential backoff.
    Backoff,
    /// A permanent local/device invariant needs operator action.
    Faulted,
    /// Actor has accepted shutdown.
    Stopped,
}

impl ConnectionState {
    /// Selected unavailable bearer or active ready bearer.
    pub const fn transport(&self) -> Option<&ConnectionTransport> {
        match self {
            Self::Unavailable { transport } | Self::Ready { transport, .. } => Some(transport),
            Self::Starting
            | Self::Disconnected
            | Self::Connecting
            | Self::Backoff
            | Self::Faulted
            | Self::Stopped => None,
        }
    }

    /// Bearer-specific endpoint label, when the actor is ready.
    pub fn endpoint(&self) -> Option<&str> {
        match self {
            Self::Ready { endpoint, .. } => Some(endpoint),
            Self::Starting
            | Self::Disconnected
            | Self::Connecting
            | Self::Unavailable { .. }
            | Self::Backoff
            | Self::Faulted
            | Self::Stopped => None,
        }
    }

    /// Stable user-visible device label, when the actor is ready.
    pub fn device_label(&self) -> Option<&str> {
        match self {
            Self::Ready { device_label, .. } => Some(device_label),
            Self::Starting
            | Self::Disconnected
            | Self::Connecting
            | Self::Unavailable { .. }
            | Self::Backoff
            | Self::Faulted
            | Self::Stopped => None,
        }
    }
}

/// JSON-safe authenticated device identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct DeviceView {
    device_id: String,
    primary_destination: String,
    lxmf_delivery_destination: String,
}

impl DeviceView {
    /// Stable device identity encoded as lowercase hexadecimal.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Primary Reticulum destination encoded as lowercase hexadecimal.
    pub fn primary_destination(&self) -> &str {
        &self.primary_destination
    }

    /// LXMF delivery destination encoded as lowercase hexadecimal.
    pub fn lxmf_delivery_destination(&self) -> &str {
        &self.lxmf_delivery_destination
    }
}

impl From<DeviceBinding> for DeviceView {
    fn from(binding: DeviceBinding) -> Self {
        Self {
            device_id: hex::encode(binding.device_id()),
            primary_destination: hex::encode(binding.primary_destination().as_bytes()),
            lxmf_delivery_destination: hex::encode(binding.lxmf_delivery_destination().as_bytes()),
        }
    }
}

/// Immutable authoritative appliance state read without touching its actor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ApplianceSnapshot {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    revision: u64,
    connection: ConnectionState,
    device: Option<DeviceView>,
    #[serde(serialize_with = "serialize_json_safe_usize")]
    #[ts(as = "JsonSafeInteger")]
    pending_outbox: usize,
    #[serde(serialize_with = "serialize_json_safe_usize")]
    #[ts(as = "JsonSafeInteger")]
    contact_count: usize,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    imported_this_run: u64,
    last_error: Option<String>,
}

impl ApplianceSnapshot {
    fn starting() -> Self {
        Self {
            revision: 1,
            connection: ConnectionState::Starting,
            device: None,
            pending_outbox: 0,
            contact_count: 0,
            imported_this_run: 0,
            last_error: None,
        }
    }

    /// Monotonically increasing in-process state revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Current connector/session lifecycle.
    pub const fn connection(&self) -> &ConnectionState {
        &self.connection
    }

    /// Authenticated device identity, when connected at least once.
    pub const fn device(&self) -> Option<&DeviceView> {
        self.device.as_ref()
    }

    /// Number of locally durable outbox rows still needing device work.
    pub const fn pending_outbox(&self) -> usize {
        self.pending_outbox
    }

    /// Number of locally stored contacts.
    pub const fn contact_count(&self) -> usize {
        self.contact_count
    }

    /// Inbound messages imported since this actor started.
    pub const fn imported_this_run(&self) -> u64 {
        self.imported_this_run
    }

    /// Latest actionable background failure, when present.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct ContactView {
    destination: String,
    name: String,
}

impl ContactView {
    /// Contact destination encoded as lowercase hexadecimal.
    pub fn destination(&self) -> &str {
        &self.destination
    }

    /// User-selected contact display name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl From<Contact> for ContactView {
    fn from(contact: Contact) -> Self {
        Self {
            destination: hex::encode(contact.destination().as_bytes()),
            name: contact.display_name().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum BytesEncoding {
    Utf8,
    Hex,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct BytesView {
    encoding: BytesEncoding,
    value: String,
}

impl BytesView {
    fn new(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(text) => Self {
                encoding: BytesEncoding::Utf8,
                value: text.to_owned(),
            },
            Err(_) => Self {
                encoding: BytesEncoding::Hex,
                value: hex::encode(bytes),
            },
        }
    }

    /// Encoding used for the projected value.
    pub const fn encoding(&self) -> BytesEncoding {
        self.encoding
    }

    /// UTF-8 text or lowercase hexadecimal, according to [`Self::encoding`].
    pub fn value(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum TimelineDirection {
    Inbound,
    Outbound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum TimelineStatus {
    Committed,
    Accepted,
    Queued,
    Preparing,
    AwaitingDelivery,
    Delivered,
    FailedNoPath,
    FailedDeliveryTimeout,
    FailedDownstreamRejection,
    FailedInternal,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct TimelineView {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    sequence: u64,
    direction: TimelineDirection,
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    timestamp_ms: u64,
    message_id: Option<String>,
    #[serde(serialize_with = "serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    outbox_id: Option<u64>,
    status: Option<TimelineStatus>,
    title: BytesView,
    content: BytesView,
}

impl TimelineView {
    /// Stable local timeline ordering sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Whether the message is inbound or outbound.
    pub const fn direction(&self) -> TimelineDirection {
        self.direction
    }

    /// LXMF timestamp in Unix milliseconds.
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// LXMF message identifier encoded as lowercase hexadecimal.
    pub fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }

    /// Durable local outbox identifier for outbound rows.
    pub const fn outbox_id(&self) -> Option<u64> {
        self.outbox_id
    }

    /// Current outbound status, or `None` for inbound rows.
    pub const fn status(&self) -> Option<TimelineStatus> {
        self.status
    }

    /// Projected message title.
    pub const fn title(&self) -> &BytesView {
        &self.title
    }

    /// Projected message content.
    pub const fn content(&self) -> &BytesView {
        &self.content
    }
}

impl From<TimelineEntry> for TimelineView {
    fn from(entry: TimelineEntry) -> Self {
        Self {
            sequence: entry.sequence().get(),
            direction: match entry.direction() {
                CoreTimelineDirection::Inbound => TimelineDirection::Inbound,
                CoreTimelineDirection::Outbound => TimelineDirection::Outbound,
            },
            timestamp_ms: entry.timestamp().get(),
            message_id: entry
                .message_id()
                .map(|message_id| hex::encode(message_id.as_bytes())),
            outbox_id: entry.outbox_id().map(OutboxId::get),
            status: entry.outbox_status().map(status_name),
            title: BytesView::new(entry.title()),
            content: BytesView::new(entry.content()),
        }
    }
}

fn status_name(status: OutboxStatus) -> TimelineStatus {
    match status {
        OutboxStatus::Committed => TimelineStatus::Committed,
        OutboxStatus::Accepted => TimelineStatus::Accepted,
        OutboxStatus::Device(SubmissionState::Queued) => TimelineStatus::Queued,
        OutboxStatus::Device(SubmissionState::Preparing) => TimelineStatus::Preparing,
        OutboxStatus::Device(SubmissionState::AwaitingDelivery(_)) => {
            TimelineStatus::AwaitingDelivery
        }
        OutboxStatus::Device(SubmissionState::Delivered(_)) => TimelineStatus::Delivered,
        OutboxStatus::Device(SubmissionState::Failed(failure)) => match failure {
            SubmissionFailure::NoPath => TimelineStatus::FailedNoPath,
            SubmissionFailure::DeliveryTimeout => TimelineStatus::FailedDeliveryTimeout,
            SubmissionFailure::DownstreamRejection => TimelineStatus::FailedDownstreamRejection,
            SubmissionFailure::Internal => TimelineStatus::FailedInternal,
        },
        OutboxStatus::Device(SubmissionState::Cancelled) => TimelineStatus::Cancelled,
    }
}

enum Command {
    Contacts(oneshot::Sender<Result<Vec<ContactView>, String>>),
    Timeline {
        peer: DestinationHash,
        reply: oneshot::Sender<Result<Vec<TimelineView>, String>>,
    },
    UpsertContact {
        contact: Contact,
        reply: oneshot::Sender<Result<ContactUpsertOutcome, String>>,
    },
    EnqueueSend {
        material: OutboxMaterial,
        reply: oneshot::Sender<Result<OutboxCommitOutcome, String>>,
    },
    SyncNow(oneshot::Sender<Result<(), String>>),
    EnsureConnected(oneshot::Sender<Result<(), String>>),
    Reconnect(oneshot::Sender<Result<(), String>>),
    Shutdown,
}

/// Bounded service-handle failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceError {
    /// Actor command queue is currently full.
    Busy,
    /// Actor has stopped or dropped the response.
    Stopped,
    /// A durable local operation failed.
    Operation(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("appliance command queue is busy"),
            Self::Stopped => formatter.write_str("appliance service has stopped"),
            Self::Operation(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for ServiceError {}

/// Cloneable bounded command and immutable-snapshot handle.
#[derive(Clone)]
pub struct ApplianceHandle {
    commands: mpsc::SyncSender<Command>,
    snapshot: Arc<RwLock<Arc<ApplianceSnapshot>>>,
    revisions: watch::Receiver<u64>,
    terminated: watch::Receiver<bool>,
}

impl ApplianceHandle {
    /// Read the current immutable state without contacting the actor.
    pub fn snapshot(&self) -> Arc<ApplianceSnapshot> {
        self.snapshot
            .read()
            .expect("snapshot lock must not be poisoned")
            .clone()
    }

    /// Subscribe to immutable snapshot revision changes.
    pub fn subscribe_revisions(&self) -> watch::Receiver<u64> {
        self.revisions.clone()
    }

    /// Load the current contact projection.
    pub async fn contacts(&self) -> Result<Vec<ContactView>, ServiceError> {
        self.request(Command::Contacts).await
    }

    /// Load the ordered timeline for one peer.
    pub async fn timeline(&self, peer: DestinationHash) -> Result<Vec<TimelineView>, ServiceError> {
        self.request(|reply| Command::Timeline { peer, reply })
            .await
    }

    /// Insert or update one durable contact.
    pub async fn upsert_contact(
        &self,
        contact: Contact,
    ) -> Result<ContactUpsertOutcome, ServiceError> {
        self.request(|reply| Command::UpsertContact { contact, reply })
            .await
    }

    /// Durably enqueue one outbound LXMF message.
    pub async fn enqueue_send(
        &self,
        material: OutboxMaterial,
    ) -> Result<OutboxCommitOutcome, ServiceError> {
        self.request(|reply| Command::EnqueueSend { material, reply })
            .await
    }

    /// Schedule inbox and outbox work immediately.
    pub async fn sync_now(&self) -> Result<(), ServiceError> {
        self.request(Command::SyncNow).await
    }

    /// Schedule the connector immediately if there is no current session.
    ///
    /// Unlike [`Self::reconnect`], this never drops an already established
    /// bearer. Platform-owned transports use it after registering a fresh
    /// physical link, including when the actor raced ahead and already claimed
    /// that link.
    pub async fn ensure_connected(&self) -> Result<(), ServiceError> {
        self.request(Command::EnsureConnected).await
    }

    /// Drop the current bearer and immediately retry the connector.
    pub async fn reconnect(&self) -> Result<(), ServiceError> {
        self.request(Command::Reconnect).await
    }

    /// Ask the actor to stop. This does not block on thread termination.
    pub fn shutdown(&self) -> Result<(), ServiceError> {
        self.commands
            .try_send(Command::Shutdown)
            .map_err(map_try_send)
    }

    /// Stop the actor after any already-queued commands and wait until its
    /// final immutable snapshot confirms that the database owner exited.
    pub async fn shutdown_and_wait(&self) -> Result<(), ServiceError> {
        let mut terminated = self.terminated.clone();
        if *terminated.borrow() {
            return Ok(());
        }
        loop {
            match self.shutdown() {
                Ok(()) => break,
                Err(ServiceError::Busy) => tokio::time::sleep(COMMAND_WAIT).await,
                Err(error) => return Err(error),
            }
        }
        while !*terminated.borrow_and_update() {
            terminated
                .changed()
                .await
                .map_err(|_| ServiceError::Stopped)?;
        }
        Ok(())
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Command,
    ) -> Result<T, ServiceError> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .try_send(command(reply))
            .map_err(map_try_send)?;
        receive
            .await
            .map_err(|_| ServiceError::Stopped)?
            .map_err(ServiceError::Operation)
    }

    /// Construct an inert handle for downstream HTTP adapter tests.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn for_web_test() -> Self {
        let snapshot = ApplianceSnapshot::starting();
        let (commands, _receiver) = mpsc::sync_channel(1);
        let revision = snapshot.revision();
        let (_revision_tx, revisions) = watch::channel(revision);
        let (_termination_tx, terminated) = watch::channel(false);
        Self {
            commands,
            snapshot: Arc::new(RwLock::new(Arc::new(snapshot))),
            revisions,
            terminated,
        }
    }
}

fn map_try_send(error: mpsc::TrySendError<Command>) -> ServiceError {
    match error {
        mpsc::TrySendError::Full(_) => ServiceError::Busy,
        mpsc::TrySendError::Disconnected(_) => ServiceError::Stopped,
    }
}

/// Open the database and start its sole connector/device owner thread.
pub fn start_appliance<C>(
    config: ApplianceConfig,
    connector: C,
) -> Result<ApplianceHandle, ServiceError>
where
    C: Connector,
{
    let store = SqliteChatStore::open(&config.database)
        .map_err(|error| ServiceError::Operation(error.to_string()))?;
    start_with_connector(config, store, connector)
}

fn start_with_connector<C: Connector>(
    config: ApplianceConfig,
    store: SqliteChatStore,
    connector: C,
) -> Result<ApplianceHandle, ServiceError> {
    let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let initial = Arc::new(ApplianceSnapshot::starting());
    let snapshot = Arc::new(RwLock::new(initial.clone()));
    let (revision_tx, revision_rx) = watch::channel(initial.revision());
    let (termination_tx, termination_rx) = watch::channel(false);
    let handle = ApplianceHandle {
        commands,
        snapshot: snapshot.clone(),
        revisions: revision_rx,
        terminated: termination_rx,
    };
    thread::Builder::new()
        .name("lxmf-appliance".to_owned())
        .spawn(move || {
            Actor::new(
                config,
                ChatEngine::new(store),
                connector,
                receiver,
                snapshot,
                revision_tx,
            )
            .run();
            termination_tx.send_replace(true);
        })
        .map_err(|error| {
            ServiceError::Operation(format!("could not start appliance actor: {error}"))
        })?;
    Ok(handle)
}

struct Actor<C> {
    config: ApplianceConfig,
    engine: ChatEngine<SqliteChatStore>,
    connector: C,
    commands: mpsc::Receiver<Command>,
    published: Arc<RwLock<Arc<ApplianceSnapshot>>>,
    revisions: watch::Sender<u64>,
    snapshot: ApplianceSnapshot,
    session: Option<BoxedSession>,
    session_lease: Option<Box<dyn Send>>,
    reconnect_delay: Duration,
    next_connect: Instant,
    next_reconcile: Instant,
    next_inbox: Instant,
    prefer_inbox: bool,
    background_error_work: Option<BackgroundWork>,
    permanent_fault: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BackgroundWork {
    Inbox,
    Reconcile,
}

impl<C: Connector> Actor<C> {
    fn new(
        config: ApplianceConfig,
        engine: ChatEngine<SqliteChatStore>,
        connector: C,
        commands: mpsc::Receiver<Command>,
        published: Arc<RwLock<Arc<ApplianceSnapshot>>>,
        revisions: watch::Sender<u64>,
    ) -> Self {
        let now = Instant::now();
        Self {
            reconnect_delay: config.reconnect_initial,
            config,
            engine,
            connector,
            commands,
            published,
            revisions,
            snapshot: ApplianceSnapshot::starting(),
            session: None,
            session_lease: None,
            next_connect: now,
            next_reconcile: now,
            next_inbox: now,
            prefer_inbox: false,
            background_error_work: None,
            permanent_fault: false,
        }
    }

    fn run(mut self) {
        self.refresh_counts();
        self.publish();
        loop {
            match self.commands.recv_timeout(COMMAND_WAIT) {
                Ok(Command::Shutdown) => break,
                Ok(command) => self.handle_command(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.background_turn();
        }
        self.clear_session();
        self.snapshot.connection = ConnectionState::Stopped;
        self.publish();
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Contacts(reply) => {
                let result = self
                    .engine
                    .contacts()
                    .map(|contacts| contacts.into_iter().map(ContactView::from).collect())
                    .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            Command::Timeline { peer, reply } => {
                let result = self
                    .engine
                    .timeline(peer)
                    .map(|entries| entries.into_iter().map(TimelineView::from).collect())
                    .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            Command::UpsertContact { contact, reply } => {
                let result = self
                    .engine
                    .upsert_contact(contact)
                    .map_err(|error| error.to_string());
                if result.is_ok() {
                    self.refresh_counts();
                    self.publish();
                }
                let _ = reply.send(result);
            }
            Command::EnqueueSend { material, reply } => {
                let result = self
                    .engine
                    .enqueue_send(material)
                    .map_err(|error| error.to_string());
                if result.is_ok() {
                    self.next_reconcile = Instant::now();
                    self.refresh_counts();
                    self.publish();
                }
                let _ = reply.send(result);
            }
            Command::SyncNow(reply) => {
                self.next_inbox = Instant::now();
                self.next_reconcile = Instant::now();
                let _ = reply.send(Ok(()));
            }
            Command::EnsureConnected(reply) => {
                if self.session.is_none() && !self.permanent_fault {
                    self.next_connect = Instant::now();
                }
                let _ = reply.send(Ok(()));
            }
            Command::Reconnect(reply) => {
                self.clear_session();
                self.engine.reset_session_scan();
                self.background_error_work = None;
                self.permanent_fault = false;
                self.next_connect = Instant::now();
                self.snapshot.connection = ConnectionState::Disconnected;
                self.snapshot.last_error = None;
                self.publish();
                let _ = reply.send(Ok(()));
            }
            Command::Shutdown => unreachable!("shutdown is handled by the actor loop"),
        }
    }

    fn background_turn(&mut self) {
        let now = Instant::now();
        if self.session.is_none() {
            if !self.permanent_fault && now >= self.next_connect {
                self.connect();
            }
            return;
        }

        let inbox_due = now >= self.next_inbox;
        let reconcile_due = now >= self.next_reconcile;
        if !inbox_due && !reconcile_due {
            return;
        }
        let run_inbox = inbox_due && (!reconcile_due || self.prefer_inbox);
        self.prefer_inbox = !run_inbox;
        if run_inbox {
            self.inbox_turn();
        } else {
            self.reconcile_turn();
        }
    }

    fn connect(&mut self) {
        self.snapshot.connection = ConnectionState::Connecting;
        self.snapshot.last_error = None;
        self.publish();
        let mut connected = match self.connector.connect() {
            Ok(connected) => connected,
            Err(ConnectFailure::Retryable(error)) => {
                self.retry_connection(error);
                return;
            }
            Err(ConnectFailure::Unavailable { transport, reason }) => {
                self.unavailable_connection(transport, reason);
                return;
            }
            Err(ConnectFailure::Permanent(error)) => {
                self.permanent_fault(error);
                return;
            }
        };
        let binding = match connected.session.binding() {
            Ok(binding) => binding,
            Err(error) => {
                self.retry_connection(error.to_string());
                return;
            }
        };
        let device_label = connected.metadata.device_label().to_owned();
        match self.engine.store_mut().bind_device(binding) {
            Ok(_) => {}
            Err(SqliteStoreError::DeviceBindingMismatch { .. }) => {
                self.permanent_fault(format!(
                    "database/device identity mismatch for device {device_label}"
                ));
                return;
            }
            Err(error) => {
                self.permanent_fault(error.to_string());
                return;
            }
        }
        let ConnectedSession {
            connection_lease,
            metadata,
            session,
        } = connected;
        let ConnectionMetadata {
            transport,
            endpoint,
            device_label,
        } = metadata;
        self.engine.reset_session_scan();
        self.session = Some(session);
        self.session_lease = connection_lease;
        self.reconnect_delay = self.config.reconnect_initial;
        self.next_inbox = Instant::now();
        self.next_reconcile = Instant::now();
        self.snapshot.connection = ConnectionState::Ready {
            transport,
            endpoint,
            device_label,
        };
        self.snapshot.device = Some(binding.into());
        self.background_error_work = None;
        self.snapshot.last_error = None;
        self.refresh_counts();
        self.publish();
    }

    fn reconcile_turn(&mut self) {
        let session = self.session.as_mut().expect("ready actor has a session");
        match self.engine.reconcile_step(session.as_mut()) {
            Ok(ReconcileStep::Idle) => {
                self.next_reconcile = Instant::now() + self.config.status_poll_interval;
                if self.clear_background_error(BackgroundWork::Reconcile) {
                    self.publish();
                }
            }
            Ok(ReconcileStep::Submitted { .. } | ReconcileStep::Refreshed { .. }) => {
                self.next_reconcile = Instant::now() + self.config.operation_gap;
                self.clear_background_error(BackgroundWork::Reconcile);
                self.refresh_counts();
                self.publish();
            }
            Err(error) => self.background_error(BackgroundWork::Reconcile, error),
        }
    }

    fn inbox_turn(&mut self) {
        let session = self.session.as_mut().expect("ready actor has a session");
        match self.engine.inbox_step(session.as_mut()) {
            Ok(InboxStep::EndOfScan) => {
                self.next_inbox = Instant::now() + self.config.inbox_poll_interval;
                if self.clear_background_error(BackgroundWork::Inbox) {
                    self.publish();
                }
            }
            Ok(InboxStep::AlreadyImported { .. }) => {
                self.next_inbox = Instant::now() + self.config.operation_gap;
                if self.clear_background_error(BackgroundWork::Inbox) {
                    self.publish();
                }
            }
            Ok(InboxStep::Imported { outcome, .. }) => {
                self.next_inbox = Instant::now() + self.config.operation_gap;
                if outcome == InboundCommitOutcome::Inserted {
                    self.snapshot.imported_this_run =
                        self.snapshot.imported_this_run.saturating_add(1);
                }
                self.clear_background_error(BackgroundWork::Inbox);
                self.publish();
            }
            Err(error) => self.background_error(BackgroundWork::Inbox, error),
        }
    }

    fn background_error(
        &mut self,
        work: BackgroundWork,
        error: EngineError<SqliteStoreError, DeviceSessionError>,
    ) {
        match error {
            EngineError::Store(error) => self.permanent_fault(error.to_string()),
            EngineError::InboxMessageIdMismatch { .. } => {
                self.permanent_fault("device inbox summary/message mismatch".to_owned());
            }
            EngineError::Session(error) => {
                let usable = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_usable());
                if !usable {
                    self.clear_session();
                    self.engine.reset_session_scan();
                    self.retry_connection(error.to_string());
                    return;
                }
                match &error {
                    DeviceSessionError::Client(ClientError::Api {
                        error: response, ..
                    }) if response.code == ApiErrorCode::CapacityExhausted => {
                        self.background_error_work = Some(work);
                        self.snapshot.last_error = Some(error.to_string());
                        let retry_at = Instant::now() + self.config.capacity_backoff;
                        match work {
                            BackgroundWork::Inbox => self.next_inbox = retry_at,
                            BackgroundWork::Reconcile => self.next_reconcile = retry_at,
                        }
                        self.publish();
                    }
                    DeviceSessionError::Client(ClientError::Api { .. }) => {
                        self.permanent_fault(error.to_string());
                    }
                    _ => self.permanent_fault(error.to_string()),
                }
            }
        }
    }

    fn clear_background_error(&mut self, work: BackgroundWork) -> bool {
        if self.background_error_work != Some(work) {
            return false;
        }
        self.background_error_work = None;
        self.snapshot.last_error = None;
        true
    }

    fn retry_connection(&mut self, error: String) {
        self.clear_session();
        self.engine.reset_session_scan();
        self.background_error_work = None;
        self.snapshot.connection = ConnectionState::Backoff;
        self.snapshot.last_error = Some(error);
        self.next_connect = Instant::now() + self.reconnect_delay;
        self.reconnect_delay = self
            .reconnect_delay
            .saturating_mul(2)
            .min(self.config.reconnect_maximum);
        self.publish();
    }

    fn permanent_fault(&mut self, error: String) {
        self.clear_session();
        self.background_error_work = None;
        self.permanent_fault = true;
        self.snapshot.connection = ConnectionState::Faulted;
        self.snapshot.last_error = Some(error);
        self.publish();
    }

    fn unavailable_connection(&mut self, transport: ConnectionTransport, reason: String) {
        self.clear_session();
        self.engine.reset_session_scan();
        self.background_error_work = None;
        self.permanent_fault = true;
        self.snapshot.connection = ConnectionState::Unavailable { transport };
        self.snapshot.last_error = Some(reason);
        self.publish();
    }

    fn clear_session(&mut self) {
        self.session = None;
        self.session_lease = None;
    }

    fn refresh_counts(&mut self) {
        match self.engine.store().reconcile() {
            Ok(work) => self.snapshot.pending_outbox = work.len(),
            Err(error) => self.snapshot.last_error = Some(error.to_string()),
        }
        match self.engine.contacts() {
            Ok(contacts) => self.snapshot.contact_count = contacts.len(),
            Err(error) => self.snapshot.last_error = Some(error.to_string()),
        }
    }

    fn publish(&mut self) {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
        let value = Arc::new(self.snapshot.clone());
        *self
            .published
            .write()
            .expect("snapshot lock must not be poisoned") = value;
        self.revisions.send_replace(self.snapshot.revision);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use reticulum_lxmf_chat_app::{InboxCursor, InboxSummary};
    use reticulum_lxmf_chat_core::{
        AcceptanceIds, IdempotencyKey, InboundMessage, MessageId, SubmissionId, UnixTimestampMillis,
    };

    use super::*;

    static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    type FakeInbox = Vec<(InboxSummary, InboundMessage)>;
    type ConnectOutcome = Result<(DeviceBinding, FakeInbox), String>;

    struct TestDatabase(PathBuf);

    impl TestDatabase {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "reticulum-appliance-{label}-{}-{nonce}-{sequence}.sqlite3",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            let _ = fs::remove_file(self.0.with_extension("sqlite3-shm"));
            let _ = fs::remove_file(self.0.with_extension("sqlite3-wal"));
        }
    }

    fn destination(tag: u8) -> DestinationHash {
        DestinationHash::new([tag; 16])
    }

    fn binding(tag: u8) -> DeviceBinding {
        DeviceBinding::new(
            [tag; 16],
            destination(tag.wrapping_add(1)),
            destination(tag.wrapping_add(2)),
        )
    }

    fn outbound(tag: u8) -> OutboxMaterial {
        OutboxMaterial::new(
            destination(tag),
            UnixTimestampMillis::new(1_000 + u64::from(tag)).unwrap(),
            IdempotencyKey::new([tag; 16]),
            b"title".to_vec(),
            vec![tag],
        )
    }

    fn inbox(handle: u64, tag: u8) -> (InboxSummary, InboundMessage) {
        let id = MessageId::new([tag; 32]);
        (
            InboxSummary::new(InboxCursor::new(handle).unwrap(), id),
            InboundMessage::new(
                id,
                destination(0xa0),
                destination(tag),
                UnixTimestampMillis::new(2_000 + u64::from(tag)).unwrap(),
                b"inbound".to_vec(),
                vec![tag],
            ),
        )
    }

    #[test]
    fn connection_bearer_is_part_of_the_transport_neutral_projection() {
        let state = ConnectionState::Ready {
            transport: ConnectionTransport::UsbSerial,
            endpoint: "/dev/cu.usbmodem-test".to_owned(),
            device_label: "001122334455".to_owned(),
        };
        assert!(matches!(
            state.transport(),
            Some(ConnectionTransport::UsbSerial)
        ));
        assert_eq!(state.endpoint(), Some("/dev/cu.usbmodem-test"));
        assert_eq!(state.device_label(), Some("001122334455"));
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            serde_json::json!({
                "state": "ready",
                "transport": "usb_serial",
                "endpoint": "/dev/cu.usbmodem-test",
                "device_label": "001122334455",
            })
        );
    }

    #[test]
    fn shared_requests_validate_and_preserve_the_existing_json_contract() {
        let contact: ContactRequest =
            serde_json::from_value(serde_json::json!({ "name": "Field node" })).unwrap();
        let contact = contact.into_contact(&"ab".repeat(16)).unwrap();
        assert_eq!(contact.destination(), destination(0xab));
        assert_eq!(contact.display_name(), "Field node");
        assert_eq!(
            serde_json::to_value(MutationResponse::from(ContactUpsertOutcome::Inserted)).unwrap(),
            serde_json::json!({ "outcome": "inserted" })
        );

        let send: SendRequest = serde_json::from_value(serde_json::json!({
            "destination": "cd".repeat(16),
            "timestamp_ms": 1_234,
            "idempotency_key": "ef".repeat(16),
            "title": "hello",
            "content": "mesh",
        }))
        .unwrap();
        let material = send.into_material().unwrap();
        assert_eq!(material.destination(), destination(0xcd));
        assert_eq!(material.timestamp().get(), 1_234);
        assert_eq!(material.idempotency_key().as_bytes(), &[0xef; 16]);
        assert_eq!(material.title(), b"hello");
        assert_eq!(material.content(), b"mesh");
        assert!(
            serde_json::from_value::<SendRequest>(serde_json::json!({
                "destination": "cd".repeat(16),
                "timestamp_ms": MAX_JSON_SAFE_INTEGER + 1,
                "idempotency_key": "ef".repeat(16),
                "title": "",
                "content": "",
            }))
            .is_err()
        );
    }

    struct FakeSession {
        binding: DeviceBinding,
        inbox: Vec<(InboxSummary, InboundMessage)>,
        submitted: Arc<Mutex<Vec<OutboxMaterial>>>,
    }

    impl LxmfSession for FakeSession {
        type Error = DeviceSessionError;

        fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
            Ok(self.binding)
        }

        fn submit(&mut self, material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
            let mut submitted = self.submitted.lock().unwrap();
            submitted.push(material.clone());
            let id = SubmissionId::new(submitted.len() as u64).unwrap();
            Ok(AcceptanceIds::new(id, MessageId::new([id.get() as u8; 32])))
        }

        fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
            Ok(SubmissionState::Queued)
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
            Ok(self
                .inbox
                .iter()
                .find(|(candidate, _)| *candidate == summary)
                .unwrap()
                .1
                .clone())
        }

        fn is_usable(&self) -> bool {
            true
        }
    }

    struct FakeConnector {
        outcomes: VecDeque<ConnectOutcome>,
        attempts: Arc<AtomicUsize>,
        submitted: Arc<Mutex<Vec<OutboxMaterial>>>,
    }

    struct UnavailableConnector {
        attempts: Arc<AtomicUsize>,
    }

    impl Connector for UnavailableConnector {
        fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            Err(ConnectFailure::unavailable(
                ConnectionTransport::BluetoothLowEnergy,
                "BLE adapter is not implemented",
            ))
        }
    }

    struct CountingLease(Arc<AtomicUsize>);

    impl Drop for CountingLease {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct LeaseConnector {
        connected: bool,
        lease_drops: Arc<AtomicUsize>,
    }

    impl Connector for LeaseConnector {
        fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
            if self.connected {
                return Err(ConnectFailure::retryable("device remains disconnected"));
            }
            self.connected = true;
            Ok(ConnectedSession::new(
                FakeSession {
                    binding: binding(0x71),
                    inbox: Vec::new(),
                    submitted: Arc::new(Mutex::new(Vec::new())),
                },
                ConnectionMetadata::usb_serial("/dev/fake", "001122334455"),
            )
            .with_connection_lease(CountingLease(self.lease_drops.clone())))
        }
    }

    struct DropOrderSession {
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Drop for DropOrderSession {
        fn drop(&mut self) {
            self.order.lock().unwrap().push("session");
        }
    }

    impl LxmfSession for DropOrderSession {
        type Error = DeviceSessionError;

        fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn submit(&mut self, _material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn next_inbox(
            &mut self,
            _after: Option<InboxCursor>,
        ) -> Result<Option<InboxSummary>, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn read_inbox(&mut self, _summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn is_usable(&self) -> bool {
            false
        }
    }

    struct DropOrderLease(Arc<Mutex<Vec<&'static str>>>);

    impl Drop for DropOrderLease {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("lease");
        }
    }

    struct BindingFailureConnector {
        returned_session: bool,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Connector for BindingFailureConnector {
        fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
            if self.returned_session {
                return Err(ConnectFailure::retryable("device remains disconnected"));
            }
            self.returned_session = true;
            Ok(ConnectedSession::new(
                DropOrderSession {
                    order: self.order.clone(),
                },
                ConnectionMetadata::usb_serial("/dev/fake", "001122334455"),
            )
            .with_connection_lease(DropOrderLease(self.order.clone())))
        }
    }

    impl Connector for FakeConnector {
        fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            let (binding, inbox) = self
                .outcomes
                .pop_front()
                .unwrap_or_else(|| Err("no scripted connection remains".to_owned()))
                .map_err(ConnectFailure::retryable)?;
            Ok(ConnectedSession::new(
                FakeSession {
                    binding,
                    inbox,
                    submitted: self.submitted.clone(),
                },
                ConnectionMetadata::usb_serial("/dev/fake", "001122334455"),
            ))
        }
    }

    fn config(database: &TestDatabase) -> ApplianceConfig {
        let mut config = ApplianceConfig::new(database.0.clone());
        config.reconnect_initial = Duration::from_millis(5);
        config.reconnect_maximum = Duration::from_millis(20);
        config.operation_gap = Duration::from_millis(2);
        config.inbox_poll_interval = Duration::from_millis(5);
        config.status_poll_interval = Duration::from_millis(5);
        config
    }

    async fn wait_for(handle: &ApplianceHandle, predicate: impl Fn(&ApplianceSnapshot) -> bool) {
        for _ in 0..200 {
            let snapshot = handle.snapshot();
            if predicate(&snapshot) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "timed out waiting for appliance state: {:?}",
            handle.snapshot()
        );
    }

    #[tokio::test]
    async fn actor_reconnects_binds_imports_and_processes_offline_outbox() {
        let database = TestDatabase::new("actor-flow");
        let expected_binding = binding(0x21);
        let inbound = inbox(1, 0x31);
        let attempts = Arc::new(AtomicUsize::new(0));
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let connector = FakeConnector {
            outcomes: VecDeque::from([
                Err("board absent".to_owned()),
                Ok((expected_binding, vec![inbound.clone()])),
            ]),
            attempts: attempts.clone(),
            submitted: submitted.clone(),
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();

        let committed = handle.enqueue_send(outbound(0x41)).await.unwrap();
        assert!(matches!(committed, OutboxCommitOutcome::Inserted(_)));
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
                && snapshot.imported_this_run == 1
        })
        .await;
        wait_for(&handle, |_| !submitted.lock().unwrap().is_empty()).await;
        assert!(attempts.load(Ordering::Relaxed) >= 2);
        assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x41)]);
        let timeline = handle.timeline(destination(0x31)).await.unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].content.encoding, BytesEncoding::Utf8);
        assert_eq!(timeline[0].content.value, "1");

        handle.shutdown_and_wait().await.unwrap();
        let reopened = SqliteChatStore::open(&database.0).unwrap();
        assert_eq!(reopened.device_binding().unwrap(), Some(expected_binding));
    }

    #[tokio::test]
    async fn database_binding_mismatch_faults_without_retrying_another_board() {
        let database = TestDatabase::new("binding-mismatch");
        let mut store = SqliteChatStore::open(&database.0).unwrap();
        store.bind_device(binding(0x51)).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let connector = FakeConnector {
            outcomes: VecDeque::from([Ok((binding(0x61), Vec::new()))]),
            attempts: attempts.clone(),
            submitted: Arc::new(Mutex::new(Vec::new())),
        };
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            snapshot.connection() == &ConnectionState::Faulted
        })
        .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(handle.snapshot().last_error().unwrap().contains("mismatch"));
        handle.shutdown_and_wait().await.unwrap();
    }

    #[tokio::test]
    async fn unavailable_connector_settles_without_background_retry() {
        let database = TestDatabase::new("unavailable");
        let attempts = Arc::new(AtomicUsize::new(0));
        let connector = UnavailableConnector {
            attempts: attempts.clone(),
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            snapshot.connection()
                == &ConnectionState::Unavailable {
                    transport: ConnectionTransport::BluetoothLowEnergy,
                }
        })
        .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert_eq!(
            handle.snapshot().last_error(),
            Some("BLE adapter is not implemented")
        );
        handle.shutdown_and_wait().await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_drops_the_transport_lease_with_the_session() {
        let database = TestDatabase::new("connection-lease");
        let lease_drops = Arc::new(AtomicUsize::new(0));
        let connector = LeaseConnector {
            connected: false,
            lease_drops: lease_drops.clone(),
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
        })
        .await;
        assert_eq!(lease_drops.load(Ordering::Relaxed), 0);

        handle.reconnect().await.unwrap();
        assert_eq!(lease_drops.load(Ordering::Relaxed), 1);
        handle.shutdown_and_wait().await.unwrap();
        assert_eq!(lease_drops.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn ensure_connected_preserves_an_already_ready_transport_lease() {
        let database = TestDatabase::new("ensure-ready-lease");
        let lease_drops = Arc::new(AtomicUsize::new(0));
        let connector = LeaseConnector {
            connected: false,
            lease_drops: lease_drops.clone(),
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
        })
        .await;

        handle.ensure_connected().await.unwrap();
        assert!(
            matches!(
                handle.snapshot().connection(),
                ConnectionState::Ready { .. }
            ),
            "non-destructive wake preserves the ready session"
        );
        assert_eq!(lease_drops.load(Ordering::Relaxed), 0);

        handle.shutdown_and_wait().await.unwrap();
        assert_eq!(lease_drops.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn ensure_connected_interrupts_retry_backoff_without_clearing_state() {
        let database = TestDatabase::new("ensure-backoff");
        let expected_binding = binding(0x72);
        let attempts = Arc::new(AtomicUsize::new(0));
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let connector = FakeConnector {
            outcomes: VecDeque::from([
                Err("physical link is not registered".to_owned()),
                Ok((expected_binding, Vec::new())),
            ]),
            attempts: attempts.clone(),
            submitted,
        };
        let mut delayed = config(&database);
        delayed.reconnect_initial = Duration::from_secs(30);
        delayed.reconnect_maximum = Duration::from_secs(30);
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(delayed, store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            snapshot.connection() == &ConnectionState::Backoff
        })
        .await;
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        handle.ensure_connected().await.unwrap();
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
        })
        .await;
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        let expected_device = DeviceView::from(expected_binding);
        assert_eq!(handle.snapshot().device(), Some(&expected_device));

        handle.shutdown_and_wait().await.unwrap();
    }

    #[tokio::test]
    async fn binding_failure_drops_session_before_transport_lease() {
        let database = TestDatabase::new("binding-drop-order");
        let order = Arc::new(Mutex::new(Vec::new()));
        let connector = BindingFailureConnector {
            returned_session: false,
            order: order.clone(),
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let mut failure_config = config(&database);
        failure_config.reconnect_initial = Duration::from_secs(1);
        failure_config.reconnect_maximum = Duration::from_secs(1);
        let handle = start_with_connector(failure_config, store, connector).unwrap();
        wait_for(&handle, |_| order.lock().unwrap().len() == 2).await;
        assert_eq!(
            order.lock().unwrap().as_slice(),
            &["session", "lease"],
            "the transport gate must remain leased until the old session/FD closes"
        );
        handle.shutdown_and_wait().await.unwrap();
    }
}
