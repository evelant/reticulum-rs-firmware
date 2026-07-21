use core::cell::Cell;

use reticulum_lxmf_ingress::{
    DeferredIngress, IngressOutcome, LocalDeliveryDestination, RejectedIngress,
    SourceIdentityResolver, UnrelatedEvent, UnsupportedCarrier, validate_application_event,
};
use reticulum_lxmf_wire::{
    CarrierKind, PowBudget, RequiredStampCost, StampAdmission, StampError, StampPolicy, WireLimits,
};
use reticulum_node_core::{
    APPLICATION_LINK_CONTEXT_NONE, ApplicationEvent, ApplicationEventDiscardReason,
    ApplicationEventOwner, ApplicationEventSlot, NodeActions,
};
use serde::Deserialize;

const CORPUS_JSON: &str = include_str!("../../../interop/vectors/lxmf-1.0.1-v1.json");

fn limits() -> WireLimits {
    WireLimits::new(4_096, 2_048, 256, 2_048, 65_536, 16)
}

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
    message_id_hex: String,
    decoded: DecodedFixture,
    ingress: IngressFixture,
    stamp: Option<StampFixture>,
}

#[derive(Deserialize)]
struct DecodedFixture {
    timestamp_f64_bits_hex: String,
    title_hex: String,
    content_hex: String,
}

#[derive(Deserialize)]
struct IngressFixture {
    carrier_event: String,
    payload_hex: String,
}

#[derive(Deserialize)]
struct StampFixture {
    ticket_hex: Option<String>,
    target_cost: Option<u16>,
    value: u16,
}

#[derive(Deserialize)]
struct NegativeFixture {
    name: String,
    full_wire_hex: String,
}

fn corpus() -> Corpus {
    serde_json::from_str(CORPUS_JSON).expect("checked-in LXMF corpus")
}

fn fixture<'a>(corpus: &'a Corpus, name: &str) -> &'a MessageFixture {
    corpus
        .messages
        .iter()
        .find(|fixture| fixture.name == name)
        .expect("fixture name")
}

fn mutation<'a>(corpus: &'a Corpus, name: &str) -> &'a NegativeFixture {
    corpus
        .negative_mutations
        .iter()
        .find(|fixture| fixture.name == name)
        .expect("mutation name")
}

fn decode(value: &str) -> Vec<u8> {
    hex::decode(value).expect("fixture hex")
}

fn array<const N: usize>(value: &str) -> [u8; N] {
    decode(value).try_into().expect("fixed fixture width")
}

struct RecordingResolver {
    expected_source: [u8; 16],
    public_key: Option<[u8; 64]>,
    calls: Cell<usize>,
}

impl SourceIdentityResolver for RecordingResolver {
    fn resolve_source_identity(&self, source_destination: &[u8; 16]) -> Option<[u8; 64]> {
        assert_eq!(source_destination, &self.expected_source);
        self.calls.set(self.calls.get() + 1);
        self.public_key
    }
}

fn fixture_resolver(fixture: &MessageFixture) -> RecordingResolver {
    RecordingResolver {
        expected_source: array(&fixture.source_hash_hex),
        public_key: Some(array(&fixture.source_public_key_hex)),
        calls: Cell::new(0),
    }
}

fn opportunistic_event(fixture: &MessageFixture) -> ApplicationEvent {
    assert_eq!(fixture.ingress.carrier_event, "destination_data");
    ApplicationEvent::DataReceived {
        destination: array(&fixture.destination_hash_hex),
        payload: decode(&fixture.ingress.payload_hex),
    }
}

fn opportunistic_event_from_complete_wire(fixture: &MessageFixture) -> ApplicationEvent {
    let wire = decode(&fixture.ingress.payload_hex);
    let destination = array(&fixture.destination_hash_hex);
    assert_eq!(wire.get(..16), Some(destination.as_slice()));
    ApplicationEvent::DataReceived {
        destination,
        payload: wire[16..].to_vec(),
    }
}

