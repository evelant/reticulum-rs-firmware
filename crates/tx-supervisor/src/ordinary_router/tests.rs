extern crate std;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_interface_router::{
    InterfaceConfigId, InterfaceCost, InterfaceDescriptor, InterfaceFabric, InterfaceProperties,
    InterfaceTxJob, LogicalMtu, OutboundCompletion,
};
use reticulum_node_core::{
    AnnounceEmissionTime, MonotonicSeconds, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId,
    OrdinaryPacketBuffer, OrdinaryPermitResolution, PACKET_CAPACITY, TxAuthorizationCandidate,
    TxPermitRequirements, TxPermitReservation, TxPermitResourceId, TxPolicyDecision,
};
use reticulum_rns_rete::{
    EmbeddedNode, EmbeddedNodeConfig, InboundProofPolicy, InterfaceId as RnsInterfaceId,
};
use std::boxed::Box;

use super::*;

type TestNode = NodeCore<8, 4, 8, 8, 0>;

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

fn node(tag: u8) -> TestNode {
    TestNode::new(
        NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid"),
        "reticulum",
        &["ordinary-router"],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .expect("test node must construct")
}

fn coordinator<const BUFFERS: usize>(node: &mut TestNode) -> OrdinaryRouterCoordinator<BUFFERS> {
    let mut owner = node
        .take_ordinary_action_owner::<BUFFERS>()
        .expect("ordinary owner must be issued once");
    let mut pool = OrdinaryBufferPool::new();
    for _ in 0..BUFFERS {
        owner
            .register_and_park(&mut pool, Box::leak(Box::new(OrdinaryPacketBuffer::new())))
            .unwrap_or_else(|failure| {
                panic!("ordinary buffer must register: {:?}", failure.reason())
            });
    }
    OrdinaryRouterCoordinator::try_new(
        owner,
        pool,
        OrdinaryRouterConfig::new(
            TxCompletionCode::new(0x401),
            TxCompletionCode::new(0x409),
            TxCompletionCode::new(0x40a),
        ),
    )
    .unwrap_or_else(|failure| panic!("coordinator must build: {:?}", failure.reason()))
}

fn properties(config: u32) -> InterfaceProperties {
    InterfaceProperties::new(
        LogicalMtu::try_new(500).expect("test MTU must be nonzero"),
        InterfaceConfigId::new(config),
        None,
        InterfaceCost::new(0),
    )
}

fn configure_router<const DEPTH: usize>() -> (
    OutboundRouter<NoopRawMutex, 2, DEPTH>,
    [reticulum_interface_router::InterfaceActorHandoff<NoopRawMutex, DEPTH>; 2],
    [InterfaceDescriptor; 2],
) {
    let fabric = Box::leak(Box::new(InterfaceFabric::new()));
    let (mut router, actors) = fabric.split();
    let first = router
        .register(
            actors[0].queue_id(),
            PacketInterfaceId::new(2),
            properties(2),
            true,
        )
        .expect("first interface must register");
    let second = router
        .register(
            actors[1].queue_id(),
            PacketInterfaceId::new(9),
            properties(9),
            true,
        )
        .expect("second interface must register");
    (router, actors, [first, second])
}

fn announce_actions(node: &mut TestNode, now: u64, rng: &mut CounterRng) -> NodeActions {
    node.queue_announce(None, AnnounceEmissionTime::new(now).unwrap(), rng)
        .expect("bounded announce queue must accept one item");
    let actions = node.flush_announces(MonotonicSeconds::new(now), rng);
    assert_eq!(actions.packets.len(), 1);
    actions
}

fn standalone_announce_actions(tag: u8, rng: &mut CounterRng) -> NodeActions {
    let mut source = node(tag);
    announce_actions(&mut source, 10, rng)
}

fn rns_node(tag: u8) -> EmbeddedNode<4, 4, 8, 2> {
    EmbeddedNode::new(
        reticulum_rns_rete::identity_from_private_key(&[tag; 64])
            .expect("RNS test identity must be valid"),
        "reticulum",
        &["ordinary-router-source-only"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("RNS test node must construct")
}

fn source_only_actions(source_interface: u8, rng: &mut CounterRng) -> NodeActions {
    let mut sender = rns_node(71);
    let mut receiver = rns_node(72);
    receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
    receiver
        .queue_announce(None, 1, rng)
        .expect("receiver announce must queue");
    let announce = receiver.flush_announces(1, rng);
    sender.ingest(announce[0].bytes(), 1, RnsInterfaceId(2), rng);
    let mut encoded = [0u8; PACKET_CAPACITY];
    let prepared = sender
        .prepare_data_into(
            &receiver.destination_hash(),
            b"ordinary router source-only proof",
            2,
            rng,
            &mut encoded,
        )
        .expect("test DATA packet must prepare");
    receiver
        .ingest(
            &encoded[..usize::from(prepared.packet_len())],
            2,
            RnsInterfaceId(source_interface),
            rng,
        )
        .actions
}

fn admission(_now: u64) -> OrdinaryRouterAdmission {
    OrdinaryRouterAdmission::new(TxLeaseDeadline::new(MonotonicMillis::new(100_000)))
}

fn route_one_announce<const BUFFERS: usize, const SLOTS: usize, const DEPTH: usize>(
    coordinator: &mut OrdinaryRouterCoordinator<BUFFERS>,
    router: &mut OutboundRouter<NoopRawMutex, SLOTS, DEPTH>,
    actions: NodeActions,
) {
    coordinator
        .try_offer_actions(actions, admission(10))
        .unwrap_or_else(|failure| panic!("actions must be accepted: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::ActionsAdmitted { packet_count: 1 }
    );
    assert_eq!(
        coordinator.step(router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::NonPacketActionsReady
    );
    let output = coordinator
        .take_non_packet_actions()
        .expect("non-packet output must remain explicit");
    assert!(output.events.is_empty());
    assert!(output.packets.is_empty());
    assert_eq!(output.unroutable_packets, 0);
    assert!(matches!(
        coordinator.step(router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::PacketStaged { .. }
    ));
    assert!(matches!(
        coordinator.step(router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::Routed {
            interface,
            ..
        } if interface == PacketInterfaceId::new(2)
    ));
}

#[test]
fn routed_protocol_metadata_marks_only_the_first_serialized_fanout_hop_required() {
    let mut owner_node = node(73);
    let mut coordinator = coordinator::<1>(&mut owner_node);
    let (mut router, mut actors, _) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let mut initiator = rns_node(74);
    let responder = rns_node(75);
    let (packet, _) = initiator
        .initiate_link(responder.destination_hash(), 1, &mut rng)
        .expect("test LINKREQUEST must construct");
    let token = packet
        .protocol_token()
        .expect("LINKREQUEST must carry a timing token");
    let mut actions = NodeActions::default();
    actions.packets.push(packet);

    coordinator
        .try_offer_actions(actions, admission(10))
        .unwrap_or_else(|failure| {
            panic!(
                "coordinator must accept one LINKREQUEST: {:?}",
                failure.reason()
            )
        });
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::ActionsAdmitted { packet_count: 1 }
    );
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::NonPacketActionsReady
    );
    let _ = coordinator.take_non_packet_actions().unwrap();
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::PacketStaged { .. }
    ));
    let first_dispatch_token = match coordinator.step(&mut router, MonotonicMillis::new(10)) {
        OrdinaryRouterStep::Routed {
            token: dispatch_token,
            interface,
            protocol_token: Some(actual),
            first_dispatch: true,
            ..
        } if interface == PacketInterfaceId::new(2) && actual == token => dispatch_token,
        other => panic!("first LINKREQUEST dispatch was not correlated: {other:?}"),
    };

    let InterfaceTxJob::Ordinary(first) = actors[0]
        .try_receive_job()
        .expect("first interface must receive the LINKREQUEST")
    else {
        panic!("LINKREQUEST changed owner family")
    };
    let (ticket, first) = first.into_parts();
    let completion = ticket
        .complete(
            first
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x420)),
        )
        .expect("ticket must bind the first fan-out hop");
    actors[0]
        .try_send_completion(completion)
        .expect("first completion queue must accept the owner");
    let OutboundCompletion::Ordinary(completion) = router
        .try_receive_completion()
        .expect("router completion read must succeed")
        .expect("first completion must be ready")
    else {
        panic!("LINKREQUEST completion changed owner family")
    };
    coordinator
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| {
            panic!("first completion must correlate: {:?}", failure.reason())
        });
    match coordinator.step(&mut router, MonotonicMillis::new(20)) {
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Next {
            interface,
            completed,
            ..
        }) if interface == PacketInterfaceId::new(9) => {
            assert_eq!(completed.token(), first_dispatch_token);
            assert_eq!(completed.interface(), PacketInterfaceId::new(2));
            assert_eq!(
                completed.transmission(),
                OrdinaryTransmissionOutcome::NotConfirmed
            );
        }
        other => panic!("first unconfirmed hop did not advance exactly: {other:?}"),
    }
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Routed {
            interface,
            protocol_token: Some(actual),
            first_dispatch: false,
            ..
        } if interface == PacketInterfaceId::new(9) && actual == token
    ));
}

