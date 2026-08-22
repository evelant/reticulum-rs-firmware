//! Allocation-free durable vocabulary for inbound LXMF messages.
//!
//! This crate owns transport-neutral identifiers, immutable metadata, replay
//! semantics and borrowed normalized-message candidates. It performs no I/O,
//! hashing, parsing or allocation. In particular, a candidate represents exact
//! wire bytes as one or two borrowed slices instead of embedding a board-sized
//! payload buffer.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::fmt;

/// Length of an RNS destination hash used by LXMF.
pub const DESTINATION_HASH_LENGTH: usize = 16;
/// Length of a Python-compatible LXMF message ID.
pub const MESSAGE_ID_LENGTH: usize = 32;
/// Length of the domain-separated authenticated-material fingerprint.
pub const AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH: usize = 32;
/// Length of the SHA-256 digest covering exact normalized LXMF wire bytes.
pub const EXACT_WIRE_DIGEST_LENGTH: usize = 32;

/// Python-compatible LXMF authenticated-message identifier.
///
/// This is `SHA-256(destination || source || payload_without_stamp)`. It
/// therefore identifies one authenticated logical message while deliberately
/// excluding an optional ticket or proof-of-work stamp.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageId([u8; MESSAGE_ID_LENGTH]);

impl MessageId {
    /// Construct an identifier from all digest bytes.
    pub const fn new(bytes: [u8; MESSAGE_ID_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; MESSAGE_ID_LENGTH] {
        &self.0
    }
}

/// Domain-separated digest of LXMF signature-input material.
///
/// The wire/ingress authority calculates this over the complete authenticated
/// material `destination || source || payload_without_stamp` under a
/// project-owned domain separator. Signature verification is recorded
/// separately, matching Python LXMF's ability to deliver a parsed message whose
/// source is unknown or whose signature is invalid. It is intentionally
/// independent of the protocol [`MessageId`], allowing replay logic to fail
/// closed if persisted or deliberately forced input presents one message ID
/// for different signed material. An optional stamp is excluded from both
/// values.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuthenticatedMaterialFingerprint([u8; AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH]);

impl AuthenticatedMaterialFingerprint {
    /// Construct a fingerprint from all domain-separated digest bytes.
    pub const fn new(bytes: [u8; AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all fingerprint bytes.
    pub const fn as_bytes(&self) -> &[u8; AUTHENTICATED_MATERIAL_FINGERPRINT_LENGTH] {
        &self.0
    }
}

/// Python LXMF-compatible signature state for one parsed inbound message.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SignatureVerification {
    /// The source identity was known and its Ed25519 signature validated.
    Validated,
    /// No source identity was known when the message was parsed.
    SourceUnknown,
    /// A known source identity did not validate the supplied signature.
    Invalid,
}

/// Complete RNS destination hash receiving an LXMF message.
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

/// Authenticated source `lxmf.delivery` destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceHash([u8; DESTINATION_HASH_LENGTH]);

impl SourceHash {
    /// Construct a source hash from all bytes.
    pub const fn new(bytes: [u8; DESTINATION_HASH_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all source-hash bytes.
    pub const fn as_bytes(&self) -> &[u8; DESTINATION_HASH_LENGTH] {
        &self.0
    }
}

/// SHA-256 of the exact normalized LXMF wire representation.
///
/// Unlike [`MessageId`], this digest covers the destination prefix, signature
/// and optional stamp exactly as retained. Hash calculation belongs to the
/// durable store so this semantic crate remains dependency-free.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExactWireDigest([u8; EXACT_WIRE_DIGEST_LENGTH]);

impl ExactWireDigest {
    /// Construct a digest from all SHA-256 bytes.
    pub const fn new(bytes: [u8; EXACT_WIRE_DIGEST_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow all digest bytes.
    pub const fn as_bytes(&self) -> &[u8; EXACT_WIRE_DIGEST_LENGTH] {
        &self.0
    }
}

/// Stable logical identifier for one committed inbound message.
///
/// Handles are non-zero durable values allocated by the semantic owner. They
/// are never flash offsets, partition addresses, RAM slot indices or pointers;
/// compaction may therefore move a record without changing its handle.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageHandle(u64);

impl MessageHandle {
    /// Construct a stable non-zero handle.
    pub const fn new(value: u64) -> Result<Self, InvalidMessageHandle> {
        if value == 0 {
            Err(InvalidMessageHandle)
        } else {
            Ok(Self(value))
        }
    }

    /// Complete persisted numeric representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Zero cannot identify a committed inbound message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidMessageHandle;

impl fmt::Display for InvalidMessageHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an inbound message handle must be non-zero")
    }
}

/// First carrier by which the retained message reached this node.
///
/// Carrier provenance is useful arrival evidence, but it is deliberately not
/// part of authenticated logical-message identity. The same LXMF message can
/// validly arrive through several Reticulum transports or carriers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CarrierProvenance {
    /// Complete bytes parsed without more specific carrier evidence.
    Complete,
    /// Destination DATA omitted the local destination prefix.
    Opportunistic,
    /// Context-`NONE` Link DATA carried complete LXMF bytes.
    LinkDataContextNone,
    /// A completed RNS Resource carried complete LXMF bytes.
    ResourceComplete,
}

impl CarrierProvenance {
    /// Whether the retained carrier omitted the normalized destination prefix.
    pub const fn omits_destination_prefix(self) -> bool {
        matches!(self, Self::Opportunistic)
    }
}

/// Receiver-required proof-of-work cost in Python LXMF's enabled range.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequiredStampCost(u8);

impl RequiredStampCost {
    /// Construct a required cost in the inclusive range `1..=254`.
    pub const fn new(value: u16) -> Result<Self, InvalidStampCost> {
        if value == 0 || value >= 255 {
            Err(InvalidStampCost { actual: value })
        } else {
            Ok(Self(value as u8))
        }
    }

    /// Numeric target cost.
    pub const fn get(self) -> u16 {
        self.0 as u16
    }
}

/// Value outside Python LXMF's enabled proof-of-work cost range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidStampCost {
    actual: u16,
}

impl InvalidStampCost {
    /// Rejected numeric value.
    pub const fn actual(self) -> u16 {
        self.actual
    }
}

impl fmt::Display for InvalidStampCost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LXMF stamp cost {} is outside 1..=254",
            self.actual
        )
    }
}

