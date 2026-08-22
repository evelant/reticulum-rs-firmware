//! Portable protocol for the product-owned OTA application above PRNS.
//!
//! PRNS carries the control requests and verified Resources opaquely. This
//! module is the single wire-format authority shared by embedded targets and
//! native clients; it contains no transport, executor, flash, or board policy.

/// Identified-Link request path that opens a new OTA transfer.
pub const OTA_START_PATH: &str = "/e290/ota/start";
/// Identified-Link request path that arms exactly the next Resource.
pub const OTA_NEXT_PATH: &str = "/e290/ota/next";
/// Identified-Link request path that reads current OTA progress.
pub const OTA_STATUS_PATH: &str = "/e290/ota/status";
/// Identified-Link request path that reboots into a completely staged image.
pub const OTA_REBOOT_PATH: &str = "/e290/ota/reboot";
/// Canonical MessagePack `nil` supplied by a status request with no arguments.
pub const OTA_STATUS_REQUEST_VALUE: [u8; 1] = [0xc0];
/// Canonical MessagePack `nil` supplied by an explicit reboot request.
pub const OTA_REBOOT_REQUEST_VALUE: [u8; 1] = [0xc0];

/// Current application-level OTA protocol version.
pub const OTA_PROTOCOL_VERSION: u8 = 1;
/// ESP-IDF image-header magic byte.
pub const ESP_IMAGE_MAGIC: u8 = 0xe9;
/// Largest target-version label retained in product state.
pub const OTA_VERSION_BYTES: usize = 32;
/// Smallest image accepted before target-specific slot and structure checks.
pub const MIN_OTA_IMAGE_BYTES: u32 = 64;
/// Exact logical image bytes in a full application Resource.
pub const OTA_IMAGE_CHUNK_BYTES: usize = 7 * 1024;
/// Exact MessagePack metadata bytes carried beside each ordinary PRNS Resource.
///
/// Python represents this as one `bytes` value, encoded as `bin8(30)` followed
/// by the fixed application body. PRNS carries those packed bytes opaquely.
pub const OTA_CHUNK_METADATA_BYTES: usize = 32;
/// Exact bytes in one next-Resource control request.
pub const OTA_NEXT_REQUEST_BYTES: usize = 26;
/// Largest fixed status response including the bounded release label.
pub const OTA_STATUS_RESPONSE_MAX_BYTES: usize = 71;

const OTA_MAGIC: [u8; 4] = *b"EOTA";
const OTA_START_KIND: u8 = 1;
const OTA_NEXT_KIND: u8 = 2;
const OTA_CHUNK_KIND: u8 = 3;

const _: () = assert!(OTA_IMAGE_CHUNK_BYTES.is_multiple_of(4));

/// Opaque identity of one boot-scoped OTA transfer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OtaSessionId([u8; 16]);

impl OtaSessionId {
    /// Construct a session identity from all protocol bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete session identity.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Bootloader-visible firmware slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtaSlot {
    /// ESP-IDF `ota_0` application partition.
    Ota0,
    /// ESP-IDF `ota_1` application partition.
    Ota1,
}

/// Validated bounded release-version label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtaVersion {
    bytes: [u8; OTA_VERSION_BYTES],
    len: u8,
}

impl OtaVersion {
    /// Validate a non-empty printable ASCII release label.
    pub fn try_from_bytes(bytes: &[u8]) -> Result<Self, OtaProtocolError> {
        if bytes.is_empty()
            || bytes.len() > OTA_VERSION_BYTES
            || !bytes.iter().all(|byte| byte.is_ascii_graphic())
        {
            return Err(OtaProtocolError::InvalidVersion);
        }
        let mut version = [0_u8; OTA_VERSION_BYTES];
        version[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: version,
            len: bytes.len() as u8,
        })
    }

    /// Exact validated version bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// Complete release manifest accepted before target-specific slot validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtaManifest {
    image_bytes: u32,
    image_sha256: [u8; 32],
    version: OtaVersion,
}

