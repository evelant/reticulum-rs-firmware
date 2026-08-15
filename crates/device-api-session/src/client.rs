//! Portable client handshake and request/response typestates.

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};
use reticulum_device_api_framing::{
    AUTH_TAG_LENGTH, FrameEncodeError, FramedRecord, PayloadLength, Record, TxAdvanceError,
};
use reticulum_device_api_handoff::{MessageLength, OwnedMessage};
use zeroize::Zeroizing;

use crate::{
    crypto::{
        KeySchedule, client_proof, client_record_tag, derive, verify_server_proof,
        verify_server_record_tag,
    },
    protocol::{
        BearerBinding, ClientHello, DeviceId, HandshakeRecordError, RECORD_KIND_CLIENT_PROOF,
        RECORD_KIND_REQUEST, RECORD_KIND_RESPONSE, RECORD_KIND_SERVER_PROOF, ServerHello,
        SessionId, SessionSuite, proof_record, take_proof,
    },
};

/// Expected device and bearer facts for one client-side handshake attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientParameters {
    expected_device_id: DeviceId,
    bearer: BearerBinding,
    suite: SessionSuite,
}

impl ClientParameters {
    /// Bind the supported BLE GATT handshake to one expected device ID and bearer.
    pub const fn new(expected_device_id: DeviceId, bearer: BearerBinding) -> Self {
        Self::new_for_suite(expected_device_id, bearer, SessionSuite::BleGatt)
    }

    /// Bind a handshake attempt to one device, bearer, and explicit suite.
    pub const fn new_for_suite(
        expected_device_id: DeviceId,
        bearer: BearerBinding,
        suite: SessionSuite,
    ) -> Self {
        Self {
            expected_device_id,
            bearer,
            suite,
        }
    }

    /// Stable device API identifier the client expects to authenticate.
    pub const fn expected_device_id(self) -> DeviceId {
        self.expected_device_id
    }

    /// Actual local bearer carrying this handshake.
    pub const fn bearer(self) -> BearerBinding {
        self.bearer
    }

    /// Cryptographic session suite this client requires.
    pub const fn suite(self) -> SessionSuite {
        self.suite
    }
}

/// Client-owned credential material for one handshake attempt.
///
/// This owner deliberately implements neither `Clone`, `Copy`, nor `Debug`.
/// Consuming it into a handshake keeps the PSK in zeroizing storage until the
/// server hello has been qualified and the session key schedule is derived.
pub struct ClientCredential {
    id: CredentialId,
    generation: CredentialGeneration,
    psk: Zeroizing<[u8; 32]>,
}

impl ClientCredential {
    /// Construct a client credential from an exact PSK owner.
    pub fn new(id: CredentialId, generation: CredentialGeneration, psk: [u8; 32]) -> Self {
        Self {
            id,
            generation,
            psk: Zeroizing::new(psk),
        }
    }

    /// Construct a client credential without copying an existing zeroizing PSK owner.
    pub const fn from_zeroizing(
        id: CredentialId,
        generation: CredentialGeneration,
        psk: Zeroizing<[u8; 32]>,
    ) -> Self {
        Self {
            id,
            generation,
            psk,
        }
    }

    /// Opaque credential identifier sent in the client hello.
    pub const fn id(&self) -> CredentialId {
        self.id
    }

    /// Device-owned authorization generation the client expects.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }
}