#[test]
fn python_opportunistic_corpus_validates_without_copying_event_payload() {
    let corpus = corpus();
    for name in ["basic_binary", "rich_fields", "opportunistic_limit_295"] {
        let fixture = fixture(&corpus, name);
        let event = opportunistic_event(fixture);
        let ApplicationEvent::DataReceived { payload, .. } = &event else {
            unreachable!()
        };
        let expected_pointer = payload.as_ptr();
        let expected_length = payload.len();
        let resolver = fixture_resolver(fixture);
        let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
        let IngressOutcome::Validated(validated) = validate_application_event(
            &event,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ) else {
            panic!("{name} must validate")
        };

        assert_eq!(resolver.calls.get(), 1, "{name}");
        assert_eq!(
            validated.carrier_payload().as_ptr(),
            expected_pointer,
            "{name}"
        );
        assert_eq!(validated.carrier_payload().len(), expected_length, "{name}");
        assert_eq!(
            validated.evidence().message_id(),
            &array::<32>(&fixture.message_id_hex)
        );
        assert_ne!(
            validated.evidence().authenticated_material_fingerprint(),
            validated.evidence().message_id(),
            "the durable collision discriminator is domain-separated from the protocol ID"
        );
        assert_eq!(validated.evidence().destination(), local.as_bytes());
        assert_eq!(
            validated.evidence().source(),
            &array::<16>(&fixture.source_hash_hex)
        );
        assert_eq!(
            validated.evidence().timestamp_bits(),
            u64::from_be_bytes(array(&fixture.decoded.timestamp_f64_bits_hex))
        );
        assert_eq!(
            validated.evidence().title_len(),
            decode(&fixture.decoded.title_hex).len()
        );
        assert_eq!(
            validated.evidence().content_len(),
            decode(&fixture.decoded.content_hex).len()
        );
        assert_eq!(validated.evidence().carrier(), CarrierKind::Opportunistic);
        assert_eq!(
            validated.evidence().normalized_wire_len(),
            expected_length + 16
        );
        assert_eq!(
            validated.evidence().stamp_admission(),
            StampAdmission::NotRequired {
                stamp_present: false
            }
        );

        let debug = format!("{validated:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&fixture.ingress.payload_hex));
    }
}

#[test]
fn python_opportunistic_ceiling_is_not_mistaken_for_the_temporary_raw_inbox_ceiling() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "opportunistic_limit_295");
    let event = opportunistic_event(fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &event else {
        unreachable!()
    };
    assert!(payload.len() > reticulum_rns_inbox_store::MAX_PAYLOAD_SIZE);

    let resolver = fixture_resolver(fixture);
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let IngressOutcome::Validated(validated) =
        validate_application_event(&event, local, limits(), &resolver, StampPolicy::NotRequired)
    else {
        panic!("the Python-valid boundary must not be narrowed by a test-only store")
    };
    assert_eq!(validated.carrier_payload().len(), payload.len());
}

#[test]
fn destination_and_noncarrier_events_are_classified_without_identity_lookup() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "basic_binary");
    let event = opportunistic_event(fixture);
    let resolver = fixture_resolver(fixture);
    let other_local = LocalDeliveryDestination::new([0x55; 16]);
    assert!(matches!(
        validate_application_event(
            &event,
            other_local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ),
        IngressOutcome::Unrelated(UnrelatedEvent::OtherDestination {
            expected,
            actual,
        }) if expected == other_local && actual == array::<16>(&fixture.destination_hash_hex)
    ));
    assert_eq!(resolver.calls.get(), 0);

    let tick = ApplicationEvent::Tick {
        expired_paths: 1,
        closed_links: 2,
    };
    assert!(matches!(
        validate_application_event(
            &tick,
            other_local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ),
        IngressOutcome::Unrelated(UnrelatedEvent::OtherKind(_))
    ));
    assert_eq!(resolver.calls.get(), 0);

    let other_context = ApplicationEvent::LinkData {
        link: [0x66; 16],
        data: vec![0xa5; 9],
        context: 0x5a,
    };
    let ApplicationEvent::LinkData { data, .. } = &other_context else {
        unreachable!()
    };
    let pointer = data.as_ptr();
    assert!(matches!(
        validate_application_event(
            &other_context,
            other_local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ),
        IngressOutcome::Unrelated(UnrelatedEvent::OtherLinkContext {
            link,
            context: 0x5a,
        }) if link == [0x66; 16]
    ));
    let ApplicationEvent::LinkData { data, .. } = &other_context else {
        unreachable!()
    };
    assert_eq!(data.as_ptr(), pointer);
    assert_eq!(resolver.calls.get(), 0);
}

