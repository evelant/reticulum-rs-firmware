extern crate std;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiVersion, DeviceRequest, Permissions, PrincipalId, RequestEnvelope, RequestId, encode_request,
};
use reticulum_device_api_credentials::{
    AuthorityRevision, AuthorizationPolicyVersion, CredentialAudit, CredentialAuthorityBuilder,
    CredentialGeneration, CredentialId, CredentialRecord, CredentialStatus, PairingOrigin,
    SelectedCredential,
};
use reticulum_device_api_framing::{DecodeEvent, Record, StreamDecoder};
use reticulum_device_api_handoff::{
    DeviceApiHandoff, LocalApiReply, MESSAGE_CAPACITY, MessageLength, OwnedMessage, SessionEpoch,
};
use reticulum_device_api_pairing_policy::{ConnectionId, MonotonicMillis as PairingMillis};
use reticulum_device_api_session::{
    AuthenticatedGrant, BearerBinding, ClientCredential, ClientHello, ClientHelloFlight,
    ClientParameters, ClientSession, DeviceId, ServerParameters,
};

use crate::session_admission_handoff::{
    SessionAdmissionHandoff, SessionAdmissionOutcome, SessionAdmissionReply,
};

use super::{
    AUTHENTICATED_SESSION_RAM_CEILING, AdmissionCommandDisposition, AdmissionReplyDisposition,
    AuthenticatedSession, AuthenticatedSessionFault, AuthenticatedSessionPhase,
    NodeReplyDisposition, PairingExclusiveCloseDisposition, RequestHandoffDisposition,
    SessionFramingFault, SessionRxDisposition, SessionTxAdvance, SessionTxKind,
    StaleNodeReplyReason,
};

const CREDENTIAL_ID: CredentialId = CredentialId::new([0x31; 16]);
const GENERATION: CredentialGeneration = CredentialGeneration::new(7);
const PSK: [u8; 32] = [0x42; 32];
const DEVICE_ID: DeviceId = DeviceId::new([0x53; 16]);

struct FixedRng {
    byte: u8,
}

impl FixedRng {
    const fn new(byte: u8) -> Self {
        Self { byte }
    }
}

impl RngCore for FixedRng {
    fn next_u32(&mut self) -> u32 {
        u32::from_le_bytes([self.byte; 4])
    }