/// Fatal failure before a client session becomes established.
#[derive(Debug)]
pub enum ClientHandshakeError {
    /// A server handshake record was not canonical.
    Record(HandshakeRecordError),
    /// A session suite was configured for a bearer it does not permit.
    SuiteBearerMismatch {
        /// Configured session suite.
        suite: SessionSuite,
        /// Bearer configured for the handshake attempt.
        bearer: BearerBinding,
        /// Sole bearer permitted by the configured suite.
        required: BearerBinding,
    },
    /// Server selected a different supported suite than the client requires.
    SuiteMismatch {
        /// Suite required by the client instance.
        expected: SessionSuite,
        /// Suite declared by the server.
        observed: SessionSuite,
    },
    /// Server selected a different bearer than the client instance owns.
    BearerMismatch {
        /// Bearer owned by the client instance.
        expected: BearerBinding,
        /// Bearer declared by the server.
        observed: BearerBinding,
    },
    /// Server declared a different stable device identifier.
    DeviceMismatch {
        /// Device identifier selected by the client.
        expected: DeviceId,
        /// Device identifier declared by the server.
        observed: DeviceId,
    },
    /// Server authenticated a different credential generation.
    CredentialGenerationMismatch {
        /// Credential generation selected by the client.
        expected: CredentialGeneration,
        /// Credential generation declared by the server.
        observed: CredentialGeneration,
    },
    /// The server proof did not authenticate under the selected credential.
    AuthenticationFailed,
    /// Qualified entropy failed while producing the fresh client nonce.
    Entropy(rand_core::Error),
    /// A fixed handshake record unexpectedly failed framing.
    Framing,
}

impl From<HandshakeRecordError> for ClientHandshakeError {
    fn from(error: HandshakeRecordError) -> Self {
        Self::Record(error)
    }
}

impl From<FrameEncodeError> for ClientHandshakeError {
    fn from(_: FrameEncodeError) -> Self {
        Self::Framing
    }
}

/// Exact outbound client-hello bytes plus retained credential state.
///
/// The caller must completely acknowledge this frame before
/// [`Self::try_finish`] yields the state waiting for a server hello. Dropping
/// an incomplete flight destroys the retained credential and nonce state.
#[must_use = "a client hello flight must be fully transmitted or explicitly dropped"]
pub struct ClientHelloFlight {
    pending: AwaitingServerHello,
    frame: FramedRecord,
}

impl ClientHelloFlight {
    /// Start one session handshake with fresh client entropy.
    pub fn begin<R>(
        parameters: ClientParameters,
        credential: ClientCredential,
        rng: &mut R,
    ) -> Result<Self, ClientHandshakeError>
    where
        R: RngCore + CryptoRng,
    {
        let required_bearer = parameters.suite.required_bearer();
        if parameters.bearer != required_bearer {
            return Err(ClientHandshakeError::SuiteBearerMismatch {
                suite: parameters.suite,
                bearer: parameters.bearer,
                required: required_bearer,
            });
        }

        let mut client_nonce = [0_u8; 32];
        rng.try_fill_bytes(&mut client_nonce)
            .map_err(ClientHandshakeError::Entropy)?;
        let client_hello = ClientHello::new_for_suite(
            parameters.suite,
            parameters.bearer,
            credential.id,
            client_nonce,
        );
        let frame = FramedRecord::encode(&client_hello.into_record())?;
        Ok(Self {
            pending: AwaitingServerHello {
                parameters,
                credential,
                client_hello,
            },
            frame,
        })
    }

    /// Bytes not yet acknowledged by the bearer backend.
    pub fn remaining(&self) -> &[u8] {
        self.frame.remaining()
    }

    /// At most `maximum` bytes for the next backend write.
    pub fn next_chunk(&self, maximum: usize) -> &[u8] {
        self.frame.next_chunk(maximum)
    }

    /// Advance only after the backend completes that exact byte count.
    pub fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        self.frame.advance(acknowledged)
    }

    /// Finish only after every client-hello byte has been acknowledged.
    #[allow(
        clippy::result_large_err,
        reason = "an incomplete flight must return its exact fixed-capacity owner"
    )]
    pub fn try_finish(self) -> Result<AwaitingServerHello, Self> {
        if self.frame.is_complete() {
            Ok(self.pending)
        } else {
            Err(self)
        }
    }
}

