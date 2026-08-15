use core::{
    fmt,
    num::{NonZeroU32, NonZeroU64},
};

use std::{string::String, vec::Vec};

/// Bytes in an RNS destination hash.
pub const DESTINATION_HASH_LENGTH: usize = 16;
/// Bytes in a Python-compatible LXMF message identifier.
pub const MESSAGE_ID_LENGTH: usize = 32;
/// Bytes in an LXMF idempotency key used by the device API.
pub const IDEMPOTENCY_KEY_LENGTH: usize = 16;
/// Maximum immutable message-activity events retained by a store.
pub const MAX_MESSAGE_ACTIVITY_EVENTS: usize = 10_000;
/// Maximum events returned by one message-activity page.
pub const MAX_MESSAGE_ACTIVITY_PAGE_SIZE: usize = 100;
/// Maximum RF trace events admitted by one atomic import or returned page.
pub const MAX_RF_TRACE_PAGE_SIZE: usize = 100;
/// Bytes in a SHA-256 digest of a complete encoded Reticulum packet.
pub const ENCODED_PACKET_SHA256_LENGTH: usize = 32;
/// Bytes in a Reticulum delivery-proof attempt token.
pub const RNS_ATTEMPT_TOKEN_LENGTH: usize = 32;
/// Bytes in the immutable applied-radio-profile fingerprint.
pub const RF_TRACE_PROFILE_FINGERPRINT_LENGTH: usize = 16;
/// Bytes in the authenticated public device API identifier.
pub const DEVICE_ID_LENGTH: usize = 16;
/// Largest positive whole-millisecond timestamp that remains injective when
/// converted to LXMF's binary64 seconds representation.
pub const MAX_UNIX_TIMESTAMP_MILLIS: u64 = (1_u64 << 43) * 1_000 - 1;
/// Smallest valid latitude in millionths of one degree.
pub const MIN_LATITUDE_E6: i32 = -90_000_000;
/// Largest valid latitude in millionths of one degree.
pub const MAX_LATITUDE_E6: i32 = 90_000_000;
/// Smallest valid longitude in millionths of one degree.
pub const MIN_LONGITUDE_E6: i32 = -180_000_000;
/// Largest valid longitude in millionths of one degree.
pub const MAX_LONGITUDE_E6: i32 = 180_000_000;

/// Copyable location snapshot carried inside one authenticated LXMF message.
///
/// The integer units match Sideband's location telemetry representation while
/// keeping the persistence and client boundaries independent of floating-point
/// rounding behavior.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MessageLocation {
    latitude_e6: i32,
    longitude_e6: i32,
    altitude_cm: i32,
    speed_cm_per_second: u32,
    bearing_centidegrees: i32,
    accuracy_cm: u16,
    updated_at_unix_seconds: u32,
}

impl MessageLocation {
    /// Validate geographic coordinates and construct one complete snapshot.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        latitude_e6: i32,
        longitude_e6: i32,
        altitude_cm: i32,
        speed_cm_per_second: u32,
        bearing_centidegrees: i32,
        accuracy_cm: u16,
        updated_at_unix_seconds: u32,
    ) -> Option<Self> {
        if latitude_e6 < MIN_LATITUDE_E6
            || latitude_e6 > MAX_LATITUDE_E6
            || longitude_e6 < MIN_LONGITUDE_E6
            || longitude_e6 > MAX_LONGITUDE_E6
        {
            return None;
        }
        Some(Self {
            latitude_e6,
            longitude_e6,
            altitude_cm,
            speed_cm_per_second,
            bearing_centidegrees,
            accuracy_cm,
            updated_at_unix_seconds,
        })
    }

    /// Latitude in millionths of one degree.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Longitude in millionths of one degree.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }

    /// Altitude in centimetres.
    pub const fn altitude_cm(self) -> i32 {
        self.altitude_cm
    }

    /// Ground speed in centimetres per second.
    pub const fn speed_cm_per_second(self) -> u32 {
        self.speed_cm_per_second
    }

    /// Bearing in hundredths of one degree.
    pub const fn bearing_centidegrees(self) -> i32 {
        self.bearing_centidegrees
    }

    /// Horizontal accuracy radius in centimetres.
    pub const fn accuracy_cm(self) -> u16 {
        self.accuracy_cm
    }

    /// Source fix update time in whole Unix seconds.
    pub const fn updated_at_unix_seconds(self) -> u32 {
        self.updated_at_unix_seconds
    }
}

/// Stable device and local-destination identity bound to one chat database.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceBinding {
    device_id: [u8; DEVICE_ID_LENGTH],
    primary_destination: DestinationHash,
    lxmf_delivery_destination: DestinationHash,
}

impl DeviceBinding {
    /// Construct one complete authenticated device binding.
    pub const fn new(
        device_id: [u8; DEVICE_ID_LENGTH],
        primary_destination: DestinationHash,
        lxmf_delivery_destination: DestinationHash,
    ) -> Self {
        Self {
            device_id,
            primary_destination,
            lxmf_delivery_destination,
        }
    }

    /// Stable device API identifier authenticated by the session handshake.
    pub const fn device_id(self) -> [u8; DEVICE_ID_LENGTH] {
        self.device_id
    }

    /// Node's public primary Reticulum destination.
    pub const fn primary_destination(self) -> DestinationHash {
        self.primary_destination
    }

    /// Node's registered local `lxmf.delivery` destination.
    pub const fn lxmf_delivery_destination(self) -> DestinationHash {
        self.lxmf_delivery_destination
    }
}

/// Complete `lxmf.delivery` destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DestinationHash([u8; DESTINATION_HASH_LENGTH]);

