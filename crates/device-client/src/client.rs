use std::{
    fmt,
    io::{self, Read, Write},
    time::{Duration, Instant},
};

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiErrorResponse, ApiVersion, DecodeError, DestinationHash, DeviceRequest, DeviceResponse,
    EncodeError, IdempotencyKey, IdentitySummary, LxmfBasicSendAccepted, LxmfMessageHandle,
    LxmfMessageSummary, LxmfPeerDiscoveryCursor, LxmfPeerDiscoveryPage, LxmfReadLength,
    MAX_LXMF_READ_CHUNK_BYTES, MAX_MESSAGE_BYTES, RequestEnvelope, RequestId, SubmissionId,
    SubmissionStatus, decode_response, encode_request,
};
use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder, TxAdvanceError};
use reticulum_device_api_handoff::{MessageLength, OwnedMessage};
use reticulum_device_api_session::{
    BearerBinding, ClientHandshakeError, ClientHelloFlight, ClientParameters, ClientProofFlight,
    ClientRequestFaultKind, ClientRequestFlight, ClientSession, ClientSessionFault, DeviceId,
    SessionId, SessionSuite,
};
use reticulum_lxmf_wire::{MessageView, WireError, WireLimits};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::ActivatedCredential;

const READ_BUFFER_CAPACITY: usize = 1_024;
const DEFAULT_MAX_LXMF_MESSAGES: usize = 512;
const DEFAULT_MAX_LXMF_WIRE_BYTES: usize = 16 * 1024 * 1024;
const LXMF_PARSE_MAX_NESTING: usize = 16;

/// Byte-stream transport accepted by [`DeviceClient`].
///
/// This blanket trait deliberately adds no serial-specific methods. Callers
/// configure baud rate, DTR/RTS, finite OS I/O timeouts, and reconnect policy
/// before constructing the client.
pub trait ClientTransport: Read + Write {}

impl<T: Read + Write> ClientTransport for T {}

/// Host-side safety and deadline policy for one client connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientConfig {
    handshake_timeout: Duration,
    request_timeout: Duration,
    max_lxmf_messages: usize,
    max_lxmf_wire_bytes: usize,
}

impl ClientConfig {
    /// Construct an explicit client policy.
    pub const fn new(
        handshake_timeout: Duration,
        request_timeout: Duration,
        max_lxmf_messages: usize,
        max_lxmf_wire_bytes: usize,
    ) -> Self {
        Self {
            handshake_timeout,
            request_timeout,
            max_lxmf_messages,
            max_lxmf_wire_bytes,
        }
    }

    /// Complete handshake deadline.
    pub const fn handshake_timeout(self) -> Duration {
        self.handshake_timeout
    }

    /// Deadline independently applied to each logical request/response exchange.
    pub const fn request_timeout(self) -> Duration {
        self.request_timeout
    }

    /// Maximum summaries retained by [`DeviceClient::lxmf_list`].
    pub const fn max_lxmf_messages(self) -> usize {
        self.max_lxmf_messages
    }

    /// Maximum complete normalized wire accepted by [`DeviceClient::lxmf_read`].
    pub const fn max_lxmf_wire_bytes(self) -> usize {
        self.max_lxmf_wire_bytes
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(5),
            Duration::from_secs(5),
            DEFAULT_MAX_LXMF_MESSAGES,
            DEFAULT_MAX_LXMF_WIRE_BYTES,
        )
    }
}

/// Transcript and bearer profile for one authenticated client connection.
///
/// All current profiles provide authentication and integrity but no
/// application-layer confidentiality. The Wi-Fi and BLE profiles are explicit
/// proof profiles for local wireless links; neither is the final wireless
/// security construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientSessionProfile {
    /// Original suite-1 USB Serial/JTAG qualification profile.
    UsbSerialJtagQualification,
    /// Suite-2 Wi-Fi-bound authentication and integrity qualification profile.
    WifiQualification,
    /// Suite-3 BLE GATT-bound authentication and integrity qualification profile.
    BleGattQualification,
}