#[test]
fn link_and_resource_carriers_are_explicitly_deferred_unchanged() {
    let resolver = |_source: &[u8; 16]| Some([0_u8; 64]);
    let local = LocalDeliveryDestination::new([0x11; 16]);
    let link = ApplicationEvent::LinkData {
        link: [0x22; 16],
        data: vec![0xa5; 17],
        context: APPLICATION_LINK_CONTEXT_NONE,
    };
    let ApplicationEvent::LinkData { data, .. } = &link else {
        unreachable!()
    };
    let link_pointer = data.as_ptr();
    assert!(matches!(
        validate_application_event(
            &link,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ),
        IngressOutcome::Deferred(DeferredIngress::UnsupportedCarrier(
            UnsupportedCarrier::LinkData {
                link,
                context: APPLICATION_LINK_CONTEXT_NONE,
            }
        )) if link == [0x22; 16]
    ));
    let ApplicationEvent::LinkData { data, .. } = &link else {
        unreachable!()
    };
    assert_eq!(data.as_ptr(), link_pointer);

    let resource = ApplicationEvent::ResourceComplete {
        link: [0x33; 16],
        resource_hash: [0x44; 16],
        data: vec![0x5a; 19],
    };
    let ApplicationEvent::ResourceComplete { data, .. } = &resource else {
        unreachable!()
    };
    let resource_pointer = data.as_ptr();
    assert!(matches!(
        validate_application_event(
            &resource,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ),
        IngressOutcome::Deferred(DeferredIngress::UnsupportedCarrier(
            UnsupportedCarrier::ResourceComplete { link, resource_hash }
        )) if link == [0x33; 16] && resource_hash == [0x44; 16]
    ));
    let ApplicationEvent::ResourceComplete { data, .. } = &resource else {
        unreachable!()
    };
    assert_eq!(data.as_ptr(), resource_pointer);
}

#[test]
fn missing_source_identity_is_retryable_and_preserves_exact_event() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "rich_fields");
    let event = opportunistic_event(fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &event else {
        unreachable!()
    };
    let pointer = payload.as_ptr();
    let resolver = RecordingResolver {
        expected_source: array(&fixture.source_hash_hex),
        public_key: None,
        calls: Cell::new(0),
    };
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let outcome =
        validate_application_event(&event, local, limits(), &resolver, StampPolicy::NotRequired);
    assert!(matches!(
        outcome,
        IngressOutcome::Deferred(reason @ DeferredIngress::SourceIdentityUnavailable { source })
            if source == array::<16>(&fixture.source_hash_hex)
                && reason.retryable_after_identity_update()
    ));
    assert_eq!(resolver.calls.get(), 1);
    let ApplicationEvent::DataReceived { payload, .. } = &event else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), pointer);
}

#[test]
fn wire_signature_and_stamp_rejections_remain_distinct() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "basic_binary");
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));

    let truncated = mutation(&corpus, "truncated_payload");
    let truncated_wire = decode(&truncated.full_wire_hex);
    let truncated_event = ApplicationEvent::DataReceived {
        destination: *local.as_bytes(),
        payload: truncated_wire[16..].to_vec(),
    };
    let resolver = fixture_resolver(fixture);
    assert!(matches!(
        validate_application_event(
            &truncated_event,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ),
        IngressOutcome::Rejected(RejectedIngress::Wire(_))
    ));
    assert_eq!(resolver.calls.get(), 0);

    let signature = mutation(&corpus, "signature_bit_flip");
    let signature_wire = decode(&signature.full_wire_hex);
    let signature_event = ApplicationEvent::DataReceived {
        destination: *local.as_bytes(),
        payload: signature_wire[16..].to_vec(),
    };
    let resolver = fixture_resolver(fixture);
    assert!(matches!(
        validate_application_event(
            &signature_event,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ),
        IngressOutcome::Rejected(RejectedIngress::Signature(_))
    ));
    assert_eq!(resolver.calls.get(), 1);

    let event = opportunistic_event(fixture);
    let resolver = fixture_resolver(fixture);
    assert!(matches!(
        validate_application_event(
            &event,
            local,
            limits(),
            &resolver,
            StampPolicy::TrustedPriorTickets(&[]),
        ),
        IngressOutcome::Rejected(RejectedIngress::StampPolicy(_))
    ));
    assert_eq!(resolver.calls.get(), 1);
}

#[test]
fn incomplete_stamp_work_is_deferred_instead_of_conclusively_rejected() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "pow_stamp_32");
    assert_eq!(fixture.ingress.carrier_event, "link_data");
    let full_wire = decode(&fixture.ingress.payload_hex);
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let event = ApplicationEvent::DataReceived {
        destination: *local.as_bytes(),
        payload: full_wire[16..].to_vec(),
    };
    let resolver = fixture_resolver(fixture);
    let target_cost = RequiredStampCost::new(8).expect("fixture policy cost");
    assert!(matches!(
        validate_application_event(
            &event,
            local,
            limits(),
            &resolver,
            StampPolicy::ProofOfWork {
                target_cost,
                budget: PowBudget::new(0),
            },
        ),
        IngressOutcome::Deferred(DeferredIngress::StampValidationUnavailable(
            StampError::BudgetExceeded {
                required: 3_000,
                authorized: 0,
            }
        ))
    ));
}