/// Durable evidence for the receiver-owned stamp policy used at admission.
///
/// This is audit provenance rather than logical-message identity. In
/// particular, two valid stamps for the same [`MessageId`] remain replays of
/// one authenticated message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StampAdmissionProvenance {
    /// Receiver policy explicitly did not require stamp validation.
    NotRequired {
        /// Whether a well-formed stamp was present but not consulted.
        stamp_present: bool,
    },
    /// A 16-byte stamp matched a receiver-owned prior ticket.
    TrustedPriorTicket,
    /// A proof-of-work stamp met the inclusive receiver-owned target.
    ProofOfWork {
        /// Required cost at the time of admission.
        target_cost: RequiredStampCost,
        /// Observed Python-compatible leading-zero value.
        observed_value: u16,
    },
}

/// One persisted length field in inbound-message metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageLengthKind {
    /// Complete normalized LXMF bytes.
    NormalizedWire,
    /// Bytes physically supplied by the arrival carrier.
    CarrierPayload,
    /// Decoded title bytes.
    Title,
    /// Decoded content bytes.
    Content,
    /// Exact encoded MessagePack fields map.
    FieldsEncoded,
}

/// A host-size length cannot be represented by the portable durable model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageLengthOverflow {
    kind: MessageLengthKind,
    actual: usize,
}

impl MessageLengthOverflow {
    /// Field which overflowed.
    pub const fn kind(self) -> MessageLengthKind {
        self.kind
    }

    /// Rejected host-size value.
    pub const fn actual(self) -> usize {
        self.actual
    }
}

impl fmt::Display for MessageLengthOverflow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} length {} exceeds the portable u32 representation",
            self.kind, self.actual
        )
    }
}

