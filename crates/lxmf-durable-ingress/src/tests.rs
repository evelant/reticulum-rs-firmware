use super::*;

extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use rand_core::{CryptoRng, RngCore};
use reticulum_lxmf_ingress::{DeferredIngress, LocalDeliveryDestination, StampPolicy, WireLimits};
use reticulum_lxmf_store::{
    BoundLxmfStore, LxmfProgramStage, LxmfStoreBinding, LxmfStoreDeviceId, LxmfStoreIndexSlot,
    PHYSICAL_FORMAT_VERSION, RECORD_FOOTER_SIZE, mount,
};
use reticulum_node_core::{
    ApplicationEvent, ApplicationEventOwner, ApplicationEventQuarantineReason,
    ApplicationEventSlot, ApplicationLinkRole, DelayedProofOwner, DelayedProofReservationError,
    DelayedProofSlot, DelayedProofTransactionError, NodeActions,
};
use reticulum_rns_rete::{
    EmbeddedNode, EmbeddedNodeConfig, InboundProofPolicy, IngressDisposition, InterfaceId, LinkId,
    LinkState, MAX_DATA_PAYLOAD, Packet, RNS_MTU, TxTarget, identity_from_private_key,
};
use serde::Deserialize;
use std::{string::String, vec, vec::Vec};

const CORPUS_JSON: &str = include_str!("../../../interop/vectors/lxmf-1.0.1-v1.json");
const PARTITION_SIZE: usize = 4 * reticulum_lxmf_store::EXTENT_SIZE;
const STORE_OFFSET: usize = 0x73_0000;