    fn next_u64(&mut self) -> u64 {
        u64::from_le_bytes([self.byte; 8])
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        destination.fill(self.byte);
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for FixedRng {}

fn connection(value: u64) -> ConnectionId {
    ConnectionId::new(value).expect("test connection is nonzero")
}

fn selected_credential() -> SelectedCredential {
    let revision = AuthorityRevision::new(GENERATION.get());
    CredentialAuthorityBuilder::<1>::new(revision)
        .unwrap_or_else(|fault| panic!("authority revision rejected: {:?}", fault.kind()))
        .insert(CredentialRecord::with_secret(
            CREDENTIAL_ID,
            GENERATION,
            PrincipalId([0x64; 16]),
            Permissions::NONE,
            CredentialStatus::Active,
            CredentialAudit::new(
                revision,
                revision,
                PairingOrigin::LocalPhysicalPresence,
                AuthorizationPolicyVersion::new(1),
            ),
            PSK,
        ))
        .unwrap_or_else(|fault| panic!("credential rejected: {:?}", fault.kind()))
        .finish()
        .select_for_handshake(CREDENTIAL_ID)
        .unwrap_or_else(|_| panic!("active credential was not selectable"))
}

fn new_machine() -> AuthenticatedSession {
    AuthenticatedSession::new(ServerParameters::new(DEVICE_ID, BearerBinding::BleGatt))
}

fn decode_one(bytes: &[u8]) -> Record {
    let mut decoder = StreamDecoder::new();
    for byte in bytes {
        match decoder.push(*byte) {
            DecodeEvent::Pending => {}
            DecodeEvent::Record(record) => return record,
            DecodeEvent::MalformedCobs
            | DecodeEvent::MalformedRecord(_)
            | DecodeEvent::Overflow => panic!("test flight emitted malformed framing"),
        }
    }
    panic!("test flight emitted no complete record")
}

fn complete_client_flight<F>(
    remaining: impl Fn(&F) -> &[u8],
    advance: impl Fn(&mut F, usize),
    flight: &mut F,
) -> std::vec::Vec<u8> {
    let bytes = remaining(flight).to_vec();
    advance(flight, bytes.len());
    bytes
}

fn drain_machine_record(
    machine: &mut AuthenticatedSession,
    expected: SessionTxKind,
    maximum: usize,
) -> std::vec::Vec<u8> {
    let mut bytes = std::vec::Vec::new();
    while machine.tx_kind() == Some(expected) {
        let chunk = machine
            .next_tx_chunk(maximum)
            .expect("machine retained the expected flight")
            .to_vec();
        assert!(!chunk.is_empty());
        bytes.extend_from_slice(&chunk);
        let disposition = machine
            .advance_tx(chunk.len())
            .expect("exact acknowledgement is valid");
        if machine.tx_kind() == Some(expected) {
            assert_eq!(disposition, SessionTxAdvance::Partial);
        }
    }
    bytes
}

fn establish(
    machine: &mut AuthenticatedSession,
    connection: ConnectionId,
    client_nonce: u8,
    server_nonce: u8,
) -> ClientSession {
    assert!(machine.begin_connection(connection));
    complete_handshake(machine, connection, client_nonce, server_nonce)
}

fn replace_established(
    machine: &mut AuthenticatedSession,
    connection: ConnectionId,
    client_nonce: u8,
    server_nonce: u8,
) -> ClientSession {
    assert_eq!(machine.connection(), Some(connection));
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::Established);
    complete_handshake(machine, connection, client_nonce, server_nonce)
}

fn complete_handshake(
    machine: &mut AuthenticatedSession,
    connection: ConnectionId,
    client_nonce: u8,
    server_nonce: u8,
) -> ClientSession {
    let mut client_hello = ClientHelloFlight::begin(
        ClientParameters::new(DEVICE_ID, BearerBinding::BleGatt),
        ClientCredential::new(CREDENTIAL_ID, GENERATION, PSK),
        &mut FixedRng::new(client_nonce),
    )
    .expect("client hello begins");
    let hello_bytes = complete_client_flight(
        |flight: &ClientHelloFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut client_hello,
    );
    let awaiting_server_hello = client_hello
        .try_finish()
        .unwrap_or_else(|_| panic!("hello fully sent"));
    assert_eq!(
        machine
            .accept_record(decode_one(&hello_bytes), PairingMillis::new(10))
            .unwrap(),
        SessionRxDisposition::ClientHelloAccepted
    );

    let admission: &'static mut SessionAdmissionHandoff<NoopRawMutex> =
        std::boxed::Box::leak(std::boxed::Box::new(SessionAdmissionHandoff::new()));
    let (mut bearer_admission, mut node_admission) = admission.split();
    assert_eq!(
        machine.try_send_admission_command(&mut bearer_admission),
        AdmissionCommandDisposition::Transferred
    );
    let command = node_admission
        .try_receive_command()
        .expect("selection command transferred");
    assert_eq!(command.connection(), connection);
    assert_eq!(command.credential_id(), CREDENTIAL_ID);
    node_admission
        .try_send_reply(SessionAdmissionReply::new(
            command.connection(),
            SessionAdmissionOutcome::Selected(selected_credential()),
        ))
        .unwrap_or_else(|_| panic!("empty selection reply channel"));
    let reply = bearer_admission
        .try_receive_reply()
        .expect("selection reply transferred");
    assert_eq!(
        machine
            .accept_admission_reply(reply, &mut FixedRng::new(server_nonce))
            .unwrap(),
        AdmissionReplyDisposition::ServerHelloStarted
    );

    let server_hello_bytes = drain_machine_record(machine, SessionTxKind::ServerHello, 7);
    let awaiting_server_proof = awaiting_server_hello
        .accept(decode_one(&server_hello_bytes))
        .expect("server hello authenticates expected facts");
    let server_proof_bytes = drain_machine_record(machine, SessionTxKind::ServerProof, 5);
    let mut client_proof = awaiting_server_proof
        .verify(decode_one(&server_proof_bytes))
        .expect("server proof authenticates");
    let proof_bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientProofFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut client_proof,
    );
    let client_session = client_proof
        .try_finish()
        .unwrap_or_else(|_| panic!("client proof fully sent"));
    assert_eq!(
        machine
            .accept_record(decode_one(&proof_bytes), PairingMillis::new(11))
            .unwrap(),
        SessionRxDisposition::SessionEstablished
    );
    client_session
}

