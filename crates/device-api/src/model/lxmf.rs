//! LXMF messaging and mailbox model.

use core::num::NonZeroU64;

use super::*;

/// Maximum exact normalized LXMF wire bytes returned by one read response.
#[cfg(feature = "lxmf")]
pub const MAX_LXMF_READ_CHUNK_BYTES: usize = 416;
/// Read the next committed LXMF message summary after an optional stable handle.
#[cfg(feature = "lxmf")]
pub const OP_LXMF_NEXT: u16 = 0xf004;
/// Read one bounded chunk of a committed normalized LXMF wire message.
#[cfg(feature = "lxmf")]
pub const OP_LXMF_READ: u16 = 0xf005;
/// Compose and durably submit one basic LXMF message through the local identity.
#[cfg(feature = "lxmf")]
pub const OP_LXMF_BASIC_SEND: u16 = 0xf006;
/// Read one bounded nearby `lxmf.delivery` peer after an optional boot-scoped cursor.
#[cfg(feature = "lxmf")]
pub const OP_LXMF_PEER_NEXT: u16 = 0xf007;
/// Read durable LXMF mailbox collection state.
#[cfg(feature = "lxmf")]
pub const OP_LXMF_MAILBOX_STATUS: u16 = 0xf010;
/// Monotonically acknowledge locally collected LXMF messages.
#[cfg(feature = "lxmf")]
pub const OP_LXMF_MAILBOX_ACKNOWLEDGE: u16 = 0xf011;
/// One phone-sourced location snapshot attached to an LXMF message.
///
/// The fixed-point representation is intentionally identical to the semantic
/// values carried by Sideband's LXMF telemetry location sensor. The device
/// owns the MessagePack encoding and never accepts caller-supplied fields.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfMessageLocation {
    latitude_e6: i32,
    longitude_e6: i32,
    altitude_cm: i32,
    speed_cm_per_second: u32,
    bearing_centidegrees: i32,
    accuracy_cm: u16,
    updated_at_unix_seconds: u32,
}

#[cfg(feature = "lxmf")]
impl LxmfMessageLocation {
    /// Validate one complete Sideband-compatible location sample.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        latitude_e6: i32,
        longitude_e6: i32,
        altitude_cm: i32,
        speed_cm_per_second: u32,
        bearing_centidegrees: i32,
        accuracy_cm: u16,
        updated_at_unix_seconds: u32,
    ) -> Result<Self, InvalidLxmfMessageLocation> {
        if latitude_e6 < -90_000_000 || latitude_e6 > 90_000_000 {
            Err(InvalidLxmfMessageLocation::LatitudeOutOfRange)
        } else if longitude_e6 < -180_000_000 || longitude_e6 > 180_000_000 {
            Err(InvalidLxmfMessageLocation::LongitudeOutOfRange)
        } else {
            Ok(Self {
                latitude_e6,
                longitude_e6,
                altitude_cm,
                speed_cm_per_second,
                bearing_centidegrees,
                accuracy_cm,
                updated_at_unix_seconds,
            })
        }
    }

    /// Latitude in signed decimal microdegrees.
    pub const fn latitude_e6(self) -> i32 {
        self.latitude_e6
    }

    /// Longitude in signed decimal microdegrees.
    pub const fn longitude_e6(self) -> i32 {
        self.longitude_e6
    }

    /// Altitude above mean sea level in centimetres, or zero when unavailable.
    pub const fn altitude_cm(self) -> i32 {
        self.altitude_cm
    }

    /// Ground speed in centimetres per second, or zero when unavailable.
    pub const fn speed_cm_per_second(self) -> u32 {
        self.speed_cm_per_second
    }

    /// Bearing in hundredths of a degree, or zero when unavailable.
    pub const fn bearing_centidegrees(self) -> i32 {
        self.bearing_centidegrees
    }

    /// Horizontal accuracy in centimetres, or zero when unavailable.
    pub const fn accuracy_cm(self) -> u16 {
        self.accuracy_cm
    }

    /// Time of the location fix in whole Unix seconds.
    pub const fn updated_at_unix_seconds(self) -> u32 {
        self.updated_at_unix_seconds
    }
}