#[derive(Deserialize)]
struct Corpus {
    messages: Vec<MessageFixture>,
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

fn named_fixture(name: &str) -> MessageFixture {
    serde_json::from_str::<Corpus>(CORPUS_JSON)
        .expect("checked-in Python LXMF corpus")
        .messages
        .into_iter()
        .find(|fixture| fixture.name == name)
        .expect("named Python LXMF fixture")
}

fn fixture() -> MessageFixture {
    named_fixture("opportunistic_limit_295")
}

fn basic_fixture() -> MessageFixture {
    named_fixture("basic_binary")
}

fn decode(value: &str) -> Vec<u8> {
    hex::decode(value).expect("fixture hex")
}

fn array<const N: usize>(value: &str) -> [u8; N] {
    decode(value).try_into().expect("fixed fixture width")
}

fn event(fixture: &MessageFixture) -> ApplicationEvent {
    assert_eq!(fixture.ingress.carrier_event, "destination_data");
    ApplicationEvent::DataReceived {
        destination: array(&fixture.destination_hash_hex),
        payload: decode(&fixture.ingress.payload_hex),
        ingress: None,
    }
}

fn limits() -> WireLimits {
    WireLimits::new(4_096, 2_048, 256, 2_048, 65_536, 16)
}

fn binding() -> LxmfStoreBinding {
    LxmfStoreBinding::new(
        LxmfStoreDeviceId::new([0x5a; 16]),
        STORE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn store_index<const N: usize>() -> [LxmfStoreIndexSlot; N] {
    core::array::from_fn(|_| LxmfStoreIndexSlot::new())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
    Injected,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::Injected => NorFlashErrorKind::Other,
        }
    }
}

struct FakeNor {
    bytes: Vec<u8>,
    fail_next_write: bool,
    fail_next_read: bool,
    lose_commit_reply: bool,
    reads: usize,
    writes: usize,
    erases: usize,
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
            fail_next_write: false,
            fail_next_read: false,
            lose_commit_reply: false,
            reads: 0,
            writes: 0,
            erases: 0,
        }
    }

    fn fail_next_write(&mut self) {
        self.fail_next_write = true;
    }

    fn lose_commit_reply(&mut self) {
        self.lose_commit_reply = true;
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.reads += 1;
        if core::mem::take(&mut self.fail_next_read) {
            return Err(FakeError::Injected);
        }
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = reticulum_lxmf_store::EXTENT_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.writes += 1;
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        if core::mem::take(&mut self.fail_next_write) {
            return Err(FakeError::Injected);
        }
        let offset = offset as usize;
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
        let commit_offset = RECORD_FOOTER_SIZE - 32;
        if self.lose_commit_reply
            && bytes.len() == RECORD_FOOTER_SIZE
            && bytes[..commit_offset].iter().all(|byte| *byte == 0xff)
            && bytes[commit_offset..].iter().any(|byte| *byte != 0xff)
        {
            self.lose_commit_reply = false;
            self.fail_next_read = true;
            return Err(FakeError::Injected);
        }
        Ok(())
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.erases += 1;
        check_erase(self, from, to).map_err(map_check_error)?;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

fn offer<'owner, 'slots>(
    owner: &'owner mut ApplicationEventOwner<'slots>,
    event: ApplicationEvent,
) -> ApplicationEventLease<'owner, 'slots> {
    owner
        .try_offer_actions(NodeActions::without_retained_proofs(vec![event], vec![], 0))
        .expect("event owner capacity");
    owner.lease_next().expect("offered event lease")
}

fn offer_actions<'owner, 'slots>(
    owner: &'owner mut ApplicationEventOwner<'slots>,
    actions: NodeActions,
) -> ApplicationEventLease<'owner, 'slots> {
    owner
        .try_offer_actions(actions)
        .expect("event owner accepts retained actions");
    owner.lease_next().expect("offered retained event lease")
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

type ProofNode = EmbeddedNode<4, 2, 8, 2>;

fn fixture_identity(first: u8, second: u8) -> reticulum_rns_rete::Identity {
    let mut private_key = [first; 64];
    private_key[32..].fill(second);
    identity_from_private_key(&private_key).expect("fixture identity key is valid")
}

fn proof_bearing_actions(fixture: &MessageFixture) -> NodeActions {
    proof_bearing_actions_on(fixture, InterfaceId(7))
}

fn proof_bearing_actions_on(
    fixture: &MessageFixture,
    source_interface: InterfaceId,
) -> NodeActions {
    let destination_identity = fixture_identity(0x07, 0x08);
    let mut sender = ProofNode::new(
        fixture_identity(0x05, 0x06),
        "lxmf",
        &["delivery"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("fixture sender node is valid");
    sender
        .register_peer(&destination_identity, "lxmf", &["delivery"], 1)
        .expect("fixture receiver is registered");
    let mut receiver = ProofNode::new(
        destination_identity,
        "lxmf",
        &["delivery"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("fixture receiver node is valid");
    receiver.set_inbound_proof_policy(InboundProofPolicy::Retain);
    assert_eq!(
        receiver.destination_hash().as_bytes(),
        &array::<16>(&fixture.destination_hash_hex)
    );

    let payload = decode(&fixture.ingress.payload_hex);
    assert_eq!(payload.len(), 126);
    assert!(payload.len() <= MAX_DATA_PAYLOAD);
    let mut rng = CounterRng::default();
    let mut raw = [0u8; RNS_MTU];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            &payload,
            2,
            &mut rng,
            &mut raw,
        )
        .expect("fixture RNS DATA prepares");
    let report = receiver.ingest(
        &raw[..usize::from(prepared.packet_len())],
        3,
        source_interface,
        &mut rng,
    );
    assert_eq!(report.disposition, IngressDisposition::Processed);
    assert_eq!(report.actions.events.len(), 1);
    assert_eq!(report.actions.retained_proof_count(), 1);
    assert!(report.actions.packets.is_empty());
    report.actions
}

fn link_data_actions(fixture: &MessageFixture) -> NodeActions {
    link_data_actions_for_role(fixture, ApplicationLinkRole::Responder, false)
}

fn fixture_active_link(
    fixture: &MessageFixture,
    retain_responder_proof: bool,
) -> (ProofNode, ProofNode, LinkId, CounterRng) {
    assert_eq!(fixture.ingress.carrier_event, "link_data");
    let destination_identity = fixture_identity(0x07, 0x08);
    let mut sender = ProofNode::new(
        fixture_identity(0x09, 0x0a),
        "lxmf",
        &["sender"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("fixture Link sender node is valid");
    sender
        .register_peer(&destination_identity, "lxmf", &["delivery"], 1)
        .expect("fixture Link receiver is registered");
    let mut receiver = ProofNode::new(
        destination_identity,
        "lxmf",
        &["delivery"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("fixture Link receiver node is valid");
    if retain_responder_proof {
        receiver.set_inbound_proof_policy(InboundProofPolicy::Retain);
    }
    assert_eq!(
        receiver.destination_hash().as_bytes(),
        &array::<16>(&fixture.destination_hash_hex)
    );

    let mut rng = CounterRng::default();
    let (request, link_id) = sender
        .initiate_link(receiver.destination_hash(), 2, &mut rng)
        .expect("fixture Link request");
    let proof = receiver.ingest(request.bytes(), 2, InterfaceId(7), &mut rng);
    assert!(proof.actions.events.is_empty());
    assert_eq!(proof.actions.packets.len(), 1);
    let lrrtt = sender.ingest(
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
    assert_eq!(sender.link_state(&link_id), Some(LinkState::Active));
    assert_eq!(receiver.link_state(&link_id), Some(LinkState::Active));
    (sender, receiver, link_id, rng)
}

fn retained_link_data_over_active_link(
    sender: &mut ProofNode,
    receiver: &mut ProofNode,
    link_id: &LinkId,
    payload: &[u8],
    now: u64,
    rng: &mut CounterRng,
) -> (NodeActions, [u8; 32]) {
    assert_eq!(sender.link_state(link_id), Some(LinkState::Active));
    assert_eq!(receiver.link_state(link_id), Some(LinkState::Active));
    let packet = sender
        .send_link_data(link_id, payload, now, rng)
        .expect("fixture responder-side direct LXMF fits Link MDU");
    let packet_hash = Packet::parse(packet.bytes())
        .expect("fixture Link DATA packet parses")
        .compute_hash();
    let received = receiver.ingest(packet.bytes(), now, InterfaceId(7), rng);
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert_eq!(received.actions.events.len(), 1);
    assert!(received.actions.packets.is_empty());
    assert_eq!(received.actions.retained_proof_count(), 1);
    assert_eq!(sender.link_state(link_id), Some(LinkState::Active));
    assert_eq!(receiver.link_state(link_id), Some(LinkState::Active));
    (received.actions, packet_hash)
}

fn link_data_actions_for_role(
    fixture: &MessageFixture,
    role: ApplicationLinkRole,
    retain_responder_proof: bool,
) -> NodeActions {
    let (mut sender, mut receiver, link_id, mut rng) =
        fixture_active_link(fixture, retain_responder_proof);

    let payload = decode(&fixture.ingress.payload_hex);
    let received = match role {
        ApplicationLinkRole::Initiator => {
            assert!(!retain_responder_proof);
            let packet = receiver
                .send_link_data(&link_id, &payload, 5, &mut rng)
                .expect("fixture initiator-side direct LXMF fits Link MDU");
            sender.ingest(packet.bytes(), 5, InterfaceId(3), &mut rng)
        }
        ApplicationLinkRole::Responder => {
            let packet = sender
                .send_link_data(&link_id, &payload, 5, &mut rng)
                .expect("fixture responder-side direct LXMF fits Link MDU");
            receiver.ingest(packet.bytes(), 5, InterfaceId(7), &mut rng)
        }
    };
    assert_eq!(received.disposition, IngressDisposition::Processed);
    assert_eq!(received.actions.events.len(), 1);
    assert!(received.actions.packets.is_empty());
    assert_eq!(
        received.actions.retained_proof_count(),
        usize::from(retain_responder_proof)
    );
    let ApplicationEvent::LinkData {
        binding,
        data,
        context,
        ..
    } = &received.actions.events[0]
    else {
        panic!("fixture receiver must emit Link DATA")
    };
    assert_eq!(binding.link(), link_id.as_bytes());
    assert_eq!(binding.role(), role);
    assert_eq!(
        binding.destination(),
        &array::<16>(&fixture.destination_hash_hex)
    );
    assert_eq!(*context, reticulum_node_core::APPLICATION_LINK_CONTEXT_NONE);
    assert_eq!(data, &payload);
    received.actions
}

#[allow(clippy::too_many_arguments)]
fn commit_proofless_event<'owner, 'slots, R, A>(
    lease: ApplicationEventLease<'owner, 'slots>,
    local_destination: LocalDeliveryDestination,
    limits: WireLimits,
    source_identities: &R,
    stamp_policy: StampPolicy<'_>,
    store: &mut MountedLxmfStore<'_>,
    access: &mut A,
) -> DurableIngressOutcome<'owner, 'slots, A::Error>
where
    R: SourceIdentityResolver + ?Sized,
    A: BoundLxmfStoreAccess,
{
    let mut proof_slots = [];
    let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);
    commit_application_event(
        lease,
        DurableIngressProofMode::Optional,
        &mut delayed_proofs,
        local_destination,
        limits,
        source_identities,
        stamp_policy,
        store,
        access,
    )
}

#[test]
fn rebind_reports_typed_event_carrier_mismatch_without_revalidating_wire() {
    let fixture = basic_fixture();
    let admitted_event = event(&fixture);
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let IngressOutcome::Validated(validated) = validate_application_event(
        &admitted_event,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
    ) else {
        panic!("basic Python fixture must validate")
    };
    let evidence = validated.evidence();
    let metadata = metadata_from_evidence(evidence, None).expect("validated metadata is portable");
    drop(validated);

    let wrong_destination = ApplicationEvent::DataReceived {
        destination: [0xa5; 16],
        payload: decode(&fixture.ingress.payload_hex),
        ingress: None,
    };
    assert_eq!(
        rebind_candidate(&wrong_destination, evidence, metadata),
        Err(DurableCandidateError::EventCarrierMismatch(
            EventCarrierMismatch::Destination
        ))
    );

    let wrong_length = ApplicationEvent::DataReceived {
        destination: array(&fixture.destination_hash_hex),
        payload: vec![0; evidence.carrier_payload_len() - 1],
        ingress: None,
    };
    assert_eq!(
        rebind_candidate(&wrong_length, evidence, metadata),
        Err(DurableCandidateError::EventCarrierMismatch(
            EventCarrierMismatch::PayloadLength {
                expected: evidence.carrier_payload_len(),
                actual: evidence.carrier_payload_len() - 1,
            }
        ))
    );

    let direct_fixture = named_fixture("opportunistic_over_296");
    let direct_local = LocalDeliveryDestination::new(array(&direct_fixture.destination_hash_hex));
    let direct_source = array::<16>(&direct_fixture.source_hash_hex);
    let direct_public_key = array::<64>(&direct_fixture.source_public_key_hex);
    let direct_resolver =
        |candidate: &[u8; 16]| (candidate == &direct_source).then_some(direct_public_key);
    let mut direct_slots = [ApplicationEventSlot::new()];
    let mut direct_owner = ApplicationEventOwner::new(&mut direct_slots);
    let direct_lease = offer_actions(&mut direct_owner, link_data_actions(&direct_fixture));
    let IngressOutcome::Validated(direct_validated) = validate_application_event(
        direct_lease.event(),
        direct_local,
        limits(),
        &direct_resolver,
        StampPolicy::NotRequired,
    ) else {
        panic!("direct Python fixture must validate")
    };
    let direct_evidence = direct_validated.evidence();
    let direct_metadata = metadata_from_evidence(direct_evidence, None)
        .expect("validated direct metadata is portable");
    drop(direct_validated);
    let direct_event = direct_lease
        .acknowledge()
        .expect("fixture Link DATA has no retained proof");
    let ApplicationEvent::LinkData { binding, data, .. } = direct_event else {
        unreachable!()
    };
    let wrong_context = ApplicationEvent::LinkData {
        binding,
        data,
        context: 0x5a,
        ingress: None,
    };
    assert_eq!(
        rebind_candidate(&wrong_context, direct_evidence, direct_metadata),
        Err(DurableCandidateError::EventCarrierMismatch(
            EventCarrierMismatch::LinkContext {
                expected: reticulum_node_core::APPLICATION_LINK_CONTEXT_NONE,
                actual: 0x5a,
            }
        ))
    );

    let mut initiator_slots = [ApplicationEventSlot::new()];
    let mut initiator_owner = ApplicationEventOwner::new(&mut initiator_slots);
    let initiator_lease = offer_actions(
        &mut initiator_owner,
        link_data_actions_for_role(&direct_fixture, ApplicationLinkRole::Initiator, false),
    );
    let initiator_event = initiator_lease
        .acknowledge()
        .expect("fixture initiator Link DATA has no retained proof");
    assert_eq!(
        rebind_candidate(&initiator_event, direct_evidence, direct_metadata),
        Err(DurableCandidateError::EventCarrierMismatch(
            EventCarrierMismatch::LinkRole {
                expected: ApplicationLinkRole::Responder,
                actual: ApplicationLinkRole::Initiator,
            }
        ))
    );
}

#[test]
fn required_mode_returns_exact_proofless_lease_before_store_io() {
    let fixture = basic_fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<1>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    let io_before = (
        access.backend().reads,
        access.backend().writes,
        access.backend().erases,
    );
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let lease = offer(&mut owner, event(&fixture));
    let event_id = lease.id();
    let event_pointer = match lease.event() {
        ApplicationEvent::DataReceived { payload, .. } => payload.as_ptr(),
        _ => unreachable!(),
    };
    let mut proof_slots = [DelayedProofSlot::new()];
    let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);

    let DurableIngressOutcome::Retained(retained) = commit_application_event(
        lease,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("required mode must not admit a proofless event")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::DelayedProof(DelayedProofTransactionError::ProofNotPresent)
    ));
    assert_eq!(delayed_proofs.counters().reservation_attempts, 0);
    assert_eq!(delayed_proofs.capacities().vacant, 1);
    assert_eq!(
        (
            access.backend().reads,
            access.backend().writes,
            access.backend().erases,
        ),
        io_before
    );
    let lease = retained.into_lease();
    assert_eq!(lease.id(), event_id);
    assert_eq!(
        match lease.event() {
            ApplicationEvent::DataReceived { payload, .. } => payload.as_ptr(),
            _ => unreachable!(),
        },
        event_pointer
    );
    lease.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);
    assert_eq!(store.message_count(), 0);
    assert_eq!(owner.counters().acknowledged_events, 0);
}

#[test]
fn retained_responder_link_exact_replay_over_one_active_link_is_durable_before_each_proof() {
    let fixture = named_fixture("direct_limit_319");
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let (mut sender, mut receiver, link_id, mut rng) = fixture_active_link(&fixture, true);

    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<1>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    let mut event_slots = [ApplicationEventSlot::new()];
    let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
    let mut proof_slots = [DelayedProofSlot::new(), DelayedProofSlot::new()];
    let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);
    let payload = decode(&fixture.ingress.payload_hex);
    let mut ready_proofs = Vec::new();
    let mut first_receipt = None;
    let mut writes_after_new = None;

    for (ordinal, expected_kind) in [
        DurableIngressCommitKind::New,
        DurableIngressCommitKind::Replay,
    ]
    .into_iter()
    .enumerate()
    {
        let now = 5 + ordinal as u64;
        let (mut actions, packet_hash) = retained_link_data_over_active_link(
            &mut sender,
            &mut receiver,
            &link_id,
            &payload,
            now,
            &mut rng,
        );
        actions.attach_ingress_observation(7, Some((-94 + ordinal as i16, 3)));
        let lease = offer_actions(&mut event_owner, actions);
        assert!(lease.has_retained_proof());
        let event_id = lease.id();
        let payload_pointer = match lease.event() {
            ApplicationEvent::LinkData {
                binding,
                data,
                context,
                ..
            } => {
                assert_eq!(binding.link(), link_id.as_bytes());
                assert_eq!(binding.role(), ApplicationLinkRole::Responder);
                assert_eq!(
                    binding.destination(),
                    &array::<16>(&fixture.destination_hash_hex)
                );
                assert_eq!(*context, reticulum_node_core::APPLICATION_LINK_CONTEXT_NONE);
                assert_eq!(data, &payload);
                data.as_ptr()
            }
            _ => unreachable!(),
        };
        let DurableIngressOutcome::Durable(success) = commit_application_event(
            lease,
            DurableIngressProofMode::Required,
            &mut delayed_proofs,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
            &mut store,
            &mut access,
        ) else {
            panic!("proof-bearing responder Link DATA must become durable")
        };
        assert_eq!(success.event_id(), event_id);
        assert_eq!(success.kind(), expected_kind);
        match expected_kind {
            DurableIngressCommitKind::New => {
                first_receipt = Some(success.receipt());
                writes_after_new = Some(access.backend().writes);
            }
            DurableIngressCommitKind::Replay => {
                assert_eq!(success.receipt(), first_receipt.unwrap());
                assert_eq!(
                    access.backend().writes,
                    writes_after_new.unwrap(),
                    "an exact replay must not rewrite committed NOR"
                );
            }
        }
        ready_proofs.push((
            success
                .queued_proof_id()
                .expect("durable responder Link DATA queues its retained proof"),
            packet_hash,
        ));
        let metadata = store
            .metadata(success.receipt().handle())
            .expect("committed direct message metadata");
        assert_eq!(metadata.carrier(), CarrierProvenance::LinkDataContextNone);
        assert_eq!(
            metadata.lengths().carrier_payload() as usize,
            decode(&fixture.ingress.payload_hex).len()
        );
        assert_eq!(
            metadata.ingress_observation(),
            Some(InboundTransportObservation::new(
                InboundInterfaceId::new(7),
                Some(InboundSignalObservation::new(-94, 3)),
            )),
            "the first durable Link carrier remains authoritative"
        );
        assert!(!payload_pointer.is_null());
        assert_eq!(sender.link_state(&link_id), Some(LinkState::Active));
        assert_eq!(receiver.link_state(&link_id), Some(LinkState::Active));
    }

    assert_ne!(ready_proofs[0].0, ready_proofs[1].0);
    assert_ne!(
        ready_proofs[0].1, ready_proofs[1].1,
        "two deliveries over one Link must retain proofs for their own packets"
    );
    assert_eq!(store.message_count(), 1);
    assert_eq!(event_owner.counters().acknowledged_events, 2);
    assert_eq!(delayed_proofs.counters().reservation_attempts, 2);
    assert_eq!(delayed_proofs.capacities().ready, 2);
    for (expected_id, expected_packet_hash) in ready_proofs {
        let proof = delayed_proofs
            .lease_next()
            .expect("each durable responder Link DATA queues one proof");
        assert_eq!(proof.id(), expected_id);
        let actions = proof.release_actions();
        assert_eq!(actions.packets.len(), 1);
        assert_eq!(actions.packets[0].target(), TxTarget::Only(InterfaceId(7)));
        let packet = Packet::parse(actions.packets[0].bytes())
            .expect("released retained proof remains a valid RNS packet");
        assert_eq!(
            packet.payload.get(..32),
            Some(expected_packet_hash.as_slice()),
            "each released proof must cover its own Link DATA packet"
        );
        assert!(actions.events.is_empty());
    }
    assert!(delayed_proofs.lease_next().is_none());
    assert_eq!(sender.link_state(&link_id), Some(LinkState::Active));
    assert_eq!(receiver.link_state(&link_id), Some(LinkState::Active));
}

#[test]
fn retained_proof_is_preclassified_then_capacity_checked_before_store_io() {
    let fixture = basic_fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<1>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    let io_before = (
        access.backend().reads,
        access.backend().writes,
        access.backend().erases,
    );
    let mut event_slots = [ApplicationEventSlot::new()];
    let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
    let lease = offer_actions(&mut event_owner, proof_bearing_actions(&fixture));
    let event_id = lease.id();
    let event_pointer = match lease.event() {
        ApplicationEvent::DataReceived { payload, .. } => payload.as_ptr(),
        _ => unreachable!(),
    };
    assert!(lease.has_retained_proof());
    let mut proof_slots: [DelayedProofSlot; 0] = [];
    let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);

    let DurableIngressOutcome::Retained(unrelated) = commit_application_event(
        lease,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        LocalDeliveryDestination::new([0x99; 16]),
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("classification must run before proof-capacity admission")
    };
    assert!(matches!(
        unrelated.reason(),
        DurableIngressRetentionReason::Unrelated(_)
    ));
    assert_eq!(delayed_proofs.counters().reservation_attempts, 0);
    let lease = unrelated.into_lease();
    assert_eq!(lease.id(), event_id);
    assert!(lease.has_retained_proof());

    let DurableIngressOutcome::Retained(full) = commit_application_event(
        lease,
        DurableIngressProofMode::Optional,
        &mut delayed_proofs,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("optional mode must still reserve every proof that is present")
    };
    assert!(matches!(
        full.reason(),
        DurableIngressRetentionReason::DelayedProof(DelayedProofTransactionError::Reservation(
            DelayedProofReservationError::Full { capacity: 0 }
        ))
    ));
    assert_eq!(delayed_proofs.counters().reservation_attempts, 1);
    assert_eq!(delayed_proofs.counters().full_rejections, 1);
    assert_eq!(
        (
            access.backend().reads,
            access.backend().writes,
            access.backend().erases,
        ),
        io_before
    );
    let lease = full.into_lease();
    assert_eq!(lease.id(), event_id);
    assert!(lease.has_retained_proof());
    assert_eq!(
        match lease.event() {
            ApplicationEvent::DataReceived { payload, .. } => payload.as_ptr(),
            _ => unreachable!(),
        },
        event_pointer
    );
    lease.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);
    assert_eq!(store.message_count(), 0);
    assert_eq!(event_owner.counters().acknowledged_events, 0);
}

#[test]
fn opportunistic_replay_preserves_first_arrival_interface_and_signal() {
    let fixture = basic_fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<1>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    let mut event_slots = [ApplicationEventSlot::new()];
    let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
    let mut proof_slots = [DelayedProofSlot::new(), DelayedProofSlot::new()];
    let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);

    let mut first_actions = proof_bearing_actions_on(&fixture, InterfaceId(7));
    first_actions.attach_ingress_observation(7, Some((-101, -4)));
    let first = offer_actions(&mut event_owner, first_actions);
    let DurableIngressOutcome::Durable(first) = commit_application_event(
        first,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("first observed carrier must commit")
    };
    assert_eq!(first.kind(), DurableIngressCommitKind::New);
    let handle = first.receipt().handle();
    let first_observation = InboundTransportObservation::new(
        InboundInterfaceId::new(7),
        Some(InboundSignalObservation::new(-101, -4)),
    );
    assert_eq!(
        store
            .metadata(handle)
            .expect("committed message metadata")
            .ingress_observation(),
        Some(first_observation)
    );

    let mut replay_actions = proof_bearing_actions_on(&fixture, InterfaceId(9));
    replay_actions.attach_ingress_observation(9, Some((-72, 11)));
    let replay = offer_actions(&mut event_owner, replay_actions);
    let DurableIngressOutcome::Durable(replay) = commit_application_event(
        replay,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("replayed carrier must reconcile")
    };
    assert_eq!(replay.kind(), DurableIngressCommitKind::Replay);
    assert_eq!(replay.receipt().handle(), handle);
    assert_eq!(
        store
            .metadata(handle)
            .expect("first committed metadata remains authoritative")
            .ingress_observation(),
        Some(first_observation)
    );
}

#[test]
fn retained_basic_binary_new_and_replay_each_queue_one_ready_proof() {
    let fixture = basic_fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<2>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    let mut event_slots = [ApplicationEventSlot::new()];
    let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
    let mut proof_slots = [DelayedProofSlot::new(), DelayedProofSlot::new()];
    let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);

    let (first_event_id, first_proof_id) = {
        let lease = offer_actions(&mut event_owner, proof_bearing_actions(&fixture));
        let event_id = lease.id();
        let DurableIngressOutcome::Durable(success) = commit_application_event(
            lease,
            DurableIngressProofMode::Required,
            &mut delayed_proofs,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
            &mut store,
            &mut access,
        ) else {
            panic!("first retained basic_binary event must become durable")
        };
        assert_eq!(success.event_id(), event_id);
        assert_eq!(success.kind(), DurableIngressCommitKind::New);
        let proof_id = success
            .queued_proof_id()
            .expect("new durable event reports its ready proof");
        (event_id, proof_id)
    };
    assert_eq!(delayed_proofs.capacities().ready, 1);
    assert_eq!(store.message_count(), 1);

    let (second_event_id, second_proof_id) = {
        let lease = offer_actions(&mut event_owner, proof_bearing_actions(&fixture));
        let event_id = lease.id();
        let DurableIngressOutcome::Durable(success) = commit_application_event(
            lease,
            DurableIngressProofMode::Required,
            &mut delayed_proofs,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
            &mut store,
            &mut access,
        ) else {
            panic!("replayed retained basic_binary event must reconcile durably")
        };
        assert_eq!(success.event_id(), event_id);
        assert_eq!(success.kind(), DurableIngressCommitKind::Replay);
        let proof_id = success
            .queued_proof_id()
            .expect("durable replay reports its distinct ready proof");
        (event_id, proof_id)
    };
    assert_ne!(first_event_id, second_event_id);
    assert_eq!(delayed_proofs.capacities().ready, 2);
    assert_eq!(delayed_proofs.counters().reservations_committed, 2);
    assert_eq!(store.message_count(), 1);
    assert_eq!(event_owner.counters().acknowledged_events, 2);

    for (expected_event_id, expected_proof_id) in [
        (first_event_id, first_proof_id),
        (second_event_id, second_proof_id),
    ] {
        let proof = delayed_proofs
            .lease_next()
            .expect("each durable event queued one proof");
        assert_eq!(proof.event_id(), expected_event_id);
        assert_eq!(proof.id(), expected_proof_id);
        let actions = proof.release_actions();
        assert_eq!(actions.packets.len(), 1);
        assert_eq!(actions.packets[0].target(), TxTarget::Only(InterfaceId(7)));
        assert!(actions.events.is_empty());
    }
    assert!(delayed_proofs.lease_next().is_none());
}

#[test]
fn reset_remount_and_fresh_retransmission_queue_only_the_fresh_interface_proof() {
    let fixture = basic_fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());

    let writes_after_new = {
        let mut index = store_index::<1>();
        let mut store = mount(&mut access, &mut index).expect("empty store mounts");
        let mut event_slots = [ApplicationEventSlot::new()];
        let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
        let mut proof_slots = [DelayedProofSlot::new()];
        let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);
        let lease = offer_actions(
            &mut event_owner,
            proof_bearing_actions_on(&fixture, InterfaceId(7)),
        );
        let DurableIngressOutcome::Durable(success) = commit_application_event(
            lease,
            DurableIngressProofMode::Required,
            &mut delayed_proofs,
            local,
            limits(),
            &resolver,
            StampPolicy::NotRequired,
            &mut store,
            &mut access,
        ) else {
            panic!("first proof-bearing event must commit")
        };
        assert_eq!(success.kind(), DurableIngressCommitKind::New);
        assert_eq!(delayed_proofs.capacities().ready, 1);
        assert_eq!(store.message_count(), 1);
        // The scope ends without releasing this ready proof, modeling loss of
        // all volatile application/proof ownership during reset.
        access.backend().writes
    };

    let mut remount_index = store_index::<1>();
    let mut remounted = mount(&mut access, &mut remount_index).expect("durable bytes remount");
    assert_eq!(remounted.message_count(), 1);
    assert_eq!(access.backend().writes, writes_after_new);

    let mut fresh_event_slots = [ApplicationEventSlot::new()];
    let mut fresh_event_owner = ApplicationEventOwner::new(&mut fresh_event_slots);
    let mut fresh_proof_slots = [DelayedProofSlot::new()];
    let mut fresh_delayed_proofs = DelayedProofOwner::new(&mut fresh_proof_slots);
    let fresh_lease = offer_actions(
        &mut fresh_event_owner,
        proof_bearing_actions_on(&fixture, InterfaceId(9)),
    );
    let DurableIngressOutcome::Durable(replayed) = commit_application_event(
        fresh_lease,
        DurableIngressProofMode::Required,
        &mut fresh_delayed_proofs,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut remounted,
        &mut access,
    ) else {
        panic!("fresh retransmission after reset must replay durably")
    };
    assert_eq!(replayed.kind(), DurableIngressCommitKind::Replay);
    assert_eq!(remounted.message_count(), 1);
    assert_eq!(access.backend().writes, writes_after_new);
    assert_eq!(fresh_delayed_proofs.capacities().ready, 1);

    let proof = fresh_delayed_proofs
        .lease_next()
        .expect("fresh retransmission creates one fresh proof");
    let actions = proof.release_actions();
    assert_eq!(actions.packets.len(), 1);
    assert_eq!(actions.packets[0].target(), TxTarget::Only(InterfaceId(9)));
    assert!(fresh_delayed_proofs.lease_next().is_none());
}

