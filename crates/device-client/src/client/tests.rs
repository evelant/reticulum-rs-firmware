use std::{collections::VecDeque, io, time::Duration};

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, DestinationHash, DeviceRequest, DeviceResponse,
    EncodedPacketSha256, IdempotencyKey, IdentitySummary, LxmfBasicSendAccepted, LxmfMessageHandle,
    LxmfMessageSummary, LxmfPeerDiscoveryCursor, LxmfPeerDiscoveryIncarnation,
    LxmfPeerDiscoveryPage, LxmfPeerGeneration, LxmfReadChunk, NomadFetchId, NomadFetchPollResponse,
    NomadFetchStartAccepted, NomadFetchStartOutcome, NomadFetchStartRequest, NomadPage,
    NomadPagePath, NomadRequestTimestampUnixMs, PreparedPacketDetails, RequestEnvelope,
    ResponseEnvelope, SubmissionId, SubmissionState, SubmissionStatus, decode_request,
    encode_response,
};
use reticulum_device_api_framing::{DecodeEvent, StreamDecoder};
use reticulum_device_api_handoff::{LocalApiReply, MessageLength, OwnedMessage};
use reticulum_device_api_session::{
    ActiveCredential, BearerBinding, ClientHello, CredentialGeneration, CredentialId,
    PendingClientProof, ServerHelloFlight, ServerParameters, ServerSession, SessionEpochAllocator,
    SessionSuite,
};
use reticulum_lxmf_wire::{MessageView, WireLimits};
use sha2::{Digest, Sha256};

use super::{BasicLxmfSend, ClientConfig, ClientError, ClientSessionProfile, DeviceClient};
use crate::{ACTIVATED_CREDENTIAL_STATE_BYTES, ActivatedCredential, CredentialStateError};

const DEVICE_BYTES: [u8; 16] = [0x11; 16];
const CREDENTIAL_BYTES: [u8; 16] = [0x22; 16];
const GENERATION: u64 = 7;
const PSK: [u8; 32] = [0x33; 32];
const PRIMARY: DestinationHash = DestinationHash([0x44; 16]);
const LXMF_LOCAL: DestinationHash = DestinationHash([0x55; 16]);
const LXMF_REMOTE: DestinationHash = DestinationHash([0x66; 16]);
const NOMAD_REMOTE: DestinationHash = DestinationHash([0x77; 16]);
const NOMAD_FETCH_ID: NomadFetchId = match NomadFetchId::new([0x88; 8], 3) {
    Ok(id) => id,
    Err(_) => panic!("test fetch ID is valid"),
};

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

enum PeerState {
    AwaitClientHello,
    AwaitClientProof(PendingClientProof),
    Established(ServerSession),
}

struct MockPeer {
    decoder: StreamDecoder,
    output: VecDeque<u8>,
    state: Option<PeerState>,
    epochs: SessionEpochAllocator,
    rng: FixedRng,
    summary: LxmfMessageSummary,
    wire: Vec<u8>,
    request_count: usize,
    last_send: Option<OwnedSend>,
    last_peer_cursor: Option<Option<LxmfPeerDiscoveryCursor>>,
    last_nomad_start: Option<OwnedNomadFetch>,
    last_nomad_poll: Option<NomadFetchId>,
    parameters: ServerParameters,
    max_io_chunk: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct OwnedSend {
    destination: DestinationHash,
    timestamp_unix_ms: u64,
    title: Vec<u8>,
    content: Vec<u8>,
    idempotency_key: IdempotencyKey,
}

#[derive(Debug, Eq, PartialEq)]
struct OwnedNomadFetch {
    destination: DestinationHash,
    path: String,
    timestamp_unix_ms: u64,
    idempotency_key: IdempotencyKey,
}

impl MockPeer {
    fn new() -> Self {
        Self::new_for_profile(ClientSessionProfile::UsbSerialJtagQualification, usize::MAX)
    }