/// Exact wire and decoded-component lengths retained for one message.
///
/// These values are descriptive evidence, not board ceilings. Admission and
/// storage capacities remain explicit caller-owned profile policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InboundMessageLengths {
    normalized_wire: u32,
    carrier_payload: u32,
    title: u32,
    content: u32,
    fields_encoded: u32,
}

impl InboundMessageLengths {
    /// Convert exact host-size lengths into a portable durable representation.
    pub fn new(
        normalized_wire: usize,
        carrier_payload: usize,
        title: usize,
        content: usize,
        fields_encoded: usize,
    ) -> Result<Self, MessageLengthOverflow> {
        Ok(Self {
            normalized_wire: portable_length(MessageLengthKind::NormalizedWire, normalized_wire)?,
            carrier_payload: portable_length(MessageLengthKind::CarrierPayload, carrier_payload)?,
            title: portable_length(MessageLengthKind::Title, title)?,
            content: portable_length(MessageLengthKind::Content, content)?,
            fields_encoded: portable_length(MessageLengthKind::FieldsEncoded, fields_encoded)?,
        })
    }

    /// Complete normalized LXMF wire length.
    pub const fn normalized_wire(self) -> u32 {
        self.normalized_wire
    }

    /// Exact bytes physically supplied by the arrival carrier.
    pub const fn carrier_payload(self) -> u32 {
        self.carrier_payload
    }

    /// Decoded binary title length.
    pub const fn title(self) -> u32 {
        self.title
    }

    /// Decoded binary content length.
    pub const fn content(self) -> u32 {
        self.content
    }

    /// Exact encoded MessagePack fields-map length.
    pub const fn fields_encoded(self) -> u32 {
        self.fields_encoded
    }
}

fn portable_length(kind: MessageLengthKind, actual: usize) -> Result<u32, MessageLengthOverflow> {
    u32::try_from(actual).map_err(|_| MessageLengthOverflow { kind, actual })
}

/// Inconsistent normalized and carrier byte counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CarrierLengthMismatch {
    carrier: CarrierProvenance,
    normalized_wire: u32,
    carrier_payload: u32,
}

impl CarrierLengthMismatch {
    /// Carrier whose normalization relationship was violated.
    pub const fn carrier(self) -> CarrierProvenance {
        self.carrier
    }

    /// Reported normalized wire length.
    pub const fn normalized_wire(self) -> u32 {
        self.normalized_wire
    }

    /// Reported physical carrier payload length.
    pub const fn carrier_payload(self) -> u32 {
        self.carrier_payload
    }
}

impl fmt::Display for CarrierLengthMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} carrier length {} cannot normalize to {} bytes",
            self.carrier, self.carrier_payload, self.normalized_wire
        )
    }
}

/// Product-local interface identifier that received one LXMF carrier.
///
/// The identifier is meaningful only within the appliance incarnation that
/// committed the message. It deliberately does not encode a concrete
/// transport such as LoRa or TCP; callers resolve the identifier through the
/// device's interface descriptors.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InboundInterfaceId([u8; 8]);

impl InboundInterfaceId {
    /// Preserve one complete product-local interface identifier.
    pub const fn new(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete Reticulum interface identity.
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

/// Receiver-local physical signal evidence for one complete carrier.
///
/// These values describe the final physical hop into this appliance. They do
/// not claim end-to-end signal strength or identify the original sender when
/// Reticulum used one or more relays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InboundSignalObservation {
    rssi_dbm: i16,
    snr_db: i16,
}

impl InboundSignalObservation {
    /// Preserve receiver-reported RSSI and SNR without deriving a quality
    /// percentage.
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

/// First-arrival transport evidence retained with one parsed LXMF message.
///
/// The evidence is deliberately excluded from the authenticated-message
/// fingerprint. A replay over a different route is still the same LXMF
/// message, and the first durable arrival remains authoritative.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InboundTransportObservation {
    interface: InboundInterfaceId,
    signal: Option<InboundSignalObservation>,
}

impl InboundTransportObservation {
    /// Construct one first-arrival observation.
    pub const fn new(
        interface: InboundInterfaceId,
        signal: Option<InboundSignalObservation>,
    ) -> Self {
        Self { interface, signal }
    }

