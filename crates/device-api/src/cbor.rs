//! Allocation-free, strict CBOR codec for the logical device API.

use minicbor::{Decoder, Encoder, data::Type, encode::write::Cursor};

#[cfg(any(feature = "experimental-rns-data", feature = "experimental-lxmf"))]
use crate::model::IdempotencyKey;
use crate::model::{
    API_VERSION_MAJOR, ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilityAvailability,
    CapabilitySnapshot, DestinationHash, DeviceRequest, DeviceResponse, IdentitySummary,
    MAX_BODY_BYTES, MAX_MESSAGE_BYTES, OP_IDENTITY_SUMMARY, OP_SUBMISSION_STATUS,
    OP_SYSTEM_CAPABILITIES, PreparedPacketDetails, RESPONSE_ERROR, RequestEnvelope, RequestId,
    ResponseEnvelope, SubmissionFailure, SubmissionId, SubmissionState, SubmissionStatus,
};
#[cfg(feature = "experimental-lxmf")]
use crate::model::{
    LxmfBasicSendAccepted, LxmfMessageHandle, LxmfMessageSummary, LxmfReadChunk, LxmfReadLength,
    MAX_LXMF_BASIC_CONTENT_BYTES, MAX_LXMF_BASIC_TITLE_BYTES, MAX_LXMF_READ_CHUNK_BYTES,
    OP_EXPERIMENTAL_LXMF_BASIC_SEND, OP_EXPERIMENTAL_LXMF_NEXT, OP_EXPERIMENTAL_LXMF_READ,
};
#[cfg(feature = "experimental-rns-inbox")]
use crate::model::{
    MAX_RNS_INBOX_PAYLOAD_BYTES, OP_EXPERIMENTAL_RNS_INBOX_PEEK, OP_EXPERIMENTAL_RNS_INBOX_STATUS,
    RnsInboxItem, RnsInboxStatus,
};
#[cfg(feature = "experimental-rns-data")]
use crate::model::{
    MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES, OP_EXPERIMENTAL_SUBMIT_RNS_DATA, SubmissionAccepted,
};

const MAX_MAP_ENTRIES: u64 = 32;
/// Maximum container/tag nesting accepted while validating an operation body
/// or skipping an unknown field value.
pub const MAX_CBOR_NESTING_DEPTH: usize = 8;

/// A known field whose absence, duplication, length, or value is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredField {
    /// Envelope version map at key 0.
    EnvelopeVersion,
    /// Envelope request identifier at key 1.
    EnvelopeRequestId,
    /// Envelope operation/response kind at key 2.
    EnvelopeKind,
    /// Envelope operation-specific body at key 3.
    EnvelopeBody,
    /// Version major at key 0.
    VersionMajor,
    /// Version minor at key 1.
    VersionMinor,
    /// Submission identifier at body key 0.
    SubmissionId,
    /// Experimental submission destination hash at body key 0.
    SubmitDestination,
    /// Experimental submission payload at body key 1.
    SubmitPayload,
    /// Experimental submission idempotency key at body key 2.
    SubmitIdempotencyKey,
    /// Capability API version at body key 0.
    CapabilityApiVersion,
    /// Capability raw packet-output flag at body key 1.
    CapabilityPacketOutput,
    /// Capability direct-radio-TX availability at body key 2.
    CapabilityDirectRadioTx,
    /// Capability experimental outbound RNS DATA submission flag at body key 3.
    CapabilityExperimentalSubmit,
    /// Capability logical message limit at body key 4.
    CapabilityMaxMessageBytes,
    /// Capability body limit at body key 5.
    CapabilityMaxBodyBytes,
    /// Capability experimental submission payload limit at body key 6.
    CapabilityMaxSubmitPayloadBytes,
    /// Capability experimental inbound RNS mailbox availability at body key 7.
    CapabilityExperimentalRnsInbox,
    /// Capability experimental inbound RNS mailbox payload limit at body key 8.
    CapabilityMaxRnsInboxPayloadBytes,
    /// Capability experimental LXMF read availability at body key 9.
    CapabilityExperimentalLxmf,
    /// Capability maximum LXMF read chunk bytes at body key 10.
    CapabilityMaxLxmfReadChunkBytes,
    /// Capability basic LXMF send availability at body key 11.
    CapabilityExperimentalLxmfBasicSend,
    /// Capability maximum basic LXMF title bytes at body key 12.
    CapabilityMaxLxmfBasicTitleBytes,
    /// Capability maximum basic LXMF content bytes at body key 13.
    CapabilityMaxLxmfBasicContentBytes,
    /// Identity summary primary destination hash at body key 0.
    IdentityPrimaryDestination,
    /// Optional identity summary `lxmf.delivery` destination hash at body key 1.
    IdentityLxmfDeliveryDestination,
    /// Inbound RNS mailbox depth at body key 0.
    RnsInboxDepth,
    /// Inbound RNS mailbox capacity at body key 1.
    RnsInboxCapacity,
    /// Inbound RNS mailbox dropped counter at body key 2.
    RnsInboxDroppedSinceBoot,
    /// Inbound RNS mailbox payload limit at body key 3.
    RnsInboxMaxPayloadBytes,
    /// Inbound RNS mailbox durability flag at body key 4.
    RnsInboxDurable,
    /// Inbound RNS mailbox item identifier at body key 0.
    RnsInboxItemId,
    /// Inbound RNS mailbox destination hash at body key 1.
    RnsInboxDestination,
    /// Inbound RNS mailbox payload at body key 2.
    RnsInboxPayload,
    /// Optional exclusive LXMF listing cursor at request body key 0.
    LxmfAfterHandle,
    /// Stable committed LXMF message handle.
    LxmfHandle,
    /// Python-compatible LXMF message ID.
    LxmfMessageId,
    /// Local LXMF delivery destination.
    LxmfDestination,
    /// Authenticated LXMF source destination.
    LxmfSource,
    /// Exact LXMF timestamp bits.
    LxmfTimestampBits,
    /// Complete normalized LXMF wire length.
    LxmfNormalizedWireLength,
    /// Decoded LXMF title length.
    LxmfTitleLength,
    /// Decoded LXMF content length.
    LxmfContentLength,
    /// Encoded LXMF fields-map length.
    LxmfFieldsEncodedLength,
    /// SHA-256 of exact normalized LXMF wire bytes.
    LxmfExactWireSha256,
    /// Zero-based LXMF read offset.
    LxmfReadOffset,
    /// Requested maximum LXMF read bytes.
    LxmfReadMaxBytes,
    /// Exact bytes returned by an LXMF read.
    LxmfReadBytes,
    /// Basic LXMF send destination at request body key 0.
    LxmfBasicSendDestination,
    /// Basic LXMF send Unix-millisecond timestamp at request body key 1.
    LxmfBasicSendTimestampUnixMs,
    /// Basic LXMF send binary title at request body key 2.
    LxmfBasicSendTitle,
    /// Basic LXMF send binary content at request body key 3.
    LxmfBasicSendContent,
    /// Basic LXMF send idempotency key at request body key 4.
    LxmfBasicSendIdempotencyKey,
    /// Submission state at body key 1.
    SubmissionState,
    /// State-specific prepared packet length at body key 2.
    SubmissionPacketLength,
    /// State-specific encoded-packet SHA-256 at body key 3.
    SubmissionEncodedPacketSha256,
    /// Failed-state submission category at body key 4.
    SubmissionFailure,
    /// API error code at body key 0.
    ErrorCode,
    /// Optional API error operation at body key 1.
    ErrorOperation,
}

