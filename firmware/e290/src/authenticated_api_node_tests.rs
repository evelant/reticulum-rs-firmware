use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilityAvailability, CapabilitySnapshot,
    DestinationHash, DeviceRequest, DeviceResponse, IdempotencyKey, IdentitySummary,
    ManualServiceAnnounceDisposition, NetworkConfigMutationOutcome, NetworkConfigMutationRequest,
    NetworkConfigSnapshot, NetworkRuntimeStatus, NodeDiagnosticsSnapshot, NomadFetchId,
    NomadFetchPhase, NomadFetchPollRequest, NomadFetchPollResponse, NomadFetchStartAccepted,
    NomadFetchStartOutcome, NomadFetchStartRequest, NomadPagePath, NomadRequestTimestampUnixMs,
    OP_SYSTEM_CAPABILITIES, Permissions, PrincipalId, RequestEnvelope, RequestId, ResponseEnvelope,
    RnsDiagnostics, RouteDiagnosticsPage, RouteDiagnosticsRequest, decode_response, encode_request,
};
use reticulum_device_api_adapter::{
    LxmfComposeAcceptance, LxmfComposePort, LxmfComposePortError, LxmfComposeRequest,
    LxmfInboxPort, LxmfInboxPortError, ManualServiceAnnouncePort, NetworkConfigPort,
    NetworkConfigPortError, NodeDiagnosticsPort, NodeDiagnosticsPortError, NomadFetchPort,
    NomadFetchPortError, NomadFetchStartDisposition, PeerDiscoveryPort, PeerDiscoveryPortError,
    SubmissionAcceptance, SubmissionPort, SubmissionPortError,
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
    dispatch_authenticated_request_with_lxmf_and_nomad,
    dispatch_authenticated_request_with_lxmf_nomad_and_network_config,
};

const CREDENTIAL_ID: CredentialId = CredentialId::new([
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
]);
const GENERATION: CredentialGeneration = CredentialGeneration::new(7);
const DEVICE_ID: DeviceId = DeviceId::new([
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
]);
const PRINCIPAL: PrincipalId = PrincipalId([0x31; 16]);
const PSK: [u8; 32] = [0x53; 32];
const CLIENT_NONCE: [u8; 32] = [0x64; 32];
const SERVER_NONCE: [u8; 32] = [0x75; 32];
const EXPECTED_KEY: RequestKey = RequestKey::new(SessionEpoch::new(1), CorrelationId::new(0));
const IDENTITY_SUMMARY: IdentitySummary = IdentitySummary::new(DestinationHash([0x91; 16]));
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
struct CountingCompletePort {
    submission_availability_calls: usize,
    submission_status_calls: usize,
    submission_accept_calls: usize,
    observed_submission_permission_bits: Option<u32>,
    lxmf_availability_calls: usize,
    lxmf_next_calls: usize,
    lxmf_read_calls: usize,
    lxmf_mailbox_status_calls: usize,
    lxmf_mailbox_acknowledge_calls: usize,
    compose_availability_calls: usize,
    compose_calls: usize,
    peer_availability_calls: usize,
    peer_max_app_data_calls: usize,
    peer_next_calls: usize,
    network_availability_calls: usize,
    network_configuration_calls: usize,
    network_mutation_calls: usize,
    network_status_calls: usize,
    manual_announce_availability_calls: usize,
    manual_announce_queue_calls: usize,
    node_diagnostics_calls: usize,
    route_diagnostics_calls: usize,
    radio_trace_calls: usize,
}

impl CountingCompletePort {
    const fn total_calls(&self) -> usize {
        self.submission_availability_calls
            + self.submission_status_calls
            + self.submission_accept_calls
            + self.lxmf_availability_calls
            + self.lxmf_next_calls
            + self.lxmf_read_calls
            + self.lxmf_mailbox_status_calls
            + self.lxmf_mailbox_acknowledge_calls
            + self.compose_availability_calls
            + self.compose_calls
            + self.peer_availability_calls
            + self.peer_max_app_data_calls
            + self.peer_next_calls
            + self.network_availability_calls
            + self.network_configuration_calls
            + self.network_mutation_calls
            + self.network_status_calls
            + self.manual_announce_availability_calls
            + self.manual_announce_queue_calls
            + self.node_diagnostics_calls
            + self.route_diagnostics_calls
            + self.radio_trace_calls
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
        candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        self.submission_accept_calls += 1;
        self.observed_submission_permission_bits =
            Some(candidate.authorization().granted_permission_bits());
        Ok(SubmissionAcceptance::CapacityExhausted)
    }
}