    /// Product-local interface that received the carrier.
    pub const fn interface(self) -> InboundInterfaceId {
        self.interface
    }

    /// Optional receiver-local physical signal evidence.
    pub const fn signal(self) -> Option<InboundSignalObservation> {
        self.signal
    }
}

/// Immutable semantic and first-arrival metadata for one admitted message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InboundMessageMetadata {
    message_id: MessageId,
    authenticated_material: AuthenticatedMaterialFingerprint,
    destination: DestinationHash,
    source: SourceHash,
    signature_verification: SignatureVerification,
    timestamp_bits: u64,
    carrier: CarrierProvenance,
    stamp_admission: StampAdmissionProvenance,
    lengths: InboundMessageLengths,
    ingress: Option<InboundTransportObservation>,
}

impl InboundMessageMetadata {
    /// Construct metadata after validating the carrier normalization lengths.
    ///
    /// The explicit arguments prevent a partially populated or defaulted
    /// durable record at this trust boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        message_id: MessageId,
        authenticated_material: AuthenticatedMaterialFingerprint,
        destination: DestinationHash,
        source: SourceHash,
        signature_verification: SignatureVerification,
        timestamp_bits: u64,
        carrier: CarrierProvenance,
        stamp_admission: StampAdmissionProvenance,
        lengths: InboundMessageLengths,
    ) -> Result<Self, CarrierLengthMismatch> {
        let expected = if carrier.omits_destination_prefix() {
            lengths
                .carrier_payload
                .checked_add(DESTINATION_HASH_LENGTH as u32)
        } else {
            Some(lengths.carrier_payload)
        };
        if expected != Some(lengths.normalized_wire) {
            return Err(CarrierLengthMismatch {
                carrier,
                normalized_wire: lengths.normalized_wire,
                carrier_payload: lengths.carrier_payload,
            });
        }
        Ok(Self {
            message_id,
            authenticated_material,
            destination,
            source,
            signature_verification,
            timestamp_bits,
            carrier,
            stamp_admission,
            lengths,
            ingress: None,
        })
    }

    /// Attach receiver-local evidence before the metadata is durably
    /// committed.
    ///
    /// Callers must invoke this only for the first admitted carrier. Replay
    /// handling compares authenticated material independently and never
    /// replaces an already durable observation.
    pub const fn with_ingress_observation(
        mut self,
        ingress: Option<InboundTransportObservation>,
    ) -> Self {
        self.ingress = ingress;
        self
    }

    /// Python-compatible logical message ID.
    pub const fn message_id(self) -> MessageId {
        self.message_id
    }

    /// Independent digest of the complete authenticated LXMF material.
    pub const fn authenticated_material(self) -> AuthenticatedMaterialFingerprint {
        self.authenticated_material
    }

    /// Local delivery destination bound by the Reticulum carrier.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Source delivery destination claimed by the parsed LXMF message.
    pub const fn source(self) -> SourceHash {
        self.source
    }

    /// Signature state observed while parsing this arrival.
    pub const fn signature_verification(self) -> SignatureVerification {
        self.signature_verification
    }

    /// Exact IEEE-754 timestamp bits retained from MessagePack.
    pub const fn timestamp_bits(self) -> u64 {
        self.timestamp_bits
    }

    /// First carrier observed by this node.
    pub const fn carrier(self) -> CarrierProvenance {
        self.carrier
    }

    /// Receiver-owned stamp admission evidence.
    pub const fn stamp_admission(self) -> StampAdmissionProvenance {
        self.stamp_admission
    }

    /// Exact normalized, carrier and decoded-component lengths.
    pub const fn lengths(self) -> InboundMessageLengths {
        self.lengths
    }

    /// First-arrival interface and optional receiver-local signal evidence.
    pub const fn ingress_observation(self) -> Option<InboundTransportObservation> {
        self.ingress
    }

    /// Logical-message fingerprint used for replay decisions.
    pub const fn authenticated_fingerprint(self) -> AuthenticatedMessageFingerprint {
        AuthenticatedMessageFingerprint {
            message_id: self.message_id,
            authenticated_material: self.authenticated_material,
            destination: self.destination,
            source: self.source,
            timestamp_bits: self.timestamp_bits,
            title_len: self.lengths.title,
            content_len: self.lengths.content,
            fields_encoded_len: self.lengths.fields_encoded,
        }
    }
}