impl ClientSessionProfile {
    const fn parameters(self, device_id: DeviceId) -> ClientParameters {
        match self {
            Self::UsbSerialJtagQualification => {
                ClientParameters::new(device_id, BearerBinding::UsbSerialJtag)
            }
            Self::WifiQualification => ClientParameters::new_for_suite(
                device_id,
                BearerBinding::Wifi,
                SessionSuite::WifiQualification,
            ),
            Self::BleGattQualification => ClientParameters::new_for_suite(
                device_id,
                BearerBinding::BleGatt,
                SessionSuite::BleGattQualification,
            ),
        }
    }
}

/// Logical operation associated with a typed response or error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// `identity.summary`.
    IdentitySummary,
    /// `submission.status`.
    SubmissionStatus,
    /// `experimental.lxmf.next`.
    LxmfNext,
    /// `experimental.lxmf.read`.
    LxmfRead,
    /// `experimental.lxmf.basic_send`.
    LxmfBasicSend,
    /// `experimental.lxmf.peer_next`.
    LxmfPeerNext,
}

impl Operation {
    const fn name(self) -> &'static str {
        match self {
            Self::IdentitySummary => "identity.summary",
            Self::SubmissionStatus => "submission.status",
            Self::LxmfNext => "experimental.lxmf.next",
            Self::LxmfRead => "experimental.lxmf.read",
            Self::LxmfBasicSend => "experimental.lxmf.basic_send",
            Self::LxmfPeerNext => "experimental.lxmf.peer_next",
        }
    }
}

/// Framing-layer stream failure before session authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramingFault {
    /// COBS body was malformed.
    MalformedCobs,
    /// Decoded record was noncanonical.
    MalformedRecord,
    /// Record exceeded the fixed framing capacity.
    Overflow,
}