impl OtaManifest {
    /// Validate the portable image minimum, digest, and release label.
    pub fn new(
        image_bytes: u32,
        image_sha256: [u8; 32],
        version: OtaVersion,
    ) -> Result<Self, OtaProtocolError> {
        if image_bytes < MIN_OTA_IMAGE_BYTES {
            return Err(OtaProtocolError::InvalidImageSize);
        }
        if image_sha256 == [0; 32] {
            return Err(OtaProtocolError::InvalidDigest);
        }
        Ok(Self {
            image_bytes,
            image_sha256,
            version,
        })
    }

    /// Exact complete image byte count.
    pub const fn image_bytes(self) -> u32 {
        self.image_bytes
    }

    /// Manifest SHA-256 over every image byte.
    pub const fn image_sha256(self) -> [u8; 32] {
        self.image_sha256
    }

    /// Validated release-version label.
    pub const fn version(self) -> OtaVersion {
        self.version
    }

    /// Number of application Resources required for this image.
    pub const fn chunk_count(self) -> u32 {
        self.image_bytes.div_ceil(OTA_IMAGE_CHUNK_BYTES as u32)
    }
}

/// Exact metadata accompanying one image-chunk Resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtaChunkMetadata {
    session: OtaSessionId,
    index: u32,
    offset: u32,
}

impl OtaChunkMetadata {
    /// Construct metadata for one ordered application chunk.
    pub const fn new(session: OtaSessionId, index: u32, offset: u32) -> Self {
        Self {
            session,
            index,
            offset,
        }
    }

    /// Bound transfer session.
    pub const fn session(self) -> OtaSessionId {
        self.session
    }

    /// Zero-based ordered chunk index.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Exact zero-based image offset.
    pub const fn offset(self) -> u32 {
        self.offset
    }

    /// Encode fixed metadata for PRNS's opaque Resource metadata field.
    pub fn encode(self) -> [u8; OTA_CHUNK_METADATA_BYTES] {
        let mut output = [0_u8; OTA_CHUNK_METADATA_BYTES];
        output[0] = 0xc4;
        output[1] = 30;
        output[2..6].copy_from_slice(&OTA_MAGIC);
        output[6] = OTA_PROTOCOL_VERSION;
        output[7] = OTA_CHUNK_KIND;
        output[8..24].copy_from_slice(self.session.as_bytes());
        output[24..28].copy_from_slice(&self.index.to_be_bytes());
        output[28..32].copy_from_slice(&self.offset.to_be_bytes());
        output
    }

    /// Decode exact metadata without accepting extensions or trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, OtaProtocolError> {
        if bytes.len() != OTA_CHUNK_METADATA_BYTES
            || bytes[0] != 0xc4
            || bytes[1] != 30
            || bytes[2..6] != OTA_MAGIC
            || bytes[6] != OTA_PROTOCOL_VERSION
            || bytes[7] != OTA_CHUNK_KIND
        {
            return Err(OtaProtocolError::Malformed);
        }
        let mut session = [0_u8; 16];
        session.copy_from_slice(&bytes[8..24]);
        Ok(Self {
            session: OtaSessionId::new(session),
            index: u32::from_be_bytes(bytes[24..28].try_into().expect("fixed index")),
            offset: u32::from_be_bytes(bytes[28..32].try_into().expect("fixed offset")),
        })
    }
}

/// Stable application-protocol failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtaProtocolError {
    /// Bytes did not form the exact selected protocol value.
    Malformed,
    /// Version label was empty, non-printable, or over its fixed bound.
    InvalidVersion,
    /// Manifest image size was below the portable protocol minimum.
    InvalidImageSize,
    /// Manifest used the reserved all-zero digest.
    InvalidDigest,
}