/// Failure to decode exactly one bounded logical API message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Input exceeds [`MAX_MESSAGE_BYTES`].
    MessageTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Encoded operation body exceeds [`MAX_BODY_BYTES`].
    BodyTooLarge {
        /// Supplied encoded body byte count.
        actual: usize,
        /// Accepted encoded body byte count.
        max: usize,
    },
    /// Input is not the expected, definite-map CBOR shape.
    Malformed,
    /// An indefinite-length byte string, text string, array, or map appeared.
    IndefiniteLength,
    /// A body or unknown field exceeds [`MAX_CBOR_NESTING_DEPTH`].
    NestingTooDeep {
        /// Attempted container/tag nesting depth.
        actual: usize,
        /// Accepted container/tag nesting depth.
        max: usize,
    },
    /// One definite map contains too many fields for bounded processing.
    TooManyMapEntries {
        /// Declared number of fields.
        actual: u64,
        /// Accepted number of fields.
        max: u64,
    },
    /// Complete CBOR item was followed by additional bytes.
    TrailingData,
    /// Required known field was absent.
    MissingField(RequiredField),
    /// Known field appeared more than once.
    DuplicateField(RequiredField),
    /// Fixed-width byte string had the wrong length.
    InvalidByteStringLength {
        /// Field being decoded.
        field: RequiredField,
        /// Required byte count.
        expected: usize,
        /// Supplied byte count.
        actual: usize,
    },
    /// Experimental application payload exceeds its semantic limit.
    PayloadTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// An inbound RNS mailbox payload exceeds its fixed response limit.
    InboxPayloadTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// An LXMF read response exceeded its fixed owned chunk limit.
    LxmfReadChunkTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// A basic LXMF title exceeded its individual semantic limit.
    LxmfBasicTitleTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Basic LXMF content exceeded its individual semantic limit.
    LxmfBasicContentTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// An LXMF summary contained a semantically impossible value combination.
    InvalidLxmfMessageSummary,
    /// An LXMF read response did not fit its declared complete message boundary.
    InvalidLxmfReadChunk,
    /// Envelope selected an incompatible protocol major version.
    UnsupportedVersion(ApiVersion),
    /// Request selected an unknown or unavailable operation.
    UnsupportedOperation(u16),
    /// Response selected an unknown response kind.
    UnsupportedResponseKind(u16),
    /// Submission state and state-specific fields contradict one another.
    InvalidSubmissionStatus,
    /// Known numeric enum field contained an unknown value.
    InvalidValue {
        /// Field being decoded.
        field: RequiredField,
        /// Unsupported numeric value.
        value: u64,
    },
}

/// Failure to encode a bounded logical API message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// Caller-provided output buffer cannot hold the canonical message.
    OutputTooSmall,
    /// Experimental application payload exceeds its semantic limit.
    PayloadTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Encoded operation body exceeds [`MAX_BODY_BYTES`].
    BodyTooLarge {
        /// Required encoded body byte count.
        actual: usize,
        /// Accepted encoded body byte count.
        max: usize,
    },
    /// A basic LXMF title exceeded its individual semantic limit.
    LxmfBasicTitleTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Basic LXMF content exceeded its individual semantic limit.
    LxmfBasicContentTooLarge {
        /// Supplied byte count.
        actual: usize,
        /// Accepted byte count.
        max: usize,
    },
    /// Envelope selected an incompatible protocol major version.
    UnsupportedVersion(ApiVersion),
}

macro_rules! put {
    ($expression:expr) => {
        $expression.map_err(|_| EncodeError::OutputTooSmall)?
    };
}

/// Encode one request as canonical, definite-map CBOR into `output`.
///
/// The returned count never exceeds [`MAX_MESSAGE_BYTES`].
pub fn encode_request(
    envelope: &RequestEnvelope<'_>,
    output: &mut [u8],
) -> Result<usize, EncodeError> {
    check_encode_version(envelope.version)?;
    #[cfg(feature = "experimental-rns-data")]
    if let DeviceRequest::SubmitRnsData { payload, .. } = envelope.request
        && payload.len() > MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES
    {
        return Err(EncodeError::PayloadTooLarge {
            actual: payload.len(),
            max: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES,
        });
    }
    #[cfg(feature = "experimental-lxmf")]
    if let DeviceRequest::LxmfBasicSend {
        timestamp_unix_ms,
        title,
        content,
        ..
    } = envelope.request
    {
        if title.len() > MAX_LXMF_BASIC_TITLE_BYTES {
            return Err(EncodeError::LxmfBasicTitleTooLarge {
                actual: title.len(),
                max: MAX_LXMF_BASIC_TITLE_BYTES,
            });
        }
        if content.len() > MAX_LXMF_BASIC_CONTENT_BYTES {
            return Err(EncodeError::LxmfBasicContentTooLarge {
                actual: content.len(),
                max: MAX_LXMF_BASIC_CONTENT_BYTES,
            });
        }
        let body_len = lxmf_basic_send_body_len(timestamp_unix_ms, title.len(), content.len());
        if body_len > MAX_BODY_BYTES {
            return Err(EncodeError::BodyTooLarge {
                actual: body_len,
                max: MAX_BODY_BYTES,
            });
        }
    }

    let capacity = output.len().min(MAX_MESSAGE_BYTES);
    let mut encoder = Encoder::new(Cursor::new(&mut output[..capacity]));
    put!(encoder.map(4));
    put!(encoder.u8(0));
    encode_version(&mut encoder, envelope.version)?;
    put!(encoder.u8(1));
    put!(encoder.u64(envelope.request_id.0));
    put!(encoder.u8(2));
    put!(encoder.u16(envelope.request.operation()));
    put!(encoder.u8(3));
    match envelope.request {
        DeviceRequest::SystemCapabilities => {
            put!(encoder.map(0));
        }
        DeviceRequest::IdentitySummary => {
            put!(encoder.map(0));
        }
        DeviceRequest::SubmissionStatus { id } => {
            put!(encoder.map(1));
            put!(encoder.u8(0));
            put!(encoder.u64(id.0));
        }
        #[cfg(feature = "experimental-rns-inbox")]
        DeviceRequest::RnsInboxStatus | DeviceRequest::RnsInboxPeek => {
            put!(encoder.map(0));
        }
        #[cfg(feature = "experimental-lxmf")]
        DeviceRequest::LxmfNext { after } => {
            put!(encoder.map(u64::from(after.is_some())));
            if let Some(after) = after {
                put!(encoder.u8(0));
                put!(encoder.u64(after.get()));
            }
        }
        #[cfg(feature = "experimental-lxmf")]
        DeviceRequest::LxmfRead {
            handle,
            offset,
            max_bytes,
        } => {
            put!(encoder.map(3));
            put!(encoder.u8(0));
            put!(encoder.u64(handle.get()));
            put!(encoder.u8(1));
            put!(encoder.u32(offset));
            put!(encoder.u8(2));
            put!(encoder.u16(max_bytes.get()));
        }
        #[cfg(feature = "experimental-lxmf")]
        DeviceRequest::LxmfBasicSend {
            destination,
            timestamp_unix_ms,
            title,
            content,
            idempotency_key,
        } => {
            put!(encoder.map(5));
            put!(encoder.u8(0));
            put!(encoder.bytes(&destination.0));
            put!(encoder.u8(1));
            put!(encoder.u64(timestamp_unix_ms));
            put!(encoder.u8(2));
            put!(encoder.bytes(title));
            put!(encoder.u8(3));
            put!(encoder.bytes(content));
            put!(encoder.u8(4));
            put!(encoder.bytes(&idempotency_key.0));
        }
        #[cfg(feature = "experimental-rns-data")]
        DeviceRequest::SubmitRnsData {
            destination,
            payload,
            idempotency_key,
        } => {
            put!(encoder.map(3));
            put!(encoder.u8(0));
            put!(encoder.bytes(&destination.0));
            put!(encoder.u8(1));
            put!(encoder.bytes(payload));
            put!(encoder.u8(2));
            put!(encoder.bytes(&idempotency_key.0));
        }
        DeviceRequest::__Borrowed(never, _) => match never {},
    }
    Ok(encoder.writer().position())
}