/// Fatal client operation failure.
///
/// Handshake, framing, transport, or authenticated-session failures consume
/// the live session. Logical device API errors do not; another typed operation
/// may be attempted on the same client.
#[derive(Debug)]
pub enum ClientError {
    /// The requested deadline could not be represented by the host clock.
    DeadlineOverflow {
        /// Stage selecting the deadline.
        stage: &'static str,
    },
    /// A bounded transport stage reached its deadline.
    Timeout {
        /// Stage that timed out.
        stage: &'static str,
    },
    /// A transport returned a successful zero-byte write.
    WriteNoProgress {
        /// Stage being written.
        stage: &'static str,
    },
    /// Host byte-stream I/O failed.
    Io {
        /// Stage performing I/O.
        stage: &'static str,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Framed stream was malformed.
    Framing(FramingFault),
    /// Portable session handshake rejected the peer or local inputs.
    Handshake(ClientHandshakeError),
    /// An outbound flight reported impossible backend acknowledgement.
    FlightAcknowledgement {
        /// Stage owning the outbound flight.
        stage: &'static str,
    },
    /// An outbound logical request could not be framed.
    RequestFraming(ClientRequestFaultKind),
    /// An authenticated response failed session checks.
    ResponseAuthentication(ClientSessionFault),
    /// Logical request encoding failed.
    RequestEncoding(EncodeError),
    /// Logical response decoding failed.
    ResponseDecoding(DecodeError),
    /// Device API major version is incompatible.
    IncompatibleApiVersion {
        /// Device-selected version.
        observed: ApiVersion,
    },
    /// Device echoed a different logical request ID.
    RequestIdMismatch {
        /// ID sent by this client.
        expected: RequestId,
        /// ID returned by the device.
        observed: RequestId,
    },
    /// No later request ID can be allocated.
    RequestIdExhausted,
    /// A previous transport/session fault consumed the connection.
    SessionUnavailable,
    /// Device returned a typed logical API error.
    Api {
        /// Operation that was rejected.
        operation: Operation,
        /// Exact device error.
        error: ApiErrorResponse,
    },
    /// Authenticated response kind did not match the operation.
    UnexpectedResponse {
        /// Requested operation.
        operation: Operation,
        /// Returned response kind.
        kind: u16,
    },
    /// LXMF enumeration did not advance monotonically.
    NonAdvancingLxmfHandle {
        /// Previous exclusive cursor.
        previous: LxmfMessageHandle,
        /// Invalid returned handle.
        observed: LxmfMessageHandle,
    },
    /// Bounded list policy was reached while another item existed.
    LxmfListLimitExceeded {
        /// Configured summary limit.
        limit: usize,
    },
    /// Requested LXMF handle was absent.
    LxmfNotFound {
        /// Requested stable handle.
        handle: LxmfMessageHandle,
    },
    /// Authenticated summary exceeds the caller's complete-wire policy.
    LxmfWireTooLarge {
        /// Authenticated wire length.
        actual: u32,
        /// Host-configured ceiling.
        maximum: usize,
    },
    /// Complete wire length does not fit this host process.
    LxmfWireLengthUnsupported {
        /// Authenticated wire length.
        actual: u32,
    },
    /// A chunk contradicted its authenticated summary or failed to advance.
    LxmfChunkMismatch {
        /// Contradicted property.
        property: &'static str,
    },
    /// Complete wire digest disagreed with authenticated metadata.
    LxmfDigestMismatch,
    /// Downloaded normalized wire was structurally invalid.
    LxmfWire(WireError),
    /// Parsed normalized wire contradicted authenticated summary metadata.
    LxmfSummaryMismatch {
        /// Contradicted property.
        property: &'static str,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeadlineOverflow { stage } => write!(formatter, "{stage} deadline overflowed"),
            Self::Timeout { stage } => write!(formatter, "timed out during {stage}"),
            Self::WriteNoProgress { stage } => {
                write!(formatter, "transport made no progress writing {stage}")
            }
            Self::Io { stage, source } => write!(formatter, "{stage} I/O failed: {source}"),
            Self::Framing(fault) => write!(formatter, "framing failed: {fault:?}"),
            Self::Handshake(error) => write!(formatter, "handshake failed: {error:?}"),
            Self::FlightAcknowledgement { stage } => {
                write!(formatter, "transport over-acknowledged {stage}")
            }
            Self::RequestFraming(kind) => write!(formatter, "request framing failed: {kind:?}"),
            Self::ResponseAuthentication(fault) => {
                write!(formatter, "response authentication failed: {fault:?}")
            }
            Self::RequestEncoding(error) => write!(formatter, "request encoding failed: {error:?}"),
            Self::ResponseDecoding(error) => {
                write!(formatter, "response decoding failed: {error:?}")
            }
            Self::IncompatibleApiVersion { observed } => write!(
                formatter,
                "device API {}.{} is incompatible with client major {}",
                observed.major,
                observed.minor,
                ApiVersion::CURRENT.major
            ),
            Self::RequestIdMismatch { expected, observed } => write!(
                formatter,
                "device returned request ID {} instead of {}",
                observed.0, expected.0
            ),
            Self::RequestIdExhausted => formatter.write_str("logical request ID space exhausted"),
            Self::SessionUnavailable => formatter.write_str("authenticated session is unavailable"),
            Self::Api { operation, error } => write!(
                formatter,
                "device rejected {}: code={} operation={:?}",
                operation.name(),
                error.code.wire_code(),
                error.operation
            ),
            Self::UnexpectedResponse { operation, kind } => write!(
                formatter,
                "device returned response kind {kind} for {}",
                operation.name()
            ),
            Self::NonAdvancingLxmfHandle { previous, observed } => write!(
                formatter,
                "device returned LXMF handle {} after {}",
                observed.get(),
                previous.get()
            ),
            Self::LxmfListLimitExceeded { limit } => {
                write!(formatter, "LXMF list exceeds configured limit {limit}")
            }
            Self::LxmfNotFound { handle } => {
                write!(formatter, "LXMF handle {} was not found", handle.get())
            }
            Self::LxmfWireTooLarge { actual, maximum } => write!(
                formatter,
                "LXMF wire is {actual} bytes; host limit is {maximum}"
            ),
            Self::LxmfWireLengthUnsupported { actual } => {
                write!(
                    formatter,
                    "LXMF wire length {actual} does not fit this host"
                )
            }
            Self::LxmfChunkMismatch { property } => {
                write!(formatter, "LXMF chunk contradicted {property}")
            }
            Self::LxmfDigestMismatch => {
                formatter.write_str("LXMF wire digest disagrees with authenticated summary")
            }
            Self::LxmfWire(error) => write!(formatter, "invalid LXMF wire: {error}"),
            Self::LxmfSummaryMismatch { property } => {
                write!(formatter, "LXMF wire contradicted summary {property}")
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Borrowed semantic input for source-free basic LXMF composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BasicLxmfSend<'a> {
    destination: DestinationHash,
    timestamp_unix_ms: u64,
    title: &'a [u8],
    content: &'a [u8],
    idempotency_key: IdempotencyKey,
}

impl<'a> BasicLxmfSend<'a> {
    /// Construct one replayable send request.
    pub const fn new(
        destination: DestinationHash,
        timestamp_unix_ms: u64,
        title: &'a [u8],
        content: &'a [u8],
        idempotency_key: IdempotencyKey,
    ) -> Self {
        Self {
            destination,
            timestamp_unix_ms,
            title,
            content,
            idempotency_key,
        }
    }

    /// Remote `lxmf.delivery` destination.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Caller-retained Unix timestamp in whole milliseconds.
    pub const fn timestamp_unix_ms(self) -> u64 {
        self.timestamp_unix_ms
    }

    /// Binary LXMF title.
    pub const fn title(self) -> &'a [u8] {
        self.title
    }

    /// Binary LXMF content.
    pub const fn content(self) -> &'a [u8] {
        self.content
    }

    /// Principal-scoped replay key.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }
}

/// Complete digest-verified normalized LXMF wire plus its authenticated summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LxmfMessage {
    summary: LxmfMessageSummary,
    wire: Vec<u8>,
}