#[test]
fn missing_source_identity_exact_retry_succeeds_after_identity_is_learned() {
    let fixture = basic_fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let missing = |_candidate: &[u8; 16]| None;
    let learned = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<1>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    let mut event_slots = [ApplicationEventSlot::new()];
    let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
    let mut proof_slots = [DelayedProofSlot::new()];
    let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);
    let lease = offer_actions(
        &mut event_owner,
        proof_bearing_actions_on(&fixture, InterfaceId(11)),
    );

    let DurableIngressOutcome::Retained(retained) = commit_application_event(
        lease,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        local,
        limits(),
        &missing,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("unknown source identity must defer the exact event")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::Deferred(
            DeferredIngress::SourceIdentityUnavailable { source: missing_source }
        ) if *missing_source == source
    ));
    assert_eq!(store.message_count(), 0);
    assert_eq!(delayed_proofs.capacities().ready, 0);

    let token = retained
        .into_lease()
        .quarantine_for_retry(ApplicationEventQuarantineReason::ConsumerDeferred);
    let exact = event_owner
        .try_reacquire_quarantined(token)
        .expect("learned identity retries the exact opaque owner");
    let DurableIngressOutcome::Durable(success) = commit_application_event(
        exact,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        local,
        limits(),
        &learned,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("learned source identity must admit the retained message")
    };
    assert_eq!(success.kind(), DurableIngressCommitKind::New);
    assert_eq!(store.message_count(), 1);
    assert_eq!(delayed_proofs.capacities().ready, 1);
    let actions = delayed_proofs
        .lease_next()
        .expect("durability releases the retained proof")
        .release_actions();
    assert_eq!(actions.packets[0].target(), TxTarget::Only(InterfaceId(11)));
}