/// Borrowed exact normalized LXMF bytes.
///
/// Opportunistic RNS DATA physically omits the destination hash. Its normalized
/// representation is therefore the logical concatenation of the borrowed
/// destination prefix and borrowed carrier payload. Complete carriers use one
/// contiguous slice.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum NormalizedWire<'a> {
    /// Carrier already contains complete normalized LXMF bytes.
    Contiguous(&'a [u8]),
    /// Opportunistic carrier requiring an implied destination prefix.
    Opportunistic {
        /// Destination authenticated by the Reticulum carrier.
        implied_destination: &'a [u8; DESTINATION_HASH_LENGTH],
        /// Exact physical DATA payload beginning with the LXMF source hash.
        carrier_payload: &'a [u8],
    },
}

impl<'a> NormalizedWire<'a> {
    /// Exact normalized bytes as one or two ordered borrowed segments.
    pub const fn segments(self) -> NormalizedWireSegments<'a> {
        match self {
            Self::Contiguous(bytes) => NormalizedWireSegments {
                first: bytes,
                second: &[],
            },
            Self::Opportunistic {
                implied_destination,
                carrier_payload,
            } => NormalizedWireSegments {
                first: implied_destination,
                second: carrier_payload,
            },
        }
    }

    /// Exact physical carrier payload.
    pub const fn carrier_payload(self) -> &'a [u8] {
        match self {
            Self::Contiguous(bytes) => bytes,
            Self::Opportunistic {
                carrier_payload, ..
            } => carrier_payload,
        }
    }

    /// Whether normalization supplies a separate destination prefix.
    pub const fn is_opportunistic(self) -> bool {
        matches!(self, Self::Opportunistic { .. })
    }
}

impl fmt::Debug for NormalizedWire<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NormalizedWire")
            .field("opportunistic", &self.is_opportunistic())
            .field("segments", &self.segments().segment_count())
            .field("normalized_len", &self.segments().total_len())
            .field("bytes", &"<redacted>")
            .finish()
    }
}

/// Ordered borrowed segments forming one complete normalized LXMF message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NormalizedWireSegments<'a> {
    first: &'a [u8],
    second: &'a [u8],
}

impl<'a> NormalizedWireSegments<'a> {
    /// First segment, containing either the complete wire or destination prefix.
    pub const fn first(self) -> &'a [u8] {
        self.first
    }

    /// Optional second segment, containing opportunistic carrier bytes.
    pub const fn second(self) -> &'a [u8] {
        self.second
    }

    /// Number of segments selected by the representation.
    ///
    /// A contiguous representation reports one even when its borrowed slice
    /// is empty. Candidate construction separately rejects an empty wire.
    pub const fn segment_count(self) -> usize {
        if self.second.is_empty() { 1 } else { 2 }
    }

    /// Checked complete normalized length of both segments.
    pub const fn checked_total_len(self) -> Option<usize> {
        self.first.len().checked_add(self.second.len())
    }

    /// Complete normalized length, saturating only for an impossible address-
    /// space-wide borrowed input.
    pub const fn total_len(self) -> usize {
        self.first.len().saturating_add(self.second.len())
    }
}

/// Inconsistency between validated metadata and its borrowed exact bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateError {
    /// The borrowed carrier shape disagrees with durable carrier provenance.
    CarrierShapeMismatch {
        /// Carrier retained by metadata.
        carrier: CarrierProvenance,
        /// Whether the borrowed bytes use opportunistic segmentation.
        wire_is_opportunistic: bool,
    },
    /// Ordered segment lengths overflowed the target address space.
    NormalizedLengthOverflow,
    /// Borrowed normalized bytes disagree with metadata.
    NormalizedLengthMismatch {
        /// Metadata length.
        expected: u32,
        /// Borrowed length when representable portably.
        actual: usize,
    },
    /// Complete contiguous wire is shorter than its destination prefix.
    MissingDestinationPrefix {
        /// Borrowed normalized length.
        actual: usize,
    },
    /// Borrowed or implied destination disagrees with authenticated metadata.
    DestinationMismatch,
}

