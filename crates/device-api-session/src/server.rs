//! Server handshake, authenticated grant, and request/reply typestates.

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api_credentials::{
    CredentialAuthority, CredentialGeneration, CredentialId, CredentialRejected, DispatchLease,
    SelectedCredential,
};
use reticulum_device_api_framing::{
    AUTH_TAG_LENGTH, FrameEncodeError, FramedRecord, PayloadLength, Record, TxAdvanceError,
};
use reticulum_device_api_handoff::{
    CorrelationId, LocalApiReply, LocalApiRequest, MessageLength, OwnedMessage, RequestKey,
    SessionEpoch,
};
use zeroize::Zeroizing;

use crate::{
    crypto::{
        KeySchedule, derive, server_proof, server_record_tag, verify_client_proof,
        verify_client_record_tag,
    },
    protocol::{
        BearerBinding, ClientHello, DeviceId, HandshakeRecordError, RECORD_KIND_CLIENT_PROOF,
        RECORD_KIND_REQUEST, RECORD_KIND_RESPONSE, RECORD_KIND_SERVER_PROOF, ServerHello,
        SessionId, SessionSuite, proof_record, take_proof,
    },
};

/// Fixed device and bearer facts for one server-side handshake attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerParameters {
    device_id: DeviceId,
    bearer: BearerBinding,
    suite: SessionSuite,
}

impl ServerParameters {
    /// Bind the supported BLE GATT handshake to one stable device ID and bearer.
    pub const fn new(device_id: DeviceId, bearer: BearerBinding) -> Self {
        Self::new_for_suite(device_id, bearer, SessionSuite::BleGatt)
    }

    /// Bind a handshake attempt to one device, bearer, and explicit suite.
    pub const fn new_for_suite(
        device_id: DeviceId,
        bearer: BearerBinding,
        suite: SessionSuite,
    ) -> Self {
        Self {
            device_id,
            bearer,
            suite,
        }
    }

    /// Stable public device API identifier.
    pub const fn device_id(self) -> DeviceId {
        self.device_id
    }

    /// Actual bearer this server instance owns.
    pub const fn bearer(self) -> BearerBinding {
        self.bearer
    }

    /// Cryptographic session suite this server requires.
    pub const fn suite(self) -> SessionSuite {
        self.suite
    }
}

/// One active device-owned credential selected before the handshake KDF.
///
/// This owner contains only authentication material. Principal and permissions
/// remain in the credential authority and are looked up again from the grant's
/// ID and generation at logical request dispatch.
pub struct ActiveCredential {
    id: CredentialId,
    generation: CredentialGeneration,
    psk: Zeroizing<[u8; 32]>,
}

impl ActiveCredential {
    /// Construct an active credential from device-owned state.
    pub fn new(id: CredentialId, generation: CredentialGeneration, psk: [u8; 32]) -> Self {
        Self {
            id,
            generation,
            psk: Zeroizing::new(psk),
        }
    }

    /// Consume one device-authority selection without exposing its PSK.
    pub fn from_selected(selected: SelectedCredential) -> Self {
        let (id, generation, psk) = selected.into_parts();
        Self {
            id,
            generation,
            psk,
        }
    }

    /// Opaque credential identifier.
    pub const fn id(&self) -> CredentialId {
        self.id
    }

    /// Authorization generation authenticated by this credential.
    pub const fn generation(&self) -> CredentialGeneration {
        self.generation
    }
}

/// Boot-lifetime source of unique local reply-routing epochs.
///
/// The local bearer manager must construct exactly one allocator after boot and
/// retain it across every reconnect. Reconstructing this allocator while a
/// node-side request or reply can still exist would violate the reply-routing
/// contract. Exhaustion is terminal rather than wrapping to an earlier epoch.
#[must_use = "the sole epoch allocator must be retained for the complete bearer-manager lifetime"]
pub struct SessionEpochAllocator {
    next: Option<u64>,
}

#[allow(
    clippy::new_without_default,
    reason = "constructing this boot-lifetime singleton should remain explicit"
)]
impl SessionEpochAllocator {
    /// Start the sole boot-lifetime allocator at epoch one.
    pub const fn new() -> Self {
        Self { next: Some(1) }
    }

    fn allocate(&mut self) -> Result<SessionEpoch, SessionEpochExhausted> {
        let value = self.next.ok_or(SessionEpochExhausted)?;
        self.next = value.checked_add(1);
        Ok(SessionEpoch::new(value))
    }
}