    fn new_for_profile(profile: ClientSessionProfile, max_io_chunk: usize) -> Self {
        let wire = sample_lxmf_wire(b"hello", b"from peer");
        let summary = sample_lxmf_summary(1, &wire);
        let device_id = reticulum_device_api_session::DeviceId::new(DEVICE_BYTES);
        let parameters = match profile {
            ClientSessionProfile::UsbSerialJtagQualification => {
                ServerParameters::new(device_id, BearerBinding::UsbSerialJtag)
            }
            ClientSessionProfile::WifiQualification => ServerParameters::new_for_suite(
                device_id,
                BearerBinding::Wifi,
                SessionSuite::WifiQualification,
            ),
            ClientSessionProfile::BleGattQualification => ServerParameters::new_for_suite(
                device_id,
                BearerBinding::BleGatt,
                SessionSuite::BleGattQualification,
            ),
        };
        Self {
            decoder: StreamDecoder::new(),
            output: VecDeque::new(),
            state: Some(PeerState::AwaitClientHello),
            epochs: SessionEpochAllocator::new(),
            rng: FixedRng::new(0x88),
            summary,
            wire,
            request_count: 0,
            last_send: None,
            last_peer_cursor: None,
            last_nomad_start: None,
            last_nomad_poll: None,
            parameters,
            max_io_chunk,
        }
    }

    fn process(&mut self, record: reticulum_device_api_framing::Record) {
        let state = self.state.take().expect("mock peer state is retained");
        self.state = Some(match state {
            PeerState::AwaitClientHello => {
                let hello = ClientHello::from_record(record).expect("canonical client hello");
                let mut hello_flight = ServerHelloFlight::begin(
                    hello,
                    ActiveCredential::new(
                        CredentialId::new(CREDENTIAL_BYTES),
                        CredentialGeneration::new(GENERATION),
                        PSK,
                    ),
                    self.parameters,
                    &mut self.epochs,
                    &mut self.rng,
                )
                .expect("mock handshake begins");
                self.output.extend(hello_flight.remaining());
                let length = hello_flight.remaining().len();
                hello_flight.advance(length).expect("hello advances");
                let mut proof_flight = hello_flight
                    .try_finish()
                    .unwrap_or_else(|_| panic!("complete server hello advances"));
                self.output.extend(proof_flight.remaining());
                let length = proof_flight.remaining().len();
                proof_flight.advance(length).expect("proof advances");
                PeerState::AwaitClientProof(
                    proof_flight
                        .try_finish()
                        .unwrap_or_else(|_| panic!("complete server proof advances")),
                )
            }
            PeerState::AwaitClientProof(pending) => PeerState::Established(
                pending
                    .authenticate(record)
                    .expect("mock accepts client proof"),
            ),
            PeerState::Established(session) => {
                let authenticated = session
                    .authenticate_request(record)
                    .expect("mock authenticates request");
                let (request, waiting) = authenticated.into_parts();
                self.request_count += 1;
                let envelope = decode_request(request.message().encoded())
                    .expect("client emits canonical logical request");
                let response = self.dispatch(envelope);
                let response = ResponseEnvelope {
                    version: ApiVersion::CURRENT,
                    request_id: envelope.request_id,
                    response,
                };
                let mut encoded = [0_u8; reticulum_device_api::MAX_MESSAGE_BYTES];
                let length = encode_response(&response, &mut encoded)
                    .expect("mock response fits logical API");
                let message = OwnedMessage::new(
                    MessageLength::new(length).expect("mock response is bounded"),
                    encoded,
                );
                let key = waiting.request_key();
                let mut reply = waiting
                    .frame_reply(LocalApiReply::new(key, message))
                    .unwrap_or_else(|fault| panic!("mock reply frames: {:?}", fault.kind()));
                self.output.extend(reply.remaining());
                let length = reply.remaining().len();
                reply.advance(length).expect("reply advances");
                PeerState::Established(
                    reply
                        .try_finish()
                        .unwrap_or_else(|_| panic!("complete reply restores session")),
                )
            }
        });
    }