impl DestinationHash {
    /// Construct a destination hash from all bytes.
    pub const fn new(bytes: [u8; DESTINATION_HASH_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all destination-hash bytes.
    pub const fn as_bytes(&self) -> &[u8; DESTINATION_HASH_LENGTH] {
        &self.0
    }
}

/// Python-compatible authenticated LXMF message identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageId([u8; MESSAGE_ID_LENGTH]);

impl MessageId {
    /// Construct a message identifier from all bytes.
    pub const fn new(bytes: [u8; MESSAGE_ID_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all message-identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; MESSAGE_ID_LENGTH] {
        &self.0
    }
}

/// Principal-scoped device-API request key.
///
/// An ambiguous call is replayed with the same key. An explicit retry of a
/// retryable terminal row uses a fresh key to create a replacement durable
/// device submission while preserving the semantic LXMF material and message
/// identity. Board-owned carrier retries do not rotate it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IdempotencyKey([u8; IDEMPOTENCY_KEY_LENGTH]);

impl IdempotencyKey {
    /// Construct a key from all bytes.
    pub const fn new(bytes: [u8; IDEMPOTENCY_KEY_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all key bytes.
    pub const fn as_bytes(&self) -> &[u8; IDEMPOTENCY_KEY_LENGTH] {
        &self.0
    }
}

/// SHA-256 of the complete encoded Reticulum packet reported by the device.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EncodedPacketSha256([u8; ENCODED_PACKET_SHA256_LENGTH]);

impl EncodedPacketSha256 {
    /// Construct a digest from all bytes.
    pub const fn new(bytes: [u8; ENCODED_PACKET_SHA256_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all digest bytes.
    pub const fn as_bytes(&self) -> &[u8; ENCODED_PACKET_SHA256_LENGTH] {
        &self.0
    }
}

/// Positive whole-millisecond Unix timestamp accepted by the current LXMF
/// composition boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnixTimestampMillis(u64);

impl UnixTimestampMillis {
    /// Validate and construct a timestamp.
    pub const fn new(value: u64) -> Result<Self, InvalidTimestamp> {
        if value == 0 || value > MAX_UNIX_TIMESTAMP_MILLIS {
            Err(InvalidTimestamp { actual: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Whole milliseconds since the Unix epoch.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A timestamp is zero or outside the exact LXMF/JavaScript-safe range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTimestamp {
    actual: u64,
}

impl InvalidTimestamp {
    /// Rejected value.
    pub const fn actual(self) -> u64 {
        self.actual
    }
}

impl fmt::Display for InvalidTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LXMF timestamp {} is outside 1..={MAX_UNIX_TIMESTAMP_MILLIS}",
            self.actual
        )
    }
}

/// Stable local outbox record identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OutboxId(NonZeroU64);

impl OutboxId {
    /// Construct a non-zero outbox identifier.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Complete persisted numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Stable device-assigned submission identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SubmissionId(NonZeroU64);

impl SubmissionId {
    /// Validate and construct a submission identifier.
    pub const fn new(value: u64) -> Result<Self, InvalidSubmissionId> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(InvalidSubmissionId),
        }
    }

    /// Complete persisted numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Zero cannot identify a device submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSubmissionId;

impl fmt::Display for InvalidSubmissionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a submission identifier must be non-zero")
    }
}

/// Stable insertion order used to break equal-timestamp timeline ties.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TimelineSequence(NonZeroU64);

impl TimelineSequence {
    /// Construct a non-zero timeline sequence.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Complete persisted numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Stable monotonically allocated message-activity identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageActivityId(NonZeroU64);

impl MessageActivityId {
    /// Construct a non-zero activity identifier.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Complete persisted numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// One-based app-created device-submission number for a durable outbound row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageAttemptNumber(NonZeroU32);

impl MessageAttemptNumber {
    /// Construct a non-zero app-created device-submission number.
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Initial app-created device-submission number for a new outbound row.
    pub const fn first() -> Self {
        Self(NonZeroU32::MIN)
    }

    /// Advance to the next replacement submission when persistence permits.
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.get().checked_add(1) {
            Some(value) => Self::new(value),
            None => None,
        }
    }

    /// One-based numeric representation.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Location authorization precision reported by the phone platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhoneLocationAuthorization {
    /// Precise or fine location authorization is active.
    Precise,
    /// Only reduced or coarse location authorization is active.
    Approximate,
    /// The platform did not report its authorization precision.
    Unknown,
}

/// Origin of one phone location fix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhoneLocationSource {
    /// The fix arrived from a foreground location subscription.
    ForegroundStream,
    /// The platform supplied a previously retained last-known fix.
    LastKnown,
}

/// Validated phone location retained with a local message observation.
///
/// Depending on its owner this is the phone's position when an app-created
/// submission was queued or when an inbound message was imported. Board-owned
/// carrier retries reuse the submission stamp. It is neither board GNSS nor
/// proof of the exact RF emission or reception position/time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhoneLocationSample {
    latitude_e6: i32,
    longitude_e6: i32,
    horizontal_accuracy_mm: Option<u32>,
    altitude_mm: Option<i32>,
    vertical_accuracy_mm: Option<u32>,
    captured_at_unix_ms: u64,
    authorization: PhoneLocationAuthorization,
    source: PhoneLocationSource,
    mocked: Option<bool>,
}

impl PhoneLocationSample {
    /// Validate and construct one complete phone location fix.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        latitude_e6: i32,
        longitude_e6: i32,
        horizontal_accuracy_mm: Option<u32>,
        captured_at_unix_ms: u64,
        authorization: PhoneLocationAuthorization,
        source: PhoneLocationSource,
        mocked: Option<bool>,
    ) -> Option<Self> {
        if latitude_e6 < MIN_LATITUDE_E6
            || latitude_e6 > MAX_LATITUDE_E6
            || longitude_e6 < MIN_LONGITUDE_E6
            || longitude_e6 > MAX_LONGITUDE_E6
            || captured_at_unix_ms > MAX_UNIX_TIMESTAMP_MILLIS
        {
            return None;
        }
        Some(Self {
            latitude_e6,
            longitude_e6,
            horizontal_accuracy_mm,
            altitude_mm: None,
            vertical_accuracy_mm: None,
            captured_at_unix_ms,
            authorization,
            source,
            mocked,
        })
    }

    /// Latitude in millionths of one degree.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Longitude in millionths of one degree.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }

    /// Optional horizontal accuracy radius in millimetres.
    pub const fn horizontal_accuracy_mm(self) -> Option<u32> {
        self.horizontal_accuracy_mm
    }

    /// Retain optional platform-reported altitude and vertical accuracy.
    ///
    /// Altitude is expressed relative to the platform location provider's
    /// reference ellipsoid. Keeping both values optional avoids conflating a
    /// valid zero altitude with an unavailable sensor field.
    pub const fn with_altitude(
        mut self,
        altitude_mm: Option<i32>,
        vertical_accuracy_mm: Option<u32>,
    ) -> Self {
        self.altitude_mm = altitude_mm;
        self.vertical_accuracy_mm = vertical_accuracy_mm;
        self
    }

    /// Optional platform-reported altitude in millimetres.
    pub const fn altitude_mm(self) -> Option<i32> {
        self.altitude_mm
    }

    /// Optional vertical accuracy radius in millimetres.
    pub const fn vertical_accuracy_mm(self) -> Option<u32> {
        self.vertical_accuracy_mm
    }

    /// Platform capture time in Unix milliseconds.
    pub const fn captured_at_unix_ms(self) -> u64 {
        self.captured_at_unix_ms
    }

    /// Platform authorization precision at capture time.
    pub const fn authorization(self) -> PhoneLocationAuthorization {
        self.authorization
    }

    /// How the app obtained this fix.
    pub const fn source(self) -> PhoneLocationSource {
        self.source
    }

    /// Whether Android reported this fix as mocked, when known.
    pub const fn mocked(self) -> Option<bool> {
        self.mocked
    }
}

/// Why an outbound attempt has no phone location fix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhoneLocationUnavailableReason {
    /// No app-side location state has reached this runtime incarnation yet.
    NotObserved,
    /// The user disabled private field telemetry.
    TelemetryDisabled,
    /// Foreground location permission was denied.
    PermissionDenied,
    /// Platform location services were disabled.
    ServicesDisabled,
    /// This client platform cannot provide phone location.
    PlatformUnavailable,
    /// Telemetry is enabled but no fix has arrived yet.
    NoFixYet,
    /// The platform location provider failed.
    ProviderError,
}

/// Closed phone-location stamp attached to every app-created outbound
/// submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptLocationStamp {
    /// The phone supplied a validated fix.
    Available(PhoneLocationSample),
    /// No fix was available for an explicit retained reason.
    Unavailable(PhoneLocationUnavailableReason),
}

impl AttemptLocationStamp {
    /// Default stamp used before any app-side observation reaches the runtime.
    pub const fn not_observed() -> Self {
        Self::Unavailable(PhoneLocationUnavailableReason::NotObserved)
    }
}

/// Immutable semantic activity attached to one durable message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageActivityKind {
    /// A new authenticated inbound message was imported.
    InboundImported {
        /// Authenticated LXMF message identifier.
        message_id: MessageId,
    },
    /// Exact outbound material was committed locally before submission.
    OutboundQueued {
        /// Phone-location state atomically captured for this initial app
        /// submission.
        location: AttemptLocationStamp,
    },
    /// The device durably accepted the current app-created submission.
    OutboundAccepted {
        /// Exact submission and LXMF message identifiers.
        acceptance: AcceptanceIds,
    },
    /// A new device lifecycle state was durably projected.
    OutboundStatus {
        /// Newly projected state, including packet evidence when available.
        state: SubmissionState,
    },
    /// A terminal row was rearmed as a replacement durable device submission.
    OutboundRequeued {
        /// Phone-location state atomically captured for this replacement.
        location: AttemptLocationStamp,
    },
}

/// One immutable, privacy-bounded durable message-activity event.
///
/// The observation timestamp records when the local store observed the
/// mutation. It is not a claim about RF transmission or remote delivery time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageActivityEvent {
    pub(crate) id: MessageActivityId,
    pub(crate) observed_at_unix_ms: Option<u64>,
    pub(crate) timeline_sequence: TimelineSequence,
    pub(crate) peer: DestinationHash,
    pub(crate) direction: TimelineDirection,
    pub(crate) outbox_id: Option<OutboxId>,
    pub(crate) attempt_number: Option<MessageAttemptNumber>,
    pub(crate) ingress_observation: Option<MessageIngressObservation>,
    pub(crate) message_location: Option<MessageLocation>,
    pub(crate) receiver_location: Option<PhoneLocationSample>,
    pub(crate) kind: MessageActivityKind,
}

impl MessageActivityEvent {
    /// Stable activity ordering and pagination identifier.
    pub const fn id(&self) -> MessageActivityId {
        self.id
    }