/// Decode exactly one request while borrowing any byte-string payload.
pub fn decode_request(input: &[u8]) -> Result<RequestEnvelope<'_>, DecodeError> {
    check_message_size(input)?;
    let mut decoder = Decoder::new(input);
    let entries = decode_map_len(&mut decoder)?;

    let mut version = None;
    let mut request_id = None;
    let mut operation = None;
    let mut body = None;

    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(version.is_some(), RequiredField::EnvelopeVersion)?;
                version = Some(decode_version(&mut decoder)?);
            }
            1 => {
                reject_duplicate(request_id.is_some(), RequiredField::EnvelopeRequestId)?;
                request_id = Some(RequestId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            2 => {
                reject_duplicate(operation.is_some(), RequiredField::EnvelopeKind)?;
                operation = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(body.is_some(), RequiredField::EnvelopeBody)?;
                body = Some(capture_body(input, &mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_item(&decoder, input)?;

    let version = require(version, RequiredField::EnvelopeVersion)?;
    check_version(version)?;
    let request_id = require(request_id, RequiredField::EnvelopeRequestId)?;
    let operation = require(operation, RequiredField::EnvelopeKind)?;
    let body = require(body, RequiredField::EnvelopeBody)?;
    let request = decode_request_body(operation, body)?;
    Ok(RequestEnvelope {
        version,
        request_id,
        request,
    })
}

/// Encode one response as canonical, definite-map CBOR into `output`.
pub fn encode_response(
    envelope: &ResponseEnvelope,
    output: &mut [u8],
) -> Result<usize, EncodeError> {
    check_encode_version(envelope.version)?;
    if let DeviceResponse::SystemCapabilities(capabilities) = envelope.response {
        check_encode_version(capabilities.api_version)?;
    }
    let capacity = output.len().min(MAX_MESSAGE_BYTES);
    let mut encoder = Encoder::new(Cursor::new(&mut output[..capacity]));
    put!(encoder.map(4));
    put!(encoder.u8(0));
    encode_version(&mut encoder, envelope.version)?;
    put!(encoder.u8(1));
    put!(encoder.u64(envelope.request_id.0));
    put!(encoder.u8(2));
    put!(encoder.u16(envelope.response.kind()));
    put!(encoder.u8(3));
    match envelope.response {
        DeviceResponse::SystemCapabilities(capabilities) => {
            encode_capabilities(&mut encoder, capabilities)?;
        }
        DeviceResponse::IdentitySummary(summary) => {
            encode_identity_summary(&mut encoder, summary)?;
        }
        DeviceResponse::SubmissionStatus(status) => {
            encode_submission_status(&mut encoder, status)?;
        }
        #[cfg(feature = "experimental-rns-inbox")]
        DeviceResponse::RnsInboxStatus(status) => {
            encode_rns_inbox_status(&mut encoder, status)?;
        }
        #[cfg(feature = "experimental-rns-inbox")]
        DeviceResponse::RnsInboxPeek(item) => {
            encode_rns_inbox_item(&mut encoder, &item)?;
        }
        #[cfg(feature = "experimental-lxmf")]
        DeviceResponse::LxmfNext(summary) => {
            encode_lxmf_summary(&mut encoder, summary)?;
        }
        #[cfg(feature = "experimental-lxmf")]
        DeviceResponse::LxmfRead(chunk) => {
            encode_lxmf_read_chunk(&mut encoder, &chunk)?;
        }
        #[cfg(feature = "experimental-lxmf")]
        DeviceResponse::LxmfBasicSendAccepted(accepted) => {
            encode_lxmf_basic_send_accepted(&mut encoder, accepted)?;
        }
        #[cfg(feature = "experimental-rns-data")]
        DeviceResponse::SubmitRnsDataAccepted(accepted) => {
            encode_submission_accepted(&mut encoder, accepted)?;
        }
        DeviceResponse::Error(error) => encode_error(&mut encoder, error)?,
    }
    Ok(encoder.writer().position())
}

/// Decode exactly one response and reject duplicate known fields.
pub fn decode_response(input: &[u8]) -> Result<ResponseEnvelope, DecodeError> {
    check_message_size(input)?;
    let mut decoder = Decoder::new(input);
    let entries = decode_map_len(&mut decoder)?;

    let mut version = None;
    let mut request_id = None;
    let mut kind = None;
    let mut body = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(version.is_some(), RequiredField::EnvelopeVersion)?;
                version = Some(decode_version(&mut decoder)?);
            }
            1 => {
                reject_duplicate(request_id.is_some(), RequiredField::EnvelopeRequestId)?;
                request_id = Some(RequestId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            2 => {
                reject_duplicate(kind.is_some(), RequiredField::EnvelopeKind)?;
                kind = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(body.is_some(), RequiredField::EnvelopeBody)?;
                body = Some(capture_body(input, &mut decoder)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_item(&decoder, input)?;

    let version = require(version, RequiredField::EnvelopeVersion)?;
    check_version(version)?;
    let request_id = require(request_id, RequiredField::EnvelopeRequestId)?;
    let kind = require(kind, RequiredField::EnvelopeKind)?;
    let body = require(body, RequiredField::EnvelopeBody)?;
    let response = match kind {
        OP_SYSTEM_CAPABILITIES => DeviceResponse::SystemCapabilities(decode_capabilities(body)?),
        OP_IDENTITY_SUMMARY => DeviceResponse::IdentitySummary(decode_identity_summary(body)?),
        OP_SUBMISSION_STATUS => DeviceResponse::SubmissionStatus(decode_submission_status(body)?),
        #[cfg(feature = "experimental-rns-inbox")]
        OP_EXPERIMENTAL_RNS_INBOX_STATUS => {
            DeviceResponse::RnsInboxStatus(decode_rns_inbox_status(body)?)
        }
        #[cfg(feature = "experimental-rns-inbox")]
        OP_EXPERIMENTAL_RNS_INBOX_PEEK => {
            DeviceResponse::RnsInboxPeek(decode_rns_inbox_item(body)?)
        }
        #[cfg(feature = "experimental-lxmf")]
        OP_EXPERIMENTAL_LXMF_NEXT => DeviceResponse::LxmfNext(decode_lxmf_summary(body)?),
        #[cfg(feature = "experimental-lxmf")]
        OP_EXPERIMENTAL_LXMF_READ => DeviceResponse::LxmfRead(decode_lxmf_read_chunk(body)?),
        #[cfg(feature = "experimental-lxmf")]
        OP_EXPERIMENTAL_LXMF_BASIC_SEND => {
            DeviceResponse::LxmfBasicSendAccepted(decode_lxmf_basic_send_accepted(body)?)
        }
        #[cfg(feature = "experimental-rns-data")]
        OP_EXPERIMENTAL_SUBMIT_RNS_DATA => {
            DeviceResponse::SubmitRnsDataAccepted(decode_submission_accepted(body)?)
        }
        RESPONSE_ERROR => DeviceResponse::Error(decode_error(body)?),
        other => return Err(DecodeError::UnsupportedResponseKind(other)),
    };
    Ok(ResponseEnvelope {
        version,
        request_id,
        response,
    })
}

type SliceEncoder<'a> = Encoder<Cursor<&'a mut [u8]>>;

#[cfg(feature = "experimental-lxmf")]
const fn cbor_u64_len(value: u64) -> usize {
    match value {
        0..=23 => 1,
        24..=0xff => 2,
        0x100..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

#[cfg(feature = "experimental-lxmf")]
const fn cbor_bytes_len(length: usize) -> usize {
    let header = match length {
        0..=23 => 1,
        24..=0xff => 2,
        0x100..=0xffff => 3,
        _ => 5,
    };
    header + length
}

#[cfg(feature = "experimental-lxmf")]
const fn lxmf_basic_send_body_len(timestamp_unix_ms: u64, title: usize, content: usize) -> usize {
    // map header + five single-byte keys + two fixed 16-byte strings
    1 + 5
        + 17
        + cbor_u64_len(timestamp_unix_ms)
        + cbor_bytes_len(title)
        + cbor_bytes_len(content)
        + 17
}

fn encode_version(encoder: &mut SliceEncoder<'_>, version: ApiVersion) -> Result<(), EncodeError> {
    check_encode_version(version)?;
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.u16(version.major));
    put!(encoder.u8(1));
    put!(encoder.u16(version.minor));
    Ok(())
}

fn encode_capabilities(
    encoder: &mut SliceEncoder<'_>,
    capabilities: CapabilitySnapshot,
) -> Result<(), EncodeError> {
    put!(encoder.map(14));
    put!(encoder.u8(0));
    encode_version(encoder, capabilities.api_version)?;
    put!(encoder.u8(1));
    put!(encoder.bool(capabilities.packet_output));
    put!(encoder.u8(2));
    put!(encoder.u8(capabilities.direct_radio_tx.wire_code()));
    put!(encoder.u8(3));
    put!(encoder.bool(capabilities.experimental_submit_rns_data));
    put!(encoder.u8(4));
    put!(encoder.u16(capabilities.max_message_bytes));
    put!(encoder.u8(5));
    put!(encoder.u16(capabilities.max_body_bytes));
    put!(encoder.u8(6));
    put!(encoder.u16(capabilities.max_submit_rns_data_payload_bytes));
    put!(encoder.u8(7));
    put!(encoder.u8(capabilities.experimental_rns_inbox.wire_code()));
    put!(encoder.u8(8));
    put!(encoder.u16(capabilities.max_rns_inbox_payload_bytes));
    put!(encoder.u8(9));
    put!(encoder.u8(capabilities.experimental_lxmf.wire_code()));
    put!(encoder.u8(10));
    put!(encoder.u16(capabilities.max_lxmf_read_chunk_bytes));
    put!(encoder.u8(11));
    put!(encoder.u8(capabilities.experimental_lxmf_basic_send.wire_code()));
    put!(encoder.u8(12));
    put!(encoder.u16(capabilities.max_lxmf_basic_title_bytes));
    put!(encoder.u8(13));
    put!(encoder.u16(capabilities.max_lxmf_basic_content_bytes));
    Ok(())
}

#[cfg(feature = "experimental-rns-inbox")]
fn encode_rns_inbox_status(
    encoder: &mut SliceEncoder<'_>,
    status: RnsInboxStatus,
) -> Result<(), EncodeError> {
    put!(encoder.map(5));
    put!(encoder.u8(0));
    put!(encoder.u16(status.depth));
    put!(encoder.u8(1));
    put!(encoder.u16(status.capacity));
    put!(encoder.u8(2));
    put!(encoder.u64(status.dropped_since_boot));
    put!(encoder.u8(3));
    put!(encoder.u16(status.max_payload_bytes));
    put!(encoder.u8(4));
    put!(encoder.bool(status.durable));
    Ok(())
}

#[cfg(feature = "experimental-rns-inbox")]
fn encode_rns_inbox_item(
    encoder: &mut SliceEncoder<'_>,
    item: &RnsInboxItem,
) -> Result<(), EncodeError> {
    put!(encoder.map(3));
    put!(encoder.u8(0));
    put!(encoder.u64(item.id()));
    put!(encoder.u8(1));
    put!(encoder.bytes(&item.destination().0));
    put!(encoder.u8(2));
    put!(encoder.bytes(item.payload()));
    Ok(())
}

fn encode_identity_summary(
    encoder: &mut SliceEncoder<'_>,
    summary: IdentitySummary,
) -> Result<(), EncodeError> {
    put!(encoder.map(1 + u64::from(summary.lxmf_delivery_destination().is_some())));
    put!(encoder.u8(0));
    put!(encoder.bytes(&summary.primary_destination().0));
    if let Some(destination) = summary.lxmf_delivery_destination() {
        put!(encoder.u8(1));
        put!(encoder.bytes(&destination.0));
    }
    Ok(())
}

#[cfg(feature = "experimental-lxmf")]
fn encode_lxmf_summary(
    encoder: &mut SliceEncoder<'_>,
    summary: LxmfMessageSummary,
) -> Result<(), EncodeError> {
    put!(encoder.map(10));
    put!(encoder.u8(0));
    put!(encoder.u64(summary.handle().get()));
    put!(encoder.u8(1));
    put!(encoder.bytes(summary.message_id()));
    put!(encoder.u8(2));
    put!(encoder.bytes(&summary.destination().0));
    put!(encoder.u8(3));
    put!(encoder.bytes(&summary.source().0));
    put!(encoder.u8(4));
    put!(encoder.u64(summary.timestamp_bits()));
    put!(encoder.u8(5));
    put!(encoder.u32(summary.normalized_wire_len()));
    put!(encoder.u8(6));
    put!(encoder.u32(summary.title_len()));
    put!(encoder.u8(7));
    put!(encoder.u32(summary.content_len()));
    put!(encoder.u8(8));
    put!(encoder.u32(summary.fields_encoded_len()));
    put!(encoder.u8(9));
    put!(encoder.bytes(summary.exact_wire_sha256()));
    Ok(())
}

#[cfg(feature = "experimental-lxmf")]
fn encode_lxmf_read_chunk(
    encoder: &mut SliceEncoder<'_>,
    chunk: &LxmfReadChunk,
) -> Result<(), EncodeError> {
    put!(encoder.map(4));
    put!(encoder.u8(0));
    put!(encoder.u64(chunk.handle().get()));
    put!(encoder.u8(1));
    put!(encoder.u32(chunk.offset()));
    put!(encoder.u8(2));
    put!(encoder.u32(chunk.total_len()));
    put!(encoder.u8(3));
    put!(encoder.bytes(chunk.bytes()));
    Ok(())
}

#[cfg(feature = "experimental-lxmf")]
fn encode_lxmf_basic_send_accepted(
    encoder: &mut SliceEncoder<'_>,
    accepted: LxmfBasicSendAccepted,
) -> Result<(), EncodeError> {
    put!(encoder.map(2));
    put!(encoder.u8(0));
    put!(encoder.u64(accepted.id.0));
    put!(encoder.u8(1));
    put!(encoder.bytes(accepted.message_id()));
    Ok(())
}

fn encode_submission_status(
    encoder: &mut SliceEncoder<'_>,
    status: SubmissionStatus,
) -> Result<(), EncodeError> {
    let entries = match status.state {
        SubmissionState::Queued | SubmissionState::Preparing | SubmissionState::Cancelled => 2,
        SubmissionState::AwaitingDelivery(_) | SubmissionState::Delivered(_) => 4,
        SubmissionState::Failed(_) => 3,
    };
    put!(encoder.map(entries));
    put!(encoder.u8(0));
    put!(encoder.u64(status.id.0));
    put!(encoder.u8(1));
    put!(encoder.u8(status.state.wire_code()));
    match status.state {
        SubmissionState::AwaitingDelivery(details) | SubmissionState::Delivered(details) => {
            put!(encoder.u8(2));
            put!(encoder.u16(details.packet_len));
            put!(encoder.u8(3));
            put!(encoder.bytes(details.encoded_packet_sha256.as_bytes()));
        }
        SubmissionState::Failed(failure) => {
            put!(encoder.u8(4));
            put!(encoder.u8(failure.wire_code()));
        }
        SubmissionState::Queued | SubmissionState::Preparing | SubmissionState::Cancelled => {}
    }
    Ok(())
}

#[cfg(feature = "experimental-rns-data")]
fn encode_submission_accepted(
    encoder: &mut SliceEncoder<'_>,
    accepted: SubmissionAccepted,
) -> Result<(), EncodeError> {
    put!(encoder.map(1));
    put!(encoder.u8(0));
    put!(encoder.u64(accepted.id.0));
    Ok(())
}

fn encode_error(
    encoder: &mut SliceEncoder<'_>,
    error: ApiErrorResponse,
) -> Result<(), EncodeError> {
    put!(encoder.map(1 + u64::from(error.operation.is_some())));
    put!(encoder.u8(0));
    put!(encoder.u16(error.code.wire_code()));
    if let Some(operation) = error.operation {
        put!(encoder.u8(1));
        put!(encoder.u16(operation));
    }
    Ok(())
}

fn check_message_size(input: &[u8]) -> Result<(), DecodeError> {
    if input.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::MessageTooLarge {
            actual: input.len(),
            max: MAX_MESSAGE_BYTES,
        });
    }
    Ok(())
}

fn decode_map_len(decoder: &mut Decoder<'_>) -> Result<u64, DecodeError> {
    let entries = decoder
        .map()
        .map_err(|_| DecodeError::Malformed)?
        .ok_or(DecodeError::IndefiniteLength)?;
    if entries > MAX_MAP_ENTRIES {
        return Err(DecodeError::TooManyMapEntries {
            actual: entries,
            max: MAX_MAP_ENTRIES,
        });
    }
    Ok(entries)
}

fn skip_strict(decoder: &mut Decoder<'_>, depth: usize) -> Result<(), DecodeError> {
    match decoder.datatype().map_err(|_| DecodeError::Malformed)? {
        Type::BytesIndef | Type::StringIndef | Type::ArrayIndef | Type::MapIndef => {
            Err(DecodeError::IndefiniteLength)
        }
        Type::Array => {
            let child_depth = enter_nesting(depth)?;
            let entries = decoder
                .array()
                .map_err(|_| DecodeError::Malformed)?
                .ok_or(DecodeError::IndefiniteLength)?;
            for _ in 0..entries {
                skip_strict(decoder, child_depth)?;
            }
            Ok(())
        }
        Type::Map => {
            let child_depth = enter_nesting(depth)?;
            let entries = decoder
                .map()
                .map_err(|_| DecodeError::Malformed)?
                .ok_or(DecodeError::IndefiniteLength)?;
            for _ in 0..entries {
                skip_strict(decoder, child_depth)?;
                skip_strict(decoder, child_depth)?;
            }
            Ok(())
        }
        Type::Tag => {
            let child_depth = enter_nesting(depth)?;
            decoder.tag().map_err(|_| DecodeError::Malformed)?;
            skip_strict(decoder, child_depth)
        }
        Type::Break | Type::Unknown(_) => Err(DecodeError::Malformed),
        Type::Bool
        | Type::Null
        | Type::Undefined
        | Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::Int
        | Type::F16
        | Type::F32
        | Type::F64
        | Type::Simple
        | Type::Bytes
        | Type::String => decoder.skip().map_err(|_| DecodeError::Malformed),
    }
}

fn enter_nesting(depth: usize) -> Result<usize, DecodeError> {
    let actual = depth + 1;
    if actual > MAX_CBOR_NESTING_DEPTH {
        Err(DecodeError::NestingTooDeep {
            actual,
            max: MAX_CBOR_NESTING_DEPTH,
        })
    } else {
        Ok(actual)
    }
}

fn capture_body<'a>(input: &'a [u8], decoder: &mut Decoder<'a>) -> Result<&'a [u8], DecodeError> {
    let start = decoder.position();
    skip_strict(decoder, 0)?;
    let end = decoder.position();
    let size = end - start;
    if size > MAX_BODY_BYTES {
        return Err(DecodeError::BodyTooLarge {
            actual: size,
            max: MAX_BODY_BYTES,
        });
    }
    Ok(&input[start..end])
}

fn finish_item(decoder: &Decoder<'_>, input: &[u8]) -> Result<(), DecodeError> {
    if decoder.position() == input.len() {
        Ok(())
    } else {
        Err(DecodeError::TrailingData)
    }
}

fn finish_body(decoder: &Decoder<'_>, body: &[u8]) -> Result<(), DecodeError> {
    if decoder.position() == body.len() {
        Ok(())
    } else {
        Err(DecodeError::Malformed)
    }
}

fn reject_duplicate(present: bool, field: RequiredField) -> Result<(), DecodeError> {
    if present {
        Err(DecodeError::DuplicateField(field))
    } else {
        Ok(())
    }
}

fn require<T>(value: Option<T>, field: RequiredField) -> Result<T, DecodeError> {
    value.ok_or(DecodeError::MissingField(field))
}

fn check_version(version: ApiVersion) -> Result<(), DecodeError> {
    if version.major == API_VERSION_MAJOR {
        Ok(())
    } else {
        Err(DecodeError::UnsupportedVersion(version))
    }
}

fn check_encode_version(version: ApiVersion) -> Result<(), EncodeError> {
    if version.major == API_VERSION_MAJOR {
        Ok(())
    } else {
        Err(EncodeError::UnsupportedVersion(version))
    }
}

fn decode_version(decoder: &mut Decoder<'_>) -> Result<ApiVersion, DecodeError> {
    let entries = decode_map_len(decoder)?;
    let mut major = None;
    let mut minor = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(major.is_some(), RequiredField::VersionMajor)?;
                major = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(minor.is_some(), RequiredField::VersionMinor)?;
                minor = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(decoder, 0)?,
        }
    }
    Ok(ApiVersion {
        major: require(major, RequiredField::VersionMajor)?,
        minor: require(minor, RequiredField::VersionMinor)?,
    })
}

fn decode_request_body<'a>(
    operation: u16,
    body: &'a [u8],
) -> Result<DeviceRequest<'a>, DecodeError> {
    match operation {
        OP_SYSTEM_CAPABILITIES => decode_capabilities_request(body),
        OP_IDENTITY_SUMMARY => decode_identity_summary_request(body),
        OP_SUBMISSION_STATUS => decode_status_request(body),
        #[cfg(feature = "experimental-rns-inbox")]
        OP_EXPERIMENTAL_RNS_INBOX_STATUS => {
            decode_empty_request(body, DeviceRequest::RnsInboxStatus)
        }
        #[cfg(feature = "experimental-rns-inbox")]
        OP_EXPERIMENTAL_RNS_INBOX_PEEK => decode_empty_request(body, DeviceRequest::RnsInboxPeek),
        #[cfg(feature = "experimental-rns-data")]
        OP_EXPERIMENTAL_SUBMIT_RNS_DATA => decode_submit_request(body),
        #[cfg(feature = "experimental-lxmf")]
        OP_EXPERIMENTAL_LXMF_NEXT => decode_lxmf_next_request(body),
        #[cfg(feature = "experimental-lxmf")]
        OP_EXPERIMENTAL_LXMF_READ => decode_lxmf_read_request(body),
        #[cfg(feature = "experimental-lxmf")]
        OP_EXPERIMENTAL_LXMF_BASIC_SEND => decode_lxmf_basic_send_request(body),
        other => Err(DecodeError::UnsupportedOperation(other)),
    }
}

#[cfg(feature = "experimental-rns-inbox")]
fn decode_empty_request(
    body: &[u8],
    request: DeviceRequest<'static>,
) -> Result<DeviceRequest<'static>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    for _ in 0..entries {
        decoder.u64().map_err(|_| DecodeError::Malformed)?;
        skip_strict(&mut decoder, 0)?;
    }
    finish_body(&decoder, body)?;
    Ok(request)
}