#[test]
fn rejected_route_does_not_consume_the_first_successful_dispatch_edge() {
    let mut owner_node = node(76);
    let mut coordinator = coordinator::<1>(&mut owner_node);
    let (mut router, _actors, descriptors) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let mut initiator = rns_node(77);
    let responder = rns_node(78);
    let (packet, _) = initiator
        .initiate_link(responder.destination_hash(), 1, &mut rng)
        .expect("test LINKREQUEST must construct");
    let token = packet.protocol_token().unwrap();
    let mut actions = NodeActions::default();
    actions.packets.push(packet);

    coordinator
        .try_offer_actions(actions, admission(10))
        .unwrap_or_else(|failure| panic!("offer failed: {:?}", failure.reason()));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::ActionsAdmitted { .. }
    ));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::NonPacketActionsReady
    );
    let _ = coordinator.take_non_packet_actions().unwrap();
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::PacketStaged { .. }
    ));
    router
        .set_online(descriptors[0].lease(), false)
        .expect("first route must go offline after admission");
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::RouteRejected { .. }
    ));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(11)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Next {
            interface,
            ..
        }) if interface == PacketInterfaceId::new(9)
    ));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(11)),
        OrdinaryRouterStep::Routed {
            interface,
            protocol_token: Some(actual),
            first_dispatch: true,
            ..
        } if interface == PacketInterfaceId::new(9) && actual == token
    ));
}

struct Allow;