fn replacement_hello(nonce: u8) -> Record {
    ClientHello::new(BearerBinding::BleGatt, CREDENTIAL_ID, [nonce; 32]).into_record()
}

fn canonical_request(request_id: u64) -> OwnedMessage {
    let envelope = RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(request_id),
        request: DeviceRequest::SystemCapabilities,
    };
    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = encode_request(&envelope, &mut encoded).expect("request encodes");
    OwnedMessage::new(MessageLength::new(length).unwrap(), encoded)
}

fn message(bytes: &[u8]) -> OwnedMessage {
    let mut buffer = [0_u8; MESSAGE_CAPACITY];
    buffer[..bytes.len()].copy_from_slice(bytes);
    OwnedMessage::new(MessageLength::new(bytes.len()).unwrap(), buffer)
}

#[test]
fn real_handshake_and_request_reply_preserve_partial_tx_and_exact_owners() {
    let mut machine = new_machine();
    let client = establish(&mut machine, connection(1), 0x71, 0x81);
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::Established);
    assert_eq!(client.next_client_sequence(), 0);
    assert_eq!(client.next_server_sequence(), 0);

    let mut request_flight = client
        .frame_request(canonical_request(44))
        .unwrap_or_else(|_| panic!("request frames"));
    let request_bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut request_flight,
    );
    let awaiting_response = request_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("request fully sent"));
    assert_eq!(
        machine
            .accept_record(decode_one(&request_bytes), PairingMillis::new(12))
            .unwrap(),
        SessionRxDisposition::RequestAuthenticated
    );

    let api: &'static mut DeviceApiHandoff<NoopRawMutex, AuthenticatedGrant> =
        std::boxed::Box::leak(std::boxed::Box::new(DeviceApiHandoff::new()));
    let (mut bearer, mut node) = api.split();
    assert_eq!(
        machine.try_send_request(bearer.requests()),
        RequestHandoffDisposition::Transferred
    );
    let request = node
        .requests()
        .try_receive()
        .expect("node owns authenticated request");
    assert_eq!(request.key().epoch(), SessionEpoch::new(1));
    assert_eq!(request.key().correlation().get(), 0);
    assert_eq!(request.grant().admission_sequence(), 0);
    let reply = LocalApiReply::new(request.key(), message(b"exact reply"));
    drop(request);
    assert_eq!(
        machine
            .accept_node_reply(reply)
            .unwrap_or_else(|_| panic!("matching node reply starts a flight")),
        NodeReplyDisposition::ReplyFlightStarted
    );
    let reply_bytes = drain_machine_record(&mut machine, SessionTxKind::Reply, 3);
    let authenticated = awaiting_response
        .authenticate(decode_one(&reply_bytes))
        .expect("client authenticates exact response");
    let (client, response) = authenticated.into_parts();
    assert_eq!(response.encoded(), b"exact reply");
    assert_eq!(client.next_client_sequence(), 1);
    assert_eq!(client.next_server_sequence(), 1);
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::Established);
}

