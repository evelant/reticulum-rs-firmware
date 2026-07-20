//! Narrow integration of the pinned Rete RNS foundation.
//!
//! The default product surface owns Rete behind [`EmbeddedNode`], while wire
//! parsing and validation remain available without copying. Deterministic raw
//! announce construction and fixture nodes are restricted to the
//! `conformance` feature and crate tests.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(any(test, feature = "conformance"))]
use rand_core::{CryptoRng, RngCore};
#[cfg(test)]
use rete_stack::NodeCore;
#[cfg(any(test, feature = "conformance"))]
use rete_transport::{HeaplessStorage, Transport};
use reticulum_rns_conformance::{CandidateMetadata, CandidateStatus};

pub mod capacity;
pub mod embedded;

pub use embedded::{
    AdmissionCounters, AnnounceAdmissionError, DestinationRegistrationError, EmbeddedNode,
    EmbeddedNodeConfig, EmbeddedNodeMetrics, EmbeddedSendError, InboundData, InboundDataProjection,
    InboundProofPolicy, IngressCounters, IngressDisposition, IngressDropReason, IngressMetadata,
    IngressReport, InterfaceId, MAX_CHANNEL_PAYLOAD, MAX_DATA_PAYLOAD, NodeActions, NodeRole,
    PrepareDataError, PreparedData, RNS_MTU, ReceiptCandidate, ReceiptId, ReceiptKind,
    ReceiptReservationUnavailable, ReceiptTerminal, ReceiptTerminalCounters,
    ReceiptTerminalReservation, ReceiptTerminalSink, ReceiptTickReport, RouteSnapshot,
    TransportCounters, TxPacket, TxTarget, project_inbound_data,
};
pub use rete_core::{DestHash, DestType, Identity, IdentityHash, LinkId, Packet, PacketType};
pub use rete_stack::{DestinationType, Direction, NodeEvent};
pub use rete_transport::{AnnounceError, AnnounceInfo, LinkState};

/// Reviewed Rete integration-fork source revision.
pub const SOURCE_REVISION: &str = "14c7b4955a1ff6903e87cc40b42498f7869b6f4f";

/// Initial table capacities used only to obtain comparable Phase-0 numbers.
pub mod probe_capacity {
    pub const PATHS: usize = 64;
    pub const ANNOUNCES: usize = 16;
    pub const DEDUPLICATION_ENTRIES: usize = 128;
    pub const LINKS: usize = 4;
}

/// Explicitly sized Rete storage used by crate-private white-box probes.
#[cfg(any(test, feature = "conformance"))]
pub(crate) type ProbeStorage = HeaplessStorage<
    { probe_capacity::PATHS },
    { probe_capacity::ANNOUNCES },
    { probe_capacity::DEDUPLICATION_ENTRIES },
    { probe_capacity::LINKS },
>;

/// Raw Rete node retained only for crate-private white-box probes.
#[cfg(test)]
pub(crate) type ProbeNode = NodeCore<ProbeStorage>;

/// Initial owning embedded profile used for integration and measurement.
pub type InitialEmbeddedNode = EmbeddedNode<
    { probe_capacity::PATHS },
    { probe_capacity::ANNOUNCES },
    { probe_capacity::DEDUPLICATION_ENTRIES },
    { probe_capacity::LINKS },
>;

/// Maximum application data that fits in a non-ratchet announce at the base
/// Reticulum MTU.
pub const MAX_ANNOUNCE_APP_DATA: usize =
    rete_core::MTU - rete_core::HEADER_1_OVERHEAD - rete_transport::announce::MIN_ANNOUNCE_PAYLOAD;

/// Metadata emitted with every Rete conformance result.
pub const fn metadata() -> CandidateMetadata {
    CandidateMetadata {
        id: "rete",
        source: "https://github.com/evelant/rete",
        revision: SOURCE_REVISION,
        license: "Apache-2.0",
        status: CandidateStatus::ProvisionalFoundation,
    }
}

/// Load an identity from Reticulum's 64-byte combined private-key format.
///
/// The returned error is Rete's native core error so invalid key material is
/// not flattened into an adapter-specific status.
pub fn identity_from_private_key(private_key: &[u8]) -> Result<Identity, rete_core::Error> {
    Identity::from_private_key(private_key)
}