impl TxAuthorizationPolicy for Allow {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        TxPolicyDecision::Authorize(
            TxPermitReservation::try_new(
                candidate.requirements.resource(),
                candidate.requirements.required_units(),
            )
            .expect("test policy must cover valid nonzero requirements"),
        )
    }
}

#[test]
fn all_fanout_survives_stale_router_lease_and_returns_exact_pool_owner() {
    let mut node = node(11);
    let mut coordinator = coordinator::<1>(&mut node);
    let (mut router, mut actors, descriptors) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let actions = announce_actions(&mut node, 10, &mut rng);
    route_one_announce(&mut coordinator, &mut router, actions);

    let InterfaceTxJob::Ordinary(first) = actors[0]
        .try_receive_job()
        .expect("first interface must receive the All hop")
    else {
        panic!("ordinary route changed owner family")
    };
    let (first_ticket, first) = first.into_parts();
    let expected_slot = first.slot_id();
    let completion = first_ticket
        .complete(
            first
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x402)),
        )
        .unwrap_or_else(|_| panic!("ticket must bind the exact first-hop owner"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("first completion must fit"));

    router
        .reconfigure(descriptors[0].lease(), properties(22), true)
        .expect("test reconfiguration must supersede the first lease");
    let recovery = router
        .try_receive_completion()
        .expect_err("superseded completion must use explicit recovery")
        .into_node_recovery();
    let OutboundCompletion::Ordinary(first_completion) = recovery.into_outbound() else {
        panic!("stale ordinary completion changed owner family")
    };
    coordinator
        .try_accept_completion(first_completion)
        .unwrap_or_else(|failure| {
            panic!(
                "stale-lease recovery must reconcile: {:?}",
                failure.reason()
            )
        });
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Next {
            slot,
            interface,
            ..
        }) if slot == expected_slot && interface == PacketInterfaceId::new(9)
    ));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Routed {
            slot,
            interface,
            protocol_token: None,
            first_dispatch: false,
            ..
        } if slot == expected_slot && interface == PacketInterfaceId::new(9)
    ));

    let InterfaceTxJob::Ordinary(second) = actors[1]
        .try_receive_job()
        .expect("second interface must receive serialized Next")
    else {
        panic!("ordinary Next changed owner family")
    };
    let (second_ticket, second) = second.into_parts();
    assert_eq!(second.slot_id(), expected_slot);
    let completion = second_ticket
        .complete(
            second
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x403)),
        )
        .unwrap_or_else(|_| panic!("ticket must bind the exact second-hop owner"));
    actors[1]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("second completion must fit"));
    let Some(OutboundCompletion::Ordinary(second_completion)) = router
        .try_receive_completion()
        .expect("current second lease must route completion")
    else {
        panic!("second ordinary completion was not returned")
    };
    coordinator
        .try_accept_completion(second_completion)
        .unwrap_or_else(|failure| {
            panic!("second completion must be accepted: {:?}", failure.reason())
        });
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot,
            ..
        }) if slot == expected_slot
    ));
    assert_eq!(coordinator.parked_count(), 1);
    assert_eq!(coordinator.active_count(), 0);
    assert_eq!(coordinator.capacities().active, 0);
    assert_eq!(coordinator.next_deadline(), None);
    assert_eq!(coordinator.fault(), None);
}

#[test]
fn active_owner_backpressures_and_then_admits_the_exact_next_envelope() {
    let mut node = node(21);
    let mut coordinator = coordinator::<1>(&mut node);
    let (mut router, mut actors, _) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let first_actions = announce_actions(&mut node, 10, &mut rng);
    route_one_announce(&mut coordinator, &mut router, first_actions);

    let second_actions = standalone_announce_actions(22, &mut rng);
    coordinator
        .try_offer_actions(second_actions, admission(20))
        .unwrap_or_else(|failure| panic!("empty input boundary rejected: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::AdmissionBackpressured {
            needed: 1,
            available: 0,
        }
    );

    let InterfaceTxJob::Ordinary(first) = actors[0]
        .try_receive_job()
        .expect("first active owner must remain in the actor queue")
    else {
        panic!("ordinary route changed owner family")
    };
    let (ticket, first) = first.into_parts();
    let completion = ticket
        .complete(first.cancel(TxCompletionCode::new(0x404)))
        .unwrap_or_else(|_| panic!("first ticket must remain exact"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("first completion must fit"));
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("current completion must route")
    else {
        panic!("ordinary completion changed owner family")
    };
    coordinator
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| panic!("completion must fit: {:?}", failure.reason()));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { .. })
    ));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::ActionsAdmitted { packet_count: 1 }
    );
    assert_eq!(coordinator.fault(), None);
}