#[test]
fn completed_ticket_and_pow_work_admit_while_invalid_pow_is_rejected() {
    let corpus = corpus();

    let ticket_fixture = fixture(&corpus, "ticket_stamp_16");
    let ticket_stamp = ticket_fixture.stamp.as_ref().expect("ticket stamp");
    let trusted_ticket = array::<16>(
        ticket_stamp
            .ticket_hex
            .as_deref()
            .expect("trusted ticket bytes"),
    );
    let trusted_tickets = [trusted_ticket];
    let ticket_event = opportunistic_event_from_complete_wire(ticket_fixture);
    let ticket_resolver = fixture_resolver(ticket_fixture);
    let ticket_local = LocalDeliveryDestination::new(array(&ticket_fixture.destination_hash_hex));
    let IngressOutcome::Validated(ticket) = validate_application_event(
        &ticket_event,
        ticket_local,
        limits(),
        &ticket_resolver,
        StampPolicy::TrustedPriorTickets(&trusted_tickets),
    ) else {
        panic!("trusted ticket must admit")
    };
    assert_eq!(
        ticket.evidence().stamp_admission(),
        StampAdmission::TrustedPriorTicket
    );

    let pow_fixture = fixture(&corpus, "pow_stamp_32");
    let pow_stamp = pow_fixture.stamp.as_ref().expect("PoW stamp");
    let target_cost =
        RequiredStampCost::new(pow_stamp.target_cost.expect("PoW target cost")).unwrap();
    let pow_event = opportunistic_event_from_complete_wire(pow_fixture);
    let pow_resolver = fixture_resolver(pow_fixture);
    let pow_local = LocalDeliveryDestination::new(array(&pow_fixture.destination_hash_hex));
    let IngressOutcome::Validated(pow) = validate_application_event(
        &pow_event,
        pow_local,
        limits(),
        &pow_resolver,
        StampPolicy::ProofOfWork {
            target_cost,
            budget: PowBudget::new(3_000),
        },
    ) else {
        panic!("completed valid PoW must admit")
    };
    assert_eq!(
        pow.evidence().stamp_admission(),
        StampAdmission::ProofOfWork {
            target_cost,
            value: pow_stamp.value,
        }
    );

    let invalid = mutation(&corpus, "pow_stamp_bit_flip");
    let invalid_wire = decode(&invalid.full_wire_hex);
    let invalid_event = ApplicationEvent::DataReceived {
        destination: *pow_local.as_bytes(),
        payload: invalid_wire[16..].to_vec(),
    };
    let invalid_resolver = fixture_resolver(pow_fixture);
    assert!(matches!(
        validate_application_event(
            &invalid_event,
            pow_local,
            limits(),
            &invalid_resolver,
            StampPolicy::ProofOfWork {
                target_cost,
                budget: PowBudget::new(3_000),
            },
        ),
        IngressOutcome::Rejected(RejectedIngress::StampPolicy(_))
    ));
}

#[test]
fn application_event_owner_retains_event_until_caller_disposes_lease() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "basic_binary");
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    owner
        .try_offer_actions(NodeActions::without_retained_proofs(
            vec![opportunistic_event(fixture)],
            vec![],
            0,
        ))
        .expect("event owner admission");

    let lease = owner.lease_next().expect("ready event");
    let event_id = lease.id();
    let resolver = fixture_resolver(fixture);
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let IngressOutcome::Validated(validated) = validate_application_event(
        lease.event(),
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
    ) else {
        panic!("owned event must validate")
    };
    let payload_pointer = validated.carrier_payload().as_ptr();
    let message_id = *validated.evidence().message_id();
    drop(validated);

    // Validation alone performs no disposition. An unresolved lease therefore
    // follows node-core's fail-closed quarantine path with the same allocation.
    drop(lease);
    assert_eq!(owner.capacities().quarantined, 1);
    let quarantined = owner
        .lease_next_quarantined()
        .expect("exact event remains quarantined");
    assert_eq!(quarantined.id(), event_id);
    let ApplicationEvent::DataReceived { payload, .. } = quarantined.event() else {
        panic!("same DATA event")
    };
    assert_eq!(payload.as_ptr(), payload_pointer);
    let IngressOutcome::Validated(revalidated) = validate_application_event(
        quarantined.event(),
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
    ) else {
        panic!("retained event must revalidate")
    };
    assert_eq!(revalidated.evidence().message_id(), &message_id);
    drop(revalidated);
    quarantined.discard(ApplicationEventDiscardReason::PolicyRejected);
    assert_eq!(owner.capacities().vacant, 1);
    assert_eq!(owner.counters().discarded_events, 1);
}