/// Why an LXMF message location was rejected.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLxmfMessageLocation {
    /// Latitude was outside the world bounds.
    LatitudeOutOfRange,
    /// Longitude was outside the world bounds.
    LongitudeOutOfRange,
}

/// Stable non-zero handle for one committed inbound LXMF message.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfMessageHandle(NonZeroU64);

#[cfg(feature = "lxmf")]
impl LxmfMessageHandle {
    /// Construct a stable handle from its complete numeric representation.
    pub const fn new(value: u64) -> Result<Self, InvalidLxmfMessageHandle> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(InvalidLxmfMessageHandle),
        }
    }

    /// Complete numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Zero cannot identify a committed LXMF message.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfMessageHandle;

/// Durable appliance-side collection state for the committed LXMF mailbox.
///
/// Message handles are allocated contiguously in commit order. Consequently
/// the difference between the latest and acknowledged handles is the exact
/// number of committed messages not yet collected by the client.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfMailboxStatus {
    latest: Option<LxmfMessageHandle>,
    acknowledged_through: Option<LxmfMessageHandle>,
    uncollected_count: u32,
}

#[cfg(feature = "lxmf")]
impl LxmfMailboxStatus {
    /// Validate and construct one complete mailbox collection snapshot.
    pub const fn new(
        latest: Option<LxmfMessageHandle>,
        acknowledged_through: Option<LxmfMessageHandle>,
    ) -> Result<Self, InvalidLxmfMailboxStatus> {
        let latest_value = match latest {
            Some(handle) => handle.get(),
            None => 0,
        };
        let acknowledged_value = match acknowledged_through {
            Some(handle) => handle.get(),
            None => 0,
        };
        if acknowledged_value > latest_value {
            return Err(InvalidLxmfMailboxStatus::AcknowledgementAhead {
                latest: latest_value,
                acknowledged_through: acknowledged_value,
            });
        }
        let uncollected = latest_value - acknowledged_value;
        if uncollected > u32::MAX as u64 {
            return Err(InvalidLxmfMailboxStatus::CountOverflow { uncollected });
        }
        Ok(Self {
            latest,
            acknowledged_through,
            uncollected_count: uncollected as u32,
        })
    }

    /// Latest committed message, or `None` when the mailbox is empty.
    pub const fn latest(self) -> Option<LxmfMessageHandle> {
        self.latest
    }

    /// Highest committed message durably collected by the client.
    pub const fn acknowledged_through(self) -> Option<LxmfMessageHandle> {
        self.acknowledged_through
    }

    /// Exact number of committed messages after the collection watermark.
    pub const fn uncollected_count(self) -> u32 {
        self.uncollected_count
    }
}

/// Contradictory or unrepresentable LXMF mailbox collection state.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLxmfMailboxStatus {
    /// The collection watermark names a message after the durable tail.
    AcknowledgementAhead {
        /// Latest committed handle, or zero for an empty mailbox.
        latest: u64,
        /// Invalid collection watermark.
        acknowledged_through: u64,
    },
    /// The uncollected difference exceeded the bounded wire representation.
    CountOverflow {
        /// Exact unrepresentable difference.
        uncollected: u64,
    },
}

/// Valid non-zero upper bound for one bounded LXMF read response.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfReadLength(u16);