#[test]
fn busy_offer_and_incomplete_build_return_every_input_unchanged() {
    let mut first_node = node(31);
    let mut coordinator = coordinator::<1>(&mut first_node);
    let mut rng = CounterRng::default();
    let first = announce_actions(&mut first_node, 10, &mut rng);
    coordinator
        .try_offer_actions(first, admission(10))
        .unwrap_or_else(|failure| panic!("first offer failed: {:?}", failure.reason()));
    let second = standalone_announce_actions(33, &mut rng);
    let pointer = second.packets.as_ptr();
    let failure = coordinator
        .try_offer_actions(second, admission(20))
        .expect_err("pending input must reject a second envelope");
    assert_eq!(
        failure.reason(),
        OrdinaryRouterOfferError::Busy(OrdinaryRouterBusyReason::PendingActions)
    );
    let (second, request) = failure.into_parts();
    assert_eq!(second.packets.as_ptr(), pointer);
    assert_eq!(request, admission(20));

    let mut other = node(32);
    let mut owner = other
        .take_ordinary_action_owner::<1>()
        .expect("ordinary owner must be issued");
    let buffer = Box::leak(Box::new(OrdinaryPacketBuffer::new()));
    owner
        .register_packet_buffer(buffer)
        .expect("buffer must register without parking");
    let failure = OrdinaryRouterCoordinator::try_new(
        owner,
        OrdinaryBufferPool::new(),
        OrdinaryRouterConfig::new(
            TxCompletionCode::new(1),
            TxCompletionCode::new(2),
            TxCompletionCode::new(3),
        ),
    )
    .err()
    .expect("missing parked owner must fail construction");
    assert_eq!(
        failure.reason(),
        OrdinaryRouterBuildError::ParkingIncomplete {
            expected: 1,
            parked: 0,
        }
    );
    let (_owner, pool, config) = failure.into_parts();
    assert_eq!(pool.parked_count(), 0);
    assert_eq!(config.route_rejected_code(), TxCompletionCode::new(1));
    assert_eq!(config.deadline_expired_code(), TxCompletionCode::new(2));
    assert_eq!(config.fault_drain_code(), TxCompletionCode::new(3));
}

#[test]
fn full_actor_queue_retains_the_second_job_until_capacity_returns() {
    let mut node = node(41);
    let mut coordinator = coordinator::<2>(&mut node);
    let (mut router, mut actors, _) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let mut actions = standalone_announce_actions(42, &mut rng);
    let mut second = standalone_announce_actions(43, &mut rng);
    actions.packets.append(&mut second.packets);
    assert_eq!(actions.packets.len(), 2);

    coordinator
        .try_offer_actions(actions, admission(10))
        .unwrap_or_else(|failure| panic!("two-packet batch failed: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::ActionsAdmitted { packet_count: 2 }
    );
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::NonPacketActionsReady
    );
    let _ = coordinator
        .take_non_packet_actions()
        .expect("non-packet output must remain explicit");
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::PacketStaged { .. }
    ));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::Routed { .. }
    ));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::PacketStaged { .. }
    ));
    let pressure = coordinator.step(&mut router, MonotonicMillis::new(10));
    assert!(matches!(
        pressure,
        OrdinaryRouterStep::RouteBackpressured {
            interface,
            ..
        } if interface == PacketInterfaceId::new(2)
    ));
    assert!(!pressure.progressed());

    let InterfaceTxJob::Ordinary(first) = actors[0]
        .try_receive_job()
        .expect("first queued owner must remain exact")
    else {
        panic!("first owner changed family")
    };
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(11)),
        OrdinaryRouterStep::Routed { .. }
    ));
    let InterfaceTxJob::Ordinary(second) = actors[0]
        .try_receive_job()
        .expect("retained second owner must route after capacity returns")
    else {
        panic!("second owner changed family")
    };
    assert_eq!(
        first.context().lease().interface(),
        PacketInterfaceId::new(2)
    );
    assert_eq!(
        second.context().lease().interface(),
        PacketInterfaceId::new(2)
    );
    let (first_ticket, first) = first.into_parts();
    let (second_ticket, second) = second.into_parts();
    let first_slot = first.slot_id();
    let second_slot = second.slot_id();
    assert_ne!(first_slot, second_slot);
    assert!(first.generation() < second.generation());

    let first_completion = first_ticket
        .complete(first.cancel(TxCompletionCode::new(0x405)))
        .unwrap_or_else(|_| panic!("first ticket must remain exact"));
    actors[0]
        .try_send_completion(first_completion)
        .unwrap_or_else(|_| panic!("first completion must fit"));
    let Some(OutboundCompletion::Ordinary(first_completion)) = router
        .try_receive_completion()
        .expect("first current completion must route")
    else {
        panic!("first completion changed family")
    };
    coordinator
        .try_accept_completion(first_completion)
        .unwrap_or_else(|failure| panic!("first completion must fit: {:?}", failure.reason()));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot,
            ..
        }) if slot == first_slot
    ));

    let second_completion = second_ticket
        .complete(second.cancel(TxCompletionCode::new(0x406)))
        .unwrap_or_else(|_| panic!("second ticket must remain exact"));
    actors[0]
        .try_send_completion(second_completion)
        .unwrap_or_else(|_| panic!("second completion must fit"));
    let Some(OutboundCompletion::Ordinary(second_completion)) = router
        .try_receive_completion()
        .expect("second current completion must route")
    else {
        panic!("second completion changed family")
    };
    coordinator
        .try_accept_completion(second_completion)
        .unwrap_or_else(|failure| panic!("second completion must fit: {:?}", failure.reason()));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot,
            ..
        }) if slot == second_slot
    ));
    assert_eq!(coordinator.parked_count(), 2);
    assert_eq!(coordinator.active_count(), 0);
}