/// Encode one start request for the identified-Link control path.
pub fn encode_start_request(
    manifest: OtaManifest,
    output: &mut [u8],
) -> Result<usize, OtaProtocolError> {
    let needed = 43 + manifest.version().as_bytes().len();
    if output.len() < needed {
        return Err(OtaProtocolError::Malformed);
    }
    output[..4].copy_from_slice(&OTA_MAGIC);
    output[4] = OTA_PROTOCOL_VERSION;
    output[5] = OTA_START_KIND;
    output[6..10].copy_from_slice(&manifest.image_bytes().to_be_bytes());
    output[10..42].copy_from_slice(&manifest.image_sha256());
    output[42] = manifest.version().as_bytes().len() as u8;
    output[43..needed].copy_from_slice(manifest.version().as_bytes());
    Ok(needed)
}

/// Decode one exact start request.
pub fn decode_start_request(bytes: &[u8]) -> Result<OtaManifest, OtaProtocolError> {
    if bytes.len() < 44
        || bytes[..4] != OTA_MAGIC
        || bytes[4] != OTA_PROTOCOL_VERSION
        || bytes[5] != OTA_START_KIND
    {
        return Err(OtaProtocolError::Malformed);
    }
    let version_len = usize::from(bytes[42]);
    if bytes.len() != 43 + version_len {
        return Err(OtaProtocolError::Malformed);
    }
    let image_bytes = u32::from_be_bytes(bytes[6..10].try_into().expect("fixed image size"));
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&bytes[10..42]);
    OtaManifest::new(
        image_bytes,
        digest,
        OtaVersion::try_from_bytes(&bytes[43..])?,
    )
}

/// Encode one request to arm an exact next Resource.
pub fn encode_next_request(session: OtaSessionId, index: u32) -> [u8; OTA_NEXT_REQUEST_BYTES] {
    let mut output = [0_u8; OTA_NEXT_REQUEST_BYTES];
    output[..4].copy_from_slice(&OTA_MAGIC);
    output[4] = OTA_PROTOCOL_VERSION;
    output[5] = OTA_NEXT_KIND;
    output[6..22].copy_from_slice(session.as_bytes());
    output[22..26].copy_from_slice(&index.to_be_bytes());
    output
}

/// Decode one exact next-Resource request.
pub fn decode_next_request(bytes: &[u8]) -> Result<(OtaSessionId, u32), OtaProtocolError> {
    if bytes.len() != OTA_NEXT_REQUEST_BYTES
        || bytes[..4] != OTA_MAGIC
        || bytes[4] != OTA_PROTOCOL_VERSION
        || bytes[5] != OTA_NEXT_KIND
    {
        return Err(OtaProtocolError::Malformed);
    }
    let mut session = [0_u8; 16];
    session.copy_from_slice(&bytes[6..22]);
    Ok((
        OtaSessionId::new(session),
        u32::from_be_bytes(bytes[22..26].try_into().expect("fixed index")),
    ))
}

/// Stable terminal failure retained for management and display projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtaFailure {
    /// Control or Resource bytes violated the application protocol.
    Protocol,
    /// A Resource arrived from a Link other than the bound management Link.
    WrongLink,
    /// Chunk session, index, offset, or byte count was not the armed value.
    UnexpectedChunk,
    /// First staged bytes did not contain an ESP image header.
    InvalidEspImage,
    /// Complete staged bytes did not match the release manifest.
    DigestMismatch,
    /// Flash preparation, write, or readback failed.
    Flash,
    /// Complete staged ESP structure validation failed.
    ImageValidation,
    /// Boot-slot selection failed after complete verification.
    Activation,
    /// The bound Reticulum Link closed before activation.
    Interrupted,
}

/// Public OTA phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtaPhase {
    /// No update has been attempted this boot.
    Idle,
    /// An inactive-slot update is accepting ordered application chunks.
    Receiving,
    /// A verified image was selected for the next boot.
    Activated,
    /// The most recent update attempt ended without activation.
    Failed,
}

/// Copyable management/display projection of coordinator state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtaStatus {
    /// Current public phase.
    pub phase: OtaPhase,
    /// Transfer session when one has been allocated.
    pub session: Option<OtaSessionId>,
    /// Selected inactive slot when known.
    pub slot: Option<OtaSlot>,
    /// Target release version when a manifest was accepted.
    pub version: Option<OtaVersion>,
    /// Complete manifest image bytes.
    pub image_bytes: u32,
    /// Bytes written and verified in the inactive slot.
    pub verified_bytes: u32,
    /// Zero-based chunk expected next.
    pub next_chunk: u32,
    /// Whether PRNS has been armed for exactly that chunk.
    pub resource_armed: bool,
    /// Stable terminal failure when the phase is failed.
    pub failure: Option<OtaFailure>,
}