#[test]
fn request_handoff_pressure_retains_the_exact_request_until_capacity_returns() {
    let mut machine = new_machine();
    let client = establish(&mut machine, connection(1), 0x72, 0x82);
    let mut request_flight = client
        .frame_request(canonical_request(45))
        .unwrap_or_else(|_| panic!("request frames"));
    let bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut request_flight,
    );
    drop(
        request_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("request fully sent")),
    );
    machine
        .accept_record(decode_one(&bytes), PairingMillis::new(12))
        .unwrap();

    let api: &'static mut DeviceApiHandoff<NoopRawMutex, AuthenticatedGrant> =
        std::boxed::Box::leak(std::boxed::Box::new(DeviceApiHandoff::new()));
    let (mut bearer, mut node) = api.split();
    // Occupy request capacity with a real request from a second established
    // machine, then verify the first machine keeps its exact owner.
    let mut blocker = new_machine();
    let blocker_client = establish(&mut blocker, connection(2), 0x73, 0x83);
    let mut blocker_flight = blocker_client
        .frame_request(canonical_request(46))
        .unwrap_or_else(|_| panic!("blocker frames"));
    let blocker_bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut blocker_flight,
    );
    drop(
        blocker_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("blocker fully sent")),
    );
    blocker
        .accept_record(decode_one(&blocker_bytes), PairingMillis::new(12))
        .unwrap();
    assert_eq!(
        blocker.try_send_request(bearer.requests()),
        RequestHandoffDisposition::Transferred
    );

    assert_eq!(
        machine.try_send_request(bearer.requests()),
        RequestHandoffDisposition::RetainedUnderPressure
    );
    assert_eq!(
        machine.phase(),
        AuthenticatedSessionPhase::RequestHandoffPending
    );
    let blocker_request = node.requests().try_receive().expect("blocker queued");
    assert_eq!(blocker_request.key().epoch(), SessionEpoch::new(1));
    drop(blocker_request);
    assert_eq!(
        machine.try_send_request(bearer.requests()),
        RequestHandoffDisposition::Transferred
    );
    let retained = node
        .requests()
        .try_receive()
        .expect("retained request queued");
    assert_eq!(
        retained.message().encoded(),
        canonical_request(45).encoded()
    );
}

#[test]
fn reset_drains_stale_reply_and_advances_epoch_without_sharing_sequences() {
    let mut machine = new_machine();
    let first_client = establish(&mut machine, connection(1), 0x74, 0x84);
    let mut flight = first_client
        .frame_request(canonical_request(47))
        .unwrap_or_else(|_| panic!("first request frames"));
    let bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut flight,
    );
    drop(
        flight
            .try_finish()
            .unwrap_or_else(|_| panic!("first request fully sent")),
    );
    machine
        .accept_record(decode_one(&bytes), PairingMillis::new(12))
        .unwrap();
    let api: &'static mut DeviceApiHandoff<NoopRawMutex, AuthenticatedGrant> =
        std::boxed::Box::leak(std::boxed::Box::new(DeviceApiHandoff::new()));
    let (mut bearer, mut node) = api.split();
    machine.try_send_request(bearer.requests());
    let old_request = node.requests().try_receive().expect("old request queued");
    let old_key = old_request.key();
    drop(old_request);

    machine.reset();
    let stale = machine
        .accept_node_reply(LocalApiReply::new(old_key, message(b"late")))
        .unwrap_or_else(|_| panic!("reset reply is classified stale"));
    assert_eq!(
        stale,
        NodeReplyDisposition::DrainedStale {
            key: old_key,
            reason: StaleNodeReplyReason::NoLiveSession,
        }
    );

    let second_client = establish(&mut machine, connection(2), 0x75, 0x85);
    assert_eq!(second_client.next_client_sequence(), 0);
    assert_eq!(second_client.next_server_sequence(), 0);
    let mut second_flight = second_client
        .frame_request(canonical_request(48))
        .unwrap_or_else(|_| panic!("second request frames"));
    let second_bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut second_flight,
    );
    drop(
        second_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("second request fully sent")),
    );
    machine
        .accept_record(decode_one(&second_bytes), PairingMillis::new(13))
        .unwrap();
    machine.try_send_request(bearer.requests());
    let second_request = node
        .requests()
        .try_receive()
        .expect("second request queued");
    assert_eq!(second_request.key().epoch(), SessionEpoch::new(2));
    assert_eq!(second_request.key().correlation().get(), 0);
    assert_eq!(second_request.grant().admission_sequence(), 0);
}