#[test]
fn retained_lost_commit_reply_retry_queues_exactly_one_ready_proof() {
    let fixture = basic_fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<1>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    access.backend_mut().lose_commit_reply();
    let mut event_slots = [ApplicationEventSlot::new()];
    let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
    let lease = offer_actions(&mut event_owner, proof_bearing_actions(&fixture));
    let event_id = lease.id();
    let event_sequence = lease.sequence();
    let event_pointer = match lease.event() {
        ApplicationEvent::DataReceived { payload, .. } => payload.as_ptr(),
        _ => unreachable!(),
    };
    let mut proof_slots = [DelayedProofSlot::new()];
    let mut delayed_proofs = DelayedProofOwner::new(&mut proof_slots);

    let DurableIngressOutcome::Retained(retained) = commit_application_event(
        lease,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("lost terminal write reply must return the proof-bearing lease")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::Backend {
            stage: LxmfProgramStage::Commit,
            error: FakeError::Injected,
        })
    ));
    let pending_message = store
        .pending_message_id()
        .expect("lost commit reply retains exact pending mutation identity");
    assert_eq!(delayed_proofs.capacities().vacant, 1);
    assert_eq!(delayed_proofs.capacities().ready, 0);
    assert_eq!(delayed_proofs.counters().reservations_created, 1);
    assert_eq!(delayed_proofs.counters().reservations_released, 1);
    let mut remount_index = store_index::<1>();
    assert_eq!(
        mount(&mut access, &mut remount_index)
            .expect("terminal write reached physical media")
            .message_count(),
        1
    );
    let lease = retained.into_lease();
    assert_eq!(lease.id(), event_id);
    assert_eq!(lease.sequence(), event_sequence);
    assert!(lease.has_retained_proof());
    assert_eq!(
        match lease.event() {
            ApplicationEvent::DataReceived { payload, .. } => payload.as_ptr(),
            _ => unreachable!(),
        },
        event_pointer
    );

    let missing = |_candidate: &[u8; 16]| None;
    let DurableIngressOutcome::Retained(deferred) = commit_application_event(
        lease,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        local,
        limits(),
        &missing,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("identity loss before exact retry must retain the pending owner")
    };
    assert!(matches!(
        deferred.reason(),
        DurableIngressRetentionReason::Deferred(
            DeferredIngress::SourceIdentityUnavailable { source: missing_source }
        ) if *missing_source == source
    ));
    assert_eq!(store.pending_message_id(), Some(pending_message));
    assert_eq!(delayed_proofs.capacities().ready, 0);
    let lease = deferred.into_lease();
    assert_eq!(lease.id(), event_id);
    assert_eq!(lease.sequence(), event_sequence);
    assert!(lease.has_retained_proof());

    let DurableIngressOutcome::Durable(success) = commit_application_event(
        lease,
        DurableIngressProofMode::Required,
        &mut delayed_proofs,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("exact retry must reconcile and queue its retained proof")
    };
    assert_eq!(success.event_id(), event_id);
    assert_eq!(success.kind(), DurableIngressCommitKind::Replay);
    assert!(success.queued_proof_id().is_some());
    assert_eq!(delayed_proofs.capacities().ready, 1);
    assert_eq!(delayed_proofs.counters().reservations_created, 2);
    assert_eq!(delayed_proofs.counters().reservations_released, 1);
    assert_eq!(delayed_proofs.counters().reservations_committed, 1);
    assert_eq!(event_owner.counters().acknowledged_events, 1);
    assert_eq!(store.message_count(), 1);

    let proof = delayed_proofs
        .lease_next()
        .expect("one reconciled proof is ready");
    assert_eq!(proof.id(), success.queued_proof_id().unwrap());
    assert_eq!(proof.event_id(), event_id);
    let actions = proof.release_actions();
    assert_eq!(actions.packets.len(), 1);
    assert_eq!(actions.packets[0].target(), TxTarget::Only(InterfaceId(7)));
    assert!(delayed_proofs.lease_next().is_none());
}