impl OtaStatus {
    /// Construct the canonical idle projection.
    pub const fn idle() -> Self {
        Self {
            phase: OtaPhase::Idle,
            session: None,
            slot: None,
            version: None,
            image_bytes: 0,
            verified_bytes: 0,
            next_chunk: 0,
            resource_armed: false,
            failure: None,
        }
    }
}

/// Encode one complete coordinator projection for start, next, or status.
pub fn encode_status_response(
    status: OtaStatus,
    output: &mut [u8],
) -> Result<usize, OtaProtocolError> {
    let version = status
        .version
        .map(|version| version.as_bytes().len())
        .unwrap_or(0);
    let needed = 39 + version;
    if output.len() < needed {
        return Err(OtaProtocolError::Malformed);
    }
    output[..4].copy_from_slice(&OTA_MAGIC);
    output[4] = OTA_PROTOCOL_VERSION;
    output[5] = phase_code(status.phase);
    output[6] = u8::from(status.resource_armed);
    let session = status
        .session
        .map(|session| *session.as_bytes())
        .unwrap_or([0; 16]);
    output[7..23].copy_from_slice(&session);
    output[23] = match status.slot {
        None => 0,
        Some(OtaSlot::Ota0) => 1,
        Some(OtaSlot::Ota1) => 2,
    };
    output[24..28].copy_from_slice(&status.image_bytes.to_be_bytes());
    output[28..32].copy_from_slice(&status.verified_bytes.to_be_bytes());
    output[32..36].copy_from_slice(&status.next_chunk.to_be_bytes());
    output[36] = status.failure.map(failure_code).unwrap_or(0);
    output[37] = version as u8;
    output[38] = 0;
    if let Some(version) = status.version {
        output[39..needed].copy_from_slice(version.as_bytes());
    }
    Ok(needed)
}

/// Decode one exact coordinator projection.
pub fn decode_status_response(bytes: &[u8]) -> Result<OtaStatus, OtaProtocolError> {
    if bytes.len() < 39 || bytes[..4] != OTA_MAGIC || bytes[4] != OTA_PROTOCOL_VERSION {
        return Err(OtaProtocolError::Malformed);
    }
    let version_len = usize::from(bytes[37]);
    if bytes[38] != 0 || bytes.len() != 39 + version_len {
        return Err(OtaProtocolError::Malformed);
    }
    let phase = decode_phase(bytes[5])?;
    let resource_armed = match bytes[6] {
        0 => false,
        1 => true,
        _ => return Err(OtaProtocolError::Malformed),
    };
    let session = if bytes[7..23] == [0; 16] {
        None
    } else {
        let mut session = [0_u8; 16];
        session.copy_from_slice(&bytes[7..23]);
        Some(OtaSessionId::new(session))
    };
    let slot = match bytes[23] {
        0 => None,
        1 => Some(OtaSlot::Ota0),
        2 => Some(OtaSlot::Ota1),
        _ => return Err(OtaProtocolError::Malformed),
    };
    let failure = match bytes[36] {
        0 => None,
        code => Some(decode_failure(code)?),
    };
    let version = if version_len == 0 {
        None
    } else {
        Some(OtaVersion::try_from_bytes(&bytes[39..])?)
    };
    Ok(OtaStatus {
        phase,
        session,
        slot,
        version,
        image_bytes: u32::from_be_bytes(bytes[24..28].try_into().expect("fixed image bytes")),
        verified_bytes: u32::from_be_bytes(bytes[28..32].try_into().expect("fixed verified bytes")),
        next_chunk: u32::from_be_bytes(bytes[32..36].try_into().expect("fixed chunk")),
        resource_armed,
        failure,
    })
}