#[test]
fn idle_established_session_is_replaced_on_the_same_connection_with_a_fresh_epoch() {
    let mut machine = new_machine();
    let first_client = establish(&mut machine, connection(1), 0x76, 0x86);
    assert_eq!(first_client.next_client_sequence(), 0);
    assert_eq!(machine.connection(), Some(connection(1)));

    let second_client = replace_established(&mut machine, connection(1), 0x77, 0x87);
    assert_eq!(machine.connection(), Some(connection(1)));
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::Established);
    assert_eq!(machine.fault(), None);
    assert_eq!(second_client.next_client_sequence(), 0);
    assert_eq!(second_client.next_server_sequence(), 0);

    let mut request_flight = second_client
        .frame_request(canonical_request(49))
        .unwrap_or_else(|_| panic!("replacement request frames"));
    let request_bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut request_flight,
    );
    drop(
        request_flight
            .try_finish()
            .unwrap_or_else(|_| panic!("replacement request fully sent")),
    );
    assert_eq!(
        machine
            .accept_record(decode_one(&request_bytes), PairingMillis::new(22))
            .unwrap(),
        SessionRxDisposition::RequestAuthenticated
    );

    let api: &'static mut DeviceApiHandoff<NoopRawMutex, AuthenticatedGrant> =
        std::boxed::Box::leak(std::boxed::Box::new(DeviceApiHandoff::new()));
    let (mut bearer, mut node) = api.split();
    assert_eq!(
        machine.try_send_request(bearer.requests()),
        RequestHandoffDisposition::Transferred
    );
    let request = node
        .requests()
        .try_receive()
        .expect("replacement request queued");
    assert_eq!(request.key().epoch(), SessionEpoch::new(2));
    assert_eq!(request.key().correlation().get(), 0);
    assert_eq!(request.grant().admission_sequence(), 0);
}

#[test]
fn malformed_idle_replacement_hello_remains_fail_closed() {
    let mut machine = new_machine();
    let _client = establish(&mut machine, connection(1), 0x77, 0x87);
    let (kind, mut session_id, sequence, payload_length, payload, tag) =
        replacement_hello(0x90).into_parts();
    session_id[0] = 1;
    let malformed = Record::new(kind, session_id, sequence, payload_length, payload, tag);

    assert_eq!(
        machine
            .accept_record(malformed, PairingMillis::new(12))
            .expect_err("a replacement hello must retain canonical handshake framing"),
        AuthenticatedSessionFault::Handshake
    );
    assert_eq!(
        machine.phase(),
        AuthenticatedSessionPhase::TerminatedUntilReset
    );
    assert_eq!(machine.fault(), Some(AuthenticatedSessionFault::Handshake));
}

#[test]
fn replacement_hello_never_displaces_request_or_reply_owners() {
    let mut machine = new_machine();
    let client = establish(&mut machine, connection(1), 0x78, 0x88);
    let mut request_flight = client
        .frame_request(canonical_request(50))
        .unwrap_or_else(|_| panic!("request frames"));
    let request_bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut request_flight,
    );
    let awaiting_response = request_flight
        .try_finish()
        .unwrap_or_else(|_| panic!("request fully sent"));
    machine
        .accept_record(decode_one(&request_bytes), PairingMillis::new(12))
        .unwrap();
    assert_eq!(
        machine
            .accept_record(replacement_hello(0x91), PairingMillis::new(13))
            .unwrap(),
        SessionRxDisposition::ClientHelloDroppedBusy
    );
    assert_eq!(
        machine.phase(),
        AuthenticatedSessionPhase::RequestHandoffPending
    );

    let api: &'static mut DeviceApiHandoff<NoopRawMutex, AuthenticatedGrant> =
        std::boxed::Box::leak(std::boxed::Box::new(DeviceApiHandoff::new()));
    let (mut bearer, mut node) = api.split();
    assert_eq!(
        machine.try_send_request(bearer.requests()),
        RequestHandoffDisposition::Transferred
    );
    let request = node.requests().try_receive().expect("exact request queued");
    assert_eq!(request.message().encoded(), canonical_request(50).encoded());
    assert_eq!(
        machine
            .accept_record(replacement_hello(0x92), PairingMillis::new(14))
            .unwrap(),
        SessionRxDisposition::ClientHelloDroppedBusy
    );
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::AwaitingReply);

    let reply = LocalApiReply::new(request.key(), message(b"retained reply"));
    drop(request);
    assert_eq!(
        machine
            .accept_node_reply(reply)
            .unwrap_or_else(|_| panic!("matching reply starts a flight")),
        NodeReplyDisposition::ReplyFlightStarted
    );
    let full_reply = machine
        .next_tx_chunk(usize::MAX)
        .expect("reply flight retained")
        .to_vec();
    assert_eq!(
        machine
            .accept_record(replacement_hello(0x93), PairingMillis::new(15))
            .unwrap(),
        SessionRxDisposition::ClientHelloDroppedBusy
    );
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::ReplyFlight);
    assert_eq!(machine.next_tx_chunk(usize::MAX).unwrap(), full_reply);

    let first_length = 3;
    let mut reply_bytes = full_reply[..first_length].to_vec();
    assert_eq!(
        machine.advance_tx(first_length).unwrap(),
        SessionTxAdvance::Partial
    );
    let retained_tail = machine.next_tx_chunk(usize::MAX).unwrap().to_vec();
    assert_eq!(
        machine
            .accept_record(replacement_hello(0x94), PairingMillis::new(16))
            .unwrap(),
        SessionRxDisposition::ClientHelloDroppedBusy
    );
    assert_eq!(machine.next_tx_chunk(usize::MAX).unwrap(), retained_tail);
    reply_bytes.extend_from_slice(&drain_machine_record(&mut machine, SessionTxKind::Reply, 5));
    let authenticated = awaiting_response
        .authenticate(decode_one(&reply_bytes))
        .expect("original client authenticates retained reply");
    let (_, response) = authenticated.into_parts();
    assert_eq!(response.encoded(), b"retained reply");
}