#[cfg(feature = "lxmf")]
impl LxmfReadLength {
    /// Validate a requested response length against the frozen logical limit.
    pub const fn new(value: u16) -> Result<Self, InvalidLxmfReadLength> {
        if value == 0 || value as usize > MAX_LXMF_READ_CHUNK_BYTES {
            Err(InvalidLxmfReadLength { actual: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Maximum bytes requested from the named offset.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A requested LXMF chunk length was zero or exceeded the logical limit.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfReadLength {
    actual: u16,
}

#[cfg(feature = "lxmf")]
impl InvalidLxmfReadLength {
    /// Rejected requested length.
    pub const fn actual(self) -> u16 {
        self.actual
    }

    /// Maximum accepted requested length.
    pub const fn maximum(self) -> u16 {
        MAX_LXMF_READ_CHUNK_BYTES as u16
    }
}

/// Opaque public token identifying one volatile peer-discovery incarnation.
///
/// The device changes this value whenever its boot-scoped discovery table is
/// reconstructed. It is not a credential or secret.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfPeerDiscoveryIncarnation([u8; 8]);

#[cfg(feature = "lxmf")]
impl LxmfPeerDiscoveryIncarnation {
    /// Construct a token from its complete public wire bytes.
    pub const fn new(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Borrow all public token bytes.
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

/// Exclusive peer-discovery cursor scoped to one device incarnation.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LxmfPeerDiscoveryCursor {
    incarnation: LxmfPeerDiscoveryIncarnation,
    after_generation: u64,
}

#[cfg(feature = "lxmf")]
impl LxmfPeerDiscoveryCursor {
    /// Construct a complete boot-scoped exclusive cursor.
    pub const fn new(incarnation: LxmfPeerDiscoveryIncarnation, after_generation: u64) -> Self {
        Self {
            incarnation,
            after_generation,
        }
    }

    /// Incarnation in which the exclusive generation was observed.
    pub const fn incarnation(self) -> LxmfPeerDiscoveryIncarnation {
        self.incarnation
    }

    /// Exclusive observation generation; zero starts before all observations.
    pub const fn after_generation(self) -> u64 {
        self.after_generation
    }
}

/// Nonzero generation assigned to one retained peer's latest observation.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfPeerGeneration(NonZeroU64);

#[cfg(feature = "lxmf")]
impl LxmfPeerGeneration {
    /// Construct a generation, rejecting the reserved pre-history value zero.
    pub const fn new(value: u64) -> Result<Self, InvalidLxmfPeerGeneration> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(InvalidLxmfPeerGeneration),
        }
    }

    /// Complete nonzero generation value.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Zero is reserved for the cursor before any peer observation.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfPeerGeneration;

/// One retained, authenticated nearby `lxmf.delivery` announce observation.
///
/// This is display and contact-selection evidence, not routing authority.
/// The complete 64-byte Reticulum public key intentionally remains inside the
/// firmware's protocol owner and is not exposed by the local device API.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct LxmfDiscoveredPeer {
    destination: DestinationHash,
    identity_hash: IdentityHash,
    app_data_len: u16,
    app_data: [u8; MAX_LXMF_PEER_APP_DATA_BYTES],
    hops: u8,
    interface_id: ReticulumInterfaceId,
    rssi_dbm: Option<i16>,
    snr_db: Option<i16>,
    observed_age_ms: u64,
    generation: LxmfPeerGeneration,
}

#[cfg(feature = "lxmf")]
impl LxmfDiscoveredPeer {
    /// Copy one bounded, already-authenticated discovery observation.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        destination: DestinationHash,
        identity_hash: IdentityHash,
        app_data: &[u8],
        hops: u8,
        interface_id: ReticulumInterfaceId,
        rssi_dbm: Option<i16>,
        snr_db: Option<i16>,
        observed_age_ms: u64,
        generation: LxmfPeerGeneration,
    ) -> Result<Self, LxmfPeerAppDataTooLarge> {
        if app_data.len() > MAX_LXMF_PEER_APP_DATA_BYTES {
            return Err(LxmfPeerAppDataTooLarge {
                actual: app_data.len(),
            });
        }
        let mut owned = [0_u8; MAX_LXMF_PEER_APP_DATA_BYTES];
        owned[..app_data.len()].copy_from_slice(app_data);
        Ok(Self {
            destination,
            identity_hash,
            app_data_len: app_data.len() as u16,
            app_data: owned,
            hops,
            interface_id,
            rssi_dbm,
            snr_db,
            observed_age_ms,
            generation,
        })
    }

    /// Complete announced `lxmf.delivery` destination hash.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Public hash of the identity that authenticated the announce.
    pub const fn identity_hash(&self) -> IdentityHash {
        self.identity_hash
    }

    /// Exact authenticated announce application data.
    pub fn app_data(&self) -> &[u8] {
        &self.app_data[..self.app_data_len as usize]
    }

    /// Reticulum hop count reported for the latest observation.
    pub const fn hops(&self) -> u8 {
        self.hops
    }

    /// Complete opaque identity of the observing Reticulum interface.
    pub const fn interface_id(&self) -> ReticulumInterfaceId {
        self.interface_id
    }

    /// Observed RSSI in whole dBm, when available.
    pub const fn rssi_dbm(&self) -> Option<i16> {
        self.rssi_dbm
    }

    /// Observed signal-to-noise ratio in whole dB, when available.
    pub const fn snr_db(&self) -> Option<i16> {
        self.snr_db
    }

    /// Saturating age in milliseconds at the response snapshot.
    pub const fn observed_age_ms(&self) -> u64 {
        self.observed_age_ms
    }

    /// Generation of the latest retained observation.
    pub const fn generation(&self) -> LxmfPeerGeneration {
        self.generation
    }
}

#[cfg(feature = "lxmf")]
impl core::fmt::Debug for LxmfDiscoveredPeer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LxmfDiscoveredPeer")
            .field("destination", &self.destination)
            .field("identity_hash", &self.identity_hash)
            .field("app_data_len", &self.app_data_len)
            .field("app_data", &"<redacted>")
            .field("hops", &self.hops)
            .field("interface_id", &self.interface_id)
            .field("rssi_dbm", &self.rssi_dbm)
            .field("snr_db", &self.snr_db)
            .field("observed_age_ms", &self.observed_age_ms)
            .field("generation", &self.generation)
            .finish()
    }
}

/// Authenticated announce application data exceeded the logical response bound.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfPeerAppDataTooLarge {
    actual: usize,
}