    /// Best-effort local persistence observation time in Unix milliseconds.
    pub const fn observed_at_unix_ms(&self) -> Option<u64> {
        self.observed_at_unix_ms
    }

    /// Stable message row to which this activity belongs.
    pub const fn timeline_sequence(&self) -> TimelineSequence {
        self.timeline_sequence
    }

    /// Remote conversation destination.
    pub const fn peer(&self) -> DestinationHash {
        self.peer
    }

    /// Message direction relative to the local identity.
    pub const fn direction(&self) -> TimelineDirection {
        self.direction
    }

    /// Stable local outbox identifier for outbound activity.
    pub const fn outbox_id(&self) -> Option<OutboxId> {
        self.outbox_id
    }

    /// One-based app-created device-submission number for outbound activity.
    pub const fn attempt_number(&self) -> Option<MessageAttemptNumber> {
        self.attempt_number
    }

    /// Receiver-local first-arrival evidence associated with an inbound row.
    ///
    /// Store queries project this from the canonical inbound message so a
    /// later duplicate that supplies previously unavailable signal metadata is
    /// visible without mutating or duplicating the activity journal.
    pub const fn ingress_observation(&self) -> Option<MessageIngressObservation> {
        self.ingress_observation
    }

    pub(crate) const fn with_ingress_observation(
        mut self,
        ingress_observation: Option<MessageIngressObservation>,
    ) -> Self {
        self.ingress_observation = ingress_observation;
        self
    }

    /// Authenticated sender-attached location associated with an inbound row.
    ///
    /// Store queries project this from the canonical inbound message rather
    /// than duplicating it in the immutable activity journal.
    pub const fn message_location(&self) -> Option<MessageLocation> {
        self.message_location
    }

    /// Receiver-phone fix captured when an inbound row was first imported.
    ///
    /// This is an app import-time observation, not proof of the phone or board
    /// position at the exact instant the RF carrier arrived.
    pub const fn receiver_location(&self) -> Option<PhoneLocationSample> {
        self.receiver_location
    }

    pub(crate) const fn with_inbound_locations(
        mut self,
        message_location: Option<MessageLocation>,
        receiver_location: Option<PhoneLocationSample>,
    ) -> Self {
        self.message_location = message_location;
        self.receiver_location = receiver_location;
        self
    }

    /// Semantic activity payload.
    pub const fn kind(&self) -> MessageActivityKind {
        self.kind
    }

    /// Phone-location state for an attempt-begin event.
    pub const fn attempt_location(&self) -> Option<AttemptLocationStamp> {
        match self.kind {
            MessageActivityKind::OutboundQueued { location }
            | MessageActivityKind::OutboundRequeued { location } => Some(location),
            MessageActivityKind::InboundImported { .. }
            | MessageActivityKind::OutboundAccepted { .. }
            | MessageActivityKind::OutboundStatus { .. } => None,
        }
    }
}

/// Scope of one bounded message-activity query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageActivityScope {
    /// Activity across all durable messages.
    All,
    /// Activity for exactly one durable timeline row.
    Timeline(TimelineSequence),
}

/// Validated newest-first message-activity page request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageActivityPageRequest {
    scope: MessageActivityScope,
    before: Option<MessageActivityId>,
    limit: usize,
}

impl MessageActivityPageRequest {
    /// Validate a query. `before` is an exclusive activity cursor.
    pub const fn new(
        scope: MessageActivityScope,
        before: Option<MessageActivityId>,
        limit: usize,
    ) -> Result<Self, InvalidMessageActivityPageLimit> {
        if limit == 0 || limit > MAX_MESSAGE_ACTIVITY_PAGE_SIZE {
            return Err(InvalidMessageActivityPageLimit { actual: limit });
        }
        Ok(Self {
            scope,
            before,
            limit,
        })
    }

    /// Query scope.
    pub const fn scope(self) -> MessageActivityScope {
        self.scope
    }

    /// Exclusive newest-first activity cursor.
    pub const fn before(self) -> Option<MessageActivityId> {
        self.before
    }

    /// Maximum events returned.
    pub const fn limit(self) -> usize {
        self.limit
    }
}

/// A message-activity page size was zero or exceeded the shared bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidMessageActivityPageLimit {
    actual: usize,
}

impl InvalidMessageActivityPageLimit {
    /// Rejected limit.
    pub const fn actual(self) -> usize {
        self.actual
    }
}

impl fmt::Display for InvalidMessageActivityPageLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "message activity page limit {} is outside 1..={MAX_MESSAGE_ACTIVITY_PAGE_SIZE}",
            self.actual
        )
    }
}

/// One bounded newest-first page of immutable message activity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageActivityPage {
    pub(crate) events: Vec<MessageActivityEvent>,
    pub(crate) next_before: Option<MessageActivityId>,
    pub(crate) history_incomplete: bool,
}

impl MessageActivityPage {
    /// Events in newest-first activity identifier order.
    pub fn events(&self) -> &[MessageActivityEvent] {
        &self.events
    }

    /// Exclusive cursor for the next older page, when one exists.
    pub const fn next_before(&self) -> Option<MessageActivityId> {
        self.next_before
    }

    /// Whether bounded retention pruning omitted older activity.
    pub const fn history_incomplete(&self) -> bool {
        self.history_incomplete
    }
}

/// Boot-scoped identifier supplied by the appliance RF trace producer.
///
/// Event sequence numbers may restart under a different boot identifier. Zero
/// remains representable because this is an opaque producer-owned value rather
/// than a local database identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RfTraceBootId(u64);

impl RfTraceBootId {
    /// Preserve one complete producer-owned boot identifier.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Complete numeric representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable local identifier assigned when an RF trace event is first imported.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RfTraceEventId(NonZeroU64);

impl RfTraceEventId {
    /// Construct a non-zero local trace identifier.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Complete numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Non-zero producer sequence within one RF trace boot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RfTraceEventSequence(NonZeroU64);

impl RfTraceEventSequence {
    /// Construct a non-zero boot-local sequence.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Complete numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Device-local interface selected for one RF trace event.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RfTraceInterfaceId(u8);

impl RfTraceInterfaceId {
    /// Preserve one complete device-local interface identifier.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Numeric device-local interface identifier.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Public Reticulum identity hash used for one selected next hop.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RfTraceIdentityHash([u8; DESTINATION_HASH_LENGTH]);

impl RfTraceIdentityHash {
    /// Preserve one complete public identity hash.
    pub const fn new(bytes: [u8; DESTINATION_HASH_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all public identity-hash bytes.
    pub const fn as_bytes(&self) -> &[u8; DESTINATION_HASH_LENGTH] {
        &self.0
    }
}

/// Immutable applied LoRa profile associated with one trace-producing boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceRadioProfile {
    fingerprint: [u8; RF_TRACE_PROFILE_FINGERPRINT_LENGTH],
    frequency_hz: u32,
    bandwidth_hz: u32,
    preamble_symbols: u16,
    requested_power_dbm: i16,
    spreading_factor: u8,
    coding_rate_denominator: u8,
    explicit_header: bool,
    crc: bool,
    iq_inverted: bool,
}

impl RfTraceRadioProfile {
    /// Validate and construct one complete applied profile.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        fingerprint: [u8; RF_TRACE_PROFILE_FINGERPRINT_LENGTH],
        frequency_hz: u32,
        bandwidth_hz: u32,
        preamble_symbols: u16,
        requested_power_dbm: i16,
        spreading_factor: u8,
        coding_rate_denominator: u8,
        explicit_header: bool,
        crc: bool,
        iq_inverted: bool,
    ) -> Option<Self> {
        if frequency_hz == 0
            || bandwidth_hz == 0
            || preamble_symbols == 0
            || spreading_factor == 0
            || coding_rate_denominator == 0
        {
            return None;
        }
        Some(Self {
            fingerprint,
            frequency_hz,
            bandwidth_hz,
            preamble_symbols,
            requested_power_dbm,
            spreading_factor,
            coding_rate_denominator,
            explicit_header,
            crc,
            iq_inverted,
        })
    }

    /// Complete immutable profile fingerprint.
    pub const fn fingerprint(self) -> [u8; RF_TRACE_PROFILE_FINGERPRINT_LENGTH] {
        self.fingerprint
    }