    fn dispatch(&mut self, envelope: RequestEnvelope<'_>) -> DeviceResponse {
        match envelope.request {
            DeviceRequest::IdentitySummary => DeviceResponse::IdentitySummary(
                IdentitySummary::with_lxmf_delivery_destination(PRIMARY, LXMF_LOCAL),
            ),
            DeviceRequest::SubmissionStatus { id } => {
                DeviceResponse::SubmissionStatus(SubmissionStatus {
                    id,
                    state: SubmissionState::Delivered(PreparedPacketDetails {
                        packet_len: 211,
                        encoded_packet_sha256: EncodedPacketSha256::new([0xaa; 32]),
                    }),
                })
            }
            DeviceRequest::LxmfNext { after: None } => DeviceResponse::LxmfNext(self.summary),
            DeviceRequest::LxmfNext { after: Some(after) } if after == self.summary.handle() => {
                not_found(envelope.request.operation())
            }
            DeviceRequest::LxmfNext { .. } => not_found(envelope.request.operation()),
            DeviceRequest::LxmfRead {
                handle,
                offset,
                max_bytes,
            } if handle == self.summary.handle() => {
                let start = offset as usize;
                let end = (start + usize::from(max_bytes.get())).min(self.wire.len());
                DeviceResponse::LxmfRead(
                    LxmfReadChunk::new(
                        handle,
                        offset,
                        self.wire.len() as u32,
                        &self.wire[start..end],
                    )
                    .expect("mock chunk is valid"),
                )
            }
            DeviceRequest::LxmfRead { .. } => not_found(envelope.request.operation()),
            DeviceRequest::LxmfPeerNext { after } => {
                self.last_peer_cursor = Some(after);
                let incarnation = LxmfPeerDiscoveryIncarnation::new([0x88; 8]);
                let generation = LxmfPeerGeneration::new(3).unwrap();
                let current = LxmfPeerDiscoveryCursor::new(incarnation, generation.get());
                let at_end = after == Some(current);
                let stale = after.is_some() && !at_end;
                let peer = if at_end {
                    None
                } else {
                    Some(
                        reticulum_device_api::LxmfDiscoveredPeer::new(
                            LXMF_REMOTE,
                            reticulum_device_api::IdentityHash::new([0x77; 16]),
                            b"Metalbeard",
                            1,
                            2,
                            Some(-92),
                            Some(5),
                            125,
                            generation,
                        )
                        .unwrap(),
                    )
                };
                DeviceResponse::LxmfPeerNext(LxmfPeerDiscoveryPage::new(
                    current,
                    Some(generation),
                    Some(generation),
                    stale,
                    peer,
                ))
            }
            DeviceRequest::LxmfBasicSend {
                destination,
                timestamp_unix_ms,
                title,
                content,
                idempotency_key,
            } => {
                self.last_send = Some(OwnedSend {
                    destination,
                    timestamp_unix_ms,
                    title: title.to_vec(),
                    content: content.to_vec(),
                    idempotency_key,
                });
                DeviceResponse::LxmfBasicSendAccepted(LxmfBasicSendAccepted::new(
                    SubmissionId(9),
                    [0xbb; 32],
                ))
            }
            DeviceRequest::NomadFetchStart(request) => {
                self.last_nomad_start = Some(OwnedNomadFetch {
                    destination: request.destination(),
                    path: request.path().as_str().to_owned(),
                    timestamp_unix_ms: request.timestamp_unix_ms().get(),
                    idempotency_key: request.idempotency_key(),
                });
                DeviceResponse::NomadFetchStartAccepted(NomadFetchStartAccepted {
                    id: NOMAD_FETCH_ID,
                    outcome: NomadFetchStartOutcome::Accepted,
                })
            }
            DeviceRequest::NomadFetchPoll(request) => {
                self.last_nomad_poll = Some(request.id);
                DeviceResponse::NomadFetchPoll(NomadFetchPollResponse::Ready(
                    NomadPage::new(b">Metalbeard").unwrap(),
                ))
            }
            _ => DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::UnsupportedOperation,
                operation: Some(envelope.request.operation()),
            }),
        }
    }
}

impl io::Read for MockPeer {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.output.is_empty() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let count = output.len().min(self.output.len()).min(self.max_io_chunk);
        for byte in &mut output[..count] {
            *byte = self.output.pop_front().expect("count is bounded by queue");
        }
        Ok(count)
    }
}