impl ManualServiceAnnouncePort for CountingCompletePort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.manual_announce_availability_calls += 1;
        CapabilityAvailability::Available
    }

    fn queue_service_announce(&mut self) -> ManualServiceAnnounceDisposition {
        self.manual_announce_queue_calls += 1;
        ManualServiceAnnounceDisposition::Queued
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

    fn mailbox_status(
        &mut self,
    ) -> Result<reticulum_device_api::LxmfMailboxStatus, LxmfInboxPortError> {
        self.lxmf_mailbox_status_calls += 1;
        reticulum_device_api::LxmfMailboxStatus::new(None, None)
            .map_err(|_| LxmfInboxPortError::Faulted)
    }

    fn acknowledge_mailbox_through(
        &mut self,
        through: reticulum_device_api::LxmfMessageHandle,
    ) -> Result<reticulum_device_api::LxmfMailboxStatus, LxmfInboxPortError> {
        self.lxmf_mailbox_acknowledge_calls += 1;
        reticulum_device_api::LxmfMailboxStatus::new(Some(through), Some(through))
            .map_err(|_| LxmfInboxPortError::Faulted)
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

impl NetworkConfigPort for CountingCompletePort {
    fn availability(&mut self) -> CapabilityAvailability {
        self.network_availability_calls += 1;
        CapabilityAvailability::Available
    }

    fn configuration(&mut self) -> Result<NetworkConfigSnapshot, NetworkConfigPortError> {
        self.network_configuration_calls += 1;
        NetworkConfigSnapshot::with_defaults(0, [None; 4], None)
            .map_err(|_| NetworkConfigPortError::Invariant)
    }

    fn mutate(
        &mut self,
        _principal: PrincipalId,
        _request: NetworkConfigMutationRequest<'_>,
    ) -> Result<NetworkConfigMutationOutcome, NetworkConfigPortError> {
        self.network_mutation_calls += 1;
        Err(NetworkConfigPortError::InvalidRequest)
    }

    fn status(&mut self) -> Result<NetworkRuntimeStatus, NetworkConfigPortError> {
        self.network_status_calls += 1;
        Err(NetworkConfigPortError::Unavailable)
    }
}

impl NodeDiagnosticsPort for CountingCompletePort {
    fn node_diagnostics(&mut self) -> Result<NodeDiagnosticsSnapshot, NodeDiagnosticsPortError> {
        self.node_diagnostics_calls += 1;
        Ok(NodeDiagnosticsSnapshot::new(
            0,
            [None; reticulum_device_api::MAX_DIAGNOSTIC_INTERFACES],
            None,
            RnsDiagnostics::new(0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
            0,
            0,
            0,
        ))
    }

    fn route_diagnostics_page(
        &mut self,
        _request: RouteDiagnosticsRequest,
    ) -> Result<RouteDiagnosticsPage, NodeDiagnosticsPortError> {
        self.route_diagnostics_calls += 1;
        RouteDiagnosticsPage::new(
            0,
            0,
            [None; reticulum_device_api::MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES],
            None,
        )
        .map_err(|_| NodeDiagnosticsPortError::Invariant)
    }

    fn radio_trace_page(
        &mut self,
        _request: reticulum_device_api::RadioTracePageRequest,
    ) -> Result<reticulum_device_api::RadioTracePage, NodeDiagnosticsPortError> {
        self.radio_trace_calls += 1;
        reticulum_device_api::RadioTracePage::new(
            1,
            reticulum_device_api::RadioTraceAppliedLoraProfile::new(
                [0x51; 16],
                915_000_000,
                125_000,
                8,
                22,
                10,
                5,
                true,
                true,
                false,
            ),
            1,
            1,
            false,
            [None; reticulum_device_api::MAX_RADIO_TRACE_PAGE_ENTRIES],
            None,
        )
        .map_err(|_| NodeDiagnosticsPortError::Invariant)
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
    active_authority_with_facts(
        generation,
        Permissions::NONE,
        PairingOrigin::LocalPhysicalPresence,
        AuthorizationPolicyVersion::new(1),
    )
}

fn active_authority_with_facts(
    generation: CredentialGeneration,
    permissions: Permissions,
    pairing_origin: PairingOrigin,
    policy_version: AuthorizationPolicyVersion,
) -> CredentialAuthority<1> {
    let revision = AuthorityRevision::new(generation.get());
    let builder = CredentialAuthorityBuilder::new(revision)
        .unwrap_or_else(|fault| panic!("authority revision rejected: {:?}", fault.kind()));
    builder
        .insert(CredentialRecord::with_secret(
            CREDENTIAL_ID,
            generation,
            PRINCIPAL,
            permissions,
            CredentialStatus::Active,
            CredentialAudit::new(revision, revision, pairing_origin, policy_version),
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
                PairingOrigin::LocalPhysicalPresence,
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
        ClientParameters::new(DEVICE_ID, BearerBinding::BleGatt),
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
        ServerParameters::new(DEVICE_ID, BearerBinding::BleGatt),
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

fn network_config_request(request_id: RequestId) -> LocalApiRequest<AuthenticatedGrant> {
    let envelope = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id,
        request: DeviceRequest::NetworkConfigGet,
    };
    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = encode_request(&envelope, &mut encoded)
        .unwrap_or_else(|error| panic!("network config request encoding failed: {error:?}"));
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
            response: DeviceResponse::SystemCapabilities(CapabilitySnapshot::for_dispatch(true)),
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
fn complete_wrapper_rejects_absent_and_stale_authority_before_either_port() {
    let mut appliance = CountingCompletePort::default();
    let mut nomad = CountingNomadPort::new();

    let absent_id = RequestId(41);
    let absent = dispatch_authenticated_request_with_lxmf_and_nomad::<1, _, _>(
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
    let stale = dispatch_authenticated_request_with_lxmf_and_nomad(
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

    let reply = dispatch_authenticated_request_with_lxmf_and_nomad(
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
                CapabilitySnapshot::for_dispatch_with_lxmf_basic_send_and_peer_discovery(
                    true,
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
    assert_eq!(appliance.lxmf_availability_calls, 1);
    assert_eq!(appliance.compose_availability_calls, 1);
    assert_eq!(appliance.peer_availability_calls, 1);
    assert_eq!(appliance.peer_max_app_data_calls, 1);
    assert_eq!(appliance.total_calls(), 5);
    assert_eq!(nomad.availability_calls, 1);
    assert_eq!(nomad.start_calls, 0);
    assert_eq!(nomad.poll_calls, 0);
    assert_eq!(nomad.total_calls(), 1);
}

#[test]
fn network_wrapper_reads_configuration_through_the_same_appliance_owner() {
    let request_id = RequestId(46);
    let authority = active_authority(GENERATION);
    let mut appliance = CountingCompletePort::default();
    let mut nomad = CountingNomadPort::new();

    let reply = dispatch_authenticated_request_with_lxmf_nomad_and_network_config(
        network_config_request(request_id),
        Some(&authority),
        IDENTITY_SUMMARY,
        &mut appliance,
        &mut nomad,
    )
    .unwrap_or_else(|fault| {
        panic!(
            "valid network configuration dispatch failed: {:?}",
            fault.kind()
        )
    });

    assert_eq!(reply.key(), EXPECTED_KEY);
    let response = decode_response(reply.message().encoded()).unwrap_or_else(|error| {
        panic!("network configuration response was not canonical: {error:?}")
    });
    assert_eq!(
        response,
        ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id,
            response: DeviceResponse::NetworkConfig(
                NetworkConfigSnapshot::with_defaults(0, [None; 4], None)
                    .expect("empty erased snapshot is valid"),
            ),
        }
    );
    assert_eq!(appliance.network_availability_calls, 1);
    assert_eq!(appliance.network_configuration_calls, 1);
    assert_eq!(appliance.network_mutation_calls, 0);
    assert_eq!(appliance.network_status_calls, 0);
    assert_eq!(appliance.total_calls(), 2);
    assert_eq!(nomad.total_calls(), 0);
}

#[test]
fn complete_wrapper_nomad_start_and_poll_touch_only_the_nomad_port() {
    let authority = active_authority(GENERATION);
    let mut appliance = CountingCompletePort::default();
    let mut nomad = CountingNomadPort::new();

    let start_id = RequestId(44);
    let start = dispatch_authenticated_request_with_lxmf_and_nomad(
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
    let poll = dispatch_authenticated_request_with_lxmf_and_nomad(
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
