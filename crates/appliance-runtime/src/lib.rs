//! Transport-neutral durable LXMF chat actor.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reticulum_appliance_store::{
    AttemptLocationStamp, ChatStore, Contact, ContactUpsertOutcome, ConversationPeer,
    DestinationHash, DeviceBinding, IdempotencyKey, InboundCommitOutcome,
    MessageIngressObservation, MessageLocation, MessageSignalObservation, OutboxCommitOutcome,
    OutboxId, OutboxMaterial, OutboxRetryOutcome, OutboxStatus, PacketEvidence, SqliteChatStore,
    SqliteStoreError, StatusProjectionOutcome, SubmissionFailure, SubmissionState,
    TimelineDirection as CoreTimelineDirection, TimelineEntry, UnixTimestampMillis,
};
use reticulum_appliance_sync::{
    ChatEngine, DeviceSessionError, EngineError, InboxStep, LxmfSession, ReconcileStep,
};
use reticulum_device_api::{
    ApiErrorCode, MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES,
};
use reticulum_device_client::ClientError;
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
    MessageActivityPageView, MessageActivityRequestError,
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
/// The host service implements [`Self::BluetoothLowEnergy`]. The remaining
/// variants deliberately reserve runtime vocabulary for future local-bearer
/// adapters without implying that they are already available.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionTransport {
    /// Reserved host-side USB serial adapter.
    UsbSerial,
    /// Reserved direct USB OTG adapter owned by a native client.
    UsbOtg,
    /// Bluetooth Low Energy connection owned by a native client.
    BluetoothLowEnergy,
    /// Reserved local Wi-Fi adapter owned by a native or web client.
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

    /// Exact one-based app-created device-submission number.
    pub const fn current_attempt_number(&self) -> Option<u32> {
        self.current_attempt_number
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

    /// Create a replacement device submission for a retryable terminal message
    /// without adding a timeline row.
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
mod tests;
