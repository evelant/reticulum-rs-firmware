extern crate std;

use super::*;
use crate::{
    InboundProofPolicy, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId, NodeRole,
    TxPacketBuffer,
};
use rand_core::{CryptoRng, RngCore};
use reticulum_rns_rete::{
    ApplicationEvent, EmbeddedNode, EmbeddedNodeConfig, InterfaceId as RnsInterfaceId,
    TxTarget as RnsTxTarget,
};

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

type TestNode = NodeCore<4, 4, 8, 2, 1>;
type RnsNode = EmbeddedNode<4, 4, 8, 2>;

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).unwrap()
}

fn rns_identity(tag: u8) -> reticulum_rns_rete::Identity {
    reticulum_rns_rete::identity_from_private_key(&[tag; 64]).unwrap()
}

fn node(tag: u8, role: NodeRole) -> TestNode {
    let config = NodeConfig {
        role,
        max_additional_destinations: 4,
    };
    TestNode::new(
        identity(tag),
        "reticulum",
        &["ordinary-actions"],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        config,
    )
    .unwrap()
}

fn rns_node(tag: u8, config: EmbeddedNodeConfig) -> RnsNode {
    RnsNode::new(
        rns_identity(tag),
        "reticulum",
        &["ordinary-actions"],
        config,
    )
    .unwrap()
}

const fn owner_time(milliseconds: u64) -> MonotonicMillis {
    MonotonicMillis::new(milliseconds)
}

const fn deadline(milliseconds: u64) -> TxLeaseDeadline {
    TxLeaseDeadline::new(owner_time(milliseconds))
}

const fn interfaces(bits: u64) -> InterfaceSet {
    InterfaceSet::from_bits(bits)
}

const TEST_PERMIT_RESOURCE: crate::TxPermitResourceId = crate::TxPermitResourceId::new([0x4f; 16]);

fn test_permit_requirements() -> TxPermitRequirements {
    TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1).unwrap()
}

fn test_permit_reservation() -> TxPermitReservation {
    TxPermitReservation::try_new(TEST_PERMIT_RESOURCE, 1).unwrap()
}

const fn request(
    now: u64,
    deadline_ms: u64,
    enabled: InterfaceSet,
) -> OrdinaryActionAdmissionRequest {
    OrdinaryActionAdmissionRequest {
        owner_now: owner_time(now),
        deadline: deadline(deadline_ms),
        enabled_interfaces: enabled,
    }
}

fn complete_unpermitted<'a, const BUFFERS: usize>(
    owner: &mut OrdinaryActionOwner<BUFFERS>,
    job: OrdinaryTxJob<'a>,
    now: MonotonicMillis,
) -> Result<OrdinaryCompletionDisposition<'a>, OrdinaryCompletionFailure<'a>> {
    owner.complete_tx(
        job.return_unpermitted()
            .complete(TxCompletionCode::new(0x554e)),
        now,
    )
}

struct RecordingPolicy {
    decision: TxPolicyDecision,
    calls: usize,
    candidate: Option<TxAuthorizationCandidate>,
}

impl RecordingPolicy {
    const fn authorizing(reservation: TxPermitReservation) -> Self {
        Self {
            decision: TxPolicyDecision::Authorize(reservation),
            calls: 0,
            candidate: None,
        }
    }

    const fn denying(reason: TxPolicyDenial) -> Self {
        Self {
            decision: TxPolicyDecision::Deny(reason),
            calls: 0,
            candidate: None,
        }
    }
}

impl TxAuthorizationPolicy for RecordingPolicy {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        self.calls += 1;
        self.candidate = Some(candidate);
        self.decision
    }
}

fn announces(count: usize) -> NodeActions {
    let mut node = rns_node(40, EmbeddedNodeConfig::endpoint());
    let mut rng = CounterRng::default();
    for index in 0..count {
        node.queue_announce(Some(&[index as u8]), 1, &mut rng)
            .unwrap();
    }
    NodeActions::without_retained_proofs(std::vec::Vec::new(), node.flush_announces(1, &mut rng), 0)
}

fn resolved_routing_actions() -> (NodeActions, NodeActions) {
    let mut rng = CounterRng::default();

    let mut sender = rns_node(41, EmbeddedNodeConfig::endpoint());
    let mut receiver = rns_node(42, EmbeddedNodeConfig::endpoint());
    receiver.set_inbound_proof_policy(reticulum_rns_rete::InboundProofPolicy::Always);
    receiver.queue_announce(None, 1, &mut rng).unwrap();
    let receiver_announce = receiver.flush_announces(1, &mut rng);
    sender.ingest(receiver_announce[0].bytes(), 1, RnsInterfaceId(2), &mut rng);
    let mut encoded = [0u8; PACKET_CAPACITY];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"proof action",
            2,
            &mut rng,
            &mut encoded,
        )
        .unwrap();
    let proof = receiver
        .ingest(
            &encoded[..usize::from(prepared.packet_len())],
            2,
            RnsInterfaceId(7),
            &mut rng,
        )
        .actions;
    assert_eq!(proof.packets.len(), 1);
    assert_eq!(
        proof.packets[0].target(),
        RnsTxTarget::Only(RnsInterfaceId(7))
    );

    let mut relay_sender = rns_node(43, EmbeddedNodeConfig::endpoint());
    let mut target = rns_node(44, EmbeddedNodeConfig::endpoint());
    let mut relay = rns_node(45, EmbeddedNodeConfig::transport());
    relay_sender
        .register_peer(&rns_identity(44), "reticulum", &["ordinary-actions"], 3)
        .unwrap();
    target.queue_announce(None, 3, &mut rng).unwrap();
    let target_announce = target
        .flush_announces(3, &mut rng)
        .into_iter()
        .next()
        .expect("the target announce must be ready immediately");
    let learned = relay.ingest(target_announce.bytes(), 3, RnsInterfaceId(9), &mut rng);
    assert_eq!(
        learned.disposition,
        reticulum_rns_rete::IngressDisposition::Processed
    );
    assert_eq!(
        relay
            .route(&target.destination_hash())
            .and_then(|route| route.received_on),
        Some(RnsInterfaceId(9))
    );
    let mut forwarded_bytes = [0u8; PACKET_CAPACITY];
    let forwarded = relay_sender
        .prepare_data_into(
            &target.destination_hash(),
            b"forward action",
            3,
            &mut rng,
            &mut forwarded_bytes,
        )
        .unwrap();
    let forwarding = relay
        .ingest_local_origin(
            &forwarded_bytes[..usize::from(forwarded.packet_len())],
            3,
            RnsInterfaceId(5),
            &mut rng,
        )
        .actions;
    assert_eq!(forwarding.packets.len(), 1);
    assert_eq!(
        forwarding.packets[0].target(),
        RnsTxTarget::Only(RnsInterfaceId(9))
    );
    (proof, forwarding)
}