#[cfg(feature = "experimental-lxmf")]
fn decode_lxmf_next_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut after = None;
    let mut after_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(after_seen, RequiredField::LxmfAfterHandle)?;
                after_seen = true;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                after =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfAfterHandle,
                            value,
                        })?,
                    );
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::LxmfNext { after })
}

#[cfg(feature = "experimental-lxmf")]
fn decode_lxmf_read_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut handle = None;
    let mut offset = None;
    let mut max_bytes = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(handle.is_some(), RequiredField::LxmfHandle)?;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                handle =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfHandle,
                            value,
                        })?,
                    );
            }
            1 => {
                reject_duplicate(offset.is_some(), RequiredField::LxmfReadOffset)?;
                offset = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(max_bytes.is_some(), RequiredField::LxmfReadMaxBytes)?;
                let value = decoder.u16().map_err(|_| DecodeError::Malformed)?;
                max_bytes =
                    Some(
                        LxmfReadLength::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfReadMaxBytes,
                            value: u64::from(value),
                        })?,
                    );
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::LxmfRead {
        handle: require(handle, RequiredField::LxmfHandle)?,
        offset: require(offset, RequiredField::LxmfReadOffset)?,
        max_bytes: require(max_bytes, RequiredField::LxmfReadMaxBytes)?,
    })
}