    /// Applied carrier frequency.
    pub const fn frequency_hz(self) -> u32 {
        self.frequency_hz
    }

    /// Applied LoRa bandwidth.
    pub const fn bandwidth_hz(self) -> u32 {
        self.bandwidth_hz
    }

    /// Applied preamble length in symbols.
    pub const fn preamble_symbols(self) -> u16 {
        self.preamble_symbols
    }

    /// Requested radio output power, without an EIRP claim.
    pub const fn requested_power_dbm(self) -> i16 {
        self.requested_power_dbm
    }

    /// Applied spreading-factor number.
    pub const fn spreading_factor(self) -> u8 {
        self.spreading_factor
    }

    /// Denominator of the applied `4/x` coding rate.
    pub const fn coding_rate_denominator(self) -> u8 {
        self.coding_rate_denominator
    }

    /// Whether explicit LoRa packet headers were enabled.
    pub const fn explicit_header(self) -> bool {
        self.explicit_header
    }

    /// Whether the LoRa packet CRC was enabled.
    pub const fn crc(self) -> bool {
        self.crc
    }

    /// Whether LoRa IQ polarity was inverted.
    pub const fn iq_inverted(self) -> bool {
        self.iq_inverted
    }
}

/// Exact Reticulum delivery-proof correlation token for a DATA attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RnsAttemptToken([u8; RNS_ATTEMPT_TOKEN_LENGTH]);

impl RnsAttemptToken {
    /// Construct a token from all bytes.
    pub const fn new(bytes: [u8; RNS_ATTEMPT_TOKEN_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow every token byte.
    pub const fn as_bytes(&self) -> &[u8; RNS_ATTEMPT_TOKEN_LENGTH] {
        &self.0
    }
}

/// Detailed terminal outcome reported for one LoRa DATA dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RfTraceTxOutcome {
    /// Every planned physical frame completed.
    Transmitted,
    /// Initial channel access rejected the packet.
    AccessRejected,
    /// The node owner denied the exact permit request.
    PermitDenied,
    /// A matching authorization arrived after its deadline.
    AuthorizationExpired,
    /// Fresh post-grant channel access rejected the packet.
    PostGrantAccessRejected,
    /// Airtime could not be calculated or admitted.
    AirtimeRejected,
    /// A dispatch deadline could not be represented.
    DeadlineConversionOverflow,
    /// The selected radio was inactive.
    RadioInactive,
    /// Router and dispatcher interface configurations differed.
    InterfaceConfigurationMismatch,
    /// Immutable radio configuration changed before permit negotiation.
    RadioConfigurationChangedBeforePermit,
    /// Immutable radio configuration changed after permit negotiation.
    RadioConfigurationChangedAfterPermit,
    /// Channel-activity detection failed.
    CadFault,
    /// Physical transmission failed.
    TxFault,
    /// A permit exchange could not be reconciled.
    ControlPlaneRecovery,
    /// Authorized framing or byte exposure violated an invariant.
    FrameInvariantRecovery,
    /// A dropped CAD or transmit future was explicitly reconciled.
    CancelledRadioOperation,
}

/// Why one exact DATA route selected its concrete interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RfTraceRouteResolution {
    /// An exact retained route was usable.
    ExactReady,
    /// An exact retained route existed but its interface was offline or faulted.
    ExactOffline,
    /// An exact retained route had incomplete next-hop or interface state.
    ExactMissing,
    /// No exact route existed, but a broadcast interface was usable.
    BroadcastReady,
    /// Neither an exact route nor a usable broadcast interface existed.
    BroadcastUnavailable,
}

/// Route facts captured for one exact prepared DATA packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceRouteObservation {
    destination: DestinationHash,
    next_hop: Option<RfTraceIdentityHash>,
    hops: u8,
    selected_interface: RfTraceInterfaceId,
    resolution: RfTraceRouteResolution,
    packet_evidence: PacketEvidence,
    rns_attempt_token: RnsAttemptToken,
    submission_id: SubmissionId,
}

impl RfTraceRouteObservation {
    /// Preserve complete immutable route-selection evidence.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        destination: DestinationHash,
        next_hop: Option<RfTraceIdentityHash>,
        hops: u8,
        selected_interface: RfTraceInterfaceId,
        resolution: RfTraceRouteResolution,
        packet_evidence: PacketEvidence,
        rns_attempt_token: RnsAttemptToken,
        submission_id: SubmissionId,
    ) -> Self {
        Self {
            destination,
            next_hop,
            hops,
            selected_interface,
            resolution,
            packet_evidence,
            rns_attempt_token,
            submission_id,
        }
    }

    /// Addressed Reticulum destination.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Retained next-hop identity, absent for a direct or broadcast route.
    pub const fn next_hop(self) -> Option<RfTraceIdentityHash> {
        self.next_hop
    }

    /// Reticulum hop count captured during selection.
    pub const fn hops(self) -> u8 {
        self.hops
    }

    /// Concrete interface selected for dispatch.
    pub const fn selected_interface(self) -> RfTraceInterfaceId {
        self.selected_interface
    }

    /// Route resolution category.
    pub const fn resolution(self) -> RfTraceRouteResolution {
        self.resolution
    }

    /// Complete encoded packet length and digest.
    pub const fn packet_evidence(self) -> PacketEvidence {
        self.packet_evidence
    }

    /// Hop-invariant proof-correlation token.
    pub const fn rns_attempt_token(self) -> RnsAttemptToken {
        self.rns_attempt_token
    }

    /// Exact device submission that produced this prepared attempt.
    pub const fn submission_id(self) -> SubmissionId {
        self.submission_id
    }
}

/// DATA-transmit-specific evidence in one imported RF trace observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceTxObservation {
    rns_attempt_token: RnsAttemptToken,
    interface: RfTraceInterfaceId,
    packet_evidence: PacketEvidence,
    outcome: RfTraceTxOutcome,
    planned_physical_frames: u8,
    completed_physical_frames: u8,
    frame_completed_at_us: [Option<u64>; 2],
    authorized_frame_observed: bool,
    submission_id: Option<SubmissionId>,
}

impl RfTraceTxObservation {
    /// Validate and construct complete terminal DATA evidence.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        rns_attempt_token: RnsAttemptToken,
        interface: RfTraceInterfaceId,
        packet_evidence: PacketEvidence,
        outcome: RfTraceTxOutcome,
        planned_physical_frames: u8,
        completed_physical_frames: u8,
        frame_completed_at_us: [Option<u64>; 2],
        authorized_frame_observed: bool,
        submission_id: Option<SubmissionId>,
    ) -> Option<Self> {
        if planned_physical_frames == 0
            || planned_physical_frames > 2
            || completed_physical_frames > planned_physical_frames
            || frame_completed_at_us[0].is_some() != (completed_physical_frames >= 1)
            || frame_completed_at_us[1].is_some() != (completed_physical_frames >= 2)
        {
            return None;
        }
        if let (Some(first), Some(second)) = (frame_completed_at_us[0], frame_completed_at_us[1])
            && second < first
        {
            return None;
        }
        Some(Self {
            rns_attempt_token,
            interface,
            packet_evidence,
            outcome,
            planned_physical_frames,
            completed_physical_frames,
            frame_completed_at_us,
            authorized_frame_observed,
            submission_id,
        })
    }

    /// Exact proof-correlation token retained by the board.
    pub const fn rns_attempt_token(self) -> RnsAttemptToken {
        self.rns_attempt_token
    }

    /// Concrete interface selected for dispatch.
    pub const fn interface(self) -> RfTraceInterfaceId {
        self.interface
    }

    /// Complete encoded packet length and digest.
    pub const fn packet_evidence(self) -> PacketEvidence {
        self.packet_evidence
    }

    /// Detailed dispatcher terminal outcome.
    pub const fn outcome(self) -> RfTraceTxOutcome {
        self.outcome
    }

    /// Number of physical frames planned for this logical packet.
    pub const fn planned_physical_frames(self) -> u8 {
        self.planned_physical_frames
    }

    /// Physical frames with definitive completion evidence.
    pub const fn completed_physical_frames(self) -> u8 {
        self.completed_physical_frames
    }

    /// Definitive per-physical-frame completion timestamps.
    pub const fn frame_completed_at_us(self) -> [Option<u64>; 2] {
        self.frame_completed_at_us
    }

