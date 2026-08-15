//! Transport-neutral durable LXMF chat actor.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reticulum_device_api::{
    ApiErrorCode, MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES,
};
use reticulum_device_client::ClientError;
use reticulum_lxmf_chat_app::{
    ChatEngine, DeviceSessionError, EngineError, InboxStep, LxmfSession, ReconcileStep,
};
use reticulum_lxmf_chat_core::{
    AttemptLocationStamp, ChatStore, Contact, ContactUpsertOutcome, ConversationPeer,
    DestinationHash, DeviceBinding, IdempotencyKey, InboundCommitOutcome,
    MessageIngressObservation, MessageLocation, MessageSignalObservation, OutboxCommitOutcome,
    OutboxId, OutboxMaterial, OutboxRetryOutcome, OutboxStatus, PacketEvidence, SqliteChatStore,
    SqliteStoreError, StatusProjectionOutcome, SubmissionFailure, SubmissionState,
    TimelineDirection as CoreTimelineDirection, TimelineEntry, UnixTimestampMillis,
};
pub use reticulum_lxmf_wire::{
    BASIC_LXMF_SELECTION_OVERHEAD_BYTES, EMPTY_LXMF_FIELDS_ENCODED_BYTES, LINK_PACKET_MAX_CONTENT,
    MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES,
};
use reticulum_lxmf_wire::{
    SidebandLocationTelemetry, basic_lxmf_payload_fits_content_limit,
    encoded_sideband_location_fields_len,
};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::{oneshot, watch};
use ts_rs::TS;

mod activity;
mod diagnostics;
mod location;
mod nearby;
mod network;
mod nomad;
mod probe;
mod radio_trace;

pub use activity::{
    MessageActivityEventView, MessageActivityKindView, MessageActivityPageRequest,
    MessageActivityPageView, MessageActivityRequestError, MessageActivityRetryTriggerView,
};
pub use diagnostics::{
    DiagnosticInterfaceKindView, DiagnosticInterfaceStateView, DiagnosticInterfaceView,
    DiagnosticLoraDataTxEvidenceView, DiagnosticLoraLastRxView, DiagnosticLoraLastTxView,
    DiagnosticLoraTxFamilyView, DiagnosticLoraTxOutcomeView, LoraDiagnosticsView,
    MAX_RADIO_ROUTE_ENTRIES, RadioRoutesStatusView, RetainedRouteView, RnsDiagnosticsView,
    RouteDiagnosticResolutionView,
};
pub use location::{
    PhoneLocationAuthorizationView, PhoneLocationObservationError, PhoneLocationObservationView,
    PhoneLocationSourceView, PhoneLocationUnavailableReasonView,
};
pub use nearby::{MAX_NEARBY_PEERS, NearbyPeerView};
pub use network::{
    LoraRadioProfileView, NetworkConfigMutation, NetworkConfigMutationOutcome,
    NetworkConfigMutationRequest, NetworkConfigView, NetworkRequestError, NetworkRuntimeStatusView,
    ReticulumDnsDiagnosticsView, ReticulumDnsPrimaryOutcomeView, ReticulumDnsRawAttemptView,
    ReticulumDnsRawOutcomeView, ReticulumDnsRawSetupStateView, ReticulumDnsRawSourceView,
    ReticulumDnsResolutionSourceView, ReticulumDnsResolutionView, ReticulumTcpFailureView,
    ReticulumTcpPeerHostnameInput, ReticulumTcpPeerIpv4Input, ReticulumTcpPeerStateView,
    ReticulumTcpPeerView, RmapPhoneLocation, WifiCredentialUpdate, WifiNetworkProfileView,
    WifiStationStateView,
};
pub use nomad::{
    NomadFetchFailure, NomadFetchPhase, NomadFetchPollRequest, NomadFetchPollResponse,
    NomadFetchStartOutcome, NomadFetchStartRequest, NomadFetchStartResponse,
};
pub use probe::{
    ReticulumProbeFailure, ReticulumProbeIngressView, ReticulumProbePhase,
    ReticulumProbePollRequest, ReticulumProbePollResponse, ReticulumProbeSignalView,
    ReticulumProbeStartOutcome, ReticulumProbeStartRequest, ReticulumProbeStartResponse,
    ReticulumProbeSuccessView,
};
pub use radio_trace::{
    RadioTraceAttemptOutcomeView, RadioTraceEventKindView, RadioTraceEventView,
    RadioTraceMessageCorrelationView, RadioTracePageRequest, RadioTracePageView,
    RadioTraceProfileView, RadioTraceRequestError, RadioTraceRouteResolutionView,
    RadioTraceTxOutcomeView,
};

const COMMAND_CAPACITY: usize = 32;
const COMMAND_WAIT: Duration = Duration::from_millis(50);
const RADIO_TRACE_CHAT_YIELD_TURNS: u8 = 4;
const RADIO_TRACE_IDLE_INTERVAL: Duration = Duration::from_secs(5);
const RETRY_LATER_BACKOFF: Duration = Duration::from_millis(250);

/// Largest UTF-8 contact display name accepted by every client adapter.
pub const MAX_CONTACT_NAME_BYTES: usize = 256;

/// Largest integer JSON clients may represent without precision loss.
pub const MAX_JSON_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Admission result for an authenticated manual ordinary service announce.
///
/// Both outcomes are successful. Repeated app presses coalesce on the board
/// rather than consuming additional queue capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ManualServiceAnnounceDisposition {
    /// A fresh set of primary, LXMF, and NomadNet announces was queued.
    Queued,
    /// An equivalent set of service announces was already pending.
    AlreadyPending,
}

impl From<reticulum_device_api::ManualServiceAnnounceDisposition>
    for ManualServiceAnnounceDisposition
{
    fn from(disposition: reticulum_device_api::ManualServiceAnnounceDisposition) -> Self {
        match disposition {
            reticulum_device_api::ManualServiceAnnounceDisposition::Queued => Self::Queued,
            reticulum_device_api::ManualServiceAnnounceDisposition::AlreadyPending => {
                Self::AlreadyPending
            }
        }
    }
}

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
    /// The combined title, content, and fields exceed the direct LXMF lane.
    MessageTooLarge,
    /// A timestamp is outside the exact LXMF/JavaScript range.
    InvalidTimestamp,
    /// An idempotency key is not an exact 16-byte hexadecimal value.
    InvalidIdempotencyKey,
    /// An outbox identifier is zero or outside the local identifier range.
    InvalidOutboxId,
    /// A message location contains coordinates outside geographic bounds.
    InvalidMessageLocation,
    /// A NomadNet path was empty, relative, or contained a NUL byte.
    InvalidNomadPath,
    /// A NomadNet path exceeded the device API's fixed UTF-8 limit.
    NomadPathTooLong,
    /// A NomadNet fetch ID was not an exact valid 16-byte hexadecimal value.
    InvalidNomadFetchId,
    /// A Reticulum proof probe ID was not an exact nonzero 16-byte hexadecimal value.
    InvalidReticulumProbeId,
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
            Self::MessageTooLarge => {
                "message title, content, and fields exceed the direct LXMF packet limit"
            }
            Self::InvalidTimestamp => "timestamp is outside the client range",
            Self::InvalidIdempotencyKey => {
                "idempotency key must contain exactly 32 hexadecimal characters"
            }
            Self::InvalidOutboxId => "outbox identifier must be a non-zero safe integer",
            Self::InvalidMessageLocation => "message location coordinates are invalid",
            Self::InvalidNomadPath => {
                "NomadNet path must be absolute, non-empty, and contain no NUL byte"
            }
            Self::NomadPathTooLong => "NomadNet path exceeds device limit",
            Self::InvalidNomadFetchId => {
                "NomadNet fetch ID must contain exactly 32 valid hexadecimal characters"
            }
            Self::InvalidReticulumProbeId => {
                "Reticulum probe ID must contain exactly 32 nonzero hexadecimal characters"
            }
        })
    }
}

/// Sideband-compatible location snapshot carried inside one LXMF message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[allow(missing_docs)]
pub struct MessageLocationView {
    latitude_e6: i32,
    longitude_e6: i32,
    altitude_cm: i32,
    speed_cm_per_second: u32,
    bearing_centidegrees: i32,
    accuracy_cm: u16,
    updated_at_unix_seconds: u32,
}

impl MessageLocationView {
    fn into_core(self) -> Result<MessageLocation, ClientRequestError> {
        MessageLocation::new(
            self.latitude_e6,
            self.longitude_e6,
            self.altitude_cm,
            self.speed_cm_per_second,
            self.bearing_centidegrees,
            self.accuracy_cm,
            self.updated_at_unix_seconds,
        )
        .ok_or(ClientRequestError::InvalidMessageLocation)
    }
}