#[test]
fn registration_is_stable_and_validation_rejects_foreign_and_busy_buffers() {
    let mut first_node = node(1, NodeRole::Endpoint);
    let mut second_node = node(2, NodeRole::Endpoint);
    let mut first_owner = first_node.take_ordinary_action_owner::<2>().unwrap();
    let mut second_owner = second_node.take_ordinary_action_owner::<1>().unwrap();
    let mut first = OrdinaryPacketBuffer::new();
    let mut second = OrdinaryPacketBuffer::new();
    let mut extra = OrdinaryPacketBuffer::new();
    let mut foreign = OrdinaryPacketBuffer::new();

    assert_eq!(
        first_owner
            .register_packet_buffer(&mut first)
            .unwrap()
            .get(),
        0
    );
    assert_eq!(
        first_owner
            .register_packet_buffer(&mut second)
            .unwrap()
            .get(),
        1
    );
    assert_eq!(
        first_owner.register_packet_buffer(&mut first),
        Err(OrdinaryBufferRegistrationError::AlreadyRegistered)
    );
    assert_eq!(
        first_owner.register_packet_buffer(&mut extra),
        Err(OrdinaryBufferRegistrationError::PoolFull { limit: 2 })
    );
    second_owner.register_packet_buffer(&mut foreign).unwrap();
    assert_eq!(
        first_owner.validate_available_buffer(&foreign),
        Err(OrdinaryAvailableBufferError::ForeignOwner)
    );

    let mut owners = [&mut first];
    let mut batch = first_owner
        .admit(
            announces(1),
            &mut owners,
            request(10, 100, interfaces(1 << 1)),
        )
        .unwrap();
    let job = batch.take_next_packet().unwrap();
    drop(job);
    drop(batch);
    assert_eq!(
        first_owner.validate_available_buffer(&first),
        Err(OrdinaryAvailableBufferError::Busy)
    );
    assert_eq!(
        first_owner.capacities(),
        OrdinaryActionCapacitySnapshot {
            registered: 2,
            active: 1,
            limit: 2,
        }
    );
}

#[test]
fn register_and_park_collision_is_atomic_and_returns_the_exact_unregistered_owner() {
    let mut first_node = node(61, NodeRole::Endpoint);
    let mut second_node = node(62, NodeRole::Endpoint);
    let mut first_owner = first_node.take_ordinary_action_owner::<1>().unwrap();
    let mut second_owner = second_node.take_ordinary_action_owner::<1>().unwrap();
    let mut first = OrdinaryPacketBuffer::new();
    let mut second = OrdinaryPacketBuffer::new();
    let second_pointer = core::ptr::from_ref(&second);
    let mut pool = OrdinaryBufferPool::<1>::new();

    assert!(matches!(
        first_owner.register_and_park(&mut pool, &mut first),
        Ok(OrdinaryPacketSlotId(0))
    ));
    let before = second_owner.capacities();
    let failure = second_owner
        .register_and_park(&mut pool, &mut second)
        .expect_err("occupied stable slot unexpectedly accepted a second owner");
    assert_eq!(
        failure.reason(),
        OrdinaryRegisterAndParkError::Parking(OrdinaryBufferPoolError::SlotOccupied {
            slot: OrdinaryPacketSlotId(0),
        })
    );
    let second = failure.into_buffer();
    assert_eq!(core::ptr::from_ref(&*second), second_pointer);
    assert_eq!(second.slot_id(), None);
    assert_eq!(second_owner.capacities(), before);
    assert_eq!(pool.parked_count(), 1);
    assert!(pool.contains_slot(OrdinaryPacketSlotId(0)));
}

#[test]
fn park_return_collision_retains_the_complete_owner_for_reparking_and_readmission() {
    let mut first_node = node(64, NodeRole::Endpoint);
    let mut second_node = node(65, NodeRole::Endpoint);
    let mut first_owner = first_node.take_ordinary_action_owner::<1>().unwrap();
    let mut second_owner = second_node.take_ordinary_action_owner::<1>().unwrap();
    let mut first_buffer = OrdinaryPacketBuffer::new();
    let mut second_buffer = OrdinaryPacketBuffer::new();
    let first_pointer = core::ptr::from_ref(&first_buffer);
    let mut first_pool = OrdinaryBufferPool::<1>::new();
    let mut second_pool = OrdinaryBufferPool::<1>::new();
    let slot = first_owner
        .register_and_park(&mut first_pool, &mut first_buffer)
        .unwrap_or_else(|failure| panic!("first registration: {:?}", failure.reason()));
    second_owner
        .register_and_park(&mut second_pool, &mut second_buffer)
        .unwrap_or_else(|failure| panic!("second registration: {:?}", failure.reason()));

    let mut batch = first_owner
        .admit_from_pool(
            announces(1),
            &mut first_pool,
            request(10, 100, interfaces(1 << 1)),
        )
        .unwrap();
    let job = batch.take_next_packet().unwrap();
    let returned = match complete_unpermitted(&mut first_owner, job, owner_time(20)).unwrap() {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        OrdinaryCompletionDisposition::Next(_) => panic!("single route fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => {
            panic!("valid completion quarantined")
        }
    };
    let prepared = returned.prepared();
    let failure = second_pool
        .park_return(returned)
        .expect_err("occupied foreign pool slot unexpectedly accepted a return");
    assert_eq!(
        failure.reason(),
        OrdinaryBufferPoolError::SlotOccupied { slot }
    );
    let returned = failure.into_return();
    assert_eq!(returned.prepared(), prepared);
    assert_eq!(
        first_pool
            .park_return(returned)
            .unwrap_or_else(|failure| panic!("reparking: {:?}", failure.reason())),
        slot
    );

    let mut second_batch = first_owner
        .admit_from_pool(
            announces(1),
            &mut first_pool,
            request(30, 100, interfaces(1 << 1)),
        )
        .unwrap();
    let second_job = second_batch.take_next_packet().unwrap();
    assert!(second_job.generation() > prepared.generation());
    let returned = match complete_unpermitted(&mut first_owner, second_job, owner_time(40)).unwrap()
    {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        OrdinaryCompletionDisposition::Next(_) => panic!("single route fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => {
            panic!("valid second completion quarantined")
        }
    };
    assert_eq!(core::ptr::from_ref(&*returned.into_buffer()), first_pointer);
    assert!(second_pool.contains_slot(slot));
}