/// Build a signed, non-ratchet Reticulum announce into a caller-owned buffer.
///
/// This conformance-only helper supplies an identity, entropy, Unix emission
/// time and caller-owned storage directly to Rete. Reticulum peers compare the
/// five-byte timestamp embedded in the announce random hash, so callers must
/// not pass monotonic uptime or move this value backwards for an identity.
/// Product firmware queues announces through [`EmbeddedNode`].
///
/// Rete's native [`rete_core::Error`] is returned unchanged.
#[cfg(any(test, feature = "conformance"))]
pub fn build_announce_packet<R: RngCore + CryptoRng>(
    identity: &Identity,
    app_name: &str,
    aspects: &[&str],
    app_data: Option<&[u8]>,
    rng: &mut R,
    emitted_at_unix_seconds: u64,
    out: &mut [u8],
) -> Result<usize, rete_core::Error> {
    if app_data.is_some_and(|data| data.len() > MAX_ANNOUNCE_APP_DATA) {
        return Err(rete_core::Error::PayloadTooLarge);
    }

    Transport::<ProbeStorage>::create_announce(
        identity,
        app_name,
        aspects,
        app_data,
        None,
        rng,
        emitted_at_unix_seconds,
        out,
    )
}

/// Parse a Reticulum packet as a zero-copy Rete packet view.
///
/// Packet length policy belongs to the interface boundary; this function
/// intentionally preserves Rete's support for negotiated MTUs above 500 bytes.
pub fn parse_packet(raw: &[u8]) -> Result<Packet<'_>, rete_core::Error> {
    Packet::parse(raw)
}

/// Result of decoding the first MessagePack number in an LRRTT plaintext.
///
/// This is a conformance-only view of Rete's native decoder. `consumed` makes
/// Python `umsgpack.unpackb()`'s first-object/trailing-byte behavior observable
/// without exposing any mutable transport state.
#[cfg(any(test, feature = "conformance"))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConformanceLrrttNumber {
    /// Numeric value returned by Rete's MessagePack decoder.
    pub value: f64,
    /// Bytes consumed by the first decoded object.
    pub consumed: usize,
}

/// Decode the first Python-compatible numeric LRRTT MessagePack object.
#[cfg(any(test, feature = "conformance"))]
pub fn decode_lrrtt_number_for_conformance(
    plaintext: &[u8],
) -> Result<ConformanceLrrttNumber, rete_core::msgpack::MsgpackError> {
    let mut consumed = 0;
    let value = rete_core::msgpack::read_float64(plaintext, &mut consumed)?;
    Ok(ConformanceLrrttNumber { value, consumed })
}

/// Encode one LRRTT value with Rete's canonical MessagePack float64 writer.
#[cfg(any(test, feature = "conformance"))]
pub fn encode_lrrtt_float64_for_conformance(value: f64) -> alloc::vec::Vec<u8> {
    let mut encoded = alloc::vec::Vec::with_capacity(9);
    rete_core::msgpack::write_float64(&mut encoded, value);
    encoded
}

/// Apply Python's `max(measured_rtt, peer_rtt)` comparison order.
///
/// The explicit comparison is significant for NaN: a peer NaN does not
/// replace a finite local measurement.
#[cfg(any(test, feature = "conformance"))]
pub fn select_lrrtt_for_conformance(measured_rtt: f64, peer_rtt: f64) -> f64 {
    if peer_rtt > measured_rtt {
        peer_rtt
    } else {
        measured_rtt
    }
}

/// A parsed packet together with its cryptographically validated announce
/// fields. Both views borrow the original packet bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedAnnounce<'a> {
    /// Zero-copy Reticulum packet view.
    pub packet: Packet<'a>,
    /// Public identity, hashes, signature and optional application data.
    pub fields: AnnounceInfo<'a>,
}

/// Failure while parsing and validating a complete announce packet.
///
/// Native Rete failures are retained inside their corresponding variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnouncePacketError {
    /// Rete could not parse the packet wire header.
    Packet(rete_core::Error),
    /// The packet parsed correctly but is not an announce.
    UnexpectedPacketType(PacketType),
    /// Rete rejected the announce payload or signature.
    Validation(AnnounceError),
}