#[test]
fn replacement_hello_preserves_pairing_exclusivity() {
    let mut machine = new_machine();
    let _client = establish(&mut machine, connection(1), 0x79, 0x89);
    assert_eq!(
        machine.close_for_pairing_exclusivity(connection(1)),
        PairingExclusiveCloseDisposition::Closed
    );
    assert_eq!(
        machine
            .accept_record(replacement_hello(0x95), PairingMillis::new(12))
            .unwrap(),
        SessionRxDisposition::ClientHelloDroppedBusy
    );
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::PairingExclusive);
    assert_eq!(machine.fault(), None);
}

#[test]
fn established_fault_is_terminal_until_explicit_reset() {
    let mut machine = new_machine();
    let client = establish(&mut machine, connection(1), 0x76, 0x86);
    let mut request = client
        .frame_request(canonical_request(49))
        .unwrap_or_else(|_| panic!("request frames"));
    let bytes = complete_client_flight(
        |flight: &reticulum_device_api_session::ClientRequestFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut request,
    );
    drop(
        request
            .try_finish()
            .unwrap_or_else(|_| panic!("request sent")),
    );
    machine
        .accept_record(decode_one(&bytes), PairingMillis::new(12))
        .unwrap();
    let fault = machine
        .accept_record(decode_one(&bytes), PairingMillis::new(13))
        .expect_err("second in-flight request violates ordering");
    assert!(matches!(
        fault,
        AuthenticatedSessionFault::UnexpectedRecord {
            phase: AuthenticatedSessionPhase::RequestHandoffPending,
            ..
        }
    ));
    assert_eq!(
        machine.phase(),
        AuthenticatedSessionPhase::TerminatedUntilReset
    );
    assert!(!machine.begin_connection(connection(2)));
    machine.reset();
    assert!(machine.begin_connection(connection(2)));
    let framing = machine
        .accept_decode_event(DecodeEvent::MalformedCobs, PairingMillis::new(14))
        .expect_err("framing is terminal");
    assert_eq!(
        framing,
        AuthenticatedSessionFault::Framing(SessionFramingFault::MalformedCobs)
    );
}