#[test]
fn python_maximum_opportunistic_event_commits_before_acknowledgement_and_replays() {
    let fixture = fixture();
    let first_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &first_event else {
        unreachable!()
    };
    assert_eq!(payload.len(), 391);
    assert!(payload.len() > 383);

    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<2>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);

    let first = match commit_proofless_event(
        offer(&mut owner, first_event),
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) {
        DurableIngressOutcome::Durable(success) => success,
        DurableIngressOutcome::Retained(retained) => {
            retained
                .into_lease()
                .quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            panic!("valid Python event must commit")
        }
    };
    assert_eq!(first.kind(), DurableIngressCommitKind::New);
    assert_eq!(first.queued_proof_id(), None);
    assert_eq!(store.message_count(), 1);
    assert_eq!(owner.counters().acknowledged_events, 1);
    assert_eq!(
        store.receipt(first.receipt().handle()),
        Some(first.receipt())
    );

    let replay = match commit_proofless_event(
        offer(&mut owner, event(&fixture)),
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) {
        DurableIngressOutcome::Durable(success) => success,
        DurableIngressOutcome::Retained(retained) => {
            retained
                .into_lease()
                .quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            panic!("duplicate event must resolve durably")
        }
    };
    assert_eq!(replay.kind(), DurableIngressCommitKind::Replay);
    assert_eq!(replay.queued_proof_id(), None);
    assert_eq!(replay.receipt(), first.receipt());
    assert_eq!(store.message_count(), 1);
    assert_eq!(owner.counters().acknowledged_events, 2);
}