impl core::fmt::Display for AnnouncePacketError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Packet(error) => write!(f, "packet parse failed: {error}"),
            Self::UnexpectedPacketType(packet_type) => {
                write!(f, "expected announce packet, got {packet_type:?}")
            }
            Self::Validation(error) => write!(f, "announce validation failed: {error}"),
        }
    }
}

/// Parse and cryptographically validate a complete Reticulum announce.
pub fn parse_announce_packet(raw: &[u8]) -> Result<ValidatedAnnounce<'_>, AnnouncePacketError> {
    // Announce packets are base Reticulum packets, never negotiated Link-MTU
    // packets. Rete's validator uses a fixed-MTU signature scratch buffer and
    // will panic if oversized app-data is allowed to reach it.
    if raw.len() > rete_core::MTU {
        return Err(AnnouncePacketError::Packet(rete_core::Error::PacketTooLong));
    }
    let packet = parse_packet(raw).map_err(AnnouncePacketError::Packet)?;
    if packet.packet_type != PacketType::Announce {
        return Err(AnnouncePacketError::UnexpectedPacketType(
            packet.packet_type,
        ));
    }

    let fields = rete_transport::validate_announce(
        packet.destination_hash,
        packet.payload,
        packet.context_flag,
    )
    .map_err(AnnouncePacketError::Validation)?;

    Ok(ValidatedAnnounce { packet, fields })
}

/// Construct a deterministic owning endpoint for host/vector probes only.
///
/// Production firmware must create or load an identity from qualified entropy;
/// it must never call this helper or ship a fixture seed.
#[cfg(any(test, feature = "conformance"))]
pub fn new_conformance_node(seed: &[u8; 32]) -> Result<InitialEmbeddedNode, rete_core::Error> {
    let identity = Identity::from_seed(seed)?;
    InitialEmbeddedNode::new(
        identity,
        "reticulum",
        &["phase0"],
        EmbeddedNodeConfig::endpoint(),
    )
}

/// Construct a deterministic owning transport node for host/vector probes.
#[cfg(any(test, feature = "conformance"))]
pub fn new_conformance_transport_node(
    seed: &[u8; 32],
) -> Result<InitialEmbeddedNode, rete_core::Error> {
    let identity = Identity::from_seed(seed)?;
    InitialEmbeddedNode::new(
        identity,
        "reticulum",
        &["phase0"],
        EmbeddedNodeConfig::transport(),
    )
}