/// The boot-lifetime reply-routing epoch space has been exhausted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionEpochExhausted;

/// Failure before an authenticated session becomes established.
#[derive(Debug)]
pub enum HandshakeError {
    /// A handshake record was not canonical.
    Record(HandshakeRecordError),
    /// Client requested a different bearer than the server instance owns.
    BearerMismatch {
        /// Client-declared bearer.
        client: BearerBinding,
        /// Actual server bearer.
        server: BearerBinding,
    },
    /// A session suite was configured for a bearer it does not permit.
    SuiteBearerMismatch {
        /// Configured session suite.
        suite: SessionSuite,
        /// Bearer configured for the handshake attempt.
        bearer: BearerBinding,
        /// Sole bearer permitted by the configured suite.
        required: BearerBinding,
    },
    /// Client requested a different supported suite than the server owns.
    SuiteMismatch {
        /// Suite declared by the client.
        client: SessionSuite,
        /// Suite configured by the server.
        server: SessionSuite,
    },
    /// Credential selection or proof failed without revealing which fact differed.
    AuthenticationFailed,
    /// Qualified entropy failed while producing the fresh server nonce.
    Entropy(rand_core::Error),
    /// The boot-lifetime reply-routing epoch cannot advance without reuse.
    SessionEpochExhausted,
    /// A fixed handshake record unexpectedly failed framing.
    Framing,
}

impl From<HandshakeRecordError> for HandshakeError {
    fn from(error: HandshakeRecordError) -> Self {
        Self::Record(error)
    }
}

impl From<FrameEncodeError> for HandshakeError {
    fn from(_: FrameEncodeError) -> Self {
        Self::Framing
    }
}

struct GrantFacts {
    credential_id: CredentialId,
    credential_generation: CredentialGeneration,
    bearer: BearerBinding,
    epoch: SessionEpoch,
}

/// First exact outbound handshake record plus retained proof/session state.
///
/// The caller must completely acknowledge this framed server hello before
/// [`Self::try_finish`] yields the server-proof flight.
#[must_use = "a server hello flight must be fully transmitted or explicitly dropped"]
pub struct ServerHelloFlight {
    pending: PendingClientProof,
    server_proof: FramedRecord,
    frame: FramedRecord,
}

impl ServerHelloFlight {
    /// Start one session handshake from a parsed client hello.
    ///
    /// Unknown or revoked credential IDs should be rejected by the external
    /// credential authority before constructing `credential`. A mismatch at
    /// this boundary returns the same generic authentication failure as a bad
    /// proof. Bearer managers must apply handshake timeout and attempt-rate
    /// policy around this operation and retain the same boot-lifetime `epochs`
    /// owner across every reconnect.
    pub fn begin<R>(
        client_hello: ClientHello,
        credential: ActiveCredential,
        parameters: ServerParameters,
        epochs: &mut SessionEpochAllocator,
        rng: &mut R,
    ) -> Result<Self, HandshakeError>
    where
        R: RngCore + CryptoRng,
    {
        let required_bearer = parameters.suite.required_bearer();
        if parameters.bearer != required_bearer {
            return Err(HandshakeError::SuiteBearerMismatch {
                suite: parameters.suite,
                bearer: parameters.bearer,
                required: required_bearer,
            });
        }
        if client_hello.suite() != parameters.suite {
            return Err(HandshakeError::SuiteMismatch {
                client: client_hello.suite(),
                server: parameters.suite,
            });
        }
        if client_hello.bearer() != parameters.bearer {
            return Err(HandshakeError::BearerMismatch {
                client: client_hello.bearer(),
                server: parameters.bearer,
            });
        }
        if client_hello.credential_id() != credential.id {
            return Err(HandshakeError::AuthenticationFailed);
        }

        let mut server_nonce = [0_u8; 32];
        rng.try_fill_bytes(&mut server_nonce)
            .map_err(HandshakeError::Entropy)?;
        let server_hello = ServerHello::new(
            parameters.suite,
            parameters.bearer,
            parameters.device_id,
            server_nonce,
            credential.generation,
        );
        let schedule = derive(&credential.psk, &client_hello, &server_hello);
        let proof = server_proof(&schedule);
        let hello_frame = FramedRecord::encode(&server_hello.into_record())?;
        let proof_frame = FramedRecord::encode(&proof_record(
            RECORD_KIND_SERVER_PROOF,
            schedule.session_id,
            &proof,
        ))?;
        let epoch = epochs
            .allocate()
            .map_err(|_| HandshakeError::SessionEpochExhausted)?;
        let facts = GrantFacts {
            credential_id: credential.id,
            credential_generation: credential.generation,
            bearer: parameters.bearer,
            epoch,
        };
        Ok(Self {
            pending: PendingClientProof {
                schedule,
                server_proof: proof,
                facts,
            },
            server_proof: proof_frame,
            frame: hello_frame,
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

    /// Finish only after every server-hello byte has been acknowledged.
    #[allow(
        clippy::result_large_err,
        reason = "an incomplete flight must return its exact fixed-capacity owner"
    )]
    pub fn try_finish(self) -> Result<ServerProofFlight, Self> {
        if self.frame.is_complete() {
            Ok(ServerProofFlight {
                pending: self.pending,
                frame: self.server_proof,
            })
        } else {
            Err(self)
        }
    }
}

/// Exact outbound server-proof record plus retained pending session state.
#[must_use = "a server proof flight must be fully transmitted or explicitly dropped"]
pub struct ServerProofFlight {
    pending: PendingClientProof,
    frame: FramedRecord,
}

impl ServerProofFlight {
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

