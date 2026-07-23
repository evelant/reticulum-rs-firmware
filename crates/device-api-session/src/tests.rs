extern crate std;

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiVersion, CapabilityAvailability, DestinationHash, DeviceRequest, DeviceResponse,
    IdempotencyKey, IdentitySummary, Permissions, PrincipalId, RequestEnvelope, RequestId,
    decode_request, decode_response, encode_request, encode_response,
};
use reticulum_device_api_adapter::{
    SubmissionAcceptance, SubmissionPort, SubmissionPortError, dispatch,
};
use reticulum_device_api_credentials::{
    AuthorityRevision, AuthorizationPolicyVersion, CredentialAudit, CredentialAuthority,
    CredentialAuthorityBuilder, CredentialRecord, CredentialStatus, PairingOrigin,
    RevocationReason,
};
use reticulum_device_api_framing::{
    AUTH_TAG_LENGTH, AUTHENTICATED_DATA_CAPACITY, DecodeEvent, FramedRecord, PAYLOAD_CAPACITY,
    PayloadLength, Record, StreamDecoder,
};
use reticulum_device_api_handoff::{
    CorrelationId, LocalApiReply, MessageLength, OwnedMessage, RequestKey, SessionEpoch,
};
use reticulum_storage_model::{
    AcceptanceCandidate, LifecycleState, PrincipalId as StoragePrincipalId,
    SubmissionId as StorageSubmissionId,
};
use zeroize::Zeroizing;

use crate::{
    ActiveCredential, AwaitingReply, AwaitingResponse, AwaitingServerHello, BearerBinding,
    ClientCredential, ClientHandshakeError, ClientHello, ClientHelloFlight, ClientParameters,
    ClientRequestFaultKind, ClientSession, ClientSessionFault, CredentialGeneration, CredentialId,
    DeviceId, HandshakeError, PendingClientProof, ReplyFlight, ReplyRouteFaultKind, ServerHello,
    ServerHelloFlight, ServerParameters, ServerSession, SessionEpochAllocator, SessionFault,
    SessionSuite,
    crypto::{
        KeySchedule, client_proof, client_record_tag, derive, server_proof, server_record_tag,
        verify_server_record_tag,
    },
    protocol::{
        RECORD_KIND_CLIENT_PROOF, RECORD_KIND_REQUEST, RECORD_KIND_RESPONSE,
        RECORD_KIND_SERVER_PROOF, proof_record, take_proof,
    },
    server::client_tag_for_test,
};

const CREDENTIAL_ID: CredentialId = CredentialId::new([
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
]);
const DEVICE_ID: DeviceId = DeviceId::new([
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
]);
const CLIENT_NONCE: [u8; 32] = [
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f,
    0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f,
];
const SERVER_NONCE: [u8; 32] = [
    0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f,
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f,
];
const PSK: [u8; 32] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];
const GENERATION: CredentialGeneration = CredentialGeneration::new(0x0102_0304_0506_0708);
const EPOCH: SessionEpoch = SessionEpoch::new(1);
const PRINCIPAL: PrincipalId = PrincipalId([0x31; 16]);

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

fn client_hello() -> ClientHello {
    ClientHello::new(BearerBinding::UsbSerialJtag, CREDENTIAL_ID, CLIENT_NONCE)
}

fn decode_one(bytes: &[u8]) -> Record {
    let mut decoder = StreamDecoder::new();
    for byte in bytes {
        match decoder.push(*byte) {
            DecodeEvent::Pending => {}
            DecodeEvent::Record(record) => return record,
            DecodeEvent::MalformedCobs
            | DecodeEvent::MalformedRecord(_)
            | DecodeEvent::Overflow => panic!("server emitted malformed framing"),
        }
    }
    panic!("server flight did not contain a complete record")
}

fn assert_record_vector(record: &Record, decoded_hex: &str, wire_hex: &str) {
    let expected_decoded = hex::decode(decoded_hex).unwrap();
    let mut authenticated = [0_u8; AUTHENTICATED_DATA_CAPACITY];
    let authenticated_length = record.write_authenticated_data(&mut authenticated);
    assert_eq!(
        &expected_decoded[..authenticated_length],
        &authenticated[..authenticated_length]
    );
    assert_eq!(
        &expected_decoded[authenticated_length..],
        record.authentication_tag()
    );

    let framed = FramedRecord::encode(record).unwrap();
    assert_eq!(hex::encode(framed.encoded()), wire_hex);
}

fn begin() -> ServerHelloFlight {
    begin_with(GENERATION, SERVER_NONCE)
}

fn begin_with(generation: CredentialGeneration, nonce: [u8; 32]) -> ServerHelloFlight {
    let mut epochs = SessionEpochAllocator::new();
    begin_with_allocator(generation, nonce, &mut epochs)
}

fn begin_with_allocator(
    generation: CredentialGeneration,
    nonce: [u8; 32],
    epochs: &mut SessionEpochAllocator,
) -> ServerHelloFlight {
    let mut rng = FixedRng::new(nonce);
    ServerHelloFlight::begin(
        client_hello(),
        ActiveCredential::new(CREDENTIAL_ID, generation, PSK),
        ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
        epochs,
        &mut rng,
    )
    .unwrap_or_else(|error| panic!("handshake begin failed: {error:?}"))
}

fn complete_server_flight(
    mut hello_flight: ServerHelloFlight,
) -> (PendingClientProof, KeySchedule, ServerHello, [u8; 32]) {
    let server_hello_record = decode_one(hello_flight.remaining());
    let server_hello = ServerHello::from_record(server_hello_record)
        .unwrap_or_else(|error| panic!("bad server hello: {error:?}"));
    let hello_length = hello_flight.remaining().len();
    hello_flight.advance(hello_length).unwrap();
    let mut proof_flight = match hello_flight.try_finish() {
        Ok(flight) => flight,
        Err(_) => panic!("complete hello did not advance typestate"),
    };
    let schedule = derive(&PSK, &client_hello(), &server_hello);
    let server_proof_record = decode_one(proof_flight.remaining());
    let observed_server_proof = take_proof(
        server_proof_record,
        RECORD_KIND_SERVER_PROOF,
        schedule.session_id,
    )
    .unwrap_or_else(|error| panic!("bad server proof record: {error:?}"));
    assert_eq!(observed_server_proof, server_proof(&schedule));
    let proof_length = proof_flight.remaining().len();
    proof_flight.advance(proof_length).unwrap();
    let pending = match proof_flight.try_finish() {
        Ok(pending) => pending,
        Err(_) => panic!("complete proof did not advance typestate"),
    };
    (pending, schedule, server_hello, observed_server_proof)
}

fn establish() -> (ServerSession, KeySchedule, ServerHello, [u8; 32]) {
    let mut epochs = SessionEpochAllocator::new();
    establish_with_allocator(&mut epochs, SERVER_NONCE)
}

fn establish_with_allocator(
    epochs: &mut SessionEpochAllocator,
    nonce: [u8; 32],
) -> (ServerSession, KeySchedule, ServerHello, [u8; 32]) {
    let flight = begin_with_allocator(GENERATION, nonce, epochs);
    let (pending, schedule, server_hello, observed_server_proof) = complete_server_flight(flight);
    let proof = client_proof(&schedule, &observed_server_proof);
    let client_proof_record = proof_record(RECORD_KIND_CLIENT_PROOF, schedule.session_id, &proof);
    let session = pending
        .authenticate(client_proof_record)
        .unwrap_or_else(|error| panic!("client proof failed: {error:?}"));
    (session, schedule, server_hello, observed_server_proof)
}

fn credential_authority(
    generation: CredentialGeneration,
    permissions: Permissions,
) -> CredentialAuthority<1> {
    let revision = AuthorityRevision::new(generation.get());
    let builder = match CredentialAuthorityBuilder::new(revision) {
        Ok(builder) => builder,
        Err(fault) => panic!("credential authority revision failed: {:?}", fault.kind()),
    };
    let record = CredentialRecord::with_secret(
        CREDENTIAL_ID,
        generation,
        PRINCIPAL,
        permissions,
        CredentialStatus::Active,
        CredentialAudit::new(
            revision,
            revision,
            PairingOrigin::UsbPhysicalPresence,
            AuthorizationPolicyVersion::new(1),
        ),
        PSK,
    );
    builder
        .insert(record)
        .unwrap_or_else(|fault| panic!("credential authority record failed: {:?}", fault.kind()))
        .finish()
}

fn revoked_authority(generation: CredentialGeneration) -> CredentialAuthority<1> {
    let revision = AuthorityRevision::new(generation.get());
    let builder = match CredentialAuthorityBuilder::new(revision) {
        Ok(builder) => builder,
        Err(fault) => panic!("credential authority revision failed: {:?}", fault.kind()),
    };
    let record = CredentialRecord::revoked(
        CREDENTIAL_ID,
        generation,
        PRINCIPAL,
        CredentialAudit::new(
            AuthorityRevision::new(GENERATION.get()),
            revision,
            PairingOrigin::UsbPhysicalPresence,
            AuthorizationPolicyVersion::new(1),
        ),
        RevocationReason::Explicit,
    );
    builder
        .insert(record)
        .unwrap_or_else(|fault| panic!("revoked credential record failed: {:?}", fault.kind()))
        .finish()
}