    /// Whether the exact authorized DATA frame reached the interface boundary.
    pub const fn authorized_frame_observed(self) -> bool {
        self.authorized_frame_observed
    }

    /// Exact device submission resolved by the board, when available.
    pub const fn submission_id(self) -> Option<SubmissionId> {
        self.submission_id
    }
}

/// Receiver-local physical values for one complete logical packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceRxObservation {
    interface: RfTraceInterfaceId,
    packet_evidence: PacketEvidence,
    rns_packet_hash: Option<RnsAttemptToken>,
    rssi_dbm: i16,
    snr_db: i16,
}

impl RfTraceRxObservation {
    /// Preserve one complete receiver-local observation.
    pub const fn new(
        interface: RfTraceInterfaceId,
        packet_evidence: PacketEvidence,
        rns_packet_hash: Option<RnsAttemptToken>,
        rssi_dbm: i16,
        snr_db: i16,
    ) -> Self {
        Self {
            interface,
            packet_evidence,
            rns_packet_hash,
            rssi_dbm,
            snr_db,
        }
    }

    /// Interface that accepted the complete packet.
    pub const fn interface(self) -> RfTraceInterfaceId {
        self.interface
    }

    /// Complete encoded packet length and digest.
    pub const fn packet_evidence(self) -> PacketEvidence {
        self.packet_evidence
    }

    /// Hop-invariant RNS hash when the received packet parsed successfully.
    pub const fn rns_packet_hash(self) -> Option<RnsAttemptToken> {
        self.rns_packet_hash
    }

    /// Receiver-reported RSSI in dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Receiver-reported SNR in dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// Application-visible terminal classification for one exact DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RfTraceAttemptOutcome {
    /// A valid delivery proof completed the attempt.
    Delivered,
    /// The receipt expired without a valid proof.
    DeliveryTimeout,
    /// The attempt became terminal without a final-hop transmission.
    Unsent,
}

/// Final-hop ingress facts for the proof that completed one attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceProofIngress {
    interface: RfTraceInterfaceId,
    signal: Option<(i16, i16)>,
}

impl RfTraceProofIngress {
    /// Preserve optional whole-dB `(RSSI, SNR)` proof ingress values.
    pub const fn new(interface: RfTraceInterfaceId, signal: Option<(i16, i16)>) -> Self {
        Self { interface, signal }
    }

    /// Interface on which the proof arrived.
    pub const fn interface(self) -> RfTraceInterfaceId {
        self.interface
    }

    /// Receiver-local whole-dB `(RSSI, SNR)`, when reported.
    pub const fn signal(self) -> Option<(i16, i16)> {
        self.signal
    }
}

/// Terminal Reticulum receipt evidence for one exact DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceAttemptObservation {
    rns_attempt_token: RnsAttemptToken,
    outcome: RfTraceAttemptOutcome,
    proof_ingress: Option<RfTraceProofIngress>,
}

impl RfTraceAttemptObservation {
    /// Preserve one complete attempt-terminal observation.
    pub const fn new(
        rns_attempt_token: RnsAttemptToken,
        outcome: RfTraceAttemptOutcome,
        proof_ingress: Option<RfTraceProofIngress>,
    ) -> Self {
        Self {
            rns_attempt_token,
            outcome,
            proof_ingress,
        }
    }

    /// Hop-invariant proof-correlation token.
    pub const fn rns_attempt_token(self) -> RnsAttemptToken {
        self.rns_attempt_token
    }

    /// Terminal receipt classification.
    pub const fn outcome(self) -> RfTraceAttemptOutcome {
        self.outcome
    }

    /// Proof ingress evidence, present for a locally observed delivery proof.
    pub const fn proof_ingress(self) -> Option<RfTraceProofIngress> {
        self.proof_ingress
    }
}

/// Direction-specific payload of one raw board RF trace observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RfTraceObservationKind {
    /// Route selected immediately before DATA authorization.
    RouteSelected(RfTraceRouteObservation),
    /// Terminal local DATA dispatch.
    DataTx(RfTraceTxObservation),
    /// Complete logical packet accepted from the physical interface.
    LogicalRx(RfTraceRxObservation),
    /// Terminal delivery-receipt state for one exact DATA attempt.
    AttemptTerminal(RfTraceAttemptObservation),
}

/// One scalar immutable observation supplied by the board.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceObservation {
    event_sequence: RfTraceEventSequence,
    observed_at_us: u64,
    kind: RfTraceObservationKind,
}

impl RfTraceObservation {
    /// Construct one complete raw observation.
    pub const fn new(
        event_sequence: RfTraceEventSequence,
        observed_at_us: u64,
        kind: RfTraceObservationKind,
    ) -> Self {
        Self {
            event_sequence,
            observed_at_us,
            kind,
        }
    }

    /// Boot-local event sequence.
    pub const fn event_sequence(self) -> RfTraceEventSequence {
        self.event_sequence
    }

    /// Board monotonic observation time in microseconds.
    pub const fn observed_at_us(self) -> u64 {
        self.observed_at_us
    }

    /// Typed immutable trace payload.
    pub const fn kind(self) -> RfTraceObservationKind {
        self.kind
    }

    /// Device submission carried by a terminal DATA dispatch, when available.
    pub const fn submission_id(self) -> Option<SubmissionId> {
        match self.kind {
            RfTraceObservationKind::RouteSelected(route) => Some(route.submission_id()),
            RfTraceObservationKind::DataTx(tx) => tx.submission_id(),
            RfTraceObservationKind::LogicalRx(_) | RfTraceObservationKind::AttemptTerminal(_) => {
                None
            }
        }
    }

    /// Hop-invariant Reticulum token usable for safe cross-event correlation.
    pub const fn rns_attempt_token(self) -> Option<RnsAttemptToken> {
        match self.kind {
            RfTraceObservationKind::RouteSelected(route) => Some(route.rns_attempt_token()),
            RfTraceObservationKind::DataTx(tx) => Some(tx.rns_attempt_token()),
            RfTraceObservationKind::LogicalRx(rx) => rx.rns_packet_hash(),
            RfTraceObservationKind::AttemptTerminal(terminal) => Some(terminal.rns_attempt_token()),
        }
    }
}

/// One atomic page of observations imported from a trace-producing boot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RfTraceImportBatch {
    boot_id: RfTraceBootId,
    profile: RfTraceRadioProfile,
    imported_at_unix_ms: u64,
    history_incomplete: bool,
    observations: Vec<RfTraceObservation>,
}

impl RfTraceImportBatch {
    /// Validate a bounded, strictly sequence-ordered import batch.
    pub fn new(
        boot_id: RfTraceBootId,
        profile: RfTraceRadioProfile,
        imported_at_unix_ms: u64,
        history_incomplete: bool,
        observations: Vec<RfTraceObservation>,
    ) -> Result<Self, InvalidRfTraceImportBatch> {
        if imported_at_unix_ms > MAX_UNIX_TIMESTAMP_MILLIS {
            return Err(InvalidRfTraceImportBatch::ImportedTimestamp);
        }
        if observations.len() > MAX_RF_TRACE_PAGE_SIZE {
            return Err(InvalidRfTraceImportBatch::TooManyEvents);
        }
        if observations
            .windows(2)
            .any(|pair| pair[0].event_sequence() >= pair[1].event_sequence())
        {
            return Err(InvalidRfTraceImportBatch::SequenceOrder);
        }
        Ok(Self {
            boot_id,
            profile,
            imported_at_unix_ms,
            history_incomplete,
            observations,
        })
    }

    /// Trace-producing boot.
    pub const fn boot_id(&self) -> RfTraceBootId {
        self.boot_id
    }

    /// Immutable applied profile for this boot.
    pub const fn profile(&self) -> RfTraceRadioProfile {
        self.profile
    }

    /// Host wall-clock time when this page was imported.
    pub const fn imported_at_unix_ms(&self) -> u64 {
        self.imported_at_unix_ms
    }

    /// Whether events preceding this batch were unavailable.
    pub const fn history_incomplete(&self) -> bool {
        self.history_incomplete
    }

    /// Strictly increasing raw observations.
    pub fn observations(&self) -> &[RfTraceObservation] {
        &self.observations
    }
}