    /// Finish only after every server-proof byte has been acknowledged.
    #[allow(
        clippy::result_large_err,
        reason = "an incomplete flight must return its exact fixed-capacity owner"
    )]
    pub fn try_finish(self) -> Result<PendingClientProof, Self> {
        if self.frame.is_complete() {
            Ok(self.pending)
        } else {
            Err(self)
        }
    }
}

/// Retained server handshake waiting for the client's full proof.
#[must_use = "pending handshake key material must be authenticated or explicitly dropped"]
pub struct PendingClientProof {
    schedule: KeySchedule,
    server_proof: [u8; 32],
    facts: GrantFacts,
}

impl PendingClientProof {
    /// Verify one exact client-proof record and establish the session.
    pub fn authenticate(self, record: Record) -> Result<ServerSession, HandshakeError> {
        let proof = take_proof(record, RECORD_KIND_CLIENT_PROOF, self.schedule.session_id)?;
        if !verify_client_proof(&self.schedule, &self.server_proof, &proof) {
            return Err(HandshakeError::AuthenticationFailed);
        }

        let KeySchedule {
            transcript_hash: _,
            server_proof_key: _,
            client_proof_key: _,
            client_record_key,
            server_record_key,
            session_id,
        } = self.schedule;
        Ok(ServerSession {
            session_id,
            client_record_key,
            server_record_key,
            next_client_sequence: 0,
            next_server_sequence: 0,
            facts: self.facts,
        })
    }

    /// Session ID the client proof record must carry.
    pub const fn session_id(&self) -> SessionId {
        self.schedule.session_id
    }
}

/// Opaque device-minted authorization reference carried to node dispatch.
///
/// This type deliberately implements neither `Clone`, `Copy`, nor `Debug`, and
/// has no public constructor. It contains no PSK, principal, or permissions.
/// The node uses credential ID and generation to obtain fresh device-owned
/// authorization facts immediately before logical dispatch.
pub struct AuthenticatedGrant {
    credential_id: CredentialId,
    credential_generation: CredentialGeneration,
    bearer: BearerBinding,
    session_id: SessionId,
    epoch: SessionEpoch,
    admission_sequence: u64,
}

impl AuthenticatedGrant {
    /// Credential record that authenticated this request.
    pub const fn credential_id(&self) -> CredentialId {
        self.credential_id
    }

    /// Credential generation that must still be active at dispatch.
    pub const fn credential_generation(&self) -> CredentialGeneration {
        self.credential_generation
    }

    /// Local bearer on which authentication completed.
    pub const fn bearer(&self) -> BearerBinding {
        self.bearer
    }

    /// Handshake-derived session identifier.
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Boot-local session epoch used for reply routing.
    pub const fn epoch(&self) -> SessionEpoch {
        self.epoch
    }

    /// Authenticated client-to-device record sequence that admitted the request.
    pub const fn admission_sequence(&self) -> u64 {
        self.admission_sequence
    }

