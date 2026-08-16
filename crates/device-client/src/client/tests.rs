use std::{collections::VecDeque, io, time::Duration};

use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilityAvailability, CapabilitySnapshot,
    DestinationHash, DeviceRequest, DeviceResponse, EncodedPacketSha256, IdempotencyKey,
    IdentityHash, IdentitySummary, IngressObservation, IngressSignal, LxmfBasicSendAccepted,
    LxmfMailboxStatus, LxmfMessageHandle, LxmfMessageLocation, LxmfMessageSummary,
    LxmfPeerDiscoveryCursor, LxmfPeerDiscoveryIncarnation, LxmfPeerDiscoveryPage,
    LxmfPeerGeneration, LxmfReadChunk, ManualServiceAnnounceDisposition, NodeDiagnosticsSnapshot,
    NomadFetchId, NomadFetchPollResponse, NomadFetchStartAccepted, NomadFetchStartOutcome,
    NomadFetchStartRequest, NomadPage, NomadPagePath, NomadRequestTimestampUnixMs,
    PreparedPacketDetails, ProbeId, ProbePollResponse, ProbeStartAccepted, ProbeStartOutcome,
    ProbeStartRequest, ProbeSuccess, RadioTraceAppliedLoraProfile, RadioTraceCursor,
    RadioTracePage, RadioTracePageRequest, RequestEnvelope, ResponseEnvelope, RnsDiagnostics,
    RouteDiagnosticEntry, RouteDiagnosticResolution, RouteDiagnosticsPage, RouteDiagnosticsRequest,
    SubmissionId, SubmissionState, SubmissionStatus, decode_request, encode_response,
};
#[cfg(feature = "network-config")]
use reticulum_device_api::{
    NetworkConfigMutation, NetworkConfigMutationOutcome, NetworkConfigMutationRequest,
    NetworkConfigSnapshot, NetworkRuntimeStatus, ReticulumTcpPeerConfigSummary,
    ReticulumTcpPeerIpv4Address, ReticulumTcpPeerState, ReticulumTcpPeerUpdate,
    WifiCredentialUpdate, WifiNetworkConfigSummary, WifiNetworkProfileId, WifiNetworkUpdate,
    WifiSsid, WifiStationState,
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
const PROBE_ID: ProbeId = match ProbeId::new([0x99; 16]) {
    Ok(id) => id,
    Err(_) => panic!("test probe ID is valid"),
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
    last_mailbox_acknowledgement: Option<LxmfMessageHandle>,
    last_peer_cursor: Option<Option<LxmfPeerDiscoveryCursor>>,
    last_nomad_start: Option<OwnedNomadFetch>,
    last_nomad_poll: Option<NomadFetchId>,
    last_probe_start: Option<ProbeStartRequest>,
    last_probe_poll: Option<ProbeId>,
    probe_start_response: DeviceResponse,
    probe_poll_response: DeviceResponse,
    last_route_diagnostics_cursor: Option<Option<DestinationHash>>,
    last_radio_trace_cursor: Option<Option<RadioTraceCursor>>,
    manual_announce_availability: CapabilityAvailability,
    manual_announce_disposition: ManualServiceAnnounceDisposition,
    manual_announce_requests: usize,
    #[cfg(feature = "network-config")]
    last_network_mutation: Option<OwnedNetworkMutation>,
    parameters: ServerParameters,
    max_io_chunk: usize,
    response_version: ApiVersion,
}

#[derive(Debug, Eq, PartialEq)]
struct OwnedSend {
    destination: DestinationHash,
    timestamp_unix_ms: u64,
    title: Vec<u8>,
    content: Vec<u8>,
    location: Option<LxmfMessageLocation>,
    idempotency_key: IdempotencyKey,
}

#[derive(Debug, Eq, PartialEq)]
struct OwnedNomadFetch {
    destination: DestinationHash,
    path: String,
    timestamp_unix_ms: u64,
    idempotency_key: IdempotencyKey,
}

#[cfg(feature = "network-config")]
#[derive(Debug, Eq, PartialEq)]
enum OwnedNetworkMutation {
    UpsertWifi {
        profile_id: WifiNetworkProfileId,
        enabled: bool,
        priority: u8,
        ssid: Vec<u8>,
        replacement_passphrase: Option<Vec<u8>>,
        expected_revision: u64,
        idempotency_key: IdempotencyKey,
    },
    ReplaceTcpPeer {
        peer: Option<ReticulumTcpPeerUpdate>,
        expected_revision: u64,
        idempotency_key: IdempotencyKey,
    },
}

impl MockPeer {
    fn new() -> Self {
        Self::new_for_profile(ClientSessionProfile::BleGatt, usize::MAX)
    }

    fn new_for_profile(profile: ClientSessionProfile, max_io_chunk: usize) -> Self {
        let wire = sample_lxmf_wire(b"hello", b"from peer");
        let summary = sample_lxmf_summary(1, &wire);
        let device_id = reticulum_device_api_session::DeviceId::new(DEVICE_BYTES);
        let parameters = match profile {
            ClientSessionProfile::BleGatt => ServerParameters::new_for_suite(
                device_id,
                BearerBinding::BleGatt,
                SessionSuite::BleGatt,
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
            last_mailbox_acknowledgement: None,
            last_peer_cursor: None,
            last_nomad_start: None,
            last_nomad_poll: None,
            last_probe_start: None,
            last_probe_poll: None,
            probe_start_response: DeviceResponse::ReticulumProbeStartAccepted(
                ProbeStartAccepted::new(PROBE_ID, ProbeStartOutcome::Accepted),
            ),
            probe_poll_response: DeviceResponse::ReticulumProbePoll(ProbePollResponse::Succeeded(
                sample_probe_success(),
            )),
            last_route_diagnostics_cursor: None,
            last_radio_trace_cursor: None,
            manual_announce_availability: CapabilityAvailability::Available,
            manual_announce_disposition: ManualServiceAnnounceDisposition::Queued,
            manual_announce_requests: 0,
            #[cfg(feature = "network-config")]
            last_network_mutation: None,
            parameters,
            max_io_chunk,
            response_version: ApiVersion::CURRENT,
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
                    version: self.response_version,
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
            DeviceRequest::SystemCapabilities => DeviceResponse::SystemCapabilities(
                CapabilitySnapshot::for_dispatch(false)
                    .with_dispatch_manual_service_announce(self.manual_announce_availability),
            ),
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
            DeviceRequest::LxmfMailboxStatus => DeviceResponse::LxmfMailboxStatus(
                LxmfMailboxStatus::new(Some(self.summary.handle()), None).unwrap(),
            ),
            DeviceRequest::LxmfMailboxAcknowledge { through }
                if through == self.summary.handle() =>
            {
                self.last_mailbox_acknowledgement = Some(through);
                DeviceResponse::LxmfMailboxAcknowledged(
                    LxmfMailboxStatus::new(Some(self.summary.handle()), Some(through)).unwrap(),
                )
            }
            DeviceRequest::LxmfMailboxAcknowledge { .. } => not_found(envelope.request.operation()),
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
                location,
                idempotency_key,
            } => {
                self.last_send = Some(OwnedSend {
                    destination,
                    timestamp_unix_ms,
                    title: title.to_vec(),
                    content: content.to_vec(),
                    location,
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
            DeviceRequest::ReticulumProbeStart(request) => {
                self.last_probe_start = Some(request);
                self.probe_start_response
            }
            DeviceRequest::ReticulumProbePoll(request) => {
                self.last_probe_poll = Some(request.id());
                self.probe_poll_response
            }
            DeviceRequest::NodeDiagnostics => {
                DeviceResponse::NodeDiagnostics(sample_node_diagnostics())
            }
            DeviceRequest::RouteDiagnosticsPage(request) => {
                self.last_route_diagnostics_cursor = Some(request.after());
                DeviceResponse::RouteDiagnosticsPage(sample_route_diagnostics_page())
            }
            DeviceRequest::RadioTracePage(request) => {
                self.last_radio_trace_cursor = Some(request.after());
                DeviceResponse::RadioTracePage(sample_radio_trace_page())
            }
            DeviceRequest::ManualServiceAnnounce => {
                self.manual_announce_requests += 1;
                DeviceResponse::ManualServiceAnnounce(self.manual_announce_disposition)
            }
            #[cfg(feature = "network-config")]
            DeviceRequest::NetworkConfigGet => {
                DeviceResponse::NetworkConfig(sample_network_config())
            }
            #[cfg(feature = "network-config")]
            DeviceRequest::NetworkConfigMutate(request) => {
                self.last_network_mutation = Some(match request.mutation() {
                    NetworkConfigMutation::UpsertWifi {
                        profile_id,
                        network,
                    } => OwnedNetworkMutation::UpsertWifi {
                        profile_id,
                        enabled: network.enabled(),
                        priority: network.priority(),
                        ssid: network.ssid().as_bytes().to_vec(),
                        replacement_passphrase: network
                            .credential()
                            .replacement()
                            .map(<[u8]>::to_vec),
                        expected_revision: request.expected_revision(),
                        idempotency_key: request.idempotency_key(),
                    },
                    NetworkConfigMutation::ReplaceTcpPeer(peer) => {
                        OwnedNetworkMutation::ReplaceTcpPeer {
                            peer,
                            expected_revision: request.expected_revision(),
                            idempotency_key: request.idempotency_key(),
                        }
                    }
                    NetworkConfigMutation::RemoveWifi { .. } => {
                        panic!("mock does not expect a remove mutation")
                    }
                    NetworkConfigMutation::ReplaceTcpHostPeer(_)
                    | NetworkConfigMutation::SetGatewayPolicy(_)
                    | NetworkConfigMutation::SetRmapConfig(_)
                    | NetworkConfigMutation::SetLoraTxPower(_)
                    | NetworkConfigMutation::SetLoraProfile(_)
                    | NetworkConfigMutation::SetDeviceName(_) => {
                        panic!("mock does not expect this network mutation")
                    }
                });
                DeviceResponse::NetworkConfigMutation(NetworkConfigMutationOutcome::Applied {
                    revision: 8,
                    reboot_required: true,
                })
            }
            #[cfg(feature = "network-config")]
            DeviceRequest::NetworkStatus => DeviceResponse::NetworkStatus(sample_network_status()),
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

#[cfg(feature = "network-config")]
fn sample_network_profile_id() -> WifiNetworkProfileId {
    WifiNetworkProfileId::new([0x91; 16]).unwrap()
}

#[cfg(feature = "network-config")]
fn sample_network_config() -> NetworkConfigSnapshot {
    let wifi = WifiNetworkConfigSummary::new(
        sample_network_profile_id(),
        true,
        17,
        b"field\xffnode",
        true,
    )
    .unwrap();
    let peer_address = ReticulumTcpPeerIpv4Address::new([192, 0, 2, 44]).unwrap();
    let peer = ReticulumTcpPeerConfigSummary::new(true, peer_address, 4242).unwrap();
    NetworkConfigSnapshot::with_defaults(7, [Some(wifi), None, None, None], Some(peer)).unwrap()
}

#[cfg(feature = "network-config")]
fn sample_network_status() -> NetworkRuntimeStatus {
    NetworkRuntimeStatus::new(
        7,
        7,
        WifiStationState::Connected,
        Some(sample_network_profile_id()),
        Some(b"field\xffnode"),
        Some([192, 0, 2, 12]),
        Some(-71),
        ReticulumTcpPeerState::Connecting,
    )
    .unwrap()
}

fn sample_node_diagnostics() -> NodeDiagnosticsSnapshot {
    NodeDiagnosticsSnapshot::new(
        12_345,
        [None, None, None, None],
        None,
        RnsDiagnostics::new(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11),
        2,
        4,
        3,
    )
}

fn sample_probe_success() -> ProbeSuccess {
    ProbeSuccess::new(
        1_345,
        2,
        IngressObservation::new(7, Some(IngressSignal::new(-94, 5))),
    )
}

fn sample_route_diagnostics_page() -> RouteDiagnosticsPage {
    let entry = RouteDiagnosticEntry::new(
        DestinationHash([0x31; 16]),
        Some(IdentityHash::new([0x41; 16])),
        2,
        Some(1),
        RouteDiagnosticResolution::ExactReady,
        Some(100),
        Some(50),
        Some(30_000),
    );
    RouteDiagnosticsPage::new(13, 1, [Some(entry), None, None, None], None).unwrap()
}

fn sample_radio_trace_page() -> RadioTracePage {
    RadioTracePage::new(
        99,
        RadioTraceAppliedLoraProfile::new(
            [0x91; 16],
            915_000_000,
            125_000,
            12,
            22,
            10,
            5,
            true,
            true,
            false,
        ),
        7,
        7,
        false,
        [None; reticulum_device_api::MAX_RADIO_TRACE_PAGE_ENTRIES],
        None,
    )
    .unwrap()
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

    assert_eq!(
        client
            .node_diagnostics()
            .expect("node diagnostics succeeds"),
        sample_node_diagnostics()
    );
    let route_cursor = DestinationHash([0x20; 16]);
    assert_eq!(
        client
            .route_diagnostics_page(RouteDiagnosticsRequest::new(Some(route_cursor)))
            .expect("route diagnostics succeeds"),
        sample_route_diagnostics_page()
    );
    let radio_trace_cursor = RadioTraceCursor::new(99, 6);
    assert_eq!(
        client
            .radio_trace_page(RadioTracePageRequest::new(Some(radio_trace_cursor)))
            .expect("radio trace succeeds"),
        sample_radio_trace_page()
    );

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
    let mailbox = client
        .lxmf_mailbox_status()
        .expect("mailbox status succeeds");
    assert_eq!(mailbox.latest(), Some(summaries[0].handle()));
    assert_eq!(mailbox.acknowledged_through(), None);
    assert_eq!(mailbox.uncollected_count(), 1);
    let mailbox = client
        .lxmf_mailbox_acknowledge(summaries[0].handle())
        .expect("mailbox acknowledgement succeeds");
    assert_eq!(mailbox.acknowledged_through(), Some(summaries[0].handle()));
    assert_eq!(mailbox.uncollected_count(), 0);

    let idempotency_key = IdempotencyKey([0xcc; 16]);
    let location = LxmfMessageLocation::new(
        44_123_456,
        -73_987_654,
        12_345,
        678,
        27_050,
        321,
        1_784_732_099,
    )
    .unwrap();
    let accepted = client
        .lxmf_basic_send(
            BasicLxmfSend::new(
                LXMF_REMOTE,
                1_784_732_100_000,
                b"subject",
                b"body",
                idempotency_key,
            )
            .with_location(location),
        )
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
    assert_eq!(
        client.announce_now().expect("manual announce queues"),
        ManualServiceAnnounceDisposition::Queued
    );
    #[cfg(feature = "network-config")]
    {
        let config = client
            .network_config_get()
            .expect("redacted network config succeeds");
        assert_eq!(config, sample_network_config());

        let network_key = IdempotencyKey([0xee; 16]);
        let profile_id = WifiNetworkProfileId::new([0xa1; 16]).unwrap();
        let network = WifiNetworkUpdate::new(
            true,
            23,
            WifiSsid::new(b"mesh-lab").unwrap(),
            WifiCredentialUpdate::replace(b"correct horse battery staple").unwrap(),
        );
        assert_eq!(
            client
                .network_config_mutate(NetworkConfigMutationRequest::new(
                    NetworkConfigMutation::UpsertWifi {
                        profile_id,
                        network,
                    },
                    7,
                    network_key,
                ))
                .expect("network mutation succeeds"),
            NetworkConfigMutationOutcome::Applied {
                revision: 8,
                reboot_required: true,
            }
        );

        assert_eq!(
            client.network_status().expect("network status succeeds"),
            sample_network_status()
        );
    }
    assert!(client.is_session_available());

    let peer = client.into_transport();
    #[cfg(not(feature = "network-config"))]
    assert_eq!(peer.request_count, 18);
    #[cfg(feature = "network-config")]
    assert_eq!(peer.request_count, 21);
    assert_eq!(
        peer.last_mailbox_acknowledgement,
        Some(summaries[0].handle())
    );
    assert_eq!(peer.last_route_diagnostics_cursor, Some(Some(route_cursor)));
    assert_eq!(peer.last_radio_trace_cursor, Some(Some(radio_trace_cursor)));
    assert_eq!(peer.last_peer_cursor, Some(Some(nearby.next_cursor())));
    assert_eq!(
        peer.last_send,
        Some(OwnedSend {
            destination: LXMF_REMOTE,
            timestamp_unix_ms: 1_784_732_100_000,
            title: b"subject".to_vec(),
            content: b"body".to_vec(),
            location: Some(location),
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
    assert_eq!(peer.manual_announce_requests, 1);
    #[cfg(feature = "network-config")]
    assert_eq!(
        peer.last_network_mutation,
        Some(OwnedNetworkMutation::UpsertWifi {
            profile_id: WifiNetworkProfileId::new([0xa1; 16]).unwrap(),
            enabled: true,
            priority: 23,
            ssid: b"mesh-lab".to_vec(),
            replacement_passphrase: Some(b"correct horse battery staple".to_vec()),
            expected_revision: 7,
            idempotency_key: IdempotencyKey([0xee; 16]),
        })
    );
}

#[test]
fn same_major_future_minor_response_is_accepted() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let config = ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096);
    let mut peer = MockPeer::new();
    peer.response_version = ApiVersion {
        major: ApiVersion::CURRENT.major,
        minor: ApiVersion::CURRENT.minor + 1,
    };
    let mut client = DeviceClient::connect(peer, credential, &mut FixedRng::new(0x99), config)
        .expect("real client/server handshake succeeds");

    assert_eq!(
        client
            .identity_summary()
            .expect("same-major future-minor response is compatible")
            .primary_destination(),
        PRIMARY
    );
}

#[test]
fn ble_gatt_profile_uses_suite_three_across_partial_stream_io() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let config = ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096);
    let mut client = DeviceClient::connect_with_profile(
        MockPeer::new_for_profile(ClientSessionProfile::BleGatt, 3),
        credential,
        &mut FixedRng::new(0x99),
        config,
        ClientSessionProfile::BleGatt,
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
fn manual_service_announce_is_capability_gated_without_consuming_the_session() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let mut peer = MockPeer::new();
    peer.manual_announce_availability = CapabilityAvailability::Disabled;
    let mut client = DeviceClient::connect(
        peer,
        credential,
        &mut FixedRng::new(0x99),
        ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096),
    )
    .expect("handshake succeeds");

    assert!(matches!(
        client.manual_service_announce(),
        Err(ClientError::CapabilityUnavailable {
            operation: super::Operation::ManualServiceAnnounce,
            availability: CapabilityAvailability::Disabled,
        })
    ));
    assert!(client.is_session_available());
    assert_eq!(
        client
            .identity_summary()
            .expect("later authenticated operation succeeds")
            .primary_destination(),
        PRIMARY
    );

    let peer = client.into_transport();
    assert_eq!(peer.request_count, 2);
    assert_eq!(peer.manual_announce_requests, 0);
}

#[test]
fn manual_service_announce_returns_coalesced_device_disposition() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let mut peer = MockPeer::new();
    peer.manual_announce_disposition = ManualServiceAnnounceDisposition::AlreadyPending;
    let mut client = DeviceClient::connect(
        peer,
        credential,
        &mut FixedRng::new(0x99),
        ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096),
    )
    .expect("handshake succeeds");

    assert_eq!(
        client
            .manual_service_announce()
            .expect("available operation succeeds"),
        ManualServiceAnnounceDisposition::AlreadyPending
    );

    let peer = client.into_transport();
    assert_eq!(peer.request_count, 2);
    assert_eq!(peer.manual_announce_requests, 1);
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

#[test]
fn reticulum_probe_start_and_poll_capture_exact_requests_and_return_typed_responses() {
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let mut client = DeviceClient::connect(
        MockPeer::new(),
        credential,
        &mut FixedRng::new(0x99),
        ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096),
    )
    .expect("handshake succeeds");
    let request = ProbeStartRequest::new(NOMAD_REMOTE, IdempotencyKey([0xa2; 16]));

    assert_eq!(
        client
            .reticulum_probe_start(request)
            .expect("probe start succeeds"),
        ProbeStartAccepted::new(PROBE_ID, ProbeStartOutcome::Accepted)
    );
    assert_eq!(
        client
            .reticulum_probe_poll(PROBE_ID)
            .expect("probe poll succeeds"),
        ProbePollResponse::Succeeded(sample_probe_success())
    );
    assert!(client.is_session_available());

    let peer = client.into_transport();
    assert_eq!(peer.request_count, 2);
    assert_eq!(peer.last_probe_start, Some(request));
    assert_eq!(peer.last_probe_poll, Some(PROBE_ID));
}

#[test]
fn reticulum_probe_start_and_poll_preserve_typed_api_errors() {
    let start_error = ApiErrorResponse {
        code: ApiErrorCode::IdempotencyConflict,
        operation: Some(reticulum_device_api::OP_RETICULUM_PROBE_START),
    };
    let poll_error = ApiErrorResponse {
        code: ApiErrorCode::NotFound,
        operation: Some(reticulum_device_api::OP_RETICULUM_PROBE_POLL),
    };
    let mut peer = MockPeer::new();
    peer.probe_start_response = DeviceResponse::Error(start_error);
    peer.probe_poll_response = DeviceResponse::Error(poll_error);
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let mut client = DeviceClient::connect(
        peer,
        credential,
        &mut FixedRng::new(0x99),
        ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096),
    )
    .expect("handshake succeeds");
    let request = ProbeStartRequest::new(NOMAD_REMOTE, IdempotencyKey([0xa3; 16]));

    assert!(matches!(
        client.reticulum_probe_start(request),
        Err(ClientError::Api {
            operation: super::Operation::ReticulumProbeStart,
            error,
        }) if error == start_error
    ));
    assert!(matches!(
        client.reticulum_probe_poll(PROBE_ID),
        Err(ClientError::Api {
            operation: super::Operation::ReticulumProbePoll,
            error,
        }) if error == poll_error
    ));
    assert!(client.is_session_available());

    let peer = client.into_transport();
    assert_eq!(peer.request_count, 2);
    assert_eq!(peer.last_probe_start, Some(request));
    assert_eq!(peer.last_probe_poll, Some(PROBE_ID));
}

#[test]
fn reticulum_probe_start_and_poll_reject_unexpected_authenticated_response_kinds() {
    let unexpected_start = DeviceResponse::IdentitySummary(
        IdentitySummary::with_lxmf_delivery_destination(PRIMARY, LXMF_LOCAL),
    );
    let unexpected_poll =
        DeviceResponse::ManualServiceAnnounce(ManualServiceAnnounceDisposition::Queued);
    let mut peer = MockPeer::new();
    peer.probe_start_response = unexpected_start;
    peer.probe_poll_response = unexpected_poll;
    let credential = ActivatedCredential::decode(&active_state()).expect("credential decodes");
    let mut client = DeviceClient::connect(
        peer,
        credential,
        &mut FixedRng::new(0x99),
        ClientConfig::new(Duration::from_secs(1), Duration::from_secs(1), 8, 4_096),
    )
    .expect("handshake succeeds");
    let request = ProbeStartRequest::new(NOMAD_REMOTE, IdempotencyKey([0xa4; 16]));

    assert!(matches!(
        client.reticulum_probe_start(request),
        Err(ClientError::UnexpectedResponse {
            operation: super::Operation::ReticulumProbeStart,
            kind,
        }) if kind == unexpected_start.kind()
    ));
    assert!(matches!(
        client.reticulum_probe_poll(PROBE_ID),
        Err(ClientError::UnexpectedResponse {
            operation: super::Operation::ReticulumProbePoll,
            kind,
        }) if kind == unexpected_poll.kind()
    ));
    assert!(client.is_session_available());

    let peer = client.into_transport();
    assert_eq!(peer.request_count, 2);
    assert_eq!(peer.last_probe_start, Some(request));
    assert_eq!(peer.last_probe_poll, Some(PROBE_ID));
}