impl LxmfMessage {
    /// Authenticated committed-message summary.
    pub const fn summary(&self) -> LxmfMessageSummary {
        self.summary
    }

    /// Exact complete normalized wire bytes.
    pub fn wire(&self) -> &[u8] {
        &self.wire
    }

    /// Reparse the immutable normalized wire as a bounded borrowed view.
    pub fn view(&self) -> Result<MessageView<'_>, WireError> {
        MessageView::parse_complete(&self.wire, wire_limits(self.wire.len()))
    }
}

/// Established sequential authenticated client over one byte-stream transport.
pub struct DeviceClient<T> {
    transport: T,
    reader: RecordReader,
    session: Option<ClientSession>,
    device_id: DeviceId,
    session_id: SessionId,
    next_request_id: Option<u64>,
    config: ClientConfig,
}

impl<T: ClientTransport> DeviceClient<T> {
    /// Authenticate one already-open USB Serial/JTAG byte stream.
    ///
    /// `transport` must use finite blocking I/O timeouts. The supplied random
    /// source provides the fresh client handshake nonce.
    pub fn connect<R>(
        transport: T,
        credential: ActivatedCredential,
        rng: &mut R,
        config: ClientConfig,
    ) -> Result<Self, ClientError>
    where
        R: RngCore + CryptoRng,
    {
        Self::connect_with_profile(
            transport,
            credential,
            rng,
            config,
            ClientSessionProfile::UsbSerialJtagQualification,
        )
    }

    /// Authenticate one already-open byte stream under an explicit local-bearer profile.
    ///
    /// `transport` must use finite blocking I/O timeouts. The supplied random
    /// source provides the fresh client handshake nonce. Profile selection is
    /// never inferred from the stream or endpoint, so a wireless connection
    /// cannot silently fall back to a different bearer transcript.
    pub fn connect_with_profile<R>(
        mut transport: T,
        credential: ActivatedCredential,
        rng: &mut R,
        config: ClientConfig,
        profile: ClientSessionProfile,
    ) -> Result<Self, ClientError>
    where
        R: RngCore + CryptoRng,
    {
        let device_id = credential.device_id();
        let parameters = profile.parameters(device_id);
        let credential = credential.into_handshake();
        let deadline = deadline(config.handshake_timeout, "handshake")?;
        let hello = ClientHelloFlight::begin(parameters, credential, rng)
            .map_err(ClientError::Handshake)?;
        let hello = write_flight(&mut transport, hello, deadline, "client hello")?;
        let awaiting_hello =
            hello
                .try_finish()
                .map_err(|_| ClientError::FlightAcknowledgement {
                    stage: "client hello",
                })?;

        let mut reader = RecordReader::new();
        let server_hello = reader.read_record(&mut transport, deadline, "server hello")?;
        let awaiting_proof = awaiting_hello
            .accept(server_hello)
            .map_err(ClientError::Handshake)?;
        let server_proof = reader.read_record(&mut transport, deadline, "server proof")?;
        let proof = awaiting_proof
            .verify(server_proof)
            .map_err(ClientError::Handshake)?;
        let proof = write_flight(&mut transport, proof, deadline, "client proof")?;
        let session = proof
            .try_finish()
            .map_err(|_| ClientError::FlightAcknowledgement {
                stage: "client proof",
            })?;
        let session_id = session.session_id();

        Ok(Self {
            transport,
            reader,
            session: Some(session),
            device_id,
            session_id,
            next_request_id: Some(1),
            config,
        })
    }