/// Retained client handshake waiting for one canonical server hello.
#[must_use = "pending credential material must receive a server hello or be explicitly dropped"]
pub struct AwaitingServerHello {
    parameters: ClientParameters,
    credential: ClientCredential,
    client_hello: ClientHello,
}

impl AwaitingServerHello {
    /// Qualify one server hello and derive the pending proof schedule.
    ///
    /// Any error consumes and terminates this handshake attempt.
    pub fn accept(self, record: Record) -> Result<AwaitingServerProof, ClientHandshakeError> {
        let server_hello = ServerHello::from_record(record)?;
        if server_hello.suite() != self.parameters.suite {
            return Err(ClientHandshakeError::SuiteMismatch {
                expected: self.parameters.suite,
                observed: server_hello.suite(),
            });
        }
        if server_hello.bearer() != self.parameters.bearer {
            return Err(ClientHandshakeError::BearerMismatch {
                expected: self.parameters.bearer,
                observed: server_hello.bearer(),
            });
        }
        if server_hello.device_id() != self.parameters.expected_device_id {
            return Err(ClientHandshakeError::DeviceMismatch {
                expected: self.parameters.expected_device_id,
                observed: server_hello.device_id(),
            });
        }
        if server_hello.credential_generation() != self.credential.generation {
            return Err(ClientHandshakeError::CredentialGenerationMismatch {
                expected: self.credential.generation,
                observed: server_hello.credential_generation(),
            });
        }

        let schedule = derive(&self.credential.psk, &self.client_hello, &server_hello);
        Ok(AwaitingServerProof { schedule })
    }
}

/// Retained key schedule waiting for the server's full proof.
#[must_use = "pending handshake key material must be authenticated or explicitly dropped"]
pub struct AwaitingServerProof {
    schedule: KeySchedule,
}

impl AwaitingServerProof {
    /// Verify the server proof and construct the exact outbound client proof.
    ///
    /// Any record or authentication error consumes and terminates the attempt.
    pub fn verify(self, record: Record) -> Result<ClientProofFlight, ClientHandshakeError> {
        let server_proof = take_proof(record, RECORD_KIND_SERVER_PROOF, self.schedule.session_id)?;
        if !verify_server_proof(&self.schedule, &server_proof) {
            return Err(ClientHandshakeError::AuthenticationFailed);
        }

        let proof = client_proof(&self.schedule, &server_proof);
        let frame = FramedRecord::encode(&proof_record(
            RECORD_KIND_CLIENT_PROOF,
            self.schedule.session_id,
            &proof,
        ))?;
        let KeySchedule {
            transcript_hash: _,
            server_proof_key: _,
            client_proof_key: _,
            client_record_key,
            server_record_key,
            session_id,
        } = self.schedule;
        Ok(ClientProofFlight {
            session: ClientSession {
                session_id,
                client_record_key,
                server_record_key,
                next_client_sequence: 0,
                next_server_sequence: 0,
            },
            frame,
        })
    }
}

/// Exact outbound client-proof bytes plus the newly derived client session.
#[must_use = "a client proof flight must be fully transmitted or explicitly dropped"]
pub struct ClientProofFlight {
    session: ClientSession,
    frame: FramedRecord,
}

impl ClientProofFlight {
    /// Bytes not yet acknowledged by the bearer backend.
    pub fn remaining(&self) -> &[u8] {
        self.frame.remaining()
    }

    /// At most `maximum` bytes for the next backend write.
    pub fn next_chunk(&self, maximum: usize) -> &[u8] {
        self.frame.next_chunk(maximum)
    }

    /// Advance only after the backend completes that exact byte count.
    pub fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        self.frame.advance(acknowledged)
    }

    /// Establish the session only after every client-proof byte is acknowledged.
    #[allow(
        clippy::result_large_err,
        reason = "an incomplete proof must retain its session and exact framed owner"
    )]
    pub fn try_finish(self) -> Result<ClientSession, Self> {
        if self.frame.is_complete() {
            Ok(self.session)
        } else {
            Err(self)
        }
    }
}