/// Structural failure in one RF trace import batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidRfTraceImportBatch {
    /// Import wall time exceeds the shared exact timestamp range.
    ImportedTimestamp,
    /// The page exceeds the shared bounded event count.
    TooManyEvents,
    /// Boot-local event sequences were duplicated or not increasing.
    SequenceOrder,
}

impl fmt::Display for InvalidRfTraceImportBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ImportedTimestamp => "RF trace import timestamp is outside the supported range",
            Self::TooManyEvents => "RF trace import exceeds the bounded page size",
            Self::SequenceOrder => "RF trace import sequences must be strictly increasing",
        })
    }
}

/// Durable association between a TX trace and one immutable outbox attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceMessageCorrelation {
    timeline_sequence: TimelineSequence,
    outbox_id: OutboxId,
    attempt_number: MessageAttemptNumber,
    attempt_location: AttemptLocationStamp,
}

impl RfTraceMessageCorrelation {
    pub(crate) const fn new(
        timeline_sequence: TimelineSequence,
        outbox_id: OutboxId,
        attempt_number: MessageAttemptNumber,
        attempt_location: AttemptLocationStamp,
    ) -> Self {
        Self {
            timeline_sequence,
            outbox_id,
            attempt_number,
            attempt_location,
        }
    }

    /// Stable message timeline row.
    pub const fn timeline_sequence(self) -> TimelineSequence {
        self.timeline_sequence
    }

    /// Stable local outbox row.
    pub const fn outbox_id(self) -> OutboxId {
        self.outbox_id
    }

    /// Exact app-created device submission associated with the trace.
    pub const fn attempt_number(self) -> MessageAttemptNumber {
        self.attempt_number
    }

    /// Phone location atomically captured when this app submission began.
    pub const fn attempt_location(self) -> AttemptLocationStamp {
        self.attempt_location
    }
}

/// One durable host-imported RF trace event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTraceEvent {
    pub(crate) id: RfTraceEventId,
    pub(crate) boot_id: RfTraceBootId,
    pub(crate) profile: RfTraceRadioProfile,
    pub(crate) imported_at_unix_ms: u64,
    pub(crate) observation: RfTraceObservation,
    pub(crate) correlation: Option<RfTraceMessageCorrelation>,
}

impl RfTraceEvent {
    /// Stable local import-order identifier.
    pub const fn id(self) -> RfTraceEventId {
        self.id
    }

    /// Trace-producing boot.
    pub const fn boot_id(self) -> RfTraceBootId {
        self.boot_id
    }

    /// Immutable applied profile for the producing boot.
    pub const fn profile(self) -> RfTraceRadioProfile {
        self.profile
    }

    /// Host wall-clock time when this observation was first imported.
    pub const fn imported_at_unix_ms(self) -> u64 {
        self.imported_at_unix_ms
    }

    /// Exact raw board observation.
    pub const fn observation(self) -> RfTraceObservation {
        self.observation
    }

    /// Immutable accepted-attempt correlation, when the submission was known.
    pub const fn message_correlation(self) -> Option<RfTraceMessageCorrelation> {
        self.correlation
    }
}

/// Scope of one bounded RF trace query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RfTraceScope {
    /// Every durable RF trace event.
    All,
    /// Events correlated to exactly one message timeline row.
    Timeline(TimelineSequence),
}

/// Validated newest-first RF trace query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RfTracePageRequest {
    scope: RfTraceScope,
    before: Option<RfTraceEventId>,
    limit: usize,
}

impl RfTracePageRequest {
    /// Validate a query. `before` is an exclusive local event cursor.
    pub const fn new(
        scope: RfTraceScope,
        before: Option<RfTraceEventId>,
        limit: usize,
    ) -> Result<Self, InvalidRfTracePageLimit> {
        if limit == 0 || limit > MAX_RF_TRACE_PAGE_SIZE {
            return Err(InvalidRfTracePageLimit { actual: limit });
        }
        Ok(Self {
            scope,
            before,
            limit,
        })
    }

    /// Query scope.
    pub const fn scope(self) -> RfTraceScope {
        self.scope
    }

    /// Exclusive newest-first cursor.
    pub const fn before(self) -> Option<RfTraceEventId> {
        self.before
    }

    /// Maximum returned events.
    pub const fn limit(self) -> usize {
        self.limit
    }
}

/// An RF trace page size was zero or exceeded the shared bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRfTracePageLimit {
    actual: usize,
}

impl InvalidRfTracePageLimit {
    /// Rejected limit.
    pub const fn actual(self) -> usize {
        self.actual
    }
}

impl fmt::Display for InvalidRfTracePageLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "RF trace page limit {} is outside 1..={MAX_RF_TRACE_PAGE_SIZE}",
            self.actual
        )
    }
}

/// One bounded newest-first page of durable RF trace events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RfTracePage {
    pub(crate) events: Vec<RfTraceEvent>,
    pub(crate) next_before: Option<RfTraceEventId>,
    pub(crate) history_incomplete: bool,
}

impl RfTracePage {
    /// Events in newest-first local import order.
    pub fn events(&self) -> &[RfTraceEvent] {
        &self.events
    }

    /// Exclusive cursor for the next older page, when one exists.
    pub const fn next_before(&self) -> Option<RfTraceEventId> {
        self.next_before
    }

    /// Whether at least one imported boot reported an overwritten trace gap.
    pub const fn history_incomplete(&self) -> bool {
        self.history_incomplete
    }
}

/// User-facing metadata for one remote LXMF destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Contact {
    destination: DestinationHash,
    display_name: String,
}

impl Contact {
    /// Construct a contact. Empty display names are allowed so the client can
    /// retain a discovered hash before learning a friendly name.
    pub fn new(destination: DestinationHash, display_name: impl Into<String>) -> Self {
        Self {
            destination,
            display_name: display_name.into(),
        }
    }

    /// Remote `lxmf.delivery` destination.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// User-facing name, possibly empty.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

/// Device-local interface that received one imported LXMF carrier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MessageInterfaceId(u8);

impl MessageInterfaceId {
    /// Preserve one complete device-local interface identifier.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Numeric device-local interface identifier.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Receiver-local physical signal values for one imported LXMF carrier.
///
/// These values describe the final physical hop into the appliance, not an
/// end-to-end path or necessarily the original sender on a relayed route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageSignalObservation {
    rssi_dbm: i16,
    snr_db: i16,
}

impl MessageSignalObservation {
    /// Preserve receiver-reported RSSI and SNR.
    pub const fn new(rssi_dbm: i16, snr_db: i16) -> Self {
        Self { rssi_dbm, snr_db }
    }

    /// Receiver-reported RSSI in dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Receiver-reported SNR in dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// Immutable first-arrival transport evidence for one imported message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageIngressObservation {
    interface: MessageInterfaceId,
    signal: Option<MessageSignalObservation>,
}

impl MessageIngressObservation {
    /// Construct one first-arrival observation.
    pub const fn new(
        interface: MessageInterfaceId,
        signal: Option<MessageSignalObservation>,
    ) -> Self {
        Self { interface, signal }
    }

    /// Device-local interface that received the carrier.
    pub const fn interface(self) -> MessageInterfaceId {
        self.interface
    }

    /// Optional receiver-local physical signal values.
    pub const fn signal(self) -> Option<MessageSignalObservation> {
        self.signal
    }
}

/// Fully decoded inbound message material retained by the chat database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundMessage {
    message_id: MessageId,
    local_destination: DestinationHash,
    source: DestinationHash,
    timestamp: UnixTimestampMillis,
    title: Vec<u8>,
    content: Vec<u8>,
    location: Option<MessageLocation>,
    ingress: Option<MessageIngressObservation>,
}

impl InboundMessage {
    /// Construct an inbound semantic message after LXMF validation.
    pub fn new(
        message_id: MessageId,
        local_destination: DestinationHash,
        source: DestinationHash,
        timestamp: UnixTimestampMillis,
        title: Vec<u8>,
        content: Vec<u8>,
    ) -> Self {
        Self {
            message_id,
            local_destination,
            source,
            timestamp,
            title,
            content,
            location: None,
            ingress: None,
        }
    }

    /// Attach the authenticated location decoded from the LXMF fields.
    pub const fn with_location(mut self, location: Option<MessageLocation>) -> Self {
        self.location = location;
        self
    }