#[cfg(feature = "experimental-lxmf")]
fn decode_lxmf_basic_send_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut destination = None;
    let mut timestamp_unix_ms = None;
    let mut title = None;
    let mut content = None;
    let mut idempotency_key = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    destination.is_some(),
                    RequiredField::LxmfBasicSendDestination,
                )?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::LxmfBasicSendDestination,
                )?));
            }
            1 => {
                reject_duplicate(
                    timestamp_unix_ms.is_some(),
                    RequiredField::LxmfBasicSendTimestampUnixMs,
                )?;
                timestamp_unix_ms = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(title.is_some(), RequiredField::LxmfBasicSendTitle)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_LXMF_BASIC_TITLE_BYTES {
                    return Err(DecodeError::LxmfBasicTitleTooLarge {
                        actual: bytes.len(),
                        max: MAX_LXMF_BASIC_TITLE_BYTES,
                    });
                }
                title = Some(bytes);
            }
            3 => {
                reject_duplicate(content.is_some(), RequiredField::LxmfBasicSendContent)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_LXMF_BASIC_CONTENT_BYTES {
                    return Err(DecodeError::LxmfBasicContentTooLarge {
                        actual: bytes.len(),
                        max: MAX_LXMF_BASIC_CONTENT_BYTES,
                    });
                }
                content = Some(bytes);
            }
            4 => {
                reject_duplicate(
                    idempotency_key.is_some(),
                    RequiredField::LxmfBasicSendIdempotencyKey,
                )?;
                idempotency_key = Some(IdempotencyKey(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::LxmfBasicSendIdempotencyKey,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::LxmfBasicSend {
        destination: require(destination, RequiredField::LxmfBasicSendDestination)?,
        timestamp_unix_ms: require(
            timestamp_unix_ms,
            RequiredField::LxmfBasicSendTimestampUnixMs,
        )?,
        title: require(title, RequiredField::LxmfBasicSendTitle)?,
        content: require(content, RequiredField::LxmfBasicSendContent)?,
        idempotency_key: require(idempotency_key, RequiredField::LxmfBasicSendIdempotencyKey)?,
    })
}

fn decode_capabilities_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    for _ in 0..entries {
        decoder.u64().map_err(|_| DecodeError::Malformed)?;
        skip_strict(&mut decoder, 0)?;
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::SystemCapabilities)
}