    /// Expected device ID authenticated during the handshake.
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Handshake-derived session ID.
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Whether no transport/session fault has consumed this connection.
    pub const fn is_session_available(&self) -> bool {
        self.session.is_some()
    }

    /// Recover the underlying transport, ending this session without a close record.
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Read the node's public primary and optional LXMF delivery destinations.
    pub fn identity_summary(&mut self) -> Result<IdentitySummary, ClientError> {
        match self.exchange(DeviceRequest::IdentitySummary)? {
            DeviceResponse::IdentitySummary(summary) => Ok(summary),
            DeviceResponse::Error(error) => Err(ClientError::Api {
                operation: Operation::IdentitySummary,
                error,
            }),
            other => Err(ClientError::UnexpectedResponse {
                operation: Operation::IdentitySummary,
                kind: other.kind(),
            }),
        }
    }

    /// Read durable state for one accepted submission.
    pub fn submission_status(&mut self, id: SubmissionId) -> Result<SubmissionStatus, ClientError> {
        match self.exchange(DeviceRequest::SubmissionStatus { id })? {
            DeviceResponse::SubmissionStatus(status) => Ok(status),
            DeviceResponse::Error(error) => Err(ClientError::Api {
                operation: Operation::SubmissionStatus,
                error,
            }),
            other => Err(ClientError::UnexpectedResponse {
                operation: Operation::SubmissionStatus,
                kind: other.kind(),
            }),
        }
    }

    /// Read the next committed LXMF summary after an optional stable cursor.
    pub fn lxmf_next(
        &mut self,
        after: Option<LxmfMessageHandle>,
    ) -> Result<Option<LxmfMessageSummary>, ClientError> {
        match self.exchange(DeviceRequest::LxmfNext { after })? {
            DeviceResponse::LxmfNext(summary) => {
                if let Some(previous) = after
                    && summary.handle().get() <= previous.get()
                {
                    return Err(ClientError::NonAdvancingLxmfHandle {
                        previous,
                        observed: summary.handle(),
                    });
                }
                Ok(Some(summary))
            }
            DeviceResponse::Error(error)
                if error.code == reticulum_device_api::ApiErrorCode::NotFound =>
            {
                Ok(None)
            }
            DeviceResponse::Error(error) => Err(ClientError::Api {
                operation: Operation::LxmfNext,
                error,
            }),
            other => Err(ClientError::UnexpectedResponse {
                operation: Operation::LxmfNext,
                kind: other.kind(),
            }),
        }
    }

    /// Enumerate all committed LXMF summaries within the configured host limit.
    pub fn lxmf_list(&mut self) -> Result<Vec<LxmfMessageSummary>, ClientError> {
        let mut summaries = Vec::new();
        let mut after = None;
        while let Some(summary) = self.lxmf_next(after)? {
            if summaries.len() == self.config.max_lxmf_messages {
                return Err(ClientError::LxmfListLimitExceeded {
                    limit: self.config.max_lxmf_messages,
                });
            }
            after = Some(summary.handle());
            summaries.push(summary);
        }
        Ok(summaries)
    }

    /// Find, download, digest-check, parse, and cross-check one committed LXMF message.
    pub fn lxmf_read(&mut self, handle: LxmfMessageHandle) -> Result<LxmfMessage, ClientError> {
        let summary = self.find_lxmf_summary(handle)?;
        self.lxmf_read_summary(summary)
    }