    /// Revalidate this grant against current device-owned credential state.
    ///
    /// The returned lease borrows `authority`. Product ownership requires its
    /// callback to contain the immediate synchronous logical dispatch; the type
    /// freezes the borrowed authority but does not prevent holding the lease
    /// across an await. Failure must not be downgraded to an unauthenticated
    /// context.
    pub fn revalidate<'authority, const CAPACITY: usize>(
        &self,
        authority: &'authority CredentialAuthority<CAPACITY>,
    ) -> Result<DispatchLease<'authority, CAPACITY>, CredentialRejected> {
        authority.revalidate(self.credential_id, self.credential_generation)
    }
}

/// Fatal established-session authentication or ordering failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionFault {
    /// Session received a record kind that is not a logical request.
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

/// Idle authenticated server session able to accept exactly one request.
#[must_use = "an authenticated session must be driven, disconnected, or explicitly dropped"]
pub struct ServerSession {
    session_id: SessionId,
    client_record_key: Zeroizing<[u8; 32]>,
    server_record_key: Zeroizing<[u8; 32]>,
    next_client_sequence: u64,
    next_server_sequence: u64,
    facts: GrantFacts,
}

impl ServerSession {
    /// Authenticate one exact next request and enter the awaiting-reply state.
    ///
    /// Any error consumes and terminates the session. The caller must likewise
    /// drop this session on a framing-layer fault before calling this method.
    pub fn authenticate_request(
        mut self,
        record: Record,
    ) -> Result<AuthenticatedRequest, SessionFault> {
        if record.kind() != RECORD_KIND_REQUEST {
            return Err(SessionFault::UnexpectedKind {
                observed: record.kind(),
            });
        }
        if record.session_id() != self.session_id.as_bytes() {
            return Err(SessionFault::WrongSession);
        }
        if record.sequence() != self.next_client_sequence {
            return Err(SessionFault::UnexpectedSequence {
                expected: self.next_client_sequence,
                observed: record.sequence(),
            });
        }
        if self.next_client_sequence == u64::MAX {
            return Err(SessionFault::SequenceExhausted);
        }
        if !verify_client_record_tag(&self.client_record_key, &record) {
            return Err(SessionFault::BadTag);
        }

        let sequence = self.next_client_sequence;
        self.next_client_sequence += 1;
        let (_, _, _, payload_length, payload, _) = record.into_parts();
        let message_length = MessageLength::new(payload_length.get())
            .expect("framing and logical handoff share the 512-byte capacity");
        let key = RequestKey::new(self.facts.epoch, CorrelationId::new(sequence));
        let grant = AuthenticatedGrant {
            credential_id: self.facts.credential_id,
            credential_generation: self.facts.credential_generation,
            bearer: self.facts.bearer,
            session_id: self.session_id,
            epoch: self.facts.epoch,
            admission_sequence: sequence,
        };
        let request = LocalApiRequest::new(key, grant, OwnedMessage::new(message_length, payload));
        Ok(AuthenticatedRequest {
            request,
            waiting: AwaitingReply { session: self, key },
        })
    }