const fn phase_code(phase: OtaPhase) -> u8 {
    match phase {
        OtaPhase::Idle => 0,
        OtaPhase::Receiving => 1,
        OtaPhase::Activated => 2,
        OtaPhase::Failed => 3,
    }
}

fn decode_phase(code: u8) -> Result<OtaPhase, OtaProtocolError> {
    match code {
        0 => Ok(OtaPhase::Idle),
        1 => Ok(OtaPhase::Receiving),
        2 => Ok(OtaPhase::Activated),
        3 => Ok(OtaPhase::Failed),
        _ => Err(OtaProtocolError::Malformed),
    }
}

const fn failure_code(failure: OtaFailure) -> u8 {
    match failure {
        OtaFailure::Protocol => 1,
        OtaFailure::WrongLink => 2,
        OtaFailure::UnexpectedChunk => 3,
        OtaFailure::InvalidEspImage => 4,
        OtaFailure::DigestMismatch => 5,
        OtaFailure::Flash => 6,
        OtaFailure::ImageValidation => 7,
        OtaFailure::Activation => 8,
        OtaFailure::Interrupted => 9,
    }
}

fn decode_failure(code: u8) -> Result<OtaFailure, OtaProtocolError> {
    match code {
        1 => Ok(OtaFailure::Protocol),
        2 => Ok(OtaFailure::WrongLink),
        3 => Ok(OtaFailure::UnexpectedChunk),
        4 => Ok(OtaFailure::InvalidEspImage),
        5 => Ok(OtaFailure::DigestMismatch),
        6 => Ok(OtaFailure::Flash),
        7 => Ok(OtaFailure::ImageValidation),
        8 => Ok(OtaFailure::Activation),
        9 => Ok(OtaFailure::Interrupted),
        _ => Err(OtaProtocolError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_values_round_trip_and_resource_metadata_is_python_msgpack() {
        let version = OtaVersion::try_from_bytes(b"0.2.0-test").unwrap();
        let manifest = OtaManifest::new(9_001, [0x41; 32], version).unwrap();
        let mut start = [0; 75];
        let start_len = encode_start_request(manifest, &mut start).unwrap();
        assert_eq!(decode_start_request(&start[..start_len]), Ok(manifest));

        let session = OtaSessionId::new([0x31; 16]);
        let next = encode_next_request(session, 7);
        assert_eq!(decode_next_request(&next), Ok((session, 7)));

        let metadata = OtaChunkMetadata::new(session, 7, 49).encode();
        assert_eq!(&metadata[..2], &[0xc4, 30]);
        assert_eq!(
            OtaChunkMetadata::decode(&metadata),
            Ok(OtaChunkMetadata::new(session, 7, 49))
        );

        let status = OtaStatus {
            phase: OtaPhase::Receiving,
            session: Some(session),
            slot: Some(OtaSlot::Ota1),
            version: Some(version),
            image_bytes: 9_001,
            verified_bytes: 7_168,
            next_chunk: 1,
            resource_armed: true,
            failure: None,
        };
        let mut encoded = [0; OTA_STATUS_RESPONSE_MAX_BYTES];
        let encoded_len = encode_status_response(status, &mut encoded).unwrap();
        assert_eq!(decode_status_response(&encoded[..encoded_len]), Ok(status));
    }

    #[test]
    fn malformed_extensions_and_reserved_values_are_rejected() {
        assert_eq!(
            OtaManifest::new(63, [1; 32], OtaVersion::try_from_bytes(b"v").unwrap()),
            Err(OtaProtocolError::InvalidImageSize)
        );
        assert_eq!(
            OtaChunkMetadata::decode(&[0xc4, 30]),
            Err(OtaProtocolError::Malformed)
        );
        let mut status = [0; 39];
        status[..4].copy_from_slice(&OTA_MAGIC);
        status[4] = OTA_PROTOCOL_VERSION;
        status[6] = 2;
        assert_eq!(
            decode_status_response(&status),
            Err(OtaProtocolError::Malformed)
        );
    }
}