impl From<MessageLocation> for MessageLocationView {
    fn from(location: MessageLocation) -> Self {
        Self {
            latitude_e6: location.latitude_e6(),
            longitude_e6: location.longitude_e6(),
            altitude_cm: location.altitude_cm(),
            speed_cm_per_second: location.speed_cm_per_second(),
            bearing_centidegrees: location.bearing_centidegrees(),
            accuracy_cm: location.accuracy_cm(),
            updated_at_unix_seconds: location.updated_at_unix_seconds(),
        }
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
    #[serde(default)]
    #[ts(optional = nullable)]
    location: Option<MessageLocationView>,
}

impl SendRequest {
    /// Validate and project this request into exact commit-before-send data.
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
        let location = self
            .location
            .map(MessageLocationView::into_core)
            .transpose()?;
        let fields_encoded_bytes = location.map_or(EMPTY_LXMF_FIELDS_ENCODED_BYTES, |location| {
            encoded_sideband_location_fields_len(SidebandLocationTelemetry::new(
                location.latitude_e6(),
                location.longitude_e6(),
                location.altitude_cm(),
                location.speed_cm_per_second(),
                location.bearing_centidegrees(),
                location.accuracy_cm(),
                location.updated_at_unix_seconds(),
            ))
        });
        if !basic_lxmf_payload_fits_content_limit(
            self.title.len(),
            self.content.len(),
            fields_encoded_bytes,
            LINK_PACKET_MAX_CONTENT,
        ) {
            return Err(ClientRequestError::MessageTooLarge);
        }
        Ok(OutboxMaterial::new(
            destination,
            timestamp,
            idempotency_key,
            self.title.into_bytes(),
            self.content.into_bytes(),
        )
        .with_location(location))
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

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
#[allow(missing_docs)]
pub struct RetrySendRequest {
    #[serde(deserialize_with = "deserialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    outbox_id: u64,
    idempotency_key: String,
}

impl RetrySendRequest {
    /// Validate the stable row identifier and fresh device request key.
    pub fn into_retry(self) -> Result<(OutboxId, IdempotencyKey), ClientRequestError> {
        let outbox_id = OutboxId::new(self.outbox_id).ok_or(ClientRequestError::InvalidOutboxId)?;
        let idempotency_key = parse_hex::<16>(&self.idempotency_key)
            .map(IdempotencyKey::new)
            .ok_or(ClientRequestError::InvalidIdempotencyKey)?;
        Ok((outbox_id, idempotency_key))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum RetrySendOutcome {
    Requeued,
    AlreadyPending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct RetrySendResponse {
    #[serde(serialize_with = "serialize_json_safe_u64")]
    #[ts(as = "JsonSafeInteger")]
    outbox_id: u64,
    outcome: RetrySendOutcome,
}

impl From<OutboxRetryOutcome> for RetrySendResponse {
    fn from(outcome: OutboxRetryOutcome) -> Self {
        match outcome {
            OutboxRetryOutcome::Requeued(id) => Self {
                outbox_id: id.get(),
                outcome: RetrySendOutcome::Requeued,
            },
            OutboxRetryOutcome::AlreadyPending(id) => Self {
                outbox_id: id.get(),
                outcome: RetrySendOutcome::AlreadyPending,
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
    radio_trace_idle_interval: Duration,
    retry_later_backoff: Duration,
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
            radio_trace_idle_interval: RADIO_TRACE_IDLE_INTERVAL,
            retry_later_backoff: RETRY_LATER_BACKOFF,
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

/// One peer present in saved contacts or durable message history.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct ConversationPeerView {
    destination: String,
    name: Option<String>,
    #[serde(serialize_with = "serialize_json_safe_usize")]
    #[ts(as = "JsonSafeInteger")]
    message_count: usize,
    #[serde(serialize_with = "serialize_json_safe_usize")]
    #[ts(as = "JsonSafeInteger")]
    inbound_message_count: usize,
    last_message: Option<TimelineView>,
}

impl ConversationPeerView {
    /// Peer `lxmf.delivery` destination encoded as lowercase hexadecimal.
    pub fn destination(&self) -> &str {
        &self.destination
    }

    /// User-selected display name, only when this is a saved contact.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Number of durable messages in this conversation.
    pub const fn message_count(&self) -> usize {
        self.message_count
    }

    /// Number of authenticated inbound messages observed from this peer.
    pub const fn inbound_message_count(&self) -> usize {
        self.inbound_message_count
    }

    /// Most recent durable message, when this peer has history.
    pub const fn last_message(&self) -> Option<&TimelineView> {
        self.last_message.as_ref()
    }
}

impl From<ConversationPeer> for ConversationPeerView {
    fn from(peer: ConversationPeer) -> Self {
        Self {
            destination: hex::encode(peer.peer().as_bytes()),
            name: peer.saved_name().map(str::to_owned),
            message_count: peer.message_count(),
            inbound_message_count: peer.inbound_message_count(),
            last_message: peer.last_message().cloned().map(TimelineView::from),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum BytesEncoding {
    Utf8,
    Hex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
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

/// Immutable packet evidence retained for the current durable device submission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct PacketEvidenceView {
    encoded_packet_len: u16,
    encoded_packet_sha256: String,
}

impl From<PacketEvidence> for PacketEvidenceView {
    fn from(evidence: PacketEvidence) -> Self {
        Self {
            encoded_packet_len: evidence.encoded_packet_len(),
            encoded_packet_sha256: hex::encode(evidence.encoded_packet_sha256().as_bytes()),
        }
    }
}

impl PacketEvidenceView {
    /// Complete encoded Reticulum packet length.
    pub const fn encoded_packet_len(&self) -> u16 {
        self.encoded_packet_len
    }

    /// SHA-256 digest encoded as lowercase hexadecimal.
    pub fn encoded_packet_sha256(&self) -> &str {
        &self.encoded_packet_sha256
    }
}

/// Receiver-local physical signal values for one inbound message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct MessageSignalObservationView {
    rssi_dbm: i16,
    snr_db: i16,
}

impl From<MessageSignalObservation> for MessageSignalObservationView {
    fn from(signal: MessageSignalObservation) -> Self {
        Self {
            rssi_dbm: signal.rssi_dbm(),
            snr_db: signal.snr_db(),
        }
    }
}

/// First-arrival interface and optional final-hop signal evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, TS)]
#[allow(missing_docs)]
pub struct MessageIngressObservationView {
    interface_id: u8,
    signal: Option<MessageSignalObservationView>,
}

impl From<MessageIngressObservation> for MessageIngressObservationView {
    fn from(ingress: MessageIngressObservation) -> Self {
        Self {
            interface_id: ingress.interface().get(),
            signal: ingress.signal().map(MessageSignalObservationView::from),
        }
    }
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
    #[serde(serialize_with = "serialize_optional_json_safe_u64")]
    #[ts(as = "Option<JsonSafeInteger>")]
    submission_id: Option<u64>,
    current_attempt_number: Option<u32>,
    automatic_retry_count: Option<u8>,
    status: Option<TimelineStatus>,
    packet_evidence: Option<PacketEvidenceView>,
    ingress_observation: Option<MessageIngressObservationView>,
    receiver_location: Option<PhoneLocationObservationView>,
    location: Option<MessageLocationView>,
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

    /// Current device submission identifier for an accepted outbound row.
    pub const fn submission_id(&self) -> Option<u64> {
        self.submission_id
    }

    /// Best reconstructable one-based app-created device-submission number.
    ///
    /// Consult the activity page's `history_incomplete` flag for migrated
    /// databases whose historical manual retries cannot be reconstructed.
    pub const fn current_attempt_number(&self) -> Option<u32> {
        self.current_attempt_number
    }

    /// Historical app-owned automatic-rearm count for this outbound row.
    ///
    /// This is read-side migration compatibility, not a firmware RNS-attempt
    /// count. Current production rows do not increment it.
    pub const fn automatic_retry_count(&self) -> Option<u8> {
        self.automatic_retry_count
    }

    /// Current outbound status, or `None` for inbound rows.
    pub const fn status(&self) -> Option<TimelineStatus> {
        self.status
    }

    /// Current immutable packet evidence, when projected by the device.
    pub const fn packet_evidence(&self) -> Option<&PacketEvidenceView> {
        self.packet_evidence.as_ref()
    }

    /// Receiver-local first-arrival evidence for an inbound row.
    pub const fn ingress_observation(&self) -> Option<MessageIngressObservationView> {
        self.ingress_observation
    }

    /// Receiver phone position retained when this inbound message was imported.
    ///
    /// This is an app import-time observation, not board GNSS or a claim about
    /// the exact RF-arrival position.
    pub const fn receiver_location(&self) -> Option<PhoneLocationObservationView> {
        self.receiver_location
    }

    /// Optional authenticated location snapshot carried with the message.
    pub const fn location(&self) -> Option<MessageLocationView> {
        self.location
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
            submission_id: entry.submission_id().map(|id| id.get()),
            current_attempt_number: entry.current_attempt().map(|attempt| attempt.get()),
            automatic_retry_count: entry.automatic_retry_count(),
            status: entry.outbox_status().map(status_name),
            packet_evidence: entry.packet_evidence().map(PacketEvidenceView::from),
            ingress_observation: entry
                .ingress_observation()
                .map(MessageIngressObservationView::from),
            receiver_location: entry.receiver_location().map(|sample| {
                PhoneLocationObservationView::from(AttemptLocationStamp::Available(sample))
            }),
            location: entry.location().map(MessageLocationView::from),
            title: BytesView::new(entry.title()),
            content: BytesView::new(entry.content()),
        }
    }
}

pub(crate) fn status_name(status: OutboxStatus) -> TimelineStatus {
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

fn current_unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

enum Command {
    Contacts(oneshot::Sender<Result<Vec<ContactView>, String>>),
    ConversationPeers(oneshot::Sender<Result<Vec<ConversationPeerView>, String>>),
    PhoneLocationGet(oneshot::Sender<Result<PhoneLocationObservationView, String>>),
    PhoneLocationUpdate {
        observation: AttemptLocationStamp,
        reply: oneshot::Sender<Result<PhoneLocationObservationView, String>>,
    },
    NearbyPeers(oneshot::Sender<Result<Vec<NearbyPeerView>, String>>),
    NomadFetchStart {
        request: NomadFetchStartRequest,
        reply: oneshot::Sender<Result<NomadFetchStartResponse, String>>,
    },
    NomadFetchPoll {
        request: NomadFetchPollRequest,
        reply: oneshot::Sender<Result<NomadFetchPollResponse, String>>,
    },
    ReticulumProbeStart {
        request: ReticulumProbeStartRequest,
        reply: oneshot::Sender<Result<ReticulumProbeStartResponse, String>>,
    },
    ReticulumProbePoll {
        request: ReticulumProbePollRequest,
        reply: oneshot::Sender<Result<ReticulumProbePollResponse, String>>,
    },
    NetworkConfigGet(oneshot::Sender<Result<NetworkConfigView, String>>),
    NetworkConfigMutate {
        request: NetworkConfigMutationRequest,
        reply: oneshot::Sender<Result<NetworkConfigMutationOutcome, String>>,
    },
    NetworkStatus(oneshot::Sender<Result<NetworkRuntimeStatusView, String>>),
    RadioRoutesStatus(oneshot::Sender<Result<RadioRoutesStatusView, String>>),
    ManualServiceAnnounce(oneshot::Sender<Result<ManualServiceAnnounceDisposition, String>>),
    Timeline {
        peer: DestinationHash,
        reply: oneshot::Sender<Result<Vec<TimelineView>, String>>,
    },
    MessageActivity {
        request: MessageActivityPageRequest,
        reply: oneshot::Sender<Result<MessageActivityPageView, String>>,
    },
    RadioTrace {
        request: RadioTracePageRequest,
        reply: oneshot::Sender<Result<RadioTracePageView, String>>,
    },
    UpsertContact {
        contact: Contact,
        reply: oneshot::Sender<Result<ContactUpsertOutcome, String>>,
    },
    EnqueueSend {
        material: OutboxMaterial,
        reply: oneshot::Sender<Result<OutboxCommitOutcome, String>>,
    },
    RetrySend {
        outbox_id: OutboxId,
        idempotency_key: IdempotencyKey,
        reply: oneshot::Sender<Result<OutboxRetryOutcome, String>>,
    },
    SyncNow(oneshot::Sender<Result<(), String>>),
    EnsureConnected(oneshot::Sender<Result<(), String>>),
    Reconnect(oneshot::Sender<Result<(), String>>),
    Shutdown,
}

impl Command {
    /// Whether this command is confined to actor-owned memory or SQLite.
    ///
    /// Only these commands may share one foreground burst. Device-session
    /// calls can each consume a complete transport deadline and therefore
    /// must be separated by a background inbox/reconcile/trace opportunity.
    const fn is_local_actor_work(&self) -> bool {
        matches!(
            self,
            Self::Contacts(_)
                | Self::ConversationPeers(_)
                | Self::PhoneLocationGet(_)
                | Self::PhoneLocationUpdate { .. }
                | Self::Timeline { .. }
                | Self::MessageActivity { .. }
                | Self::RadioTrace { .. }
                | Self::UpsertContact { .. }
                | Self::EnqueueSend { .. }
                | Self::RetrySend { .. }
                | Self::SyncNow(_)
                | Self::EnsureConnected(_)
        )
    }
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

    /// Load saved contacts and otherwise-unknown durable message peers.
    pub async fn conversation_peers(&self) -> Result<Vec<ConversationPeerView>, ServiceError> {
        self.request(Command::ConversationPeers).await
    }

    /// Read the phone-location state that will stamp the next outbound attempt.
    pub async fn phone_location_observation(
        &self,
    ) -> Result<PhoneLocationObservationView, ServiceError> {
        self.request(Command::PhoneLocationGet).await
    }

    /// Replace the cached phone-location state used by future message attempts.
    ///
    /// This local mutation never waits for or contacts the appliance. Existing
    /// attempts retain their original durable stamp.
    pub async fn update_phone_location(
        &self,
        observation: PhoneLocationObservationView,
    ) -> Result<PhoneLocationObservationView, ServiceError> {
        let observation = observation
            .into_core()
            .map_err(|error| ServiceError::Operation(error.to_string()))?;
        self.request(|reply| Command::PhoneLocationUpdate { observation, reply })
            .await
    }

    /// Read the current bounded semantic nearby-peer projection.
    ///
    /// This operation requires a ready authenticated device session. It is
    /// intentionally not cached in SQLite: Reticulum announces and route hints
    /// are volatile selection evidence, while an explicitly added contact is
    /// durable.
    pub async fn nearby_peers(&self) -> Result<Vec<NearbyPeerView>, ServiceError> {
        self.request(Command::NearbyPeers).await
    }

    /// Begin or idempotently replay one bounded anonymous NomadNet page fetch.
    ///
    /// The actor executes this through the same authenticated session that owns
    /// chat, discovery, and background synchronization.
    pub async fn nomad_fetch_start(
        &self,
        request: NomadFetchStartRequest,
    ) -> Result<NomadFetchStartResponse, ServiceError> {
        self.request(|reply| Command::NomadFetchStart { request, reply })
            .await
    }

    /// Poll one boot-scoped NomadNet fetch through the actor-owned session.
    pub async fn nomad_fetch_poll(
        &self,
        request: NomadFetchPollRequest,
    ) -> Result<NomadFetchPollResponse, ServiceError> {
        self.request(|reply| Command::NomadFetchPoll { request, reply })
            .await
    }

    /// Begin or idempotently replay one bounded Reticulum path-and-proof probe.
    pub async fn reticulum_probe_start(
        &self,
        request: ReticulumProbeStartRequest,
    ) -> Result<ReticulumProbeStartResponse, ServiceError> {
        self.request(|reply| Command::ReticulumProbeStart { request, reply })
            .await
    }

    /// Poll one boot-scoped Reticulum path-and-proof probe.
    pub async fn reticulum_probe_poll(
        &self,
        request: ReticulumProbePollRequest,
    ) -> Result<ReticulumProbePollResponse, ServiceError> {
        self.request(|reply| Command::ReticulumProbePoll { request, reply })
            .await
    }

    /// Read the board-owned desired network configuration with secrets redacted.
    pub async fn network_config(&self) -> Result<NetworkConfigView, ServiceError> {
        self.request(Command::NetworkConfigGet).await
    }

    /// Apply one compare-and-swap Wi-Fi or Reticulum TCP configuration mutation.
    pub async fn mutate_network_config(
        &self,
        request: NetworkConfigMutationRequest,
    ) -> Result<NetworkConfigMutationOutcome, ServiceError> {
        self.request(|reply| Command::NetworkConfigMutate { request, reply })
            .await
    }

    /// Read current secret-free Wi-Fi station and Reticulum TCP interface state.
    pub async fn network_status(&self) -> Result<NetworkRuntimeStatusView, ServiceError> {
        self.request(Command::NetworkStatus).await
    }

    /// Read one coherent bounded local radio, interface, and retained-route view.
    ///
    /// Retained routes are routing-table state rather than connected or
    /// necessarily reachable peers. Recently heard announce observations
    /// remain available through [`Self::nearby_peers`].
    pub async fn radio_routes_status(&self) -> Result<RadioRoutesStatusView, ServiceError> {
        self.request(Command::RadioRoutesStatus).await
    }

    /// Queue ordinary primary, LXMF, and NomadNet service announces.
    pub async fn manual_service_announce(
        &self,
    ) -> Result<ManualServiceAnnounceDisposition, ServiceError> {
        self.request(Command::ManualServiceAnnounce).await
    }

    /// Load the ordered timeline for one peer.
    pub async fn timeline(&self, peer: DestinationHash) -> Result<Vec<TimelineView>, ServiceError> {
        self.request(|reply| Command::Timeline { peer, reply })
            .await
    }

    /// Load one bounded newest-first page of durable message activity.
    ///
    /// This local database query remains available while the appliance
    /// transport is disconnected.
    pub async fn message_activity(
        &self,
        request: MessageActivityPageRequest,
    ) -> Result<MessageActivityPageView, ServiceError> {
        self.request(|reply| Command::MessageActivity { request, reply })
            .await
    }

    /// Load one bounded newest-first page of durable packet-correlated RF trace.
    ///
    /// This local database query remains available while the appliance
    /// transport is disconnected. Background synchronization fills it whenever
    /// an authenticated board session is ready.
    pub async fn radio_trace(
        &self,
        request: RadioTracePageRequest,
    ) -> Result<RadioTracePageView, ServiceError> {
        self.request(|reply| Command::RadioTrace { request, reply })
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

    /// Create a transitional replacement device submission for a legacy or
    /// permanently terminal message without adding a timeline row.
    ///
    /// Current board-owned `Preparing` messages retry without this app action.
    pub async fn retry_send(
        &self,
        outbox_id: OutboxId,
        idempotency_key: IdempotencyKey,
    ) -> Result<OutboxRetryOutcome, ServiceError> {
        self.request(|reply| Command::RetrySend {
            outbox_id,
            idempotency_key,
            reply,
        })
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
    deferred_foreground: Option<Command>,
    published: Arc<RwLock<Arc<ApplianceSnapshot>>>,
    revisions: watch::Sender<u64>,
    snapshot: ApplianceSnapshot,
    session: Option<BoxedSession>,
    session_lease: Option<Box<dyn Send>>,
    reconnect_delay: Duration,
    next_connect: Instant,
    next_reconcile: Instant,
    next_inbox: Instant,
    next_radio_trace: Instant,
    radio_trace_chat_yields_remaining: u8,
    urgent_reconcile_outbox: Option<OutboxId>,
    urgent_reconcile_due: bool,
    urgent_reconcile_reserves_lane: bool,
    prefer_inbox: bool,
    background_error_work: Option<BackgroundWork>,
    permanent_fault: bool,
    phone_location: AttemptLocationStamp,
    radio_trace_cursor: Option<reticulum_device_api::RadioTraceCursor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackgroundWork {
    Inbox,
    RadioTrace,
    Reconcile,
}

fn reconcile_refresh_delay(config: &ApplianceConfig, state: SubmissionState) -> Duration {
    if state.is_terminal() {
        config.operation_gap
    } else {
        config.status_poll_interval
    }
}

fn background_api_retry_delay(config: &ApplianceConfig, code: ApiErrorCode) -> Option<Duration> {
    match code {
        // Retained flash ownership is routine, short-lived pressure. Retrying
        // the exact operation preserves both the authenticated session and
        // the engine's durable reconcile/inbox cursor.
        ApiErrorCode::RetryLater => Some(config.retry_later_backoff),
        // Capacity exhaustion is a structural rejection, not transient
        // ownership pressure. Keep its deliberately slower policy distinct.
        ApiErrorCode::CapacityExhausted => Some(config.capacity_backoff),
        _ => None,
    }
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
            deferred_foreground: None,
            published,
            revisions,
            snapshot: ApplianceSnapshot::starting(),
            session: None,
            session_lease: None,
            next_connect: now,
            next_reconcile: now,
            next_inbox: now,
            next_radio_trace: now,
            radio_trace_chat_yields_remaining: 0,
            urgent_reconcile_outbox: None,
            urgent_reconcile_due: false,
            urgent_reconcile_reserves_lane: false,
            prefer_inbox: false,
            background_error_work: None,
            permanent_fault: false,
            phone_location: AttemptLocationStamp::not_observed(),
            radio_trace_cursor: None,
        }
    }

    fn run(mut self) {
        self.refresh_counts();
        self.publish();
        loop {
            let urgent_wait = self.urgent_status_wait(Instant::now());
            let command = match (self.deferred_foreground.is_some(), urgent_wait) {
                (true, Some(wait)) => {
                    if !wait.is_zero() {
                        std::thread::sleep(wait);
                    }
                    None
                }
                (true, None) => self.deferred_foreground.take(),
                (false, wait) => {
                    let wait = wait.unwrap_or(COMMAND_WAIT);
                    if wait.is_zero() {
                        None
                    } else {
                        match self.commands.recv_timeout(wait) {
                            Ok(command) => Some(command),
                            Err(mpsc::RecvTimeoutError::Timeout) => None,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                }
            };
            if let Some(command) = command {
                if matches!(command, Command::Shutdown) {
                    break;
                }
                if self.command_blocked_by_urgent_status(&command) {
                    self.deferred_foreground = Some(command);
                } else {
                    if !self.foreground_turn(command) {
                        break;
                    }
                }
            }
            self.background_turn();
        }
        self.clear_session();
        self.snapshot.connection = ConnectionState::Stopped;
        self.publish();
    }

    fn urgent_status_wait(&self, now: Instant) -> Option<Duration> {
        (self.urgent_reconcile_reserves_lane && self.session.is_some()).then(|| {
            self.next_reconcile
                .saturating_duration_since(now)
                .min(COMMAND_WAIT)
        })
    }

    fn command_blocked_by_urgent_status(&self, command: &Command) -> bool {
        self.urgent_reconcile_reserves_lane
            && self.session.is_some()
            && !command.is_local_actor_work()
            && !matches!(command, Command::Shutdown)
    }

    /// Drain one bounded local foreground burst before background device I/O.
    ///
    /// Local commands already waiting in the bounded channel stay together,
    /// avoiding needless device I/O between a concurrently queued group of
    /// SQLite reads. A device-session command is deferred before execution so
    /// the run loop performs one inbox/reconcile/trace turn first. Never wait
    /// here for a caller to enqueue more work: doing so lets a fast foreground
    /// poll continuously postpone background progress.
    fn foreground_turn(&mut self, first: Command) -> bool {
        let mut next = first;
        for handled in 0..COMMAND_CAPACITY {
            let local = next.is_local_actor_work();
            match next {
                Command::Shutdown => return false,
                command => self.handle_command(command),
            }

            if !local {
                return true;
            }

            if handled + 1 == COMMAND_CAPACITY {
                return true;
            }
            next = match self.commands.try_recv() {
                Ok(Command::Shutdown) => return false,
                Ok(command) if command.is_local_actor_work() => command,
                Ok(command) => {
                    self.deferred_foreground = Some(command);
                    return true;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => return false,
            };
        }
        true
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
            Command::ConversationPeers(reply) => {
                let result = self
                    .engine
                    .conversation_peers()
                    .map(|peers| peers.into_iter().map(ConversationPeerView::from).collect())
                    .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
            Command::PhoneLocationGet(reply) => {
                let _ = reply.send(Ok(PhoneLocationObservationView::from(self.phone_location)));
            }
            Command::PhoneLocationUpdate { observation, reply } => {
                self.phone_location = observation;
                let _ = reply.send(Ok(PhoneLocationObservationView::from(observation)));
            }
            Command::NearbyPeers(reply) => {
                let read = match self.session.as_mut() {
                    Some(session) => nearby::read_nearby_peers(session.as_mut())
                        .map_err(|error| error.to_string()),
                    None => Err(
                        "no authenticated appliance session is ready for nearby discovery"
                            .to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = read.as_ref().err().cloned().unwrap_or_else(|| {
                        "nearby peer request consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(read);
            }
            Command::NomadFetchStart { request, reply } => {
                let result = match self.session.as_mut() {
                    Some(session) => request
                        .as_device_request()
                        .map_err(|error| error.to_string())
                        .and_then(|request| {
                            session
                                .nomad_fetch_start(request)
                                .map(NomadFetchStartResponse::from)
                                .map_err(|error| error.to_string())
                        }),
                    None => Err(
                        "no authenticated appliance session is ready for NomadNet fetch".to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "NomadNet fetch start consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(result);
            }
            Command::NomadFetchPoll { request, reply } => {
                let result = match self.session.as_mut() {
                    Some(session) => request
                        .as_device_id()
                        .map_err(|error| error.to_string())
                        .and_then(|id| {
                            session
                                .nomad_fetch_poll(id)
                                .map(NomadFetchPollResponse::from)
                                .map_err(|error| error.to_string())
                        }),
                    None => Err(
                        "no authenticated appliance session is ready for NomadNet fetch".to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "NomadNet fetch poll consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(result);
            }
            Command::ReticulumProbeStart { request, reply } => {
                let result = match self.session.as_mut() {
                    Some(session) => request
                        .as_device_request()
                        .map_err(|error| error.to_string())
                        .and_then(|request| {
                            session
                                .reticulum_probe_start(request)
                                .map(ReticulumProbeStartResponse::from)
                                .map_err(|error| error.to_string())
                        }),
                    None => Err(
                        "no authenticated appliance session is ready for Reticulum measurement"
                            .to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "Reticulum probe start consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(result);
            }
            Command::ReticulumProbePoll { request, reply } => {
                let result = match self.session.as_mut() {
                    Some(session) => request
                        .as_device_id()
                        .map_err(|error| error.to_string())
                        .and_then(|id| {
                            session
                                .reticulum_probe_poll(id)
                                .map(ReticulumProbePollResponse::from)
                                .map_err(|error| error.to_string())
                        }),
                    None => Err(
                        "no authenticated appliance session is ready for Reticulum measurement"
                            .to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "Reticulum probe poll consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(result);
            }
            Command::NetworkConfigGet(reply) => {
                let result = match self.session.as_mut() {
                    Some(session) => session
                        .network_config_get()
                        .map(NetworkConfigView::from)
                        .map_err(|error| error.to_string()),
                    None => Err(
                        "no authenticated appliance session is ready for network configuration"
                            .to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "network configuration read consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(result);
            }
            Command::NetworkConfigMutate { request, reply } => {
                let result = match self.session.as_mut() {
                    Some(session) => request
                        .with_device_request(|request| session.network_config_mutate(request))
                        .map_err(|error| error.to_string())
                        .and_then(|result| {
                            result
                                .map(NetworkConfigMutationOutcome::from)
                                .map_err(|error| error.to_string())
                        }),
                    None => Err(
                        "no authenticated appliance session is ready for network configuration"
                            .to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "network configuration mutation consumed the authenticated session"
                            .to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(result);
            }
            Command::NetworkStatus(reply) => {
                let result = match self.session.as_mut() {
                    Some(session) => session
                        .network_status()
                        .map(NetworkRuntimeStatusView::from)
                        .map_err(|error| error.to_string()),
                    None => Err(
                        "no authenticated appliance session is ready for network status".to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "network status read consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(result);
            }
            Command::RadioRoutesStatus(reply) => {
                let result = match self.session.as_mut() {
                    Some(session) => diagnostics::read_radio_routes(session.as_mut())
                        .map_err(|error| error.to_string()),
                    None => Err(
                        "no authenticated appliance session is ready for radio and route diagnostics"
                            .to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "radio and route diagnostics consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
                let _ = reply.send(result);
            }
            Command::ManualServiceAnnounce(reply) => {
                let result = match self.session.as_mut() {
                    Some(session) => session
                        .manual_service_announce()
                        .map(ManualServiceAnnounceDisposition::from)
                        .map_err(|error| error.to_string()),
                    None => Err(
                        "no authenticated appliance session is ready for a service announce"
                            .to_owned(),
                    ),
                };
                let session_consumed = self
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.is_usable());
                if session_consumed {
                    let reason = result.as_ref().err().cloned().unwrap_or_else(|| {
                        "manual service announce consumed the authenticated session".to_owned()
                    });
                    self.retry_connection(reason);
                }
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
            Command::MessageActivity { request, reply } => {
                let result = request
                    .as_core()
                    .map_err(|error| error.to_string())
                    .and_then(|request| {
                        self.engine
                            .message_activity(request)
                            .map(MessageActivityPageView::from)
                            .map_err(|error| error.to_string())
                    });
                let _ = reply.send(result);
            }
            Command::RadioTrace { request, reply } => {
                let result = request
                    .as_core()
                    .map_err(|error| error.to_string())
                    .and_then(|request| {
                        self.engine
                            .store()
                            .rf_trace(request)
                            .map(RadioTracePageView::from)
                            .map_err(|error| error.to_string())
                    });
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
                    .store_mut()
                    .commit_outbound_with_location(material, self.phone_location)
                    .map_err(|error| error.to_string());
                if result.is_ok() {
                    self.next_reconcile = Instant::now();
                    self.urgent_reconcile_outbox =
                        result.as_ref().ok().map(|outcome| outcome.outbox_id());
                    self.urgent_reconcile_due = self.urgent_reconcile_outbox.is_some();
                    // Reserving the serialized device lane is useful only
                    // after the board accepts this row. Before that first
                    // submission turn there may be no session at all, and an
                    // offline durable enqueue must not block unrelated device
                    // commands or spin the actor while reconnecting.
                    self.urgent_reconcile_reserves_lane = false;
                    self.refresh_counts();
                    self.publish();
                }
                let _ = reply.send(result);
            }
            Command::RetrySend {
                outbox_id,
                idempotency_key,
                reply,
            } => {
                let result = self
                    .engine
                    .store_mut()
                    .retry_outbox_with_location(outbox_id, idempotency_key, self.phone_location)
                    .map_err(|error| error.to_string());
                if result.is_ok() {
                    self.next_reconcile = Instant::now();
                    self.urgent_reconcile_outbox =
                        result.as_ref().ok().map(|outcome| match outcome {
                            OutboxRetryOutcome::Requeued(outbox_id)
                            | OutboxRetryOutcome::AlreadyPending(outbox_id) => *outbox_id,
                        });
                    self.urgent_reconcile_due = self.urgent_reconcile_outbox.is_some();
                    self.urgent_reconcile_reserves_lane = false;
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

        match self.select_background_work(now) {
            Some(BackgroundWork::Inbox) => self.inbox_turn(),
            Some(BackgroundWork::RadioTrace) => self.radio_trace_turn(),
            Some(BackgroundWork::Reconcile) => self.reconcile_turn(),
            None => {}
        }
    }

    fn select_background_work(&mut self, now: Instant) -> Option<BackgroundWork> {
        let inbox_due = now >= self.next_inbox;
        let reconcile_due = now >= self.next_reconcile;
        let radio_trace_due = now >= self.next_radio_trace;
        if self.urgent_reconcile_outbox.is_some() && self.urgent_reconcile_due {
            if reconcile_due {
                self.radio_trace_chat_yields_remaining =
                    self.radio_trace_chat_yields_remaining.saturating_sub(1);
                return Some(BackgroundWork::Reconcile);
            }
            if self.urgent_reconcile_reserves_lane {
                // The short submit-to-first-status gap is deliberate. Do not
                // start a multi-page device request that can run well beyond
                // that deadline before the exact accepted row is projected.
                return None;
            }
        }
        if radio_trace_due
            && (self.radio_trace_chat_yields_remaining == 0 || (!inbox_due && !reconcile_due))
        {
            self.radio_trace_chat_yields_remaining = RADIO_TRACE_CHAT_YIELD_TURNS;
            return Some(BackgroundWork::RadioTrace);
        }
        if !inbox_due && !reconcile_due {
            return None;
        }
        self.radio_trace_chat_yields_remaining =
            self.radio_trace_chat_yields_remaining.saturating_sub(1);
        let run_inbox = inbox_due && (!reconcile_due || self.prefer_inbox);
        self.prefer_inbox = !run_inbox;
        Some(if run_inbox {
            BackgroundWork::Inbox
        } else {
            BackgroundWork::Reconcile
        })
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
        self.next_radio_trace = Instant::now();
        // A newly usable session first imports chat and hands off any durable
        // outbox work. Trace catch-up remains bounded, but must not preempt the
        // first useful foreground-visible synchronization turn.
        self.radio_trace_chat_yields_remaining = RADIO_TRACE_CHAT_YIELD_TURNS;
        if self.urgent_reconcile_outbox.is_some() {
            self.urgent_reconcile_due = true;
        }
        self.radio_trace_cursor = None;
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
        let preferred_outbox = self
            .urgent_reconcile_due
            .then_some(self.urgent_reconcile_outbox)
            .flatten();
        let step = match preferred_outbox {
            Some(outbox_id) => self
                .engine
                .reconcile_outbox_step(session.as_mut(), outbox_id),
            None => match self.urgent_reconcile_outbox {
                Some(outbox_id) => self
                    .engine
                    .reconcile_step_avoiding(session.as_mut(), outbox_id),
                None => self.engine.reconcile_step(session.as_mut()),
            },
        };
        match step {
            Ok(ReconcileStep::Idle) => {
                self.urgent_reconcile_outbox = None;
                self.urgent_reconcile_due = false;
                self.urgent_reconcile_reserves_lane = false;
                self.next_reconcile = Instant::now() + self.config.status_poll_interval;
                if self.clear_background_error(BackgroundWork::Reconcile) {
                    self.publish();
                }
            }
            Ok(ReconcileStep::Submitted { outbox_id, .. }) => {
                if let Some(preferred) = preferred_outbox {
                    if preferred == outbox_id {
                        self.urgent_reconcile_outbox = Some(outbox_id);
                        self.urgent_reconcile_due = true;
                        self.urgent_reconcile_reserves_lane = true;
                    } else {
                        // The preferred row disappeared and ordinary fallback
                        // returned a different row. Do not transfer UI urgency.
                        self.urgent_reconcile_outbox = None;
                        self.urgent_reconcile_due = false;
                        self.urgent_reconcile_reserves_lane = false;
                    }
                } else if self.urgent_reconcile_outbox.is_none()
                    || self.urgent_reconcile_outbox == Some(outbox_id)
                {
                    self.urgent_reconcile_outbox = Some(outbox_id);
                    self.urgent_reconcile_due = true;
                    self.urgent_reconcile_reserves_lane = true;
                } else {
                    // A fairness turn submitted an older row. Preserve the
                    // newer hot row, but reserve no long device-I/O gap.
                    self.urgent_reconcile_due = true;
                    self.urgent_reconcile_reserves_lane = false;
                }
                self.next_reconcile = Instant::now() + self.config.operation_gap;
                self.clear_background_error(BackgroundWork::Reconcile);
                self.refresh_counts();
                self.publish();
            }
            Ok(ReconcileStep::Refreshed {
                outbox_id,
                state,
                outcome,
                ..
            }) => {
                if preferred_outbox.is_some_and(|preferred| preferred != outbox_id) {
                    // The preferred row disappeared from reconcile work (for
                    // example, it became terminal through an idempotent replay).
                    self.urgent_reconcile_outbox = None;
                    self.urgent_reconcile_due = false;
                    self.urgent_reconcile_reserves_lane = false;
                } else if self.urgent_reconcile_outbox == Some(outbox_id) {
                    if state.is_terminal() {
                        self.urgent_reconcile_outbox = None;
                        self.urgent_reconcile_due = false;
                        self.urgent_reconcile_reserves_lane = false;
                    } else {
                        // One ordinary round-robin row gets the next turn. If
                        // this is the only row, the avoiding selector returns
                        // it again at the normal status cadence without an
                        // additional exact-priority turn.
                        self.urgent_reconcile_due = false;
                        self.urgent_reconcile_reserves_lane = false;
                    }
                } else if self.urgent_reconcile_outbox.is_some() {
                    // The fairness turn completed. Re-arm the hot row without
                    // resetting the engine's independent round-robin cursor.
                    self.urgent_reconcile_due = true;
                    self.urgent_reconcile_reserves_lane = false;
                }
                // Advance promptly after a terminal row, but do not flood the
                // serialized BLE session while board-owned delivery is active.
                self.next_reconcile = Instant::now() + reconcile_refresh_delay(&self.config, state);
                if state.is_terminal() {
                    self.next_radio_trace = Instant::now();
                }
                let cleared = self.clear_background_error(BackgroundWork::Reconcile);
                if outcome == StatusProjectionOutcome::Advanced || cleared {
                    self.refresh_counts();
                    self.publish();
                }
            }
            Err(error) => self.background_error(BackgroundWork::Reconcile, error),
        }
    }

    fn inbox_turn(&mut self) {
        let receiver_location = match self.phone_location {
            AttemptLocationStamp::Available(sample) => Some(sample),
            AttemptLocationStamp::Unavailable(_) => None,
        };
        let session = self.session.as_mut().expect("ready actor has a session");
        match self
            .engine
            .inbox_step_with_receiver_location(session.as_mut(), receiver_location)
        {
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
                self.next_radio_trace = Instant::now();
                if outcome == InboundCommitOutcome::Inserted {
                    self.snapshot.imported_this_run =
                        self.snapshot.imported_this_run.saturating_add(1);
                }
                self.clear_background_error(BackgroundWork::Inbox);
                self.publish();
            }
            Ok(InboxStep::Acknowledged { .. }) => {
                self.next_inbox = Instant::now() + self.config.inbox_poll_interval;
                if self.clear_background_error(BackgroundWork::Inbox) {
                    self.publish();
                }
            }
            Err(error) => self.background_error(BackgroundWork::Inbox, error),
        }
    }

    fn radio_trace_turn(&mut self) {
        let request = reticulum_device_api::RadioTracePageRequest::new(self.radio_trace_cursor);
        let page = {
            let session = self.session.as_mut().expect("ready actor has a session");
            session.radio_trace_page(request)
        };
        let page = match page {
            Ok(page) => page,
            Err(error) => {
                let usable = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.is_usable());
                if !usable {
                    self.retry_connection(error.to_string());
                    return;
                }
                self.background_error_work = Some(BackgroundWork::RadioTrace);
                self.snapshot.last_error = Some(format!("radio trace sync failed: {error}"));
                self.next_radio_trace = Instant::now() + self.config.capacity_backoff;
                self.publish();
                return;
            }
        };
        let imported_at_unix_ms = current_unix_timestamp_millis();
        let import =
            radio_trace::import_device_page(self.engine.store_mut(), page, imported_at_unix_ms);
        let outcome = match import {
            Ok(outcome) => outcome,
            Err(error) => {
                self.permanent_fault(format!("could not persist radio trace: {error}"));
                return;
            }
        };

        let previous = self.radio_trace_cursor;
        let last = page.entries().iter().flatten().last().copied();
        self.radio_trace_cursor = match last {
            Some(event) => Some(reticulum_device_api::RadioTraceCursor::new(
                page.boot_id(),
                event.sequence(),
            )),
            None if previous.is_some_and(|cursor| cursor.boot_id() == page.boot_id()) => previous,
            None => None,
        };
        self.next_radio_trace = if page.next_cursor().is_some() {
            Instant::now() + self.config.operation_gap
        } else {
            Instant::now() + self.config.radio_trace_idle_interval
        };
        let cleared = self.clear_background_error(BackgroundWork::RadioTrace);
        if outcome.inserted() > 0 || outcome.correlations_added() > 0 || cleared {
            self.publish();
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
                if work == BackgroundWork::Reconcile {
                    // Lane reservation only covers the short accepted-submit
                    // to first-status gap. Any retry/backoff must allow inbox
                    // and trace work to continue while the row remains hot.
                    self.urgent_reconcile_reserves_lane = false;
                }
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
                    }) => match background_api_retry_delay(&self.config, response.code) {
                        Some(delay) => {
                            if work == BackgroundWork::Reconcile
                                && self.urgent_reconcile_outbox.is_some()
                            {
                                // A fairness turn may have failed while the
                                // UI-hot row was deliberately not due. Re-arm
                                // it after the shared backoff so repeated
                                // pressure cannot suppress that row forever.
                                self.urgent_reconcile_due = true;
                            }
                            self.background_error_work = Some(work);
                            self.snapshot.last_error = Some(error.to_string());
                            let retry_at = Instant::now() + delay;
                            match work {
                                BackgroundWork::Inbox => self.next_inbox = retry_at,
                                BackgroundWork::RadioTrace => self.next_radio_trace = retry_at,
                                BackgroundWork::Reconcile => self.next_reconcile = retry_at,
                            }
                            self.publish();
                        }
                        None => self.permanent_fault(error.to_string()),
                    },
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
        self.radio_trace_cursor = None;
        self.radio_trace_chat_yields_remaining = 0;
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

    fn empty_radio_trace_page() -> reticulum_device_api::RadioTracePage {
        let profile = reticulum_device_api::RadioTraceAppliedLoraProfile::new(
            [0x51; 16],
            915_000_000,
            125_000,
            8,
            22,
            10,
            5,
            true,
            true,
            false,
        );
        reticulum_device_api::RadioTracePage::new(1, profile, 1, 1, false, [None, None, None], None)
            .unwrap()
    }

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

    fn message_location(latitude_e6: i32, updated_at_unix_seconds: u32) -> MessageLocation {
        MessageLocation::new(
            latitude_e6,
            -72_345_678,
            12_345,
            678,
            27_050,
            950,
            updated_at_unix_seconds,
        )
        .unwrap()
    }

    fn phone_location(latitude_e6: i32, captured_at_unix_ms: u64) -> PhoneLocationObservationView {
        PhoneLocationObservationView::Available {
            latitude_e6,
            longitude_e6: -72_345_678,
            altitude_mm: Some(12_345),
            horizontal_accuracy_mm: Some(5_500),
            vertical_accuracy_mm: Some(8_500),
            captured_at_unix_ms,
            authorization: PhoneLocationAuthorizationView::Precise,
            source: PhoneLocationSourceView::ForegroundStream,
            mocked: Some(false),
        }
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
        assert_eq!(material.location(), None);

        let located_send: SendRequest = serde_json::from_value(serde_json::json!({
            "destination": "cd".repeat(16),
            "timestamp_ms": 1_235,
            "idempotency_key": "ee".repeat(16),
            "title": "located",
            "content": "mesh",
            "location": {
                "latitude_e6": 43_123_456,
                "longitude_e6": -72_345_678,
                "altitude_cm": 12_345,
                "speed_cm_per_second": 678,
                "bearing_centidegrees": 27_050,
                "accuracy_cm": 950,
                "updated_at_unix_seconds": 1_784_000_001,
            },
        }))
        .unwrap();
        assert_eq!(
            located_send.into_material().unwrap().location(),
            Some(message_location(43_123_456, 1_784_000_001))
        );

        let invalid_location: SendRequest = serde_json::from_value(serde_json::json!({
            "destination": "cd".repeat(16),
            "timestamp_ms": 1_236,
            "idempotency_key": "ed".repeat(16),
            "title": "invalid",
            "content": "mesh",
            "location": {
                "latitude_e6": 90_000_001,
                "longitude_e6": 0,
                "altitude_cm": 0,
                "speed_cm_per_second": 0,
                "bearing_centidegrees": 0,
                "accuracy_cm": 0,
                "updated_at_unix_seconds": 0,
            },
        }))
        .unwrap();
        assert_eq!(
            invalid_location.into_material(),
            Err(ClientRequestError::InvalidMessageLocation)
        );

        let located_at_direct_limit: SendRequest = serde_json::from_value(serde_json::json!({
            "destination": "cd".repeat(16),
            "timestamp_ms": 1_237,
            "idempotency_key": "ec".repeat(16),
            "title": "",
            "content": "x".repeat(268),
            "location": {
                "latitude_e6": 43_123_456,
                "longitude_e6": -72_345_678,
                "altitude_cm": 12_345,
                "speed_cm_per_second": 678,
                "bearing_centidegrees": 27_050,
                "accuracy_cm": 950,
                "updated_at_unix_seconds": 1_784_000_001,
            },
        }))
        .unwrap();
        assert!(located_at_direct_limit.into_material().is_ok());

        let located_over_direct_limit: SendRequest = serde_json::from_value(serde_json::json!({
            "destination": "cd".repeat(16),
            "timestamp_ms": 1_238,
            "idempotency_key": "eb".repeat(16),
            "title": "",
            "content": "x".repeat(269),
            "location": {
                "latitude_e6": 43_123_456,
                "longitude_e6": -72_345_678,
                "altitude_cm": 12_345,
                "speed_cm_per_second": 678,
                "bearing_centidegrees": 27_050,
                "accuracy_cm": 950,
                "updated_at_unix_seconds": 1_784_000_001,
            },
        }))
        .unwrap();
        assert_eq!(
            located_over_direct_limit.into_material(),
            Err(ClientRequestError::MessageTooLarge)
        );

        let combined_unlocated_over_direct_limit: SendRequest =
            serde_json::from_value(serde_json::json!({
                "destination": "cd".repeat(16),
                "timestamp_ms": 1_239,
                "idempotency_key": "ea".repeat(16),
                "title": "t".repeat(MAX_LXMF_BASIC_TITLE_BYTES),
                "content": "c".repeat(MAX_LXMF_BASIC_CONTENT_BYTES),
            }))
            .unwrap();
        assert_eq!(
            combined_unlocated_over_direct_limit.into_material(),
            Err(ClientRequestError::MessageTooLarge)
        );
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

    #[test]
    fn timeline_view_projects_a_persisted_message_location() {
        let location = message_location(44_654_321, 1_784_000_002);
        let mut store = SqliteChatStore::open_in_memory().unwrap();
        store
            .commit_outbound(outbound(0x31).with_location(Some(location)))
            .unwrap();
        let entry = store
            .conversation_timeline(destination(0x31))
            .unwrap()
            .pop()
            .unwrap();
        let view = TimelineView::from(entry);

        assert_eq!(view.location(), Some(MessageLocationView::from(location)));
        assert_eq!(
            serde_json::to_value(view).unwrap()["location"],
            serde_json::json!({
                "latitude_e6": 44_654_321,
                "longitude_e6": -72_345_678,
                "altitude_cm": 12_345,
                "speed_cm_per_second": 678,
                "bearing_centidegrees": 27_050,
                "accuracy_cm": 950,
                "updated_at_unix_seconds": 1_784_000_002,
            })
        );
    }

    struct FakeSession {
        binding: DeviceBinding,
        inbox: Vec<(InboxSummary, InboundMessage)>,
        submitted: Arc<Mutex<Vec<OutboxMaterial>>>,
        status: SubmissionState,
        nearby_generation: Option<Arc<AtomicU64>>,
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
            Ok(AcceptanceIds::new(
                id,
                MessageId::new([material.content()[0]; 32]),
            ))
        }

        fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
            Ok(self.status)
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

        fn next_nearby_peer(
            &mut self,
            _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
        ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
            let generation = self
                .nearby_generation
                .as_ref()
                .map(|generation| generation.load(Ordering::Relaxed))
                .expect("ordinary actor tests do not request nearby peers");
            let generation = reticulum_device_api::LxmfPeerGeneration::new(generation)
                .expect("test nearby generations are nonzero");
            let incarnation = reticulum_device_api::LxmfPeerDiscoveryIncarnation::new([0x7a; 8]);
            Ok(reticulum_device_api::LxmfPeerDiscoveryPage::new(
                reticulum_device_api::LxmfPeerDiscoveryCursor::new(incarnation, generation.get()),
                Some(generation),
                None,
                false,
                None,
            ))
        }

        fn nomad_fetch_start(
            &mut self,
            _request: reticulum_device_api::NomadFetchStartRequest<'_>,
        ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
            unreachable!("ordinary actor tests do not request NomadNet fetches")
        }

        fn nomad_fetch_poll(
            &mut self,
            _id: reticulum_device_api::NomadFetchId,
        ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
            unreachable!("ordinary actor tests do not request NomadNet fetches")
        }

        fn reticulum_probe_start(
            &mut self,
            _request: reticulum_device_api::ProbeStartRequest,
        ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
            unreachable!("ordinary actor tests do not request Reticulum probes")
        }

        fn reticulum_probe_poll(
            &mut self,
            _id: reticulum_device_api::ProbeId,
        ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
            unreachable!("ordinary actor tests do not request Reticulum probes")
        }

        fn network_config_get(
            &mut self,
        ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
            unreachable!("ordinary actor tests do not request network configuration")
        }

        fn network_config_mutate(
            &mut self,
            _request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
        ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
            unreachable!("ordinary actor tests do not mutate network configuration")
        }

        fn network_status(
            &mut self,
        ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
            unreachable!("ordinary actor tests do not request network status")
        }

        fn manual_service_announce(
            &mut self,
        ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
            unreachable!("ordinary actor tests do not request service announces")
        }

        fn node_diagnostics(
            &mut self,
        ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
            unreachable!("ordinary actor tests do not request node diagnostics")
        }

        fn route_diagnostics_page(
            &mut self,
            _request: reticulum_device_api::RouteDiagnosticsRequest,
        ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
            unreachable!("ordinary actor tests do not request route diagnostics")
        }

        fn radio_trace_page(
            &mut self,
            _request: reticulum_device_api::RadioTracePageRequest,
        ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
            Ok(empty_radio_trace_page())
        }

        fn is_usable(&self) -> bool {
            true
        }
    }

    struct FakeConnector {
        outcomes: VecDeque<ConnectOutcome>,
        attempts: Arc<AtomicUsize>,
        submitted: Arc<Mutex<Vec<OutboxMaterial>>>,
        status: SubmissionState,
        nearby_generation: Option<Arc<AtomicU64>>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ObservedNomadStart {
        destination: [u8; 16],
        path: String,
        timestamp_unix_ms: u64,
        idempotency_key: [u8; 16],
    }

    #[derive(Default)]
    struct NomadTrace {
        starts: Vec<ObservedNomadStart>,
        polls: Vec<reticulum_device_api::NomadFetchId>,
        probe_starts: Vec<ObservedProbeStart>,
        probe_polls: Vec<reticulum_device_api::ProbeId>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ObservedProbeStart {
        destination: [u8; 16],
        idempotency_key: [u8; 16],
    }

    struct NomadSession {
        trace: Arc<Mutex<NomadTrace>>,
    }

    impl LxmfSession for NomadSession {
        type Error = DeviceSessionError;

        fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
            Ok(binding(0x81))
        }

        fn submit(&mut self, _material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
            unreachable!("the Nomad actor test has no outbox work")
        }

        fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
            unreachable!("the Nomad actor test has no accepted submissions")
        }

        fn next_inbox(
            &mut self,
            _after: Option<InboxCursor>,
        ) -> Result<Option<InboxSummary>, Self::Error> {
            Ok(None)
        }

        fn read_inbox(&mut self, _summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
            unreachable!("the Nomad actor test has no inbox messages")
        }

        fn next_nearby_peer(
            &mut self,
            _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
        ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
            unreachable!("the Nomad actor test does not request nearby peers")
        }

        fn nomad_fetch_start(
            &mut self,
            request: reticulum_device_api::NomadFetchStartRequest<'_>,
        ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
            self.trace.lock().unwrap().starts.push(ObservedNomadStart {
                destination: request.destination().0,
                path: request.path().as_str().to_owned(),
                timestamp_unix_ms: request.timestamp_unix_ms().get(),
                idempotency_key: request.idempotency_key().0,
            });
            Ok(reticulum_device_api::NomadFetchStartAccepted {
                id: reticulum_device_api::NomadFetchId::new([0x82; 8], 1).unwrap(),
                outcome: reticulum_device_api::NomadFetchStartOutcome::Accepted,
            })
        }

        fn nomad_fetch_poll(
            &mut self,
            id: reticulum_device_api::NomadFetchId,
        ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
            self.trace.lock().unwrap().polls.push(id);
            Ok(reticulum_device_api::NomadFetchPollResponse::Ready(
                reticulum_device_api::NomadPage::new(b">Metalbeard").unwrap(),
            ))
        }

        fn reticulum_probe_start(
            &mut self,
            request: reticulum_device_api::ProbeStartRequest,
        ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
            self.trace
                .lock()
                .unwrap()
                .probe_starts
                .push(ObservedProbeStart {
                    destination: request.destination().0,
                    idempotency_key: request.idempotency_key().0,
                });
            Ok(reticulum_device_api::ProbeStartAccepted::new(
                reticulum_device_api::ProbeId::new([0x85; 16]).unwrap(),
                reticulum_device_api::ProbeStartOutcome::Accepted,
            ))
        }

        fn reticulum_probe_poll(
            &mut self,
            id: reticulum_device_api::ProbeId,
        ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
            self.trace.lock().unwrap().probe_polls.push(id);
            Ok(reticulum_device_api::ProbePollResponse::Succeeded(
                reticulum_device_api::ProbeSuccess::new(
                    1_234,
                    2,
                    reticulum_device_api::IngressObservation::new(
                        7,
                        Some(reticulum_device_api::IngressSignal::new(-91, 7)),
                    ),
                ),
            ))
        }

        fn network_config_get(
            &mut self,
        ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
            unreachable!("the Nomad actor test does not request network configuration")
        }

        fn network_config_mutate(
            &mut self,
            _request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
        ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
            unreachable!("the Nomad actor test does not mutate network configuration")
        }

        fn network_status(
            &mut self,
        ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
            unreachable!("the Nomad actor test does not request network status")
        }

        fn manual_service_announce(
            &mut self,
        ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
            unreachable!("the Nomad actor test does not request service announces")
        }

        fn node_diagnostics(
            &mut self,
        ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
            unreachable!("the Nomad actor test does not request node diagnostics")
        }

        fn route_diagnostics_page(
            &mut self,
            _request: reticulum_device_api::RouteDiagnosticsRequest,
        ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
            unreachable!("the Nomad actor test does not request route diagnostics")
        }

        fn radio_trace_page(
            &mut self,
            _request: reticulum_device_api::RadioTracePageRequest,
        ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
            Ok(empty_radio_trace_page())
        }

        fn is_usable(&self) -> bool {
            true
        }
    }

    struct NomadConnector {
        trace: Arc<Mutex<NomadTrace>>,
        connected: bool,
    }

    impl Connector for NomadConnector {
        fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
            if self.connected {
                return Err(ConnectFailure::retryable(
                    "test session was already claimed",
                ));
            }
            self.connected = true;
            Ok(ConnectedSession::new(
                NomadSession {
                    trace: self.trace.clone(),
                },
                ConnectionMetadata::usb_serial("/dev/fake", "001122334455"),
            ))
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ObservedNetworkMutation {
        profile_id: [u8; 16],
        enabled: bool,
        priority: u8,
        ssid: Vec<u8>,
        replacement_passphrase_len: Option<usize>,
        expected_revision: u64,
        idempotency_key: [u8; 16],
    }

    #[derive(Default)]
    struct NetworkTrace {
        announces: usize,
        config_reads: usize,
        status_reads: usize,
        diagnostics_reads: usize,
        route_reads: usize,
        mutations: Vec<ObservedNetworkMutation>,
    }

    struct NetworkSession {
        trace: Arc<Mutex<NetworkTrace>>,
    }

    impl LxmfSession for NetworkSession {
        type Error = DeviceSessionError;

        fn binding(&mut self) -> Result<DeviceBinding, Self::Error> {
            Ok(binding(0x91))
        }

        fn submit(&mut self, _material: &OutboxMaterial) -> Result<AcceptanceIds, Self::Error> {
            unreachable!("the network actor test has no outbox work")
        }

        fn submission_status(&mut self, _id: SubmissionId) -> Result<SubmissionState, Self::Error> {
            unreachable!("the network actor test has no accepted submissions")
        }

        fn next_inbox(
            &mut self,
            _after: Option<InboxCursor>,
        ) -> Result<Option<InboxSummary>, Self::Error> {
            Ok(None)
        }

        fn read_inbox(&mut self, _summary: InboxSummary) -> Result<InboundMessage, Self::Error> {
            unreachable!("the network actor test has no inbox messages")
        }

        fn next_nearby_peer(
            &mut self,
            _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
        ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
            unreachable!("the network actor test does not request nearby peers")
        }

        fn nomad_fetch_start(
            &mut self,
            _request: reticulum_device_api::NomadFetchStartRequest<'_>,
        ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
            unreachable!("the network actor test does not request NomadNet pages")
        }

        fn nomad_fetch_poll(
            &mut self,
            _id: reticulum_device_api::NomadFetchId,
        ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
            unreachable!("the network actor test does not request NomadNet pages")
        }

        fn reticulum_probe_start(
            &mut self,
            _request: reticulum_device_api::ProbeStartRequest,
        ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
            unreachable!("the network actor test does not request Reticulum probes")
        }

        fn reticulum_probe_poll(
            &mut self,
            _id: reticulum_device_api::ProbeId,
        ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
            unreachable!("the network actor test does not request Reticulum probes")
        }

        fn network_config_get(
            &mut self,
        ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
            self.trace.lock().unwrap().config_reads += 1;
            let profile_id = reticulum_device_api::WifiNetworkProfileId::new([0x92; 16]).unwrap();
            let profile = reticulum_device_api::WifiNetworkConfigSummary::new(
                profile_id,
                true,
                220,
                b"mesh\xff",
                true,
            )
            .unwrap();
            Ok(reticulum_device_api::NetworkConfigSnapshot::new(
                5,
                [Some(profile), None, None, None],
                None,
            )
            .unwrap())
        }

        fn network_config_mutate(
            &mut self,
            request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
        ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
            let reticulum_device_api::NetworkConfigMutation::UpsertWifi {
                profile_id,
                network,
            } = request.mutation()
            else {
                unreachable!("the network actor test submits a Wi-Fi upsert")
            };
            self.trace
                .lock()
                .unwrap()
                .mutations
                .push(ObservedNetworkMutation {
                    profile_id: *profile_id.as_bytes(),
                    enabled: network.enabled(),
                    priority: network.priority(),
                    ssid: network.ssid().as_bytes().to_vec(),
                    replacement_passphrase_len: network.credential().replacement().map(<[u8]>::len),
                    expected_revision: request.expected_revision(),
                    idempotency_key: request.idempotency_key().0,
                });
            Ok(
                reticulum_device_api::NetworkConfigMutationOutcome::Applied {
                    revision: 6,
                    reboot_required: true,
                },
            )
        }

        fn network_status(
            &mut self,
        ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
            self.trace.lock().unwrap().status_reads += 1;
            Ok(reticulum_device_api::NetworkRuntimeStatus::new(
                6,
                5,
                reticulum_device_api::WifiStationState::Connected,
                Some(reticulum_device_api::WifiNetworkProfileId::new([0x92; 16]).unwrap()),
                Some(b"mesh\xff"),
                Some([192, 0, 2, 33]),
                Some(-68),
                reticulum_device_api::ReticulumTcpPeerState::WaitingForNetwork,
            )
            .unwrap())
        }

        fn manual_service_announce(
            &mut self,
        ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
            self.trace.lock().unwrap().announces += 1;
            Ok(reticulum_device_api::ManualServiceAnnounceDisposition::Queued)
        }

        fn node_diagnostics(
            &mut self,
        ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
            self.trace.lock().unwrap().diagnostics_reads += 1;
            Ok(reticulum_device_api::NodeDiagnosticsSnapshot::new(
                4_500,
                [
                    Some(reticulum_device_api::DiagnosticInterfaceRecord::new(
                        1,
                        reticulum_device_api::DiagnosticInterfaceKind::LoRa,
                        reticulum_device_api::DiagnosticInterfaceState::Online,
                        2,
                        500,
                        Some(5_470),
                    )),
                    None,
                    None,
                    None,
                ],
                Some(reticulum_device_api::LoraDiagnostics::new(
                    22,
                    915_000_000,
                    125_000,
                    7,
                    5,
                    8,
                    7,
                    1,
                    0,
                    3,
                    2,
                    5,
                    1,
                    0,
                    4,
                    9,
                    Some(reticulum_device_api::DiagnosticLoraLastRx::new(500, -91, 7)),
                    Some(reticulum_device_api::DiagnosticLoraLastTx::data(
                        700,
                        reticulum_device_api::DiagnosticLoraTxOutcome::Completed,
                        reticulum_device_api::DiagnosticLoraDataTxEvidence::try_new(
                            1,
                            183,
                            reticulum_device_api::EncodedPacketSha256::new([0xab; 32]),
                        )
                        .unwrap(),
                    )),
                    None,
                )),
                reticulum_device_api::RnsDiagnostics::new(9, 2, 1, 0, 4, 1, 0, 0, 0, 0),
                3,
                1,
                1,
            ))
        }

        fn route_diagnostics_page(
            &mut self,
            request: reticulum_device_api::RouteDiagnosticsRequest,
        ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
            self.trace.lock().unwrap().route_reads += 1;
            assert_eq!(request.after(), None);
            let route = reticulum_device_api::RouteDiagnosticEntry::new(
                reticulum_device_api::DestinationHash([0x95; 16]),
                Some(reticulum_device_api::IdentityHash::new([0x96; 16])),
                2,
                Some(1),
                reticulum_device_api::RouteDiagnosticResolution::ExactReady,
                Some(1_200),
                Some(400),
                Some(28_800),
            );
            Ok(reticulum_device_api::RouteDiagnosticsPage::new(
                1,
                1,
                [Some(route), None, None, None],
                None,
            )
            .unwrap())
        }

        fn radio_trace_page(
            &mut self,
            _request: reticulum_device_api::RadioTracePageRequest,
        ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
            Ok(empty_radio_trace_page())
        }

        fn is_usable(&self) -> bool {
            true
        }
    }

    struct NetworkConnector {
        trace: Arc<Mutex<NetworkTrace>>,
        connected: bool,
    }

    impl Connector for NetworkConnector {
        fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
            if self.connected {
                return Err(ConnectFailure::retryable(
                    "test session was already claimed",
                ));
            }
            self.connected = true;
            Ok(ConnectedSession::new(
                NetworkSession {
                    trace: self.trace.clone(),
                },
                ConnectionMetadata::usb_serial("/dev/fake", "001122334455"),
            ))
        }
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
                    status: SubmissionState::Queued,
                    nearby_generation: None,
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

        fn next_nearby_peer(
            &mut self,
            _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
        ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn nomad_fetch_start(
            &mut self,
            _request: reticulum_device_api::NomadFetchStartRequest<'_>,
        ) -> Result<reticulum_device_api::NomadFetchStartAccepted, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn nomad_fetch_poll(
            &mut self,
            _id: reticulum_device_api::NomadFetchId,
        ) -> Result<reticulum_device_api::NomadFetchPollResponse, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn reticulum_probe_start(
            &mut self,
            _request: reticulum_device_api::ProbeStartRequest,
        ) -> Result<reticulum_device_api::ProbeStartAccepted, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn reticulum_probe_poll(
            &mut self,
            _id: reticulum_device_api::ProbeId,
        ) -> Result<reticulum_device_api::ProbePollResponse, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn network_config_get(
            &mut self,
        ) -> Result<reticulum_device_api::NetworkConfigSnapshot, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn network_config_mutate(
            &mut self,
            _request: reticulum_device_api::NetworkConfigMutationRequest<'_>,
        ) -> Result<reticulum_device_api::NetworkConfigMutationOutcome, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn network_status(
            &mut self,
        ) -> Result<reticulum_device_api::NetworkRuntimeStatus, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn manual_service_announce(
            &mut self,
        ) -> Result<reticulum_device_api::ManualServiceAnnounceDisposition, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn node_diagnostics(
            &mut self,
        ) -> Result<reticulum_device_api::NodeDiagnosticsSnapshot, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn route_diagnostics_page(
            &mut self,
            _request: reticulum_device_api::RouteDiagnosticsRequest,
        ) -> Result<reticulum_device_api::RouteDiagnosticsPage, Self::Error> {
            Err(DeviceSessionError::MissingLxmfDeliveryDestination)
        }

        fn radio_trace_page(
            &mut self,
            _request: reticulum_device_api::RadioTracePageRequest,
        ) -> Result<reticulum_device_api::RadioTracePage, Self::Error> {
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
                    status: self.status,
                    nearby_generation: self.nearby_generation.clone(),
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
        config.retry_later_backoff = Duration::from_millis(7);
        config
    }

    fn connected_fake_actor(
        config: ApplianceConfig,
        store: SqliteChatStore,
        inbox: FakeInbox,
    ) -> (Actor<FakeConnector>, Arc<Mutex<Vec<OutboxMaterial>>>) {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let connector = FakeConnector {
            outcomes: VecDeque::from([Ok((binding(0x28), inbox))]),
            attempts: Arc::new(AtomicUsize::new(0)),
            submitted: submitted.clone(),
            status: SubmissionState::Queued,
            nearby_generation: None,
        };
        let (_commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let initial = Arc::new(ApplianceSnapshot::starting());
        let published = Arc::new(RwLock::new(initial.clone()));
        let (revisions, _revision_rx) = watch::channel(initial.revision());
        let mut actor = Actor::new(
            config,
            ChatEngine::new(store),
            connector,
            receiver,
            published,
            revisions,
        );
        actor.connect();
        assert!(actor.session.is_some());
        (actor, submitted)
    }

    fn background_api_error(
        code: ApiErrorCode,
        operation: reticulum_device_client::Operation,
        wire_operation: u16,
    ) -> EngineError<SqliteStoreError, DeviceSessionError> {
        EngineError::Session(DeviceSessionError::Client(ClientError::Api {
            operation,
            error: reticulum_device_api::ApiErrorResponse {
                code,
                operation: Some(wire_operation),
            },
        }))
    }

    #[test]
    fn retry_later_and_structural_capacity_use_distinct_bounded_backoffs() {
        let database = TestDatabase::new("api-background-backoffs");
        let config = ApplianceConfig::new(database.0.clone());
        assert_eq!(config.retry_later_backoff, Duration::from_millis(250));
        assert_eq!(
            background_api_retry_delay(&config, ApiErrorCode::RetryLater),
            Some(config.retry_later_backoff)
        );
        assert_eq!(
            background_api_retry_delay(&config, ApiErrorCode::CapacityExhausted),
            Some(config.capacity_backoff)
        );
        assert_ne!(config.retry_later_backoff, config.capacity_backoff);
        assert_eq!(
            background_api_retry_delay(&config, ApiErrorCode::Internal),
            None
        );
        assert_eq!(
            background_api_retry_delay(&config, ApiErrorCode::CapabilityUnavailable),
            None
        );
    }

    #[test]
    fn retryable_reconcile_error_rearms_hot_row_without_dropping_the_session() {
        for (label, code, expected_delay) in [
            (
                "retry-later",
                ApiErrorCode::RetryLater,
                Duration::from_millis(7),
            ),
            (
                "capacity",
                ApiErrorCode::CapacityExhausted,
                Duration::from_millis(43),
            ),
        ] {
            let database = TestDatabase::new(label);
            let mut config = config(&database);
            config.capacity_backoff = Duration::from_millis(43);
            let store = SqliteChatStore::open(&database.0).unwrap();
            let (mut actor, _submitted) = connected_fake_actor(config, store, Vec::new());
            let hot = OutboxId::new(17).unwrap();
            actor.urgent_reconcile_outbox = Some(hot);
            actor.urgent_reconcile_due = false;
            actor.urgent_reconcile_reserves_lane = true;
            let unchanged_inbox = actor.next_inbox;
            let before = Instant::now();

            actor.background_error(
                BackgroundWork::Reconcile,
                background_api_error(
                    code,
                    reticulum_device_client::Operation::SubmissionStatus,
                    reticulum_device_api::OP_SUBMISSION_STATUS,
                ),
            );
            let after = Instant::now();

            assert!(actor.session.is_some());
            assert!(!actor.permanent_fault);
            assert!(matches!(
                actor.snapshot.connection(),
                ConnectionState::Ready { .. }
            ));
            assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
            assert!(actor.urgent_reconcile_due);
            assert!(!actor.urgent_reconcile_reserves_lane);
            assert_eq!(actor.background_error_work, Some(BackgroundWork::Reconcile));
            assert_eq!(actor.next_inbox, unchanged_inbox);
            assert!(actor.next_reconcile >= before + expected_delay);
            assert!(actor.next_reconcile <= after + expected_delay);
        }
    }

    #[test]
    fn retry_later_delays_only_inbox_work_then_retries_on_the_same_session() {
        let database = TestDatabase::new("retry-later-inbox");
        let store = SqliteChatStore::open(&database.0).unwrap();
        let (mut actor, _submitted) = connected_fake_actor(config(&database), store, Vec::new());
        let unchanged_reconcile = actor.next_reconcile;
        let before = Instant::now();
        actor.background_error(
            BackgroundWork::Inbox,
            background_api_error(
                ApiErrorCode::RetryLater,
                reticulum_device_client::Operation::LxmfNext,
                reticulum_device_api::OP_EXPERIMENTAL_LXMF_NEXT,
            ),
        );
        let after = Instant::now();

        assert!(actor.session.is_some());
        assert!(!actor.permanent_fault);
        assert_eq!(actor.next_reconcile, unchanged_reconcile);
        assert!(actor.next_inbox >= before + Duration::from_millis(7));
        assert!(actor.next_inbox <= after + Duration::from_millis(7));
        assert_eq!(actor.background_error_work, Some(BackgroundWork::Inbox));

        actor.next_inbox = Instant::now();
        actor.inbox_turn();
        assert!(actor.session.is_some());
        assert_eq!(actor.background_error_work, None);
        assert_eq!(actor.snapshot.last_error(), None);
    }

    #[test]
    fn retry_later_reconcile_runs_the_same_durable_row_after_backoff() {
        let database = TestDatabase::new("retry-later-reconcile");
        let mut store = SqliteChatStore::open(&database.0).unwrap();
        let outcome = store.commit_outbound(outbound(0x29)).unwrap();
        let outbox_id = outcome.outbox_id();
        let (mut actor, submitted) = connected_fake_actor(config(&database), store, Vec::new());
        actor.urgent_reconcile_outbox = Some(outbox_id);
        actor.urgent_reconcile_due = false;
        actor.urgent_reconcile_reserves_lane = true;
        actor.background_error(
            BackgroundWork::Reconcile,
            background_api_error(
                ApiErrorCode::RetryLater,
                reticulum_device_client::Operation::LxmfBasicSend,
                reticulum_device_api::OP_EXPERIMENTAL_LXMF_BASIC_SEND,
            ),
        );

        actor.next_reconcile = Instant::now();
        actor.reconcile_turn();

        assert!(actor.session.is_some());
        assert!(!actor.permanent_fault);
        assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x29)]);
        assert_eq!(actor.background_error_work, None);
        assert_eq!(actor.snapshot.last_error(), None);
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

    #[test]
    fn foreground_turn_drains_commands_that_are_already_queued() {
        let database = TestDatabase::new("foreground-command-burst");
        let store = SqliteChatStore::open(&database.0).unwrap();
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let initial = Arc::new(ApplianceSnapshot::starting());
        let published = Arc::new(RwLock::new(initial.clone()));
        let (revisions, _revision_rx) = watch::channel(initial.revision());
        let mut actor = Actor::new(
            config(&database),
            ChatEngine::new(store),
            UnavailableConnector {
                attempts: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
            published,
            revisions,
        );
        let (contacts_reply, contacts) = oneshot::channel::<Result<Vec<ContactView>, String>>();
        let (peers_reply, peers) = oneshot::channel::<Result<Vec<ConversationPeerView>, String>>();
        let (timeline_reply, timeline) = oneshot::channel::<Result<Vec<TimelineView>, String>>();
        commands
            .send(Command::ConversationPeers(peers_reply))
            .unwrap();
        commands
            .send(Command::Timeline {
                peer: destination(0x44),
                reply: timeline_reply,
            })
            .unwrap();

        assert!(actor.foreground_turn(Command::Contacts(contacts_reply)));
        assert!(contacts.blocking_recv().unwrap().unwrap().is_empty());
        assert!(peers.blocking_recv().unwrap().unwrap().is_empty());
        assert!(timeline.blocking_recv().unwrap().unwrap().is_empty());
        assert!(matches!(
            actor.commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn foreground_turn_is_bounded_by_command_channel_capacity() {
        let database = TestDatabase::new("foreground-command-bound");
        let store = SqliteChatStore::open(&database.0).unwrap();
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let initial = Arc::new(ApplianceSnapshot::starting());
        let published = Arc::new(RwLock::new(initial.clone()));
        let (revisions, _revision_rx) = watch::channel(initial.revision());
        let mut actor = Actor::new(
            config(&database),
            ChatEngine::new(store),
            UnavailableConnector {
                attempts: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
            published,
            revisions,
        );
        let (first_reply, _first) = oneshot::channel();
        for _ in 0..COMMAND_CAPACITY {
            let (reply, _receive) = oneshot::channel();
            commands.send(Command::SyncNow(reply)).unwrap();
        }

        assert!(actor.foreground_turn(Command::SyncNow(first_reply)));
        assert!(matches!(actor.commands.try_recv(), Ok(Command::SyncNow(_))));
    }

    #[test]
    fn foreground_local_burst_defers_device_io_until_after_background_work() {
        let database = TestDatabase::new("foreground-device-boundary");
        let store = SqliteChatStore::open(&database.0).unwrap();
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let initial = Arc::new(ApplianceSnapshot::starting());
        let published = Arc::new(RwLock::new(initial.clone()));
        let (revisions, _revision_rx) = watch::channel(initial.revision());
        let mut actor = Actor::new(
            config(&database),
            ChatEngine::new(store),
            UnavailableConnector {
                attempts: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
            published,
            revisions,
        );
        let (device_reply, _device_result) = oneshot::channel();
        let (trailing_reply, _trailing_result) = oneshot::channel();
        commands.send(Command::NetworkStatus(device_reply)).unwrap();
        commands.send(Command::Contacts(trailing_reply)).unwrap();
        let (first_reply, first_result) = oneshot::channel();

        assert!(actor.foreground_turn(Command::Contacts(first_reply)));
        assert!(first_result.blocking_recv().unwrap().unwrap().is_empty());
        assert!(matches!(
            actor.deferred_foreground.as_ref(),
            Some(Command::NetworkStatus(_))
        ));
        assert!(matches!(
            actor.commands.try_recv(),
            Ok(Command::Contacts(_))
        ));
    }

    #[test]
    fn offline_enqueue_does_not_reserve_or_block_the_device_lane() {
        let database = TestDatabase::new("offline-enqueue-lane");
        let store = SqliteChatStore::open(&database.0).unwrap();
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let initial = Arc::new(ApplianceSnapshot::starting());
        let published = Arc::new(RwLock::new(initial.clone()));
        let (revisions, _revision_rx) = watch::channel(initial.revision());
        let mut actor = Actor::new(
            config(&database),
            ChatEngine::new(store),
            UnavailableConnector {
                attempts: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
            published,
            revisions,
        );
        let (device_reply, _device_result) = oneshot::channel();
        commands.send(Command::NetworkStatus(device_reply)).unwrap();
        let (send_reply, send_result) = oneshot::channel();

        assert!(actor.foreground_turn(Command::EnqueueSend {
            material: outbound(0x45),
            reply: send_reply,
        }));
        assert!(send_result.blocking_recv().unwrap().is_ok());
        assert!(actor.session.is_none());
        assert!(actor.urgent_reconcile_outbox.is_some());
        assert!(actor.urgent_reconcile_due);
        assert!(!actor.urgent_reconcile_reserves_lane);
        assert_eq!(actor.urgent_status_wait(Instant::now()), None);
        assert!(matches!(
            actor.deferred_foreground.as_ref(),
            Some(Command::NetworkStatus(_))
        ));
        assert!(
            !actor.command_blocked_by_urgent_status(actor.deferred_foreground.as_ref().unwrap())
        );
    }

    #[test]
    fn deferred_device_io_waits_through_the_hot_rows_first_status() {
        let database = TestDatabase::new("deferred-device-after-hot-status");
        let mut store = SqliteChatStore::open(&database.0).unwrap();
        let hot = store.commit_outbound(outbound(0x43)).unwrap().outbox_id();
        let (mut actor, submitted) = connected_fake_actor(config(&database), store, Vec::new());
        actor.urgent_reconcile_outbox = Some(hot);
        actor.urgent_reconcile_due = true;
        actor.urgent_reconcile_reserves_lane = false;
        actor.next_reconcile = Instant::now();

        // The first background turn submits the exact UI-hot row.
        actor.background_turn();
        assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x43)]);
        assert!(actor.urgent_reconcile_due);
        assert!(actor.urgent_reconcile_reserves_lane);

        let (device_reply, _device_result) = oneshot::channel();
        actor.deferred_foreground = Some(Command::NetworkStatus(device_reply));
        assert!(
            actor.command_blocked_by_urgent_status(actor.deferred_foreground.as_ref().unwrap())
        );
        assert!(actor.urgent_status_wait(Instant::now()).is_some());

        // Once the exact first status is projected, the deferred device RPC
        // may run on the following loop iteration.
        actor.next_reconcile = Instant::now();
        actor.background_turn();
        assert!(!actor.urgent_reconcile_reserves_lane);
        assert!(
            !actor.command_blocked_by_urgent_status(actor.deferred_foreground.as_ref().unwrap())
        );
    }

    #[test]
    fn urgent_send_reconciliation_precedes_due_inbox_and_radio_trace_work() {
        let database = TestDatabase::new("urgent-reconcile-order");
        let store = SqliteChatStore::open(&database.0).unwrap();
        let (_commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let initial = Arc::new(ApplianceSnapshot::starting());
        let published = Arc::new(RwLock::new(initial.clone()));
        let (revisions, _revision_rx) = watch::channel(initial.revision());
        let mut actor = Actor::new(
            config(&database),
            ChatEngine::new(store),
            UnavailableConnector {
                attempts: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
            published,
            revisions,
        );
        let now = Instant::now();
        actor.next_inbox = now;
        actor.next_reconcile = now;
        actor.next_radio_trace = now;
        actor.urgent_reconcile_outbox = Some(OutboxId::new(1).unwrap());
        actor.urgent_reconcile_due = true;
        actor.radio_trace_chat_yields_remaining = 0;
        actor.prefer_inbox = true;

        assert_eq!(
            actor.select_background_work(now),
            Some(BackgroundWork::Reconcile)
        );

        actor.next_reconcile = now + Duration::from_millis(10);
        actor.urgent_reconcile_reserves_lane = true;
        assert_eq!(actor.select_background_work(now), None);

        actor.urgent_reconcile_reserves_lane = false;
        assert_eq!(
            actor.select_background_work(now),
            Some(BackgroundWork::RadioTrace)
        );
    }

    #[test]
    fn hot_outbox_alternates_with_ordinary_fairness_without_starving_older_work() {
        let database = TestDatabase::new("hot-outbox-fairness");
        let mut store = SqliteChatStore::open(&database.0).unwrap();
        let older = store.commit_outbound(outbound(0x41)).unwrap().outbox_id();
        let hot = store.commit_outbound(outbound(0x42)).unwrap().outbox_id();
        assert_ne!(older, hot);
        let (_commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let initial = Arc::new(ApplianceSnapshot::starting());
        let published = Arc::new(RwLock::new(initial.clone()));
        let (revisions, _revision_rx) = watch::channel(initial.revision());
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let mut actor = Actor::new(
            config(&database),
            ChatEngine::new(store),
            UnavailableConnector {
                attempts: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
            published,
            revisions,
        );
        actor.session = Some(Box::new(FakeSession {
            binding: binding(0x71),
            inbox: Vec::new(),
            submitted: submitted.clone(),
            status: SubmissionState::Preparing,
            nearby_generation: None,
        }));
        actor.urgent_reconcile_outbox = Some(hot);
        actor.urgent_reconcile_due = true;
        actor.urgent_reconcile_reserves_lane = true;

        actor.reconcile_turn();
        assert_eq!(submitted.lock().unwrap().as_slice(), &[outbound(0x42)]);
        assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
        assert!(actor.urgent_reconcile_due);
        assert!(actor.urgent_reconcile_reserves_lane);

        actor.reconcile_turn();
        assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
        assert!(!actor.urgent_reconcile_due);
        assert!(!actor.urgent_reconcile_reserves_lane);

        actor.reconcile_turn();
        assert_eq!(
            submitted.lock().unwrap().as_slice(),
            &[outbound(0x42), outbound(0x41)]
        );
        assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
        assert!(actor.urgent_reconcile_due);
        assert!(!actor.urgent_reconcile_reserves_lane);

        actor.reconcile_turn();
        assert_eq!(actor.urgent_reconcile_outbox, Some(hot));
        assert!(!actor.urgent_reconcile_due);
        assert!(!actor.urgent_reconcile_reserves_lane);
    }

    #[test]
    fn idle_radio_trace_poll_is_slower_than_chat_status_polling() {
        let database = TestDatabase::new("radio-trace-idle-cadence");
        let config = ApplianceConfig::new(database.0.clone());
        assert_eq!(config.radio_trace_idle_interval, Duration::from_secs(5));
        assert!(config.radio_trace_idle_interval > config.status_poll_interval);
        assert!(config.radio_trace_idle_interval > config.inbox_poll_interval);
    }

    #[test]
    fn nonterminal_status_refresh_uses_status_cadence_not_operation_gap() {
        let database = TestDatabase::new("status-refresh-cadence");
        let config = ApplianceConfig::new(database.0.clone());
        assert_eq!(
            reconcile_refresh_delay(&config, SubmissionState::Preparing),
            config.status_poll_interval
        );
        assert_ne!(
            reconcile_refresh_delay(&config, SubmissionState::Preparing),
            config.operation_gap
        );
        assert_eq!(
            reconcile_refresh_delay(&config, SubmissionState::Cancelled),
            config.operation_gap
        );
    }

    #[test]
    fn newly_connected_chat_work_precedes_due_trace_catch_up() {
        let database = TestDatabase::new("connected-chat-before-trace");
        let store = SqliteChatStore::open(&database.0).unwrap();
        let (_commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let initial = Arc::new(ApplianceSnapshot::starting());
        let published = Arc::new(RwLock::new(initial.clone()));
        let (revisions, _revision_rx) = watch::channel(initial.revision());
        let mut actor = Actor::new(
            config(&database),
            ChatEngine::new(store),
            UnavailableConnector {
                attempts: Arc::new(AtomicUsize::new(0)),
            },
            receiver,
            published,
            revisions,
        );
        let now = Instant::now();
        actor.next_inbox = now;
        actor.next_reconcile = now;
        actor.next_radio_trace = now;
        actor.radio_trace_chat_yields_remaining = RADIO_TRACE_CHAT_YIELD_TURNS;

        assert_eq!(
            actor.select_background_work(now),
            Some(BackgroundWork::Reconcile)
        );
        assert_eq!(
            actor.select_background_work(now),
            Some(BackgroundWork::Inbox)
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
            status: SubmissionState::Queued,
            nearby_generation: None,
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
        assert!(handle.contacts().await.unwrap().is_empty());
        let unknown = handle
            .conversation_peers()
            .await
            .unwrap()
            .into_iter()
            .find(|peer| peer.destination() == hex::encode([0x31; 16]))
            .expect("imported unknown sender must be queryable");
        assert_eq!(unknown.name(), None);
        assert_eq!(unknown.message_count(), 1);
        assert_eq!(unknown.inbound_message_count(), 1);
        assert_eq!(
            unknown.last_message().unwrap().direction(),
            TimelineDirection::Inbound
        );

        handle
            .upsert_contact(Contact::new(destination(0x31), "Field sender"))
            .await
            .unwrap();
        handle
            .upsert_contact(Contact::new(destination(0x31), "Renamed sender"))
            .await
            .unwrap();
        let renamed = handle
            .conversation_peers()
            .await
            .unwrap()
            .into_iter()
            .find(|peer| peer.destination() == hex::encode([0x31; 16]))
            .expect("renamed sender must remain queryable");
        assert_eq!(renamed.name(), Some("Renamed sender"));
        assert_eq!(renamed.message_count(), 1);
        assert_eq!(renamed.inbound_message_count(), 1);
        assert_eq!(handle.timeline(destination(0x31)).await.unwrap().len(), 1);

        handle.shutdown_and_wait().await.unwrap();
        let reopened = SqliteChatStore::open(&database.0).unwrap();
        assert_eq!(reopened.device_binding().unwrap(), Some(expected_binding));
    }

    fn seed_terminal_outbox(database: &TestDatabase, marker: u8) -> OutboxId {
        let mut store = SqliteChatStore::open(&database.0).unwrap();
        let outbox_id = store.commit_outbound(outbound(marker)).unwrap().outbox_id();
        let acceptance = AcceptanceIds::new(
            SubmissionId::new(700 + u64::from(marker)).unwrap(),
            MessageId::new([marker; 32]),
        );
        store.record_acceptance(outbox_id, acceptance).unwrap();
        store
            .project_submission_status(
                acceptance.submission_id(),
                SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            )
            .unwrap();
        store.close().unwrap();
        outbox_id
    }

    #[tokio::test]
    async fn terminal_outbox_is_not_resubmitted_on_startup_sync_or_reconnect() {
        let database = TestDatabase::new("terminal-outbox-reconnect");
        let outbox_id = seed_terminal_outbox(&database, 0x42);
        let connection_attempts = Arc::new(AtomicUsize::new(0));
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let connector = FakeConnector {
            outcomes: VecDeque::from([
                Ok((binding(0x22), Vec::new())),
                Ok((binding(0x22), Vec::new())),
            ]),
            attempts: connection_attempts.clone(),
            submitted: submitted.clone(),
            status: SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            nearby_generation: None,
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();

        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
        })
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(submitted.lock().unwrap().is_empty());
        assert_eq!(handle.snapshot().pending_outbox(), 0);

        handle.sync_now().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            submitted.lock().unwrap().is_empty(),
            "an explicit sync must not rearm a terminal device submission"
        );

        handle.reconnect().await.unwrap();
        wait_for(&handle, |snapshot| {
            connection_attempts.load(Ordering::Relaxed) >= 2
                && matches!(snapshot.connection(), ConnectionState::Ready { .. })
        })
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            submitted.lock().unwrap().is_empty(),
            "reconnecting must not create a fresh device submission"
        );

        handle.shutdown_and_wait().await.unwrap();
        let reopened = SqliteChatStore::open(&database.0).unwrap();
        let terminal = reopened.outbox(outbox_id).unwrap().unwrap();
        assert_eq!(terminal.automatic_retry_count(), 0);
        assert!(matches!(
            terminal.status(),
            OutboxStatus::Device(SubmissionState::Failed(SubmissionFailure::DeliveryTimeout))
        ));
    }

    #[tokio::test]
    async fn nearby_generation_changes_do_not_rearm_a_terminal_outbox() {
        let database = TestDatabase::new("terminal-outbox-nearby");
        seed_terminal_outbox(&database, 0x43);
        let nearby_generation = Arc::new(AtomicU64::new(1));
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let connector = FakeConnector {
            outcomes: VecDeque::from([Ok((binding(0x23), Vec::new()))]),
            attempts: Arc::new(AtomicUsize::new(0)),
            submitted: submitted.clone(),
            status: SubmissionState::Failed(SubmissionFailure::DeliveryTimeout),
            nearby_generation: Some(nearby_generation.clone()),
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();

        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
        })
        .await;
        assert!(handle.nearby_peers().await.unwrap().is_empty());
        nearby_generation.store(2, Ordering::Relaxed);
        assert!(handle.nearby_peers().await.unwrap().is_empty());
        handle.sync_now().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            submitted.lock().unwrap().is_empty(),
            "nearby observations and sync activity must not create a fresh device submission"
        );
        handle.shutdown_and_wait().await.unwrap();
    }

    #[tokio::test]
    async fn actor_serializes_nomad_fetches_and_probes_through_its_existing_session() {
        let database = TestDatabase::new("nomad-fetch");
        let trace = Arc::new(Mutex::new(NomadTrace::default()));
        let connector = NomadConnector {
            trace: trace.clone(),
            connected: false,
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
        })
        .await;

        let start: NomadFetchStartRequest = serde_json::from_value(serde_json::json!({
            "destination": "83".repeat(16),
            "path": "/page/index.mu",
            "timestamp_unix_ms": 1_784_732_100_001_u64,
            "idempotency_key": "84".repeat(16),
        }))
        .unwrap();
        let accepted = handle.nomad_fetch_start(start).await.unwrap();
        let id = reticulum_device_api::NomadFetchId::new([0x82; 8], 1).unwrap();
        assert_eq!(
            serde_json::to_value(accepted).unwrap(),
            serde_json::json!({
                "id": hex::encode(id.as_bytes()),
                "outcome": "accepted",
            })
        );

        let poll: NomadFetchPollRequest = serde_json::from_value(serde_json::json!({
            "id": hex::encode(id.as_bytes()),
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(handle.nomad_fetch_poll(poll).await.unwrap()).unwrap(),
            serde_json::json!({
                "state": "ready",
                "page": ">Metalbeard",
            })
        );

        let probe_start: ReticulumProbeStartRequest = serde_json::from_value(serde_json::json!({
            "destination": "86".repeat(16),
            "idempotency_key": "87".repeat(16),
        }))
        .unwrap();
        let accepted = handle.reticulum_probe_start(probe_start).await.unwrap();
        let probe_id = reticulum_device_api::ProbeId::new([0x85; 16]).unwrap();
        assert_eq!(
            serde_json::to_value(accepted).unwrap(),
            serde_json::json!({
                "id": hex::encode(probe_id.as_bytes()),
                "outcome": "accepted",
            })
        );
        let probe_poll: ReticulumProbePollRequest = serde_json::from_value(serde_json::json!({
            "id": hex::encode(probe_id.as_bytes()),
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(handle.reticulum_probe_poll(probe_poll).await.unwrap()).unwrap(),
            serde_json::json!({
                "state": "succeeded",
                "result": {
                    "round_trip_ms": 1_234,
                    "hops": 2,
                    "ingress_observation": {
                        "interface_id": 7,
                        "signal": {
                            "rssi_dbm": -91,
                            "snr_db": 7,
                        },
                    },
                },
            })
        );
        {
            let trace = trace.lock().unwrap();
            assert_eq!(
                trace.starts,
                [ObservedNomadStart {
                    destination: [0x83; 16],
                    path: "/page/index.mu".to_owned(),
                    timestamp_unix_ms: 1_784_732_100_001,
                    idempotency_key: [0x84; 16],
                }]
            );
            assert_eq!(trace.polls, [id]);
            assert_eq!(
                trace.probe_starts,
                [ObservedProbeStart {
                    destination: [0x86; 16],
                    idempotency_key: [0x87; 16],
                }]
            );
            assert_eq!(trace.probe_polls, [probe_id]);
        }

        handle.shutdown_and_wait().await.unwrap();
    }

    #[tokio::test]
    async fn actor_serializes_network_reads_and_secret_bearing_mutations() {
        let database = TestDatabase::new("network-config");
        let trace = Arc::new(Mutex::new(NetworkTrace::default()));
        let connector = NetworkConnector {
            trace: trace.clone(),
            connected: false,
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Ready { .. })
        })
        .await;

        assert_eq!(
            serde_json::to_value(handle.network_config().await.unwrap()).unwrap(),
            serde_json::json!({
                "revision": 5,
                "wifi_profiles": [{
                    "profile_id": "92".repeat(16),
                    "enabled": true,
                    "priority": 220,
                    "ssid": {"encoding": "hex", "value": "6d657368ff"},
                    "credential_configured": true
                }],
                "tcp_peer": null,
                "wifi_transport_enabled": true,
                "automatic_announces_enabled": true,
                "rmap_discovery_enabled": false,
                "rmap_share_location": false,
                "rmap_phone_location": null,
                "lora_tx_power_dbm": 14,
                "lora_profile": {
                    "frequency_hz": 915_000_000,
                    "bandwidth_hz": 125_000,
                    "spreading_factor": 7,
                    "coding_rate_denominator": 5,
                    "tx_power_dbm": 14
                }
            })
        );
        assert_eq!(
            serde_json::to_value(handle.network_status().await.unwrap()).unwrap(),
            serde_json::json!({
                "configured_revision": 6,
                "applied_revision": 5,
                "wifi_state": "connected",
                "active_wifi_profile": "92".repeat(16),
                "connected_ssid": {"encoding": "hex", "value": "6d657368ff"},
                "ipv4_address": "192.0.2.33",
                "rssi_dbm": -68,
                "tcp_peer_state": "waiting_for_network",
                "last_tcp_failure": null,
                "dns_diagnostics": null
            })
        );
        assert_eq!(
            handle.manual_service_announce().await.unwrap(),
            ManualServiceAnnounceDisposition::Queued
        );
        assert_eq!(
            serde_json::to_value(handle.radio_routes_status().await.unwrap()).unwrap(),
            serde_json::json!({
                "uptime_ms": 4_500,
                "interfaces": [{
                    "id": 1,
                    "kind": "lora",
                    "state": "online",
                    "generation": 2,
                    "logical_mtu": 500,
                    "bitrate": 5_470
                }],
                "lora": {
                    "applied_tx_power_dbm": 22,
                    "frequency_hz": 915_000_000,
                    "bandwidth_hz": 125_000,
                    "spreading_factor": 7,
                    "coding_rate_denominator": 5,
                    "rx_physical_frames": 8,
                    "rx_packets": 7,
                    "rx_errors": 1,
                    "rx_drops": 0,
                    "tx_terminal_jobs": 3,
                    "tx_successes": 2,
                    "tx_completed_frames": 5,
                    "tx_access_rejects": 1,
                    "tx_failures": 0,
                    "cad_busy": 4,
                    "cad_clear": 9,
                    "last_rx": {"age_ms": 500, "rssi_dbm": -91, "snr_db": 7},
                    "last_tx": {
                        "age_ms": 700,
                        "outcome": "completed",
                        "family": "data",
                        "data_evidence": {
                            "interface_id": 1,
                            "encoded_packet_len": 183,
                            "encoded_packet_sha256": "ab".repeat(32)
                        }
                    },
                    "last_data_tx": {
                        "age_ms": 700,
                        "outcome": "completed",
                        "family": "data",
                        "data_evidence": {
                            "interface_id": 1,
                            "encoded_packet_len": 183,
                            "encoded_packet_sha256": "ab".repeat(32)
                        }
                    }
                },
                "rns": {
                    "received": 9,
                    "forwarded": 2,
                    "dedup_drops": 1,
                    "invalid_drops": 0,
                    "announces_received": 4,
                    "paths_learned": 1,
                    "paths_expired": 0,
                    "links_established": 0,
                    "links_closed": 0,
                    "links_failed": 0
                },
                "observed_peer_count": 3,
                "retained_route_count": 1,
                "usable_route_count": 1,
                "route_table_revision": 1,
                "routes": [{
                    "destination": "95".repeat(16),
                    "next_hop_identity": "96".repeat(16),
                    "hops": 2,
                    "retained_interface_id": 1,
                    "resolution": "exact_ready",
                    "learned_age_ms": 1_200,
                    "last_local_use_age_ms": 400,
                    "expires_in_ms": 28_800
                }]
            })
        );

        let request: NetworkConfigMutationRequest = serde_json::from_value(serde_json::json!({
            "mutation": {
                "kind": "upsert_wifi",
                "profile_id": "93".repeat(16),
                "enabled": true,
                "priority": 230,
                "ssid": {"encoding": "utf8", "value": "field-node"},
                "credential": {
                    "kind": "replace",
                    "passphrase": "correct horse battery staple"
                }
            },
            "expected_revision": 5,
            "idempotency_key": "94".repeat(16)
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(handle.mutate_network_config(request).await.unwrap()).unwrap(),
            serde_json::json!({
                "outcome": "applied",
                "revision": 6,
                "reboot_required": true
            })
        );

        {
            let trace = trace.lock().unwrap();
            assert_eq!(trace.announces, 1);
            assert_eq!(trace.config_reads, 1);
            assert_eq!(trace.status_reads, 1);
            assert_eq!(trace.diagnostics_reads, 1);
            assert_eq!(trace.route_reads, 1);
            assert_eq!(
                trace.mutations,
                [ObservedNetworkMutation {
                    profile_id: [0x93; 16],
                    enabled: true,
                    priority: 230,
                    ssid: b"field-node".to_vec(),
                    replacement_passphrase_len: Some(28),
                    expected_revision: 5,
                    idempotency_key: [0x94; 16],
                }]
            );
        }

        handle.shutdown_and_wait().await.unwrap();
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
            status: SubmissionState::Queued,
            nearby_generation: None,
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
    async fn manual_retry_stamps_the_latest_phone_observation_without_rewriting_the_first_attempt()
    {
        let database = TestDatabase::new("manual-retry-location");
        let material = outbound(0xa2);
        let mut store = SqliteChatStore::open(&database.0).unwrap();
        let outbox_id = store.commit_outbound(material).unwrap().outbox_id();
        let accepted =
            AcceptanceIds::new(SubmissionId::new(0xa2).unwrap(), MessageId::new([0xa2; 32]));
        store.record_acceptance(outbox_id, accepted).unwrap();
        store
            .project_submission_status(
                accepted.submission_id(),
                SubmissionState::Failed(SubmissionFailure::NoPath),
            )
            .unwrap();
        let connector = UnavailableConnector {
            attempts: Arc::new(AtomicUsize::new(0)),
        };
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Unavailable { .. })
        })
        .await;

        let retry_location = phone_location(42_234_567, 1_784_732_300_003);
        handle.update_phone_location(retry_location).await.unwrap();
        assert_eq!(
            handle
                .retry_send(outbox_id, IdempotencyKey::new([0xa3; 16]))
                .await
                .unwrap(),
            OutboxRetryOutcome::Requeued(outbox_id)
        );
        let attempt_locations = handle
            .message_activity(MessageActivityPageRequest::new(None, 100, None))
            .await
            .unwrap()
            .events()
            .iter()
            .filter_map(MessageActivityEventView::attempt_location)
            .collect::<Vec<_>>();
        assert_eq!(
            attempt_locations,
            vec![
                retry_location,
                PhoneLocationObservationView::Unavailable {
                    reason: PhoneLocationUnavailableReasonView::NotObserved,
                },
            ]
        );
        handle.shutdown_and_wait().await.unwrap();
    }

    #[tokio::test]
    async fn durable_activity_query_remains_available_without_a_device_session() {
        let database = TestDatabase::new("offline-message-activity");
        let attempts = Arc::new(AtomicUsize::new(0));
        let connector = UnavailableConnector {
            attempts: attempts.clone(),
        };
        let store = SqliteChatStore::open(&database.0).unwrap();
        let handle = start_with_connector(config(&database), store, connector).unwrap();
        wait_for(&handle, |snapshot| {
            matches!(snapshot.connection(), ConnectionState::Unavailable { .. })
        })
        .await;

        assert_eq!(
            handle.phone_location_observation().await.unwrap(),
            PhoneLocationObservationView::Unavailable {
                reason: PhoneLocationUnavailableReasonView::NotObserved,
            }
        );
        let attempt_location = phone_location(43_765_432, 1_784_732_200_002);
        assert_eq!(
            handle
                .update_phone_location(attempt_location)
                .await
                .unwrap(),
            attempt_location
        );
        let committed = handle.enqueue_send(outbound(0xa1)).await.unwrap();
        let page = handle
            .message_activity(MessageActivityPageRequest::new(None, 100, None))
            .await
            .unwrap();
        assert_eq!(page.events().len(), 1);
        assert!(!page.history_incomplete());
        assert_eq!(page.events()[0].attempt_number(), Some(1));
        assert!(matches!(
            page.events()[0].activity(),
            MessageActivityKindView::OutboundQueued
        ));
        assert_eq!(page.events()[0].attempt_location(), Some(attempt_location));
        handle
            .update_phone_location(PhoneLocationObservationView::Unavailable {
                reason: PhoneLocationUnavailableReasonView::TelemetryDisabled,
            })
            .await
            .unwrap();
        assert_eq!(
            handle
                .message_activity(MessageActivityPageRequest::new(None, 100, None))
                .await
                .unwrap()
                .events()[0]
                .attempt_location(),
            Some(attempt_location),
            "later cache updates must not rewrite an existing attempt"
        );
        assert_eq!(
            handle
                .message_activity(MessageActivityPageRequest::new(
                    None,
                    100,
                    Some(page.events()[0].timeline_sequence()),
                ))
                .await
                .unwrap()
                .events()
                .len(),
            1
        );
        assert!(matches!(committed, OutboxCommitOutcome::Inserted(_)));

        assert!(matches!(
            handle
                .message_activity(MessageActivityPageRequest::new(None, 0, None))
                .await,
            Err(ServiceError::Operation(error)) if error.contains("page limit")
        ));
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
            status: SubmissionState::Queued,
            nearby_generation: None,
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