    /// Handshake-derived session identifier.
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Boot-local epoch used by the handoff reply route.
    pub const fn epoch(&self) -> SessionEpoch {
        self.facts.epoch
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

/// Exact authenticated request owner paired with the sole waiting session.
#[must_use = "the request must be handed off while retaining its waiting session"]
pub struct AuthenticatedRequest {
    request: LocalApiRequest<AuthenticatedGrant>,
    waiting: AwaitingReply,
}

impl AuthenticatedRequest {
    /// Split into the exact handoff request and the session waiting for its reply.
    pub fn into_parts(self) -> (LocalApiRequest<AuthenticatedGrant>, AwaitingReply) {
        (self.request, self.waiting)
    }
}

/// Authenticated session with one request already admitted.
#[must_use = "the matching reply must be sealed or the disconnected session dropped"]
pub struct AwaitingReply {
    session: ServerSession,
    key: RequestKey,
}

impl AwaitingReply {
    /// Exact handoff key required from the node reply.
    pub const fn request_key(&self) -> RequestKey {
        self.key
    }

    /// Authenticate and frame the matching reply, reserving its TX sequence.
    ///
    /// A routing mismatch or sequence exhaustion consumes and terminates the
    /// session but returns the exact reply owner for explicit draining.
    #[allow(clippy::result_large_err)]
    pub fn frame_reply(mut self, reply: LocalApiReply) -> Result<ReplyFlight, ReplyRouteFault> {
        if !reply.matches(self.key) {
            let kind = if reply.key().epoch() != self.key.epoch() {
                ReplyRouteFaultKind::WrongEpoch
            } else {
                ReplyRouteFaultKind::WrongCorrelation
            };
            return Err(ReplyRouteFault { kind, reply });
        }
        if self.session.next_server_sequence == u64::MAX {
            return Err(ReplyRouteFault {
                kind: ReplyRouteFaultKind::SequenceExhausted,
                reply,
            });
        }

        let (_, message) = reply.into_parts();
        let (message_length, payload) = message.into_parts();
        let sequence = self.session.next_server_sequence;
        let untagged = Record::new(
            RECORD_KIND_RESPONSE,
            self.session.session_id.0,
            sequence,
            PayloadLength::new(message_length.get())
                .expect("logical handoff and framing share the 512-byte capacity"),
            payload,
            [0_u8; AUTH_TAG_LENGTH],
        );
        let tag = server_record_tag(&self.session.server_record_key, &untagged);
        let (kind, session_id, sequence, length, payload, _) = untagged.into_parts();
        let tagged = Record::new(kind, session_id, sequence, length, payload, tag);
        let frame = match FramedRecord::encode(&tagged) {
            Ok(frame) => frame,
            Err(_) => {
                let (_, _, _, length, payload, _) = tagged.into_parts();
                let message = OwnedMessage::new(
                    MessageLength::new(length.get())
                        .expect("framing and logical handoff share one capacity"),
                    payload,
                );
                return Err(ReplyRouteFault {
                    kind: ReplyRouteFaultKind::Framing,
                    reply: LocalApiReply::new(self.key, message),
                });
            }
        };
        self.session.next_server_sequence += 1;
        Ok(ReplyFlight {
            session: self.session,
            frame,
        })
    }
}

/// Category for a reply that could not be routed into the live session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyRouteFaultKind {
    /// Reply belongs to a stale or foreign session epoch.
    WrongEpoch,
    /// Reply epoch matches but correlation does not.
    WrongCorrelation,
    /// Device-to-client record sequence cannot advance without wrapping.
    SequenceExhausted,
    /// Canonical response unexpectedly failed fixed-capacity framing.
    Framing,
}

/// Fatal reply-routing failure retaining the exact undelivered reply owner.
#[must_use = "the undelivered reply must be drained, retained, or explicitly dropped"]
pub struct ReplyRouteFault {
    kind: ReplyRouteFaultKind,
    reply: LocalApiReply,
}

impl ReplyRouteFault {
    /// Failure category.
    pub const fn kind(&self) -> ReplyRouteFaultKind {
        self.kind
    }

    /// Recover the complete undelivered reply owner.
    pub fn into_reply(self) -> LocalApiReply {
        self.reply
    }
}

/// Exact authenticated response bytes with an acknowledgement cursor.
///
/// The session and its already-reserved TX sequence remain inside this owner.
/// A backend write future with uncertain cancellation semantics must be driven
/// to completion before calling [`Self::try_finish`]. Dropping the flight
/// terminates the session and therefore cannot reuse the reserved sequence.
#[must_use = "a reply flight must be completely acknowledged or explicitly dropped"]
pub struct ReplyFlight {
    session: ServerSession,
    frame: FramedRecord,
}

impl ReplyFlight {
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

    /// Return to idle only after every response byte has been acknowledged.
    #[allow(
        clippy::result_large_err,
        reason = "an incomplete reply must retain its session and exact framed owner"
    )]
    pub fn try_finish(self) -> Result<ServerSession, Self> {
        if self.frame.is_complete() {
            Ok(self.session)
        } else {
            Err(self)
        }
    }
}

#[cfg(test)]
pub(crate) fn client_tag_for_test(key: &[u8; 32], record: &Record) -> [u8; AUTH_TAG_LENGTH] {
    crate::crypto::client_record_tag(key, record)
}

#[cfg(test)]
mod epoch_tests {
    use super::{SessionEpochAllocator, SessionEpochExhausted};

    #[test]
    fn allocator_uses_the_final_epoch_once_and_never_wraps() {
        let mut epochs = SessionEpochAllocator {
            next: Some(u64::MAX),
        };
        assert_eq!(epochs.allocate().unwrap().get(), u64::MAX);
        assert_eq!(epochs.allocate(), Err(SessionEpochExhausted));
        assert_eq!(epochs.allocate(), Err(SessionEpochExhausted));
    }
}
