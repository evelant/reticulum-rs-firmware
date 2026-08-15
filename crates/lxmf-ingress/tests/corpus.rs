use core::cell::Cell;

use rand_core::{CryptoRng, RngCore};
use reticulum_lxmf_ingress::{
    DeferredIngress, IngressOutcome, LocalDeliveryDestination, RejectedIngress,
    SourceIdentityResolver, UnrelatedEvent, UnsupportedCarrier, validate_application_event,
};
use reticulum_lxmf_wire::{
    CarrierKind, PowBudget, RequiredStampCost, StampAdmission, StampError, StampPolicy, WireError,
    WireLimits,
};
use reticulum_node_core::{
    ApplicationEvent, ApplicationEventDiscardReason, ApplicationEventOwner, ApplicationEventSlot,
    ApplicationLinkRole, NodeActions,
};
use reticulum_rns_rete::{
    EmbeddedNode, EmbeddedNodeConfig, InterfaceId, identity_from_private_key,
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
        ingress: None,
    }
}

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

type LinkNode = EmbeddedNode<4, 2, 8, 2>;

fn identity(first: u8, second: u8) -> reticulum_rns_rete::Identity {
    let mut private_key = [first; 64];
    private_key[32..].fill(second);
    identity_from_private_key(&private_key).expect("fixture identity key is valid")
}

fn linked_event_for_role(
    payload: Vec<u8>,
    receiver_first: u8,
    receiver_second: u8,
    role: ApplicationLinkRole,
) -> ApplicationEvent {
    let receiver_identity = identity(receiver_first, receiver_second);
    let mut initiator = LinkNode::new(
        identity(0x09, 0x0a),
        "lxmf",
        &["sender"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("fixture initiator node");
    let mut receiver = LinkNode::new(
        identity(receiver_first, receiver_second),
        "lxmf",
        &["delivery"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("fixture receiver node");
    initiator
        .register_peer(&receiver_identity, "lxmf", &["delivery"], 1)
        .expect("fixture receiver registration");

    let mut rng = CounterRng::default();
    let (request, link_id) = initiator
        .initiate_link(receiver.destination_hash(), 2, &mut rng)
        .expect("fixture Link request");
    let proof = receiver.ingest(request.bytes(), 2, InterfaceId(7), &mut rng);
    assert!(proof.actions.events.is_empty());
    assert_eq!(proof.actions.packets.len(), 1);
    let lrrtt = initiator.ingest(
        proof.actions.packets[0].bytes(),
        3,
        InterfaceId(3),
        &mut rng,
    );
    assert_eq!(lrrtt.actions.packets.len(), 1);
    let active = receiver.ingest(
        lrrtt.actions.packets[0].bytes(),
        4,
        InterfaceId(7),
        &mut rng,
    );
    assert_eq!(active.actions.events.len(), 1);
    assert!(active.actions.packets.is_empty());

    let received = match role {
        ApplicationLinkRole::Initiator => {
            let packet = receiver
                .send_link_data(&link_id, &payload, 5, &mut rng)
                .expect("fixture responder Link DATA");
            initiator.ingest(packet.bytes(), 5, InterfaceId(3), &mut rng)
        }
        ApplicationLinkRole::Responder => {
            let packet = initiator
                .send_link_data(&link_id, &payload, 5, &mut rng)
                .expect("fixture initiator Link DATA");
            receiver.ingest(packet.bytes(), 5, InterfaceId(7), &mut rng)
        }
    };
    assert!(received.actions.packets.is_empty());
    assert_eq!(received.actions.events.len(), 1);
    assert!(matches!(
        &received.actions.events[0],
        ApplicationEvent::LinkData { binding, .. } if binding.role() == role
    ));

    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    owner
        .try_offer_actions(received.actions)
        .expect("fixture event owner admission");
    owner
        .lease_next()
        .expect("fixture Link DATA lease")
        .acknowledge()
        .expect("proofless Link DATA acknowledgement")
}

fn linked_event_for_receiver(
    payload: Vec<u8>,
    receiver_first: u8,
    receiver_second: u8,
) -> ApplicationEvent {
    linked_event_for_role(
        payload,
        receiver_first,
        receiver_second,
        ApplicationLinkRole::Responder,
    )
}

fn linked_event(fixture: &MessageFixture) -> ApplicationEvent {
    assert_eq!(fixture.ingress.carrier_event, "link_data");
    let event = linked_event_for_receiver(decode(&fixture.ingress.payload_hex), 0x07, 0x08);
    let ApplicationEvent::LinkData { binding, .. } = &event else {
        unreachable!()
    };
    assert_eq!(
        binding.destination(),
        &array::<16>(&fixture.destination_hash_hex)
    );
    assert_eq!(binding.role(), ApplicationLinkRole::Responder);
    event
}

fn with_link_context(event: ApplicationEvent, context: u8) -> ApplicationEvent {
    match event {
        ApplicationEvent::LinkData {
            binding,
            data,
            ingress,
            ..
        } => ApplicationEvent::LinkData {
            binding,
            data,
            context,
            ingress,
        },
        _ => panic!("fixture event must be Link DATA"),
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
fn destination_and_noncarrier_events_are_classified_without_identity_lookup() {
    let corpus = corpus();
    let base_fixture = fixture(&corpus, "basic_binary");
    let event = opportunistic_event(base_fixture);
    let resolver = fixture_resolver(base_fixture);
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
        }) if expected == other_local && actual == array::<16>(&base_fixture.destination_hash_hex)
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

    let direct_fixture = fixture(&corpus, "opportunistic_over_296");
    let other_context = with_link_context(linked_event(direct_fixture), 0x5a);
    let ApplicationEvent::LinkData { binding, data, .. } = &other_context else {
        unreachable!()
    };
    let expected_link = *binding.link();
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
        }) if link == expected_link
    ));
    let ApplicationEvent::LinkData { data, .. } = &other_context else {
        unreachable!()
    };
    assert_eq!(data.as_ptr(), pointer);
    assert_eq!(resolver.calls.get(), 0);
}