#[test]
fn quarantine_leaves_only_its_stable_pool_slot_unparked() {
    let mut node = node(63, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<2>().unwrap();
    let mut first_buffer = OrdinaryPacketBuffer::new();
    let mut second_buffer = OrdinaryPacketBuffer::new();
    let mut pool = OrdinaryBufferPool::<2>::new();
    let first_slot = owner
        .register_and_park(&mut pool, &mut first_buffer)
        .unwrap_or_else(|failure| panic!("first registration: {:?}", failure.reason()));
    let second_slot = owner
        .register_and_park(&mut pool, &mut second_buffer)
        .unwrap_or_else(|failure| panic!("second registration: {:?}", failure.reason()));
    let mut batch = owner
        .admit_from_pool(
            announces(2),
            &mut pool,
            request(10, 100, interfaces(1 << 1)),
        )
        .unwrap();
    let first = batch.take_next_packet().unwrap();
    let second = batch.take_next_packet().unwrap();
    assert!(pool.is_empty());

    let first_return = match complete_unpermitted(&mut owner, first, owner_time(20)).unwrap() {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        OrdinaryCompletionDisposition::Next(_) => panic!("single route fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => {
            panic!("valid first completion quarantined")
        }
    };
    pool.park_return(first_return)
        .unwrap_or_else(|failure| panic!("first return parking: {:?}", failure.reason()));

    let quarantine = match owner
        .complete_tx(
            second.recovery_fault(TxCompletionCode::new(0x6302)),
            owner_time(20),
        )
        .unwrap()
    {
        OrdinaryCompletionDisposition::Quarantined(quarantine) => quarantine,
        OrdinaryCompletionDisposition::Next(_) => panic!("recovery fault fanned out"),
        OrdinaryCompletionDisposition::Returned(_) => {
            panic!("recovery fault returned an available owner")
        }
    };
    assert_eq!(
        quarantine.reason(),
        OrdinaryQuarantineReason::RecoveryFault(TxCompletionCode::new(0x6302))
    );
    assert!(pool.contains_slot(first_slot));
    assert!(!pool.contains_slot(second_slot));
    assert_eq!(pool.parked_count(), 1);
    assert_eq!(
        owner.capacities(),
        OrdinaryActionCapacitySnapshot {
            registered: 2,
            active: 1,
            limit: 2,
        }
    );
}

#[test]
fn node_issues_only_one_pool_while_a_slot_zero_generation_one_job_is_live() {
    let mut node = node(9, NodeRole::Endpoint);
    {
        let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
        let mut buffer = OrdinaryPacketBuffer::new();
        owner.register_packet_buffer(&mut buffer).unwrap();
        let mut buffers = [&mut buffer];
        let mut batch = owner
            .admit(
                announces(1),
                &mut buffers,
                request(1, 10, interfaces(1 << 1)),
            )
            .unwrap();
        let job = batch.take_next_packet().unwrap();
        assert_eq!(job.slot_id(), OrdinaryPacketSlotId(0));
        assert_eq!(job.generation(), OrdinaryPacketGeneration(1));

        assert_eq!(
            node.take_ordinary_action_owner::<1>()
                .err()
                .expect("same node incarnation issued a second ordinary pool"),
            OrdinaryActionOwnerClaimError::AlreadyClaimed
        );

        let returned = match complete_unpermitted(&mut owner, job, owner_time(2)).unwrap() {
            OrdinaryCompletionDisposition::Returned(value) => value,
            OrdinaryCompletionDisposition::Next(_) => panic!("single route fanned out"),
            OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
        };
        assert_eq!(returned.reason(), OrdinaryPacketReturnReason::RouteComplete);
    }
    assert_eq!(
        node.take_ordinary_action_owner::<2>()
            .err()
            .expect("dropping the first pool released its one-shot claim"),
        OrdinaryActionOwnerClaimError::AlreadyClaimed
    );
}

#[test]
fn full_pool_failure_is_atomic_and_returns_the_exact_action_vectors() {
    let mut node = node(3, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut actions = announces(2);
    actions.unroutable_packets = 7;
    let packets_ptr = actions.packets.as_ptr();
    let packets_capacity = actions.packets.capacity();
    let first_bytes = actions.packets[0].bytes().to_vec();
    let before = owner.capacities();
    let mut owners = [&mut buffer];

    let failure = owner
        .admit(actions, &mut owners, request(10, 100, interfaces(1 << 1)))
        .err()
        .expect("full pool unexpectedly admitted the batch");
    assert_eq!(
        failure.reason(),
        OrdinaryActionAdmissionError::PoolFull {
            needed: 2,
            available: 1,
        }
    );
    let actions = failure.into_actions();
    assert_eq!(actions.packets.as_ptr(), packets_ptr);
    assert_eq!(actions.packets.capacity(), packets_capacity);
    assert_eq!(actions.packets.len(), 2);
    assert_eq!(actions.packets[0].bytes(), first_bytes);
    assert_eq!(actions.unroutable_packets, 7);
    assert_eq!(owner.capacities(), before);
    assert!(buffer.bytes.iter().all(|byte| *byte == 0));
    assert_eq!(
        owner.validate_available_buffer(&buffer),
        Ok(OrdinaryPacketSlotId(0))
    );
}

#[test]
fn whole_batch_preserves_order_targets_non_packet_actions_and_fixed_bytes() {
    let mut node = node(4, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<3>().unwrap();
    let mut first_buffer = OrdinaryPacketBuffer::new();
    let mut second_buffer = OrdinaryPacketBuffer::new();
    let mut third_buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut first_buffer).unwrap();
    owner.register_packet_buffer(&mut second_buffer).unwrap();
    owner.register_packet_buffer(&mut third_buffer).unwrap();

    let mut announce = announces(1);
    let (mut actions, mut forwarding) = resolved_routing_actions();
    assert!(forwarding.events.is_empty());
    let mut packets = core::mem::take(&mut announce.packets);
    packets.append(&mut actions.packets);
    packets.append(&mut forwarding.packets);
    actions.packets = packets;
    actions.unroutable_packets = 9;
    let expected: std::vec::Vec<_> = actions
        .packets
        .iter()
        .map(|packet| (packet.bytes().to_vec(), packet.target()))
        .collect();
    assert_eq!(
        expected
            .iter()
            .map(|value| value.1)
            .collect::<std::vec::Vec<_>>(),
        [
            RnsTxTarget::All,
            RnsTxTarget::Only(RnsInterfaceId(7)),
            RnsTxTarget::Only(RnsInterfaceId(9)),
        ]
    );

    let mut owners = [&mut first_buffer, &mut second_buffer, &mut third_buffer];
    let mut batch = owner
        .admit(
            actions,
            &mut owners,
            request(
                10,
                100,
                interfaces((1 << 1) | (1 << 3) | (1 << 7) | (1 << 9)),
            ),
        )
        .unwrap();
    assert_eq!(batch.packet_count(), 3);
    assert_eq!(batch.remaining_packet_count(), 3);
    assert!(batch.non_packet_actions().packets.is_empty());
    assert_eq!(batch.non_packet_actions().unroutable_packets, 9);
    assert!(batch.non_packet_actions().events.iter().any(|event| {
            matches!(event, ApplicationEvent::DataReceived { payload, .. } if payload == b"proof action")
        }));
    let retained = batch.take_non_packet_actions();
    assert_eq!(retained.unroutable_packets, 9);
    assert!(batch.non_packet_actions().events.is_empty());

    let first = batch.take_next_packet().unwrap();
    let second = batch.take_next_packet().unwrap();
    let third = batch.take_next_packet().unwrap();
    assert!(batch.take_next_packet().is_none());
    assert_eq!(batch.remaining_packet_count(), 0);
    assert_eq!(first.slot_id().get(), 0);
    assert_eq!(second.slot_id().get(), 1);
    assert_eq!(third.slot_id().get(), 2);
    assert_eq!(first.generation().get(), 1);
    assert_eq!(second.generation().get(), 2);
    assert_eq!(third.generation().get(), 3);
    assert_eq!(first.target(), TxTarget::All);
    assert_eq!(first.interface(), PacketInterfaceId::new(1));
    assert_eq!(second.target(), TxTarget::Only(PacketInterfaceId::new(7)));
    assert_eq!(second.interface(), PacketInterfaceId::new(7));
    assert_eq!(third.target(), TxTarget::Only(PacketInterfaceId::new(9)));
    assert_eq!(third.interface(), PacketInterfaceId::new(9));
    assert_eq!(
        &first.buffer.bytes[..usize::from(first.packet_len())],
        expected[0].0
    );
    assert_eq!(
        &second.buffer.bytes[..usize::from(second.packet_len())],
        expected[1].0
    );
    assert_eq!(
        &third.buffer.bytes[..usize::from(third.packet_len())],
        expected[2].0
    );

    let first_generation = first.generation();
    let first = match complete_unpermitted(&mut owner, first, owner_time(20)).unwrap() {
        OrdinaryCompletionDisposition::Next(job) => job,
        OrdinaryCompletionDisposition::Returned(_) => panic!("fan-out ended early"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid fan-out quarantined"),
    };
    assert_eq!(first.interface(), PacketInterfaceId::new(3));
    assert_eq!(first.generation(), first_generation);
    let first = match complete_unpermitted(&mut owner, first, owner_time(30)).unwrap() {
        OrdinaryCompletionDisposition::Next(job) => job,
        OrdinaryCompletionDisposition::Returned(_) => panic!("fan-out ended early"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid fan-out quarantined"),
    };
    assert_eq!(first.interface(), PacketInterfaceId::new(7));
    let first = match complete_unpermitted(&mut owner, first, owner_time(40)).unwrap() {
        OrdinaryCompletionDisposition::Next(job) => job,
        OrdinaryCompletionDisposition::Returned(_) => panic!("fan-out ended early"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid fan-out quarantined"),
    };
    assert_eq!(first.interface(), PacketInterfaceId::new(9));
    assert_eq!(first.generation(), first_generation);
    let first_return = match complete_unpermitted(&mut owner, first, owner_time(50)).unwrap() {
        OrdinaryCompletionDisposition::Returned(value) => value,
        OrdinaryCompletionDisposition::Next(_) => panic!("fan-out did not finish"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
    };
    assert_eq!(
        first_return.reason(),
        OrdinaryPacketReturnReason::RouteComplete
    );

    let second_return = match complete_unpermitted(&mut owner, second, owner_time(40)).unwrap() {
        OrdinaryCompletionDisposition::Returned(value) => value,
        OrdinaryCompletionDisposition::Next(_) => panic!("only-target packet fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
    };
    assert_eq!(
        second_return.reason(),
        OrdinaryPacketReturnReason::RouteComplete
    );

    let third_return = match complete_unpermitted(&mut owner, third, owner_time(50)).unwrap() {
        OrdinaryCompletionDisposition::Returned(value) => value,
        OrdinaryCompletionDisposition::Next(_) => panic!("only-target packet fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
    };
    assert_eq!(
        third_return.reason(),
        OrdinaryPacketReturnReason::RouteComplete
    );
    assert_eq!(owner.capacities().active, 0);
    assert_eq!(
        owner.validate_available_buffer(first_return.into_buffer()),
        Ok(OrdinaryPacketSlotId(0))
    );
    assert_eq!(
        owner.validate_available_buffer(second_return.into_buffer()),
        Ok(OrdinaryPacketSlotId(1))
    );
    assert_eq!(
        owner.validate_available_buffer(third_return.into_buffer()),
        Ok(OrdinaryPacketSlotId(2))
    );
}

#[test]
fn deadline_and_generation_checks_fail_closed_without_losing_owners() {
    let mut core = node(5, NodeRole::Endpoint);
    let mut owner = core.take_ordinary_action_owner::<1>().unwrap();
    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut owners = [&mut buffer];

    let expired = owner
        .admit(
            announces(1),
            &mut owners,
            request(100, 100, interfaces((1 << 1) | (1 << 2))),
        )
        .err()
        .expect("expired deadline unexpectedly admitted the packet");
    assert_eq!(
        expired.reason(),
        OrdinaryActionAdmissionError::DeadlineExpired {
            now: owner_time(100),
            deadline: deadline(100),
        }
    );
    assert_eq!(expired.into_actions().packets.len(), 1);
    assert_eq!(owner.generation, 0);

    let mut batch = owner
        .admit(
            announces(1),
            &mut owners,
            request(99, 100, interfaces((1 << 1) | (1 << 2))),
        )
        .unwrap();
    let job = batch.take_next_packet().unwrap();
    drop(batch);
    let mut foreign_node = node(55, NodeRole::Endpoint);
    let mut foreign_owner = foreign_node.take_ordinary_action_owner::<1>().unwrap();
    let completion = job
        .return_unpermitted()
        .complete(TxCompletionCode::new(0x554e));
    let failure = foreign_owner
        .complete_tx(completion, owner_time(99))
        .err()
        .expect("foreign owner unexpectedly completed the packet");
    assert_eq!(failure.reason(), OrdinaryCompletionError::ForeignOwner);
    let completion = failure.into_completion();
    let packet = completion.job.packet;
    owner.slots[packet.slot.index()].state = OrdinarySlotState::Active(OrdinaryDispatchRecord {
        packet: OrdinaryPreparedPacket {
            generation: OrdinaryPacketGeneration(packet.generation.get() + 1),
            ..packet
        },
        phase: OrdinaryDispatchPhase::Routed,
        may_have_transmitted: false,
    });
    let failure = owner
        .complete_tx(completion, owner_time(99))
        .err()
        .expect("stale generation unexpectedly completed");
    assert_eq!(failure.reason(), OrdinaryCompletionError::StaleGeneration);
    let completion = failure.into_completion();
    owner.slots[packet.slot.index()].state = OrdinarySlotState::Active(OrdinaryDispatchRecord {
        packet,
        phase: OrdinaryDispatchPhase::Routed,
        may_have_transmitted: false,
    });
    let returned = match owner.complete_tx(completion, owner_time(100)).unwrap() {
        OrdinaryCompletionDisposition::Returned(value) => value,
        OrdinaryCompletionDisposition::Next(_) => panic!("deadline permitted fan-out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
    };
    assert_eq!(
        returned.reason(),
        OrdinaryPacketReturnReason::DeadlineExpired
    );
    assert_eq!(
        returned.prepared().generation(),
        OrdinaryPacketGeneration(1)
    );
    assert_eq!(owner.capacities().active, 0);
}

#[test]
fn final_definitive_hop_at_deadline_is_route_complete_not_suppressed() {
    let mut node = node(10, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut buffers = [&mut buffer];
    let mut batch = owner
        .admit(
            announces(1),
            &mut buffers,
            request(9, 10, interfaces(1 << 1)),
        )
        .unwrap();
    let job = batch.take_next_packet().unwrap();
    assert!(job.prepared().remaining_interfaces().is_empty());

    let returned = match complete_unpermitted(&mut owner, job, owner_time(10)).unwrap() {
        OrdinaryCompletionDisposition::Returned(value) => value,
        OrdinaryCompletionDisposition::Next(_) => panic!("final hop unexpectedly fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
    };
    assert_eq!(returned.reason(), OrdinaryPacketReturnReason::RouteComplete);
    assert_eq!(owner.capacities().active, 0);
}

#[test]
fn ordinary_covering_grant_is_linearized_and_exposes_bytes_once() {
    let mut node = node(11, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut buffers = [&mut buffer];
    let mut batch = owner
        .admit(
            announces(1),
            &mut buffers,
            request(10, 100, interfaces(1 << 2)),
        )
        .unwrap();
    let job = batch.take_next_packet().unwrap();
    let packet = job.prepared();
    let expected_bytes = job.buffer.bytes[..usize::from(job.packet_len())].to_vec();
    let resource = crate::TxPermitResourceId::new([0x61; 16]);
    let requirements = TxPermitRequirements::try_new(resource, 123_456).unwrap();
    let reservation = TxPermitReservation::try_new(resource, 123_500).unwrap();
    let (pending, request) = job.begin_permit(requirements);
    let mut policy = RecordingPolicy::authorizing(reservation);
    let reply = owner
        .authorize_tx(request, owner_time(20), &mut policy)
        .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));

    assert_eq!(policy.calls, 1);
    assert_eq!(
        policy.candidate,
        Some(TxAuthorizationCandidate {
            interface: PacketInterfaceId::new(2),
            packet_len: packet.packet_len(),
            requirements,
            now: owner_time(20),
            deadline: deadline(100),
            may_have_transmitted: false,
        })
    );
    assert_eq!(
        match &reply {
            OrdinaryTxPermitReply::Granted(grant) => grant.reservation(),
            OrdinaryTxPermitReply::Denied(_) => panic!("covering reservation was denied"),
        },
        reservation
    );
    assert!(matches!(
        owner.slots[packet.slot.index()].state,
        OrdinarySlotState::Active(OrdinaryDispatchRecord {
            phase: OrdinaryDispatchPhase::Authorized,
            may_have_transmitted: true,
            ..
        })
    ));

    let mut authorized = match pending.resolve(reply, owner_time(21)) {
        Ok(OrdinaryPermitResolution::Authorized(authorized)) => authorized,
        Ok(OrdinaryPermitResolution::Expired(_)) => panic!("fresh grant expired"),
        Ok(OrdinaryPermitResolution::Unpermitted(_)) => panic!("grant became denial"),
        Err(_) => panic!("matching grant did not resolve"),
    };
    assert_eq!(authorized.reservation(), reservation);
    let frame = authorized.frame(owner_time(22)).unwrap();
    assert_eq!(frame.bytes(), expected_bytes);
    assert_eq!(frame.prepared(), packet);
    assert!(matches!(
        authorized.frame(owner_time(23)),
        Err(OrdinaryFrameError::AlreadyTaken)
    ));
    let completion = authorized.complete_transmitted(TxCompletionCode::new(0x4155));
    assert_eq!(
        completion.prepared().dispatch_token(),
        packet.dispatch_token()
    );
    assert_eq!(
        completion.transmission_outcome(),
        OrdinaryTransmissionOutcome::Transmitted
    );
    let returned = match owner.complete_tx(completion, owner_time(24)).unwrap() {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        OrdinaryCompletionDisposition::Next(_) => panic!("single route fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid grant quarantined"),
    };
    assert_eq!(returned.reason(), OrdinaryPacketReturnReason::RouteComplete);
    assert!(returned.may_have_transmitted());
}

#[test]
fn noncovering_reservation_denies_before_phase_or_history_mutation() {
    let mut node = node(12, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut buffers = [&mut buffer];
    let mut batch = owner
        .admit(
            announces(1),
            &mut buffers,
            request(10, 100, interfaces((1 << 1) | (1 << 3))),
        )
        .unwrap();
    let job = batch.take_next_packet().unwrap();
    let packet = job.prepared();
    let resource = crate::TxPermitResourceId::new([0x62; 16]);
    let requirements = TxPermitRequirements::try_new(resource, 500_000).unwrap();
    let reservation = TxPermitReservation::try_new(resource, 499_999).unwrap();
    let (pending, request) = job.begin_permit(requirements);
    let mut policy = RecordingPolicy::authorizing(reservation);
    let reply = owner
        .authorize_tx(request, owner_time(20), &mut policy)
        .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));
    assert!(matches!(
        &reply,
        OrdinaryTxPermitReply::Denied(denial)
            if denial.reason()
                == OrdinaryTxPermitDenialReason::Policy(TxPolicyDenial::CapacityUnavailable)
    ));
    assert!(matches!(
        owner.slots[packet.slot.index()].state,
        OrdinarySlotState::Active(OrdinaryDispatchRecord {
            phase: OrdinaryDispatchPhase::Routed,
            may_have_transmitted: false,
            ..
        })
    ));
    let unpermitted = match pending.resolve(reply, owner_time(21)) {
        Ok(OrdinaryPermitResolution::Unpermitted(unpermitted)) => unpermitted,
        Ok(OrdinaryPermitResolution::Authorized(_)) => panic!("under-reservation authorized"),
        Ok(OrdinaryPermitResolution::Expired(_)) => panic!("denial became expired grant"),
        Err(_) => panic!("matching denial did not resolve"),
    };
    let next = match owner
        .complete_tx(
            unpermitted.complete(TxCompletionCode::new(0x554e)),
            owner_time(22),
        )
        .unwrap()
    {
        OrdinaryCompletionDisposition::Next(next) => next,
        OrdinaryCompletionDisposition::Returned(_) => panic!("denied hop stopped fan-out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid denial quarantined"),
    };
    assert_eq!(next.interface(), PacketInterfaceId::new(3));
}

#[test]
fn cumulative_authorization_reaches_next_candidate_and_authorized_cancel_stops_fanout() {
    let mut node = node(13, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut buffers = [&mut buffer];
    let mut batch = owner
        .admit(
            announces(1),
            &mut buffers,
            request(10, 100, interfaces((1 << 1) | (1 << 2) | (1 << 3))),
        )
        .unwrap();
    let first = batch.take_next_packet().unwrap();
    let resource = crate::TxPermitResourceId::new([0x63; 16]);
    let requirements = TxPermitRequirements::try_new(resource, 200_000).unwrap();
    let reservation = TxPermitReservation::try_new(resource, 200_000).unwrap();
    let (pending, request) = first.begin_permit(requirements);
    let reply = owner
        .authorize_tx(
            request,
            owner_time(20),
            &mut RecordingPolicy::authorizing(reservation),
        )
        .unwrap_or_else(|failure| panic!("first authorization: {:?}", failure.reason()));
    let first = match pending.resolve(reply, owner_time(21)) {
        Ok(OrdinaryPermitResolution::Authorized(authorized)) => authorized,
        _ => panic!("first grant did not authorize"),
    };
    let second = match owner
        .complete_tx(first.complete(TxCompletionCode::new(1)), owner_time(22))
        .unwrap()
    {
        OrdinaryCompletionDisposition::Next(next) => next,
        OrdinaryCompletionDisposition::Returned(_) => panic!("first hop stopped fan-out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("first hop quarantined"),
    };
    assert_eq!(second.interface(), PacketInterfaceId::new(2));

    let (pending, request) = second.begin_permit(requirements);
    let mut second_policy = RecordingPolicy::authorizing(reservation);
    let reply = owner
        .authorize_tx(request, owner_time(23), &mut second_policy)
        .unwrap_or_else(|failure| panic!("second authorization: {:?}", failure.reason()));
    assert!(second_policy.candidate.unwrap().may_have_transmitted);
    let second = match pending.resolve(reply, owner_time(24)) {
        Ok(OrdinaryPermitResolution::Authorized(authorized)) => authorized,
        _ => panic!("second grant did not authorize"),
    };
    let returned = match owner
        .complete_tx(second.cancel(TxCompletionCode::new(2)), owner_time(25))
        .unwrap()
    {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        OrdinaryCompletionDisposition::Next(_) => panic!("authorized cancel fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => {
            panic!("authorized cancel was phase-incompatible")
        }
    };
    assert_eq!(returned.reason(), OrdinaryPacketReturnReason::Cancelled);
    assert!(returned.may_have_transmitted());
    assert_eq!(owner.capacities().active, 0);
}

#[test]
fn final_hop_authorized_and_unpermitted_cancellation_both_report_route_complete() {
    let mut node = node(14, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<2>().unwrap();
    let mut first_buffer = OrdinaryPacketBuffer::new();
    let mut second_buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut first_buffer).unwrap();
    owner.register_packet_buffer(&mut second_buffer).unwrap();
    let mut buffers = [&mut first_buffer, &mut second_buffer];
    let mut batch = owner
        .admit(
            announces(2),
            &mut buffers,
            request(10, 100, interfaces(1 << 1)),
        )
        .unwrap();
    let unpermitted = batch.take_next_packet().unwrap();
    let authorized = batch.take_next_packet().unwrap();

    let unpermitted_return = match owner
        .complete_tx(unpermitted.cancel(TxCompletionCode::new(3)), owner_time(20))
        .unwrap()
    {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        OrdinaryCompletionDisposition::Next(_) => panic!("final cancel fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("final cancel quarantined"),
    };
    assert_eq!(
        unpermitted_return.reason(),
        OrdinaryPacketReturnReason::RouteComplete
    );
    assert!(!unpermitted_return.may_have_transmitted());

    let resource = crate::TxPermitResourceId::new([0x64; 16]);
    let requirements = TxPermitRequirements::try_new(resource, 100_000).unwrap();
    let reservation = TxPermitReservation::try_new(resource, 100_000).unwrap();
    let (pending, request) = authorized.begin_permit(requirements);
    let reply = owner
        .authorize_tx(
            request,
            owner_time(21),
            &mut RecordingPolicy::authorizing(reservation),
        )
        .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));
    let authorized = match pending.resolve(reply, owner_time(22)) {
        Ok(OrdinaryPermitResolution::Authorized(authorized)) => authorized,
        _ => panic!("covering grant did not authorize"),
    };
    let authorized_return = match owner
        .complete_tx(authorized.cancel(TxCompletionCode::new(4)), owner_time(23))
        .unwrap()
    {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        OrdinaryCompletionDisposition::Next(_) => panic!("final authorized cancel fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => {
            panic!("final authorized cancel quarantined")
        }
    };
    assert_eq!(
        authorized_return.reason(),
        OrdinaryPacketReturnReason::RouteComplete
    );
    assert!(authorized_return.may_have_transmitted());
}

#[test]
fn pre_send_cancel_requires_and_retains_the_exact_request() {
    let mut node = node(15, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<2>().unwrap();
    let mut first_buffer = OrdinaryPacketBuffer::new();
    let mut second_buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut first_buffer).unwrap();
    owner.register_packet_buffer(&mut second_buffer).unwrap();
    let mut buffers = [&mut first_buffer, &mut second_buffer];
    let mut batch = owner
        .admit(
            announces(2),
            &mut buffers,
            request(10, 100, interfaces((1 << 1) | (1 << 2))),
        )
        .unwrap();
    let first = batch.take_next_packet().unwrap();
    let second = batch.take_next_packet().unwrap();
    let requirements = test_permit_requirements();
    let (first_pending, first_request) = first.begin_permit(requirements);
    let (second_pending, second_request) = second.begin_permit(requirements);

    let mismatch = first_pending
        .cancel_before_send(second_request, TxCompletionCode::new(5))
        .err()
        .expect("foreign request cancelled a pending owner");
    let (first_pending, second_request) = mismatch.into_parts();
    let first_completion = first_pending
        .cancel_before_send(first_request, TxCompletionCode::new(6))
        .unwrap_or_else(|_| panic!("matching first request did not cancel"));
    let second_completion = second_pending
        .cancel_before_send(second_request, TxCompletionCode::new(7))
        .unwrap_or_else(|_| panic!("matching second request did not cancel"));
    for completion in [first_completion, second_completion] {
        let returned = match owner.complete_tx(completion, owner_time(20)).unwrap() {
            OrdinaryCompletionDisposition::Returned(returned) => returned,
            OrdinaryCompletionDisposition::Next(_) => panic!("pre-send cancel fanned out"),
            OrdinaryCompletionDisposition::Quarantined(_) => {
                panic!("matching pre-send cancel quarantined")
            }
        };
        assert_eq!(returned.reason(), OrdinaryPacketReturnReason::Cancelled);
        assert!(!returned.may_have_transmitted());
    }
}

#[test]
fn deadline_denial_skips_policy_and_delayed_grant_remains_authorized_without_bytes() {
    let mut first_node = node(16, NodeRole::Endpoint);
    let mut first_owner = first_node.take_ordinary_action_owner::<1>().unwrap();
    let mut first_buffer = OrdinaryPacketBuffer::new();
    first_owner
        .register_packet_buffer(&mut first_buffer)
        .unwrap();
    let mut first_buffers = [&mut first_buffer];
    let mut first_batch = first_owner
        .admit(
            announces(1),
            &mut first_buffers,
            request(10, 30, interfaces(1 << 1)),
        )
        .unwrap();
    let first = first_batch.take_next_packet().unwrap();
    let (pending, permit_request) = first.begin_permit(test_permit_requirements());
    let mut denied_policy = RecordingPolicy::denying(TxPolicyDenial::PolicyDenied);
    let reply = first_owner
        .authorize_tx(permit_request, owner_time(30), &mut denied_policy)
        .unwrap_or_else(|failure| panic!("deadline request invalid: {:?}", failure.reason()));
    assert_eq!(denied_policy.calls, 0);
    assert!(matches!(
        &reply,
        OrdinaryTxPermitReply::Denied(denial)
            if denial.reason() == OrdinaryTxPermitDenialReason::DeadlineExpired
    ));
    let unpermitted = match pending.resolve(reply, owner_time(30)) {
        Ok(OrdinaryPermitResolution::Unpermitted(unpermitted)) => unpermitted,
        _ => panic!("deadline denial did not stay unpermitted"),
    };
    let returned = match first_owner
        .complete_tx(
            unpermitted.complete(TxCompletionCode::new(8)),
            owner_time(30),
        )
        .unwrap()
    {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        _ => panic!("final deadline denial did not return"),
    };
    assert_eq!(returned.reason(), OrdinaryPacketReturnReason::RouteComplete);
    assert!(!returned.may_have_transmitted());

    let mut second_node = node(17, NodeRole::Endpoint);
    let mut second_owner = second_node.take_ordinary_action_owner::<1>().unwrap();
    let mut second_buffer = OrdinaryPacketBuffer::new();
    second_owner
        .register_packet_buffer(&mut second_buffer)
        .unwrap();
    let mut second_buffers = [&mut second_buffer];
    let mut second_batch = second_owner
        .admit(
            announces(1),
            &mut second_buffers,
            request(10, 30, interfaces(1 << 1)),
        )
        .unwrap();
    let second = second_batch.take_next_packet().unwrap();
    let resource = crate::TxPermitResourceId::new([0x65; 16]);
    let requirements = TxPermitRequirements::try_new(resource, 10_000).unwrap();
    let reservation = TxPermitReservation::try_new(resource, 10_000).unwrap();
    let (pending, request) = second.begin_permit(requirements);
    let reply = second_owner
        .authorize_tx(
            request,
            owner_time(20),
            &mut RecordingPolicy::authorizing(reservation),
        )
        .unwrap_or_else(|failure| panic!("grant request invalid: {:?}", failure.reason()));
    let expired = match pending.resolve(reply, owner_time(30)) {
        Ok(OrdinaryPermitResolution::Expired(expired)) => expired,
        _ => panic!("exact-deadline grant exposed an authorized frame"),
    };
    assert_eq!(expired.deadline(), deadline(30));
    assert_eq!(expired.reservation(), reservation);
    let completion = expired.complete(TxCompletionCode::new(9));
    assert_eq!(
        completion.transmission_outcome(),
        OrdinaryTransmissionOutcome::NotConfirmed
    );
    let returned = match second_owner
        .complete_tx(completion, owner_time(30))
        .unwrap()
    {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        _ => panic!("final delayed grant did not return"),
    };
    assert_eq!(returned.reason(), OrdinaryPacketReturnReason::RouteComplete);
    assert!(returned.may_have_transmitted());
}

#[test]
fn authorization_invariant_and_completion_faults_retain_unique_quarantines() {
    let mut node = node(18, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<2>().unwrap();
    let mut first_buffer = OrdinaryPacketBuffer::new();
    let mut second_buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut first_buffer).unwrap();
    owner.register_packet_buffer(&mut second_buffer).unwrap();
    let mut buffers = [&mut first_buffer, &mut second_buffer];
    let mut batch = owner
        .admit(
            announces(2),
            &mut buffers,
            request(10, 100, interfaces(1 << 1)),
        )
        .unwrap();
    let first = batch.take_next_packet().unwrap();
    let second = batch.take_next_packet().unwrap();
    let first_packet = first.prepared();
    let (pending, request) = first.begin_permit(test_permit_requirements());
    let original = match owner.slots[first_packet.slot.index()].state {
        OrdinarySlotState::Active(record) => record,
        _ => panic!("admitted packet was not active"),
    };
    owner.slots[first_packet.slot.index()].state =
        OrdinarySlotState::Active(OrdinaryDispatchRecord {
            packet: OrdinaryPreparedPacket {
                scope: TxOwnerScope {
                    owner_identity: [0xee; 16],
                    instance: crate::NodeInstanceId::new([0xef; 16]),
                },
                ..original.packet
            },
            ..original
        });
    let mut policy = RecordingPolicy::denying(TxPolicyDenial::PolicyDenied);
    let failure = owner
        .authorize_tx(request, owner_time(20), &mut policy)
        .err()
        .expect("corrupt authoritative scope reached policy");
    assert_eq!(failure.reason(), OrdinaryAuthorizationErrorKind::Invariant);
    assert_eq!(policy.calls, 0);
    owner.slots[first_packet.slot.index()].state = OrdinarySlotState::Active(original);
    let request = failure.into_request();
    let reply = owner
        .authorize_tx(request, owner_time(21), &mut policy)
        .unwrap_or_else(|failure| panic!("restored request failed: {:?}", failure.reason()));
    let first = match pending.resolve(reply, owner_time(22)) {
        Ok(OrdinaryPermitResolution::Unpermitted(first)) => first,
        _ => panic!("restored policy denial did not resolve"),
    };
    let first_quarantine = match owner
        .complete_tx(
            first.recovery_fault(TxCompletionCode::new(10)),
            owner_time(23),
        )
        .unwrap()
    {
        OrdinaryCompletionDisposition::Quarantined(quarantine) => quarantine,
        _ => panic!("recovery fault did not quarantine"),
    };
    assert_eq!(
        first_quarantine.reason(),
        OrdinaryQuarantineReason::RecoveryFault(TxCompletionCode::new(10))
    );
    let _ = first_quarantine.into_completion();

    let completion = second
        .return_unpermitted()
        .complete(TxCompletionCode::new(11));
    completion.job.buffer.binding = OrdinaryBufferBinding::Available {
        scope: owner.scope,
        slot: completion.job.packet.slot,
    };
    let second_quarantine = match owner.complete_tx(completion, owner_time(24)).unwrap() {
        OrdinaryCompletionDisposition::Quarantined(quarantine) => quarantine,
        _ => panic!("metadata corruption did not quarantine"),
    };
    assert_eq!(
        second_quarantine.reason(),
        OrdinaryQuarantineReason::MetadataInvariant
    );
    let _ = second_quarantine.into_completion();
    assert_eq!(owner.capacities().active, 2);
}

#[test]
fn unpermitted_completion_cannot_resolve_an_authorized_phase() {
    let mut node = node(20, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut buffers = [&mut buffer];
    let mut batch = owner
        .admit(
            announces(1),
            &mut buffers,
            request(10, 100, interfaces(1 << 1)),
        )
        .unwrap();
    let job = batch.take_next_packet().unwrap();
    let (pending, request) = job.begin_permit(test_permit_requirements());
    let reservation = test_permit_reservation();
    let reply = owner
        .authorize_tx(
            request,
            owner_time(20),
            &mut RecordingPolicy::authorizing(reservation),
        )
        .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));
    let authorized = match pending.resolve(reply, owner_time(21)) {
        Ok(OrdinaryPermitResolution::Authorized(authorized)) => authorized,
        _ => panic!("grant did not authorize"),
    };
    let forged_unpermitted = OrdinaryTxCompletion {
        job: authorized.job,
        kind: OrdinaryCompletionKind::Unpermitted {
            code: TxCompletionCode::new(12),
            denial: None,
            stop_fanout: true,
        },
    };
    let quarantine = match owner
        .complete_tx(forged_unpermitted, owner_time(22))
        .unwrap()
    {
        OrdinaryCompletionDisposition::Quarantined(quarantine) => quarantine,
        _ => panic!("unpermitted completion resolved an authorized phase"),
    };
    assert_eq!(
        quarantine.reason(),
        OrdinaryQuarantineReason::CompletionClassInvariant
    );
    let _ = quarantine.into_completion();
    assert_eq!(owner.capacities().active, 1);
}

#[test]
fn next_deadline_tracks_the_earliest_active_owner_without_reclaiming_it() {
    let mut node = node(19, NodeRole::Endpoint);
    let mut owner = node.take_ordinary_action_owner::<2>().unwrap();
    let mut first_buffer = OrdinaryPacketBuffer::new();
    let mut second_buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut first_buffer).unwrap();
    owner.register_packet_buffer(&mut second_buffer).unwrap();
    assert_eq!(owner.next_deadline(), None);

    let mut first_buffers = [&mut first_buffer];
    let mut second_buffers = [&mut second_buffer];
    let first = {
        let mut batch = owner
            .admit(
                announces(1),
                &mut first_buffers,
                request(10, 80, interfaces(1 << 1)),
            )
            .unwrap();
        batch.take_next_packet().unwrap()
    };
    let second = {
        let mut batch = owner
            .admit(
                announces(1),
                &mut second_buffers,
                request(10, 60, interfaces(1 << 1)),
            )
            .unwrap();
        batch.take_next_packet().unwrap()
    };
    assert_eq!(owner.next_deadline(), Some(deadline(60)));
    assert_eq!(owner.capacities().active, 2);

    let second_return = match complete_unpermitted(&mut owner, second, owner_time(60)).unwrap() {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        _ => panic!("final second owner did not return"),
    };
    assert_eq!(
        second_return.reason(),
        OrdinaryPacketReturnReason::RouteComplete
    );
    assert_eq!(owner.next_deadline(), Some(deadline(80)));
    let first_return = match complete_unpermitted(&mut owner, first, owner_time(100)).unwrap() {
        OrdinaryCompletionDisposition::Returned(returned) => returned,
        _ => panic!("final first owner did not return"),
    };
    assert_eq!(
        first_return.reason(),
        OrdinaryPacketReturnReason::RouteComplete
    );
    assert_eq!(owner.next_deadline(), None);
}

#[test]
fn foreign_busy_invariant_and_identifier_failures_preserve_actions() {
    let mut first_node = node(6, NodeRole::Endpoint);
    let mut second_node = node(7, NodeRole::Endpoint);
    let mut owner = first_node.take_ordinary_action_owner::<2>().unwrap();
    let mut foreign_owner = second_node.take_ordinary_action_owner::<1>().unwrap();
    let mut first = OrdinaryPacketBuffer::new();
    let mut spare = OrdinaryPacketBuffer::new();
    let mut foreign = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut first).unwrap();
    owner.register_packet_buffer(&mut spare).unwrap();
    foreign_owner.register_packet_buffer(&mut foreign).unwrap();

    let mut foreign_refs = [&mut foreign];
    let failure = owner
        .admit(
            announces(1),
            &mut foreign_refs,
            request(1, 10, interfaces(1 << 1)),
        )
        .err()
        .expect("foreign buffer unexpectedly admitted the packet");
    assert_eq!(
        failure.reason(),
        OrdinaryActionAdmissionError::ForeignBuffer { buffer_index: 0 }
    );
    assert_eq!(failure.into_actions().packets.len(), 1);

    {
        let mut refs = [&mut first];
        let mut batch = owner
            .admit(announces(1), &mut refs, request(1, 10, interfaces(1 << 1)))
            .unwrap();
        assert!(batch.take_next_packet().is_some());
        drop(batch);
    }
    let mut busy_refs = [&mut first];
    let failure = owner
        .admit(
            announces(1),
            &mut busy_refs,
            request(1, 10, interfaces(1 << 1)),
        )
        .err()
        .expect("busy buffer unexpectedly admitted the packet");
    assert_eq!(
        failure.reason(),
        OrdinaryActionAdmissionError::BusyBuffer { buffer_index: 0 }
    );
    assert_eq!(failure.into_actions().packets.len(), 1);

    let spare_slot = spare.slot_id().unwrap();
    owner.slots[spare_slot.index()].state = OrdinarySlotState::Unregistered;
    owner.slots[0].state = OrdinarySlotState::Free;
    let mut invariant_refs = [&mut spare];
    let failure = owner
        .admit(
            announces(1),
            &mut invariant_refs,
            request(1, 10, interfaces(1 << 1)),
        )
        .err()
        .expect("inconsistent buffer unexpectedly admitted the packet");
    assert_eq!(
        failure.reason(),
        OrdinaryActionAdmissionError::BufferInvariant { buffer_index: 0 }
    );
    assert_eq!(failure.into_actions().packets.len(), 1);

    owner.slots[spare_slot.index()].state = OrdinarySlotState::Free;
    owner.generation = u64::MAX;
    let mut exhausted_refs = [&mut spare];
    let failure = owner
        .admit(
            announces(1),
            &mut exhausted_refs,
            request(1, 10, interfaces(1 << 1)),
        )
        .err()
        .expect("exhausted generation unexpectedly admitted the packet");
    assert_eq!(
        failure.reason(),
        OrdinaryActionAdmissionError::IdentifierExhausted
    );
    assert_eq!(failure.into_actions().packets.len(), 1);
    assert_eq!(owner.validate_available_buffer(&spare), Ok(spare_slot));
}

#[test]
fn oversize_validation_is_explicit_even_though_native_actions_are_bounded() {
    assert_eq!(
        OrdinaryActionOwner::<1>::validate_packet_len(3, PACKET_CAPACITY + 1),
        Err(OrdinaryActionAdmissionError::PacketTooLarge {
            packet_index: 3,
            actual: PACKET_CAPACITY + 1,
            maximum: PACKET_CAPACITY,
        })
    );
    assert_eq!(
        OrdinaryActionOwner::<1>::validate_packet_len(3, PACKET_CAPACITY),
        Ok(())
    );
}

#[test]
fn ordinary_admission_and_fanout_do_not_consume_data_capacity() {
    let mut node = node(8, NodeRole::Endpoint);
    node.set_inbound_proof_policy(InboundProofPolicy::Always);
    let mut data_buffer = TxPacketBuffer::new();
    node.register_packet_buffer(&mut data_buffer).unwrap();
    let mut owner = node.take_ordinary_action_owner::<1>().unwrap();
    let mut rng = CounterRng::default();
    node.queue_announce(
        Some(b"independent ordinary pool"),
        crate::AnnounceEmissionTime::new(1).expect("test announce time must fit"),
        &mut rng,
    )
    .unwrap();
    let actions = node.flush_announces(crate::MonotonicSeconds::new(1), &mut rng);
    let data_before = node.capacities();

    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut owners = [&mut buffer];
    let mut batch = owner
        .admit(actions, &mut owners, request(1, 10, interfaces(1 << 1)))
        .unwrap();
    let job = batch.take_next_packet().unwrap();
    assert_eq!(node.capacities(), data_before);
    let returned = match complete_unpermitted(&mut owner, job, owner_time(2)).unwrap() {
        OrdinaryCompletionDisposition::Returned(value) => value,
        OrdinaryCompletionDisposition::Next(_) => panic!("single route fanned out"),
        OrdinaryCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
    };
    assert_eq!(returned.reason(), OrdinaryPacketReturnReason::RouteComplete);
    assert_eq!(node.capacities(), data_before);
    let after = node.capacities();
    assert_eq!(after.receipts_used, 0);
    assert_eq!(after.attempts_used, 0);
    assert_eq!(after.dispatches_used, 0);
    assert_eq!(after.packet_buffers_registered, 1);
}

#[test]
fn protocol_token_survives_serialized_fanout() {
    let mut rng = CounterRng::default();
    let mut initiator = rns_node(90, EmbeddedNodeConfig::endpoint());
    let responder = rns_node(91, EmbeddedNodeConfig::endpoint());
    let (request_packet, _) = initiator
        .initiate_link(responder.destination_hash(), 1, &mut rng)
        .expect("test LINKREQUEST must construct");
    let token = request_packet
        .protocol_token()
        .expect("LINKREQUEST must carry a protocol timing token");
    let mut actions = NodeActions::default();
    actions.packets.push(request_packet);

    let mut core = node(92, NodeRole::Endpoint);
    let mut owner = core.take_ordinary_action_owner::<1>().unwrap();
    let mut buffer = OrdinaryPacketBuffer::new();
    owner.register_packet_buffer(&mut buffer).unwrap();
    let mut buffers = [&mut buffer];
    let mut batch = owner
        .admit(
            actions,
            &mut buffers,
            request(10, 100, interfaces((1 << 2) | (1 << 9))),
        )
        .unwrap();

    let first = batch.take_next_packet().unwrap();
    assert_eq!(first.protocol_token(), Some(token));
    assert_eq!(first.interface(), PacketInterfaceId::new(2));

    let second = match complete_unpermitted(&mut owner, first, owner_time(20)).unwrap() {
        OrdinaryCompletionDisposition::Next(job) => job,
        OrdinaryCompletionDisposition::Returned(_) => panic!("fan-out ended early"),
        OrdinaryCompletionDisposition::Quarantined(_) => {
            panic!("valid token-bearing fan-out quarantined")
        }
    };
    assert_eq!(second.protocol_token(), Some(token));
    assert_eq!(second.interface(), PacketInterfaceId::new(9));
}