#[cfg(feature = "lxmf")]
impl LxmfPeerAppDataTooLarge {
    /// Rejected application-data byte count.
    pub const fn actual(self) -> usize {
        self.actual
    }

    /// Maximum application-data byte count.
    pub const fn maximum(self) -> usize {
        MAX_LXMF_PEER_APP_DATA_BYTES
    }
}

/// One-record page from the volatile nearby-LXMF peer projection.
///
/// A changed incarnation or an ahead-of-history cursor resets the port to its
/// first retained record and sets `history_gap`. The returned next cursor is
/// always scoped to the current incarnation.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfPeerDiscoveryPage {
    next_cursor: LxmfPeerDiscoveryCursor,
    latest_generation: Option<LxmfPeerGeneration>,
    oldest_retained_generation: Option<LxmfPeerGeneration>,
    history_gap: bool,
    peer: Option<LxmfDiscoveredPeer>,
}

#[cfg(feature = "lxmf")]
impl LxmfPeerDiscoveryPage {
    /// Construct one bounded projection page.
    pub const fn new(
        next_cursor: LxmfPeerDiscoveryCursor,
        latest_generation: Option<LxmfPeerGeneration>,
        oldest_retained_generation: Option<LxmfPeerGeneration>,
        history_gap: bool,
        peer: Option<LxmfDiscoveredPeer>,
    ) -> Self {
        Self {
            next_cursor,
            latest_generation,
            oldest_retained_generation,
            history_gap,
            peer,
        }
    }

    /// Exclusive cursor for the next read, scoped to the current incarnation.
    pub const fn next_cursor(&self) -> LxmfPeerDiscoveryCursor {
        self.next_cursor
    }

    /// Latest accepted observation generation when this page was produced.
    pub const fn latest_generation(&self) -> Option<LxmfPeerGeneration> {
        self.latest_generation
    }

    /// Oldest generation represented by a currently retained peer.
    pub const fn oldest_retained_generation(&self) -> Option<LxmfPeerGeneration> {
        self.oldest_retained_generation
    }

    /// Whether requested history was reset, updated away, or evicted.
    pub const fn history_gap(&self) -> bool {
        self.history_gap
    }

    /// One retained peer after the requested cursor, if present.
    pub const fn peer(&self) -> Option<&LxmfDiscoveredPeer> {
        self.peer.as_ref()
    }
}