fn establish_from_authority(authority: &CredentialAuthority<1>) -> (ServerSession, KeySchedule) {
    let selected = authority
        .select_for_handshake(CREDENTIAL_ID)
        .unwrap_or_else(|_| panic!("active credential was unavailable"));
    let mut epochs = SessionEpochAllocator::new();
    let mut rng = FixedRng::new(SERVER_NONCE);
    let flight = ServerHelloFlight::begin(
        client_hello(),
        ActiveCredential::from_selected(selected),
        ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
        &mut epochs,
        &mut rng,
    )
    .unwrap_or_else(|error| panic!("authority handshake begin failed: {error:?}"));
    let (pending, schedule, _, observed_server_proof) = complete_server_flight(flight);
    let proof = client_proof(&schedule, &observed_server_proof);
    let record = proof_record(RECORD_KIND_CLIENT_PROOF, schedule.session_id, &proof);
    let session = pending
        .authenticate(record)
        .unwrap_or_else(|error| panic!("authority client proof failed: {error:?}"));
    (session, schedule)
}

#[derive(Default)]
struct CountingPort {
    availability_calls: usize,
    status_calls: usize,
    acceptance_calls: usize,
    last_candidate: Option<AcceptanceCandidate>,
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
        candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        self.acceptance_calls += 1;
        self.last_candidate = Some(candidate);
        Ok(SubmissionAcceptance::Accepted(StorageSubmissionId::new(1)))
    }
}

fn tagged_request(schedule: &KeySchedule, sequence: u64, payload: &[u8]) -> Record {
    let mut owner = [0_u8; PAYLOAD_CAPACITY];
    owner[..payload.len()].copy_from_slice(payload);
    let untagged = Record::new(
        RECORD_KIND_REQUEST,
        schedule.session_id.0,
        sequence,
        PayloadLength::new(payload.len()).unwrap(),
        owner,
        [0_u8; AUTH_TAG_LENGTH],
    );
    let tag = client_tag_for_test(&schedule.client_record_key, &untagged);
    let (kind, session_id, sequence, length, payload, _) = untagged.into_parts();
    Record::new(kind, session_id, sequence, length, payload, tag)
}

fn client_parameters() -> ClientParameters {
    ClientParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag)
}

fn client_credential() -> ClientCredential {
    ClientCredential::from_zeroizing(CREDENTIAL_ID, GENERATION, Zeroizing::new(PSK))
}

fn owned_message(payload: &[u8]) -> OwnedMessage {
    let mut owner = [0_u8; PAYLOAD_CAPACITY];
    owner[..payload.len()].copy_from_slice(payload);
    OwnedMessage::new(MessageLength::new(payload.len()).unwrap(), owner)
}

fn expect_error<T, E>(result: Result<T, E>, success_message: &str) -> E {
    match result {
        Ok(_) => panic!("{success_message}"),
        Err(error) => error,
    }
}

fn start_client_handshake() -> (AwaitingServerHello, ClientHello) {
    let mut rng = FixedRng::new(CLIENT_NONCE);
    let mut flight = ClientHelloFlight::begin(client_parameters(), client_credential(), &mut rng)
        .unwrap_or_else(|error| panic!("client handshake begin failed: {error:?}"));
    let hello = ClientHello::from_record(decode_one(flight.remaining()))
        .unwrap_or_else(|error| panic!("client emitted invalid hello: {error:?}"));
    let length = flight.remaining().len();
    flight.advance(length).unwrap();
    let awaiting = flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete client hello did not advance typestate"));
    (awaiting, hello)
}

fn server_hello_record_for(
    client_hello: ClientHello,
    device_id: DeviceId,
    generation: CredentialGeneration,
) -> Record {
    let mut epochs = SessionEpochAllocator::new();
    let mut rng = FixedRng::new(SERVER_NONCE);
    let flight = ServerHelloFlight::begin(
        client_hello,
        ActiveCredential::new(CREDENTIAL_ID, generation, PSK),
        ServerParameters::new(device_id, BearerBinding::UsbSerialJtag),
        &mut epochs,
        &mut rng,
    )
    .unwrap_or_else(|error| panic!("server handshake begin failed: {error:?}"));
    decode_one(flight.remaining())
}

fn establish_client_server() -> (ClientSession, ServerSession, KeySchedule) {
    let mut epochs = SessionEpochAllocator::new();
    establish_client_server_for(
        client_parameters(),
        ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
        &mut epochs,
        CLIENT_NONCE,
        SERVER_NONCE,
    )
}

fn establish_client_server_for(
    client_parameters: ClientParameters,
    server_parameters: ServerParameters,
    epochs: &mut SessionEpochAllocator,
    client_nonce: [u8; 32],
    server_nonce: [u8; 32],
) -> (ClientSession, ServerSession, KeySchedule) {
    let mut client_rng = FixedRng::new(client_nonce);
    let mut client_hello_flight =
        ClientHelloFlight::begin(client_parameters, client_credential(), &mut client_rng)
            .unwrap_or_else(|error| panic!("client handshake begin failed: {error:?}"));
    let parsed_client_hello = ClientHello::from_record(decode_one(client_hello_flight.remaining()))
        .unwrap_or_else(|error| panic!("client emitted invalid hello: {error:?}"));
    let client_hello_length = client_hello_flight.remaining().len();
    client_hello_flight.advance(client_hello_length).unwrap();
    let awaiting_server_hello = client_hello_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete client hello did not advance typestate"));

    let mut server_rng = FixedRng::new(server_nonce);
    let mut server_hello_flight = ServerHelloFlight::begin(
        parsed_client_hello,
        ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
        server_parameters,
        epochs,
        &mut server_rng,
    )
    .unwrap_or_else(|error| panic!("server handshake begin failed: {error:?}"));
    let server_hello = ServerHello::from_record(decode_one(server_hello_flight.remaining()))
        .unwrap_or_else(|error| panic!("server emitted invalid hello: {error:?}"));
    let awaiting_server_proof = awaiting_server_hello
        .accept(decode_one(server_hello_flight.remaining()))
        .unwrap_or_else(|error| panic!("client rejected valid server hello: {error:?}"));
    let schedule = derive(&PSK, &parsed_client_hello, &server_hello);

    let server_hello_length = server_hello_flight.remaining().len();
    server_hello_flight.advance(server_hello_length).unwrap();
    let mut server_proof_flight = server_hello_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete server hello did not advance typestate"));
    let mut client_proof_flight = awaiting_server_proof
        .verify(decode_one(server_proof_flight.remaining()))
        .unwrap_or_else(|error| panic!("client rejected valid server proof: {error:?}"));

    let server_proof_length = server_proof_flight.remaining().len();
    server_proof_flight.advance(server_proof_length).unwrap();
    let pending_client_proof = server_proof_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete server proof did not advance typestate"));
    let client_proof_record = decode_one(client_proof_flight.remaining());
    let client_proof_length = client_proof_flight.remaining().len();
    client_proof_flight.advance(client_proof_length).unwrap();
    let client_session = client_proof_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete client proof did not establish session"));
    let server_session = pending_client_proof
        .authenticate(client_proof_record)
        .unwrap_or_else(|error| panic!("server rejected valid client proof: {error:?}"));

    (client_session, server_session, schedule)
}

fn send_client_request(
    client: ClientSession,
    server: ServerSession,
    payload: &[u8],
) -> (AwaitingResponse, AwaitingReply) {
    let mut flight = match client.frame_request(owned_message(payload)) {
        Ok(flight) => flight,
        Err(fault) => panic!("client request framing failed: {:?}", fault.kind()),
    };
    let request_record = decode_one(flight.remaining());
    let request_length = flight.remaining().len();
    flight.advance(request_length).unwrap();
    let awaiting_response = flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete request did not advance typestate"));
    let authenticated = server
        .authenticate_request(request_record)
        .unwrap_or_else(|error| panic!("server rejected valid client request: {error:?}"));
    let (request, awaiting_reply) = authenticated.into_parts();
    assert_eq!(request.message().encoded(), payload);
    (awaiting_response, awaiting_reply)
}

fn frame_server_response(waiting: AwaitingReply, payload: &[u8]) -> ReplyFlight {
    let key = waiting.request_key();
    match waiting.frame_reply(LocalApiReply::new(key, owned_message(payload))) {
        Ok(flight) => flight,
        Err(fault) => panic!("server response framing failed: {:?}", fault.kind()),
    }
}

fn pending_server_response(
    request: &[u8],
    response: &[u8],
) -> (AwaitingResponse, ReplyFlight, KeySchedule) {
    let (client, server, schedule) = establish_client_server();
    let (awaiting, waiting) = send_client_request(client, server, request);
    let flight = frame_server_response(waiting, response);
    (awaiting, flight, schedule)
}

#[test]
fn client_and_server_hello_round_trip_canonical_records() {
    let hello = client_hello();
    let parsed = ClientHello::from_record(hello.into_record()).unwrap();
    assert_eq!(parsed, hello);
    assert_eq!(parsed.suite(), SessionSuite::UsbQualification);

    let (_, _, server_hello, _) = establish();
    assert_eq!(server_hello.suite(), SessionSuite::UsbQualification);
    assert_eq!(server_hello.bearer(), BearerBinding::UsbSerialJtag);
    assert_eq!(server_hello.device_id(), DEVICE_ID);
    assert_eq!(server_hello.nonce(), &SERVER_NONCE);
    assert_eq!(server_hello.credential_generation(), GENERATION);
}

