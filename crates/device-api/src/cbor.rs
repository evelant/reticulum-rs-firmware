//! Allocation-free, strict CBOR codec for the logical device API.

use minicbor::{Decoder, Encoder, data::Type, encode::write::Cursor};

use crate::model::{
    API_VERSION_MAJOR, ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilityAvailability,
    CapabilitySnapshot, DeviceRequest, DeviceResponse, MAX_BODY_BYTES, MAX_MESSAGE_BYTES,
    OP_SUBMISSION_STATUS, OP_SYSTEM_CAPABILITIES, PreparedPacketDetails, RESPONSE_ERROR,
    RequestEnvelope, RequestId, ResponseEnvelope, SubmissionFailure, SubmissionId, SubmissionState,
    SubmissionStatus,
};
#[cfg(feature = "host-sim")]
use crate::model::{
    DestinationHash, IdempotencyKey, MAX_PREPARE_RNS_DATA_PAYLOAD_BYTES,
    OP_EXPERIMENTAL_PREPARE_RNS_DATA, SubmissionAccepted,
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
    /// Experimental destination hash at body key 0.
    PrepareDestination,
    /// Experimental payload at body key 1.
    PreparePayload,
    /// Experimental idempotency key at body key 2.
    PrepareIdempotencyKey,
    /// Capability API version at body key 0.
    CapabilityApiVersion,
    /// Capability raw packet-output flag at body key 1.
    CapabilityPacketOutput,
    /// Capability radio-TX availability at body key 2.
    CapabilityRadioTx,
    /// Capability experimental preparation flag at body key 3.
    CapabilityExperimentalPrepare,
    /// Capability logical message limit at body key 4.
    CapabilityMaxMessageBytes,
    /// Capability body limit at body key 5.
    CapabilityMaxBodyBytes,
    /// Capability experimental payload limit at body key 6.
    CapabilityMaxPreparePayloadBytes,
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
    #[cfg(feature = "host-sim")]
    if let DeviceRequest::PrepareRnsData { payload, .. } = envelope.request
        && payload.len() > MAX_PREPARE_RNS_DATA_PAYLOAD_BYTES
    {
        return Err(EncodeError::PayloadTooLarge {
            actual: payload.len(),
            max: MAX_PREPARE_RNS_DATA_PAYLOAD_BYTES,
        });
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
        DeviceRequest::SubmissionStatus { id } => {
            put!(encoder.map(1));
            put!(encoder.u8(0));
            put!(encoder.u64(id.0));
        }
        #[cfg(feature = "host-sim")]
        DeviceRequest::PrepareRnsData {
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
        DeviceResponse::SubmissionStatus(status) => {
            encode_submission_status(&mut encoder, status)?;
        }
        #[cfg(feature = "host-sim")]
        DeviceResponse::PrepareRnsDataAccepted(accepted) => {
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
        OP_SUBMISSION_STATUS => DeviceResponse::SubmissionStatus(decode_submission_status(body)?),
        #[cfg(feature = "host-sim")]
        OP_EXPERIMENTAL_PREPARE_RNS_DATA => {
            DeviceResponse::PrepareRnsDataAccepted(decode_submission_accepted(body)?)
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
    put!(encoder.map(7));
    put!(encoder.u8(0));
    encode_version(encoder, capabilities.api_version)?;
    put!(encoder.u8(1));
    put!(encoder.bool(capabilities.packet_output));
    put!(encoder.u8(2));
    put!(encoder.u8(capabilities.radio_tx.wire_code()));
    put!(encoder.u8(3));
    put!(encoder.bool(capabilities.experimental_prepare_rns_data));
    put!(encoder.u8(4));
    put!(encoder.u16(capabilities.max_message_bytes));
    put!(encoder.u8(5));
    put!(encoder.u16(capabilities.max_body_bytes));
    put!(encoder.u8(6));
    put!(encoder.u16(capabilities.max_prepare_rns_data_payload_bytes));
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

#[cfg(feature = "host-sim")]
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
        OP_SUBMISSION_STATUS => decode_status_request(body),
        #[cfg(feature = "host-sim")]
        OP_EXPERIMENTAL_PREPARE_RNS_DATA => decode_prepare_request(body),
        other => Err(DecodeError::UnsupportedOperation(other)),
    }
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

#[cfg(feature = "host-sim")]
fn decode_prepare_request(body: &[u8]) -> Result<DeviceRequest<'_>, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut destination = None;
    let mut payload = None;
    let mut idempotency_key = None;
    for _ in 0..entries {
        let key = decoder.u64().map_err(|_| DecodeError::Malformed)?;
        match key {
            0 => {
                reject_duplicate(destination.is_some(), RequiredField::PrepareDestination)?;
                destination = Some(DestinationHash(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::PrepareDestination,
                )?));
            }
            1 => {
                reject_duplicate(payload.is_some(), RequiredField::PreparePayload)?;
                let bytes = decoder.bytes().map_err(|_| DecodeError::Malformed)?;
                if bytes.len() > MAX_PREPARE_RNS_DATA_PAYLOAD_BYTES {
                    return Err(DecodeError::PayloadTooLarge {
                        actual: bytes.len(),
                        max: MAX_PREPARE_RNS_DATA_PAYLOAD_BYTES,
                    });
                }
                payload = Some(bytes);
            }
            2 => {
                reject_duplicate(
                    idempotency_key.is_some(),
                    RequiredField::PrepareIdempotencyKey,
                )?;
                idempotency_key = Some(IdempotencyKey(decode_fixed_bytes::<16>(
                    &mut decoder,
                    RequiredField::PrepareIdempotencyKey,
                )?));
            }
            _ => skip_strict(&mut decoder, 0)?,
        }
    }
    finish_body(&decoder, body)?;
    Ok(DeviceRequest::PrepareRnsData {
        destination: require(destination, RequiredField::PrepareDestination)?,
        payload: require(payload, RequiredField::PreparePayload)?,
        idempotency_key: require(idempotency_key, RequiredField::PrepareIdempotencyKey)?,
    })
}

fn decode_capabilities(body: &[u8]) -> Result<CapabilitySnapshot, DecodeError> {
    let mut decoder = Decoder::new(body);
    let entries = decode_map_len(&mut decoder)?;
    let mut api_version = None;
    let mut packet_output = None;
    let mut radio_tx = None;
    let mut experimental_prepare = None;
    let mut max_message = None;
    let mut max_body = None;
    let mut max_payload = None;
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
                reject_duplicate(radio_tx.is_some(), RequiredField::CapabilityRadioTx)?;
                let value = decoder.u8().map_err(|_| DecodeError::Malformed)?;
                radio_tx = Some(decode_capability_availability(value)?);
            }
            3 => {
                reject_duplicate(
                    experimental_prepare.is_some(),
                    RequiredField::CapabilityExperimentalPrepare,
                )?;
                experimental_prepare = Some(decoder.bool().map_err(|_| DecodeError::Malformed)?);
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
                    RequiredField::CapabilityMaxPreparePayloadBytes,
                )?;
                max_payload = Some(decoder.u16().map_err(|_| DecodeError::Malformed)?);
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
        radio_tx: require(radio_tx, RequiredField::CapabilityRadioTx)?,
        experimental_prepare_rns_data: require(
            experimental_prepare,
            RequiredField::CapabilityExperimentalPrepare,
        )?,
        max_message_bytes: require(max_message, RequiredField::CapabilityMaxMessageBytes)?,
        max_body_bytes: require(max_body, RequiredField::CapabilityMaxBodyBytes)?,
        max_prepare_rns_data_payload_bytes: require(
            max_payload,
            RequiredField::CapabilityMaxPreparePayloadBytes,
        )?,
    })
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

#[cfg(feature = "host-sim")]
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

fn decode_capability_availability(value: u8) -> Result<CapabilityAvailability, DecodeError> {
    match value {
        0 => Ok(CapabilityAvailability::Unavailable),
        1 => Ok(CapabilityAvailability::Disabled),
        2 => Ok(CapabilityAvailability::Available),
        other => Err(DecodeError::InvalidValue {
            field: RequiredField::CapabilityRadioTx,
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