#[test]
fn direct_link_carriers_are_bound_validated_and_zero_copy() {
    let corpus = corpus();
    for name in ["opportunistic_over_296", "direct_limit_319"] {
        let fixture = fixture(&corpus, name);
        let event = linked_event(fixture);
        let ApplicationEvent::LinkData { binding, data, .. } = &event else {
            unreachable!()
        };
        let expected_pointer = data.as_ptr();
        let expected_length = data.len();
        let resolver = fixture_resolver(fixture);
        let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
        let IngressOutcome::Validated(validated) = validate_application_event(
            &event,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ) else {
            panic!("{name} direct Link DATA must validate")
        };
        assert_eq!(binding.destination(), local.as_bytes());
        assert_eq!(validated.carrier_payload().as_ptr(), expected_pointer);
        assert_eq!(validated.carrier_payload().len(), expected_length);
        assert_eq!(
            validated.evidence().carrier(),
            CarrierKind::LinkDataContextNone
        );
        assert_eq!(validated.evidence().normalized_wire_len(), expected_length);
    }
}

#[test]
fn link_destination_mismatch_is_unrelated_before_identity_lookup() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "opportunistic_over_296");
    let event = linked_event_for_receiver(decode(&fixture.ingress.payload_hex), 0x11, 0x12);
    let ApplicationEvent::LinkData { binding, .. } = &event else {
        unreachable!()
    };
    let actual = *binding.destination();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let resolver = fixture_resolver(fixture);
    assert!(matches!(
        validate_application_event(
            &event,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
        ),
        IngressOutcome::Unrelated(UnrelatedEvent::OtherDestination {
            expected,
            actual: observed,
        }) if expected == local && observed == actual
    ));
    assert_eq!(resolver.calls.get(), 0);
}

#[test]
fn initiator_link_data_is_unrelated_before_identity_lookup() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "opportunistic_over_296");
    let event = linked_event_for_role(
        decode(&fixture.ingress.payload_hex),
        0x07,
        0x08,
        ApplicationLinkRole::Initiator,
    );
    let ApplicationEvent::LinkData { binding, .. } = &event else {
        unreachable!()
    };
    let link = *binding.link();
    let local = LocalDeliveryDestination::new(*binding.destination());
    let resolver = fixture_resolver(fixture);

    assert!(matches!(
        validate_application_event(&event, local, limits(), &resolver, StampPolicy::NotRequired,),
        IngressOutcome::Unrelated(UnrelatedEvent::OtherLinkRole {
            link: observed,
            role: ApplicationLinkRole::Initiator,
        }) if observed == link
    ));
    assert_eq!(resolver.calls.get(), 0);
}

#[test]
fn complete_wire_destination_mismatch_is_rejected_before_identity_lookup() {
    let corpus = corpus();
    let fixture = fixture(&corpus, "opportunistic_over_296");
    let event = linked_event_for_receiver(decode(&fixture.ingress.payload_hex), 0x11, 0x12);
    let ApplicationEvent::LinkData { binding, .. } = &event else {
        unreachable!()
    };
    let local = LocalDeliveryDestination::new(*binding.destination());
    assert_ne!(
        local.as_bytes(),
        &array::<16>(&fixture.destination_hash_hex)
    );
    let resolver = fixture_resolver(fixture);

    assert!(matches!(
        validate_application_event(&event, local, limits(), &resolver, StampPolicy::NotRequired,),
        IngressOutcome::Rejected(RejectedIngress::Wire(WireError::DestinationMismatch))
    ));
    assert_eq!(resolver.calls.get(), 0);
}

#[test]
fn resource_carrier_remains_explicitly_deferred_unchanged() {
    let resolver = |_source: &[u8; 16]| Some([0_u8; 64]);
    let local = LocalDeliveryDestination::new([0x11; 16]);
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
        ingress: None,
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
        ingress: None,
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
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let event = linked_event(fixture);
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
    let ticket_event = linked_event(ticket_fixture);
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
    let pow_event = linked_event(pow_fixture);
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
    let invalid_event = linked_event_for_receiver(invalid_wire, 0x07, 0x08);
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