/// Known allocation findings that must be closed before acceptance.
pub const KNOWN_ALLOCATION_GAPS: &[&str] = &[
    "NodeCore and NodeEvent contain network-sized Vec allocations",
    "Resource receive and completion retain whole payload buffers",
    "some full-table insertion failures are not surfaced",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ZeroRng;

    impl RngCore for ZeroRng {
        fn next_u32(&mut self) -> u32 {
            0
        }

        fn next_u64(&mut self) -> u64 {
            0
        }

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            dest.fill(0);
        }

        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl CryptoRng for ZeroRng {}

    fn unhex(value: &str) -> alloc::vec::Vec<u8> {
        (0..value.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn pinned_core_agrees_with_project_mtu() {
        assert_eq!(rete_core::MTU, reticulum_rns_conformance::RNS_MTU);
    }

    #[test]
    fn deterministic_probe_node_constructs() {
        let node = new_conformance_node(&[0x52; 32]).unwrap();
        assert_ne!(node.destination_hash().as_ref(), &[0u8; 16]);
    }

    #[test]
    fn candidate_is_explicitly_provisional() {
        assert_eq!(metadata().status, CandidateStatus::ProvisionalFoundation);
        assert_eq!(metadata().source, "https://github.com/evelant/rete");
        assert_eq!(metadata().revision, SOURCE_REVISION);
        assert!(!KNOWN_ALLOCATION_GAPS.is_empty());
    }

    #[test]
    fn lrrtt_conformance_helpers_preserve_python_scalar_rules() {
        let encoded = encode_lrrtt_float64_for_conformance(0.125);
        assert_eq!(encoded, unhex("cb3fc0000000000000"));

        let decoded = decode_lrrtt_number_for_conformance(&[
            0xcb, 0x3f, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0,
        ])
        .unwrap();
        assert_eq!(decoded.value, 0.125);
        assert_eq!(decoded.consumed, 9);
        assert_eq!(select_lrrtt_for_conformance(0.25, decoded.value), 0.25);
        assert_eq!(select_lrrtt_for_conformance(0.25, f64::NAN), 0.25);
    }

    #[test]
    fn pinned_fork_enforces_native_link_request_shape() {
        let responder = Identity::from_seed(b"native request responder").unwrap();
        let initiator = Identity::from_seed(b"native request initiator").unwrap();
        let mut name_buffer = [0u8; 64];
        let expanded_name = rete_core::expand_name("testapp", &["link"], &mut name_buffer).unwrap();
        let destination_hash = rete_core::destination_hash(expanded_name, Some(&responder.hash()));

        for (destination_type, context, payload_length, expected_valid) in [
            (DestType::Single, 0x00, 64, true),
            (DestType::Single, 0x00, 67, true),
            (DestType::Single, 0x00, 0, false),
            (DestType::Single, 0x00, 63, false),
            (DestType::Single, 0x00, 65, false),
            (DestType::Single, 0x00, 66, false),
            (DestType::Single, 0x00, 68, false),
            (DestType::Group, 0x00, 67, false),
            (DestType::Plain, 0x00, 67, false),
            (DestType::Link, 0x00, 67, false),
            (DestType::Single, 0x01, 67, false),
            (DestType::Single, 0xFF, 67, false),
        ] {
            let mut transport = Transport::<ProbeStorage>::new();
            transport.add_local_destination(destination_hash);
            let mut rng = ZeroRng;
            let (_, canonical_payload) = rete_transport::Link::new_initiator(
                destination_hash,
                initiator.ed25519_pub(),
                &mut rng,
                100,
            );
            let mut payload = canonical_payload.to_vec();
            payload.resize(payload_length, 0);
            payload.truncate(payload_length);

            let mut packet_buffer = [0u8; rete_core::MTU];
            let packet_length = rete_core::PacketBuilder::new(&mut packet_buffer)
                .packet_type(PacketType::LinkRequest)
                .dest_type(destination_type)
                .destination_hash(destination_hash.as_ref())
                .context(context)
                .payload(&payload)
                .build()
                .unwrap();

            let result = transport.ingest(
                &mut packet_buffer[..packet_length],
                100,
                &mut rng,
                &responder,
            );
            if expected_valid {
                match result {
                    rete_transport::IngestResult::LinkRequestReceived { proof_raw, .. } => {
                        assert!(!proof_raw.is_empty());
                    }
                    other => panic!(
                        "expected canonical {payload_length}-byte request to be accepted, got {other:?}"
                    ),
                }
                assert_eq!(transport.link_count(), 1);
                assert_eq!(transport.stats().packets_dropped_invalid, 0);
                assert_eq!(transport.stats().link_requests_received, 1);
            } else {
                assert!(
                    matches!(result, rete_transport::IngestResult::Invalid),
                    "expected {destination_type:?}/{context:#04x}/{payload_length} to be invalid"
                );
                assert_eq!(transport.link_count(), 0);
                assert_eq!(transport.stats().packets_dropped_invalid, 1);
                assert_eq!(transport.stats().link_requests_received, 0);
            }
            assert_eq!(transport.stats().links_failed, 0);
            assert_eq!(transport.stats().crypto_failures, 0);
        }
    }

    #[test]
    fn python_announce_vector_round_trips_through_public_slice() {
        // Generated by the repository's pinned Python-RNS 1.3.8 corpus. The
        // host conformance runner independently checks the committed JSON and
        // its generator against the released peer manifest.
        let private_key = unhex(
            "408b27d3097eea5a46bf2ab6433a7234a33d5e49957b13ec7acc2ca08e1a13c7\
             5272c90c8d3385d47ede5420a7a9623aad817d9f8a70bd100a0acea7400daa59",
        );
        let expected_raw = unhex(
            "01002b7fa6842783252974dc5fcaff22b80800\
             80ffd69d6399c09c790748a2783b9bd5198652b2e14d496eaf4d29ce06a0ea0f\
             a175c596dc0558fd271c185e89f2c85f8bc490c0e7dd25da0b0142246da9628f\
             fca709a4818d4e0c78a00000000000006553f100\
             50fe696f35b4fc3c4e43e2269372ae2b603ac90dd64757c8ac224bb80f0cabd\
             4e2863f7bc593cd3a785d360ba48485fad67a39617880214dd16086c6e53d8205",
        );
        let expected_packet_hash =
            unhex("b63705cf3ed52d56e32e8e17fbd86f51f391b9ce86a1a38f0f3649c058e74cae");

        let identity = identity_from_private_key(&private_key).unwrap();
        let mut raw = [0u8; rete_core::MTU];
        let packet_len = build_announce_packet(
            &identity,
            "testapp",
            &["aspect1"],
            None,
            &mut ZeroRng,
            1_700_000_000,
            &mut raw,
        )
        .unwrap();

        assert_eq!(&raw[..packet_len], expected_raw);

        let announce = parse_announce_packet(&raw[..packet_len]).unwrap();
        assert_eq!(announce.packet.packet_type, PacketType::Announce);
        assert_eq!(
            announce.packet.compute_hash().as_slice(),
            expected_packet_hash
        );
        assert_eq!(announce.fields.identity_hash, identity.hash());
        assert_eq!(announce.fields.pub_key, identity.public_key());
        assert_eq!(announce.fields.name_hash, unhex("fca709a4818d4e0c78a0"));
        assert_eq!(announce.fields.random_hash, unhex("0000000000006553f100"));
        assert_eq!(announce.fields.app_data, None);
    }

    #[test]
    fn protocol_failures_retain_native_rete_errors() {
        assert_eq!(
            parse_announce_packet(&[0x01, 0x00]),
            Err(AnnouncePacketError::Packet(
                rete_core::Error::PacketTooShort
            ))
        );

        let mut oversized_announce = [0u8; rete_core::MTU + 1];
        oversized_announce[0] = 0x01;
        assert_eq!(
            parse_announce_packet(&oversized_announce),
            Err(AnnouncePacketError::Packet(rete_core::Error::PacketTooLong))
        );

        let identity = Identity::from_seed(b"bounded announce").unwrap();
        let oversized = [0u8; MAX_ANNOUNCE_APP_DATA + 1];
        let mut raw = [0u8; rete_core::MTU];
        assert_eq!(
            build_announce_packet(
                &identity,
                "reticulum",
                &["phase0"],
                Some(&oversized),
                &mut ZeroRng,
                0,
                &mut raw,
            ),
            Err(rete_core::Error::PayloadTooLarge)
        );
    }

    #[test]
    fn maximum_announce_app_data_exactly_fills_the_base_mtu() {
        let identity = Identity::from_seed(b"announce boundary").unwrap();
        let app_data = [0xA5; MAX_ANNOUNCE_APP_DATA];
        let mut raw = [0u8; rete_core::MTU];
        let packet_len = build_announce_packet(
            &identity,
            "reticulum",
            &["phase0"],
            Some(&app_data),
            &mut ZeroRng,
            0,
            &mut raw,
        )
        .unwrap();

        assert_eq!(packet_len, rete_core::MTU);
        let announce = parse_announce_packet(&raw[..packet_len]).unwrap();
        assert_eq!(announce.fields.app_data, Some(app_data.as_slice()));
    }

    #[test]
    fn signature_tampering_is_reported_as_announce_validation() {
        let identity = Identity::from_seed(b"tamper fixture").unwrap();
        let mut raw = [0u8; rete_core::MTU];
        let packet_len = build_announce_packet(
            &identity,
            "reticulum",
            &["phase0"],
            None,
            &mut ZeroRng,
            42,
            &mut raw,
        )
        .unwrap();
        raw[packet_len - 1] ^= 0x01;

        assert_eq!(
            parse_announce_packet(&raw[..packet_len]),
            Err(AnnouncePacketError::Validation(
                AnnounceError::InvalidSignature
            ))
        );
    }
}