/// Fatal established-client authentication or ordering failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientSessionFault {
    /// Session received a record kind that is not a logical response.
    UnexpectedKind {
        /// Observed record kind.
        observed: u8,
    },
    /// Record carried another session identifier.
    WrongSession,
    /// Record sequence was a duplicate or gap.
    UnexpectedSequence {
        /// Exact next sequence required by the session.
        expected: u64,
        /// Sequence supplied by the record.
        observed: u64,
    },
    /// Accepting the record would exhaust and wrap the direction sequence.
    SequenceExhausted,
    /// Record authentication tag failed constant-time verification.
    BadTag,
}

/// Idle authenticated client session able to send exactly one request.
#[must_use = "an authenticated client session must be driven, disconnected, or explicitly dropped"]
pub struct ClientSession {
    session_id: SessionId,
    client_record_key: Zeroizing<[u8; 32]>,
    server_record_key: Zeroizing<[u8; 32]>,
    next_client_sequence: u64,
    next_server_sequence: u64,
}

impl ClientSession {
    /// Authenticate and frame one request, reserving its TX sequence.
    ///
    /// Sequence exhaustion or an unexpected fixed-capacity framing failure
    /// consumes and terminates the session but returns the exact message owner.
    #[allow(clippy::result_large_err)]
    pub fn frame_request(
        mut self,
        message: OwnedMessage,
    ) -> Result<ClientRequestFlight, ClientRequestFault> {
        if self.next_client_sequence == u64::MAX {
            return Err(ClientRequestFault {
                kind: ClientRequestFaultKind::SequenceExhausted,
                message,
            });
        }

        let (message_length, payload) = message.into_parts();
        let sequence = self.next_client_sequence;
        let untagged = Record::new(
            RECORD_KIND_REQUEST,
            self.session_id.0,
            sequence,
            PayloadLength::new(message_length.get())
                .expect("logical handoff and framing share the 512-byte capacity"),
            payload,
            [0_u8; AUTH_TAG_LENGTH],
        );
        let tag = client_record_tag(&self.client_record_key, &untagged);
        let (kind, session_id, sequence, length, payload, _) = untagged.into_parts();
        let tagged = Record::new(kind, session_id, sequence, length, payload, tag);
        let frame = match FramedRecord::encode(&tagged) {
            Ok(frame) => frame,
            Err(_) => {
                let (_, _, _, length, payload, _) = tagged.into_parts();
                return Err(ClientRequestFault {
                    kind: ClientRequestFaultKind::Framing,
                    message: OwnedMessage::new(
                        MessageLength::new(length.get())
                            .expect("framing and logical handoff share one capacity"),
                        payload,
                    ),
                });
            }
        };
        self.next_client_sequence += 1;
        Ok(ClientRequestFlight {
            awaiting: AwaitingResponse { session: self },
            frame,
        })
    }

    /// Handshake-derived session identifier.
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Exact next client-to-device sequence.
    pub const fn next_client_sequence(&self) -> u64 {
        self.next_client_sequence
    }

    /// Exact next device-to-client sequence.
    pub const fn next_server_sequence(&self) -> u64 {
        self.next_server_sequence
    }

    #[cfg(test)]
    pub(crate) fn set_sequences_for_test(&mut self, client: u64, server: u64) {
        self.next_client_sequence = client;
        self.next_server_sequence = server;
    }
}

/// Category for a request that could not be framed into the live session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientRequestFaultKind {
    /// Client-to-device record sequence cannot advance without wrapping.
    SequenceExhausted,
    /// Canonical request unexpectedly failed fixed-capacity framing.
    Framing,
}

/// Fatal request-framing failure retaining the exact unsent message owner.
#[must_use = "the unsent request must be retained, retried on a new session, or explicitly dropped"]
pub struct ClientRequestFault {
    kind: ClientRequestFaultKind,
    message: OwnedMessage,
}