fn decode_identity_summary_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    for _ in 0..entries {
        decoder.u64().map_err(|_| DecodeError::Malformed)?;
        skip_strict(&mut decoder, 0)?;
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::IdentitySummary)
}

fn decode_status_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::SubmissionId)?;
                id = Some(SubmissionId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::SubmissionStatus {
        id: require(id, RequiredField::SubmissionId)?,
    })
}

#[cfg(feature = "experimental-rns-data")]
fn decode_submit_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut destination = None;
    let mut payload = None;
    let mut idempotency_key = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(destination.is_some(), RequiredField::SubmitDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::SubmitDestination,
                )?));
            }
            1 => {
                reject_duplicate(payload.is_some(), RequiredField::SubmitPayload)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES {
                    return Err(DecodeError::PayloadTooLarge {
                        actual: bytes.len(),
                        max: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES,
                    });
                }
                payload = Some(bytes);
            }
            2 => {
                reject_duplicate(
                    idempotency_key.is_some(),
                    RequiredField::SubmitIdempotencyKey,
                )?;
                idempotency_key = Some(IdempotencyKey(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::SubmitIdempotencyKey,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::SubmitRnsData {
        destination: require(destination, RequiredField::SubmitDestination)?,
        payload: require(payload, RequiredField::SubmitPayload)?,
        idempotency_key: require(idempotency_key, RequiredField::SubmitIdempotencyKey)?,
    })
}

fn decode_capabilities(body: &[u8]) -> Result<CapabilitySnapshot, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut api_version = None;
    let mut packet_output = None;
    let mut direct_radio_tx = None;
    let mut experimental_submit = None;
    let mut max_message = None;
    let mut max_body = None;
    let mut max_payload = None;
    let mut experimental_rns_inbox = None;
    let mut max_rns_inbox_payload = None;
    let mut experimental_lxmf = None;
    let mut max_lxmf_read_chunk = None;
    let mut experimental_lxmf_basic_send = None;
    let mut max_lxmf_basic_title = None;
    let mut max_lxmf_basic_content = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(api_version.is_some(), RequiredField::CapabilityApiVersion)?;
                api_version = Some(decode_version(&mut decoder)?);
            }
            1 => {
                reject_duplicate(
                    packet_output.is_some(),
                    RequiredField::CapabilityPacketOutput,
                )?;
                packet_output = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    direct_radio_tx.is_some(),
                    RequiredField::CapabilityDirectRadioTx,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                direct_radio_tx = Some(decode_direct_radio_availability(value)?);
            }
            3 => {
                reject_duplicate(
                    experimental_submit.is_some(),
                    RequiredField::CapabilityExperimentalSubmit,
                )?;
                experimental_submit = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(
                    max_message.is_some(),
                    RequiredField::CapabilityMaxMessageBytes,
                )?;
                max_message = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(max_body.is_some(), RequiredField::CapabilityMaxBodyBytes)?;
                max_body = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(
                    max_payload.is_some(),
                    RequiredField::CapabilityMaxSubmitPayloadBytes,
                )?;
                max_payload = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            7 => {
                reject_duplicate(
                    experimental_rns_inbox.is_some(),
                    RequiredField::CapabilityExperimentalRnsInbox,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                experimental_rns_inbox = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityExperimentalRnsInbox,
                )?);
            }
            8 => {
                reject_duplicate(
                    max_rns_inbox_payload.is_some(),
                    RequiredField::CapabilityMaxRnsInboxPayloadBytes,
                )?;
                max_rns_inbox_payload = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            9 => {
                reject_duplicate(
                    experimental_lxmf.is_some(),
                    RequiredField::CapabilityExperimentalLxmf,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                experimental_lxmf = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityExperimentalLxmf,
                )?);
            }
            10 => {
                reject_duplicate(
                    max_lxmf_read_chunk.is_some(),
                    RequiredField::CapabilityMaxLxmfReadChunkBytes,
                )?;
                max_lxmf_read_chunk = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            11 => {
                reject_duplicate(
                    experimental_lxmf_basic_send.is_some(),
                    RequiredField::CapabilityExperimentalLxmfBasicSend,
                )?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                experimental_lxmf_basic_send = Some(decode_capability_availability(
                    value,
                    RequiredField::CapabilityExperimentalLxmfBasicSend,
                )?);
            }
            12 => {
                reject_duplicate(
                    max_lxmf_basic_title.is_some(),
                    RequiredField::CapabilityMaxLxmfBasicTitleBytes,
                )?;
                max_lxmf_basic_title = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            13 => {
                reject_duplicate(
                    max_lxmf_basic_content.is_some(),
                    RequiredField::CapabilityMaxLxmfBasicContentBytes,
                )?;
                max_lxmf_basic_content = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let api_version = require(api_version, RequiredField::CapabilityApiVersion)?;
    check_version(api_version)?;
    Ok(CapabilitySnapshot {
        api_version,
        packet_output: require(packet_output, RequiredField::CapabilityPacketOutput)?,
        direct_radio_tx: require(direct_radio_tx, RequiredField::CapabilityDirectRadioTx)?,
        experimental_submit_rns_data: require(
            experimental_submit,
            RequiredField::CapabilityExperimentalSubmit,
        )?,
        max_message_bytes: require(max_message, RequiredField::CapabilityMaxMessageBytes)?,
        max_body_bytes: require(max_body, RequiredField::CapabilityMaxBodyBytes)?,
        max_submit_rns_data_payload_bytes: require(
            max_payload,
            RequiredField::CapabilityMaxSubmitPayloadBytes,
        )?,
        experimental_rns_inbox: experimental_rns_inbox
            .unwrap_or(CapabilityAvailability::Unavailable),
        max_rns_inbox_payload_bytes: max_rns_inbox_payload.unwrap_or(0),
        experimental_lxmf: experimental_lxmf.unwrap_or(CapabilityAvailability::Unavailable),
        max_lxmf_read_chunk_bytes: max_lxmf_read_chunk.unwrap_or(0),
        experimental_lxmf_basic_send: experimental_lxmf_basic_send
            .unwrap_or(CapabilityAvailability::Unavailable),
        max_lxmf_basic_title_bytes: max_lxmf_basic_title.unwrap_or(0),
        max_lxmf_basic_content_bytes: max_lxmf_basic_content.unwrap_or(0),
    })
}

#[cfg(feature = "experimental-rns-inbox")]
fn decode_rns_inbox_status(body: &[u8]) -> Result<RnsInboxStatus, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut depth = None;
    let mut capacity = None;
    let mut dropped_since_boot = None;
    let mut max_payload_bytes = None;
    let mut durable = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(depth.is_some(), RequiredField::RnsInboxDepth)?;
                depth = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(capacity.is_some(), RequiredField::RnsInboxCapacity)?;
                capacity = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(
                    dropped_since_boot.is_some(),
                    RequiredField::RnsInboxDroppedSinceBoot,
                )?;
                dropped_since_boot = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    max_payload_bytes.is_some(),
                    RequiredField::RnsInboxMaxPayloadBytes,
                )?;
                max_payload_bytes = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            4 => {
                reject_duplicate(durable.is_some(), RequiredField::RnsInboxDurable)?;
                durable = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(RnsInboxStatus {
        depth: require(depth, RequiredField::RnsInboxDepth)?,
        capacity: require(capacity, RequiredField::RnsInboxCapacity)?,
        dropped_since_boot: require(dropped_since_boot, RequiredField::RnsInboxDroppedSinceBoot)?,
        max_payload_bytes: require(max_payload_bytes, RequiredField::RnsInboxMaxPayloadBytes)?,
        durable: require(durable, RequiredField::RnsInboxDurable)?,
    })
}

#[cfg(feature = "experimental-rns-inbox")]
fn decode_rns_inbox_item(body: &[u8]) -> Result<RnsInboxItem, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    let mut destination = None;
    let mut payload = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::RnsInboxItemId)?;
                id = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            1 => {
                reject_duplicate(destination.is_some(), RequiredField::RnsInboxDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::RnsInboxDestination,
                )?));
            }
            2 => {
                reject_duplicate(payload.is_some(), RequiredField::RnsInboxPayload)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_RNS_INBOX_PAYLOAD_BYTES {
                    return Err(DecodeError::InboxPayloadTooLarge {
                        actual: bytes.len(),
                        max: MAX_RNS_INBOX_PAYLOAD_BYTES,
                    });
                }
                payload = Some(bytes);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let id = core::num::NonZeroU64::new(require(id, RequiredField::RnsInboxItemId)?).ok_or(
        DecodeError::InvalidValue {
            field: RequiredField::RnsInboxItemId,
            value: 0,
        },
    )?;
    RnsInboxItem::new(
        id,
        require(destination, RequiredField::RnsInboxDestination)?,
        require(payload, RequiredField::RnsInboxPayload)?,
    )
    .map_err(|too_large| DecodeError::InboxPayloadTooLarge {
        actual: too_large.actual(),
        max: too_large.maximum(),
    })
}

