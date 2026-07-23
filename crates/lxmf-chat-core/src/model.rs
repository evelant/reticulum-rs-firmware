use core::{fmt, num::NonZeroU64};

use std::{string::String, vec::Vec};

/// Bytes in an RNS destination hash.
pub const DESTINATION_HASH_LENGTH: usize = 16;
/// Bytes in a Python-compatible LXMF message identifier.
pub const MESSAGE_ID_LENGTH: usize = 32;
/// Bytes in an LXMF idempotency key used by the device API.
pub const IDEMPOTENCY_KEY_LENGTH: usize = 16;
/// Bytes in a SHA-256 digest of a complete encoded Reticulum packet.
pub const ENCODED_PACKET_SHA256_LENGTH: usize = 32;
/// Bytes in the authenticated public device API identifier.
pub const DEVICE_ID_LENGTH: usize = 16;
/// Largest positive whole-millisecond timestamp that remains injective when
/// converted to LXMF's binary64 seconds representation.
pub const MAX_UNIX_TIMESTAMP_MILLIS: u64 = (1_u64 << 43) * 1_000 - 1;

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

/// Principal-scoped retry key retained across device submission retries.
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
    pub(crate) const fn new(value: u64) -> Option<Self> {
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

/// Fully decoded inbound message material retained by the chat database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundMessage {
    message_id: MessageId,
    local_destination: DestinationHash,
    source: DestinationHash,
    timestamp: UnixTimestampMillis,
    title: Vec<u8>,
    content: Vec<u8>,
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
        }
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
}

/// Persisted inbound record plus stable local ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboundRecord {
    pub(crate) sequence: TimelineSequence,
    pub(crate) message: InboundMessage,
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
}

/// Exact client material that must be committed before a device send attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboxMaterial {
    destination: DestinationHash,
    timestamp: UnixTimestampMillis,
    idempotency_key: IdempotencyKey,
    title: Vec<u8>,
    content: Vec<u8>,
}

impl OutboxMaterial {
    /// Construct exact retry-stable outbound material.
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
        }
    }

    /// Remote `lxmf.delivery` destination.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Exact timestamp retained across retries.
    pub const fn timestamp(&self) -> UnixTimestampMillis {
        self.timestamp
    }

    /// Exact principal-scoped retry key.
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
    /// All released delivery attempts timed out.
    DeliveryTimeout,
    /// A downstream owner rejected the submission.
    DownstreamRejection,
    /// The device reported an internal failure.
    Internal,
}

/// Device-reported lifecycle state for one accepted submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionState {
    /// Accepted work is queued durably.
    Queued,
    /// The device is preparing a Reticulum attempt.
    Preparing,
    /// An attempt was released and is awaiting proof.
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

    /// Binary LXMF title.
    pub fn title(&self) -> &[u8] {
        &self.title
    }

    /// Binary LXMF content.
    pub fn content(&self) -> &[u8] {
        &self.content
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
