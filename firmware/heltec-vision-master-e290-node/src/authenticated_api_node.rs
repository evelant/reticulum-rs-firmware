//! Node-side authenticated device-API dispatch without bearer capabilities.
//!
//! The bearer authenticates one record and transfers its exact opaque grant and
//! bounded logical message through `reticulum-device-api-handoff`. This module
//! revalidates that grant against the current immutable device-owned authority,
//! invokes the logical adapter only inside the authority lease callback, and
//! returns one reply with the unchanged routing key. It has no USB, executor,
//! radio, raw-flash, or session-key access.

use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, DeviceResponse, IdentitySummary, ResponseEnvelope,
    decode_request, encode_response,
};
use reticulum_device_api_adapter::{
    InboundMailboxPort, LxmfComposePort, LxmfInboxPort, NomadFetchPort, PeerDiscoveryPort,
    SubmissionPort, dispatch, dispatch_with_inbox,
    dispatch_with_inbox_lxmf_peer_discovery_and_nomad,
};
use reticulum_device_api_credentials::CredentialAuthority;
use reticulum_device_api_handoff::{
    LocalApiReply, LocalApiRequest, MESSAGE_CAPACITY, MessageLength, OwnedMessage,
};
use reticulum_device_api_session::AuthenticatedGrant;

/// Terminal reason an authenticated request could not yield a canonical reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticatedApiDispatchFailureKind {
    /// The authenticated payload was not one canonical logical API request.
    MalformedRequest,
    /// A bounded logical response unexpectedly failed canonical encoding.
    ResponseEncoding,
}

/// Terminal dispatch failure retaining the exact request owner.
///
/// A response-encoding failure may occur after the port has already performed
/// its synchronous operation. The retained request is diagnostic/quarantine
/// ownership and must never be dispatched again.
#[must_use = "a terminal authenticated request must be quarantined or explicitly dropped"]
pub struct AuthenticatedApiDispatchFailure {
    kind: AuthenticatedApiDispatchFailureKind,
    request: LocalApiRequest<AuthenticatedGrant>,
}

impl AuthenticatedApiDispatchFailure {
    /// Stable terminal failure category.
    pub const fn kind(&self) -> AuthenticatedApiDispatchFailureKind {
        self.kind
    }

    /// Recover the complete request, opaque grant, and routing owner.
    pub fn into_request(self) -> LocalApiRequest<AuthenticatedGrant> {
        self.request
    }
}

/// Revalidate and synchronously dispatch one authenticated logical API request.
///
/// `identity` is a copy-only public node summary supplied independently of the
/// submission port; serving it cannot acquire storage or radio capabilities.
/// Missing, replaced, revoked, or generation-mismatched credential state emits
/// a generic `AuthenticationRequired` response without invoking `port`. It is
/// never retried through [`reticulum_device_api::DispatchContext::UNAUTHENTICATED`].
/// The bearer may treat that response as terminal for its live session.
#[allow(
    clippy::result_large_err,
    reason = "terminal failure must retain the exact allocation-free request owner"
)]
pub fn dispatch_authenticated_request<const CREDENTIALS: usize, P>(
    request: LocalApiRequest<AuthenticatedGrant>,
    authority: Option<&CredentialAuthority<CREDENTIALS>>,
    identity: IdentitySummary,
    port: &mut P,
) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
where
    P: SubmissionPort,
{
    let envelope = match decode_request(request.message().encoded()) {
        Ok(envelope) => envelope,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::MalformedRequest,
                request,
            });
        }
    };
    let operation = envelope.request.operation();
    let response = match authority.and_then(|authority| request.grant().revalidate(authority).ok())
    {
        Some(lease) => {
            lease.with_dispatch_context(|context| dispatch(port, identity, context, envelope))
        }
        None => ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: envelope.request_id,
            response: DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::AuthenticationRequired,
                operation: Some(operation),
            }),
        },
    };

    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = match encode_response(&response, &mut encoded) {
        Ok(length) => length,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::ResponseEncoding,
                request,
            });
        }
    };
    let key = request.key();
    drop(request);
    Ok(LocalApiReply::new(
        key,
        OwnedMessage::new(
            MessageLength::new(length).expect("device API and handoff share one capacity"),
            encoded,
        ),
    ))
}

/// Revalidate and dispatch one authenticated request with an independent inbox port.
///
/// This is the permanent node's API 1.2 path. Credential revalidation and
/// response ownership are identical to [`dispatch_authenticated_request`], but
/// inbox operations receive only their narrow semantic port. Public identity
/// reads still invoke neither storage port.
#[allow(
    clippy::result_large_err,
    reason = "terminal failure must retain the exact allocation-free request owner"
)]
pub fn dispatch_authenticated_request_with_inbox<const CREDENTIALS: usize, P, M>(
    request: LocalApiRequest<AuthenticatedGrant>,
    authority: Option<&CredentialAuthority<CREDENTIALS>>,
    identity: IdentitySummary,
    submission_port: &mut P,
    inbox_port: &mut M,
) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
where
    P: SubmissionPort,
    M: InboundMailboxPort,
{
    let envelope = match decode_request(request.message().encoded()) {
        Ok(envelope) => envelope,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::MalformedRequest,
                request,
            });
        }
    };
    let operation = envelope.request.operation();
    let response = match authority.and_then(|authority| request.grant().revalidate(authority).ok())
    {
        Some(lease) => lease.with_dispatch_context(|context| {
            dispatch_with_inbox(submission_port, inbox_port, identity, context, envelope)
        }),
        None => ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: envelope.request_id,
            response: DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::AuthenticationRequired,
                operation: Some(operation),
            }),
        },
    };

    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = match encode_response(&response, &mut encoded) {
        Ok(length) => length,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::ResponseEncoding,
                request,
            });
        }
    };
    let key = request.key();
    drop(request);
    Ok(LocalApiReply::new(
        key,
        OwnedMessage::new(
            MessageLength::new(length).expect("device API and handoff share one capacity"),
            encoded,
        ),
    ))
}