fn decode_identity_summary(body: &[u8]) -> Result<IdentitySummary, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut primary_destination = None;
    let mut lxmf_delivery_destination = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(
                    primary_destination.is_some(),
                    RequiredField::IdentityPrimaryDestination,
                )?;
                primary_destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::IdentityPrimaryDestination,
                )?));
            }
            1 => {
                reject_duplicate(
                    lxmf_delivery_destination.is_some(),
                    RequiredField::IdentityLxmfDeliveryDestination,
                )?;
                lxmf_delivery_destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::IdentityLxmfDeliveryDestination,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let primary_destination = require(
        primary_destination,
        RequiredField::IdentityPrimaryDestination,
    )?;
    Ok(match lxmf_delivery_destination {
        Some(lxmf_delivery_destination) => IdentitySummary::with_lxmf_delivery_destination(
            primary_destination,
            lxmf_delivery_destination,
        ),
        None => IdentitySummary::new(primary_destination),
    })
}

#[cfg(feature = "experimental-lxmf")]
fn decode_lxmf_summary(body: &[u8]) -> Result<LxmfMessageSummary, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut handle = None;
    let mut message_id = None;
    let mut destination = None;
    let mut source = None;
    let mut timestamp_bits = None;
    let mut normalized_wire_len = None;
    let mut title_len = None;
    let mut content_len = None;
    let mut fields_encoded_len = None;
    let mut exact_wire_sha256 = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(handle.is_some(), RequiredField::LxmfHandle)?;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                handle =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfHandle,
                            value,
                        })?,
                    );
            }
            1 => {
                reject_duplicate(message_id.is_some(), RequiredField::LxmfMessageId)?;
                message_id = Some(decode_fixed_bytes::<32>(
                    &mut decoder,
                    RequiredField::LxmfMessageId,
                )?);
            }
            2 => {
                reject_duplicate(destination.is_some(), RequiredField::LxmfDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::LxmfDestination,
                )?));
            }
            3 => {
                reject_duplicate(source.is_some(), RequiredField::LxmfSource)?;
                source = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::LxmfSource,
                )?));
            }
            4 => {
                reject_duplicate(timestamp_bits.is_some(), RequiredField::LxmfTimestampBits)?;
                timestamp_bits = Some(decoder.u64().map_err(|_| DecodeError::Malformed)?);
            }
            5 => {
                reject_duplicate(
                    normalized_wire_len.is_some(),
                    RequiredField::LxmfNormalizedWireLength,
                )?;
                normalized_wire_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            6 => {
                reject_duplicate(title_len.is_some(), RequiredField::LxmfTitleLength)?;
                title_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            7 => {
                reject_duplicate(content_len.is_some(), RequiredField::LxmfContentLength)?;
                content_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            8 => {
                reject_duplicate(
                    fields_encoded_len.is_some(),
                    RequiredField::LxmfFieldsEncodedLength,
                )?;
                fields_encoded_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            9 => {
                reject_duplicate(
                    exact_wire_sha256.is_some(),
                    RequiredField::LxmfExactWireSha256,
                )?;
                exact_wire_sha256 = Some(decode_fixed_bytes::<32>(
                    &mut decoder,
                    RequiredField::LxmfExactWireSha256,
                )?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    LxmfMessageSummary::new(
        require(handle, RequiredField::LxmfHandle)?,
        require(message_id, RequiredField::LxmfMessageId)?,
        require(destination, RequiredField::LxmfDestination)?,
        require(source, RequiredField::LxmfSource)?,
        require(timestamp_bits, RequiredField::LxmfTimestampBits)?,
        require(normalized_wire_len, RequiredField::LxmfNormalizedWireLength)?,
        require(title_len, RequiredField::LxmfTitleLength)?,
        require(content_len, RequiredField::LxmfContentLength)?,
        require(fields_encoded_len, RequiredField::LxmfFieldsEncodedLength)?,
        require(exact_wire_sha256, RequiredField::LxmfExactWireSha256)?,
    )
    .map_err(|_| DecodeError::InvalidLxmfMessageSummary)
}

#[cfg(feature = "experimental-lxmf")]
fn decode_lxmf_read_chunk(body: &[u8]) -> Result<LxmfReadChunk, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut handle = None;
    let mut offset = None;
    let mut total_len = None;
    let mut bytes = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(handle.is_some(), RequiredField::LxmfHandle)?;
                let value = decoder.u64().map_err(|_| DecodeError::Malformed)?;
                handle =
                    Some(
                        LxmfMessageHandle::new(value).map_err(|_| DecodeError::InvalidValue {
                            field: RequiredField::LxmfHandle,
                            value,
                        })?,
                    );
            }
            1 => {
                reject_duplicate(offset.is_some(), RequiredField::LxmfReadOffset)?;
                offset = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(total_len.is_some(), RequiredField::LxmfNormalizedWireLength)?;
                total_len = Some(decoder.u32().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(bytes.is_some(), RequiredField::LxmfReadBytes)?;
                let decoded = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if decoded.len() > MAX_LXMF_READ_CHUNK_BYTES {
                    return Err(DecodeError::LxmfReadChunkTooLarge {
                        actual: decoded.len(),
                        max: MAX_LXMF_READ_CHUNK_BYTES,
                    });
                }
                bytes = Some(decoded);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    LxmfReadChunk::new(
        require(handle, RequiredField::LxmfHandle)?,
        require(offset, RequiredField::LxmfReadOffset)?,
        require(total_len, RequiredField::LxmfNormalizedWireLength)?,
        require(bytes, RequiredField::LxmfReadBytes)?,
    )
    .map_err(|error| match error {
        crate::InvalidLxmfReadChunk::TooLarge { actual } => DecodeError::LxmfReadChunkTooLarge {
            actual,
            max: MAX_LXMF_READ_CHUNK_BYTES,
        },
        crate::InvalidLxmfReadChunk::Empty | crate::InvalidLxmfReadChunk::OutsideMessage { .. } => {
            DecodeError::InvalidLxmfReadChunk
        }
    })
}

#[cfg(feature = "experimental-lxmf")]
fn decode_lxmf_basic_send_accepted(body: &[u8]) -> Result<LxmfBasicSendAccepted, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    let mut message_id = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::SubmissionId)?;
                id = Some(SubmissionId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            1 => {
                reject_duplicate(message_id.is_some(), RequiredField::LxmfMessageId)?;
                message_id = Some(decode_fixed_bytes::<32>(
                    &mut decoder,
                    RequiredField::LxmfMessageId,
                )?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(LxmfBasicSendAccepted::new(
        require(id, RequiredField::SubmissionId)?,
        require(message_id, RequiredField::LxmfMessageId)?,
    ))
}

fn decode_submission_status(body: &[u8]) -> Result<SubmissionStatus, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    let mut state_code = None;
    let mut packet_len = None;
    let mut packet_hash = None;
    let mut failure = None;
    let mut packet_len_seen = false;
    let mut packet_hash_seen = false;
    let mut failure_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::SubmissionId)?;
                id = Some(SubmissionId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            1 => {
                reject_duplicate(state_code.is_some(), RequiredField::SubmissionState)?;
                state_code = Some(decoder.u8().map_err(|_| DecodeError::Malformed)?);
            }
            2 => {
                reject_duplicate(packet_len_seen, RequiredField::SubmissionPacketLength)?;
                packet_len_seen = true;
                packet_len = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            3 => {
                reject_duplicate(
                    packet_hash_seen,
                    RequiredField::SubmissionEncodedPacketSha256,
                )?;
                packet_hash_seen = true;
                packet_hash = Some(decode_fixed_bytes::<32>(
                    &mut decoder,
                    RequiredField::SubmissionEncodedPacketSha256,
                )?);
            }
            4 => {
                reject_duplicate(failure_seen, RequiredField::SubmissionFailure)?;
                failure_seen = true;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                failure = Some(decode_submission_failure(value)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    let state_code = require(state_code, RequiredField::SubmissionState)?;
    let state = decode_submission_state(state_code, packet_len, packet_hash, failure)?;
    Ok(SubmissionStatus {
        id: require(id, RequiredField::SubmissionId)?,
        state,
    })
}

#[cfg(feature = "experimental-rns-data")]
fn decode_submission_accepted(body: &[u8]) -> Result<SubmissionAccepted, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut id = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(id.is_some(), RequiredField::SubmissionId)?;
                id = Some(SubmissionId(
                    decoder.u64().map_err(|_| DecodeError::Malformed)?,
                ));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(SubmissionAccepted {
        id: require(id, RequiredField::SubmissionId)?,
    })
}

fn decode_error(body: &[u8]) -> Result<ApiErrorResponse, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut code = None;
    let mut operation = None;
    let mut operation_seen = false;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(code.is_some(), RequiredField::ErrorCode)?;
                let value = decoder.u16().map_err(|_| DecodeError::Malformed)?;
                code = Some(decode_api_error(value)?);
            }
            1 => {
                reject_duplicate(operation_seen, RequiredField::ErrorOperation)?;
                operation_seen = true;
                operation = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(ApiErrorResponse {
        code: require(code, RequiredField::ErrorCode)?,
        operation,
    })
}

fn decode_fixed_bytes<const N: usize>(
    decoder: &mut Decoder<'_>,
    field: RequiredField,
) -> Result<[u8; N], DecodeError> {
    let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
    bytes
        .try_into()
        .map_err(|_| DecodeError::InvalidByteStringLength {
            field,
            expected: N,
            actual: bytes.len(),
        })
}

fn decode_direct_radio_availability(value: u8) -> Result<CapabilityAvailability, DecodeError> {
    decode_capability_availability(value, RequiredField::CapabilityDirectRadioTx)
}

fn decode_capability_availability(
    value: u8,
    field: RequiredField,
) -> Result<CapabilityAvailability, DecodeError> {
    match value {
        0 => Ok(CapabilityAvailability::Unavailable),
        1 => Ok(CapabilityAvailability::Disabled),
        2 => Ok(CapabilityAvailability::Available),
        other => Err(DecodeError::InvalidValue {
            field,
            value: u64::from(other),
        }),
    }
}

fn decode_submission_state(
    code: u8,
    packet_len: Option<u16>,
    packet_hash: Option<[u8; 32]>,
    failure: Option<SubmissionFailure>,
) -> Result<SubmissionState, DecodeError> {
    match (code, packet_len, packet_hash, failure) {
        (0, None, None, None) => Ok(SubmissionState::Queued),
        (1, None, None, None) => Ok(SubmissionState::Preparing),
        (2, Some(packet_len), Some(packet_hash), None) => {
            Ok(SubmissionState::AwaitingDelivery(PreparedPacketDetails {
                packet_len,
                encoded_packet_sha256: crate::EncodedPacketSha256::new(packet_hash),
            }))
        }
        (3, Some(packet_len), Some(packet_hash), None) => {
            Ok(SubmissionState::Delivered(PreparedPacketDetails {
                packet_len,
                encoded_packet_sha256: crate::EncodedPacketSha256::new(packet_hash),
            }))
        }
        (4, None, None, Some(failure)) => Ok(SubmissionState::Failed(failure)),
        (5, None, None, None) => Ok(SubmissionState::Cancelled),
        (0..=5, _, _, _) => Err(DecodeError::InvalidSubmissionStatus),
        (other, _, _, _) => Err(DecodeError::InvalidValue {
            field: RequiredField::SubmissionState,
            value: u64::from(other),
        }),
    }
}

fn decode_submission_failure(value: u8) -> Result<SubmissionFailure, DecodeError> {
    match value {
        0 => Ok(SubmissionFailure::NoPath),
        1 => Ok(SubmissionFailure::DeliveryTimeout),
        2 => Ok(SubmissionFailure::Rejected),
        3 => Ok(SubmissionFailure::Internal),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::SubmissionFailure,
            value: u64::from(other),
        }),
    }
}

fn decode_api_error(value: u16) -> Result<ApiErrorCode, DecodeError> {
    match value {
        1 => Ok(ApiErrorCode::UnsupportedOperation),
        2 => Ok(ApiErrorCode::UnsupportedVersion),
        3 => Ok(ApiErrorCode::AuthenticationRequired),
        4 => Ok(ApiErrorCode::PermissionDenied),
        5 => Ok(ApiErrorCode::NotFound),
        6 => Ok(ApiErrorCode::InvalidRequest),
        7 => Ok(ApiErrorCode::CapabilityUnavailable),
        8 => Ok(ApiErrorCode::Internal),
        9 => Ok(ApiErrorCode::CapacityExhausted),
        10 => Ok(ApiErrorCode::IdempotencyConflict),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::ErrorCode,
            value: u64::from(other),
        }),
    }
}