#[test]
fn full_lora_queue_does_not_starve_an_independent_other_interface_packet() {
    let (mut router, mut actors, _) = configure_router::<1>();
    let mut rng = CounterRng::default();

    let mut blocker_node = node(44);
    let mut blocker = coordinator::<1>(&mut blocker_node);
    let blocker_actions = announce_actions(&mut blocker_node, 10, &mut rng);
    route_one_announce(&mut blocker, &mut router, blocker_actions);
    assert_eq!(actors[0].pending_jobs(), 1);

    let mut primary_node = node(45);
    let mut primary = coordinator::<2>(&mut primary_node);
    let mut actions = standalone_announce_actions(46, &mut rng);
    let mut only_second = source_only_actions(9, &mut rng);
    assert_eq!(only_second.packets.len(), 1);
    actions.packets.append(&mut only_second.packets);
    primary
        .try_offer_actions(actions, admission(10))
        .unwrap_or_else(|failure| panic!("mixed batch failed: {:?}", failure.reason()));
    assert_eq!(
        primary.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::ActionsAdmitted { packet_count: 2 }
    );
    assert_eq!(
        primary.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::NonPacketActionsReady
    );
    let _ = primary
        .take_non_packet_actions()
        .expect("mixed batch output must remain explicit");
    assert!(matches!(
        primary.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::PacketStaged { .. }
    ));

    // The All packet selects interface 2, whose queue is full. The same
    // bounded step must continue and stage the independent Only(9) owner
    // instead of returning pressure and starving another transport.
    assert!(matches!(
        primary.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::PacketStaged { .. }
    ));
    let second_route = primary.step(&mut router, MonotonicMillis::new(10));
    assert!(matches!(
        second_route,
        OrdinaryRouterStep::Routed {
            interface,
            ..
        } if interface == PacketInterfaceId::new(9)
    ));
    let InterfaceTxJob::Ordinary(primary_second) = actors[1]
        .try_receive_job()
        .expect("independent interface must receive its owner")
    else {
        panic!("independent owner changed family")
    };
    let (ticket, primary_second) = primary_second.into_parts();
    let second_slot = primary_second.slot_id();
    let completion = ticket
        .complete(primary_second.cancel(TxCompletionCode::new(0x40e)))
        .unwrap_or_else(|_| panic!("independent ticket must remain exact"));
    actors[1]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("independent completion must fit"));
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("independent completion must route")
    else {
        panic!("independent completion changed family")
    };
    primary
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| panic!("independent completion failed: {:?}", failure.reason()));
    assert!(matches!(
        primary.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot,
            ..
        }) if slot == second_slot
    ));

    let InterfaceTxJob::Ordinary(blocking_job) = actors[0]
        .try_receive_job()
        .expect("blocker owner must remain exact")
    else {
        panic!("blocker owner changed family")
    };
    let (ticket, blocking_job) = blocking_job.into_parts();
    let blocking_slot = blocking_job.slot_id();
    let completion = ticket
        .complete(blocking_job.cancel(TxCompletionCode::new(0x40f)))
        .unwrap_or_else(|_| panic!("blocker ticket must remain exact"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("blocker completion must fit"));
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("blocker completion must route")
    else {
        panic!("blocker completion changed family")
    };
    blocker
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| panic!("blocker completion failed: {:?}", failure.reason()));
    assert!(matches!(
        blocker.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot,
            ..
        }) if slot == blocking_slot
    ));

    let first_route = primary.step(&mut router, MonotonicMillis::new(20));
    assert!(matches!(
        first_route,
        OrdinaryRouterStep::Routed {
            interface,
            ..
        } if interface == PacketInterfaceId::new(2)
    ));
    let InterfaceTxJob::Ordinary(primary_first) = actors[0]
        .try_receive_job()
        .expect("previously pressured LoRa owner must route later")
    else {
        panic!("LoRa owner changed family")
    };
    let (ticket, primary_first) = primary_first.into_parts();
    let first_slot = primary_first.slot_id();
    let completion = ticket
        .complete(primary_first.cancel(TxCompletionCode::new(0x410)))
        .unwrap_or_else(|_| panic!("LoRa ticket must remain exact"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("LoRa completion must fit"));
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("LoRa completion must route")
    else {
        panic!("LoRa completion changed family")
    };
    primary
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| panic!("LoRa completion failed: {:?}", failure.reason()));
    assert!(matches!(
        primary.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot,
            ..
        }) if slot == first_slot
    ));
    assert_eq!(primary.parked_count(), 2);
    assert_eq!(blocker.parked_count(), 1);
    assert_eq!(primary.fault(), None);
    assert_eq!(blocker.fault(), None);
}

#[test]
fn oversized_atomic_envelope_is_returned_without_poisoning_active_work() {
    let mut node = node(51);
    let mut coordinator = coordinator::<1>(&mut node);
    let mut rng = CounterRng::default();
    let mut actions = standalone_announce_actions(52, &mut rng);
    let mut second = standalone_announce_actions(53, &mut rng);
    actions.packets.append(&mut second.packets);
    let pointer = actions.packets.as_ptr();
    let failure = coordinator
        .try_offer_actions(actions, admission(10))
        .expect_err("oversized atomic input must return unchanged");
    assert_eq!(
        failure.reason(),
        OrdinaryRouterOfferError::EnvelopeExceedsPool {
            packet_count: 2,
            limit: 1,
        }
    );
    let (actions, returned_admission) = failure.into_parts();
    assert_eq!(actions.packets.as_ptr(), pointer);
    assert_eq!(returned_admission, admission(10));
    assert_eq!(coordinator.fault(), None);
    assert_eq!(coordinator.parked_count(), 1);
}