#[test]
fn wifi_suite_authenticates_round_trip_and_reconnect_resets_sequences() {
    let client_parameters = ClientParameters::new_for_suite(
        DEVICE_ID,
        BearerBinding::Wifi,
        SessionSuite::WifiQualification,
    );
    let server_parameters = ServerParameters::new_for_suite(
        DEVICE_ID,
        BearerBinding::Wifi,
        SessionSuite::WifiQualification,
    );
    assert_eq!(
        SessionSuite::WifiQualification.required_bearer(),
        BearerBinding::Wifi
    );
    assert_eq!(client_parameters.suite(), SessionSuite::WifiQualification);
    assert_eq!(client_parameters.bearer(), BearerBinding::Wifi);
    assert_eq!(server_parameters.suite(), SessionSuite::WifiQualification);
    assert_eq!(server_parameters.bearer(), BearerBinding::Wifi);

    let mut epochs = SessionEpochAllocator::new();
    let (client, server, first_schedule) = establish_client_server_for(
        client_parameters,
        server_parameters,
        &mut epochs,
        CLIENT_NONCE,
        SERVER_NONCE,
    );
    assert_eq!(client.next_client_sequence(), 0);
    assert_eq!(client.next_server_sequence(), 0);
    assert_eq!(server.next_client_sequence(), 0);
    assert_eq!(server.next_server_sequence(), 0);
    assert_eq!(server.epoch(), SessionEpoch::new(1));

    let mut request_flight = client
        .frame_request(owned_message(b"wifi-request"))
        .unwrap_or_else(|fault| panic!("Wi-Fi request framing failed: {:?}", fault.kind()));
    let request_record = decode_one(request_flight.remaining());
    let request_length = request_flight.remaining().len();
    request_flight.advance(request_length).unwrap();
    let awaiting_response = request_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete Wi-Fi request did not advance typestate"));
    let authenticated = server
        .authenticate_request(request_record)
        .unwrap_or_else(|fault| panic!("Wi-Fi request authentication failed: {fault:?}"));
    let (request, waiting) = authenticated.into_parts();
    assert_eq!(request.message().encoded(), b"wifi-request");
    assert_eq!(request.grant().bearer(), BearerBinding::Wifi);
    assert_eq!(request.grant().admission_sequence(), 0);
    assert_eq!(request.grant().epoch(), SessionEpoch::new(1));

    let mut response_flight = frame_server_response(waiting, b"wifi-response");
    let response = awaiting_response
        .authenticate(decode_one(response_flight.remaining()))
        .unwrap_or_else(|fault| panic!("Wi-Fi response authentication failed: {fault:?}"));
    let (client, response) = response.into_parts();
    assert_eq!(response.encoded(), b"wifi-response");
    let response_length = response_flight.remaining().len();
    response_flight.advance(response_length).unwrap();
    let server = response_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete Wi-Fi response did not restore server session"));
    assert_eq!(client.next_client_sequence(), 1);
    assert_eq!(client.next_server_sequence(), 1);
    assert_eq!(server.next_client_sequence(), 1);
    assert_eq!(server.next_server_sequence(), 1);
    drop(client);
    drop(server);

    let mut reconnect_client_nonce = CLIENT_NONCE;
    reconnect_client_nonce[0] ^= 0x80;
    let mut reconnect_server_nonce = SERVER_NONCE;
    reconnect_server_nonce[0] ^= 0x80;
    let (client, server, reconnect_schedule) = establish_client_server_for(
        client_parameters,
        server_parameters,
        &mut epochs,
        reconnect_client_nonce,
        reconnect_server_nonce,
    );
    assert_ne!(reconnect_schedule.session_id, first_schedule.session_id);
    assert_eq!(client.next_client_sequence(), 0);
    assert_eq!(client.next_server_sequence(), 0);
    assert_eq!(server.next_client_sequence(), 0);
    assert_eq!(server.next_server_sequence(), 0);
    assert_eq!(server.epoch(), SessionEpoch::new(2));
}

#[test]
fn ble_gatt_suite_authenticates_request_response_round_trip() {
    let client_parameters = ClientParameters::new_for_suite(
        DEVICE_ID,
        BearerBinding::BleGatt,
        SessionSuite::BleGattQualification,
    );
    let server_parameters = ServerParameters::new_for_suite(
        DEVICE_ID,
        BearerBinding::BleGatt,
        SessionSuite::BleGattQualification,
    );
    assert_eq!(crate::BLE_GATT_QUALIFICATION_SUITE, 3);
    assert_eq!(
        SessionSuite::BleGattQualification.required_bearer(),
        BearerBinding::BleGatt
    );

    let mut epochs = SessionEpochAllocator::new();
    let (client, server, _) = establish_client_server_for(
        client_parameters,
        server_parameters,
        &mut epochs,
        CLIENT_NONCE,
        SERVER_NONCE,
    );
    let (awaiting_response, waiting) = send_client_request(client, server, b"ble-gatt-request");

    let mut response_flight = frame_server_response(waiting, b"ble-gatt-response");
    let response = awaiting_response
        .authenticate(decode_one(response_flight.remaining()))
        .unwrap_or_else(|fault| panic!("BLE GATT response authentication failed: {fault:?}"));
    let (client, response) = response.into_parts();
    assert_eq!(response.encoded(), b"ble-gatt-response");
    let response_length = response_flight.remaining().len();
    response_flight.advance(response_length).unwrap();
    let server = response_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete BLE GATT response did not restore server session"));
    assert_eq!(client.next_client_sequence(), 1);
    assert_eq!(client.next_server_sequence(), 1);
    assert_eq!(server.next_client_sequence(), 1);
    assert_eq!(server.next_server_sequence(), 1);
}

#[test]
fn ble_gatt_suite_rejects_cross_bearer_use() {
    let mut client_rng = FixedRng::new(CLIENT_NONCE);
    let error = expect_error(
        ClientHelloFlight::begin(
            ClientParameters::new_for_suite(
                DEVICE_ID,
                BearerBinding::Wifi,
                SessionSuite::BleGattQualification,
            ),
            client_credential(),
            &mut client_rng,
        ),
        "BLE GATT suite started on Wi-Fi",
    );
    assert!(matches!(
        error,
        ClientHandshakeError::SuiteBearerMismatch {
            suite: SessionSuite::BleGattQualification,
            bearer: BearerBinding::Wifi,
            required: BearerBinding::BleGatt,
        }
    ));

    let mut epochs = SessionEpochAllocator::new();
    let mut server_rng = FixedRng::new(SERVER_NONCE);
    let ble_hello = ClientHello::new_for_suite(
        SessionSuite::BleGattQualification,
        BearerBinding::BleGatt,
        CREDENTIAL_ID,
        CLIENT_NONCE,
    );
    let error = expect_error(
        ServerHelloFlight::begin(
            ble_hello,
            ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            ServerParameters::new_for_suite(
                DEVICE_ID,
                BearerBinding::Wifi,
                SessionSuite::BleGattQualification,
            ),
            &mut epochs,
            &mut server_rng,
        ),
        "BLE GATT suite server started on Wi-Fi",
    );
    assert!(matches!(
        error,
        HandshakeError::SuiteBearerMismatch {
            suite: SessionSuite::BleGattQualification,
            bearer: BearerBinding::Wifi,
            required: BearerBinding::BleGatt,
        }
    ));

    let forged_wifi_hello = ClientHello::new_for_suite(
        SessionSuite::BleGattQualification,
        BearerBinding::Wifi,
        CREDENTIAL_ID,
        CLIENT_NONCE,
    );
    let error = expect_error(
        ServerHelloFlight::begin(
            forged_wifi_hello,
            ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            ServerParameters::new_for_suite(
                DEVICE_ID,
                BearerBinding::BleGatt,
                SessionSuite::BleGattQualification,
            ),
            &mut epochs,
            &mut server_rng,
        ),
        "BLE GATT suite accepted a client hello from Wi-Fi",
    );
    assert!(matches!(
        error,
        HandshakeError::BearerMismatch {
            client: BearerBinding::Wifi,
            server: BearerBinding::BleGatt,
        }
    ));
}