    /// Download a summary already obtained from this device connection.
    pub fn lxmf_read_summary(
        &mut self,
        summary: LxmfMessageSummary,
    ) -> Result<LxmfMessage, ClientError> {
        if summary.normalized_wire_len() as u64 > self.config.max_lxmf_wire_bytes as u64 {
            return Err(ClientError::LxmfWireTooLarge {
                actual: summary.normalized_wire_len(),
                maximum: self.config.max_lxmf_wire_bytes,
            });
        }
        let total = usize::try_from(summary.normalized_wire_len()).map_err(|_| {
            ClientError::LxmfWireLengthUnsupported {
                actual: summary.normalized_wire_len(),
            }
        })?;
        let mut wire = Vec::new();
        wire.try_reserve_exact(total)
            .map_err(|_| ClientError::LxmfWireTooLarge {
                actual: summary.normalized_wire_len(),
                maximum: self.config.max_lxmf_wire_bytes,
            })?;

        let mut offset = 0_u32;
        let mut hasher = Sha256::new();
        while offset < summary.normalized_wire_len() {
            let remaining = summary.normalized_wire_len() - offset;
            let requested =
                LxmfReadLength::new(remaining.min(MAX_LXMF_READ_CHUNK_BYTES as u32) as u16)
                    .expect("positive remaining length is capped to the API chunk maximum");
            let response = self.exchange(DeviceRequest::LxmfRead {
                handle: summary.handle(),
                offset,
                max_bytes: requested,
            })?;
            let chunk = match response {
                DeviceResponse::LxmfRead(chunk) => chunk,
                DeviceResponse::Error(error) => {
                    return Err(ClientError::Api {
                        operation: Operation::LxmfRead,
                        error,
                    });
                }
                other => {
                    return Err(ClientError::UnexpectedResponse {
                        operation: Operation::LxmfRead,
                        kind: other.kind(),
                    });
                }
            };
            if chunk.handle() != summary.handle() {
                return Err(ClientError::LxmfChunkMismatch { property: "handle" });
            }
            if chunk.offset() != offset {
                return Err(ClientError::LxmfChunkMismatch { property: "offset" });
            }
            if chunk.total_len() != summary.normalized_wire_len() {
                return Err(ClientError::LxmfChunkMismatch {
                    property: "total length",
                });
            }
            if chunk.bytes().is_empty() || chunk.bytes().len() > usize::from(requested.get()) {
                return Err(ClientError::LxmfChunkMismatch {
                    property: "progress",
                });
            }
            let next = offset.checked_add(chunk.bytes().len() as u32).ok_or(
                ClientError::LxmfChunkMismatch {
                    property: "offset overflow",
                },
            )?;
            if next > summary.normalized_wire_len()
                || chunk.is_final() != (next == summary.normalized_wire_len())
            {
                return Err(ClientError::LxmfChunkMismatch {
                    property: "final boundary",
                });
            }
            wire.extend_from_slice(chunk.bytes());
            hasher.update(chunk.bytes());
            offset = next;
        }

        let digest: [u8; 32] = hasher.finalize().into();
        if digest != *summary.exact_wire_sha256() {
            return Err(ClientError::LxmfDigestMismatch);
        }
        validate_wire_against_summary(summary, &wire)?;
        Ok(LxmfMessage { summary, wire })
    }

    /// Compose with the device-owned source and durably accept one basic LXMF message.
    pub fn lxmf_basic_send(
        &mut self,
        send: BasicLxmfSend<'_>,
    ) -> Result<LxmfBasicSendAccepted, ClientError> {
        match self.exchange(DeviceRequest::LxmfBasicSend {
            destination: send.destination,
            timestamp_unix_ms: send.timestamp_unix_ms,
            title: send.title,
            content: send.content,
            idempotency_key: send.idempotency_key,
        })? {
            DeviceResponse::LxmfBasicSendAccepted(accepted) => Ok(accepted),
            DeviceResponse::Error(error) => Err(ClientError::Api {
                operation: Operation::LxmfBasicSend,
                error,
            }),
            other => Err(ClientError::UnexpectedResponse {
                operation: Operation::LxmfBasicSend,
                kind: other.kind(),
            }),
        }
    }

    /// Read one bounded nearby `lxmf.delivery` peer after an optional boot-scoped cursor.
    pub fn lxmf_peer_next(
        &mut self,
        after: Option<LxmfPeerDiscoveryCursor>,
    ) -> Result<LxmfPeerDiscoveryPage, ClientError> {
        match self.exchange(DeviceRequest::LxmfPeerNext { after })? {
            DeviceResponse::LxmfPeerNext(page) => Ok(page),
            DeviceResponse::Error(error) => Err(ClientError::Api {
                operation: Operation::LxmfPeerNext,
                error,
            }),
            other => Err(ClientError::UnexpectedResponse {
                operation: Operation::LxmfPeerNext,
                kind: other.kind(),
            }),
        }
    }