/// Revalidate and dispatch one authenticated request through the complete
/// appliance and bounded NomadNet semantic ports.
///
/// The combined appliance port permits operation-scoped use of one physical
/// flash owner without creating simultaneous mutable aliases. The independent
/// Nomad port borrows only volatile protocol/API state. Authentication failure
/// invokes neither port.
#[allow(
    clippy::result_large_err,
    reason = "terminal failure must retain the exact allocation-free request owner"
)]
pub fn dispatch_authenticated_request_with_inbox_lxmf_and_nomad<const CREDENTIALS: usize, P, N>(
    request: LocalApiRequest<AuthenticatedGrant>,
    authority: Option<&CredentialAuthority<CREDENTIALS>>,
    identity: IdentitySummary,
    port: &mut P,
    nomad_port: &mut N,
) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
where
    P: SubmissionPort + InboundMailboxPort + LxmfInboxPort + LxmfComposePort + PeerDiscoveryPort,
    N: NomadFetchPort,
{
    let envelope = match decode_request(request.message().encoded()) {
        Ok(envelope) => envelope,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::MalformedRequest,
                request,
            });
        }
    };
    let operation = envelope.request.operation();
    let response = match authority.and_then(|authority| request.grant().revalidate(authority).ok())
    {
        Some(lease) => lease.with_dispatch_context(|context| {
            dispatch_with_inbox_lxmf_peer_discovery_and_nomad(
                port, nomad_port, identity, context, envelope,
            )
        }),
        None => ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: envelope.request_id,
            response: DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::AuthenticationRequired,
                operation: Some(operation),
            }),
        },
    };

    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = match encode_response(&response, &mut encoded) {
        Ok(length) => length,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::ResponseEncoding,
                request,
            });
        }
    };
    let key = request.key();
    drop(request);
    Ok(LocalApiReply::new(
        key,
        OwnedMessage::new(
            MessageLength::new(length).expect("device API and handoff share one capacity"),
            encoded,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use rand_core::{CryptoRng, RngCore};
    use reticulum_device_api::{
        ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilityAvailability, CapabilitySnapshot,
        DestinationHash, DeviceRequest, DeviceResponse, IdempotencyKey, IdentitySummary,
        NomadFetchId, NomadFetchPhase, NomadFetchPollRequest, NomadFetchPollResponse,
        NomadFetchStartAccepted, NomadFetchStartOutcome, NomadFetchStartRequest, NomadPagePath,
        NomadRequestTimestampUnixMs, OP_EXPERIMENTAL_RNS_INBOX_STATUS, OP_SYSTEM_CAPABILITIES,
        Permissions, PrincipalId, RequestEnvelope, RequestId, ResponseEnvelope, RnsInboxStatus,
        decode_response, encode_request,
    };
    use reticulum_device_api_adapter::{
        InboundMailboxItem, InboundMailboxPort, InboundMailboxPortError, LxmfComposeAcceptance,
        LxmfComposePort, LxmfComposePortError, LxmfComposeRequest, LxmfInboxPort,
        LxmfInboxPortError, NomadFetchPort, NomadFetchPortError, NomadFetchStartDisposition,
        PeerDiscoveryPort, PeerDiscoveryPortError, SubmissionAcceptance, SubmissionPort,
        SubmissionPortError,
    };
    use reticulum_device_api_credentials::{
        AuthorityRevision, AuthorizationPolicyVersion, CredentialAudit, CredentialAuthority,
        CredentialAuthorityBuilder, CredentialGeneration, CredentialId, CredentialRecord,
        CredentialStatus, PairingOrigin, RevocationReason,
    };
    use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder};
    use reticulum_device_api_handoff::{
        CorrelationId, LocalApiRequest, MESSAGE_CAPACITY, MessageLength, OwnedMessage, RequestKey,
        SessionEpoch,
    };
    use reticulum_device_api_session::{
        ActiveCredential, AuthenticatedGrant, BearerBinding, ClientCredential, ClientHello,
        ClientHelloFlight, ClientParameters, DeviceId, ServerHelloFlight, ServerParameters,
        SessionEpochAllocator,
    };
    use reticulum_storage_model::{
        AcceptanceCandidate, LifecycleState, PrincipalId as StoragePrincipalId,
        SubmissionId as StorageSubmissionId,
    };

    use super::{
        AuthenticatedApiDispatchFailureKind, dispatch_authenticated_request,
        dispatch_authenticated_request_with_inbox,
        dispatch_authenticated_request_with_inbox_lxmf_and_nomad,
    };

    const CREDENTIAL_ID: CredentialId = CredentialId::new([
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ]);
    const GENERATION: CredentialGeneration = CredentialGeneration::new(7);
    const DEVICE_ID: DeviceId = DeviceId::new([
        0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
        0x2f,
    ]);
    const PRINCIPAL: PrincipalId = PrincipalId([0x31; 16]);
    const PSK: [u8; 32] = [0x53; 32];
    const CLIENT_NONCE: [u8; 32] = [0x64; 32];
    const SERVER_NONCE: [u8; 32] = [0x75; 32];
    const EXPECTED_KEY: RequestKey = RequestKey::new(SessionEpoch::new(1), CorrelationId::new(0));
    const IDENTITY_SUMMARY: IdentitySummary = IdentitySummary::new(DestinationHash([0x91; 16]));
    const INBOX_STATUS: RnsInboxStatus = RnsInboxStatus {
        depth: 1,
        capacity: 1,
        dropped_since_boot: 9,
        max_payload_bytes: 383,
        durable: true,
    };
    const NOMAD_DESTINATION: DestinationHash = DestinationHash([0xa1; 16]);
    const NOMAD_IDEMPOTENCY_KEY: IdempotencyKey = IdempotencyKey([0xb2; 16]);
    const NOMAD_PATH: &str = "/page/index.mu";
    const NOMAD_TIMESTAMP_UNIX_MS: u64 = 1_784_732_100_123;
    const NOMAD_PEER_APP_DATA_BYTES: u16 = 64;

    struct FixedRng {
        bytes: [u8; 32],
        offset: usize,
    }

    impl FixedRng {
        const fn new(bytes: [u8; 32]) -> Self {
            Self { bytes, offset: 0 }
        }
    }

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0_u8; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0_u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for byte in destination {
                *byte = self.bytes[self.offset % self.bytes.len()];
                self.offset += 1;
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for FixedRng {}

    #[derive(Default)]
    struct CountingPort {
        availability_calls: usize,
        status_calls: usize,
        acceptance_calls: usize,
    }

    impl CountingPort {
        const fn total_calls(&self) -> usize {
            self.availability_calls + self.status_calls + self.acceptance_calls
        }
    }

    impl SubmissionPort for CountingPort {
        fn availability(&mut self) -> CapabilityAvailability {
            self.availability_calls += 1;
            CapabilityAvailability::Available
        }

        fn submission_state(
            &mut self,
            _principal: StoragePrincipalId,
            _id: StorageSubmissionId,
        ) -> Result<Option<LifecycleState>, SubmissionPortError> {
            self.status_calls += 1;
            Ok(None)
        }

        fn accept(
            &mut self,
            _candidate: AcceptanceCandidate,
        ) -> Result<SubmissionAcceptance, SubmissionPortError> {
            self.acceptance_calls += 1;
            Ok(SubmissionAcceptance::CapacityExhausted)
        }
    }

    #[derive(Default)]
    struct CountingInboxPort {
        availability_calls: usize,
        status_calls: usize,
        peek_calls: usize,
    }

    impl CountingInboxPort {
        const fn total_calls(&self) -> usize {
            self.availability_calls + self.status_calls + self.peek_calls
        }
    }

    impl InboundMailboxPort for CountingInboxPort {
        fn availability(&mut self) -> CapabilityAvailability {
            self.availability_calls += 1;
            CapabilityAvailability::Available
        }

        fn status(&mut self) -> Result<RnsInboxStatus, InboundMailboxPortError> {
            self.status_calls += 1;
            Ok(INBOX_STATUS)
        }

        fn peek(&mut self) -> Result<Option<InboundMailboxItem>, InboundMailboxPortError> {
            self.peek_calls += 1;
            Ok(None)
        }
    }

    #[derive(Default)]
    struct CountingCompletePort {
        submission_availability_calls: usize,
        submission_status_calls: usize,
        submission_accept_calls: usize,
        inbox_availability_calls: usize,
        inbox_status_calls: usize,
        inbox_peek_calls: usize,
        lxmf_availability_calls: usize,
        lxmf_next_calls: usize,
        lxmf_read_calls: usize,
        compose_availability_calls: usize,
        compose_calls: usize,
        peer_availability_calls: usize,
        peer_max_app_data_calls: usize,
        peer_next_calls: usize,
    }

    impl CountingCompletePort {
        const fn total_calls(&self) -> usize {
            self.submission_availability_calls
                + self.submission_status_calls
                + self.submission_accept_calls
                + self.inbox_availability_calls
                + self.inbox_status_calls
                + self.inbox_peek_calls
                + self.lxmf_availability_calls
                + self.lxmf_next_calls
                + self.lxmf_read_calls
                + self.compose_availability_calls
                + self.compose_calls
                + self.peer_availability_calls
                + self.peer_max_app_data_calls
                + self.peer_next_calls
        }
    }

    impl SubmissionPort for CountingCompletePort {
        fn availability(&mut self) -> CapabilityAvailability {
            self.submission_availability_calls += 1;
            CapabilityAvailability::Available
        }

        fn submission_state(
            &mut self,
            _principal: StoragePrincipalId,
            _id: StorageSubmissionId,
        ) -> Result<Option<LifecycleState>, SubmissionPortError> {
            self.submission_status_calls += 1;
            Ok(None)
        }

        fn accept(
            &mut self,
            _candidate: AcceptanceCandidate,
        ) -> Result<SubmissionAcceptance, SubmissionPortError> {
            self.submission_accept_calls += 1;
            Ok(SubmissionAcceptance::CapacityExhausted)
        }
    }

    impl InboundMailboxPort for CountingCompletePort {
        fn availability(&mut self) -> CapabilityAvailability {
            self.inbox_availability_calls += 1;
            CapabilityAvailability::Available
        }

        fn status(&mut self) -> Result<RnsInboxStatus, InboundMailboxPortError> {
            self.inbox_status_calls += 1;
            Ok(INBOX_STATUS)
        }

        fn peek(&mut self) -> Result<Option<InboundMailboxItem>, InboundMailboxPortError> {
            self.inbox_peek_calls += 1;
            Ok(None)
        }
    }

    impl LxmfInboxPort for CountingCompletePort {
        fn availability(&mut self) -> CapabilityAvailability {
            self.lxmf_availability_calls += 1;
            CapabilityAvailability::Available
        }

        fn next(
            &mut self,
            _after: Option<reticulum_device_api::LxmfMessageHandle>,
        ) -> Result<Option<reticulum_device_api::LxmfMessageSummary>, LxmfInboxPortError> {
            self.lxmf_next_calls += 1;
            Ok(None)
        }

        fn read(
            &mut self,
            _handle: reticulum_device_api::LxmfMessageHandle,
            _offset: u32,
            _max_bytes: reticulum_device_api::LxmfReadLength,
        ) -> Result<Option<reticulum_device_api::LxmfReadChunk>, LxmfInboxPortError> {
            self.lxmf_read_calls += 1;
            Ok(None)
        }
    }

    impl LxmfComposePort for CountingCompletePort {
        fn availability(&mut self) -> CapabilityAvailability {
            self.compose_availability_calls += 1;
            CapabilityAvailability::Available
        }

        fn compose_and_accept(
            &mut self,
            _request: LxmfComposeRequest<'_>,
        ) -> Result<LxmfComposeAcceptance, LxmfComposePortError> {
            self.compose_calls += 1;
            Err(LxmfComposePortError::Unavailable)
        }
    }

    impl PeerDiscoveryPort for CountingCompletePort {
        fn availability(&mut self) -> CapabilityAvailability {
            self.peer_availability_calls += 1;
            CapabilityAvailability::Available
        }

        fn max_app_data_bytes(&mut self) -> u16 {
            self.peer_max_app_data_calls += 1;
            NOMAD_PEER_APP_DATA_BYTES
        }

        fn next(
            &mut self,
            _after: Option<reticulum_device_api::LxmfPeerDiscoveryCursor>,
        ) -> Result<reticulum_device_api::LxmfPeerDiscoveryPage, PeerDiscoveryPortError> {
            self.peer_next_calls += 1;
            Err(PeerDiscoveryPortError::Unavailable)
        }
    }

    struct CountingNomadPort {
        availability_calls: usize,
        start_calls: usize,
        poll_calls: usize,
        start_request_matches: bool,
        observed_start_principal: Option<PrincipalId>,
        observed_poll: Option<(PrincipalId, NomadFetchId)>,
    }

    impl CountingNomadPort {
        const fn new() -> Self {
            Self {
                availability_calls: 0,
                start_calls: 0,
                poll_calls: 0,
                start_request_matches: false,
                observed_start_principal: None,
                observed_poll: None,
            }
        }

        const fn total_calls(&self) -> usize {
            self.availability_calls + self.start_calls + self.poll_calls
        }
    }

    impl NomadFetchPort for CountingNomadPort {
        fn availability(&mut self) -> CapabilityAvailability {
            self.availability_calls += 1;
            CapabilityAvailability::Available
        }

        fn start(
            &mut self,
            principal: PrincipalId,
            request: NomadFetchStartRequest<'_>,
        ) -> Result<NomadFetchStartDisposition, NomadFetchPortError> {
            self.start_calls += 1;
            self.observed_start_principal = Some(principal);
            self.start_request_matches = request.destination() == NOMAD_DESTINATION
                && request.path().as_str() == NOMAD_PATH
                && request.timestamp_unix_ms().get() == NOMAD_TIMESTAMP_UNIX_MS
                && request.idempotency_key() == NOMAD_IDEMPOTENCY_KEY;
            Ok(NomadFetchStartDisposition::Accepted(nomad_fetch_id()))
        }

        fn poll(
            &mut self,
            principal: PrincipalId,
            id: NomadFetchId,
        ) -> Result<Option<NomadFetchPollResponse>, NomadFetchPortError> {
            self.poll_calls += 1;
            self.observed_poll = Some((principal, id));
            Ok(Some(NomadFetchPollResponse::Pending(
                NomadFetchPhase::AwaitingResponse,
            )))
        }
    }

    fn decode_one(bytes: &[u8]) -> Record {
        let mut decoder = StreamDecoder::new();
        for byte in bytes {
            match decoder.push(*byte) {
                DecodeEvent::Pending => {}
                DecodeEvent::Record(record) => return record,
                DecodeEvent::MalformedCobs
                | DecodeEvent::MalformedRecord(_)
                | DecodeEvent::Overflow => panic!("handshake emitted malformed framing"),
            }
        }
        panic!("flight did not contain one complete record")
    }

    fn owned_message(payload: &[u8]) -> OwnedMessage {
        let mut bytes = [0_u8; MESSAGE_CAPACITY];
        bytes[..payload.len()].copy_from_slice(payload);
        OwnedMessage::new(
            MessageLength::new(payload.len()).expect("test payload fits the logical capacity"),
            bytes,
        )
    }

    fn active_authority(generation: CredentialGeneration) -> CredentialAuthority<1> {
        let revision = AuthorityRevision::new(generation.get());
        let builder = CredentialAuthorityBuilder::new(revision)
            .unwrap_or_else(|fault| panic!("authority revision rejected: {:?}", fault.kind()));
        builder
            .insert(CredentialRecord::with_secret(
                CREDENTIAL_ID,
                generation,
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Active,
                CredentialAudit::new(
                    revision,
                    revision,
                    PairingOrigin::UsbPhysicalPresence,
                    AuthorizationPolicyVersion::new(1),
                ),
                PSK,
            ))
            .unwrap_or_else(|fault| panic!("active credential rejected: {:?}", fault.kind()))
            .finish()
    }

    fn revoked_authority() -> CredentialAuthority<1> {
        let revision = AuthorityRevision::new(GENERATION.get());
        let builder = CredentialAuthorityBuilder::new(revision)
            .unwrap_or_else(|fault| panic!("authority revision rejected: {:?}", fault.kind()));
        builder
            .insert(CredentialRecord::revoked(
                CREDENTIAL_ID,
                GENERATION,
                PRINCIPAL,
                CredentialAudit::new(
                    revision,
                    revision,
                    PairingOrigin::UsbPhysicalPresence,
                    AuthorizationPolicyVersion::new(1),
                ),
                RevocationReason::Explicit,
            ))
            .unwrap_or_else(|fault| panic!("revoked credential rejected: {:?}", fault.kind()))
            .finish()
    }

    fn authenticated_request(message: OwnedMessage) -> LocalApiRequest<AuthenticatedGrant> {
        let authority = active_authority(GENERATION);
        let selected = authority
            .select_for_handshake(CREDENTIAL_ID)
            .unwrap_or_else(|_| panic!("active credential unavailable for handshake"));

        let mut client_rng = FixedRng::new(CLIENT_NONCE);
        let mut client_hello_flight = ClientHelloFlight::begin(
            ClientParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
            ClientCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            &mut client_rng,
        )
        .unwrap_or_else(|error| panic!("client handshake begin failed: {error:?}"));
        let client_hello = ClientHello::from_record(decode_one(client_hello_flight.remaining()))
            .unwrap_or_else(|error| panic!("client hello rejected: {error:?}"));
        let client_hello_length = client_hello_flight.remaining().len();
        client_hello_flight.advance(client_hello_length).unwrap();
        let awaiting_server_hello = client_hello_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete client hello retained its flight"));

        let mut epochs = SessionEpochAllocator::new();
        let mut server_rng = FixedRng::new(SERVER_NONCE);
        let mut server_hello_flight = ServerHelloFlight::begin(
            client_hello,
            ActiveCredential::from_selected(selected),
            ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
            &mut epochs,
            &mut server_rng,
        )
        .unwrap_or_else(|error| panic!("server handshake begin failed: {error:?}"));
        let awaiting_server_proof = awaiting_server_hello
            .accept(decode_one(server_hello_flight.remaining()))
            .unwrap_or_else(|error| panic!("client rejected server hello: {error:?}"));
        let server_hello_length = server_hello_flight.remaining().len();
        server_hello_flight.advance(server_hello_length).unwrap();
        let mut server_proof_flight = server_hello_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete server hello retained its flight"));
        let mut client_proof_flight = awaiting_server_proof
            .verify(decode_one(server_proof_flight.remaining()))
            .unwrap_or_else(|error| panic!("client rejected server proof: {error:?}"));
        let server_proof_length = server_proof_flight.remaining().len();
        server_proof_flight.advance(server_proof_length).unwrap();
        let pending_client_proof = server_proof_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete server proof retained its flight"));

        let client_proof = decode_one(client_proof_flight.remaining());
        let client_proof_length = client_proof_flight.remaining().len();
        client_proof_flight.advance(client_proof_length).unwrap();
        let client_session = client_proof_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete client proof retained its flight"));
        let server_session = pending_client_proof
            .authenticate(client_proof)
            .unwrap_or_else(|error| panic!("server rejected client proof: {error:?}"));

        let mut request_flight = client_session
            .frame_request(message)
            .unwrap_or_else(|fault| panic!("client request framing failed: {:?}", fault.kind()));
        let request_record = decode_one(request_flight.remaining());
        let request_length = request_flight.remaining().len();
        request_flight.advance(request_length).unwrap();
        let _awaiting_response = request_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("complete client request retained its flight"));
        let authenticated = server_session
            .authenticate_request(request_record)
            .unwrap_or_else(|error| panic!("server rejected authenticated request: {error:?}"));
        let (request, awaiting_reply) = authenticated.into_parts();
        assert_eq!(request.key(), EXPECTED_KEY);
        assert_eq!(awaiting_reply.request_key(), EXPECTED_KEY);
        request
    }

    fn capabilities_request(request_id: RequestId) -> LocalApiRequest<AuthenticatedGrant> {
        let envelope = RequestEnvelope {
            version: ApiVersion::CURRENT,
            request_id,
            request: DeviceRequest::SystemCapabilities,
        };
        let mut encoded = [0_u8; MESSAGE_CAPACITY];
        let length = encode_request(&envelope, &mut encoded)
            .unwrap_or_else(|error| panic!("capabilities request encoding failed: {error:?}"));
        authenticated_request(OwnedMessage::new(
            MessageLength::new(length).expect("canonical request fits the handoff capacity"),
            encoded,
        ))
    }

    fn identity_request(request_id: RequestId) -> LocalApiRequest<AuthenticatedGrant> {
        let envelope = RequestEnvelope {
            version: ApiVersion::CURRENT,
            request_id,
            request: DeviceRequest::IdentitySummary,
        };
        let mut encoded = [0_u8; MESSAGE_CAPACITY];
        let length = encode_request(&envelope, &mut encoded)
            .unwrap_or_else(|error| panic!("identity request encoding failed: {error:?}"));
        authenticated_request(OwnedMessage::new(
            MessageLength::new(length).expect("canonical request fits the handoff capacity"),
            encoded,
        ))
    }

    fn inbox_status_request(request_id: RequestId) -> LocalApiRequest<AuthenticatedGrant> {
        let envelope = RequestEnvelope {
            version: ApiVersion::CURRENT,
            request_id,
            request: DeviceRequest::RnsInboxStatus,
        };
        let mut encoded = [0_u8; MESSAGE_CAPACITY];
        let length = encode_request(&envelope, &mut encoded)
            .unwrap_or_else(|error| panic!("inbox status request encoding failed: {error:?}"));
        authenticated_request(OwnedMessage::new(
            MessageLength::new(length).expect("canonical request fits the handoff capacity"),
            encoded,
        ))
    }

    fn nomad_fetch_id() -> NomadFetchId {
        NomadFetchId::new([0xc3; 8], 7).expect("fixed fetch ID is valid")
    }

    fn nomad_start_request(request_id: RequestId) -> LocalApiRequest<AuthenticatedGrant> {
        let envelope = RequestEnvelope {
            version: ApiVersion::CURRENT,
            request_id,
            request: DeviceRequest::NomadFetchStart(NomadFetchStartRequest::new(
                NOMAD_DESTINATION,
                NomadPagePath::new(NOMAD_PATH).expect("fixed Nomad path is valid"),
                NomadRequestTimestampUnixMs::new(NOMAD_TIMESTAMP_UNIX_MS)
                    .expect("fixed Nomad timestamp is valid"),
                NOMAD_IDEMPOTENCY_KEY,
            )),
        };
        let mut encoded = [0_u8; MESSAGE_CAPACITY];
        let length = encode_request(&envelope, &mut encoded)
            .unwrap_or_else(|error| panic!("Nomad start request encoding failed: {error:?}"));
        authenticated_request(OwnedMessage::new(
            MessageLength::new(length).expect("canonical request fits the handoff capacity"),
            encoded,
        ))
    }

    fn nomad_poll_request(request_id: RequestId) -> LocalApiRequest<AuthenticatedGrant> {
        let envelope = RequestEnvelope {
            version: ApiVersion::CURRENT,
            request_id,
            request: DeviceRequest::NomadFetchPoll(NomadFetchPollRequest {
                id: nomad_fetch_id(),
            }),
        };
        let mut encoded = [0_u8; MESSAGE_CAPACITY];
        let length = encode_request(&envelope, &mut encoded)
            .unwrap_or_else(|error| panic!("Nomad poll request encoding failed: {error:?}"));
        authenticated_request(OwnedMessage::new(
            MessageLength::new(length).expect("canonical request fits the handoff capacity"),
            encoded,
        ))
    }

    fn assert_authentication_required(
        reply: reticulum_device_api_handoff::LocalApiReply,
        request_id: RequestId,
        operation: u16,
    ) {
        assert_eq!(reply.key(), EXPECTED_KEY);
        let response = decode_response(reply.message().encoded())
            .unwrap_or_else(|error| panic!("authentication response was not canonical: {error:?}"));
        assert_eq!(
            response,
            ResponseEnvelope {
                version: ApiVersion::CURRENT,
                request_id,
                response: DeviceResponse::Error(ApiErrorResponse {
                    code: ApiErrorCode::AuthenticationRequired,
                    operation: Some(operation),
                }),
            }
        );
    }

    #[test]
    fn current_grant_dispatches_capabilities_with_exact_key_and_one_port_call() {
        let request_id = RequestId(0x1122_3344_5566_7788);
        let request = capabilities_request(request_id);
        let authority = active_authority(GENERATION);
        let mut port = CountingPort::default();

        let reply =
            dispatch_authenticated_request(request, Some(&authority), IDENTITY_SUMMARY, &mut port)
                .unwrap_or_else(|fault| panic!("valid dispatch failed: {:?}", fault.kind()));

        assert_eq!(reply.key(), EXPECTED_KEY);
        assert_eq!(port.availability_calls, 1);
        assert_eq!(port.status_calls, 0);
        assert_eq!(port.acceptance_calls, 0);
        let response = decode_response(reply.message().encoded())
            .unwrap_or_else(|error| panic!("capabilities response was not canonical: {error:?}"));
        assert_eq!(
            response,
            ResponseEnvelope {
                version: ApiVersion::CURRENT,
                request_id,
                response: DeviceResponse::SystemCapabilities(CapabilitySnapshot::for_dispatch(
                    true
                )),
            }
        );
    }

    #[test]
    fn current_grant_dispatches_inbox_status_only_through_the_inbox_port() {
        let request_id = RequestId(0xaabb_ccdd_eeff_0011);
        let request = inbox_status_request(request_id);
        let authority = active_authority(GENERATION);
        let mut submission_port = CountingPort::default();
        let mut inbox_port = CountingInboxPort::default();

        let reply = dispatch_authenticated_request_with_inbox(
            request,
            Some(&authority),
            IDENTITY_SUMMARY,
            &mut submission_port,
            &mut inbox_port,
        )
        .unwrap_or_else(|fault| panic!("valid inbox dispatch failed: {:?}", fault.kind()));

        assert_eq!(reply.key(), EXPECTED_KEY);
        assert_eq!(submission_port.total_calls(), 0);
        assert_eq!(inbox_port.availability_calls, 1);
        assert_eq!(inbox_port.status_calls, 1);
        assert_eq!(inbox_port.peek_calls, 0);
        let response = decode_response(reply.message().encoded())
            .unwrap_or_else(|error| panic!("inbox response was not canonical: {error:?}"));
        assert_eq!(
            response,
            ResponseEnvelope {
                version: ApiVersion::CURRENT,
                request_id,
                response: DeviceResponse::RnsInboxStatus(INBOX_STATUS),
            }
        );
    }

    #[test]
    fn current_grant_returns_public_identity_without_submission_port_io() {
        let request_id = RequestId(0x8877_6655_4433_2211);
        let request = identity_request(request_id);
        let authority = active_authority(GENERATION);
        let mut port = CountingPort::default();

        let reply =
            dispatch_authenticated_request(request, Some(&authority), IDENTITY_SUMMARY, &mut port)
                .unwrap_or_else(|fault| panic!("valid dispatch failed: {:?}", fault.kind()));

        assert_eq!(reply.key(), EXPECTED_KEY);
        assert_eq!(port.total_calls(), 0);
        let response = decode_response(reply.message().encoded())
            .unwrap_or_else(|error| panic!("identity response was not canonical: {error:?}"));
        assert_eq!(
            response,
            ResponseEnvelope {
                version: ApiVersion::CURRENT,
                request_id,
                response: DeviceResponse::IdentitySummary(IDENTITY_SUMMARY),
            }
        );
    }

    #[test]
    fn absent_revoked_and_generation_mismatched_authorities_fail_without_port_fallback() {
        let mut port = CountingPort::default();

        let absent_id = RequestId(21);
        let absent = dispatch_authenticated_request::<1, _>(
            capabilities_request(absent_id),
            None,
            IDENTITY_SUMMARY,
            &mut port,
        )
        .unwrap_or_else(|fault| panic!("absent authority response failed: {:?}", fault.kind()));
        assert_authentication_required(absent, absent_id, OP_SYSTEM_CAPABILITIES);

        let revoked_id = RequestId(22);
        let revoked_authority = revoked_authority();
        let revoked = dispatch_authenticated_request(
            capabilities_request(revoked_id),
            Some(&revoked_authority),
            IDENTITY_SUMMARY,
            &mut port,
        )
        .unwrap_or_else(|fault| panic!("revoked authority response failed: {:?}", fault.kind()));
        assert_authentication_required(revoked, revoked_id, OP_SYSTEM_CAPABILITIES);

        let mismatched_id = RequestId(23);
        let mismatched_authority = active_authority(CredentialGeneration::new(8));
        let mismatched = dispatch_authenticated_request(
            capabilities_request(mismatched_id),
            Some(&mismatched_authority),
            IDENTITY_SUMMARY,
            &mut port,
        )
        .unwrap_or_else(|fault| {
            panic!(
                "generation-mismatched authority response failed: {:?}",
                fault.kind()
            )
        });
        assert_authentication_required(mismatched, mismatched_id, OP_SYSTEM_CAPABILITIES);

        assert_eq!(port.total_calls(), 0);
    }

    #[test]
    fn stale_or_absent_authority_rejects_inbox_status_before_either_port() {
        let mut submission_port = CountingPort::default();
        let mut inbox_port = CountingInboxPort::default();

        let absent_id = RequestId(31);
        let absent = dispatch_authenticated_request_with_inbox::<1, _, _>(
            inbox_status_request(absent_id),
            None,
            IDENTITY_SUMMARY,
            &mut submission_port,
            &mut inbox_port,
        )
        .unwrap_or_else(|fault| panic!("absent authority response failed: {:?}", fault.kind()));
        assert_authentication_required(absent, absent_id, OP_EXPERIMENTAL_RNS_INBOX_STATUS);

        let revoked_id = RequestId(32);
        let revoked_authority = revoked_authority();
        let revoked = dispatch_authenticated_request_with_inbox(
            inbox_status_request(revoked_id),
            Some(&revoked_authority),
            IDENTITY_SUMMARY,
            &mut submission_port,
            &mut inbox_port,
        )
        .unwrap_or_else(|fault| panic!("revoked authority response failed: {:?}", fault.kind()));
        assert_authentication_required(revoked, revoked_id, OP_EXPERIMENTAL_RNS_INBOX_STATUS);

        let mismatched_id = RequestId(33);
        let mismatched_authority = active_authority(CredentialGeneration::new(8));
        let mismatched = dispatch_authenticated_request_with_inbox(
            inbox_status_request(mismatched_id),
            Some(&mismatched_authority),
            IDENTITY_SUMMARY,
            &mut submission_port,
            &mut inbox_port,
        )
        .unwrap_or_else(|fault| {
            panic!(
                "generation-mismatched authority response failed: {:?}",
                fault.kind()
            )
        });
        assert_authentication_required(mismatched, mismatched_id, OP_EXPERIMENTAL_RNS_INBOX_STATUS);

        assert_eq!(submission_port.total_calls(), 0);
        assert_eq!(inbox_port.total_calls(), 0);
    }

    #[test]
    fn complete_wrapper_rejects_absent_and_stale_authority_before_either_port() {
        let mut appliance = CountingCompletePort::default();
        let mut nomad = CountingNomadPort::new();

        let absent_id = RequestId(41);
        let absent = dispatch_authenticated_request_with_inbox_lxmf_and_nomad::<1, _, _>(
            capabilities_request(absent_id),
            None,
            IDENTITY_SUMMARY,
            &mut appliance,
            &mut nomad,
        )
        .unwrap_or_else(|fault| panic!("absent authority response failed: {:?}", fault.kind()));
        assert_authentication_required(absent, absent_id, OP_SYSTEM_CAPABILITIES);

        let stale_id = RequestId(42);
        let stale_authority = active_authority(CredentialGeneration::new(8));
        let stale = dispatch_authenticated_request_with_inbox_lxmf_and_nomad(
            capabilities_request(stale_id),
            Some(&stale_authority),
            IDENTITY_SUMMARY,
            &mut appliance,
            &mut nomad,
        )
        .unwrap_or_else(|fault| panic!("stale authority response failed: {:?}", fault.kind()));
        assert_authentication_required(stale, stale_id, OP_SYSTEM_CAPABILITIES);

        assert_eq!(appliance.total_calls(), 0);
        assert_eq!(nomad.total_calls(), 0);
    }

    #[test]
    fn complete_wrapper_capabilities_include_nomad_and_query_each_availability_once() {
        let request_id = RequestId(43);
        let authority = active_authority(GENERATION);
        let mut appliance = CountingCompletePort::default();
        let mut nomad = CountingNomadPort::new();

        let reply = dispatch_authenticated_request_with_inbox_lxmf_and_nomad(
            capabilities_request(request_id),
            Some(&authority),
            IDENTITY_SUMMARY,
            &mut appliance,
            &mut nomad,
        )
        .unwrap_or_else(|fault| panic!("valid capabilities dispatch failed: {:?}", fault.kind()));

        assert_eq!(reply.key(), EXPECTED_KEY);
        let response = decode_response(reply.message().encoded())
            .unwrap_or_else(|error| panic!("capabilities response was not canonical: {error:?}"));
        assert_eq!(
            response,
            ResponseEnvelope {
                version: ApiVersion::CURRENT,
                request_id,
                response: DeviceResponse::SystemCapabilities(
                    CapabilitySnapshot::for_dispatch_with_inbox_lxmf_basic_send_and_peer_discovery(
                        true,
                        CapabilityAvailability::Available,
                        CapabilityAvailability::Available,
                        CapabilityAvailability::Available,
                        CapabilityAvailability::Available,
                        NOMAD_PEER_APP_DATA_BYTES,
                    )
                    .with_dispatch_nomad(CapabilityAvailability::Available),
                ),
            }
        );

        assert_eq!(appliance.submission_availability_calls, 1);
        assert_eq!(appliance.inbox_availability_calls, 1);
        assert_eq!(appliance.lxmf_availability_calls, 1);
        assert_eq!(appliance.compose_availability_calls, 1);
        assert_eq!(appliance.peer_availability_calls, 1);
        assert_eq!(appliance.peer_max_app_data_calls, 1);
        assert_eq!(appliance.total_calls(), 6);
        assert_eq!(nomad.availability_calls, 1);
        assert_eq!(nomad.start_calls, 0);
        assert_eq!(nomad.poll_calls, 0);
        assert_eq!(nomad.total_calls(), 1);
    }

    #[test]
    fn complete_wrapper_nomad_start_and_poll_touch_only_the_nomad_port() {
        let authority = active_authority(GENERATION);
        let mut appliance = CountingCompletePort::default();
        let mut nomad = CountingNomadPort::new();

        let start_id = RequestId(44);
        let start = dispatch_authenticated_request_with_inbox_lxmf_and_nomad(
            nomad_start_request(start_id),
            Some(&authority),
            IDENTITY_SUMMARY,
            &mut appliance,
            &mut nomad,
        )
        .unwrap_or_else(|fault| panic!("valid Nomad start dispatch failed: {:?}", fault.kind()));
        let start_response = decode_response(start.message().encoded())
            .unwrap_or_else(|error| panic!("Nomad start response was not canonical: {error:?}"));
        assert_eq!(
            start_response,
            ResponseEnvelope {
                version: ApiVersion::CURRENT,
                request_id: start_id,
                response: DeviceResponse::NomadFetchStartAccepted(NomadFetchStartAccepted {
                    id: nomad_fetch_id(),
                    outcome: NomadFetchStartOutcome::Accepted,
                }),
            }
        );
        assert_eq!(appliance.total_calls(), 0);
        assert_eq!(nomad.availability_calls, 1);
        assert_eq!(nomad.start_calls, 1);
        assert_eq!(nomad.poll_calls, 0);
        assert_eq!(nomad.observed_start_principal, Some(PRINCIPAL));
        assert!(nomad.start_request_matches);

        let poll_id = RequestId(45);
        let poll = dispatch_authenticated_request_with_inbox_lxmf_and_nomad(
            nomad_poll_request(poll_id),
            Some(&authority),
            IDENTITY_SUMMARY,
            &mut appliance,
            &mut nomad,
        )
        .unwrap_or_else(|fault| panic!("valid Nomad poll dispatch failed: {:?}", fault.kind()));
        let poll_response = decode_response(poll.message().encoded())
            .unwrap_or_else(|error| panic!("Nomad poll response was not canonical: {error:?}"));
        assert_eq!(
            poll_response,
            ResponseEnvelope {
                version: ApiVersion::CURRENT,
                request_id: poll_id,
                response: DeviceResponse::NomadFetchPoll(NomadFetchPollResponse::Pending(
                    NomadFetchPhase::AwaitingResponse,
                )),
            }
        );
        assert_eq!(appliance.total_calls(), 0);
        assert_eq!(nomad.availability_calls, 2);
        assert_eq!(nomad.start_calls, 1);
        assert_eq!(nomad.poll_calls, 1);
        assert_eq!(nomad.observed_poll, Some((PRINCIPAL, nomad_fetch_id())));
    }

    #[test]
    fn malformed_authenticated_payload_retains_exact_request_without_port_calls() {
        let malformed = [0xff, 0x00, 0x01];
        let request = authenticated_request(owned_message(&malformed));
        let authority = active_authority(GENERATION);
        let mut port = CountingPort::default();

        let failure = match dispatch_authenticated_request(
            request,
            Some(&authority),
            IDENTITY_SUMMARY,
            &mut port,
        ) {
            Ok(_) => panic!("malformed logical request unexpectedly dispatched"),
            Err(failure) => failure,
        };

        assert_eq!(
            failure.kind(),
            AuthenticatedApiDispatchFailureKind::MalformedRequest
        );
        let retained = failure.into_request();
        assert_eq!(retained.key(), EXPECTED_KEY);
        assert_eq!(retained.message().encoded(), malformed);
        assert_eq!(retained.grant().credential_id(), CREDENTIAL_ID);
        assert_eq!(retained.grant().credential_generation(), GENERATION);
        assert_eq!(port.total_calls(), 0);
    }
}
