use reticulum_lxmf_ingress::{
    CarrierIngress, ParsedCarrierOutcome, RejectedIngress, SignatureVerification, WireLimits,
    parse_lxmf_carrier,
};
use serde::Deserialize;

const CORPUS_JSON: &str = include_str!("../../../interop/vectors/lxmf-1.0.1-v1.json");

#[derive(Deserialize)]
struct Corpus {
    messages: Vec<MessageFixture>,
    negative_mutations: Vec<NegativeFixture>,
}

#[derive(Deserialize)]
struct MessageFixture {
    name: String,
    destination_hash_hex: String,
    source_hash_hex: String,
    source_public_key_hex: String,
    ingress: IngressFixture,
}

#[derive(Deserialize)]
struct IngressFixture {
    carrier_event: String,
    payload_hex: String,
}

#[derive(Deserialize)]
struct NegativeFixture {
    name: String,
    full_wire_hex: String,
}

fn decode(value: &str) -> Vec<u8> {
    hex::decode(value).expect("fixture hex")
}

fn array<const N: usize>(value: &str) -> [u8; N] {
    decode(value).try_into().expect("fixed fixture width")
}

fn limits() -> WireLimits {
    WireLimits::new(4_096, 2_048, 256, 2_048, 65_536, 16)
}

#[test]
fn default_parser_records_python_signature_state_without_deferral() {
    let corpus: Corpus = serde_json::from_str(CORPUS_JSON).expect("checked-in LXMF corpus");
    let fixture = corpus
        .messages
        .iter()
        .find(|fixture| fixture.name == "basic_binary")
        .expect("basic fixture");
    assert_eq!(fixture.ingress.carrier_event, "destination_data");
    let destination = array::<16>(&fixture.destination_hash_hex);
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let payload = decode(&fixture.ingress.payload_hex);

    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let ParsedCarrierOutcome::Parsed(validated) = parse_lxmf_carrier(
        CarrierIngress::Opportunistic {
            implied_destination: &destination,
            payload: &payload,
        },
        limits(),
        &resolver,
    ) else {
        panic!("released Python LXMF carrier parses")
    };
    assert_eq!(
        validated.evidence().signature_verification(),
        SignatureVerification::Validated
    );

    let unknown = |_: &[u8; 16]| None::<[u8; 64]>;
    let ParsedCarrierOutcome::Parsed(source_unknown) = parse_lxmf_carrier(
        CarrierIngress::Opportunistic {
            implied_destination: &destination,
            payload: &payload,
        },
        limits(),
        &unknown,
    ) else {
        panic!("an unknown source is delivered with metadata")
    };
    assert_eq!(
        source_unknown.evidence().signature_verification(),
        SignatureVerification::SourceUnknown
    );

    let invalid = corpus
        .negative_mutations
        .iter()
        .find(|fixture| fixture.name == "signature_bit_flip")
        .expect("signature mutation");
    let invalid_wire = decode(&invalid.full_wire_hex);
    let ParsedCarrierOutcome::Parsed(invalid_signature) = parse_lxmf_carrier(
        CarrierIngress::Opportunistic {
            implied_destination: &destination,
            payload: &invalid_wire[16..],
        },
        limits(),
        &resolver,
    ) else {
        panic!("an invalid signature is delivered with metadata")
    };
    assert_eq!(
        invalid_signature.evidence().signature_verification(),
        SignatureVerification::Invalid
    );

    let truncated = corpus
        .negative_mutations
        .iter()
        .find(|fixture| fixture.name == "truncated_payload")
        .expect("truncated mutation");
    let truncated_wire = decode(&truncated.full_wire_hex);
    assert!(matches!(
        parse_lxmf_carrier(
            CarrierIngress::Opportunistic {
                implied_destination: &destination,
                payload: &truncated_wire[16..],
            },
            limits(),
            &resolver,
        ),
        ParsedCarrierOutcome::Rejected(RejectedIngress::Wire(_))
    ));
}