impl fmt::Display for CandidateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CarrierShapeMismatch {
                carrier,
                wire_is_opportunistic,
            } => write!(
                formatter,
                "{:?} metadata disagrees with opportunistic wire shape {}",
                carrier, wire_is_opportunistic
            ),
            Self::NormalizedLengthOverflow => {
                formatter.write_str("normalized borrowed LXMF length overflowed usize")
            }
            Self::NormalizedLengthMismatch { expected, actual } => write!(
                formatter,
                "normalized LXMF length {actual} disagrees with metadata {expected}"
            ),
            Self::MissingDestinationPrefix { actual } => write!(
                formatter,
                "complete LXMF wire has {actual} bytes and no destination prefix"
            ),
            Self::DestinationMismatch => {
                formatter.write_str("borrowed LXMF destination disagrees with metadata")
            }
        }
    }
}

/// Fully admitted immutable metadata paired with borrowed exact normalized bytes.
///
/// Construction checks only cross-boundary consistency; cryptographic and
/// MessagePack validation must already have completed in the wire/ingress
/// layer. The value contains no owned message-sized buffer.
#[must_use = "the candidate remains borrowed until a durable store commits it"]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct InboundMessageCandidate<'a> {
    metadata: InboundMessageMetadata,
    wire: NormalizedWire<'a>,
}

impl<'a> InboundMessageCandidate<'a> {
    /// Pair admitted metadata with its exact borrowed normalized bytes.
    pub fn new(
        metadata: InboundMessageMetadata,
        wire: NormalizedWire<'a>,
    ) -> Result<Self, CandidateError> {
        if metadata.carrier().omits_destination_prefix() != wire.is_opportunistic() {
            return Err(CandidateError::CarrierShapeMismatch {
                carrier: metadata.carrier(),
                wire_is_opportunistic: wire.is_opportunistic(),
            });
        }

        let segments = wire.segments();
        let normalized_len = segments
            .checked_total_len()
            .ok_or(CandidateError::NormalizedLengthOverflow)?;
        if normalized_len != metadata.lengths().normalized_wire() as usize {
            return Err(CandidateError::NormalizedLengthMismatch {
                expected: metadata.lengths().normalized_wire(),
                actual: normalized_len,
            });
        }
        let destination = match wire {
            NormalizedWire::Contiguous(bytes) => {
                if bytes.len() < DESTINATION_HASH_LENGTH {
                    return Err(CandidateError::MissingDestinationPrefix {
                        actual: bytes.len(),
                    });
                }
                &bytes[..DESTINATION_HASH_LENGTH]
            }
            NormalizedWire::Opportunistic {
                implied_destination,
                ..
            } => implied_destination,
        };
        if destination != metadata.destination().as_bytes() {
            return Err(CandidateError::DestinationMismatch);
        }

        Ok(Self { metadata, wire })
    }

    /// Immutable admitted metadata.
    pub const fn metadata(self) -> InboundMessageMetadata {
        self.metadata
    }

    /// Exact borrowed normalized wire representation.
    pub const fn wire(self) -> NormalizedWire<'a> {
        self.wire
    }

    /// Ordered borrowed segments for hashing or durable streaming writes.
    pub const fn segments(self) -> NormalizedWireSegments<'a> {
        self.wire.segments()
    }
}

impl fmt::Debug for InboundMessageCandidate<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InboundMessageCandidate")
            .field("metadata", &self.metadata)
            .field("wire", &self.wire)
            .finish()
    }
}

/// Scalar evidence expected to agree for one authenticated [`MessageId`].
///
/// This deliberately excludes exact wire length, carrier and stamp admission:
/// valid alternate stamps and transports are equivalent replays. The redundant
/// authenticated scalars make persisted metadata corruption or a cryptographic
/// ID collision fail closed instead of being silently accepted as a replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedMessageFingerprint {
    message_id: MessageId,
    authenticated_material: AuthenticatedMaterialFingerprint,
    destination: DestinationHash,
    source: SourceHash,
    timestamp_bits: u64,
    title_len: u32,
    content_len: u32,
    fields_encoded_len: u32,
}