#[test]
fn wifi_suite_rejects_wrong_bearer_and_supported_suite() {
    let mut client_rng = FixedRng::new(CLIENT_NONCE);
    let error = expect_error(
        ClientHelloFlight::begin(
            ClientParameters::new(DEVICE_ID, BearerBinding::Wifi),
            client_credential(),
            &mut client_rng,
        ),
        "legacy USB qualification suite started on Wi-Fi",
    );
    assert!(matches!(
        error,
        ClientHandshakeError::QualificationSuiteForbidden {
            bearer: BearerBinding::Wifi,
        }
    ));

    let mut server_rng = FixedRng::new(SERVER_NONCE);
    let mut epochs = SessionEpochAllocator::new();
    let error = expect_error(
        ServerHelloFlight::begin(
            client_hello(),
            ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            ServerParameters::new(DEVICE_ID, BearerBinding::Wifi),
            &mut epochs,
            &mut server_rng,
        ),
        "legacy USB qualification server started on Wi-Fi",
    );
    assert!(matches!(
        error,
        HandshakeError::QualificationSuiteForbidden {
            bearer: BearerBinding::Wifi,
        }
    ));

    let mut client_rng = FixedRng::new(CLIENT_NONCE);
    let error = expect_error(
        ClientHelloFlight::begin(
            ClientParameters::new_for_suite(
                DEVICE_ID,
                BearerBinding::UsbSerialJtag,
                SessionSuite::WifiQualification,
            ),
            client_credential(),
            &mut client_rng,
        ),
        "Wi-Fi suite started on USB",
    );
    assert!(matches!(
        error,
        ClientHandshakeError::SuiteBearerMismatch {
            suite: SessionSuite::WifiQualification,
            bearer: BearerBinding::UsbSerialJtag,
            required: BearerBinding::Wifi,
        }
    ));

    let wifi_hello = ClientHello::new_for_suite(
        SessionSuite::WifiQualification,
        BearerBinding::Wifi,
        CREDENTIAL_ID,
        CLIENT_NONCE,
    );
    let mut server_rng = FixedRng::new(SERVER_NONCE);
    let error = expect_error(
        ServerHelloFlight::begin(
            wifi_hello,
            ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            ServerParameters::new_for_suite(
                DEVICE_ID,
                BearerBinding::UsbSerialJtag,
                SessionSuite::WifiQualification,
            ),
            &mut epochs,
            &mut server_rng,
        ),
        "Wi-Fi suite server started on USB",
    );
    assert!(matches!(
        error,
        HandshakeError::SuiteBearerMismatch {
            suite: SessionSuite::WifiQualification,
            bearer: BearerBinding::UsbSerialJtag,
            required: BearerBinding::Wifi,
        }
    ));

    let mut server_rng = FixedRng::new(SERVER_NONCE);
    let error = expect_error(
        ServerHelloFlight::begin(
            wifi_hello,
            ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
            &mut epochs,
            &mut server_rng,
        ),
        "Wi-Fi client hello was accepted by the USB suite",
    );
    assert!(matches!(
        error,
        HandshakeError::SuiteMismatch {
            client: SessionSuite::WifiQualification,
            server: SessionSuite::UsbQualification,
        }
    ));

    let wifi_hello_on_ble = ClientHello::new_for_suite(
        SessionSuite::WifiQualification,
        BearerBinding::BleGatt,
        CREDENTIAL_ID,
        CLIENT_NONCE,
    );
    let mut server_rng = FixedRng::new(SERVER_NONCE);
    let error = expect_error(
        ServerHelloFlight::begin(
            wifi_hello_on_ble,
            ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
            ServerParameters::new_for_suite(
                DEVICE_ID,
                BearerBinding::Wifi,
                SessionSuite::WifiQualification,
            ),
            &mut epochs,
            &mut server_rng,
        ),
        "Wi-Fi suite accepted a client hello from BLE",
    );
    assert!(matches!(
        error,
        HandshakeError::BearerMismatch {
            client: BearerBinding::BleGatt,
            server: BearerBinding::Wifi,
        }
    ));

    let mut client_rng = FixedRng::new(CLIENT_NONCE);
    let mut client_flight = ClientHelloFlight::begin(
        ClientParameters::new_for_suite(
            DEVICE_ID,
            BearerBinding::Wifi,
            SessionSuite::WifiQualification,
        ),
        client_credential(),
        &mut client_rng,
    )
    .unwrap();
    let client_length = client_flight.remaining().len();
    client_flight.advance(client_length).unwrap();
    let awaiting_server_hello = client_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete Wi-Fi client hello did not advance typestate"));
    let usb_server_hello = decode_one(begin().remaining());
    let error = expect_error(
        awaiting_server_hello.accept(usb_server_hello),
        "Wi-Fi client accepted a USB-suite server hello",
    );
    assert!(matches!(
        error,
        ClientHandshakeError::SuiteMismatch {
            expected: SessionSuite::WifiQualification,
            observed: SessionSuite::UsbQualification,
        }
    ));
}

#[test]
fn complete_handshake_request_handoff_and_partial_reply_are_exact() {
    let (session, schedule, _, _) = establish();
    let request_payload = [0xa4, 0x00, 0x01, 0x01, 0x07];
    let authenticated = session
        .authenticate_request(tagged_request(&schedule, 0, &request_payload))
        .unwrap_or_else(|error| panic!("request auth failed: {error:?}"));
    let (request, waiting) = authenticated.into_parts();
    assert_eq!(
        request.key(),
        RequestKey::new(EPOCH, request.key().correlation())
    );
    assert_eq!(request.key().correlation().get(), 0);
    assert_eq!(request.message().encoded(), request_payload);
    assert_eq!(request.grant().credential_id(), CREDENTIAL_ID);
    assert_eq!(request.grant().credential_generation(), GENERATION);
    assert_eq!(request.grant().bearer(), BearerBinding::UsbSerialJtag);
    assert_eq!(request.grant().session_id(), schedule.session_id);
    assert_eq!(request.grant().epoch(), EPOCH);
    assert_eq!(request.grant().admission_sequence(), 0);

    let reply_payload = [0xa4, 0x00, 0x01, 0x01, 0x07, 0x02, 0x01];
    let mut reply_owner = [0_u8; PAYLOAD_CAPACITY];
    reply_owner[..reply_payload.len()].copy_from_slice(&reply_payload);
    let reply = LocalApiReply::new(
        request.key(),
        OwnedMessage::new(
            MessageLength::new(reply_payload.len()).unwrap(),
            reply_owner,
        ),
    );
    let mut flight = waiting
        .frame_reply(reply)
        .unwrap_or_else(|fault| panic!("reply route failed: {:?}", fault.kind()));
    let response = decode_one(flight.remaining());
    assert_eq!(response.kind(), RECORD_KIND_RESPONSE);
    assert_eq!(response.session_id(), schedule.session_id.as_bytes());
    assert_eq!(response.sequence(), 0);
    assert_eq!(response.payload(), reply_payload);
    assert!(verify_server_record_tag(
        &schedule.server_record_key,
        &response
    ));

    let first = 3;
    flight.advance(first).unwrap();
    assert!(flight.try_finish().is_err());

    let (session, schedule, _, _) = establish();
    let authenticated = session
        .authenticate_request(tagged_request(&schedule, 0, &request_payload))
        .unwrap();
    let (request, waiting) = authenticated.into_parts();
    let mut owner = [0_u8; PAYLOAD_CAPACITY];
    owner[..reply_payload.len()].copy_from_slice(&reply_payload);
    let reply = LocalApiReply::new(
        request.key(),
        OwnedMessage::new(MessageLength::new(reply_payload.len()).unwrap(), owner),
    );
    let mut flight = match waiting.frame_reply(reply) {
        Ok(flight) => flight,
        Err(fault) => panic!("reply route failed: {:?}", fault.kind()),
    };
    let length = flight.remaining().len();
    flight.advance(length).unwrap();
    let idle = match flight.try_finish() {
        Ok(session) => session,
        Err(_) => panic!("complete reply did not return idle session"),
    };
    assert_eq!(idle.next_client_sequence(), 1);
    assert_eq!(idle.next_server_sequence(), 1);
}

#[test]
fn handshake_rejects_wrong_bearer_credential_and_client_proof() {
    let mut rng = FixedRng::new(SERVER_NONCE);
    let mut epochs = SessionEpochAllocator::new();
    let error = match ServerHelloFlight::begin(
        ClientHello::new(BearerBinding::BleGatt, CREDENTIAL_ID, CLIENT_NONCE),
        ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
        ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
        &mut epochs,
        &mut rng,
    ) {
        Ok(_) => panic!("bearer mismatch was accepted"),
        Err(error) => error,
    };
    assert!(matches!(error, HandshakeError::BearerMismatch { .. }));

    let mut rng = FixedRng::new(SERVER_NONCE);
    let mut epochs = SessionEpochAllocator::new();
    let error = match ServerHelloFlight::begin(
        client_hello(),
        ActiveCredential::new(CredentialId::new([0x99; 16]), GENERATION, PSK),
        ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
        &mut epochs,
        &mut rng,
    ) {
        Ok(_) => panic!("credential mismatch was accepted"),
        Err(error) => error,
    };
    assert!(matches!(error, HandshakeError::AuthenticationFailed));

    let mut hello = begin();
    let length = hello.remaining().len();
    hello.advance(length).unwrap();
    let mut proof = match hello.try_finish() {
        Ok(proof) => proof,
        Err(_) => panic!("hello did not complete"),
    };
    let length = proof.remaining().len();
    proof.advance(length).unwrap();
    let pending = match proof.try_finish() {
        Ok(pending) => pending,
        Err(_) => panic!("proof did not complete"),
    };
    let bad = proof_record(RECORD_KIND_CLIENT_PROOF, pending.session_id(), &[0x55; 32]);
    assert!(matches!(
        pending.authenticate(bad),
        Err(HandshakeError::AuthenticationFailed)
    ));
}