    /// Attach the appliance's immutable first-arrival evidence.
    pub const fn with_ingress_observation(
        mut self,
        ingress: Option<MessageIngressObservation>,
    ) -> Self {
        self.ingress = ingress;
        self
    }

    /// Authenticated LXMF message identifier.
    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }

    /// Local destination that received the message.
    pub const fn local_destination(&self) -> DestinationHash {
        self.local_destination
    }

    /// Authenticated remote source destination.
    pub const fn source(&self) -> DestinationHash {
        self.source
    }

    /// Authenticated message timestamp.
    pub const fn timestamp(&self) -> UnixTimestampMillis {
        self.timestamp
    }

    /// Binary LXMF title.
    pub fn title(&self) -> &[u8] {
        &self.title
    }

    /// Binary LXMF content.
    pub fn content(&self) -> &[u8] {
        &self.content
    }

    /// Optional location snapshot authenticated with this message.
    pub const fn location(&self) -> Option<MessageLocation> {
        self.location
    }

    /// First-arrival interface and optional receiver-local signal values.
    pub const fn ingress_observation(&self) -> Option<MessageIngressObservation> {
        self.ingress
    }

    pub(crate) fn same_authenticated_message(&self, other: &Self) -> bool {
        self.message_id == other.message_id
            && self.local_destination == other.local_destination
            && self.source == other.source
            && self.timestamp == other.timestamp
            && self.title == other.title
            && self.content == other.content
            && (self.location == other.location
                || self.location.is_none()
                || other.location.is_none())
    }

    pub(crate) fn retain_ingress_if_absent(&mut self, ingress: Option<MessageIngressObservation>) {
        if self.ingress.is_none() {
            self.ingress = ingress;
        }
    }

    pub(crate) fn retain_location_if_absent(&mut self, location: Option<MessageLocation>) {
        if self.location.is_none() {
            self.location = location;
        }
    }
}

/// Persisted inbound record plus stable local ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundRecord {
    pub(crate) sequence: TimelineSequence,
    pub(crate) message: InboundMessage,
    pub(crate) receiver_location: Option<PhoneLocationSample>,
}

impl InboundRecord {
    /// Stable local insertion sequence.
    pub const fn sequence(&self) -> TimelineSequence {
        self.sequence
    }

    /// Retained semantic message.
    pub const fn message(&self) -> &InboundMessage {
        &self.message
    }

    /// Receiver-phone fix atomically retained on the first local import.
    ///
    /// A duplicate observation never fabricates or backfills this value, so
    /// its capture semantics remain tied to the original import transaction.
    pub const fn receiver_location(&self) -> Option<PhoneLocationSample> {
        self.receiver_location
    }
}

/// Exact client material that must be committed before a device send attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboxMaterial {
    destination: DestinationHash,
    timestamp: UnixTimestampMillis,
    idempotency_key: IdempotencyKey,
    title: Vec<u8>,
    content: Vec<u8>,
    location: Option<MessageLocation>,
}

impl OutboxMaterial {
    /// Construct exact commit-before-send outbound material.
    pub fn new(
        destination: DestinationHash,
        timestamp: UnixTimestampMillis,
        idempotency_key: IdempotencyKey,
        title: Vec<u8>,
        content: Vec<u8>,
    ) -> Self {
        Self {
            destination,
            timestamp,
            idempotency_key,
            title,
            content,
            location: None,
        }
    }

    /// Attach an optional location snapshot to the exact committed material.
    pub const fn with_location(mut self, location: Option<MessageLocation>) -> Self {
        self.location = location;
        self
    }

    /// Remote `lxmf.delivery` destination.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Exact timestamp retained across retries.
    pub const fn timestamp(&self) -> UnixTimestampMillis {
        self.timestamp
    }

    /// Current principal-scoped device-API attempt key.
    pub const fn idempotency_key(&self) -> IdempotencyKey {
        self.idempotency_key
    }

    /// Binary LXMF title.
    pub fn title(&self) -> &[u8] {
        &self.title
    }

    /// Binary LXMF content.
    pub fn content(&self) -> &[u8] {
        &self.content
    }

    /// Optional location snapshot retained unchanged across retries.
    pub const fn location(&self) -> Option<MessageLocation> {
        self.location
    }

    pub(crate) fn replace_idempotency_key(&mut self, idempotency_key: IdempotencyKey) {
        self.idempotency_key = idempotency_key;
    }
}

/// Identifiers returned after the device durably accepts an outbound message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptanceIds {
    submission_id: SubmissionId,
    message_id: MessageId,
}

impl AcceptanceIds {
    /// Construct the exact accepted identifier pair.
    pub const fn new(submission_id: SubmissionId, message_id: MessageId) -> Self {
        Self {
            submission_id,
            message_id,
        }
    }

    /// Device-assigned durable submission identifier.
    pub const fn submission_id(self) -> SubmissionId {
        self.submission_id
    }

    /// Device-composed authenticated LXMF message identifier.
    pub const fn message_id(self) -> MessageId {
        self.message_id
    }
}

/// Immutable packet evidence reported while a submission awaits delivery or
/// after it is delivered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketEvidence {
    encoded_packet_len: u16,
    encoded_packet_sha256: EncodedPacketSha256,
}

impl PacketEvidence {
    /// Validate and construct packet evidence.
    pub const fn new(
        encoded_packet_len: u16,
        encoded_packet_sha256: EncodedPacketSha256,
    ) -> Result<Self, InvalidEncodedPacketLength> {
        if encoded_packet_len == 0 {
            Err(InvalidEncodedPacketLength)
        } else {
            Ok(Self {
                encoded_packet_len,
                encoded_packet_sha256,
            })
        }
    }

    /// Complete encoded packet length.
    pub const fn encoded_packet_len(self) -> u16 {
        self.encoded_packet_len
    }

    /// SHA-256 of all encoded packet bytes.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }
}

/// Packet evidence cannot describe an empty encoded packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidEncodedPacketLength;

impl fmt::Display for InvalidEncodedPacketLength {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("encoded packet length must be non-zero")
    }
}

/// Stable failure category projected from the device submission lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionFailure {
    /// No route became available.
    NoPath,
    /// The device submission reached a delivery timeout.
    ///
    /// A current firmware-owned LXMF attempt timeout alone does not project
    /// this failure; the durable submission remains `Preparing`.
    DeliveryTimeout,
    /// A downstream owner rejected the submission.
    DownstreamRejection,
    /// The device reported an internal failure.
    Internal,
}

impl SubmissionFailure {
    /// Whether the exact semantic LXMF message may be attempted again.
    ///
    /// A fresh device-API idempotency key creates a replacement durable device
    /// submission while destination, timestamp, title, content, optional
    /// location, and LXMF message identity remain unchanged. This is a
    /// distinct explicit action for a retryable terminal row, not a carrier
    /// retry in the board-owned loop. A downstream rejection is an explicit
    /// refusal rather than a lossy-network condition.
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::NoPath | Self::DeliveryTimeout | Self::Internal)
    }
}

/// Device-reported lifecycle state for one accepted submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionState {
    /// Accepted work is queued durably.
    Queued,
    /// The device owns pending work. For LXMF this includes board-owned path
    /// discovery, backoff, and any number of serialized volatile RNS attempts.
    Preparing,
    /// A device attempt was released and is awaiting proof.
    AwaitingDelivery(PacketEvidence),
    /// A matching delivery proof reached a durable terminal state.
    Delivered(PacketEvidence),
    /// The submission reached a durable failure.
    Failed(SubmissionFailure),
    /// The submission was durably cancelled.
    Cancelled,
}

impl SubmissionState {
    /// Whether this state needs no further status refresh.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered(_) | Self::Failed(_) | Self::Cancelled)
    }

    pub(crate) const fn progression_rank(self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::Preparing => 1,
            Self::AwaitingDelivery(_) => 2,
            Self::Delivered(_) | Self::Failed(_) | Self::Cancelled => 3,
        }
    }

    pub(crate) const fn packet_evidence(self) -> Option<PacketEvidence> {
        match self {
            Self::AwaitingDelivery(evidence) | Self::Delivered(evidence) => Some(evidence),
            Self::Queued | Self::Preparing | Self::Failed(_) | Self::Cancelled => None,
        }
    }
}