#[test]
fn permit_authorization_exposes_bytes_and_returns_through_the_real_ticket() {
    let mut node = node(54);
    let mut coordinator = coordinator::<1>(&mut node);
    let (mut router, mut actors, _) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let actions = source_only_actions(2, &mut rng);
    coordinator
        .try_offer_actions(actions, admission(10))
        .unwrap_or_else(|failure| panic!("source-only proof: {:?}", failure.reason()));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::ActionsAdmitted { packet_count: 1 }
    ));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::NonPacketActionsReady
    );
    let _ = coordinator
        .take_non_packet_actions()
        .expect("proof event output must remain explicit");
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::PacketStaged { .. }
    ));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::Routed {
            interface,
            ..
        } if interface == PacketInterfaceId::new(2)
    ));

    let InterfaceTxJob::Ordinary(job) = actors[0]
        .try_receive_job()
        .expect("actor must receive the routed owner")
    else {
        panic!("ordinary owner changed family")
    };
    let (ticket, job) = job.into_parts();
    let slot = job.slot_id();
    let dispatch_token = job.prepared().dispatch_token();
    let requirements = TxPermitRequirements::try_new(TxPermitResourceId::new([0x54; 16]), 1)
        .expect("test requirements must be nonzero");
    let (pending, request) = job.begin_permit(requirements);
    let reply = coordinator
        .authorize_tx(request, MonotonicMillis::new(20), &mut Allow)
        .unwrap_or_else(|failure| panic!("permit request must authorize: {:?}", failure.reason()));
    let mut authorized = match pending
        .resolve(reply, MonotonicMillis::new(20))
        .unwrap_or_else(|_| panic!("permit reply must match its exact pending owner"))
    {
        OrdinaryPermitResolution::Authorized(authorized) => authorized,
        OrdinaryPermitResolution::Expired(_) => panic!("fresh grant unexpectedly expired"),
        OrdinaryPermitResolution::Unpermitted(_) => panic!("allow policy denied"),
    };
    {
        let frame = authorized
            .frame(MonotonicMillis::new(20))
            .expect("authorized owner must expose its complete native frame once");
        assert!(!frame.bytes().is_empty());
        assert_eq!(frame.prepared().slot_id(), slot);
    }
    let completion = ticket
        .complete(authorized.complete_transmitted(TxCompletionCode::new(0x40b)))
        .unwrap_or_else(|_| panic!("authorized completion must match its ticket"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("authorized completion must fit"));
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("authorized completion lease must remain current")
    else {
        panic!("authorized completion changed family")
    };
    coordinator
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| {
            panic!(
                "authorized completion must reconcile: {:?}",
                failure.reason()
            )
        });
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot: returned_slot,
            completed,
        }) if returned_slot == slot
            && completed.token() == dispatch_token
            && completed.interface() == PacketInterfaceId::new(2)
            && completed.transmission() == OrdinaryTransmissionOutcome::Transmitted
    ));
    assert_eq!(coordinator.parked_count(), 1);
    assert_eq!(coordinator.fault(), None);
}

#[test]
fn locally_retained_job_cancels_at_its_fresh_deadline() {
    let mut node = node(55);
    let mut coordinator = coordinator::<1>(&mut node);
    let (mut router, mut actors, _) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let actions = announce_actions(&mut node, 10, &mut rng);
    let deadline = TxLeaseDeadline::new(MonotonicMillis::new(15));
    coordinator
        .try_offer_actions(actions, OrdinaryRouterAdmission::new(deadline))
        .unwrap_or_else(|failure| panic!("actions failed: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::ActionsAdmitted { packet_count: 1 }
    );
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(10)),
        OrdinaryRouterStep::NonPacketActionsReady
    );
    let _ = coordinator
        .take_non_packet_actions()
        .expect("non-packet output must remain explicit");
    let OrdinaryRouterStep::PacketStaged { slot, .. } =
        coordinator.step(&mut router, MonotonicMillis::new(10))
    else {
        panic!("admitted owner was not staged")
    };
    assert_eq!(coordinator.next_deadline(), Some(deadline));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(15)),
        OrdinaryRouterStep::PendingJobExpired { slot }
    );
    assert!(actors[0].try_receive_job().is_none());
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(15)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot: returned_slot,
            ..
        }) if returned_slot == slot
    ));
    assert_eq!(coordinator.parked_count(), 1);
    assert_eq!(coordinator.next_deadline(), None);
}

