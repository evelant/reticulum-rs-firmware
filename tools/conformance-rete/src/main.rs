use std::{collections::BTreeSet, process::ExitCode};

use rand_core::{CryptoRng, RngCore};
use reticulum_radio_interface::{RNODE_HW_MTU, ReceiveOutcome, RnodeRxReassembler};
use reticulum_rns_rete::{
    DestHash, DestType, DestinationType, Direction, EmbeddedNodeConfig, Identity,
    IngressDisposition, InitialEmbeddedNode, InterfaceId, LinkState, NodeEvent, PacketType,
    TxTarget, build_announce_packet, decode_lrrtt_number_for_conformance,
    encode_lrrtt_float64_for_conformance, identity_from_private_key, new_conformance_node,
    new_conformance_transport_node, parse_announce_packet, parse_packet,
    select_lrrtt_for_conformance,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const RELEASED_VECTORS: &str = include_str!("../../../interop/vectors/rns-1.3.8.json");
const PEER_MANIFEST: &str = include_str!("../../../interop/peers.toml");
const GENERATOR_SOURCE: &[u8] = include_bytes!("../../../interop/python/generate_rns_vectors.py");
const REQUIREMENTS_SOURCE: &[u8] =
    include_bytes!("../../../interop/python/requirements-rns-1.3.8.txt");
const RELEASED_RNS_VERSION: &str = "1.3.8";
const RELEASED_RNS_REVISION: &str = "dca2a9282935d3dd251f2a1588daa41b5deee8c7";

#[derive(Debug, Deserialize)]
struct Corpus {
    schema: u8,
    protocol: String,
    lane: String,
    peer: Peer,
    generator: Generator,
    identity: IdentityVector,
    announce: AnnounceVector,
    plain_data: PlainDataVector,
    lrrtt_messagepack: LrrttMessagepackVector,
}

#[derive(Debug, Deserialize)]
struct Generator {
    script: String,
    requirements: String,
    command: String,
    python_version: String,
    source_sha256: String,
    requirements_sha256: String,
    deterministic: bool,
    notes: String,
}

#[derive(Debug, Deserialize)]
struct Peer {
    package: String,
    version: String,
    repository: String,
    revision: String,
}

#[derive(Debug, Deserialize)]
struct IdentityVector {
    label: String,
    origin: String,
    normalization: String,
    test_only: bool,
    private_key_hex: String,
    public_key_hex: String,
    identity_hash_hex: String,
}

#[derive(Debug, Deserialize)]
struct AnnounceVector {
    app_name: String,
    aspects: Vec<String>,
    origin: String,
    normalization: String,
    fixed_timestamp: u64,
    destination_hash_hex: String,
    name_hash_hex: String,
    random_hash_hex: String,
    signature_hex: String,
    raw_hex: String,
    packet_hash_hex: String,
}

#[derive(Debug, Deserialize)]
struct PlainDataVector {
    app_name: String,
    aspects: Vec<String>,
    origin: String,
    normalization: String,
    payload_hex: String,
    destination_hash_hex: String,
    raw_hex: String,
    packet_hash_hex: String,
}

#[derive(Debug, Deserialize)]
struct LrrttMessagepackVector {
    origin: String,
    scope: String,
    umsgpack_version: String,
    umsgpack_source_sha256: String,
    measured_rtt_decimal: String,
    rns_rtt_formula: String,
    canonical_float64: Vec<LrrttCase>,
    decode_cases: Vec<LrrttCase>,
}

#[derive(Debug, Deserialize)]
struct LrrttCase {
    name: String,
    wire_hex: String,
    #[serde(default)]
    input_decimal: Option<String>,
    #[serde(default)]
    first_object_wire_hex: Option<String>,
    #[serde(default)]
    trailing_hex: Option<String>,
    python_unpack: PythonValueOutcome,
    python_rns_rtt_formula: PythonValueOutcome,
}

#[derive(Debug, Deserialize)]
struct PythonValueOutcome {
    result: String,
    #[serde(default)]
    value_class: Option<String>,
    #[serde(default)]
    f64_bits_hex: Option<String>,
    #[serde(default)]
    decimal: Option<String>,
    #[serde(default)]
    value: Option<bool>,
}

#[derive(Default)]
struct ZeroRng;

impl RngCore for ZeroRng {
    fn next_u32(&mut self) -> u32 {
        0
    }

    fn next_u64(&mut self) -> u64 {
        0
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        destination.fill(0);
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for ZeroRng {}

#[derive(Default)]
struct CounterRng(u8);

impl RngCore for CounterRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        for byte in destination {
            self.0 = self.0.wrapping_add(1);
            *byte = self.0;
        }
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for CounterRng {}

fn main() -> ExitCode {
    match verify_released_vectors() {
        Ok(checks) => {
            let metadata = reticulum_rns_rete::metadata();
            println!("candidate={}", metadata.id);
            println!("source={}", metadata.source);
            println!("revision={}", metadata.revision);
            println!("license={}", metadata.license);
            println!("status={:?}", metadata.status);
            println!("peer=rns@{RELEASED_RNS_VERSION}:{RELEASED_RNS_REVISION}");
            println!("checks={checks}");
            println!("result=pass");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("result=fail: {error}");
            ExitCode::FAILURE
        }
    }
}

fn verify_released_vectors() -> Result<usize, String> {
    let corpus: Corpus = serde_json::from_str(RELEASED_VECTORS)
        .map_err(|error| format!("invalid released vector JSON: {error}"))?;
    let mut checks = 0;

    require(corpus.schema == 1, "unsupported vector schema")?;
    require(corpus.protocol == "Reticulum", "wrong protocol")?;
    require(corpus.lane == "released", "wrong compatibility lane")?;
    require(corpus.peer.package == "rns", "wrong Python package")?;
    require(
        corpus.peer.version == RELEASED_RNS_VERSION,
        "released RNS version drift",
    )?;
    require(
        corpus.peer.revision == RELEASED_RNS_REVISION,
        "released RNS revision drift",
    )?;
    require(
        corpus.peer.repository == "https://github.com/markqvist/Reticulum.git",
        "released RNS repository drift",
    )?;
    require(
        PEER_MANIFEST.contains(
            "[reticulum.released]\n\
             version = \"1.3.8\"\n\
             repository = \"https://github.com/markqvist/Reticulum.git\"\n\
             revision = \"dca2a9282935d3dd251f2a1588daa41b5deee8c7\"",
        ),
        "interop/peers.toml and the released corpus disagree",
    )?;
    checks += 8;

    require(
        corpus.generator.script == "interop/python/generate_rns_vectors.py",
        "wrong generator path",
    )?;
    require(
        corpus.generator.requirements == "interop/python/requirements-rns-1.3.8.txt",
        "wrong generator requirements path",
    )?;
    require(
        corpus.generator.command == "python interop/python/generate_rns_vectors.py",
        "wrong generator command",
    )?;
    require(
        corpus.generator.python_version == "3.13.7",
        "wrong generator Python version",
    )?;
    require(
        corpus.generator.deterministic,
        "generator must declare deterministic output",
    )?;
    require(
        corpus.generator.notes
            == "The corpus excludes encrypted packet creation because Python correctly uses a fresh ephemeral key and IV. LRRTT cases record released-peer decode and RTT-formula behavior; firmware checks pending-handshake payload parity separately.",
        "unexpected generator normalization notes",
    )?;
    require(
        corpus.generator.source_sha256 == sha256_hex(GENERATOR_SOURCE),
        "generator source hash drift",
    )?;
    require(
        corpus.generator.requirements_sha256 == sha256_hex(REQUIREMENTS_SOURCE),
        "generator requirements hash drift",
    )?;
    checks += 8;

    checks += verify_lrrtt_messagepack(&corpus.lrrtt_messagepack)?;

    let private_key = decode_hex("identity.private_key_hex", &corpus.identity.private_key_hex)?;
    let identity = identity_from_private_key(&private_key)
        .map_err(|error| format!("Rete rejected Python identity: {error}"))?;
    require(
        corpus.identity.label == "alice",
        "unexpected fixture identity",
    )?;
    require(
        corpus.identity.test_only,
        "fixture identity is not test-only",
    )?;
    require(
        corpus.identity.origin == "created by Python RNS 1.3.8 from the SHA-512 of the test label",
        "unexpected identity origin",
    )?;
    require(
        corpus.identity.normalization == "lowercase hexadecimal; no wire-byte normalization",
        "unexpected identity normalization",
    )?;
    require_hex_eq(
        "identity public key",
        &identity.public_key(),
        &corpus.identity.public_key_hex,
    )?;
    require_hex_eq(
        "identity hash",
        identity.hash().as_ref(),
        &corpus.identity.identity_hash_hex,
    )?;
    checks += 6;

    let aspects: Vec<&str> = corpus.announce.aspects.iter().map(String::as_str).collect();
    let expected_announce = decode_hex("announce.raw_hex", &corpus.announce.raw_hex)?;
    let mut actual_announce = [0u8; reticulum_rns_conformance::RNS_MTU];
    let actual_len = build_announce_packet(
        &identity,
        &corpus.announce.app_name,
        &aspects,
        None,
        &mut ZeroRng,
        corpus.announce.fixed_timestamp,
        &mut actual_announce,
    )
    .map_err(|error| format!("Rete could not build Python announce: {error}"))?;
    require(
        actual_announce[..actual_len] == expected_announce,
        "announce wire bytes differ from Python RNS 1.3.8",
    )?;

    let announce = parse_announce_packet(&expected_announce)
        .map_err(|error| format!("Rete rejected Python announce: {error}"))?;
    require(
        announce.packet.packet_type == PacketType::Announce,
        "announce parsed with the wrong packet type",
    )?;
    require_hex_eq(
        "announce destination hash",
        announce.packet.destination_hash,
        &corpus.announce.destination_hash_hex,
    )?;
    require_hex_eq(
        "announce packet hash",
        announce.packet.compute_hash().as_slice(),
        &corpus.announce.packet_hash_hex,
    )?;
    require_hex_eq(
        "announce name hash",
        announce.fields.name_hash,
        &corpus.announce.name_hash_hex,
    )?;
    require_hex_eq(
        "announce random hash",
        announce.fields.random_hash,
        &corpus.announce.random_hash_hex,
    )?;
    require_hex_eq(
        "announce signature",
        announce.fields.signature,
        &corpus.announce.signature_hex,
    )?;
    require(
        corpus.announce.origin == "created and packed by Python RNS 1.3.8",
        "unexpected announce origin",
    )?;
    require(
        corpus.announce.normalization
            == "fixed five-byte random prefix and timestamp for reproducibility",
        "unexpected announce normalization",
    )?;
    checks += 9;

    require(
        corpus.plain_data.app_name == "reticulum_rs_firmware",
        "unexpected plain fixture app name",
    )?;
    require(
        corpus.plain_data.aspects == ["phase0"],
        "unexpected plain fixture aspects",
    )?;
    let raw_plain = decode_hex("plain_data.raw_hex", &corpus.plain_data.raw_hex)?;
    let plain = parse_packet(&raw_plain)
        .map_err(|error| format!("Rete rejected Python plain packet: {error}"))?;
    require(
        plain.packet_type == PacketType::Data,
        "plain packet parsed with the wrong packet type",
    )?;
    require_hex_eq(
        "plain destination hash",
        plain.destination_hash,
        &corpus.plain_data.destination_hash_hex,
    )?;
    require_hex_eq(
        "plain payload",
        plain.payload,
        &corpus.plain_data.payload_hex,
    )?;
    require_hex_eq(
        "plain packet hash",
        plain.compute_hash().as_slice(),
        &corpus.plain_data.packet_hash_hex,
    )?;
    require(
        corpus.plain_data.origin == "created and packed by Python RNS 1.3.8",
        "unexpected plain packet origin",
    )?;
    require(
        corpus.plain_data.normalization == "lowercase hexadecimal; no wire-byte normalization",
        "unexpected plain packet normalization",
    )?;
    checks += 8;

    // Exercise the current Python fixtures through the owning embedded node,
    // rather than stopping at packet parsing and standalone signature checks.
    let plain_aspects: Vec<&str> = corpus
        .plain_data
        .aspects
        .iter()
        .map(String::as_str)
        .collect();
    let mut plain_node = new_conformance_node(&[0x31; 32])
        .map_err(|error| format!("Rete could not construct plain fixture node: {error}"))?;
    let registered_plain = plain_node
        .register_destination(
            &corpus.plain_data.app_name,
            &plain_aspects,
            DestinationType::Plain,
            Direction::In,
        )
        .map_err(|error| format!("Rete could not register Python plain destination: {error}"))?;
    require_hex_eq(
        "registered plain destination hash",
        registered_plain.as_ref(),
        &corpus.plain_data.destination_hash_hex,
    )?;

    let plain_report = plain_node.ingest(&raw_plain, 1_700_000_001, InterfaceId(7), &mut ZeroRng);
    require(
        plain_report.actions.packets.is_empty(),
        "plain fixture emitted packets",
    )?;
    require(
        plain_report.actions.events.len() == 1,
        "plain fixture emitted the wrong event count",
    )?;
    match plain_report.actions.events.first() {
        Some(NodeEvent::DataReceived { dest_hash, payload }) => {
            require(
                *dest_hash == registered_plain,
                "plain event destination mismatch",
            )?;
            require_hex_eq(
                "plain event payload",
                payload,
                &corpus.plain_data.payload_hex,
            )?;
        }
        _ => return Err("plain fixture did not emit DataReceived".to_owned()),
    }
    checks += 5;

    // Treat the Python announce as traffic arriving from a real peer. The
    // first owning node is a relay; the second learns the relayed route and
    // then originates encrypted DATA back through it. ZeroRng makes all jitter
    // and ephemeral-key choices deterministic while still running real crypto.
    let announce_dest = announce.packet.destination_hash;
    let announce_dest_hash = DestHash::from_slice(announce_dest);
    let announce_identity = announce.fields.identity_hash;
    let mut relay = new_conformance_transport_node(&[0x52; 32])
        .map_err(|error| format!("Rete could not construct relay node: {error}"))?;
    let relay_identity = relay.identity_hash();

    // Put the released-Python packet through the product-owned RNode receive
    // boundary before NodeCore sees it. The announce fits one physical frame;
    // split and hostile-frame behavior is covered exhaustively in that crate.
    let mut radio_frame = Vec::with_capacity(expected_announce.len() + 1);
    radio_frame.push(0xa0); // sequence 10, no split flag
    radio_frame.extend_from_slice(&expected_announce);
    let mut radio_reassembler = RnodeRxReassembler::new();
    let mut radio_packet = [0u8; RNODE_HW_MTU];
    let radio_packet_len = match radio_reassembler
        .feed(&radio_frame, &mut radio_packet)
        .map_err(|error| format!("RNode RX rejected Python announce: {error:?}"))?
    {
        ReceiveOutcome::Complete {
            packet_len,
            discarded_pending,
        } => {
            require(
                !discarded_pending,
                "single-frame Python announce discarded pending state",
            )?;
            packet_len
        }
        ReceiveOutcome::AwaitingContinuation { .. } => {
            return Err("single-frame Python announce was treated as split".to_owned());
        }
    };
    require(
        radio_packet_len == expected_announce.len(),
        "RNode RX changed Python announce length",
    )?;
    require(
        radio_packet[..radio_packet_len] == expected_announce,
        "RNode RX changed Python announce bytes",
    )?;
    require(
        radio_reassembler.pending().is_none(),
        "RNode RX retained state after a complete frame",
    )?;
    checks += 4;

    let relay_report = relay.ingest(
        &radio_packet[..radio_packet_len],
        1_700_000_001,
        InterfaceId(1),
        &mut ZeroRng,
    );
    require(
        relay_report.actions.events.len() == 1,
        "relay emitted the wrong event count",
    )?;
    match relay_report.actions.events.first() {
        Some(NodeEvent::AnnounceReceived {
            dest_hash,
            identity_hash,
            hops,
            app_data,
        }) => {
            require(
                dest_hash.as_ref() == announce_dest,
                "relay announce destination mismatch",
            )?;
            require(
                *identity_hash == announce_identity,
                "relay announce identity mismatch",
            )?;
            require(*hops == 1, "relay announce hop count mismatch")?;
            require(
                app_data.is_none(),
                "relay announce unexpectedly carried app data",
            )?;
        }
        _ => return Err("Python announce did not emit AnnounceReceived on relay".to_owned()),
    }
    require(
        relay_report.actions.packets.len() == 1,
        "relay did not emit one announce forward",
    )?;
    require(
        relay_report.actions.packets[0].target() == TxTarget::All,
        "relay announce used unexpected routing",
    )?;
    let relayed_announce = parse_packet(relay_report.actions.packets[0].bytes())
        .map_err(|error| format!("Rete emitted an invalid relayed announce: {error}"))?;
    require(
        relayed_announce.transport_id == Some(relay_identity.as_ref()),
        "relayed announce did not identify the relay",
    )?;
    require(
        relayed_announce.hops == 1,
        "relayed announce hop count mismatch",
    )?;
    checks += 9;

    let mut sender = new_conformance_node(&[0x73; 32])
        .map_err(|error| format!("Rete could not construct sender node: {error}"))?;
    let sender_report = sender.ingest(
        relay_report.actions.packets[0].bytes(),
        1_700_000_002,
        InterfaceId(2),
        &mut ZeroRng,
    );
    require(
        sender_report.actions.events.len() == 1,
        "sender emitted the wrong event count",
    )?;
    match sender_report.actions.events.first() {
        Some(NodeEvent::AnnounceReceived {
            dest_hash, hops, ..
        }) => {
            require(
                dest_hash.as_ref() == announce_dest,
                "sender learned wrong destination",
            )?;
            require(*hops == 2, "sender learned wrong relayed hop count")?;
        }
        _ => return Err("sender did not observe the relayed announce".to_owned()),
    }
    let learned_path = sender
        .route(&announce_dest_hash)
        .ok_or_else(|| "sender did not retain the relayed path".to_owned())?;
    require(
        learned_path.via == Some(relay_identity),
        "sender path does not point through relay",
    )?;
    require(
        learned_path.hops == 2,
        "sender path retained wrong hop count",
    )?;

    let outbound = sender
        .send_data(
            &announce_dest_hash,
            b"rete live path",
            1_700_000_003,
            &mut ZeroRng,
        )
        .map_err(|error| format!("sender could not build live-path DATA: {error}"))?;
    let outbound_packet = parse_packet(outbound.bytes())
        .map_err(|error| format!("sender emitted invalid live-path DATA: {error}"))?;
    require(
        outbound.target() == TxTarget::Only(InterfaceId(2)),
        "sender DATA did not retain its learned origin interface",
    )?;
    require(
        outbound_packet.transport_id == Some(relay_identity.as_ref()),
        "sender DATA did not target relay",
    )?;

    let forwarded = relay.ingest(
        outbound.bytes(),
        1_700_000_004,
        InterfaceId(2),
        &mut ZeroRng,
    );
    require(
        forwarded.actions.events.is_empty(),
        "relay emitted a local event for transit DATA",
    )?;
    require(
        forwarded.actions.packets.len() == 1,
        "relay did not forward transit DATA",
    )?;
    require(
        forwarded.actions.packets[0].target() == TxTarget::Only(InterfaceId(1)),
        "relay DATA used unexpected routing",
    )?;
    let forwarded_packet = parse_packet(forwarded.actions.packets[0].bytes())
        .map_err(|error| format!("relay emitted invalid forwarded DATA: {error}"))?;
    require(
        forwarded_packet.transport_id.is_none(),
        "relay did not remove its transport header",
    )?;
    require(
        forwarded_packet.destination_hash == announce_dest,
        "relay forwarded DATA to the wrong destination",
    )?;
    require(
        relay.metrics().transport.packets_forwarded == 1,
        "relay forwarding counter did not advance",
    )?;
    checks += 13;

    // Complete the path at the identity that emitted the pinned Python
    // announce. This proves Rete's native endpoint can decrypt DATA addressed
    // using the public key learned from that announce after the relay converts
    // H2 back to H1.
    let mut python_endpoint = InitialEmbeddedNode::new(
        identity,
        &corpus.announce.app_name,
        &aspects,
        EmbeddedNodeConfig::endpoint(),
    )
    .map_err(|error| format!("Rete could not construct Python identity endpoint: {error}"))?;
    require(
        python_endpoint.destination_hash() == announce_dest_hash,
        "Python identity endpoint destination mismatch",
    )?;
    let endpoint_report = python_endpoint.ingest(
        forwarded.actions.packets[0].bytes(),
        1_700_000_005,
        InterfaceId(3),
        &mut ZeroRng,
    );
    require(
        endpoint_report.actions.packets.is_empty(),
        "endpoint emitted packets for unproved DATA",
    )?;
    require(
        endpoint_report.actions.events.len() == 1,
        "endpoint emitted the wrong event count",
    )?;
    match endpoint_report.actions.events.first() {
        Some(NodeEvent::DataReceived { dest_hash, payload }) => {
            require(
                *dest_hash == announce_dest_hash,
                "decrypted DATA destination mismatch",
            )?;
            require(
                payload == b"rete live path",
                "decrypted DATA payload mismatch",
            )?;
        }
        _ => return Err("endpoint did not emit DataReceived for relayed DATA".to_owned()),
    }
    checks += 5;

    // Exercise the complete owning-adapter Link path with deterministic
    // identities and entropy. The responder and initiator use different local
    // interface IDs so source-relative LRPROOF and channel-proof routing must
    // be resolved before the ingress report leaves the adapter.
    let responder_identity = Identity::from_seed(&[0x92; 32])
        .map_err(|error| format!("Rete could not construct Link responder identity: {error}"))?;
    let mut link_initiator = new_conformance_node(&[0x91; 32])
        .map_err(|error| format!("Rete could not construct Link initiator: {error}"))?;
    let mut link_responder = new_conformance_node(&[0x92; 32])
        .map_err(|error| format!("Rete could not construct Link responder: {error}"))?;
    link_initiator
        .register_peer(&responder_identity, "reticulum", &["phase0"], 1_700_000_100)
        .map_err(|error| format!("Rete could not register Link responder: {error}"))?;
    let mut link_rng = CounterRng::default();
    let responder_interface = InterfaceId(9);
    let initiator_interface = InterfaceId(10);

    let (link_request, link_id) = link_initiator
        .initiate_link(
            link_responder.destination_hash(),
            1_700_000_100,
            &mut link_rng,
        )
        .map_err(|error| format!("Link initiator could not build LINKREQUEST: {error}"))?;
    require(
        link_request.target() == TxTarget::All,
        "LINKREQUEST used unexpected routing",
    )?;
    let parsed_link_request = parse_packet(link_request.bytes())
        .map_err(|error| format!("Rete emitted invalid LINKREQUEST: {error}"))?;
    require(
        parsed_link_request.packet_type == PacketType::LinkRequest,
        "Link initiation emitted the wrong packet type",
    )?;
    require(
        parsed_link_request.context == 0,
        "LINKREQUEST used the wrong context",
    )?;
    require(
        link_initiator.link_state(&link_id) == Some(LinkState::Handshake),
        "initiator did not retain LINKREQUEST handshake state",
    )?;
    checks += 4;

    let link_response = link_responder.ingest(
        link_request.bytes(),
        1_700_000_100,
        responder_interface,
        &mut link_rng,
    );
    require(
        link_response.actions.events.is_empty(),
        "responder announced Link establishment before LRRTT",
    )?;
    require(
        link_responder.link_state(&link_id) == Some(LinkState::Handshake),
        "responder did not retain LINKREQUEST handshake state",
    )?;
    require(
        link_response.actions.packets.len() == 1,
        "responder did not emit exactly one LRPROOF",
    )?;
    require(
        link_response.actions.packets[0].target() == TxTarget::Only(responder_interface),
        "LRPROOF was not resolved to its source interface",
    )?;
    let link_proof = parse_packet(link_response.actions.packets[0].bytes())
        .map_err(|error| format!("Rete emitted invalid LRPROOF: {error}"))?;
    require(
        link_proof.packet_type == PacketType::Proof,
        "LRPROOF used the wrong packet type",
    )?;
    require(link_proof.context == 0xff, "LRPROOF used the wrong context")?;
    checks += 6;

    let initiator_established = link_initiator.ingest(
        link_response.actions.packets[0].bytes(),
        1_700_000_101,
        initiator_interface,
        &mut link_rng,
    );
    require(
        matches!(
            initiator_established.actions.events.as_slice(),
            [NodeEvent::LinkEstablished { link_id: event_id }] if *event_id == link_id
        ),
        "initiator did not emit LinkEstablished for LRPROOF",
    )?;
    require(
        link_initiator.link_state(&link_id) == Some(LinkState::Active),
        "initiator did not become active after LRPROOF",
    )?;
    require(
        initiator_established.actions.packets.len() == 1,
        "initiator did not emit exactly one LRRTT",
    )?;
    require(
        initiator_established.actions.packets[0].target() == TxTarget::Only(initiator_interface),
        "LRRTT used unexpected routing",
    )?;
    let lrrtt = parse_packet(initiator_established.actions.packets[0].bytes())
        .map_err(|error| format!("Rete emitted invalid LRRTT: {error}"))?;
    require(
        lrrtt.packet_type == PacketType::Data,
        "LRRTT used the wrong packet type",
    )?;
    require(lrrtt.context == 0xfe, "LRRTT used the wrong context")?;
    checks += 6;

    let responder_established = link_responder.ingest(
        initiator_established.actions.packets[0].bytes(),
        1_700_000_102,
        responder_interface,
        &mut link_rng,
    );
    require(
        matches!(
            responder_established.actions.events.as_slice(),
            [NodeEvent::LinkEstablished { link_id: event_id }] if *event_id == link_id
        ),
        "responder did not emit LinkEstablished for LRRTT",
    )?;
    require(
        link_responder.link_state(&link_id) == Some(LinkState::Active),
        "responder did not become active after LRRTT",
    )?;
    require(
        responder_established.actions.packets.is_empty(),
        "responder emitted an unexpected packet after LRRTT",
    )?;
    checks += 3;

    let encrypted_link_data = link_initiator
        .send_link_data(
            &link_id,
            b"embedded Link payload",
            1_700_000_110,
            &mut link_rng,
        )
        .map_err(|error| format!("initiator could not send encrypted Link DATA: {error}"))?;
    require(
        encrypted_link_data.target() == TxTarget::Only(initiator_interface),
        "encrypted Link DATA used unexpected routing",
    )?;
    let parsed_link_data = parse_packet(encrypted_link_data.bytes())
        .map_err(|error| format!("Rete emitted invalid encrypted Link DATA: {error}"))?;
    require(
        parsed_link_data.packet_type == PacketType::Data,
        "encrypted Link DATA used the wrong packet type",
    )?;
    require(
        parsed_link_data.context == 0,
        "encrypted Link DATA used the wrong context",
    )?;
    let received_link_data = link_responder.ingest(
        encrypted_link_data.bytes(),
        1_700_000_110,
        responder_interface,
        &mut link_rng,
    );
    require(
        matches!(
            received_link_data.actions.events.as_slice(),
            [NodeEvent::LinkData { link_id: event_id, data, context }]
                if *event_id == link_id && data == b"embedded Link payload" && *context == 0
        ),
        "responder did not emit decrypted LinkData",
    )?;
    require(
        received_link_data.actions.packets.is_empty(),
        "best-effort Link DATA unexpectedly emitted a proof",
    )?;
    checks += 5;

    let channel_data = link_initiator
        .send_channel_message(
            &link_id,
            0x4242,
            b"reliable Link payload",
            1_700_000_120,
            &mut link_rng,
        )
        .map_err(|error| format!("initiator could not send channel message: {error}"))?;
    require(
        channel_data.target() == TxTarget::Only(initiator_interface),
        "channel DATA used unexpected routing",
    )?;
    require(
        link_initiator.metrics().capacity.channel_receipts.used == 1,
        "channel send did not retain its receipt",
    )?;
    let channel_packet_hash = parse_packet(channel_data.bytes())
        .map_err(|error| format!("Rete emitted invalid channel DATA: {error}"))?
        .compute_hash();
    let received_channel = link_responder.ingest(
        channel_data.bytes(),
        1_700_000_120,
        responder_interface,
        &mut link_rng,
    );
    require(
        matches!(
            received_channel.actions.events.as_slice(),
            [NodeEvent::ChannelMessages { link_id: event_id, messages }]
                if *event_id == link_id
                    && messages.as_slice() == [(0x4242, b"reliable Link payload".to_vec())]
        ),
        "responder did not emit the reliable channel message",
    )?;
    require(
        received_channel.actions.packets.len() == 1,
        "channel message did not produce exactly one proof",
    )?;
    require(
        received_channel.actions.packets[0].target() == TxTarget::Only(responder_interface),
        "channel proof was not resolved to its source interface",
    )?;

    // Lose the initial proof. The retry must retain exactly one receipt while
    // replacing its sole valid proof target with fresh ciphertext/hash.
    let channel_retry = link_initiator.tick(1_700_000_136, &mut link_rng);
    require(
        channel_retry.packets.len() == 1,
        "timed-out channel DATA did not emit exactly one retry",
    )?;
    require(
        channel_retry.unroutable_packets == 0
            && channel_retry.packets[0].target() == TxTarget::Only(initiator_interface),
        "channel retry did not retain its bound interface",
    )?;
    let parsed_retry = parse_packet(channel_retry.packets[0].bytes())
        .map_err(|error| format!("Rete emitted invalid channel retry: {error}"))?;
    require(
        parsed_retry.context == 0x0e,
        "channel retry used the wrong context",
    )?;
    let retry_packet_hash = parsed_retry.compute_hash();
    require(
        retry_packet_hash != channel_packet_hash,
        "channel retry reused the initial packet hash",
    )?;
    require(
        link_initiator.metrics().capacity.channel_receipts.used == 1,
        "channel retry did not replace its receipt in place",
    )?;

    let received_retry = link_responder.ingest(
        channel_retry.packets[0].bytes(),
        1_700_000_136,
        responder_interface,
        &mut link_rng,
    );
    require(
        received_retry.actions.events.is_empty(),
        "duplicate channel envelope was delivered twice",
    )?;
    require(
        received_retry.actions.packets.len() == 1
            && received_retry.actions.packets[0].target() == TxTarget::Only(responder_interface),
        "duplicate channel envelope did not emit one bound replacement proof",
    )?;

    let obsolete_proof = link_initiator.ingest(
        received_channel.actions.packets[0].bytes(),
        1_700_000_137,
        initiator_interface,
        &mut link_rng,
    );
    require(
        obsolete_proof.actions.events.is_empty()
            && link_initiator.metrics().capacity.channel_receipts.used == 1,
        "obsolete channel proof completed or removed the replacement receipt",
    )?;
    checks += 8;

    let channel_proof = parse_packet(received_retry.actions.packets[0].bytes())
        .map_err(|error| format!("Rete emitted invalid channel proof: {error}"))?;
    require(
        channel_proof.packet_type == PacketType::Proof,
        "channel proof used the wrong packet type",
    )?;
    require(
        channel_proof.context == 0,
        "channel proof used the wrong context",
    )?;
    require(
        channel_proof.dest_type == DestType::Link,
        "channel proof used the wrong destination type",
    )?;
    require(
        channel_proof.destination_hash == link_id.as_ref(),
        "channel proof was not addressed to its Link ID",
    )?;
    let received_proof = link_initiator.ingest(
        received_retry.actions.packets[0].bytes(),
        1_700_000_138,
        initiator_interface,
        &mut link_rng,
    );
    require(
        matches!(
            received_proof.actions.events.as_slice(),
            [NodeEvent::ProofReceived { .. }]
        ),
        "channel proof did not emit ProofReceived",
    )?;
    let received_packet_hash = match received_proof.actions.events.as_slice() {
        [NodeEvent::ProofReceived { packet_hash }] => *packet_hash,
        _ => unreachable!("ProofReceived shape was checked above"),
    };
    require(
        received_packet_hash == retry_packet_hash,
        "channel proof acknowledged the wrong DATA packet hash",
    )?;
    require(
        link_initiator.metrics().capacity.channel_receipts.used == 0,
        "channel proof did not clear its receipt",
    )?;
    let after_ack = link_initiator.tick(1_700_000_154, &mut link_rng);
    require(
        !after_ack
            .packets
            .iter()
            .any(|packet| parse_packet(packet.bytes()).is_ok_and(|parsed| parsed.context == 0x0e)),
        "acknowledged channel DATA remained pending and retransmitted",
    )?;
    checks += 13;

    // Drive the established Link through a deliberately late watchdog tick.
    // Native tick preparation must emit the initiator's final probe before the
    // watchdog transitions the retained Link into its revival window.
    let late_keepalive_at = 1_700_001_000;
    let stale_keepalive = link_initiator.tick(late_keepalive_at, &mut link_rng);
    require(
        stale_keepalive.packets.len() == 1,
        "late initiator tick did not emit exactly one keepalive",
    )?;
    require(
        stale_keepalive.unroutable_packets == 0,
        "late initiator keepalive was not routable from retained Link state",
    )?;
    require(
        link_initiator.link_state(&link_id) == Some(LinkState::Stale),
        "late initiator tick did not transition the Link to Stale",
    )?;
    let keepalive_request = &stale_keepalive.packets[0];
    require(
        keepalive_request.target() == TxTarget::Only(initiator_interface),
        "initiator keepalive did not use its bound interface",
    )?;
    require(
        keepalive_request.bytes().len() == 20,
        "initiator keepalive carried unexpected encryption overhead",
    )?;
    require(
        keepalive_request.bytes()[0] & 0xc0 == 0,
        "initiator keepalive did not use HEADER_1",
    )?;
    let parsed_keepalive_request = parse_packet(keepalive_request.bytes())
        .map_err(|error| format!("Rete emitted invalid keepalive request: {error}"))?;
    require(
        parsed_keepalive_request.packet_type == PacketType::Data,
        "initiator keepalive used the wrong packet type",
    )?;
    require(
        parsed_keepalive_request.dest_type == DestType::Link,
        "initiator keepalive used the wrong destination type",
    )?;
    require(
        parsed_keepalive_request.destination_hash == link_id.as_ref(),
        "initiator keepalive was not addressed to its Link ID",
    )?;
    require(
        parsed_keepalive_request.context == 0xfa,
        "initiator keepalive used the wrong context",
    )?;
    require(
        parsed_keepalive_request.payload == [0xff],
        "initiator keepalive did not contain the exact FF request",
    )?;

    let responder_dedup_before = link_responder.metrics().transport.packets_dropped_dedup;
    let keepalive_response = link_responder.ingest(
        keepalive_request.bytes(),
        late_keepalive_at,
        responder_interface,
        &mut link_rng,
    );
    require(
        keepalive_response.actions.events.is_empty(),
        "responder surfaced keepalive lifecycle traffic as an application event",
    )?;
    require(
        keepalive_response.actions.packets.len() == 1,
        "responder did not emit exactly one keepalive response",
    )?;
    require(
        keepalive_response.actions.unroutable_packets == 0,
        "responder keepalive response was not routable from ingress context",
    )?;
    let keepalive_reply = &keepalive_response.actions.packets[0];
    require(
        keepalive_reply.target() == TxTarget::Only(responder_interface),
        "responder keepalive did not return on its bound interface",
    )?;
    require(
        keepalive_reply.bytes().len() == 20,
        "responder keepalive carried unexpected encryption overhead",
    )?;
    require(
        keepalive_reply.bytes()[0] & 0xc0 == 0,
        "responder keepalive did not use HEADER_1",
    )?;
    let parsed_keepalive_reply = parse_packet(keepalive_reply.bytes())
        .map_err(|error| format!("Rete emitted invalid keepalive response: {error}"))?;
    require(
        parsed_keepalive_reply.packet_type == PacketType::Data,
        "responder keepalive used the wrong packet type",
    )?;
    require(
        parsed_keepalive_reply.dest_type == DestType::Link,
        "responder keepalive used the wrong destination type",
    )?;
    require(
        parsed_keepalive_reply.destination_hash == link_id.as_ref(),
        "responder keepalive was not addressed to its Link ID",
    )?;
    require(
        parsed_keepalive_reply.context == 0xfa,
        "responder keepalive used the wrong context",
    )?;
    require(
        parsed_keepalive_reply.payload == [0xfe],
        "responder keepalive did not contain the exact FE response",
    )?;
    require(
        link_responder.metrics().transport.packets_dropped_dedup == responder_dedup_before,
        "first FF keepalive request entered packet deduplication",
    )?;

    let initiator_dedup_before = link_initiator.metrics().transport.packets_dropped_dedup;
    let consumed_keepalive = link_initiator.ingest(
        keepalive_reply.bytes(),
        late_keepalive_at + 1,
        initiator_interface,
        &mut link_rng,
    );
    require(
        consumed_keepalive.actions.events.is_empty(),
        "initiator surfaced keepalive lifecycle traffic as an application event",
    )?;
    require(
        consumed_keepalive.actions.packets.is_empty(),
        "initiator emitted output after consuming a keepalive response",
    )?;
    require(
        consumed_keepalive.actions.unroutable_packets == 0,
        "initiator reported unroutable output after consuming a keepalive response",
    )?;
    require(
        link_initiator.link_state(&link_id) == Some(LinkState::Active),
        "valid FE keepalive response did not revive the stale initiator",
    )?;
    require(
        link_initiator.metrics().transport.packets_dropped_dedup == initiator_dedup_before,
        "first FE keepalive response entered packet deduplication",
    )?;

    // Replay the identical deterministic request and response once. Both must
    // remain internal lifecycle traffic and bypass the ordinary packet filter.
    let repeated_keepalive_response = link_responder.ingest(
        keepalive_request.bytes(),
        late_keepalive_at + 2,
        responder_interface,
        &mut link_rng,
    );
    require(
        repeated_keepalive_response.actions.events.is_empty(),
        "replayed FF surfaced as an application event",
    )?;
    require(
        repeated_keepalive_response.actions.packets.len() == 1,
        "replayed FF did not produce exactly one FE response",
    )?;
    require(
        repeated_keepalive_response.actions.packets[0].target()
            == TxTarget::Only(responder_interface),
        "replayed FF response used the wrong interface",
    )?;
    require(
        repeated_keepalive_response.actions.packets[0].bytes() == keepalive_reply.bytes(),
        "replayed FF did not produce the identical deterministic FE response",
    )?;
    require(
        link_responder.metrics().transport.packets_dropped_dedup == responder_dedup_before,
        "replayed FF was suppressed by packet deduplication",
    )?;

    let repeated_keepalive_consumed = link_initiator.ingest(
        repeated_keepalive_response.actions.packets[0].bytes(),
        late_keepalive_at + 3,
        initiator_interface,
        &mut link_rng,
    );
    require(
        repeated_keepalive_consumed.actions.events.is_empty(),
        "replayed FE surfaced as an application event",
    )?;
    require(
        repeated_keepalive_consumed.actions.packets.is_empty(),
        "replayed FE produced unexpected output",
    )?;
    require(
        repeated_keepalive_consumed.actions.unroutable_packets == 0,
        "replayed FE produced unroutable output",
    )?;
    require(
        link_initiator.link_state(&link_id) == Some(LinkState::Active),
        "replayed FE did not retain the active initiator Link",
    )?;
    require(
        link_initiator.metrics().transport.packets_dropped_dedup == initiator_dedup_before,
        "replayed FE was suppressed by packet deduplication",
    )?;

    // Even after crossing its own stale deadline, the responder must never
    // originate a keepalive request. Its watchdog may only retain the Link in
    // the Stale revival window.
    let responder_stale_tick = link_responder.tick(late_keepalive_at + 1_002, &mut link_rng);
    require(
        responder_stale_tick
            .packets
            .iter()
            .all(|packet| parse_packet(packet.bytes()).is_ok_and(|parsed| parsed.context != 0xfa)),
        "responder originated a keepalive request",
    )?;
    require(
        link_responder.link_state(&link_id) == Some(LinkState::Stale),
        "responder watchdog fixture did not cross the stale deadline",
    )?;
    checks += 40;

    checks += verify_three_node_relayed_link()?;

    Ok(checks)
}

/// Exercise the product-owned adapter across three independently routed node
/// instances. Interface identifiers are deliberately local to each node; the
/// relay must retain and select its two exact sides without treating the
/// endpoint identifiers as globally meaningful.
fn verify_three_node_relayed_link() -> Result<usize, String> {
    let mut rng = CounterRng::default();
    let mut node_a = new_conformance_node(&[0xa1; 32])
        .map_err(|error| format!("could not construct relayed-Link initiator: {error}"))?;
    let mut node_b = new_conformance_transport_node(&[0xb2; 32])
        .map_err(|error| format!("could not construct relayed-Link transport: {error}"))?;
    let mut node_c = new_conformance_node(&[0xc3; 32])
        .map_err(|error| format!("could not construct relayed-Link responder: {error}"))?;

    let a_to_b = InterfaceId(11);
    let b_to_a = InterfaceId(21);
    let b_to_c = InterfaceId(31);
    let c_to_b = InterfaceId(41);
    let now = 1_700_001_000;

    node_c
        .queue_announce(None, now, &mut rng)
        .map_err(|error| format!("responder could not queue announce: {error}"))?;
    let announce = node_c
        .flush_announces(now, &mut rng)
        .into_iter()
        .next()
        .ok_or_else(|| "responder announce was not immediately available".to_owned())?;
    let learned_by_b = node_b.ingest(announce.bytes(), now, b_to_c, &mut rng);
    require(
        learned_by_b.actions.packets.len() == 1,
        "transport did not relay responder announce",
    )?;
    let relayed_announce = &learned_by_b.actions.packets[0];
    require(
        relayed_announce.target() == TxTarget::All,
        "transport announce did not use propagation routing",
    )?;
    let learned_by_a = node_a.ingest(relayed_announce.bytes(), now + 1, a_to_b, &mut rng);
    require(
        learned_by_a.actions.events.len() == 1,
        "initiator did not observe responder announce through transport",
    )?;
    let route = node_a
        .route(&node_c.destination_hash())
        .ok_or_else(|| "initiator did not retain responder route".to_owned())?;
    require(
        route.via == Some(node_b.identity_hash())
            && route.hops == 2
            && route.received_on == Some(a_to_b),
        "initiator retained the wrong transport route",
    )?;

    let (request, link_id) = node_a
        .initiate_link(node_c.destination_hash(), now + 2, &mut rng)
        .map_err(|error| format!("initiator could not build relayed LINKREQUEST: {error}"))?;
    let parsed_request = parse_packet(request.bytes())
        .map_err(|error| format!("initiator emitted invalid relayed LINKREQUEST: {error}"))?;
    require(
        request.target() == TxTarget::Only(a_to_b),
        "relayed LINKREQUEST used an unexpected origin route",
    )?;
    require(
        parsed_request.transport_id == Some(node_b.identity_hash().as_ref()),
        "relayed LINKREQUEST did not target the learned transport",
    )?;
    let forwarded_request = node_b.ingest(request.bytes(), now + 3, b_to_a, &mut rng);
    require(
        forwarded_request.actions.events.is_empty() && forwarded_request.actions.packets.len() == 1,
        "transport did not forward exactly one relayed LINKREQUEST",
    )?;
    require(
        forwarded_request.actions.packets[0].target() == TxTarget::Only(b_to_c),
        "transport sent LINKREQUEST on the wrong exact interface",
    )?;
    require(
        node_b.metrics().capacity.relay_links.used == 1
            && node_b.metrics().capacity.links.used == 0,
        "transport did not retain exactly one relay-owned Link route",
    )?;
    let parsed_forwarded_request = parse_packet(forwarded_request.actions.packets[0].bytes())
        .map_err(|error| format!("transport emitted invalid LINKREQUEST: {error}"))?;
    require(
        parsed_forwarded_request.transport_id.is_none()
            && parsed_forwarded_request.hops == parsed_request.hops.saturating_add(1),
        "transport did not consume exactly one LINKREQUEST transport hop",
    )?;

    let responder_proof = node_c.ingest(
        forwarded_request.actions.packets[0].bytes(),
        now + 4,
        c_to_b,
        &mut rng,
    );
    require(
        responder_proof.actions.events.is_empty() && responder_proof.actions.packets.len() == 1,
        "responder did not retain handshake state and emit one LRPROOF",
    )?;
    require(
        responder_proof.actions.packets[0].target() == TxTarget::Only(c_to_b),
        "responder LRPROOF did not return to its source interface",
    )?;
    let parsed_responder_proof = parse_packet(responder_proof.actions.packets[0].bytes())
        .map_err(|error| format!("responder emitted invalid LRPROOF: {error}"))?;
    require(
        parsed_responder_proof.packet_type == PacketType::Proof
            && parsed_responder_proof.dest_type == DestType::Link
            && parsed_responder_proof.context == 0xff,
        "responder emitted a noncanonical LRPROOF",
    )?;
    let forwarded_proof = node_b.ingest(
        responder_proof.actions.packets[0].bytes(),
        now + 5,
        b_to_c,
        &mut rng,
    );
    require(
        forwarded_proof.actions.events.is_empty() && forwarded_proof.actions.packets.len() == 1,
        "transport did not validate and forward exactly one LRPROOF",
    )?;
    require(
        forwarded_proof.actions.packets[0].target() == TxTarget::Only(b_to_a),
        "transport returned LRPROOF on the wrong exact interface",
    )?;
    let parsed_forwarded_proof = parse_packet(forwarded_proof.actions.packets[0].bytes())
        .map_err(|error| format!("transport emitted invalid relayed LRPROOF: {error}"))?;
    require(
        parsed_forwarded_proof.hops == parsed_responder_proof.hops.saturating_add(1),
        "transport did not advance LRPROOF by exactly one hop",
    )?;

    // The hop byte is outside the proof hash/signature. A valid LRPROOF at the
    // wrong retained path height must fail before dedup, so the untouched copy
    // below can still establish the pending Link.
    let dedup_before_wrong_hop = node_a.metrics().transport.packets_dropped_dedup;
    let mut wrong_hop_proof = forwarded_proof.actions.packets[0].bytes().to_vec();
    wrong_hop_proof[1] = parsed_forwarded_proof.hops.saturating_add(1);
    let wrong_hop = node_a.ingest(&wrong_hop_proof, now + 6, a_to_b, &mut rng);
    require(
        wrong_hop.disposition == IngressDisposition::NativeInvalid,
        "initiator did not classify wrong-hop LRPROOF as invalid",
    )?;
    require(
        wrong_hop.actions.events.is_empty()
            && wrong_hop.actions.packets.is_empty()
            && wrong_hop.actions.unroutable_packets == 0
            && node_a.link_state(&link_id) == Some(LinkState::Handshake),
        "wrong-hop LRPROOF mutated or emitted pending-Link state",
    )?;
    require(
        node_a.metrics().transport.packets_dropped_dedup == dedup_before_wrong_hop,
        "wrong-hop LRPROOF poisoned the initiator dedup window",
    )?;

    let initiator_rtt = node_a.ingest(
        forwarded_proof.actions.packets[0].bytes(),
        now + 6,
        a_to_b,
        &mut rng,
    );
    require(
        matches!(
            initiator_rtt.actions.events.as_slice(),
            [NodeEvent::LinkEstablished { link_id: event_id }] if *event_id == link_id
        ) && initiator_rtt.actions.packets.len() == 1,
        "initiator did not establish Link and emit one LRRTT",
    )?;
    require(
        initiator_rtt.actions.packets[0].target() == TxTarget::Only(a_to_b),
        "initiator LRRTT did not retain the authenticated Link interface",
    )?;
    let forwarded_rtt = node_b.ingest(
        initiator_rtt.actions.packets[0].bytes(),
        now + 7,
        b_to_a,
        &mut rng,
    );
    require(
        forwarded_rtt.actions.events.is_empty() && forwarded_rtt.actions.packets.len() == 1,
        "transport did not forward exactly one LRRTT",
    )?;
    require(
        forwarded_rtt.actions.packets[0].target() == TxTarget::Only(b_to_c),
        "transport sent LRRTT on the wrong exact interface",
    )?;
    let responder_established = node_c.ingest(
        forwarded_rtt.actions.packets[0].bytes(),
        now + 8,
        c_to_b,
        &mut rng,
    );
    require(
        matches!(
            responder_established.actions.events.as_slice(),
            [NodeEvent::LinkEstablished { link_id: event_id }] if *event_id == link_id
        ),
        "responder did not establish Link after relayed LRRTT",
    )?;
    require(
        node_a.link_state(&link_id) == Some(LinkState::Active)
            && node_c.link_state(&link_id) == Some(LinkState::Active),
        "relayed Link did not become active at both endpoints",
    )?;

    let channel = node_a
        .send_channel_message(&link_id, 0x5151, b"three-node channel", now + 9, &mut rng)
        .map_err(|error| format!("initiator could not send relayed channel DATA: {error}"))?;
    require(
        channel.target() == TxTarget::Only(a_to_b),
        "initiator channel DATA did not retain the authenticated Link interface",
    )?;
    require(
        node_a.metrics().capacity.channel_receipts.used == 1,
        "initiator did not retain relayed channel receipt",
    )?;
    let relayed_channel = node_b.ingest(channel.bytes(), now + 10, b_to_a, &mut rng);
    require(
        relayed_channel.actions.events.is_empty() && relayed_channel.actions.packets.len() == 1,
        "transport did not forward relayed channel DATA",
    )?;
    require(
        relayed_channel.actions.packets[0].target() == TxTarget::Only(b_to_c),
        "transport sent channel DATA on the wrong exact interface",
    )?;
    let channel_received = node_c.ingest(
        relayed_channel.actions.packets[0].bytes(),
        now + 11,
        c_to_b,
        &mut rng,
    );
    require(
        matches!(
            channel_received.actions.events.as_slice(),
            [NodeEvent::ChannelMessages { link_id: event_id, messages }]
                if *event_id == link_id
                    && messages.as_slice() == [(0x5151, b"three-node channel".to_vec())]
        ) && channel_received.actions.packets.len() == 1,
        "responder did not deliver channel DATA and emit its proof",
    )?;
    require(
        channel_received.actions.packets[0].target() == TxTarget::Only(c_to_b),
        "responder channel proof did not return on its Link ingress interface",
    )?;
    let relayed_channel_proof = node_b.ingest(
        channel_received.actions.packets[0].bytes(),
        now + 12,
        b_to_c,
        &mut rng,
    );
    require(
        relayed_channel_proof.actions.events.is_empty()
            && relayed_channel_proof.actions.packets.len() == 1,
        "transport did not forward channel proof",
    )?;
    require(
        relayed_channel_proof.actions.packets[0].target() == TxTarget::Only(b_to_a),
        "transport returned channel proof on the wrong exact interface",
    )?;
    let channel_delivered = node_a.ingest(
        relayed_channel_proof.actions.packets[0].bytes(),
        now + 13,
        a_to_b,
        &mut rng,
    );
    require(
        matches!(
            channel_delivered.actions.events.as_slice(),
            [NodeEvent::ProofReceived { .. }]
        ) && node_a.metrics().capacity.channel_receipts.used == 0,
        "relayed channel proof did not complete the initiator receipt",
    )?;
    require(
        node_b.metrics().capacity.relay_links.used == 1
            && node_b.metrics().capacity.reverse_entries.used == 0,
        "Link relay consumed unexpected transport capacity",
    )?;

    Ok(35)
}

fn verify_lrrtt_messagepack(vectors: &LrrttMessagepackVector) -> Result<usize, String> {
    require(
        vectors.origin
            == "decoded by RNS 1.3.8's vendored RNS.vendor.umsgpack; canonical float64 cases are encoded by that same module",
        "unexpected LRRTT vector origin",
    )?;
    require(
        vectors.scope
            == "This records released-peer behavior; firmware conformance separately verifies pending-handshake numeric payload semantics.",
        "unexpected LRRTT vector scope",
    )?;
    require(
        vectors.umsgpack_version == "2.7.1",
        "released LRRTT u-msgpack version drift",
    )?;
    require(
        vectors.umsgpack_source_sha256
            == "f3f78e1281e13f96089a6b2dbac6e6d927e48b2049461c82e4be4a6e591e8d45",
        "released LRRTT u-msgpack source drift",
    )?;
    require(
        vectors.measured_rtt_decimal == "0.25",
        "unexpected LRRTT measured-RTT fixture",
    )?;
    require(
        vectors.rns_rtt_formula == "max(0.25, umsgpack.unpackb(plaintext))",
        "unexpected released LRRTT formula",
    )?;

    require(
        vectors.canonical_float64.len() == 3,
        "released LRRTT corpus must contain three canonical encodes",
    )?;
    require(
        vectors.decode_cases.len() == 25,
        "released LRRTT corpus must contain 25 decode cases",
    )?;

    const EXPECTED_CASES: [&str; 28] = [
        "float64_0.001",
        "float64_0.125",
        "float64_1.0",
        "float32_0.125",
        "positive_fixint_1",
        "negative_fixint_minus_1",
        "uint8_1",
        "uint16_1",
        "uint32_1",
        "uint64_1",
        "int8_minus_1",
        "int16_minus_1",
        "int32_minus_1",
        "int64_minus_1",
        "boolean_false",
        "boolean_true",
        "float64_positive_infinity",
        "float64_negative_infinity",
        "float64_nan_payload_1",
        "float64_1.0_with_trailing_nil_and_reserved",
        "legacy_rete_raw_u32_timestamp",
        "nil",
        "string_1.0",
        "array_float64_1.0",
        "map_rtt_float64_1.0",
        "empty",
        "truncated_float64_1.0",
        "reserved_code",
    ];
    let expected_names: BTreeSet<_> = EXPECTED_CASES.into_iter().collect();
    let actual_names: BTreeSet<_> = vectors
        .canonical_float64
        .iter()
        .chain(&vectors.decode_cases)
        .map(|case| case.name.as_str())
        .collect();
    require(
        actual_names == expected_names,
        "released LRRTT case names drifted or contain duplicates",
    )?;

    let measured_rtt = vectors
        .measured_rtt_decimal
        .parse::<f64>()
        .map_err(|error| format!("invalid measured LRRTT decimal: {error}"))?;
    for case in &vectors.canonical_float64 {
        let input = case
            .input_decimal
            .as_deref()
            .ok_or_else(|| format!("{} has no canonical input decimal", case.name))?
            .parse::<f64>()
            .map_err(|error| format!("invalid {} input decimal: {error}", case.name))?;
        let expected_wire = decode_hex(&format!("{}.wire_hex", case.name), &case.wire_hex)?;
        require(
            encode_lrrtt_float64_for_conformance(input) == expected_wire,
            &format!("{} is not Rete's canonical float64 encoding", case.name),
        )?;
        verify_lrrtt_case(case, measured_rtt)?;
    }
    for case in &vectors.decode_cases {
        require(
            case.input_decimal.is_none(),
            &format!("decode-only case {} has a canonical input", case.name),
        )?;
        verify_lrrtt_case(case, measured_rtt)?;
    }

    // Six provenance checks, three corpus-shape checks, all 28 decode/formula
    // cases, and three exact canonical encoder comparisons.
    Ok(40)
}

fn verify_lrrtt_case(case: &LrrttCase, measured_rtt: f64) -> Result<(), String> {
    let wire = decode_hex(&format!("{}.wire_hex", case.name), &case.wire_hex)?;
    let expected_decoded = python_numeric_value(&case.name, &case.python_unpack)?;
    let decoded = decode_lrrtt_number_for_conformance(&wire);

    let Some(expected_decoded) = expected_decoded else {
        require(
            decoded.is_err(),
            &format!("Rete accepted nonnumeric or malformed LRRTT {}", case.name),
        )?;
        require(
            matches!(
                case.python_rns_rtt_formula.result.as_str(),
                "exception" | "not_run"
            ),
            &format!("{} has an unexpected Python formula outcome", case.name),
        )?;
        return Ok(());
    };

    let decoded =
        decoded.map_err(|error| format!("Rete rejected numeric LRRTT {}: {error}", case.name))?;
    require_float_bits(
        &format!("{} decoded value", case.name),
        decoded.value,
        expected_decoded,
    )?;

    match (&case.first_object_wire_hex, &case.trailing_hex) {
        (Some(first_hex), Some(trailing_hex)) => {
            let first = decode_hex(&format!("{}.first_object_wire_hex", case.name), first_hex)?;
            let trailing = decode_hex(&format!("{}.trailing_hex", case.name), trailing_hex)?;
            require(
                wire.starts_with(&first) && wire[first.len()..] == trailing,
                &format!("{} first-object/trailing split is inconsistent", case.name),
            )?;
            require(
                decoded.consumed == first.len(),
                &format!("Rete consumed trailing bytes in {}", case.name),
            )?;
        }
        (None, None) => require(
            decoded.consumed == wire.len(),
            &format!(
                "Rete did not consume the complete LRRTT object {}",
                case.name
            ),
        )?,
        _ => {
            return Err(format!(
                "{} has an incomplete trailing-byte split",
                case.name
            ));
        }
    }

    let expected_selected = python_numeric_value(&case.name, &case.python_rns_rtt_formula)?
        .ok_or_else(|| format!("numeric LRRTT {} has no Python formula value", case.name))?;
    let selected = select_lrrtt_for_conformance(measured_rtt, decoded.value);
    require_float_bits(
        &format!("{} Python max semantics", case.name),
        selected,
        expected_selected,
    )
}

fn python_numeric_value(
    case_name: &str,
    outcome: &PythonValueOutcome,
) -> Result<Option<f64>, String> {
    match outcome.result.as_str() {
        "exception" | "not_run" => return Ok(None),
        "value" => {}
        other => return Err(format!("{case_name} has unknown Python outcome {other}")),
    }

    match outcome.value_class.as_deref() {
        Some("finite" | "positive_infinity" | "negative_infinity" | "nan") => {
            let bits_hex = outcome
                .f64_bits_hex
                .as_deref()
                .ok_or_else(|| format!("{case_name} has no float bits"))?;
            let bytes = decode_hex(&format!("{case_name}.f64_bits_hex"), bits_hex)?;
            let bits: [u8; 8] = bytes
                .try_into()
                .map_err(|_| format!("{case_name} float bits are not eight bytes"))?;
            Ok(Some(f64::from_be_bytes(bits)))
        }
        Some("integer") => outcome
            .decimal
            .as_deref()
            .ok_or_else(|| format!("{case_name} has no integer decimal"))?
            .parse::<f64>()
            .map(Some)
            .map_err(|error| format!("invalid {case_name} integer decimal: {error}")),
        Some("boolean") => outcome
            .value
            .map(|value| Some(if value { 1.0 } else { 0.0 }))
            .ok_or_else(|| format!("{case_name} has no boolean value")),
        Some("nil" | "string" | "array" | "map") | None => Ok(None),
        Some(other) => Err(format!(
            "{case_name} has unknown Python value class {other}"
        )),
    }
}

fn require_float_bits(label: &str, actual: f64, expected: f64) -> Result<(), String> {
    if actual.to_bits() == expected.to_bits() {
        Ok(())
    } else {
        Err(format!(
            "{label} mismatch: expected {:016x}, got {:016x}",
            expected.to_bits(),
            actual.to_bits()
        ))
    }
}

fn decode_hex(field: &str, value: &str) -> Result<Vec<u8>, String> {
    hex::decode(value).map_err(|error| format!("invalid {field}: {error}"))
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn require_hex_eq(label: &str, actual: &[u8], expected_hex: &str) -> Result<(), String> {
    let expected = decode_hex(label, expected_hex)?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{label} mismatch: expected {expected_hex}, got {}",
            hex::encode(actual)
        ))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_python_vectors_pass() {
        assert_eq!(verify_released_vectors(), Ok(235));
    }
}