    fn find_lxmf_summary(
        &mut self,
        target: LxmfMessageHandle,
    ) -> Result<LxmfMessageSummary, ClientError> {
        let mut after = None;
        let mut visited = 0_usize;
        loop {
            let Some(summary) = self.lxmf_next(after)? else {
                return Err(ClientError::LxmfNotFound { handle: target });
            };
            visited += 1;
            if visited > self.config.max_lxmf_messages {
                return Err(ClientError::LxmfListLimitExceeded {
                    limit: self.config.max_lxmf_messages,
                });
            }
            if summary.handle() == target {
                return Ok(summary);
            }
            if summary.handle().get() > target.get() {
                return Err(ClientError::LxmfNotFound { handle: target });
            }
            after = Some(summary.handle());
        }
    }

    fn exchange(&mut self, request: DeviceRequest<'_>) -> Result<DeviceResponse, ClientError> {
        let request_id = self.allocate_request_id()?;
        let envelope = RequestEnvelope {
            version: ApiVersion::CURRENT,
            request_id,
            request,
        };
        let mut request_bytes = [0_u8; MAX_MESSAGE_BYTES];
        let request_length =
            encode_request(&envelope, &mut request_bytes).map_err(ClientError::RequestEncoding)?;
        let request_owner = OwnedMessage::new(
            MessageLength::new(request_length)
                .expect("logical API and session use the same maximum message size"),
            request_bytes,
        );
        let session = self.session.take().ok_or(ClientError::SessionUnavailable)?;
        let request_flight = session
            .frame_request(request_owner)
            .map_err(|fault| ClientError::RequestFraming(fault.kind()))?;
        let deadline = deadline(self.config.request_timeout, "authenticated request")?;
        let request_flight = write_flight(
            &mut self.transport,
            request_flight,
            deadline,
            "authenticated request",
        )?;
        let awaiting =
            request_flight
                .try_finish()
                .map_err(|_| ClientError::FlightAcknowledgement {
                    stage: "authenticated request",
                })?;
        let response_record =
            self.reader
                .read_record(&mut self.transport, deadline, "authenticated response")?;
        let authenticated = awaiting
            .authenticate(response_record)
            .map_err(ClientError::ResponseAuthentication)?;
        let (session, response_message) = authenticated.into_parts();
        self.session = Some(session);

        let response =
            decode_response(response_message.encoded()).map_err(ClientError::ResponseDecoding)?;
        if response.version.major != ApiVersion::CURRENT.major {
            return Err(ClientError::IncompatibleApiVersion {
                observed: response.version,
            });
        }
        if response.request_id != request_id {
            return Err(ClientError::RequestIdMismatch {
                expected: request_id,
                observed: response.request_id,
            });
        }
        Ok(response.response)
    }

    fn allocate_request_id(&mut self) -> Result<RequestId, ClientError> {
        let value = self
            .next_request_id
            .ok_or(ClientError::RequestIdExhausted)?;
        self.next_request_id = value.checked_add(1);
        Ok(RequestId(value))
    }
}

trait OutboundFlight {
    fn remaining(&self) -> &[u8];
    fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError>;
}

impl OutboundFlight for ClientHelloFlight {
    fn remaining(&self) -> &[u8] {
        ClientHelloFlight::remaining(self)
    }

    fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        ClientHelloFlight::advance(self, acknowledged)
    }
}

impl OutboundFlight for ClientProofFlight {
    fn remaining(&self) -> &[u8] {
        ClientProofFlight::remaining(self)
    }

    fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        ClientProofFlight::advance(self, acknowledged)
    }
}

impl OutboundFlight for ClientRequestFlight {
    fn remaining(&self) -> &[u8] {
        ClientRequestFlight::remaining(self)
    }

    fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        ClientRequestFlight::advance(self, acknowledged)
    }
}