/// Metadata-only physical-commit-order entry returned by `lxmf.next`.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfMessageSummary {
    handle: LxmfMessageHandle,
    message_id: [u8; 32],
    destination: DestinationHash,
    source: DestinationHash,
    timestamp_bits: u64,
    normalized_wire_len: u32,
    title_len: u32,
    content_len: u32,
    fields_encoded_len: u32,
    exact_wire_sha256: [u8; 32],
    ingress: Option<IngressObservation>,
}

#[cfg(feature = "lxmf")]
const LXMF_NORMALIZED_WIRE_PREFIX_BYTES: u64 = 16 + 16 + 64;
#[cfg(feature = "lxmf")]
const LXMF_PAYLOAD_ARRAY_HEADER_BYTES: u64 = 1;
#[cfg(feature = "lxmf")]
const LXMF_TIMESTAMP_BYTES: u64 = 1 + 8;

#[cfg(feature = "lxmf")]
const fn minimum_msgpack_binary_bytes(decoded_len: u32) -> u64 {
    let header_len = if decoded_len <= u8::MAX as u32 {
        2
    } else if decoded_len <= u16::MAX as u32 {
        3
    } else {
        5
    };
    decoded_len as u64 + header_len
}

#[cfg(feature = "lxmf")]
impl LxmfMessageSummary {
    /// Construct one complete committed-message summary.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        handle: LxmfMessageHandle,
        message_id: [u8; 32],
        destination: DestinationHash,
        source: DestinationHash,
        timestamp_bits: u64,
        normalized_wire_len: u32,
        title_len: u32,
        content_len: u32,
        fields_encoded_len: u32,
        exact_wire_sha256: [u8; 32],
    ) -> Result<Self, InvalidLxmfMessageSummary> {
        // A normalized LXMF wire is the destination, source and signature
        // prefix followed by a four- or five-item MessagePack array. The
        // shortest valid payload has a one-byte array header, one float64
        // timestamp, two binary values with their shortest possible length
        // prefixes, and the exact encoded fields map. A fifth stamp can only
        // increase this lower bound. Perform the sum in u64 so adversarial u32
        // component lengths cannot wrap into an apparently small message.
        let minimum_wire_len = LXMF_NORMALIZED_WIRE_PREFIX_BYTES
            + LXMF_PAYLOAD_ARRAY_HEADER_BYTES
            + LXMF_TIMESTAMP_BYTES
            + minimum_msgpack_binary_bytes(title_len)
            + minimum_msgpack_binary_bytes(content_len)
            + fields_encoded_len as u64;
        if fields_encoded_len == 0 || (normalized_wire_len as u64) < minimum_wire_len {
            return Err(InvalidLxmfMessageSummary);
        }
        Ok(Self {
            handle,
            message_id,
            destination,
            source,
            timestamp_bits,
            normalized_wire_len,
            title_len,
            content_len,
            fields_encoded_len,
            exact_wire_sha256,
            ingress: None,
        })
    }

    /// Attach immutable first-arrival transport evidence.
    pub const fn with_ingress_observation(mut self, ingress: Option<IngressObservation>) -> Self {
        self.ingress = ingress;
        self
    }

    /// Stable committed-message handle.
    pub const fn handle(self) -> LxmfMessageHandle {
        self.handle
    }

    /// Python-compatible LXMF authenticated-message identifier.
    pub const fn message_id(&self) -> &[u8; 32] {
        &self.message_id
    }

    /// Local `lxmf.delivery` destination that received the message.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Authenticated source `lxmf.delivery` destination.
    pub const fn source(self) -> DestinationHash {
        self.source
    }

    /// Exact IEEE-754 bits of the LXMF timestamp.
    pub const fn timestamp_bits(self) -> u64 {
        self.timestamp_bits
    }

    /// Complete normalized wire length.
    pub const fn normalized_wire_len(self) -> u32 {
        self.normalized_wire_len
    }

    /// Decoded title byte length.
    pub const fn title_len(self) -> u32 {
        self.title_len
    }

    /// Decoded content byte length.
    pub const fn content_len(self) -> u32 {
        self.content_len
    }

    /// Encoded MessagePack fields-map length.
    pub const fn fields_encoded_len(self) -> u32 {
        self.fields_encoded_len
    }

    /// SHA-256 of the complete normalized wire representation.
    pub const fn exact_wire_sha256(&self) -> &[u8; 32] {
        &self.exact_wire_sha256
    }

    /// First-arrival interface and optional final-hop signal evidence.
    pub const fn ingress_observation(self) -> Option<IngressObservation> {
        self.ingress
    }
}