#[test]
fn unrelated_deferred_and_rejected_outcomes_return_the_exact_lease() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let missing = |_candidate: &[u8; 16]| None::<[u8; 64]>;
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<2>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);

    let unrelated_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &unrelated_event else {
        unreachable!()
    };
    let unrelated_pointer = payload.as_ptr();
    let unrelated_lease = offer(&mut owner, unrelated_event);
    let unrelated_id = unrelated_lease.id();
    let unrelated_sequence = unrelated_lease.sequence();
    let unrelated_quarantine = unrelated_lease.quarantine_reason();
    let DurableIngressOutcome::Retained(unrelated) = commit_proofless_event(
        unrelated_lease,
        LocalDeliveryDestination::new([0x99; 16]),
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("other destination must be retained")
    };
    assert_eq!(unrelated.event_id(), unrelated_id);
    assert!(matches!(
        unrelated.reason(),
        DurableIngressRetentionReason::Unrelated(_)
    ));
    let unrelated = unrelated.into_lease();
    assert_eq!(unrelated.id(), unrelated_id);
    assert_eq!(unrelated.sequence(), unrelated_sequence);
    assert_eq!(unrelated.quarantine_reason(), unrelated_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = unrelated.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), unrelated_pointer);
    unrelated.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);

    let deferred_lease = owner.lease_next_quarantined().expect("same retained event");
    let deferred_id = deferred_lease.id();
    let deferred_sequence = deferred_lease.sequence();
    let deferred_quarantine = deferred_lease.quarantine_reason();
    assert_eq!(deferred_id, unrelated_id);
    assert_eq!(deferred_sequence, unrelated_sequence);
    assert_eq!(
        deferred_quarantine,
        Some(ApplicationEventQuarantineReason::ConsumerDeferred)
    );
    let DurableIngressOutcome::Retained(deferred) = commit_proofless_event(
        deferred_lease,
        local,
        limits(),
        &missing,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("missing identity must be retained")
    };
    assert_eq!(deferred.event_id(), deferred_id);
    assert!(matches!(
        deferred.reason(),
        DurableIngressRetentionReason::Deferred(_)
    ));
    let deferred = deferred.into_lease();
    assert_eq!(deferred.id(), deferred_id);
    assert_eq!(deferred.sequence(), deferred_sequence);
    assert_eq!(deferred.quarantine_reason(), deferred_quarantine);
    deferred.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);

    let rejected_lease = owner.lease_next_quarantined().expect("same retained event");
    let rejected_id = rejected_lease.id();
    let rejected_sequence = rejected_lease.sequence();
    let rejected_quarantine = rejected_lease.quarantine_reason();
    assert_eq!(rejected_id, unrelated_id);
    assert_eq!(rejected_sequence, unrelated_sequence);
    assert_eq!(rejected_quarantine, deferred_quarantine);
    let DurableIngressOutcome::Retained(rejected) = commit_proofless_event(
        rejected_lease,
        local,
        WireLimits::new(100, 100, 10, 10, 100, 4),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("over-limit wire must be retained after rejection")
    };
    assert_eq!(rejected.event_id(), rejected_id);
    assert!(matches!(
        rejected.reason(),
        DurableIngressRetentionReason::Rejected(_)
    ));
    let rejected = rejected.into_lease();
    assert_eq!(rejected.id(), rejected_id);
    assert_eq!(rejected.sequence(), rejected_sequence);
    assert_eq!(rejected.quarantine_reason(), rejected_quarantine);
    rejected.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);

    assert_eq!(owner.counters().acknowledged_events, 0);
    assert_eq!(owner.capacities().quarantined, 1);
    assert_eq!(store.message_count(), 0);
}