impl ClientRequestFault {
    /// Failure category.
    pub const fn kind(&self) -> ClientRequestFaultKind {
        self.kind
    }

    /// Recover the complete unsent message owner.
    pub fn into_message(self) -> OwnedMessage {
        self.message
    }
}

/// Exact authenticated request bytes with an acknowledgement cursor.
///
/// The sole awaiting-response state remains inside this owner. A backend write
/// future with uncertain cancellation semantics must be driven to completion
/// before calling [`Self::try_finish`]. Dropping the flight terminates the
/// session and therefore cannot reuse its already-reserved sequence.
#[must_use = "a client request flight must be completely acknowledged or explicitly dropped"]
pub struct ClientRequestFlight {
    awaiting: AwaitingResponse,
    frame: FramedRecord,
}

impl ClientRequestFlight {
    /// Bytes not yet acknowledged by the bearer backend.
    pub fn remaining(&self) -> &[u8] {
        self.frame.remaining()
    }

    /// At most `maximum` bytes for the next backend write.
    pub fn next_chunk(&self, maximum: usize) -> &[u8] {
        self.frame.next_chunk(maximum)
    }

    /// Advance only after the backend completes that exact byte count.
    pub fn advance(&mut self, acknowledged: usize) -> Result<(), TxAdvanceError> {
        self.frame.advance(acknowledged)
    }

    /// Wait for a response only after every request byte has been acknowledged.
    #[allow(
        clippy::result_large_err,
        reason = "an incomplete request must retain its session and exact framed owner"
    )]
    pub fn try_finish(self) -> Result<AwaitingResponse, Self> {
        if self.frame.is_complete() {
            Ok(self.awaiting)
        } else {
            Err(self)
        }
    }
}

/// Authenticated client session with one request completely transmitted.
#[must_use = "the next response must be authenticated or the disconnected session dropped"]
pub struct AwaitingResponse {
    session: ClientSession,
}

impl AwaitingResponse {
    /// Authenticate one exact next response and return its message owner.
    ///
    /// Any error consumes and terminates the session. The caller must likewise
    /// drop this state on a framing-layer fault before calling this method.
    pub fn authenticate(
        mut self,
        record: Record,
    ) -> Result<AuthenticatedResponse, ClientSessionFault> {
        if record.kind() != RECORD_KIND_RESPONSE {
            return Err(ClientSessionFault::UnexpectedKind {
                observed: record.kind(),
            });
        }
        if record.session_id() != self.session.session_id.as_bytes() {
            return Err(ClientSessionFault::WrongSession);
        }
        if record.sequence() != self.session.next_server_sequence {
            return Err(ClientSessionFault::UnexpectedSequence {
                expected: self.session.next_server_sequence,
                observed: record.sequence(),
            });
        }
        if self.session.next_server_sequence == u64::MAX {
            return Err(ClientSessionFault::SequenceExhausted);
        }
        if !verify_server_record_tag(&self.session.server_record_key, &record) {
            return Err(ClientSessionFault::BadTag);
        }

        self.session.next_server_sequence += 1;
        let (_, _, _, payload_length, payload, _) = record.into_parts();
        let message_length = MessageLength::new(payload_length.get())
            .expect("framing and logical handoff share the 512-byte capacity");
        Ok(AuthenticatedResponse {
            session: self.session,
            message: OwnedMessage::new(message_length, payload),
        })
    }
}

/// Exact authenticated response owner paired with the restored idle session.
#[must_use = "the response must be consumed while retaining or closing its session"]
pub struct AuthenticatedResponse {
    session: ClientSession,
    message: OwnedMessage,
}

impl AuthenticatedResponse {
    /// Exact encoded response owner.
    pub const fn message(&self) -> &OwnedMessage {
        &self.message
    }

    /// Split into the restored idle session and exact response owner.
    pub fn into_parts(self) -> (ClientSession, OwnedMessage) {
        (self.session, self.message)
    }
}
