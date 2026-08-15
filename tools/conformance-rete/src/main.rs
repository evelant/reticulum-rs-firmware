use std::{collections::BTreeSet, process::ExitCode};

use rand_core::{CryptoRng, RngCore};
use reticulum_radio_interface::{RNODE_HW_MTU, ReceiveOutcome, RnodeRxReassembler};
use reticulum_rns_rete::{
    ApplicationEvent, ApplicationLinkRole, DestHash, DestType, DestinationType, Direction,
    EmbeddedNodeConfig, Identity, IngressDisposition, InitialEmbeddedNode, InterfaceId, LinkId,
    LinkState, MonotonicInstant, OutboundDispatchInterval, PacketType, TxTarget,
    build_announce_packet, decode_lrrtt_number_for_conformance,
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
    lrrtt_lifecycle: LrrttLifecycleVector,
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
#[serde(deny_unknown_fields)]
struct LrrttLifecycleVector {
    clock: String,
    link_source_sha256: String,
    origin: String,
    packet_source_sha256: String,
    probe_scaffolding: LrrttProbeScaffolding,
    request_time_ordering: LrrttRequestTimeOrdering,
    responder_lifecycle: LrrttResponderLifecycle,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LrrttProbeScaffolding {
    request_time_ordering: Vec<String>,
    responder_lifecycle: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LrrttRequestTimeOrdering {
    initiator: LrrttInitiatorRequestOrdering,
    responder: LrrttResponderRequestOrdering,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LrrttInitiatorRequestOrdering {
    last_outbound: LrrttTimingValue,
    observed_event_order: Vec<String>,
    released_methods: Vec<String>,
    request_time: LrrttTimingValue,
    semantic: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LrrttResponderRequestOrdering {
    last_inbound: LrrttTimingValue,
    observed_event_order: Vec<String>,
    released_methods: Vec<String>,
    request_time: LrrttTimingValue,
    semantic: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LrrttResponderLifecycle {
    authenticated_malformed_plaintext_hex: String,
    callback_observations: Vec<LrrttCallbackObservation>,
    cases: Vec<LrrttLifecycleCase>,
    ingress_scope: String,
    peer_rtt_wire_hex: String,
    released_entrypoint: String,
    request_time_semantic: String,
    teardown_boundary: String,
    transport_exact_replay_dedup_exercised: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LrrttCallbackObservation {
    activated_at: LrrttTimingValue,
    expected_hops: u8,
    rtt: LrrttTimingValue,
    state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LrrttLifecycleCase {
    activated_at_after: LrrttTimingValue,
    activated_at_before: Option<LrrttTimingValue>,
    callback_count_after: u64,
    callback_delta: u64,
    ciphertext_hex: String,
    ciphertext_provenance: String,
    expected_hops_after: u8,
    keepalive_after: LrrttTimingValue,
    last_data_after: LrrttTimingValue,
    last_inbound_after: LrrttTimingValue,
    name: String,
    observed_clock_order: Vec<String>,
    request_time_after: LrrttTimingValue,
    request_time_before: LrrttTimingValue,
    rtt_after: LrrttTimingValue,
    rtt_before: Option<LrrttTimingValue>,
    rx_after: u64,
    rxbytes_after: u64,
    stale_time_after: LrrttTimingValue,
    state_after: String,
    state_before: String,
    teardown_count_after: u64,
    teardown_delta: u64,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LrrttTimingValue {
    decimal: String,
    #[serde(default)]
    f64_bits_hex: Option<String>,
    #[serde(default)]
    python_type: Option<String>,
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

    require(corpus.schema == 2, "unsupported vector schema")?;
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
            == "The foundation packet corpus excludes encrypted packet creation because Python correctly uses a fresh ephemeral key and IV. LRRTT sections record released-peer decode, RTT-formula, request-clock ordering and direct Link.receive repeat-lifecycle behavior; candidate conformance is checked separately.",
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

    checks += verify_lrrtt_lifecycle(&corpus.lrrtt_lifecycle)?;
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
        Some(ApplicationEvent::DataReceived {
            destination,
            payload,
            ..
        }) => {
            require(
                *destination == *registered_plain.as_bytes(),
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
        Some(ApplicationEvent::AnnounceReceived {
            destination,
            identity,
            hops,
            app_data,
            ..
        }) => {
            require(
                destination.as_slice() == announce_dest,
                "relay announce destination mismatch",
            )?;
            require(
                *identity == *announce_identity.as_bytes(),
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
        Some(ApplicationEvent::AnnounceReceived {
            destination, hops, ..
        }) => {
            require(
                destination.as_slice() == announce_dest,
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
        Some(ApplicationEvent::DataReceived {
            destination,
            payload,
            ..
        }) => {
            require(
                *destination == *announce_dest_hash.as_bytes(),
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
    const LINK_BASE_MICROS: u64 = 1_700_000_100_000_000;

    let (link_request, link_id) = link_initiator
        .initiate_link_at(
            link_responder.destination_hash(),
            1_700_000_100,
            MonotonicInstant::from_micros(LINK_BASE_MICROS),
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
    let request_token = link_request
        .protocol_token()
        .ok_or_else(|| "LINKREQUEST did not carry a protocol timing token".to_owned())?;
    let request_interval = OutboundDispatchInterval::new(
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 125_000),
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 150_000),
    )
    .ok_or_else(|| "ordered LINKREQUEST dispatch interval was rejected".to_owned())?;
    require(
        link_initiator.confirm_outbound_protocol(
            request_token,
            initiator_interface,
            request_interval,
        ),
        "initiator rejected LINKREQUEST dispatch confirmation",
    )?;
    require(
        !link_initiator.confirm_outbound_protocol(
            request_token,
            initiator_interface,
            request_interval,
        ),
        "initiator accepted duplicate LINKREQUEST dispatch confirmation",
    )?;
    checks += 7;

    let link_response = link_responder.ingest_at(
        link_request.bytes(),
        1_700_000_100,
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 200_000),
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
    let proof_token = link_response.actions.packets[0]
        .protocol_token()
        .ok_or_else(|| "LRPROOF did not carry a protocol timing token".to_owned())?;
    let proof_interval = OutboundDispatchInterval::new(
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 225_000),
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 250_000),
    )
    .ok_or_else(|| "ordered LRPROOF dispatch interval was rejected".to_owned())?;
    require(
        !link_responder.confirm_outbound_protocol(proof_token, initiator_interface, proof_interval),
        "responder accepted LRPROOF confirmation from the wrong interface",
    )?;
    require(
        link_responder.confirm_outbound_protocol(proof_token, responder_interface, proof_interval),
        "responder rejected LRPROOF dispatch confirmation",
    )?;
    checks += 9;

    let initiator_established = link_initiator.ingest_at(
        link_response.actions.packets[0].bytes(),
        1_700_000_101,
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 375_000),
        initiator_interface,
        &mut link_rng,
    );
    require(
        matches!(
            initiator_established.actions.events.as_slice(),
            [ApplicationEvent::LinkEstablished { link: event_id }]
                if *event_id == *link_id.as_bytes()
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

    let responder_established = link_responder.ingest_at(
        initiator_established.actions.packets[0].bytes(),
        1_700_000_102,
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 500_000),
        responder_interface,
        &mut link_rng,
    );
    require(
        matches!(
            responder_established.actions.events.as_slice(),
            [ApplicationEvent::LinkEstablished { link: event_id }]
                if *event_id == *link_id.as_bytes()
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
    let initiator_timing = link_initiator
        .link_snapshot_for_conformance(&link_id)
        .ok_or_else(|| "initiator Link timing state disappeared".to_owned())?;
    let responder_timing = link_responder
        .link_snapshot_for_conformance(&link_id)
        .ok_or_else(|| "responder Link timing state disappeared".to_owned())?;
    require(
        initiator_timing.request_started_at.as_micros() == LINK_BASE_MICROS + 125_000
            && initiator_timing.request_time_confirmed
            && initiator_timing.request_dispatch_interface == Some(initiator_interface.0),
        "initiator did not retain the pre-dispatch LINKREQUEST edge",
    )?;
    require(
        responder_timing.request_started_at.as_micros() == LINK_BASE_MICROS + 250_000
            && responder_timing.request_time_confirmed
            && responder_timing.request_dispatch_interface == Some(responder_interface.0),
        "responder did not retain the post-dispatch LRPROOF edge",
    )?;
    require_float_bits("precise initiator Link RTT", initiator_timing.rtt, 0.25)?;
    require_float_bits("precise responder Link RTT", responder_timing.rtt, 0.25)?;
    require(
        initiator_timing.keepalive_interval.as_micros() == 51_428_571
            && initiator_timing.stale_time.as_micros() == 102_857_142
            && responder_timing.keepalive_interval == initiator_timing.keepalive_interval
            && responder_timing.stale_time == initiator_timing.stale_time,
        "precise Link keepalive/stale intervals did not match the negotiated RTT",
    )?;
    checks += 8;

    let established_before_repeat = link_responder.metrics().transport.links_established;
    let exact_repeat = link_responder.ingest_at(
        initiator_established.actions.packets[0].bytes(),
        1_700_000_102,
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 600_000),
        responder_interface,
        &mut link_rng,
    );
    require(
        exact_repeat.disposition == IngressDisposition::NativeDuplicate
            && exact_repeat.actions.events.is_empty()
            && exact_repeat.actions.packets.is_empty(),
        "exact LRRTT replay bypassed native deduplication",
    )?;
    require_float_bits(
        "responder RTT after exact LRRTT replay",
        link_responder
            .link_rtt(&link_id)
            .ok_or_else(|| "responder Link disappeared after exact replay".to_owned())?,
        0.25,
    )?;

    let fresh_repeat = link_initiator
        .build_lrrtt_for_conformance(
            &link_id,
            &encode_lrrtt_float64_for_conformance(0.25),
            &mut link_rng,
        )
        .map_err(|error| format!("initiator could not build a fresh LRRTT repeat: {error}"))?;
    require(
        fresh_repeat.target() == TxTarget::Only(initiator_interface)
            && fresh_repeat.protocol_token().is_none()
            && fresh_repeat.bytes() != initiator_established.actions.packets[0].bytes(),
        "fresh LRRTT repeat did not preserve route/ciphertext semantics",
    )?;
    let active_repeat = link_responder.ingest_at(
        fresh_repeat.bytes(),
        1_700_000_102,
        MonotonicInstant::from_micros(LINK_BASE_MICROS + 750_000),
        responder_interface,
        &mut link_rng,
    );
    require(
        matches!(
            active_repeat.actions.events.as_slice(),
            [ApplicationEvent::LinkRttUpdated {
                link: event_id,
                rtt_seconds,
            }]
                if *event_id == *link_id.as_bytes()
                    && rtt_seconds.to_bits() == 0.5_f64.to_bits()
        ),
        "fresh active LRRTT repeat did not emit one RTT-update event",
    )?;
    let repeat_timing = link_responder
        .link_snapshot_for_conformance(&link_id)
        .ok_or_else(|| "responder Link disappeared after fresh repeat".to_owned())?;
    require(
        repeat_timing.state == LinkState::Active
            && repeat_timing.request_started_at == responder_timing.request_started_at
            && repeat_timing.rtt.to_bits() == 0.5_f64.to_bits()
            && link_responder.metrics().transport.links_established == established_before_repeat,
        "fresh active LRRTT repeat changed establishment or request-origin semantics",
    )?;
    checks += 6;

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
            [ApplicationEvent::LinkData {
                binding,
                data,
                context,
                ..
            }]
                if binding.link() == link_id.as_bytes()
                    && binding.destination() == link_responder.destination_hash().as_bytes()
                    && binding.role() == ApplicationLinkRole::Responder
                    && data == b"embedded Link payload"
                    && *context == 0
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
            [ApplicationEvent::ChannelMessages {
                link: event_id,
                messages,
            }]
                if *event_id == *link_id.as_bytes()
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
            [ApplicationEvent::ProofReceived { .. }]
        ),
        "channel proof did not emit ProofReceived",
    )?;
    let received_packet_hash = match received_proof.actions.events.as_slice() {
        [ApplicationEvent::ProofReceived { packet_hash, .. }] => *packet_hash,
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

    checks += verify_lrrtt_candidate_lifecycle()?;
    checks += verify_three_node_relayed_link()?;

    Ok(checks)
}

struct CandidateLinkHandshake {
    initiator: InitialEmbeddedNode,
    responder: InitialEmbeddedNode,
    link_id: LinkId,
    responder_interface: InterfaceId,
    request_origin: MonotonicInstant,
    initial_lrrtt: Vec<u8>,
    rng: CounterRng,
}

#[derive(Clone, Copy)]
enum CandidateResponderState {
    Handshake,
    Active,
    Stale,
}

/// Exercise LRRTT lifecycle cases that are intentionally outside the primary
/// precise-establishment and active-repeat path above. Every case starts from
/// a fresh deterministic node pair so teardown, deduplication and watchdog
/// state cannot leak between observations.
fn verify_lrrtt_candidate_lifecycle() -> Result<usize, String> {
    let mut checks = 0;

    // A fresh authenticated LRRTT is a valid revival signal during the Stale
    // grace window. It updates RTT from the immutable proof-dispatch origin,
    // but must not count the already-established Link a second time.
    let mut stale_repeat = prepare_candidate_link(
        "stale LRRTT repeat",
        0xd1,
        0xd2,
        51,
        1_700_010_000_000_000,
        250_000,
    )?;
    activate_candidate_responder(&mut stale_repeat, 250_000)?;
    let established_before = stale_repeat.responder.metrics().transport.links_established;
    let active_snapshot = stale_repeat
        .responder
        .link_snapshot_for_conformance(&stale_repeat.link_id)
        .ok_or_else(|| "stale-repeat responder Link disappeared after establishment".to_owned())?;
    let stale_at = MonotonicInstant::from_micros(
        active_snapshot
            .last_inbound
            .as_micros()
            .saturating_add(active_snapshot.stale_time.as_micros()),
    );
    let stale_tick =
        stale_repeat
            .responder
            .tick_at(stale_at.as_secs(), stale_at, &mut stale_repeat.rng);
    require(
        stale_tick.packets.is_empty()
            && stale_repeat.responder.link_state(&stale_repeat.link_id) == Some(LinkState::Stale),
        "fresh-repeat fixture did not enter responder Stale state",
    )?;
    checks += 1;

    let repeated_lrrtt = stale_repeat
        .initiator
        .build_lrrtt_for_conformance(
            &stale_repeat.link_id,
            &encode_lrrtt_float64_for_conformance(0.25),
            &mut stale_repeat.rng,
        )
        .map_err(|error| format!("could not build stale LRRTT repeat: {error}"))?;
    let revived_at = MonotonicInstant::from_micros(stale_at.as_micros() + 125_000);
    let revived = stale_repeat.responder.ingest_at(
        repeated_lrrtt.bytes(),
        revived_at.as_secs(),
        revived_at,
        stale_repeat.responder_interface,
        &mut stale_repeat.rng,
    );
    let updated_rtt = match revived.actions.events.as_slice() {
        [
            ApplicationEvent::LinkRttUpdated {
                link: event_id,
                rtt_seconds,
            },
        ] if *event_id == *stale_repeat.link_id.as_bytes() => *rtt_seconds,
        _ => return Err("stale LRRTT repeat did not emit exactly one LinkRttUpdated".to_owned()),
    };
    checks += 1;
    let revived_snapshot = stale_repeat
        .responder
        .link_snapshot_for_conformance(&stale_repeat.link_id)
        .ok_or_else(|| "stale LRRTT repeat removed its Link".to_owned())?;
    require(
        revived_snapshot.state == LinkState::Active,
        "fresh LRRTT did not revive the stale responder",
    )?;
    require(
        revived_snapshot.request_started_at == stale_repeat.request_origin,
        "stale LRRTT repeat replaced the immutable request origin",
    )?;
    require(
        revived_snapshot.last_inbound == revived_at,
        "stale LRRTT repeat did not refresh authenticated inbound liveness",
    )?;
    require(
        revived_snapshot.rtt.to_bits() == updated_rtt.to_bits(),
        "stale LRRTT event and retained RTT disagree",
    )?;
    require(
        stale_repeat.responder.metrics().transport.links_established == established_before,
        "stale LRRTT repeat counted a duplicate establishment",
    )?;
    checks += 5;

    // Authenticated MessagePack 0xc1 is a protocol error for every responder
    // state that accepts LRRTT. Only a negotiating Link contributes a failed
    // establishment; Active and Stale teardown are ordinary closes.
    for (label, state, initiator_tag, responder_tag, interface, base_micros) in [
        (
            "handshake",
            CandidateResponderState::Handshake,
            0xd3,
            0xd4,
            53,
            1_700_020_000_000_000,
        ),
        (
            "active",
            CandidateResponderState::Active,
            0xd5,
            0xd6,
            55,
            1_700_030_000_000_000,
        ),
        (
            "stale",
            CandidateResponderState::Stale,
            0xd7,
            0xd8,
            57,
            1_700_040_000_000_000,
        ),
    ] {
        let mut fixture = prepare_candidate_link(
            &format!("malformed {label} LRRTT"),
            initiator_tag,
            responder_tag,
            interface,
            base_micros,
            250_000,
        )?;
        if !matches!(state, CandidateResponderState::Handshake) {
            activate_candidate_responder(&mut fixture, 250_000)?;
        }
        if matches!(state, CandidateResponderState::Stale) {
            let snapshot = fixture
                .responder
                .link_snapshot_for_conformance(&fixture.link_id)
                .ok_or_else(|| format!("malformed {label} fixture lost its active Link"))?;
            let stale_at = MonotonicInstant::from_micros(
                snapshot
                    .last_inbound
                    .as_micros()
                    .saturating_add(snapshot.stale_time.as_micros()),
            );
            let _stale_actions =
                fixture
                    .responder
                    .tick_at(stale_at.as_secs(), stale_at, &mut fixture.rng);
        }
        let expected_state = match state {
            CandidateResponderState::Handshake => LinkState::Handshake,
            CandidateResponderState::Active => LinkState::Active,
            CandidateResponderState::Stale => LinkState::Stale,
        };
        require(
            fixture.responder.link_state(&fixture.link_id) == Some(expected_state),
            &format!("malformed {label} fixture entered the wrong precondition state"),
        )?;

        let malformed = fixture
            .initiator
            .build_lrrtt_for_conformance(&fixture.link_id, &[0xc1], &mut fixture.rng)
            .map_err(|error| format!("could not build malformed {label} LRRTT: {error}"))?;
        let before = fixture.responder.metrics().transport;
        let malformed_at = MonotonicInstant::from_micros(base_micros + 1_000_000_000);
        let closed = fixture.responder.ingest_at(
            malformed.bytes(),
            malformed_at.as_secs(),
            malformed_at,
            fixture.responder_interface,
            &mut fixture.rng,
        );
        require(
            closed.disposition == IngressDisposition::NativeInvalid,
            &format!("authenticated malformed {label} LRRTT was not invalid"),
        )?;
        require(
            matches!(
                closed.actions.events.as_slice(),
                [ApplicationEvent::LinkClosed { link: event_id }]
                    if *event_id == *fixture.link_id.as_bytes()
            ),
            &format!("authenticated malformed {label} LRRTT did not emit one LinkClosed"),
        )?;
        require(
            fixture.responder.link_state(&fixture.link_id).is_none(),
            &format!("authenticated malformed {label} LRRTT retained its Link"),
        )?;
        let after = fixture.responder.metrics().transport;
        let expected_failed =
            before.links_failed + u64::from(matches!(state, CandidateResponderState::Handshake));
        require(
            after.links_failed == expected_failed,
            &format!("authenticated malformed {label} LRRTT changed links_failed incorrectly"),
        )?;
        require(
            after.links_closed == before.links_closed + 1
                && after.links_established == before.links_established,
            &format!(
                "authenticated malformed {label} LRRTT changed lifecycle counters incorrectly"
            ),
        )?;
        checks += 6;
    }

    // Rete deliberately authenticates before refreshing liveness. A damaged
    // encrypted LRRTT received during Stale therefore cannot revive the Link,
    // unlike released Python's pre-decrypt liveness update.
    let mut corrupt_stale = prepare_candidate_link(
        "corrupt stale packet",
        0xd9,
        0xda,
        59,
        1_700_050_000_000_000,
        250_000,
    )?;
    activate_candidate_responder(&mut corrupt_stale, 250_000)?;
    let active = corrupt_stale
        .responder
        .link_snapshot_for_conformance(&corrupt_stale.link_id)
        .ok_or_else(|| "corrupt-auth fixture lost its active Link".to_owned())?;
    let stale_at = MonotonicInstant::from_micros(
        active
            .last_inbound
            .as_micros()
            .saturating_add(active.stale_time.as_micros()),
    );
    let _stale_actions =
        corrupt_stale
            .responder
            .tick_at(stale_at.as_secs(), stale_at, &mut corrupt_stale.rng);
    let stale_snapshot = corrupt_stale
        .responder
        .link_snapshot_for_conformance(&corrupt_stale.link_id)
        .ok_or_else(|| "corrupt-auth fixture lost its stale Link".to_owned())?;
    require(
        stale_snapshot.state == LinkState::Stale,
        "corrupt-auth fixture did not enter Stale",
    )?;
    let mut corrupt = corrupt_stale
        .initiator
        .build_lrrtt_for_conformance(
            &corrupt_stale.link_id,
            &encode_lrrtt_float64_for_conformance(0.25),
            &mut corrupt_stale.rng,
        )
        .map_err(|error| format!("could not build corrupt-auth LRRTT fixture: {error}"))?
        .bytes()
        .to_vec();
    let last_byte = corrupt
        .last_mut()
        .ok_or_else(|| "corrupt-auth LRRTT fixture was empty".to_owned())?;
    *last_byte ^= 0x01;
    let before = corrupt_stale.responder.metrics().transport;
    let corrupt_at = MonotonicInstant::from_micros(stale_at.as_micros() + 1_000_000);
    let rejected = corrupt_stale.responder.ingest_at(
        &corrupt,
        corrupt_at.as_secs(),
        corrupt_at,
        corrupt_stale.responder_interface,
        &mut corrupt_stale.rng,
    );
    require(
        rejected.disposition == IngressDisposition::NativeInvalid
            && rejected.actions.events.is_empty()
            && rejected.actions.packets.is_empty(),
        "corrupt-auth stale LRRTT produced lifecycle output",
    )?;
    let after_snapshot = corrupt_stale
        .responder
        .link_snapshot_for_conformance(&corrupt_stale.link_id)
        .ok_or_else(|| "corrupt-auth stale LRRTT removed its Link".to_owned())?;
    require(
        after_snapshot.state == LinkState::Stale,
        "corrupt-auth stale LRRTT revived the Link",
    )?;
    require(
        after_snapshot.last_inbound == stale_snapshot.last_inbound,
        "corrupt-auth stale LRRTT refreshed inbound liveness",
    )?;
    require(
        after_snapshot.rtt.to_bits() == stale_snapshot.rtt.to_bits(),
        "corrupt-auth stale LRRTT changed RTT",
    )?;
    let after = corrupt_stale.responder.metrics().transport;
    require(
        after.crypto_failures == before.crypto_failures + 1
            && after.links_failed == before.links_failed
            && after.links_closed == before.links_closed,
        "corrupt-auth stale LRRTT changed failure/close accounting",
    )?;
    checks += 6;

    // Equal confirmed dispatch and receive edges are a real zero RTT, not an
    // absent sample. Both peers must preserve binary64 +0.0 and use the
    // protocol's five-second keepalive / ten-second stale floors.
    let mut zero = prepare_candidate_link("zero RTT", 0xdb, 0xdc, 61, 1_700_060_000_000_001, 0)?;
    activate_candidate_responder(&mut zero, 0)?;
    let zero_initiator = zero
        .initiator
        .link_snapshot_for_conformance(&zero.link_id)
        .ok_or_else(|| "zero-RTT initiator Link disappeared".to_owned())?;
    let zero_responder = zero
        .responder
        .link_snapshot_for_conformance(&zero.link_id)
        .ok_or_else(|| "zero-RTT responder Link disappeared".to_owned())?;
    require(
        zero_initiator.rtt.to_bits() == 0.0_f64.to_bits(),
        "zero measured RTT was not retained exactly by initiator",
    )?;
    require(
        zero_responder.rtt.to_bits() == 0.0_f64.to_bits(),
        "zero measured/peer RTT was not retained exactly by responder",
    )?;
    require(
        zero_initiator.keepalive_interval.as_micros() == 5_000_000
            && zero_initiator.stale_time.as_micros() == 10_000_000,
        "zero-RTT initiator did not retain 5s/10s intervals",
    )?;
    require(
        zero_responder.keepalive_interval.as_micros() == 5_000_000
            && zero_responder.stale_time.as_micros() == 10_000_000,
        "zero-RTT responder did not retain 5s/10s intervals",
    )?;
    checks += 4;

    Ok(checks)
}

fn prepare_candidate_link(
    label: &str,
    initiator_tag: u8,
    responder_tag: u8,
    responder_interface: u8,
    base_micros: u64,
    measured_rtt_micros: u64,
) -> Result<CandidateLinkHandshake, String> {
    let initiator_seed = [initiator_tag; 32];
    let responder_seed = [responder_tag; 32];
    let responder_identity = Identity::from_seed(&responder_seed)
        .map_err(|error| format!("{label}: could not construct responder identity: {error}"))?;
    let mut initiator = new_conformance_node(&initiator_seed)
        .map_err(|error| format!("{label}: could not construct initiator: {error}"))?;
    let mut responder = new_conformance_node(&responder_seed)
        .map_err(|error| format!("{label}: could not construct responder: {error}"))?;
    let mut rng = CounterRng::default();
    let responder_interface = InterfaceId(responder_interface);
    let initiator_interface = InterfaceId(responder_interface.0 + 1);
    let request_origin = MonotonicInstant::from_micros(base_micros);
    let now = request_origin.as_secs();

    initiator
        .register_peer(&responder_identity, "reticulum", &["phase0"], now)
        .map_err(|error| format!("{label}: could not register responder: {error}"))?;
    let (request, link_id) = initiator
        .initiate_link_at(responder.destination_hash(), now, request_origin, &mut rng)
        .map_err(|error| format!("{label}: could not initiate Link: {error}"))?;
    let request_token = request
        .protocol_token()
        .ok_or_else(|| format!("{label}: LINKREQUEST lacked a protocol token"))?;
    let dispatch = OutboundDispatchInterval::new(request_origin, request_origin)
        .ok_or_else(|| format!("{label}: equal dispatch interval was rejected"))?;
    require(
        initiator.confirm_outbound_protocol(request_token, initiator_interface, dispatch),
        &format!("{label}: LINKREQUEST dispatch was not confirmed"),
    )?;

    let proof = responder.ingest_at(
        request.bytes(),
        now,
        request_origin,
        responder_interface,
        &mut rng,
    );
    require(
        proof.actions.events.is_empty()
            && proof.actions.packets.len() == 1
            && responder.link_state(&link_id) == Some(LinkState::Handshake),
        &format!("{label}: responder did not retain a pending Link and one LRPROOF"),
    )?;
    let proof_token = proof.actions.packets[0]
        .protocol_token()
        .ok_or_else(|| format!("{label}: LRPROOF lacked a protocol token"))?;
    require(
        responder.confirm_outbound_protocol(proof_token, responder_interface, dispatch),
        &format!("{label}: LRPROOF dispatch was not confirmed"),
    )?;

    let proof_received = MonotonicInstant::from_micros(base_micros + measured_rtt_micros);
    let established = initiator.ingest_at(
        proof.actions.packets[0].bytes(),
        proof_received.as_secs(),
        proof_received,
        initiator_interface,
        &mut rng,
    );
    require(
        matches!(
            established.actions.events.as_slice(),
            [ApplicationEvent::LinkEstablished { link: event_id }]
                if *event_id == *link_id.as_bytes()
        ) && established.actions.packets.len() == 1
            && initiator.link_state(&link_id) == Some(LinkState::Active),
        &format!("{label}: initiator did not establish and emit one LRRTT"),
    )?;

    Ok(CandidateLinkHandshake {
        initiator,
        responder,
        link_id,
        responder_interface,
        request_origin,
        initial_lrrtt: established.actions.packets[0].bytes().to_vec(),
        rng,
    })
}

fn activate_candidate_responder(
    fixture: &mut CandidateLinkHandshake,
    responder_rtt_micros: u64,
) -> Result<(), String> {
    let received_at = MonotonicInstant::from_micros(
        fixture
            .request_origin
            .as_micros()
            .saturating_add(responder_rtt_micros),
    );
    let established = fixture.responder.ingest_at(
        &fixture.initial_lrrtt,
        received_at.as_secs(),
        received_at,
        fixture.responder_interface,
        &mut fixture.rng,
    );
    require(
        matches!(
            established.actions.events.as_slice(),
            [ApplicationEvent::LinkEstablished { link: event_id }]
                if *event_id == *fixture.link_id.as_bytes()
        ) && established.actions.packets.is_empty()
            && fixture.responder.link_state(&fixture.link_id) == Some(LinkState::Active),
        "candidate responder did not establish from its first LRRTT",
    )
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
            [ApplicationEvent::LinkEstablished { link: event_id }]
                if *event_id == *link_id.as_bytes()
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
            [ApplicationEvent::LinkEstablished { link: event_id }]
                if *event_id == *link_id.as_bytes()
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
            [ApplicationEvent::ChannelMessages {
                link: event_id,
                messages,
            }]
                if *event_id == *link_id.as_bytes()
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
            [ApplicationEvent::ProofReceived { .. }]
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

#[derive(Clone, Copy)]
struct ExpectedLrrttTiming {
    decimal: &'static str,
    f64_bits_hex: Option<&'static str>,
    python_type: Option<&'static str>,
}

impl ExpectedLrrttTiming {
    const fn seconds(decimal: &'static str, f64_bits_hex: &'static str) -> Self {
        Self {
            decimal,
            f64_bits_hex: Some(f64_bits_hex),
            python_type: None,
        }
    }

    const fn float_interval(decimal: &'static str, f64_bits_hex: &'static str) -> Self {
        Self {
            decimal,
            f64_bits_hex: Some(f64_bits_hex),
            python_type: Some("float"),
        }
    }

    const fn integer_interval(decimal: &'static str) -> Self {
        Self {
            decimal,
            f64_bits_hex: None,
            python_type: Some("int"),
        }
    }
}

struct ExpectedLrrttLifecycleCase {
    name: &'static str,
    ciphertext_hex: &'static str,
    state_before: &'static str,
    state_after: &'static str,
    observed_clock_order: &'static [&'static str],
    rtt_before: Option<ExpectedLrrttTiming>,
    rtt_after: ExpectedLrrttTiming,
    activated_at_before: Option<ExpectedLrrttTiming>,
    activated_at_after: ExpectedLrrttTiming,
    last_inbound_after: ExpectedLrrttTiming,
    expected_hops_after: u8,
    keepalive_after: ExpectedLrrttTiming,
    stale_time_after: ExpectedLrrttTiming,
    callback_delta: u64,
    callback_count_after: u64,
    teardown_delta: u64,
    teardown_count_after: u64,
    rx_after: u64,
    rxbytes_after: u64,
}

fn verify_lrrtt_lifecycle(vectors: &LrrttLifecycleVector) -> Result<usize, String> {
    const RTT_025: ExpectedLrrttTiming = ExpectedLrrttTiming::seconds("0.25", "3fd0000000000000");
    const RTT_125: ExpectedLrrttTiming = ExpectedLrrttTiming::seconds("1.25", "3ff4000000000000");
    const RTT_225: ExpectedLrrttTiming = ExpectedLrrttTiming::seconds("2.25", "4002000000000000");
    const ACTIVATED_100_375: ExpectedLrrttTiming =
        ExpectedLrrttTiming::seconds("100.375", "4059180000000000");
    const ACTIVATED_101_375: ExpectedLrrttTiming =
        ExpectedLrrttTiming::seconds("101.375", "4059580000000000");
    const ACTIVATED_102_375: ExpectedLrrttTiming =
        ExpectedLrrttTiming::seconds("102.375", "4059980000000000");
    const VALID_CLOCK_ORDER: &[&str] = &[
        "receive_liveness_sample",
        "measured_rtt_sample",
        "activation_sample",
    ];
    const FAILED_CLOCK_ORDER: &[&str] = &["receive_liveness_sample", "measured_rtt_sample"];
    const EXPECTED_CASES: [ExpectedLrrttLifecycleCase; 5] = [
        ExpectedLrrttLifecycleCase {
            name: "handshake_valid",
            ciphertext_hex: "64657465726d696e69737469632d656e637279707465642d6c72727401",
            state_before: "HANDSHAKE",
            state_after: "ACTIVE",
            observed_clock_order: VALID_CLOCK_ORDER,
            rtt_before: None,
            rtt_after: RTT_025,
            activated_at_before: None,
            activated_at_after: ACTIVATED_100_375,
            last_inbound_after: ExpectedLrrttTiming::seconds("100.125", "4059080000000000"),
            expected_hops_after: 2,
            keepalive_after: ExpectedLrrttTiming::float_interval(
                "51.42857142857143",
                "4049b6db6db6db6e",
            ),
            stale_time_after: ExpectedLrrttTiming::float_interval(
                "102.85714285714286",
                "4059b6db6db6db6e",
            ),
            callback_delta: 1,
            callback_count_after: 1,
            teardown_delta: 0,
            teardown_count_after: 0,
            rx_after: 1,
            rxbytes_after: 29,
        },
        ExpectedLrrttLifecycleCase {
            name: "active_valid_repeat",
            ciphertext_hex: "64657465726d696e69737469632d656e637279707465642d6c72727402",
            state_before: "ACTIVE",
            state_after: "ACTIVE",
            observed_clock_order: VALID_CLOCK_ORDER,
            rtt_before: Some(RTT_025),
            rtt_after: RTT_125,
            activated_at_before: Some(ACTIVATED_100_375),
            activated_at_after: ACTIVATED_101_375,
            last_inbound_after: ExpectedLrrttTiming::seconds("101.125", "4059480000000000"),
            expected_hops_after: 3,
            keepalive_after: ExpectedLrrttTiming::float_interval(
                "257.14285714285717",
                "4070124924924925",
            ),
            stale_time_after: ExpectedLrrttTiming::float_interval(
                "514.2857142857143",
                "4080124924924925",
            ),
            callback_delta: 1,
            callback_count_after: 2,
            teardown_delta: 0,
            teardown_count_after: 0,
            rx_after: 2,
            rxbytes_after: 58,
        },
        ExpectedLrrttLifecycleCase {
            name: "stale_valid_repeat",
            ciphertext_hex: "64657465726d696e69737469632d656e637279707465642d6c72727403",
            state_before: "STALE",
            state_after: "ACTIVE",
            observed_clock_order: VALID_CLOCK_ORDER,
            rtt_before: Some(RTT_125),
            rtt_after: RTT_225,
            activated_at_before: Some(ACTIVATED_101_375),
            activated_at_after: ACTIVATED_102_375,
            last_inbound_after: ExpectedLrrttTiming::seconds("102.125", "4059880000000000"),
            expected_hops_after: 4,
            keepalive_after: ExpectedLrrttTiming::integer_interval("360"),
            stale_time_after: ExpectedLrrttTiming::integer_interval("720"),
            callback_delta: 1,
            callback_count_after: 3,
            teardown_delta: 0,
            teardown_count_after: 0,
            rx_after: 3,
            rxbytes_after: 87,
        },
        ExpectedLrrttLifecycleCase {
            name: "stale_decrypt_failure_repeat",
            ciphertext_hex: "64657465726d696e69737469632d656e637279707465642d6c72727404",
            state_before: "STALE",
            state_after: "ACTIVE",
            observed_clock_order: FAILED_CLOCK_ORDER,
            rtt_before: Some(RTT_225),
            rtt_after: RTT_225,
            activated_at_before: Some(ACTIVATED_102_375),
            activated_at_after: ACTIVATED_102_375,
            last_inbound_after: ExpectedLrrttTiming::seconds("103.125", "4059c80000000000"),
            expected_hops_after: 4,
            keepalive_after: ExpectedLrrttTiming::integer_interval("360"),
            stale_time_after: ExpectedLrrttTiming::integer_interval("720"),
            callback_delta: 0,
            callback_count_after: 3,
            teardown_delta: 0,
            teardown_count_after: 0,
            rx_after: 4,
            rxbytes_after: 116,
        },
        ExpectedLrrttLifecycleCase {
            name: "active_authenticated_malformed_repeat",
            ciphertext_hex: "64657465726d696e69737469632d656e637279707465642d6c72727405",
            state_before: "ACTIVE",
            state_after: "CLOSED",
            observed_clock_order: FAILED_CLOCK_ORDER,
            rtt_before: Some(RTT_225),
            rtt_after: RTT_225,
            activated_at_before: Some(ACTIVATED_102_375),
            activated_at_after: ACTIVATED_102_375,
            last_inbound_after: ExpectedLrrttTiming::seconds("104.125", "405a080000000000"),
            expected_hops_after: 4,
            keepalive_after: ExpectedLrrttTiming::integer_interval("360"),
            stale_time_after: ExpectedLrrttTiming::integer_interval("720"),
            callback_delta: 0,
            callback_count_after: 3,
            teardown_delta: 1,
            teardown_count_after: 1,
            rx_after: 5,
            rxbytes_after: 145,
        },
    ];

    let mut checks = 0;
    require(
        vectors.origin
            == "source-hash-backed deterministic method probes: request ordering executes released Link.__init__, Link.validate_request, Link.prove and Packet.send through a recorded Transport.outbound boundary; lifecycle cases directly invoke released Link.receive, Link.rtt_packet and Link.decrypt on a field-scaffolded Link",
        "unexpected LRRTT lifecycle origin",
    )?;
    require(
        vectors.link_source_sha256
            == "57122235df52704221c8c3645b8609de4c1cffd8b7b18e2de114b4b291d73725",
        "released RNS Link.py source drift",
    )?;
    require(
        vectors.packet_source_sha256
            == "ea6741a67cccbdf0a85a8eb42408ac5fe056deb10a533e3db25de4abb3111f1a",
        "released RNS Packet.py source drift",
    )?;
    require(
        vectors.clock == "Python time.time() float seconds",
        "unexpected released LRRTT clock",
    )?;
    require(
        string_slice_eq(
            &vectors.probe_scaffolding.request_time_ordering,
            &[
                "fixed identities and an actual RNS.Packet link request",
                "scripted RNS.Link and RNS.Packet module clocks",
                "fixed Transport hop and next-hop MTU lookups for the initiator",
                "no-op Transport.register_link",
                "recorded Transport.outbound instead of interface/network egress",
                "no-op Link.start_watchdog",
            ],
        ),
        "request-ordering probe scaffolding drift",
    )?;
    require(
        string_slice_eq(
            &vectors.probe_scaffolding.responder_lifecycle,
            &[
                "Link allocated without its constructor and required fields populated",
                "scripted RNS.Link module clock",
                "ProbeToken substituted at the cryptographic token decrypt boundary",
                "no-op Link physical-statistics updater",
                "recording owner link-established callback",
                "recording Link.teardown replacement that sets CLOSED",
                "case-unique synthetic packets passed directly to Link.receive",
            ],
        ),
        "responder-lifecycle probe scaffolding drift",
    )?;
    checks += 6;

    let initiator = &vectors.request_time_ordering.initiator;
    require(
        string_slice_eq(
            &initiator.released_methods,
            &["RNS.Link.__init__", "RNS.Packet.send"],
        ),
        "unexpected initiator request-time methods",
    )?;
    require(
        string_slice_eq(
            &initiator.observed_event_order,
            &[
                "request_time_sample",
                "transport_outbound_link_request",
                "last_outbound_sample",
            ],
        ),
        "initiator request-time event ordering drift",
    )?;
    require(
        initiator.semantic == "request_time is sampled before link request send",
        "unexpected initiator request-time semantic",
    )?;
    checks += 3;
    checks += verify_lrrtt_timing(
        "initiator request_time",
        &initiator.request_time,
        ExpectedLrrttTiming::seconds("10.0", "4024000000000000"),
    )?;
    checks += verify_lrrtt_timing(
        "initiator last_outbound",
        &initiator.last_outbound,
        ExpectedLrrttTiming::seconds("10.5", "4025000000000000"),
    )?;

    let responder = &vectors.request_time_ordering.responder;
    require(
        string_slice_eq(
            &responder.released_methods,
            &[
                "RNS.Link.validate_request",
                "RNS.Link.prove",
                "RNS.Packet.send",
            ],
        ),
        "unexpected responder request-time methods",
    )?;
    require(
        string_slice_eq(
            &responder.observed_event_order,
            &[
                "proof_packet_last_outbound_sample",
                "transport_outbound_link_proof",
                "proof_had_outbound_sample",
                "request_time_sample",
                "last_inbound_sample",
            ],
        ),
        "responder request-time event ordering drift",
    )?;
    require(
        responder.semantic == "request_time is sampled after link proof send returns",
        "unexpected responder request-time semantic",
    )?;
    checks += 3;
    checks += verify_lrrtt_timing(
        "responder request_time",
        &responder.request_time,
        ExpectedLrrttTiming::seconds("20.25", "4034400000000000"),
    )?;
    checks += verify_lrrtt_timing(
        "responder last_inbound",
        &responder.last_inbound,
        ExpectedLrrttTiming::seconds("20.5", "4034800000000000"),
    )?;

    let lifecycle = &vectors.responder_lifecycle;
    require(
        lifecycle.released_entrypoint == "RNS.Link.receive -> RNS.Link.rtt_packet",
        "unexpected released LRRTT lifecycle entrypoint",
    )?;
    require(
        lifecycle.request_time_semantic
            == "immutable responder request_time; each valid repeat measures from the original post-proof sample",
        "unexpected responder request-time lifecycle semantic",
    )?;
    require(
        lifecycle.authenticated_malformed_plaintext_hex == "c1",
        "authenticated malformed LRRTT fixture drift",
    )?;
    require(
        lifecycle.teardown_boundary
            == "RNS.Link.rtt_packet calls Link.teardown; the probe replaces that method with a recorder that sets CLOSED, so teardown packet egress, key purge and close callbacks are not exercised",
        "unexpected released LRRTT teardown boundary",
    )?;
    require(
        lifecycle.ingress_scope
            == "each case is passed directly to RNS.Link.receive; RNS.Transport inbound exact-replay deduplication is outside this corpus",
        "unexpected released LRRTT ingress scope",
    )?;
    require(
        !lifecycle.transport_exact_replay_dedup_exercised,
        "released lifecycle corpus must not claim Transport replay coverage",
    )?;
    require(
        lifecycle.cases.len() == EXPECTED_CASES.len(),
        "released LRRTT lifecycle must contain five cases",
    )?;
    checks += 7;

    let peer_wire = decode_hex(
        "lrrtt_lifecycle.peer_rtt_wire_hex",
        &lifecycle.peer_rtt_wire_hex,
    )?;
    require(
        lifecycle.peer_rtt_wire_hex == "cb3fc0000000000000",
        "released LRRTT lifecycle peer payload drift",
    )?;
    let peer_rtt = decode_lrrtt_number_for_conformance(&peer_wire)
        .map_err(|error| format!("Rete rejected lifecycle peer RTT: {error}"))?;
    require(
        peer_rtt.consumed == peer_wire.len(),
        "Rete did not consume the complete lifecycle peer RTT",
    )?;
    require_float_bits("lifecycle peer RTT", peer_rtt.value, 0.125)?;
    require(
        encode_lrrtt_float64_for_conformance(peer_rtt.value) == peer_wire,
        "Rete did not reproduce the lifecycle peer RTT payload",
    )?;
    checks += 4;

    let mut ciphertexts = BTreeSet::new();
    for (case, expected) in lifecycle.cases.iter().zip(&EXPECTED_CASES) {
        checks += verify_lrrtt_lifecycle_case(case, expected)?;
        require(
            ciphertexts.insert(case.ciphertext_hex.as_str()),
            "released LRRTT lifecycle ciphertext fixtures must be case-unique",
        )?;
        checks += 1;
    }

    for (index, measured_rtt) in [0.25, 1.25, 2.25].into_iter().enumerate() {
        let selected = select_lrrtt_for_conformance(measured_rtt, peer_rtt.value);
        let recorded = lrrtt_timing_f64(
            &format!("{} rtt_after", lifecycle.cases[index].name),
            &lifecycle.cases[index].rtt_after,
        )?;
        require_float_bits(
            &format!("{} immutable-request RTT", lifecycle.cases[index].name),
            selected,
            recorded,
        )?;
        checks += 1;
    }

    require(
        lifecycle.callback_observations.len() == 3,
        "released LRRTT lifecycle must contain three callback observations",
    )?;
    checks += 1;
    for (index, callback) in lifecycle.callback_observations.iter().enumerate() {
        let expected_rtt = [RTT_025, RTT_125, RTT_225][index];
        let expected_activation = [ACTIVATED_100_375, ACTIVATED_101_375, ACTIVATED_102_375][index];
        require(
            callback.state == "ACTIVE",
            &format!("callback {index} did not observe an active Link"),
        )?;
        require(
            callback.expected_hops == [2, 3, 4][index],
            &format!("callback {index} observed the wrong hop count"),
        )?;
        checks += 2;
        checks += verify_lrrtt_timing(
            &format!("callback {index} rtt"),
            &callback.rtt,
            expected_rtt,
        )?;
        checks += verify_lrrtt_timing(
            &format!("callback {index} activated_at"),
            &callback.activated_at,
            expected_activation,
        )?;
    }

    Ok(checks)
}

fn verify_lrrtt_lifecycle_case(
    case: &LrrttLifecycleCase,
    expected: &ExpectedLrrttLifecycleCase,
) -> Result<usize, String> {
    const REQUEST_TIME: ExpectedLrrttTiming =
        ExpectedLrrttTiming::seconds("100.0", "4059000000000000");
    let mut checks = 0;

    require(
        case.name == expected.name,
        "released LRRTT lifecycle case ordering or name drift",
    )?;
    require(
        case.ciphertext_hex == expected.ciphertext_hex,
        &format!("{} ciphertext fixture drift", case.name),
    )?;
    require(
        case.ciphertext_provenance
            == "case-unique synthetic bytes passed directly to RNS.Link.receive",
        &format!("{} ciphertext provenance drift", case.name),
    )?;
    require(
        decode_hex(
            &format!("{}.ciphertext_hex", case.name),
            &case.ciphertext_hex,
        )?
        .len()
            == 29,
        &format!("{} ciphertext fixture must be 29 bytes", case.name),
    )?;
    require(
        case.state_before == expected.state_before,
        &format!("{} has the wrong initial state", case.name),
    )?;
    require(
        case.state_after == expected.state_after,
        &format!("{} has the wrong final state", case.name),
    )?;
    require(
        string_slice_eq(&case.observed_clock_order, expected.observed_clock_order),
        &format!("{} clock ordering drift", case.name),
    )?;
    require(
        case.request_time_before == case.request_time_after,
        &format!("{} mutated immutable request_time", case.name),
    )?;
    require(
        case.last_data_after == case.last_inbound_after,
        &format!("{} did not refresh LRRTT data liveness", case.name),
    )?;
    checks += 9;

    checks += verify_lrrtt_timing(
        &format!("{} request_time_before", case.name),
        &case.request_time_before,
        REQUEST_TIME,
    )?;
    checks += verify_lrrtt_timing(
        &format!("{} request_time_after", case.name),
        &case.request_time_after,
        REQUEST_TIME,
    )?;
    checks += verify_optional_lrrtt_timing(
        &format!("{} rtt_before", case.name),
        case.rtt_before.as_ref(),
        expected.rtt_before,
    )?;
    checks += verify_lrrtt_timing(
        &format!("{} rtt_after", case.name),
        &case.rtt_after,
        expected.rtt_after,
    )?;
    checks += verify_optional_lrrtt_timing(
        &format!("{} activated_at_before", case.name),
        case.activated_at_before.as_ref(),
        expected.activated_at_before,
    )?;
    checks += verify_lrrtt_timing(
        &format!("{} activated_at_after", case.name),
        &case.activated_at_after,
        expected.activated_at_after,
    )?;
    checks += verify_lrrtt_timing(
        &format!("{} last_inbound_after", case.name),
        &case.last_inbound_after,
        expected.last_inbound_after,
    )?;
    checks += verify_lrrtt_timing(
        &format!("{} last_data_after", case.name),
        &case.last_data_after,
        expected.last_inbound_after,
    )?;
    checks += verify_lrrtt_timing(
        &format!("{} keepalive_after", case.name),
        &case.keepalive_after,
        expected.keepalive_after,
    )?;
    checks += verify_lrrtt_timing(
        &format!("{} stale_time_after", case.name),
        &case.stale_time_after,
        expected.stale_time_after,
    )?;

    require(
        case.expected_hops_after == expected.expected_hops_after,
        &format!("{} retained the wrong expected hop count", case.name),
    )?;
    require(
        case.callback_delta == expected.callback_delta,
        &format!("{} callback delta drift", case.name),
    )?;
    require(
        case.callback_count_after == expected.callback_count_after,
        &format!("{} callback count drift", case.name),
    )?;
    require(
        case.teardown_delta == expected.teardown_delta,
        &format!("{} teardown delta drift", case.name),
    )?;
    require(
        case.teardown_count_after == expected.teardown_count_after,
        &format!("{} teardown count drift", case.name),
    )?;
    require(
        case.rx_after == expected.rx_after,
        &format!("{} RX packet liveness counter drift", case.name),
    )?;
    require(
        case.rxbytes_after == expected.rxbytes_after,
        &format!("{} RX byte liveness counter drift", case.name),
    )?;
    checks += 7;

    Ok(checks)
}

fn verify_optional_lrrtt_timing(
    label: &str,
    actual: Option<&LrrttTimingValue>,
    expected: Option<ExpectedLrrttTiming>,
) -> Result<usize, String> {
    match (actual, expected) {
        (Some(actual), Some(expected)) => Ok(1 + verify_lrrtt_timing(label, actual, expected)?),
        (None, None) => Ok(1),
        _ => Err(format!("{label} presence drift")),
    }
}

fn verify_lrrtt_timing(
    label: &str,
    actual: &LrrttTimingValue,
    expected: ExpectedLrrttTiming,
) -> Result<usize, String> {
    require(
        actual.decimal == expected.decimal,
        &format!("{label} decimal drift"),
    )?;
    require(
        actual.f64_bits_hex.as_deref() == expected.f64_bits_hex,
        &format!("{label} f64 bits drift"),
    )?;
    require(
        actual.python_type.as_deref() == expected.python_type,
        &format!("{label} Python type drift"),
    )?;
    let parsed = actual
        .decimal
        .parse::<f64>()
        .map_err(|error| format!("invalid {label} decimal: {error}"))?;
    match expected.f64_bits_hex {
        Some(bits_hex) => {
            let bytes = decode_hex(&format!("{label}.f64_bits_hex"), bits_hex)?;
            let bits: [u8; 8] = bytes
                .try_into()
                .map_err(|_| format!("{label} float bits are not eight bytes"))?;
            require_float_bits(label, parsed, f64::from_be_bytes(bits))?;
        }
        None => require(
            parsed.is_finite() && parsed.fract() == 0.0,
            &format!("{label} integer interval is not integral"),
        )?,
    }
    Ok(4)
}

fn lrrtt_timing_f64(label: &str, value: &LrrttTimingValue) -> Result<f64, String> {
    let bits_hex = value
        .f64_bits_hex
        .as_deref()
        .ok_or_else(|| format!("{label} has no f64 bits"))?;
    let bytes = decode_hex(&format!("{label}.f64_bits_hex"), bits_hex)?;
    let bits: [u8; 8] = bytes
        .try_into()
        .map_err(|_| format!("{label} float bits are not eight bytes"))?;
    Ok(f64::from_be_bytes(bits))
}

fn string_slice_eq(actual: &[String], expected: &[&str]) -> bool {
    actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
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
        assert_eq!(verify_released_vectors(), Ok(647));
    }
}