#[test]
fn store_capacity_and_backend_failures_return_the_exact_unacknowledged_lease() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);

    let mut zero_access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut zero_index = store_index::<0>();
    let mut zero_store =
        mount(&mut zero_access, &mut zero_index).expect("zero-entry store mounts empty");
    let mut zero_slots = [ApplicationEventSlot::new()];
    let mut zero_owner = ApplicationEventOwner::new(&mut zero_slots);
    let blocked_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &blocked_event else {
        unreachable!()
    };
    let blocked_pointer = payload.as_ptr();
    let blocked_lease = offer(&mut zero_owner, blocked_event);
    let blocked_id = blocked_lease.id();
    let blocked_sequence = blocked_lease.sequence();
    let blocked_quarantine = blocked_lease.quarantine_reason();
    let DurableIngressOutcome::Retained(blocked) = commit_proofless_event(
        blocked_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut zero_store,
        &mut zero_access,
    ) else {
        panic!("zero semantic capacity must retain the event")
    };
    assert_eq!(blocked.event_id(), blocked_id);
    assert!(matches!(
        blocked.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::IndexFull { capacity: 0 })
    ));
    let blocked = blocked.into_lease();
    assert_eq!(blocked.id(), blocked_id);
    assert_eq!(blocked.sequence(), blocked_sequence);
    assert_eq!(blocked.quarantine_reason(), blocked_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = blocked.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), blocked_pointer);
    blocked.quarantine(ApplicationEventQuarantineReason::ConsumerDeferred);
    assert_eq!(zero_owner.counters().acknowledged_events, 0);

    let mut failed_access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut failed_index = store_index::<1>();
    let mut failed_store =
        mount(&mut failed_access, &mut failed_index).expect("empty store mounts");
    failed_access.backend_mut().fail_next_write();
    let mut failed_slots = [ApplicationEventSlot::new()];
    let mut failed_owner = ApplicationEventOwner::new(&mut failed_slots);
    let failed_lease = offer(&mut failed_owner, event(&fixture));
    let failed_id = failed_lease.id();
    let failed_sequence = failed_lease.sequence();
    let failed_quarantine = failed_lease.quarantine_reason();
    let DurableIngressOutcome::Retained(failed) = commit_proofless_event(
        failed_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut failed_store,
        &mut failed_access,
    ) else {
        panic!("backend fault must retain the event")
    };
    assert_eq!(failed.event_id(), failed_id);
    assert!(matches!(
        failed.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::Backend {
            error: FakeError::Injected,
            ..
        })
    ));
    let failed = failed.into_lease();
    assert_eq!(failed.id(), failed_id);
    assert_eq!(failed.sequence(), failed_sequence);
    assert_eq!(failed.quarantine_reason(), failed_quarantine);
    failed.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
    assert_eq!(failed_owner.counters().acknowledged_events, 0);
}