impl io::Write for MockPeer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len().min(self.max_io_chunk);
        for byte in &bytes[..count] {
            match self.decoder.push(*byte) {
                DecodeEvent::Pending => {}
                DecodeEvent::Record(record) => self.process(record),
                DecodeEvent::MalformedCobs
                | DecodeEvent::MalformedRecord(_)
                | DecodeEvent::Overflow => panic!("client emitted malformed framing"),
            }
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn not_found(operation: u16) -> DeviceResponse {
    DeviceResponse::Error(ApiErrorResponse {
        code: ApiErrorCode::NotFound,
        operation: Some(operation),
    })
}

fn active_state() -> [u8; ACTIVATED_CREDENTIAL_STATE_BYTES] {
    let mut bytes = [0_u8; ACTIVATED_CREDENTIAL_STATE_BYTES];
    bytes[..8].copy_from_slice(b"RDPKEY1\0");
    bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
    bytes[10] = 2;
    bytes[16..32].copy_from_slice(&DEVICE_BYTES);
    bytes[32..48].copy_from_slice(&CREDENTIAL_BYTES);
    bytes[48..56].copy_from_slice(&GENERATION.to_le_bytes());
    bytes[56..88].copy_from_slice(&PSK);
    bytes
}

fn sample_lxmf_wire(title: &[u8], content: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    wire.extend_from_slice(&LXMF_LOCAL.0);
    wire.extend_from_slice(&LXMF_REMOTE.0);
    wire.extend_from_slice(&[0x77; 64]);
    wire.push(0x94);
    wire.push(0xcb);
    wire.extend_from_slice(&1.25_f64.to_bits().to_be_bytes());
    wire.extend_from_slice(&[0xc4, title.len() as u8]);
    wire.extend_from_slice(title);
    wire.extend_from_slice(&[0xc4, content.len() as u8]);
    wire.extend_from_slice(content);
    wire.push(0x80);
    wire
}

fn sample_lxmf_summary(handle: u64, wire: &[u8]) -> LxmfMessageSummary {
    let message = MessageView::parse_complete(
        wire,
        WireLimits::new(wire.len(), wire.len(), wire.len(), wire.len(), 65_536, 16),
    )
    .expect("sample wire parses");
    let payload = message.payload();
    LxmfMessageSummary::new(
        LxmfMessageHandle::new(handle).expect("sample handle is nonzero"),
        message.message_id(),
        DestinationHash(*message.destination_hash()),
        DestinationHash(*message.source_hash()),
        payload.timestamp_bits(),
        wire.len() as u32,
        payload.title().as_bytes().len() as u32,
        payload.content().as_bytes().len() as u32,
        payload.fields().raw().len() as u32,
        Sha256::digest(wire).into(),
    )
    .expect("sample summary is valid")
}

#[test]
fn activated_state_decoder_rejects_noncanonical_and_non_active_images() {
    let active = active_state();
    let credential = ActivatedCredential::decode(&active).expect("active state decodes");
    assert_eq!(credential.device_id().as_bytes(), &DEVICE_BYTES);
    assert_eq!(credential.credential_id().as_bytes(), &CREDENTIAL_BYTES);
    assert_eq!(credential.generation().get(), GENERATION);

    let mut pending = active;
    pending[10] = 1;
    assert!(matches!(
        ActivatedCredential::decode(&pending),
        Err(CredentialStateError::NotActive { state: 1 })
    ));

    let mut reserved = active;
    reserved[12] = 1;
    assert!(matches!(
        ActivatedCredential::decode(&reserved),
        Err(CredentialStateError::NonCanonicalReservedBytes)
    ));

    assert!(matches!(
        ActivatedCredential::decode(&active[..95]),
        Err(CredentialStateError::Length { actual: 95 })
    ));
}

#[test]
fn real_handshake_and_multi_request_session_cover_typed_device_surface() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let config = ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096);
    let mut client = DeviceClient::connect(
        MockPeer::new(),
        credential,
        &mut FixedRng::new(0x99),
        config,
    )
    .expect("real client/server handshake succeeds");

    assert_eq!(client.device_id().as_bytes(), &DEVICE_BYTES);
    let identity = client.identity_summary().expect("identity succeeds");
    assert_eq!(identity.primary_destination(), PRIMARY);
    assert_eq!(identity.lxmf_delivery_destination(), Some(LXMF_LOCAL));

    let nearby = client
        .lxmf_peer_next(None)
        .expect("nearby peer discovery succeeds");
    let discovered = nearby.peer().expect("first page contains the peer");
    assert_eq!(discovered.destination(), LXMF_REMOTE);
    assert_eq!(discovered.app_data(), b"Metalbeard");
    assert_eq!(discovered.observed_age_ms(), 125);
    let nearby_end = client
        .lxmf_peer_next(Some(nearby.next_cursor()))
        .expect("cursor continuation succeeds");
    assert!(nearby_end.peer().is_none());
    assert!(!nearby_end.history_gap());

    let summaries = client.lxmf_list().expect("LXMF list succeeds");
    assert_eq!(summaries.len(), 1);
    let message = client
        .lxmf_read(summaries[0].handle())
        .expect("LXMF read succeeds");
    assert_eq!(message.summary(), summaries[0]);
    let view = message.view().expect("verified message reparses");
    assert_eq!(view.payload().title().as_bytes(), b"hello");
    assert_eq!(view.payload().content().as_bytes(), b"from peer");

    let idempotency_key = IdempotencyKey([0xcc; 16]);
    let accepted = client
        .lxmf_basic_send(BasicLxmfSend::new(
            LXMF_REMOTE,
            1_784_732_100_000,
            b"subject",
            b"body",
            idempotency_key,
        ))
        .expect("basic send succeeds");
    assert_eq!(accepted.id, SubmissionId(9));
    assert_eq!(accepted.message_id(), &[0xbb; 32]);

    let status = client
        .submission_status(accepted.id)
        .expect("status succeeds in same session");
    assert_eq!(status.id, accepted.id);
    assert!(matches!(status.state, SubmissionState::Delivered(_)));

    let nomad_key = IdempotencyKey([0xdd; 16]);
    let fetch = client
        .nomad_fetch_start(NomadFetchStartRequest::new(
            NOMAD_REMOTE,
            NomadPagePath::new("/page/index.mu").unwrap(),
            NomadRequestTimestampUnixMs::new(1_784_732_100_001).unwrap(),
            nomad_key,
        ))
        .expect("Nomad fetch start succeeds");
    assert_eq!(fetch.id, NOMAD_FETCH_ID);
    assert_eq!(fetch.outcome, NomadFetchStartOutcome::Accepted);
    assert_eq!(
        client
            .nomad_fetch_poll(fetch.id)
            .expect("Nomad fetch poll succeeds"),
        NomadFetchPollResponse::Ready(NomadPage::new(b">Metalbeard").unwrap())
    );
    assert!(client.is_session_available());

    let peer = client.into_transport();
    assert_eq!(peer.request_count, 11);
    assert_eq!(peer.last_peer_cursor, Some(Some(nearby.next_cursor())));
    assert_eq!(
        peer.last_send,
        Some(OwnedSend {
            destination: LXMF_REMOTE,
            timestamp_unix_ms: 1_784_732_100_000,
            title: b"subject".to_vec(),
            content: b"body".to_vec(),
            idempotency_key,
        })
    );
    assert_eq!(
        peer.last_nomad_start,
        Some(OwnedNomadFetch {
            destination: NOMAD_REMOTE,
            path: "/page/index.mu".to_owned(),
            timestamp_unix_ms: 1_784_732_100_001,
            idempotency_key: nomad_key,
        })
    );
    assert_eq!(peer.last_nomad_poll, Some(NOMAD_FETCH_ID));
}