#[test]
fn established_faults_are_terminal_and_exact() {
    let (session, schedule, _, _) = establish();
    let error = match session.authenticate_request(tagged_request(&schedule, 1, b"gap")) {
        Ok(_) => panic!("sequence gap was accepted"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        SessionFault::UnexpectedSequence {
            expected: 0,
            observed: 1
        }
    );

    let (session, schedule, _, _) = establish();
    let mut bad_tag = tagged_request(&schedule, 0, b"tag");
    let (kind, session_id, sequence, length, payload, mut tag) = bad_tag.into_parts();
    tag[0] ^= 0x80;
    bad_tag = Record::new(kind, session_id, sequence, length, payload, tag);
    let error = match session.authenticate_request(bad_tag) {
        Ok(_) => panic!("bad tag was accepted"),
        Err(error) => error,
    };
    assert_eq!(error, SessionFault::BadTag);

    let (session, schedule, _, _) = establish();
    let request = tagged_request(&schedule, 0, b"kind");
    let (_, session_id, sequence, length, payload, _) = request.into_parts();
    let wrong_kind = Record::new(
        RECORD_KIND_RESPONSE,
        session_id,
        sequence,
        length,
        payload,
        [0; AUTH_TAG_LENGTH],
    );
    let error = match session.authenticate_request(wrong_kind) {
        Ok(_) => panic!("response kind was accepted as a request"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        SessionFault::UnexpectedKind {
            observed: RECORD_KIND_RESPONSE
        }
    );

    let (session, schedule, _, _) = establish();
    let request = tagged_request(&schedule, 0, b"session");
    let (kind, mut session_id, sequence, length, payload, tag) = request.into_parts();
    session_id[0] ^= 1;
    let wrong_session = Record::new(kind, session_id, sequence, length, payload, tag);
    let error = match session.authenticate_request(wrong_session) {
        Ok(_) => panic!("wrong session ID was accepted"),
        Err(error) => error,
    };
    assert_eq!(error, SessionFault::WrongSession);
}

#[test]
fn preauth_downgrade_reserved_and_tag_mutations_fail_closed() {
    let record = client_hello().into_record();
    let (kind, session_id, sequence, length, mut payload, tag) = record.into_parts();
    payload[0] = 2;
    let error = ClientHello::from_record(Record::new(
        kind, session_id, sequence, length, payload, tag,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        crate::HandshakeRecordError::UnsupportedVersion { .. }
    ));

    let record = client_hello().into_record();
    let (kind, session_id, sequence, length, mut payload, tag) = record.into_parts();
    payload[4] = 4;
    let error = ClientHello::from_record(Record::new(
        kind, session_id, sequence, length, payload, tag,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        crate::HandshakeRecordError::UnsupportedSuite { suite: 4 }
    ));

    let record = client_hello().into_record();
    let (kind, session_id, sequence, length, mut payload, tag) = record.into_parts();
    payload[7] = 1;
    let error = ClientHello::from_record(Record::new(
        kind, session_id, sequence, length, payload, tag,
    ))
    .unwrap_err();
    assert_eq!(error, crate::HandshakeRecordError::ReservedBitsSet);

    let record = client_hello().into_record();
    let (kind, session_id, sequence, length, payload, mut tag) = record.into_parts();
    tag[0] = 1;
    let error = ClientHello::from_record(Record::new(
        kind, session_id, sequence, length, payload, tag,
    ))
    .unwrap_err();
    assert_eq!(error, crate::HandshakeRecordError::NonzeroTag);
}

#[test]
fn generation_and_reset_nonce_invalidate_old_client_proofs() {
    let (old_pending, old_schedule, _, old_server_proof) =
        complete_server_flight(begin_with(GENERATION, SERVER_NONCE));
    let old_client_proof = client_proof(&old_schedule, &old_server_proof);
    drop(old_pending);

    let next_generation = CredentialGeneration::new(GENERATION.get() + 1);
    let (new_pending, new_schedule, _, _) =
        complete_server_flight(begin_with(next_generation, SERVER_NONCE));
    assert_ne!(new_schedule.session_id, old_schedule.session_id);
    let replay = proof_record(
        RECORD_KIND_CLIENT_PROOF,
        new_schedule.session_id,
        &old_client_proof,
    );
    assert!(matches!(
        new_pending.authenticate(replay),
        Err(HandshakeError::AuthenticationFailed)
    ));

    let mut reboot_nonce = SERVER_NONCE;
    reboot_nonce[0] ^= 0x80;
    let (reset_pending, reset_schedule, _, _) =
        complete_server_flight(begin_with(GENERATION, reboot_nonce));
    assert_ne!(reset_schedule.session_id, old_schedule.session_id);
    let replay = proof_record(
        RECORD_KIND_CLIENT_PROOF,
        reset_schedule.session_id,
        &old_client_proof,
    );
    assert!(matches!(
        reset_pending.authenticate(replay),
        Err(HandshakeError::AuthenticationFailed)
    ));
}

#[test]
fn reflection_duplicate_and_sequence_exhaustion_terminate() {
    let (session, schedule, _, _) = establish();
    let request = tagged_request(&schedule, 0, b"reflection");
    let (kind, session_id, sequence, length, payload, _) = request.into_parts();
    let untagged = Record::new(
        kind,
        session_id,
        sequence,
        length,
        payload,
        [0; AUTH_TAG_LENGTH],
    );
    let reflected = server_record_tag(&schedule.server_record_key, &untagged);
    let (kind, session_id, sequence, length, payload, _) = untagged.into_parts();
    let reflected = Record::new(kind, session_id, sequence, length, payload, reflected);
    let error = match session.authenticate_request(reflected) {
        Ok(_) => panic!("reflected server tag was accepted in the client direction"),
        Err(error) => error,
    };
    assert_eq!(error, SessionFault::BadTag);

    let (session, schedule, _, _) = establish();
    let authenticated = session
        .authenticate_request(tagged_request(&schedule, 0, b"first"))
        .unwrap();
    let (request, waiting) = authenticated.into_parts();
    let reply = LocalApiReply::new(
        request.key(),
        OwnedMessage::new(MessageLength::new(0).unwrap(), [0; PAYLOAD_CAPACITY]),
    );
    let mut flight = match waiting.frame_reply(reply) {
        Ok(flight) => flight,
        Err(fault) => panic!("matching reply failed: {:?}", fault.kind()),
    };
    let length = flight.remaining().len();
    flight.advance(length).unwrap();
    let idle = match flight.try_finish() {
        Ok(idle) => idle,
        Err(_) => panic!("complete reply did not restore idle session"),
    };
    let error = match idle.authenticate_request(tagged_request(&schedule, 0, b"duplicate")) {
        Ok(_) => panic!("duplicate sequence was accepted"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        SessionFault::UnexpectedSequence {
            expected: 1,
            observed: 0
        }
    );

    let (mut session, schedule, _, _) = establish();
    session.set_sequences_for_test(u64::MAX, 0);
    let error =
        match session.authenticate_request(tagged_request(&schedule, u64::MAX, b"exhausted")) {
            Ok(_) => panic!("exhausted client sequence was accepted"),
            Err(error) => error,
        };
    assert_eq!(error, SessionFault::SequenceExhausted);

    let (mut session, schedule, _, _) = establish();
    session.set_sequences_for_test(0, u64::MAX);
    let authenticated = session
        .authenticate_request(tagged_request(&schedule, 0, b"reply-exhausted"))
        .unwrap();
    let (request, waiting) = authenticated.into_parts();
    let reply = LocalApiReply::new(
        request.key(),
        OwnedMessage::new(MessageLength::new(0).unwrap(), [0; PAYLOAD_CAPACITY]),
    );
    let fault = match waiting.frame_reply(reply) {
        Ok(_) => panic!("exhausted server sequence was accepted"),
        Err(fault) => fault,
    };
    assert_eq!(fault.kind(), ReplyRouteFaultKind::SequenceExhausted);
}

#[test]
fn crossed_or_stale_reply_is_returned_and_session_is_not_reused() {
    let (session, schedule, _, _) = establish();
    let authenticated = session
        .authenticate_request(tagged_request(&schedule, 0, b"request"))
        .unwrap();
    let (request, waiting) = authenticated.into_parts();
    let reply = LocalApiReply::new(
        RequestKey::new(SessionEpoch::new(8), request.key().correlation()),
        OwnedMessage::new(MessageLength::new(0).unwrap(), [0; PAYLOAD_CAPACITY]),
    );
    let fault = match waiting.frame_reply(reply) {
        Ok(_) => panic!("stale reply was accepted"),
        Err(fault) => fault,
    };
    assert_eq!(fault.kind(), ReplyRouteFaultKind::WrongEpoch);
    assert_eq!(fault.into_reply().key().epoch(), SessionEpoch::new(8));

    let (session, schedule, _, _) = establish();
    let authenticated = session
        .authenticate_request(tagged_request(&schedule, 0, b"request"))
        .unwrap();
    let (request, waiting) = authenticated.into_parts();
    let reply = LocalApiReply::new(
        RequestKey::new(EPOCH, CorrelationId::new(1)),
        OwnedMessage::new(MessageLength::new(0).unwrap(), [0; PAYLOAD_CAPACITY]),
    );
    let fault = match waiting.frame_reply(reply) {
        Ok(_) => panic!("wrong correlation was accepted"),
        Err(fault) => fault,
    };
    assert_eq!(fault.kind(), ReplyRouteFaultKind::WrongCorrelation);
    assert_eq!(fault.into_reply().key().epoch(), request.key().epoch());
}

#[test]
fn boot_lifetime_epoch_allocator_prevents_old_reply_alias_after_reconnect() {
    let mut epochs = SessionEpochAllocator::new();
    let (old_session, old_schedule, _, _) = establish_with_allocator(&mut epochs, SERVER_NONCE);
    let old_authenticated = old_session
        .authenticate_request(tagged_request(&old_schedule, 0, b"old"))
        .unwrap();
    let (old_request, old_waiting) = old_authenticated.into_parts();
    assert_eq!(old_request.key().epoch(), SessionEpoch::new(1));
    let old_reply = LocalApiReply::new(
        old_request.key(),
        OwnedMessage::new(MessageLength::new(0).unwrap(), [0; PAYLOAD_CAPACITY]),
    );
    drop(old_waiting);

    let mut new_nonce = SERVER_NONCE;
    new_nonce[0] ^= 0x80;
    let (new_session, new_schedule, _, _) = establish_with_allocator(&mut epochs, new_nonce);
    let new_authenticated = new_session
        .authenticate_request(tagged_request(&new_schedule, 0, b"new"))
        .unwrap();
    let (new_request, new_waiting) = new_authenticated.into_parts();
    assert_eq!(new_request.key().epoch(), SessionEpoch::new(2));
    assert_eq!(new_request.key().correlation().get(), 0);

    let fault = match new_waiting.frame_reply(old_reply) {
        Ok(_) => panic!("old reply aliased the new session's first correlation"),
        Err(fault) => fault,
    };
    assert_eq!(fault.kind(), ReplyRouteFaultKind::WrongEpoch);
    assert_eq!(fault.into_reply().key(), old_request.key());
}

#[test]
fn authority_grant_supplies_adapter_context_and_revoked_grant_does_not_revalidate() {
    let permissions =
        Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA | Permissions::READ_SUBMISSION_STATUS;
    let authority = credential_authority(GENERATION, permissions);
    let (session, schedule) = establish_from_authority(&authority);

    let envelope = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(77),
        request: DeviceRequest::SubmitRnsData {
            destination: DestinationHash([0x44; 16]),
            payload: b"credential-backed request",
            idempotency_key: IdempotencyKey([0x55; 16]),
        },
    };
    let mut encoded_request = [0_u8; PAYLOAD_CAPACITY];
    let request_length = encode_request(&envelope, &mut encoded_request)
        .unwrap_or_else(|error| panic!("logical request encode failed: {error:?}"));
    let authenticated = session
        .authenticate_request(tagged_request(
            &schedule,
            0,
            &encoded_request[..request_length],
        ))
        .unwrap_or_else(|error| panic!("request authentication failed: {error:?}"));
    let (request, waiting) = authenticated.into_parts();
    let decoded = decode_request(request.message().encoded())
        .unwrap_or_else(|error| panic!("logical request decode failed: {error:?}"));
    let lease = request
        .grant()
        .revalidate(&authority)
        .unwrap_or_else(|_| panic!("session grant did not revalidate"));
    assert_eq!(lease.credential_id(), CREDENTIAL_ID);
    assert_eq!(lease.generation(), GENERATION);
    lease.with_dispatch_context(|context| {
        assert_eq!(context.principal(), Some(PRINCIPAL));
        assert_eq!(context.permissions(), permissions);
    });

    let mut port = CountingPort::default();
    let response = lease.with_dispatch_context(|context| {
        dispatch(
            &mut port,
            IdentitySummary::new(DestinationHash([0x88; 16])),
            context,
            decoded,
        )
    });
    assert_eq!(port.availability_calls, 1);
    assert_eq!(port.status_calls, 0);
    assert_eq!(port.acceptance_calls, 1);
    let candidate = port
        .last_candidate
        .expect("the authorized adapter call supplies one durable candidate");
    assert_eq!(candidate.principal(), StoragePrincipalId::new(PRINCIPAL.0));
    let durable_authorization = candidate.authorization();
    assert_eq!(
        durable_authorization.credential_id(),
        CREDENTIAL_ID.as_bytes()
    );
    assert_eq!(
        durable_authorization.credential_generation(),
        GENERATION.get()
    );
    assert_eq!(durable_authorization.authority_revision(), GENERATION.get());
    assert_eq!(durable_authorization.policy_version(), 1);
    assert_eq!(
        durable_authorization.granted_permission_bits(),
        permissions.bits()
    );
    assert!(matches!(
        &response.response,
        DeviceResponse::SubmitRnsDataAccepted(_)
    ));

    let mut encoded_response = [0_u8; PAYLOAD_CAPACITY];
    let response_length = encode_response(&response, &mut encoded_response)
        .unwrap_or_else(|error| panic!("logical response encode failed: {error:?}"));
    let mut response_owner = [0_u8; PAYLOAD_CAPACITY];
    response_owner[..response_length].copy_from_slice(&encoded_response[..response_length]);
    let reply = LocalApiReply::new(
        request.key(),
        OwnedMessage::new(MessageLength::new(response_length).unwrap(), response_owner),
    );
    let flight = waiting
        .frame_reply(reply)
        .unwrap_or_else(|fault| panic!("response framing failed: {:?}", fault.kind()));
    let response_record = decode_one(flight.remaining());
    assert!(verify_server_record_tag(
        &schedule.server_record_key,
        &response_record
    ));
    let decoded_response = decode_response(response_record.payload())
        .unwrap_or_else(|error| panic!("logical response decode failed: {error:?}"));
    assert_eq!(decoded_response, response);

    let (session, schedule) = establish_from_authority(&authority);
    let authenticated = session
        .authenticate_request(tagged_request(
            &schedule,
            0,
            &encoded_request[..request_length],
        ))
        .unwrap();
    let (queued_request, queued_waiting) = authenticated.into_parts();
    assert!(decode_request(queued_request.message().encoded()).is_ok());

    let revoked_generation = CredentialGeneration::new(GENERATION.get() + 1);
    let revoked = revoked_authority(revoked_generation);
    let rejected = queued_request.grant().revalidate(&revoked);
    assert!(rejected.is_err());
    // The future composed owner must treat this as terminal and prove that it
    // neither falls back to an unauthenticated context nor invokes its port.
    drop(queued_waiting);
}

#[test]
fn deterministic_material_matches_independent_python_vector() {
    let client_hello_record = client_hello().into_record();
    assert_record_vector(
        &client_hello_record,
        "524441310101000000000000000000000000000000000000000000000000000038000100000001000100101112131415161718191a1b1c1d1e1f404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f00000000000000000000000000000000",
        "0007524441310101010101010101010101010101010101010101010101010101010238020101010201020131101112131415161718191a1b1c1d1e1f404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0101010101010101010101010101010100",
    );

    let mut hello_flight = begin();
    let server_hello_wire = "000752444131010201010101010101010101010101010101010101010101010101024c020101010201020139202122232425262728292a2b2c2d2e2f606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f08070605040302010202030201010102070101010101010101010101010101010101010100";
    assert_eq!(hex::encode(hello_flight.remaining()), server_hello_wire);
    let server_hello_record = decode_one(hello_flight.remaining());
    assert_record_vector(
        &server_hello_record,
        "52444131010200000000000000000000000000000000000000000000000000004c000100000001000100202122232425262728292a2b2c2d2e2f606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f080706050403020100020002010000000700000000000000000000000000000000000000",
        server_hello_wire,
    );
    let server_hello = ServerHello::from_record(server_hello_record).unwrap();
    let schedule = derive(&PSK, &client_hello(), &server_hello);
    let hello_length = hello_flight.remaining().len();
    hello_flight.advance(hello_length).unwrap();
    let mut proof_flight = hello_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete vector server hello did not advance typestate"));
    let server_proof_wire = "0007524441310103011143500f898ee83026808be25bf16c114401010101010101022021f8dadb11a16a3ff23cb346843583ad980b5a4b22cc75c5a058b403089a8401e50101010101010101010101010101010100";
    assert_eq!(hex::encode(proof_flight.remaining()), server_proof_wire);
    let server_proof_record = decode_one(proof_flight.remaining());
    assert_record_vector(
        &server_proof_record,
        "524441310103000043500f898ee83026808be25bf16c114400000000000000002000f8dadb11a16a3ff23cb346843583ad980b5a4b22cc75c5a058b403089a8401e500000000000000000000000000000000",
        server_proof_wire,
    );
    let observed_server_proof = take_proof(
        server_proof_record,
        RECORD_KIND_SERVER_PROOF,
        schedule.session_id,
    )
    .unwrap();
    let proof_length = proof_flight.remaining().len();
    proof_flight.advance(proof_length).unwrap();
    let pending = proof_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete vector server proof did not advance typestate"));
    let observed_client_proof = client_proof(&schedule, &observed_server_proof);
    let client_proof_record = proof_record(
        RECORD_KIND_CLIENT_PROOF,
        schedule.session_id,
        &observed_client_proof,
    );
    assert_record_vector(
        &client_proof_record,
        "524441310104000043500f898ee83026808be25bf16c114400000000000000002000419b56f8245fb32647258ed4102531c606543b32cd125af94b5fff96fa21496d00000000000000000000000000000000",
        "0007524441310104011143500f898ee83026808be25bf16c114401010101010101022021419b56f8245fb32647258ed4102531c606543b32cd125af94b5fff96fa21496d0101010101010101010101010101010100",
    );
    let session = pending.authenticate(client_proof_record).unwrap();
    let record = tagged_request(&schedule, 0, b"vector-request");
    assert_eq!(
        hex::encode(schedule.transcript_hash),
        "f2c370c91c390b560e11f504bf1ccc858cf6f9b391be79652c79057cd30ef7e6"
    );
    assert_eq!(
        hex::encode(schedule.server_proof_key.as_ref()),
        "618fbaa38a75c76f9b6ee31988bdfedfea0f16124d06566f9e4162a893267a12"
    );
    assert_eq!(
        hex::encode(schedule.client_proof_key.as_ref()),
        "2e3c56ab766b7f0bd8800040f13fcbaa7bd7ef48de1be15f89a9a17aaf4d754e"
    );
    assert_eq!(
        hex::encode(schedule.client_record_key.as_ref()),
        "e7e8f2b704b39db2f6303636b003d885421b84958c8db152d3f5673924f87ffb"
    );
    assert_eq!(
        hex::encode(schedule.server_record_key.as_ref()),
        "8da612fe2c3324ecdce9bbd1049df50730000113a0cbeac9711e8bf6da573341"
    );
    assert_eq!(
        hex::encode(schedule.session_id.0),
        "43500f898ee83026808be25bf16c1144"
    );
    assert_eq!(
        hex::encode(observed_server_proof),
        "f8dadb11a16a3ff23cb346843583ad980b5a4b22cc75c5a058b403089a8401e5"
    );
    assert_eq!(
        hex::encode(observed_client_proof),
        "419b56f8245fb32647258ed4102531c606543b32cd125af94b5fff96fa21496d"
    );
    assert_eq!(
        hex::encode(record.authentication_tag()),
        "fba3e2eedff43b16b35829413cee6bc2"
    );
    assert_eq!(record.payload(), b"vector-request");
    let request_wire = FramedRecord::encode(&record).unwrap();
    assert_eq!(
        hex::encode(request_wire.encoded()),
        "0007524441310110011143500f898ee83026808be25bf16c114401010101010101020e1f766563746f722d72657175657374fba3e2eedff43b16b35829413cee6bc200"
    );

    let authenticated = session.authenticate_request(record).unwrap();
    let (request, waiting) = authenticated.into_parts();
    let mut owner = [0_u8; PAYLOAD_CAPACITY];
    owner[..b"vector-response".len()].copy_from_slice(b"vector-response");
    let reply = LocalApiReply::new(
        request.key(),
        OwnedMessage::new(MessageLength::new(b"vector-response".len()).unwrap(), owner),
    );
    let flight = match waiting.frame_reply(reply) {
        Ok(flight) => flight,
        Err(fault) => panic!("vector reply route failed: {:?}", fault.kind()),
    };
    assert_eq!(
        hex::encode(flight.remaining()),
        "0007524441310111011143500f898ee83026808be25bf16c114401010101010101020f20766563746f722d726573706f6e7365b04abfae374dcb7592292ba4c22729bf00"
    );
}

#[test]
fn public_client_typestate_matches_vectors_and_server_end_to_end() {
    let parameters = client_parameters();
    assert_eq!(parameters.expected_device_id(), DEVICE_ID);
    assert_eq!(parameters.bearer(), BearerBinding::UsbSerialJtag);
    let credential = client_credential();
    assert_eq!(credential.id(), CREDENTIAL_ID);
    assert_eq!(credential.generation(), GENERATION);

    let mut client_rng = FixedRng::new(CLIENT_NONCE);
    let client_hello_flight = ClientHelloFlight::begin(parameters, credential, &mut client_rng)
        .unwrap_or_else(|error| panic!("client handshake begin failed: {error:?}"));
    let client_hello_wire = "0007524441310101010101010101010101010101010101010101010101010101010238020101010201020131101112131415161718191a1b1c1d1e1f404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0101010101010101010101010101010100";
    assert_eq!(
        hex::encode(client_hello_flight.remaining()),
        client_hello_wire
    );
    let parsed_client_hello = ClientHello::from_record(decode_one(client_hello_flight.remaining()))
        .unwrap_or_else(|error| panic!("client emitted invalid hello: {error:?}"));
    assert_eq!(parsed_client_hello, client_hello());
    assert_eq!(
        client_hello_flight.next_chunk(7),
        &client_hello_flight.remaining()[..7]
    );
    let mut client_hello_flight = match client_hello_flight.try_finish() {
        Ok(_) => panic!("unacknowledged client hello advanced typestate"),
        Err(flight) => flight,
    };
    client_hello_flight.advance(7).unwrap();
    let mut client_hello_flight = match client_hello_flight.try_finish() {
        Ok(_) => panic!("partially acknowledged client hello advanced typestate"),
        Err(flight) => flight,
    };
    let remaining = client_hello_flight.remaining().len();
    client_hello_flight.advance(remaining).unwrap();
    let awaiting_server_hello = client_hello_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete client hello did not advance typestate"));

    let mut epochs = SessionEpochAllocator::new();
    let mut server_rng = FixedRng::new(SERVER_NONCE);
    let mut server_hello_flight = ServerHelloFlight::begin(
        parsed_client_hello,
        ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
        ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
        &mut epochs,
        &mut server_rng,
    )
    .unwrap_or_else(|error| panic!("server handshake begin failed: {error:?}"));
    let server_hello_wire = "000752444131010201010101010101010101010101010101010101010101010101024c020101010201020139202122232425262728292a2b2c2d2e2f606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f08070605040302010202030201010102070101010101010101010101010101010101010100";
    assert_eq!(
        hex::encode(server_hello_flight.remaining()),
        server_hello_wire
    );
    let server_hello = ServerHello::from_record(decode_one(server_hello_flight.remaining()))
        .unwrap_or_else(|error| panic!("server emitted invalid hello: {error:?}"));
    let awaiting_server_proof = awaiting_server_hello
        .accept(decode_one(server_hello_flight.remaining()))
        .unwrap_or_else(|error| panic!("client rejected valid server hello: {error:?}"));
    let schedule = derive(&PSK, &parsed_client_hello, &server_hello);
    let server_hello_length = server_hello_flight.remaining().len();
    server_hello_flight.advance(server_hello_length).unwrap();
    let mut server_proof_flight = server_hello_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete server hello did not advance typestate"));

    let server_proof_wire = "0007524441310103011143500f898ee83026808be25bf16c114401010101010101022021f8dadb11a16a3ff23cb346843583ad980b5a4b22cc75c5a058b403089a8401e50101010101010101010101010101010100";
    assert_eq!(
        hex::encode(server_proof_flight.remaining()),
        server_proof_wire
    );
    let client_proof_flight = awaiting_server_proof
        .verify(decode_one(server_proof_flight.remaining()))
        .unwrap_or_else(|error| panic!("client rejected valid server proof: {error:?}"));
    let server_proof_length = server_proof_flight.remaining().len();
    server_proof_flight.advance(server_proof_length).unwrap();
    let pending_client_proof = server_proof_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete server proof did not advance typestate"));

    let client_proof_wire = "0007524441310104011143500f898ee83026808be25bf16c114401010101010101022021419b56f8245fb32647258ed4102531c606543b32cd125af94b5fff96fa21496d0101010101010101010101010101010100";
    assert_eq!(
        hex::encode(client_proof_flight.remaining()),
        client_proof_wire
    );
    let client_proof_record = decode_one(client_proof_flight.remaining());
    let mut client_proof_flight = match client_proof_flight.try_finish() {
        Ok(_) => panic!("unacknowledged client proof established a session"),
        Err(flight) => flight,
    };
    let client_proof_length = client_proof_flight.remaining().len();
    client_proof_flight.advance(client_proof_length).unwrap();
    let client_session = client_proof_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete client proof did not establish a session"));
    let server_session = pending_client_proof
        .authenticate(client_proof_record)
        .unwrap_or_else(|error| panic!("server rejected valid client proof: {error:?}"));
    assert_eq!(client_session.session_id(), server_session.session_id());
    assert_eq!(client_session.session_id(), schedule.session_id);

    let request_flight = match client_session.frame_request(owned_message(b"vector-request")) {
        Ok(flight) => flight,
        Err(fault) => panic!("client request framing failed: {:?}", fault.kind()),
    };
    let request_wire = "0007524441310110011143500f898ee83026808be25bf16c114401010101010101020e1f766563746f722d72657175657374fba3e2eedff43b16b35829413cee6bc200";
    assert_eq!(hex::encode(request_flight.remaining()), request_wire);
    let request_record = decode_one(request_flight.remaining());
    let mut request_flight = match request_flight.try_finish() {
        Ok(_) => panic!("unacknowledged request entered the response state"),
        Err(flight) => flight,
    };
    request_flight.advance(5).unwrap();
    let mut request_flight = match request_flight.try_finish() {
        Ok(_) => panic!("partially acknowledged request entered the response state"),
        Err(flight) => flight,
    };
    let request_remaining = request_flight.remaining().len();
    request_flight.advance(request_remaining).unwrap();
    let awaiting_response = request_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete request did not enter the response state"));
    let authenticated = server_session
        .authenticate_request(request_record)
        .unwrap_or_else(|error| panic!("server rejected public-client request: {error:?}"));
    let (request, waiting) = authenticated.into_parts();
    assert_eq!(request.message().encoded(), b"vector-request");

    let mut response_flight = frame_server_response(waiting, b"vector-response");
    let response_wire = "0007524441310111011143500f898ee83026808be25bf16c114401010101010101020f20766563746f722d726573706f6e7365b04abfae374dcb7592292ba4c22729bf00";
    assert_eq!(hex::encode(response_flight.remaining()), response_wire);
    let response = awaiting_response
        .authenticate(decode_one(response_flight.remaining()))
        .unwrap_or_else(|error| panic!("client rejected valid response: {error:?}"));
    assert_eq!(response.message().encoded(), b"vector-response");
    let (client_session, response_message) = response.into_parts();
    assert_eq!(response_message.encoded(), b"vector-response");
    let response_length = response_flight.remaining().len();
    response_flight.advance(response_length).unwrap();
    let server_session = response_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete response did not restore server session"));
    assert_eq!(client_session.next_client_sequence(), 1);
    assert_eq!(client_session.next_server_sequence(), 1);
    assert_eq!(server_session.next_client_sequence(), 1);
    assert_eq!(server_session.next_server_sequence(), 1);

    let (awaiting_response, waiting) =
        send_client_request(client_session, server_session, b"second-request");
    let response_flight = frame_server_response(waiting, b"second-response");
    let valid = decode_one(response_flight.remaining());
    let (kind, session_id, _, length, payload, tag) = valid.into_parts();
    let duplicate = Record::new(kind, session_id, 0, length, payload, tag);
    let error = expect_error(
        awaiting_response.authenticate(duplicate),
        "duplicate response sequence was accepted",
    );
    assert_eq!(
        error,
        ClientSessionFault::UnexpectedSequence {
            expected: 1,
            observed: 0,
        }
    );
}

#[test]
fn client_handshake_rejects_unqualified_or_unauthenticated_servers() {
    let mut rng = FixedRng::new(CLIENT_NONCE);
    let forbidden = match ClientHelloFlight::begin(
        ClientParameters::new(DEVICE_ID, BearerBinding::BleGatt),
        client_credential(),
        &mut rng,
    ) {
        Ok(_) => panic!("qualification suite started on BLE"),
        Err(error) => error,
    };
    assert!(matches!(
        forbidden,
        ClientHandshakeError::QualificationSuiteForbidden {
            bearer: BearerBinding::BleGatt
        }
    ));

    let (awaiting, hello) = start_client_handshake();
    let record = server_hello_record_for(hello, DEVICE_ID, GENERATION);
    let (kind, session_id, sequence, length, mut payload, tag) = record.into_parts();
    payload[6] = BearerBinding::BleGatt as u8;
    let error = expect_error(
        awaiting.accept(Record::new(
            kind, session_id, sequence, length, payload, tag,
        )),
        "server hello on the wrong bearer was accepted",
    );
    assert!(matches!(
        error,
        ClientHandshakeError::BearerMismatch {
            expected: BearerBinding::UsbSerialJtag,
            observed: BearerBinding::BleGatt,
        }
    ));

    let other_device = DeviceId::new([0x99; 16]);
    let (awaiting, hello) = start_client_handshake();
    let error = expect_error(
        awaiting.accept(server_hello_record_for(hello, other_device, GENERATION)),
        "server hello for the wrong device was accepted",
    );
    match error {
        ClientHandshakeError::DeviceMismatch { expected, observed } => {
            assert_eq!(expected, DEVICE_ID);
            assert_eq!(observed, other_device);
        }
        other => panic!("wrong device produced unexpected error: {other:?}"),
    }

    let next_generation = CredentialGeneration::new(GENERATION.get() + 1);
    let (awaiting, hello) = start_client_handshake();
    let error = expect_error(
        awaiting.accept(server_hello_record_for(hello, DEVICE_ID, next_generation)),
        "server hello for the wrong credential generation was accepted",
    );
    match error {
        ClientHandshakeError::CredentialGenerationMismatch { expected, observed } => {
            assert_eq!(expected, GENERATION);
            assert_eq!(observed, next_generation);
        }
        other => panic!("wrong generation produced unexpected error: {other:?}"),
    }

    let (awaiting, hello) = start_client_handshake();
    let mut epochs = SessionEpochAllocator::new();
    let mut server_rng = FixedRng::new(SERVER_NONCE);
    let mut server_hello_flight = ServerHelloFlight::begin(
        hello,
        ActiveCredential::new(CREDENTIAL_ID, GENERATION, PSK),
        ServerParameters::new(DEVICE_ID, BearerBinding::UsbSerialJtag),
        &mut epochs,
        &mut server_rng,
    )
    .unwrap();
    let awaiting_proof = awaiting
        .accept(decode_one(server_hello_flight.remaining()))
        .unwrap();
    let length = server_hello_flight.remaining().len();
    server_hello_flight.advance(length).unwrap();
    let server_proof_flight = server_hello_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("complete server hello did not expose proof"));
    let proof = decode_one(server_proof_flight.remaining());
    let (kind, session_id, sequence, length, mut payload, tag) = proof.into_parts();
    payload[0] ^= 0x80;
    let error = expect_error(
        awaiting_proof.verify(Record::new(
            kind, session_id, sequence, length, payload, tag,
        )),
        "invalid server proof was accepted",
    );
    assert!(matches!(error, ClientHandshakeError::AuthenticationFailed));
}

#[test]
fn client_request_exhaustion_returns_the_exact_unsent_owner() {
    let (mut client, server, _) = establish_client_server();
    client.set_sequences_for_test(u64::MAX, 0);
    let mut buffer = [0xa5_u8; PAYLOAD_CAPACITY];
    buffer[..4].copy_from_slice(b"last");
    let expected = buffer;
    let message = OwnedMessage::new(MessageLength::new(4).unwrap(), buffer);
    let fault = match client.frame_request(message) {
        Ok(_) => panic!("exhausted client sequence framed a request"),
        Err(fault) => fault,
    };
    assert_eq!(fault.kind(), ClientRequestFaultKind::SequenceExhausted);
    let returned = fault.into_message();
    assert_eq!(returned.encoded(), b"last");
    assert_eq!(returned.full_buffer(), &expected);
    drop(server);
}

#[test]
fn client_response_authentication_fails_closed_for_every_session_boundary() {
    let (awaiting, flight, _) = pending_server_response(b"kind", b"response");
    let record = decode_one(flight.remaining());
    let (_, session_id, sequence, length, payload, tag) = record.into_parts();
    let error = expect_error(
        awaiting.authenticate(Record::new(
            RECORD_KIND_REQUEST,
            session_id,
            sequence,
            length,
            payload,
            tag,
        )),
        "response state accepted a request record",
    );
    assert_eq!(
        error,
        ClientSessionFault::UnexpectedKind {
            observed: RECORD_KIND_REQUEST
        }
    );

    let (awaiting, flight, _) = pending_server_response(b"session", b"response");
    let record = decode_one(flight.remaining());
    let (kind, mut session_id, sequence, length, payload, tag) = record.into_parts();
    session_id[0] ^= 0x80;
    let error = expect_error(
        awaiting.authenticate(Record::new(
            kind, session_id, sequence, length, payload, tag,
        )),
        "response from the wrong session was accepted",
    );
    assert_eq!(error, ClientSessionFault::WrongSession);

    let (awaiting, flight, _) = pending_server_response(b"gap", b"response");
    let record = decode_one(flight.remaining());
    let (kind, session_id, sequence, length, payload, tag) = record.into_parts();
    let error = expect_error(
        awaiting.authenticate(Record::new(
            kind,
            session_id,
            sequence + 1,
            length,
            payload,
            tag,
        )),
        "response sequence gap was accepted",
    );
    assert_eq!(
        error,
        ClientSessionFault::UnexpectedSequence {
            expected: 0,
            observed: 1,
        }
    );

    let (awaiting, flight, _) = pending_server_response(b"tag", b"response");
    let record = decode_one(flight.remaining());
    let (kind, session_id, sequence, length, payload, mut tag) = record.into_parts();
    tag[0] ^= 0x80;
    let error = expect_error(
        awaiting.authenticate(Record::new(
            kind, session_id, sequence, length, payload, tag,
        )),
        "response with a bad tag was accepted",
    );
    assert_eq!(error, ClientSessionFault::BadTag);

    let (awaiting, flight, schedule) = pending_server_response(b"reflection", b"response");
    let record = decode_one(flight.remaining());
    let (kind, session_id, sequence, length, payload, _) = record.into_parts();
    let untagged = Record::new(
        kind,
        session_id,
        sequence,
        length,
        payload,
        [0; AUTH_TAG_LENGTH],
    );
    let reflected = client_record_tag(&schedule.client_record_key, &untagged);
    let (kind, session_id, sequence, length, payload, _) = untagged.into_parts();
    let error = expect_error(
        awaiting.authenticate(Record::new(
            kind, session_id, sequence, length, payload, reflected,
        )),
        "client-direction tag was accepted on a response",
    );
    assert_eq!(error, ClientSessionFault::BadTag);

    let (mut client, server, schedule) = establish_client_server();
    client.set_sequences_for_test(0, u64::MAX);
    let (awaiting, waiting) = send_client_request(client, server, b"exhaustion");
    drop(waiting);
    let exhausted = Record::new(
        RECORD_KIND_RESPONSE,
        schedule.session_id.0,
        u64::MAX,
        PayloadLength::new(0).unwrap(),
        [0; PAYLOAD_CAPACITY],
        [0; AUTH_TAG_LENGTH],
    );
    let error = expect_error(
        awaiting.authenticate(exhausted),
        "exhausted response sequence was accepted",
    );
    assert_eq!(error, ClientSessionFault::SequenceExhausted);
}