#[test]
fn backpressured_envelope_uses_step_time_and_is_rejected_after_deadline() {
    let mut node = node(56);
    let mut coordinator = coordinator::<1>(&mut node);
    let (mut router, mut actors, _) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let first_actions = announce_actions(&mut node, 10, &mut rng);
    route_one_announce(&mut coordinator, &mut router, first_actions);

    let second_actions = standalone_announce_actions(57, &mut rng);
    let second_pointer = second_actions.packets.as_ptr();
    let deadline = TxLeaseDeadline::new(MonotonicMillis::new(25));
    coordinator
        .try_offer_actions(second_actions, OrdinaryRouterAdmission::new(deadline))
        .unwrap_or_else(|failure| panic!("second actions failed: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::AdmissionBackpressured {
            needed: 1,
            available: 0,
        }
    );

    let InterfaceTxJob::Ordinary(first) = actors[0]
        .try_receive_job()
        .expect("active first owner must remain queued")
    else {
        panic!("first owner changed family")
    };
    let (ticket, first) = first.into_parts();
    let completion = ticket
        .complete(first.cancel(TxCompletionCode::new(0x40c)))
        .unwrap_or_else(|_| panic!("first ticket must remain exact"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("first completion must fit"));
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("first completion must route")
    else {
        panic!("first completion changed family")
    };
    coordinator
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| panic!("first completion failed: {:?}", failure.reason()));
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { .. })
    ));
    let reason = OrdinaryActionAdmissionError::DeadlineExpired {
        now: MonotonicMillis::new(30),
        deadline,
    };
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::ActionsRejected { reason }
    );
    let rejected = coordinator
        .take_rejected_actions()
        .expect("expired envelope must remain explicit output");
    assert_eq!(rejected.reason(), reason);
    let (actions, returned_admission) = rejected.into_parts();
    assert_eq!(actions.packets.as_ptr(), second_pointer);
    assert_eq!(returned_admission.deadline(), deadline);
    assert_eq!(coordinator.fault(), None);
    assert_eq!(coordinator.parked_count(), 1);
}

#[test]
fn registry_fault_stops_intake_but_still_drains_an_active_actor_owner() {
    let mut node = node(58);
    let mut coordinator = coordinator::<2>(&mut node);
    let fabric = Box::leak(Box::new(InterfaceFabric::<NoopRawMutex, 3, 1>::new()));
    let (mut router, mut actors) = fabric.split();
    router
        .register(
            actors[0].queue_id(),
            PacketInterfaceId::new(2),
            properties(2),
            true,
        )
        .expect("first interface must register");
    router
        .register(
            actors[1].queue_id(),
            PacketInterfaceId::new(9),
            properties(9),
            true,
        )
        .expect("second interface must register");
    let mut rng = CounterRng::default();
    let first_actions = announce_actions(&mut node, 10, &mut rng);
    route_one_announce(&mut coordinator, &mut router, first_actions);

    router
        .register(
            actors[2].queue_id(),
            PacketInterfaceId::new(64),
            properties(64),
            true,
        )
        .expect("out-of-profile identity remains registrable");
    let second_actions = standalone_announce_actions(59, &mut rng);
    coordinator
        .try_offer_actions(second_actions, admission(20))
        .unwrap_or_else(|failure| panic!("second actions failed: {:?}", failure.reason()));
    let step = coordinator.step(&mut router, MonotonicMillis::new(20));
    assert!(matches!(
        step,
        OrdinaryRouterStep::Disabled(OrdinaryRouterFault::RegistryProfile(
            EligibleInterfaceSetError::InterfaceOutsideProfile(_)
        ))
    ));
    assert_eq!(coordinator.active_count(), 1);
    assert_eq!(coordinator.fault_residue_count(), 1);

    let InterfaceTxJob::Ordinary(first) = actors[0]
        .try_receive_job()
        .expect("active owner must remain in the actor queue")
    else {
        panic!("active owner changed family")
    };
    let (ticket, first) = first.into_parts();
    let slot = first.slot_id();
    let requirements = TxPermitRequirements::try_new(TxPermitResourceId::new([0x58; 16]), 1)
        .expect("fault-drain requirements must be nonzero");
    let (pending, request) = first.begin_permit(requirements);
    let reply = coordinator
        .authorize_tx(request, MonotonicMillis::new(25), &mut Allow)
        .unwrap_or_else(|failure| {
            panic!(
                "fault-drain request must receive a denial: {:?}",
                failure.reason()
            )
        });
    let first = match pending
        .resolve(reply, MonotonicMillis::new(25))
        .unwrap_or_else(|_| panic!("fault-drain denial must match its pending owner"))
    {
        OrdinaryPermitResolution::Unpermitted(owner) => owner,
        OrdinaryPermitResolution::Authorized(_) => {
            panic!("faulted coordinator exposed a fresh authorization")
        }
        OrdinaryPermitResolution::Expired(_) => panic!("fault-drain request expired"),
    };
    let completion = ticket
        .complete(first.cancel(TxCompletionCode::new(0x40d)))
        .unwrap_or_else(|_| panic!("active ticket must remain exact"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("active completion must fit"));
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("active completion must route despite registry profile fault")
    else {
        panic!("active completion changed family")
    };
    coordinator
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| {
            panic!(
                "faulted coordinator must drain active owner: {:?}",
                failure.reason()
            )
        });
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned {
            slot: returned_slot,
            ..
        }) if returned_slot == slot
    ));
    assert_eq!(coordinator.active_count(), 0);
    assert_eq!(coordinator.parked_count(), 2);
    assert_eq!(coordinator.fault_residue_count(), 1);
    assert!(matches!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::Disabled(OrdinaryRouterFault::RegistryProfile(_))
    ));
}