#[test]
fn commit_marker_lost_success_retains_exact_lease_until_same_lease_retry_is_durable() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<1>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");
    access.backend_mut().lose_commit_reply();
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let retained_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &retained_event else {
        unreachable!()
    };
    let retained_pointer = payload.as_ptr();
    let retained_lease = offer(&mut owner, retained_event);
    let retained_id = retained_lease.id();
    let retained_sequence = retained_lease.sequence();
    let retained_quarantine = retained_lease.quarantine_reason();

    let DurableIngressOutcome::Retained(retained) = commit_proofless_event(
        retained_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("lost commit reply must retain the event until reconciliation")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::Backend {
            stage: LxmfProgramStage::Commit,
            error: FakeError::Injected,
        })
    ));
    assert_eq!(store.message_count(), 0);
    let mut remount_index = store_index::<1>();
    assert_eq!(
        mount(&mut access, &mut remount_index)
            .expect("the lost reply followed physical commit")
            .message_count(),
        1
    );
    let retained = retained.into_lease();
    assert_eq!(retained.id(), retained_id);
    assert_eq!(retained.sequence(), retained_sequence);
    assert_eq!(retained.quarantine_reason(), retained_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = retained.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), retained_pointer);

    let DurableIngressOutcome::Durable(reconciled) = commit_proofless_event(
        retained,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("the same exact lease must reconcile its committed mutation")
    };
    assert_eq!(reconciled.event_id(), retained_id);
    assert_eq!(reconciled.kind(), DurableIngressCommitKind::Replay);
    assert_eq!(store.message_count(), 1);
    assert_eq!(owner.counters().acknowledged_events, 1);
}

#[test]
fn wrong_binding_returns_exact_lease_without_touching_the_supplied_backend() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut mounted_access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<1>();
    let mut store = mount(&mut mounted_access, &mut index).expect("empty store mounts");
    let mut wrong_access = BoundLxmfStore::new(
        FakeNor::erased(),
        LxmfStoreBinding::new(
            LxmfStoreDeviceId::new([0xa5; 16]),
            STORE_OFFSET,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
    );
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let retained_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &retained_event else {
        unreachable!()
    };
    let retained_pointer = payload.as_ptr();
    let retained_lease = offer(&mut owner, retained_event);
    let retained_id = retained_lease.id();
    let retained_sequence = retained_lease.sequence();
    let retained_quarantine = retained_lease.quarantine_reason();

    let DurableIngressOutcome::Retained(retained) = commit_proofless_event(
        retained_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut wrong_access,
    ) else {
        panic!("wrong operation binding must retain the event")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::Binding(_))
    ));
    assert_eq!(wrong_access.backend().reads, 0);
    assert_eq!(wrong_access.backend().writes, 0);
    assert_eq!(wrong_access.backend().erases, 0);
    let retained = retained.into_lease();
    assert_eq!(retained.id(), retained_id);
    assert_eq!(retained.sequence(), retained_sequence);
    assert_eq!(retained.quarantine_reason(), retained_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = retained.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), retained_pointer);
    retained.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
    assert_eq!(owner.counters().acknowledged_events, 0);
    assert_eq!(store.message_count(), 0);
}

#[test]
fn same_id_different_authenticated_material_collision_returns_exact_lease() {
    let fixture = fixture();
    let local = LocalDeliveryDestination::new(array(&fixture.destination_hash_hex));
    let source = array::<16>(&fixture.source_hash_hex);
    let public_key = array::<64>(&fixture.source_public_key_hex);
    let resolver = |candidate: &[u8; 16]| (candidate == &source).then_some(public_key);
    let mut access = BoundLxmfStore::new(FakeNor::erased(), binding());
    let mut index = store_index::<2>();
    let mut store = mount(&mut access, &mut index).expect("empty store mounts");

    let seed_event = event(&fixture);
    let IngressOutcome::Validated(validated) = validate_application_event(
        &seed_event,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
    ) else {
        panic!("seed fixture must validate")
    };
    let evidence = validated.evidence();
    let authentic_metadata = metadata_from_evidence(evidence, None).expect("validated metadata");
    let conflicting_material = AuthenticatedMaterialFingerprint::new([0xa5; 32]);
    assert_ne!(
        conflicting_material,
        authentic_metadata.authenticated_material()
    );
    let conflicting_metadata = InboundMessageMetadata::new(
        authentic_metadata.message_id(),
        conflicting_material,
        authentic_metadata.destination(),
        authentic_metadata.source(),
        authentic_metadata.timestamp_bits(),
        authentic_metadata.carrier(),
        authentic_metadata.stamp_admission(),
        authentic_metadata.lengths(),
    )
    .expect("only authenticated material differs");
    let conflicting_candidate = InboundMessageCandidate::new(
        conflicting_metadata,
        NormalizedWire::Opportunistic {
            implied_destination: evidence.destination(),
            carrier_payload: validated.carrier_payload(),
        },
    )
    .expect("borrowed fixture matches the conflicting semantic candidate");
    assert!(matches!(
        store.commit(&mut access, conflicting_candidate),
        Ok(LxmfCommitOutcome::Committed(_))
    ));
    drop(validated);
    let writes_before_collision = access.backend().writes;

    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let retained_event = event(&fixture);
    let ApplicationEvent::DataReceived { payload, .. } = &retained_event else {
        unreachable!()
    };
    let retained_pointer = payload.as_ptr();
    let retained_lease = offer(&mut owner, retained_event);
    let retained_id = retained_lease.id();
    let retained_sequence = retained_lease.sequence();
    let retained_quarantine = retained_lease.quarantine_reason();

    let DurableIngressOutcome::Retained(retained) = commit_proofless_event(
        retained_lease,
        local,
        limits(),
        &resolver,
        StampPolicy::NotRequired,
        &mut store,
        &mut access,
    ) else {
        panic!("same ID with different authenticated material must fail closed")
    };
    assert!(matches!(
        retained.reason(),
        DurableIngressRetentionReason::Store(LxmfCommitError::HashCollision { message_id })
            if *message_id == authentic_metadata.message_id()
    ));
    assert_eq!(access.backend().writes, writes_before_collision);
    let retained = retained.into_lease();
    assert_eq!(retained.id(), retained_id);
    assert_eq!(retained.sequence(), retained_sequence);
    assert_eq!(retained.quarantine_reason(), retained_quarantine);
    let ApplicationEvent::DataReceived { payload, .. } = retained.event() else {
        unreachable!()
    };
    assert_eq!(payload.as_ptr(), retained_pointer);
    retained.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
    assert_eq!(owner.counters().acknowledged_events, 0);
    assert_eq!(store.message_count(), 1);
}