/// Locally durable state of an outbound chat record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxStatus {
    /// Exact send material is committed locally but no acceptance was recorded.
    Committed,
    /// Device acceptance identifiers are committed, but no status response has
    /// been projected yet.
    Accepted,
    /// Latest durable device status projection.
    Device(SubmissionState),
}

impl OutboxStatus {
    /// Whether no submit or status-refresh work remains.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Device(state) if state.is_terminal())
    }
}

/// Complete durable local record for an outbound message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboxRecord {
    pub(crate) id: OutboxId,
    pub(crate) sequence: TimelineSequence,
    pub(crate) material: OutboxMaterial,
    pub(crate) acceptance: Option<AcceptanceIds>,
    pub(crate) status: OutboxStatus,
    pub(crate) current_attempt: MessageAttemptNumber,
}

impl OutboxRecord {
    /// Stable local record identifier.
    pub const fn id(&self) -> OutboxId {
        self.id
    }

    /// Stable local insertion sequence.
    pub const fn sequence(&self) -> TimelineSequence {
        self.sequence
    }

    /// Exact committed send material.
    pub const fn material(&self) -> &OutboxMaterial {
        &self.material
    }

    /// Device acceptance identifiers, once durably recorded.
    pub const fn acceptance(&self) -> Option<AcceptanceIds> {
        self.acceptance
    }

    /// Latest durable local/device lifecycle state.
    pub const fn status(&self) -> OutboxStatus {
        self.status
    }

    /// Best reconstructable one-based app-created device-submission number.
    pub const fn current_attempt(&self) -> MessageAttemptNumber {
        self.current_attempt
    }
}

/// Direction of one conversation entry relative to the local identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimelineDirection {
    /// Message received from the conversation peer.
    Inbound,
    /// Message submitted toward the conversation peer.
    Outbound,
}

/// Owned projection of one message in a conversation timeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineEntry {
    sequence: TimelineSequence,
    peer: DestinationHash,
    direction: TimelineDirection,
    timestamp: UnixTimestampMillis,
    message_id: Option<MessageId>,
    outbox_id: Option<OutboxId>,
    outbox_status: Option<OutboxStatus>,
    submission_id: Option<SubmissionId>,
    current_attempt: Option<MessageAttemptNumber>,
    packet_evidence: Option<PacketEvidence>,
    ingress: Option<MessageIngressObservation>,
    location: Option<MessageLocation>,
    receiver_location: Option<PhoneLocationSample>,
    title: Vec<u8>,
    content: Vec<u8>,
}

impl TimelineEntry {
    pub(crate) fn inbound(record: &InboundRecord) -> Self {
        let message = record.message();
        Self {
            sequence: record.sequence(),
            peer: message.source(),
            direction: TimelineDirection::Inbound,
            timestamp: message.timestamp(),
            message_id: Some(message.message_id()),
            outbox_id: None,
            outbox_status: None,
            submission_id: None,
            current_attempt: None,
            packet_evidence: None,
            ingress: message.ingress_observation(),
            location: message.location(),
            receiver_location: record.receiver_location(),
            title: message.title().to_vec(),
            content: message.content().to_vec(),
        }
    }

    pub(crate) fn outbound(record: &OutboxRecord) -> Self {
        Self {
            sequence: record.sequence(),
            peer: record.material().destination(),
            direction: TimelineDirection::Outbound,
            timestamp: record.material().timestamp(),
            message_id: record.acceptance().map(AcceptanceIds::message_id),
            outbox_id: Some(record.id()),
            outbox_status: Some(record.status()),
            submission_id: record.acceptance().map(AcceptanceIds::submission_id),
            current_attempt: Some(record.current_attempt()),
            packet_evidence: match record.status() {
                OutboxStatus::Device(state) => state.packet_evidence(),
                OutboxStatus::Committed | OutboxStatus::Accepted => None,
            },
            ingress: None,
            location: record.material().location(),
            receiver_location: None,
            title: record.material().title().to_vec(),
            content: record.material().content().to_vec(),
        }
    }

    /// Stable local insertion sequence.
    pub const fn sequence(&self) -> TimelineSequence {
        self.sequence
    }

    /// Remote conversation destination.
    pub const fn peer(&self) -> DestinationHash {
        self.peer
    }

    /// Message direction relative to the local identity.
    pub const fn direction(&self) -> TimelineDirection {
        self.direction
    }

    /// Message timestamp used for primary timeline ordering.
    pub const fn timestamp(&self) -> UnixTimestampMillis {
        self.timestamp
    }

    /// Authenticated message identifier, absent for an unaccepted outbox row.
    pub const fn message_id(&self) -> Option<MessageId> {
        self.message_id
    }

    /// Local outbox identifier for an outbound entry.
    pub const fn outbox_id(&self) -> Option<OutboxId> {
        self.outbox_id
    }

    /// Latest outbound lifecycle state, absent for an inbound entry.
    pub const fn outbox_status(&self) -> Option<OutboxStatus> {
        self.outbox_status
    }

    /// Current device submission identifier for an accepted outbound attempt.
    pub const fn submission_id(&self) -> Option<SubmissionId> {
        self.submission_id
    }

    /// Best reconstructable one-based app-created device-submission number.
    pub const fn current_attempt(&self) -> Option<MessageAttemptNumber> {
        self.current_attempt
    }

    /// Current immutable packet evidence, when projected by the device.
    pub const fn packet_evidence(&self) -> Option<PacketEvidence> {
        self.packet_evidence
    }

    /// Receiver-local first-arrival evidence for an inbound entry.
    pub const fn ingress_observation(&self) -> Option<MessageIngressObservation> {
        self.ingress
    }

    /// Optional location snapshot carried by this message.
    pub const fn location(&self) -> Option<MessageLocation> {
        self.location
    }

    /// Receiver-phone fix retained for an inbound entry at first import.
    pub const fn receiver_location(&self) -> Option<PhoneLocationSample> {
        self.receiver_location
    }

    /// Binary LXMF title.
    pub fn title(&self) -> &[u8] {
        &self.title
    }

    /// Binary LXMF content.
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

/// One peer present in either the saved contact set or durable message history.
///
/// A peer discovered only through an authenticated inbound message has no
/// saved display name. Keeping that state distinct lets clients surface a
/// message request without silently promoting its sender to a contact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationPeer {
    peer: DestinationHash,
    saved_name: Option<String>,
    message_count: usize,
    inbound_message_count: usize,
    last_message: Option<TimelineEntry>,
}

impl ConversationPeer {
    pub(crate) fn new(
        peer: DestinationHash,
        saved_name: Option<String>,
        message_count: usize,
        inbound_message_count: usize,
        last_message: Option<TimelineEntry>,
    ) -> Self {
        Self {
            peer,
            saved_name,
            message_count,
            inbound_message_count,
            last_message,
        }
    }

    /// Remote `lxmf.delivery` destination.
    pub const fn peer(&self) -> DestinationHash {
        self.peer
    }

    /// User-selected display name, only when this peer is a saved contact.
    pub fn saved_name(&self) -> Option<&str> {
        self.saved_name.as_deref()
    }

    /// Number of durable inbound and outbound timeline entries for this peer.
    pub const fn message_count(&self) -> usize {
        self.message_count
    }

    /// Number of authenticated inbound messages observed from this peer.
    ///
    /// An unsaved outbound-only peer has zero and must not be presented as an
    /// authenticated inbound message request.
    pub const fn inbound_message_count(&self) -> usize {
        self.inbound_message_count
    }

    /// Most recent durable timeline entry by timestamp and insertion sequence.
    pub const fn last_message(&self) -> Option<&TimelineEntry> {
        self.last_message.as_ref()
    }
}

/// Work required to reconcile durable outbox state after startup or reconnect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReconcileWork {
    /// No device acceptance was committed, so retry the exact material and
    /// idempotency key.
    Submit {
        /// Stable local outbox record.
        outbox_id: OutboxId,
        /// Exact commit-before-send material.
        material: OutboxMaterial,
    },
    /// Device acceptance was committed and remains nonterminal, so refresh its
    /// status rather than submitting a second semantic message.
    RefreshStatus {
        /// Stable local outbox record.
        outbox_id: OutboxId,
        /// Exact accepted identifiers.
        acceptance: AcceptanceIds,
    },
}