/// A committed LXMF summary contained an impossible zero wire length.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfMessageSummary;

/// One owned exact normalized-wire chunk returned by `lxmf.read`.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct LxmfReadChunk {
    handle: LxmfMessageHandle,
    offset: u32,
    total_len: u32,
    bytes_len: u16,
    bytes: [u8; MAX_LXMF_READ_CHUNK_BYTES],
}

#[cfg(feature = "lxmf")]
impl LxmfReadChunk {
    /// Copy and validate one non-empty bounded chunk of a committed message.
    pub fn new(
        handle: LxmfMessageHandle,
        offset: u32,
        total_len: u32,
        bytes: &[u8],
    ) -> Result<Self, InvalidLxmfReadChunk> {
        if bytes.is_empty() {
            return Err(InvalidLxmfReadChunk::Empty);
        }
        if bytes.len() > MAX_LXMF_READ_CHUNK_BYTES {
            return Err(InvalidLxmfReadChunk::TooLarge {
                actual: bytes.len(),
            });
        }
        let end = u64::from(offset) + bytes.len() as u64;
        if total_len == 0 || offset >= total_len || end > u64::from(total_len) {
            return Err(InvalidLxmfReadChunk::OutsideMessage {
                offset,
                length: bytes.len(),
                total_len,
            });
        }
        let mut owned = [0_u8; MAX_LXMF_READ_CHUNK_BYTES];
        owned[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            handle,
            offset,
            total_len,
            bytes_len: bytes.len() as u16,
            bytes: owned,
        })
    }

    /// Stable committed-message handle.
    pub const fn handle(&self) -> LxmfMessageHandle {
        self.handle
    }

    /// Zero-based offset of the first returned byte.
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Complete normalized wire length.
    pub const fn total_len(&self) -> u32 {
        self.total_len
    }

    /// Exact bytes in this response.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.bytes_len as usize]
    }

    /// Whether this chunk reaches the exact end of the committed message.
    pub const fn is_final(&self) -> bool {
        self.offset as u64 + self.bytes_len as u64 == self.total_len as u64
    }
}

#[cfg(feature = "lxmf")]
impl core::fmt::Debug for LxmfReadChunk {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LxmfReadChunk")
            .field("handle", &self.handle)
            .field("offset", &self.offset)
            .field("total_len", &self.total_len)
            .field("bytes_len", &self.bytes_len)
            .finish_non_exhaustive()
    }
}

/// A returned LXMF chunk violated its fixed logical or message boundary.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLxmfReadChunk {
    /// A successful read must make forward progress.
    Empty,
    /// Returned bytes exceeded the fixed response owner.
    TooLarge {
        /// Supplied byte count.
        actual: usize,
    },
    /// Offset and bytes did not fit inside the declared complete message.
    OutsideMessage {
        /// Supplied zero-based offset.
        offset: u32,
        /// Supplied chunk byte count.
        length: usize,
        /// Declared complete normalized wire length.
        total_len: u32,
    },
}

/// Acceptance result for source-free basic LXMF submission.
///
/// The device selects the authenticated local LXMF source. The returned
/// message identifier names the exact composed LXMF message, while the
/// submission identifier is used with [`DeviceRequest::SubmissionStatus`]. A
/// successful response means durable acceptance, not peer delivery.
#[cfg(feature = "lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfBasicSendAccepted {
    /// Device-assigned durable submission identifier.
    pub id: SubmissionId,
    message_id: [u8; 32],
}

#[cfg(feature = "lxmf")]
impl LxmfBasicSendAccepted {
    /// Construct a successful basic-LXMF acceptance result.
    pub const fn new(id: SubmissionId, message_id: [u8; 32]) -> Self {
        Self { id, message_id }
    }

    /// Python-compatible LXMF authenticated-message identifier.
    pub const fn message_id(&self) -> &[u8; 32] {
        &self.message_id
    }
}