#[test]
fn actor_quarantine_retains_an_already_offered_envelope_as_fault_residue() {
    let mut node = node(60);
    let mut coordinator = coordinator::<2>(&mut node);
    let (mut router, mut actors, _) = configure_router::<1>();
    let mut rng = CounterRng::default();
    let first_actions = announce_actions(&mut node, 10, &mut rng);
    route_one_announce(&mut coordinator, &mut router, first_actions);

    let second_actions = standalone_announce_actions(61, &mut rng);
    coordinator
        .try_offer_actions(second_actions, admission(20))
        .unwrap_or_else(|failure| panic!("second actions failed: {:?}", failure.reason()));
    assert!(coordinator.next_deadline().is_some());

    let InterfaceTxJob::Ordinary(first) = actors[0]
        .try_receive_job()
        .expect("active owner must remain in the actor queue")
    else {
        panic!("active owner changed family")
    };
    let (ticket, first) = first.into_parts();
    let slot = first.slot_id();
    let recovery_code = TxCompletionCode::new(0x410);
    let completion = ticket
        .complete(first.recovery_fault(recovery_code))
        .unwrap_or_else(|_| panic!("active ticket must remain exact"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("active completion must fit"));
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("active completion must route")
    else {
        panic!("active completion changed family")
    };
    coordinator
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| panic!("active completion failed: {:?}", failure.reason()));

    let fault =
        OrdinaryRouterFault::Quarantine(OrdinaryQuarantineReason::RecoveryFault(recovery_code));
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::Disabled(fault)
    );
    assert_eq!(coordinator.fault_residue_count(), 1);
    assert_eq!(
        coordinator.slot_fault_residue_kind(slot),
        Some(OrdinaryRouterFaultResidueKind::Quarantine)
    );
    assert_eq!(
        coordinator.step(&mut router, MonotonicMillis::new(30)),
        OrdinaryRouterStep::PendingActionsRetainedForFault
    );
    assert_eq!(coordinator.fault(), Some(fault));
    assert_eq!(coordinator.fault_residue_count(), 2);
    assert_eq!(coordinator.next_deadline(), None);
    assert_eq!(coordinator.active_count(), 0);
    assert_eq!(coordinator.parked_count(), 1);
}

#[test]
fn crossed_completion_is_rejected_unchanged_and_both_owners_reconcile() {
    let mut first_node = node(61);
    let mut first_coordinator = coordinator::<1>(&mut first_node);
    let (mut first_router, mut first_actors, _) = configure_router::<1>();
    let mut first_rng = CounterRng::default();
    let first_actions = announce_actions(&mut first_node, 10, &mut first_rng);
    route_one_announce(&mut first_coordinator, &mut first_router, first_actions);

    let mut second_node = node(62);
    let mut second_coordinator = coordinator::<1>(&mut second_node);
    let (mut second_router, mut second_actors, _) = configure_router::<1>();
    let mut second_rng = CounterRng(100);
    let second_actions = announce_actions(&mut second_node, 10, &mut second_rng);
    route_one_announce(&mut second_coordinator, &mut second_router, second_actions);

    let InterfaceTxJob::Ordinary(first) = first_actors[0]
        .try_receive_job()
        .expect("first actor must hold one owner")
    else {
        panic!("first owner changed family")
    };
    let (first_ticket, first) = first.into_parts();
    let first_prepared = first.prepared();
    let first_completion = first_ticket
        .complete(first.cancel(TxCompletionCode::new(0x407)))
        .unwrap_or_else(|_| panic!("first ticket must remain exact"));
    first_actors[0]
        .try_send_completion(first_completion)
        .unwrap_or_else(|_| panic!("first completion must fit"));
    let Some(OutboundCompletion::Ordinary(first_completion)) = first_router
        .try_receive_completion()
        .expect("first completion must route")
    else {
        panic!("first completion changed family")
    };

    let InterfaceTxJob::Ordinary(second) = second_actors[0]
        .try_receive_job()
        .expect("second actor must hold one owner")
    else {
        panic!("second owner changed family")
    };
    let (second_ticket, second) = second.into_parts();
    let second_prepared = second.prepared();
    let second_completion = second_ticket
        .complete(second.cancel(TxCompletionCode::new(0x408)))
        .unwrap_or_else(|_| panic!("second ticket must remain exact"));
    second_actors[0]
        .try_send_completion(second_completion)
        .unwrap_or_else(|_| panic!("second completion must fit"));
    let Some(OutboundCompletion::Ordinary(second_completion)) = second_router
        .try_receive_completion()
        .expect("second completion must route")
    else {
        panic!("second completion changed family")
    };

    let crossed = second_coordinator
        .try_accept_completion(first_completion)
        .expect_err("foreign owner must be rejected unchanged");
    assert_eq!(
        crossed.reason(),
        OrdinaryCompletionAcceptError::OwnerMismatch {
            expected: second_prepared,
            supplied: first_prepared,
        }
    );
    first_coordinator
        .try_accept_completion(crossed.into_completion())
        .unwrap_or_else(|failure| {
            panic!("first owner must still reconcile: {:?}", failure.reason())
        });
    second_coordinator
        .try_accept_completion(second_completion)
        .unwrap_or_else(|failure| {
            panic!("second owner must still reconcile: {:?}", failure.reason())
        });
    assert!(matches!(
        first_coordinator.step(&mut first_router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { .. })
    ));
    assert!(matches!(
        second_coordinator.step(&mut second_router, MonotonicMillis::new(20)),
        OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { .. })
    ));
    assert_eq!(first_coordinator.parked_count(), 1);
    assert_eq!(second_coordinator.parked_count(), 1);
    assert_eq!(first_coordinator.fault(), None);
    assert_eq!(second_coordinator.fault(), None);
}