impl AuthenticatedMessageFingerprint {
    /// Python-compatible logical message ID.
    pub const fn message_id(self) -> MessageId {
        self.message_id
    }

    /// Independent digest of the complete authenticated LXMF material.
    pub const fn authenticated_material(self) -> AuthenticatedMaterialFingerprint {
        self.authenticated_material
    }

    /// Authenticated receiving destination.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Authenticated source destination.
    pub const fn source(self) -> SourceHash {
        self.source
    }

    /// Exact IEEE-754 timestamp bits.
    pub const fn timestamp_bits(self) -> u64 {
        self.timestamp_bits
    }

    /// Decoded title length.
    pub const fn title_len(self) -> u32 {
        self.title_len
    }

    /// Decoded content length.
    pub const fn content_len(self) -> u32 {
        self.content_len
    }

    /// Exact encoded fields-map length.
    pub const fn fields_encoded_len(self) -> u32 {
        self.fields_encoded_len
    }
}

/// Logical authenticated evidence plus a digest of exact retained bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayFingerprint {
    authenticated: AuthenticatedMessageFingerprint,
    exact_wire_digest: ExactWireDigest,
}

impl ReplayFingerprint {
    /// Construct a replay fingerprint after hashing exact normalized wire bytes.
    pub const fn new(
        authenticated: AuthenticatedMessageFingerprint,
        exact_wire_digest: ExactWireDigest,
    ) -> Self {
        Self {
            authenticated,
            exact_wire_digest,
        }
    }

    /// Authenticated logical-message evidence.
    pub const fn authenticated(self) -> AuthenticatedMessageFingerprint {
        self.authenticated
    }

    /// SHA-256 of exact normalized bytes retained for this arrival.
    pub const fn exact_wire_digest(self) -> ExactWireDigest {
        self.exact_wire_digest
    }

    /// Compare a candidate fingerprint with an already committed fingerprint.
    pub fn classify_against(self, committed: Self) -> ReplayRelation {
        if self.authenticated.message_id != committed.authenticated.message_id {
            ReplayRelation::DistinctMessage
        } else if self.authenticated != committed.authenticated {
            ReplayRelation::AuthenticatedMetadataConflict
        } else if self.exact_wire_digest == committed.exact_wire_digest {
            ReplayRelation::ExactReplay
        } else {
            ReplayRelation::EquivalentReplay
        }
    }
}

/// Relationship between a candidate and one committed message fingerprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayRelation {
    /// Different authenticated message IDs; normal admission may continue.
    DistinctMessage,
    /// Same authenticated evidence and byte-for-byte normalized digest.
    ExactReplay,
    /// Same authenticated message with alternate exact bytes, such as a valid
    /// alternate stamp. Carrier and stamp provenance do not create a new message.
    EquivalentReplay,
    /// Same message ID but contradictory authenticated scalar evidence.
    AuthenticatedMetadataConflict,
}

/// Stable acknowledgement evidence issued by a trusted durable store.
///
/// A receipt contains no physical address and remains valid across compaction.
/// The public constructor supports storage implementations outside this crate;
/// the type itself cannot prove that its caller completed a commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurableMessageReceipt {
    handle: MessageHandle,
    fingerprint: ReplayFingerprint,
}

impl DurableMessageReceipt {
    /// Construct receipt evidence after a storage implementation completes or
    /// recovers one durable commit.
    pub const fn new(handle: MessageHandle, fingerprint: ReplayFingerprint) -> Self {
        Self {
            handle,
            fingerprint,
        }
    }

    /// Stable logical message handle.
    pub const fn handle(self) -> MessageHandle {
        self.handle
    }

    /// Logical and exact-byte replay evidence committed under the handle.
    pub const fn fingerprint(self) -> ReplayFingerprint {
        self.fingerprint
    }

    /// Python-compatible logical message ID.
    pub const fn message_id(self) -> MessageId {
        self.fingerprint.authenticated.message_id
    }
}

#[cfg(test)]
mod tests;