#[test]
fn pairing_exclusivity_drains_partial_record_then_drops_ordinary_state() {
    let mut machine = new_machine();
    assert!(machine.begin_connection(connection(1)));
    let mut hello = ClientHelloFlight::begin(
        ClientParameters::new(DEVICE_ID, BearerBinding::BleGatt),
        ClientCredential::new(CREDENTIAL_ID, GENERATION, PSK),
        &mut FixedRng::new(0x77),
    )
    .expect("hello begins");
    let hello_bytes = complete_client_flight(
        |flight: &ClientHelloFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut hello,
    );
    drop(hello.try_finish().unwrap_or_else(|_| panic!("hello sent")));
    machine
        .accept_record(decode_one(&hello_bytes), PairingMillis::new(10))
        .unwrap();
    let admission: &'static mut SessionAdmissionHandoff<NoopRawMutex> =
        std::boxed::Box::leak(std::boxed::Box::new(SessionAdmissionHandoff::new()));
    let (mut bearer, mut node) = admission.split();
    machine.try_send_admission_command(&mut bearer);
    let command = node.try_receive_command().expect("command queued");
    node.try_send_reply(SessionAdmissionReply::new(
        command.connection(),
        SessionAdmissionOutcome::Selected(selected_credential()),
    ))
    .unwrap_or_else(|_| panic!("reply channel empty"));
    machine
        .accept_admission_reply(
            bearer.try_receive_reply().expect("reply queued"),
            &mut FixedRng::new(0x87),
        )
        .unwrap();

    let first = machine.next_tx_chunk(4).unwrap().to_vec();
    assert_eq!(first.len(), 4);
    assert_eq!(
        machine.advance_tx(first.len()).unwrap(),
        SessionTxAdvance::Partial
    );
    assert_eq!(
        machine.close_for_pairing_exclusivity(connection(1)),
        PairingExclusiveCloseDisposition::DrainBeforeClose {
            kind: SessionTxKind::ServerHello,
        }
    );
    drain_machine_record(&mut machine, SessionTxKind::ServerHello, 16);
    assert_eq!(
        machine.close_for_pairing_exclusivity(connection(1)),
        PairingExclusiveCloseDisposition::Closed
    );
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::PairingExclusive);
}

#[test]
fn selected_admission_reply_after_pairing_close_is_drained_without_starting_tx() {
    let mut machine = new_machine();
    assert!(machine.begin_connection(connection(1)));
    let mut hello = ClientHelloFlight::begin(
        ClientParameters::new(DEVICE_ID, BearerBinding::BleGatt),
        ClientCredential::new(CREDENTIAL_ID, GENERATION, PSK),
        &mut FixedRng::new(0x78),
    )
    .expect("hello begins");
    let hello_bytes = complete_client_flight(
        |flight: &ClientHelloFlight| flight.remaining(),
        |flight, acknowledged| flight.advance(acknowledged).unwrap(),
        &mut hello,
    );
    drop(hello.try_finish().unwrap_or_else(|_| panic!("hello sent")));
    machine
        .accept_record(decode_one(&hello_bytes), PairingMillis::new(10))
        .unwrap();

    let admission: &'static mut SessionAdmissionHandoff<NoopRawMutex> =
        std::boxed::Box::leak(std::boxed::Box::new(SessionAdmissionHandoff::new()));
    let (mut bearer, mut node) = admission.split();
    assert_eq!(
        machine.try_send_admission_command(&mut bearer),
        AdmissionCommandDisposition::Transferred
    );
    let command = node.try_receive_command().expect("command queued");
    node.try_send_reply(SessionAdmissionReply::new(
        command.connection(),
        SessionAdmissionOutcome::Selected(selected_credential()),
    ))
    .unwrap_or_else(|_| panic!("reply channel empty"));

    assert_eq!(
        machine.close_for_pairing_exclusivity(connection(1)),
        PairingExclusiveCloseDisposition::Closed
    );
    assert_eq!(
        machine
            .accept_admission_reply(
                bearer.try_receive_reply().expect("selected reply queued"),
                &mut FixedRng::new(0x88),
            )
            .unwrap(),
        AdmissionReplyDisposition::DrainedStale
    );
    assert_eq!(machine.phase(), AuthenticatedSessionPhase::PairingExclusive);
    assert_eq!(machine.tx_kind(), None);
}

#[test]
fn owner_is_send_static_and_within_the_declared_ram_ceiling() {
    fn require_send_static<T: Send + 'static>() {}
    require_send_static::<AuthenticatedSession>();
    std::println!(
        "authenticated_session_size={}",
        core::mem::size_of::<AuthenticatedSession>()
    );
    assert!(core::mem::size_of::<AuthenticatedSession>() <= AUTHENTICATED_SESSION_RAM_CEILING);
}