#[test]
fn wifi_profile_uses_suite_two_across_partial_stream_io() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let config = ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096);
    let mut client = DeviceClient::connect_with_profile(
        MockPeer::new_for_profile(ClientSessionProfile::WifiQualification, 3),
        credential,
        &mut FixedRng::new(0x99),
        config,
        ClientSessionProfile::WifiQualification,
    )
    .expect("Wi-Fi profile authenticates across partial stream I/O");

    assert_eq!(client.device_id().as_bytes(), &DEVICE_BYTES);
    assert_eq!(
        client
            .identity_summary()
            .expect("authenticated request succeeds")
            .lxmf_delivery_destination(),
        Some(LXMF_LOCAL)
    );
    assert_eq!(client.into_transport().request_count, 1);
}

#[test]
fn ble_gatt_profile_uses_suite_three_across_partial_stream_io() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let config = ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096);
    let mut client = DeviceClient::connect_with_profile(
        MockPeer::new_for_profile(ClientSessionProfile::BleGattQualification, 3),
        credential,
        &mut FixedRng::new(0x99),
        config,
        ClientSessionProfile::BleGattQualification,
    )
    .expect("BLE GATT profile authenticates across partial stream I/O");

    assert_eq!(client.device_id().as_bytes(), &DEVICE_BYTES);
    assert_eq!(
        client
            .identity_summary()
            .expect("authenticated request succeeds")
            .lxmf_delivery_destination(),
        Some(LXMF_LOCAL)
    );
    assert_eq!(client.into_transport().request_count, 1);
}

#[test]
fn logical_not_found_does_not_consume_authenticated_session() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let mut client = DeviceClient::connect(
        MockPeer::new(),
        credential,
        &mut FixedRng::new(0x99),
        ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096),
    )
    .expect("handshake succeeds");

    let missing = LxmfMessageHandle::new(2).expect("nonzero handle");
    assert!(matches!(
        client.lxmf_read(missing),
        Err(ClientError::LxmfNotFound { handle }) if handle == missing
    ));
    assert!(client.is_session_available());
    assert_eq!(
        client
            .identity_summary()
            .expect("later operation succeeds")
            .primary_destination(),
        PRIMARY
    );
}