fn write_flight<W: Write + ?Sized, F: OutboundFlight>(
    writer: &mut W,
    mut flight: F,
    deadline: Instant,
    stage: &'static str,
) -> Result<F, ClientError> {
    while !flight.remaining().is_empty() {
        if Instant::now() >= deadline {
            return Err(ClientError::Timeout { stage });
        }
        match writer.write(flight.remaining()) {
            Ok(0) => return Err(ClientError::WriteNoProgress { stage }),
            Ok(written) => flight
                .advance(written)
                .map_err(|_| ClientError::FlightAcknowledgement { stage })?,
            Err(error) if retryable_io(&error) => {}
            Err(source) => return Err(ClientError::Io { stage, source }),
        }
    }
    loop {
        if Instant::now() >= deadline {
            return Err(ClientError::Timeout { stage });
        }
        match writer.flush() {
            Ok(()) => return Ok(flight),
            Err(error) if retryable_io(&error) => {}
            Err(source) => return Err(ClientError::Io { stage, source }),
        }
    }
}

struct RecordReader {
    decoder: StreamDecoder,
    bytes: Zeroizing<[u8; READ_BUFFER_CAPACITY]>,
    next: usize,
    length: usize,
}

impl RecordReader {
    fn new() -> Self {
        Self {
            decoder: StreamDecoder::new(),
            bytes: Zeroizing::new([0_u8; READ_BUFFER_CAPACITY]),
            next: 0,
            length: 0,
        }
    }

    fn read_record<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        deadline: Instant,
        stage: &'static str,
    ) -> Result<Record, ClientError> {
        loop {
            while self.next < self.length {
                let byte = self.bytes[self.next];
                self.next += 1;
                match self.decoder.push(byte) {
                    DecodeEvent::Pending => {}
                    DecodeEvent::Record(record) => return Ok(record),
                    DecodeEvent::MalformedCobs => {
                        return Err(ClientError::Framing(FramingFault::MalformedCobs));
                    }
                    DecodeEvent::MalformedRecord(_) => {
                        return Err(ClientError::Framing(FramingFault::MalformedRecord));
                    }
                    DecodeEvent::Overflow => {
                        return Err(ClientError::Framing(FramingFault::Overflow));
                    }
                }
            }

            self.next = 0;
            self.length = 0;
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout { stage });
            }
            match reader.read(&mut self.bytes[..]) {
                Ok(0) => std::thread::yield_now(),
                Ok(length) => self.length = length,
                Err(error) if retryable_io(&error) => {}
                Err(source) => return Err(ClientError::Io { stage, source }),
            }
        }
    }
}

fn retryable_io(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn deadline(timeout: Duration, stage: &'static str) -> Result<Instant, ClientError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(ClientError::DeadlineOverflow { stage })
}

fn wire_limits(length: usize) -> WireLimits {
    WireLimits::new(
        length,
        length,
        length,
        length,
        length.saturating_mul(16).max(65_536),
        LXMF_PARSE_MAX_NESTING,
    )
}

fn validate_wire_against_summary(
    summary: LxmfMessageSummary,
    wire: &[u8],
) -> Result<(), ClientError> {
    let message = MessageView::parse_complete(wire, wire_limits(wire.len()))
        .map_err(ClientError::LxmfWire)?;
    if message.normalized_wire_len() != summary.normalized_wire_len() as usize {
        return Err(ClientError::LxmfSummaryMismatch {
            property: "wire length",
        });
    }
    if message.message_id() != *summary.message_id() {
        return Err(ClientError::LxmfSummaryMismatch {
            property: "message ID",
        });
    }
    if message.destination_hash() != &summary.destination().0 {
        return Err(ClientError::LxmfSummaryMismatch {
            property: "destination",
        });
    }
    if message.source_hash() != &summary.source().0 {
        return Err(ClientError::LxmfSummaryMismatch { property: "source" });
    }
    let payload = message.payload();
    if payload.timestamp_bits() != summary.timestamp_bits() {
        return Err(ClientError::LxmfSummaryMismatch {
            property: "timestamp",
        });
    }
    if payload.title().as_bytes().len() != summary.title_len() as usize {
        return Err(ClientError::LxmfSummaryMismatch {
            property: "title length",
        });
    }
    if payload.content().as_bytes().len() != summary.content_len() as usize {
        return Err(ClientError::LxmfSummaryMismatch {
            property: "content length",
        });
    }
    if payload.fields().raw().len() != summary.fields_encoded_len() as usize {
        return Err(ClientError::LxmfSummaryMismatch {
            property: "fields length",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
