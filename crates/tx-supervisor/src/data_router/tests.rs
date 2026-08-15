extern crate std;

use core::future::Future;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use reticulum_interface_router::{
    InterfaceActorHandoff, InterfaceConfigId, InterfaceCost, InterfaceDescriptor, InterfaceFabric,
    InterfaceProperties, InterfaceTxJob, LogicalMtu, OutboundCompletion,
};
use reticulum_node_core::{
    AttemptOutcome, DataReceiptKind, InboundProofPolicy, LinkHandle, LinkState,
    MAX_DIRECT_LXMF_WIRE, NodeConfig, NodeIdentity, NodeInstanceId, PermitResolution,
    TxPermitDenialReason, TxPermitRequirements, TxPermitResourceId, TxPolicyDecision,
};
use reticulum_rns_rete::{PacketType, parse_packet};
use std::boxed::Box;
use std::{pin::pin, task::Waker};

use super::*;

type TestNode<const BUFFERS: usize> = NodeCore<8, 2, 8, 2, BUFFERS>;

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

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid")
}

fn node<const BUFFERS: usize>(tag: u8, aspect: &str) -> TestNode<BUFFERS> {
    TestNode::new(
        identity(tag),
        "reticulum",
        &[aspect],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .expect("test node must construct")
}

fn sender<const BUFFERS: usize>(tag: u8) -> (TestNode<BUFFERS>, DestinationHash) {
    let receiver_tag = tag.wrapping_add(1);
    let receiver = node::<0>(receiver_tag, "data-router-receiver");
    let destination = receiver.destination_hash();
    let mut sender = node::<BUFFERS>(tag, "data-router-sender");
    sender
        .register_peer(
            &identity(receiver_tag),
            "reticulum",
            &["data-router-receiver"],
            MonotonicSeconds::new(0),
        )
        .expect("receiver identity must cache");
    (sender, destination)
}

fn coordinator<const BUFFERS: usize>(
    node: &mut TestNode<BUFFERS>,
) -> DataRouterCoordinator<BUFFERS> {
    let mut buffers: [&'static mut TxPacketBuffer; BUFFERS] =
        core::array::from_fn(|_| Box::leak(Box::new(TxPacketBuffer::new())));
    for buffer in &mut buffers {
        node.register_packet_buffer(buffer)
            .expect("static DATA buffer must register");
    }
    match DataRouterCoordinator::try_new(
        node,
        buffers,
        DataRouterConfig::new(
            TxCompletionCode::new(0x501),
            TxCompletionCode::new(0x502),
            TxCompletionCode::new(0x503),
        ),
    ) {
        Ok(coordinator) => coordinator,
        Err(failure) => panic!("DATA coordinator must build: {:?}", failure.reason()),
    }
}

fn properties(config: u32) -> InterfaceProperties {
    InterfaceProperties::new(
        LogicalMtu::try_new(500).expect("test MTU must be nonzero"),
        InterfaceConfigId::new(config),
        None,
        InterfaceCost::new(0),
    )
}

fn one_interface_router<const DEPTH: usize>(
    online: bool,
) -> (
    OutboundRouter<NoopRawMutex, 1, DEPTH>,
    InterfaceActorHandoff<NoopRawMutex, DEPTH>,
    InterfaceDescriptor,
) {
    let fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, DEPTH> =
        Box::leak(Box::new(InterfaceFabric::new()));
    let (mut router, [actor]) = fabric.split();
    let descriptor = router
        .register(
            actor.queue_id(),
            PacketInterfaceId::new(2),
            properties(2),
            online,
        )
        .expect("test interface must register");
    (router, actor, descriptor)
}

fn two_interface_router<const DEPTH: usize>() -> (
    OutboundRouter<NoopRawMutex, 2, DEPTH>,
    [InterfaceActorHandoff<NoopRawMutex, DEPTH>; 2],
    [InterfaceDescriptor; 2],
) {
    let fabric: &'static mut InterfaceFabric<NoopRawMutex, 2, DEPTH> =
        Box::leak(Box::new(InterfaceFabric::new()));
    let (mut router, actors) = fabric.split();
    let first = router
        .register(
            actors[0].queue_id(),
            PacketInterfaceId::new(2),
            properties(2),
            true,
        )
        .expect("first test interface must register");
    let second = router
        .register(
            actors[1].queue_id(),
            PacketInterfaceId::new(9),
            properties(9),
            true,
        )
        .expect("second test interface must register");
    (router, actors, [first, second])
}

fn prepare_request(
    destination: DestinationHash,
    plaintext: &'static [u8],
    deadline: u64,
) -> DataRouterPrepareRequest<'static> {
    DataRouterPrepareRequest {
        destination,
        plaintext,
        rns_now: MonotonicSeconds::new(1),
        deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline)),
    }
}

fn direct_lxmf_pair<const BUFFERS: usize>(
    tag: u8,
    bound_interface: PacketInterfaceId,
    rng: &mut CounterRng,
) -> (
    TestNode<BUFFERS>,
    TestNode<0>,
    DestinationHash,
    LinkHandle,
    [u8; MAX_DIRECT_LXMF_WIRE],
    usize,
) {
    let receiver_tag = tag.wrapping_add(1);
    let mut responder = node::<0>(receiver_tag, "direct-lxmf-responder");
    let destination = responder
        .register_inbound_single_destination("lxmf", &["delivery"])
        .expect("responder delivery destination must register");
    responder
        .set_destination_accepts_links(&destination, true)
        .expect("responder delivery destination must accept Links");
    responder
        .set_destination_inbound_proof_policy(&destination, InboundProofPolicy::Always)
        .expect("direct Link DATA must produce an explicit proof");

    let mut sender = node::<BUFFERS>(tag, "direct-lxmf-sender");
    sender
        .register_inbound_single_destination("lxmf", &["delivery"])
        .expect("sender delivery destination must register");
    sender
        .register_peer(
            &identity(receiver_tag),
            "lxmf",
            &["delivery"],
            MonotonicSeconds::new(99),
        )
        .expect("responder delivery identity must cache");

    let (request, link) = sender
        .initiate_link(&destination, MonotonicSeconds::new(100), rng)
        .expect("Link request must construct");
    assert_eq!(request.packets.len(), 1);
    let response = responder
        .ingest(
            request.packets[0].bytes(),
            MonotonicSeconds::new(100),
            PacketInterfaceId::new(7),
            rng,
        )
        .expect("Link request must enter the responder");
    let established = sender
        .ingest(
            response.actions.packets[0].bytes(),
            MonotonicSeconds::new(101),
            bound_interface,
            rng,
        )
        .expect("Link proof must enter the initiator");
    assert_eq!(sender.link_state(link), Some(LinkState::Active));
    responder
        .ingest(
            established.actions.packets[0].bytes(),
            MonotonicSeconds::new(102),
            PacketInterfaceId::new(7),
            rng,
        )
        .expect("Link RTT packet must enter the responder");

    let mut wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    let prepared = sender
        .prepare_basic_direct_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"supervisor",
            b"durable direct Link DATA",
            &mut wire,
        )
        .expect("complete direct LXMF wire must compose");
    let wire_len = usize::from(prepared.wire_len());
    (sender, responder, destination, link, wire, wire_len)
}

fn take_data_job<const DEPTH: usize>(
    actor: &mut InterfaceActorHandoff<NoopRawMutex, DEPTH>,
) -> reticulum_interface_router::InterfaceDataJob {
    match actor
        .try_receive_job()
        .expect("interface actor must receive one DATA job")
    {
        InterfaceTxJob::Data(job) => job,
        InterfaceTxJob::Ordinary(_) => panic!("DATA owner changed family"),
    }
}

fn receive_data_completion<const SLOTS: usize, const DEPTH: usize>(
    router: &mut OutboundRouter<NoopRawMutex, SLOTS, DEPTH>,
) -> TxCompletion<'static> {
    match router
        .try_receive_completion()
        .expect("current completion must route")
        .expect("one completion must be queued")
    {
        OutboundCompletion::Data(completion) => completion,
        OutboundCompletion::Ordinary(_) => panic!("DATA completion changed family"),
    }
}

#[test]
fn incomplete_construction_returns_the_exact_static_buffer_owner() {
    let node = node::<1>(5, "unregistered-data-buffer");
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    let pointer = buffer as *mut TxPacketBuffer;
    let failure = match DataRouterCoordinator::try_new(
        &node,
        [buffer],
        DataRouterConfig::new(
            TxCompletionCode::new(1),
            TxCompletionCode::new(2),
            TxCompletionCode::new(3),
        ),
    ) {
        Ok(_) => panic!("unregistered construction unexpectedly succeeded"),
        Err(failure) => failure,
    };
    assert_eq!(
        failure.reason(),
        DataRouterBuildError::RegistrationIncomplete {
            expected: 1,
            registered: 0,
        }
    );
    let (mut buffers, config) = failure.into_parts();
    let returned = buffers[0]
        .take()
        .expect("construction failure must retain the buffer");
    assert_eq!(returned as *mut TxPacketBuffer, pointer);
    assert_eq!(config.route_rejected_code(), TxCompletionCode::new(1));
    assert_eq!(config.deadline_expired_code(), TxCompletionCode::new(2));
    assert_eq!(config.fault_drain_code(), TxCompletionCode::new(3));
}

#[test]
fn direct_ticketed_route_returns_the_exact_buffer_to_its_stable_slot() {
    let (mut node, destination) = sender::<1>(10);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actor, _) = one_interface_router::<1>(true);
    let mut rng = CounterRng::default();

    let prepared = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"direct DATA route", 1_000),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("DATA preparation did not stage: {other:?}"),
    };
    assert_eq!(coordinator.pending_job_count(), 1);
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(11)),
        DataRouterStep::Routed(prepared)
    );

    let (ticket, job) = take_data_job(&mut actor).into_parts();
    assert_eq!(DataPreparedHop::from_job(&job), prepared);
    let completion = ticket
        .complete(
            job.return_unpermitted()
                .complete(TxCompletionCode::new(0x510)),
        )
        .unwrap_or_else(|_| panic!("ticket must bind the exact DATA owner"));
    actor
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("completion must fit"));
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| panic!("completion must be accepted: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(12)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(prepared.slot_id()))
    );
    assert_eq!(coordinator.parked_counts().available(), 1);
    assert_eq!(coordinator.completion_correlated_count(), 0);
    assert_eq!(coordinator.fault(), None);
}

#[test]
fn rehydrated_direct_lxmf_uses_only_the_link_bound_interface() {
    let mut rng = CounterRng::default();
    let (mut node, _responder, destination, link, wire, wire_len) =
        direct_lxmf_pair::<1>(12, PacketInterfaceId::new(9), &mut rng);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actors, descriptors) = two_interface_router::<1>();

    let prepared = match coordinator.try_prepare_rehydrated_direct_lxmf(
        &mut node,
        &router,
        link,
        DataRouterPrepareRequest {
            destination,
            plaintext: &wire[..wire_len],
            rns_now: MonotonicSeconds::new(110),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(1_000)),
        },
        MonotonicMillis::new(110),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("direct LXMF DATA did not prepare: {other:?}"),
    };
    assert_eq!(
        prepared.prepared().receipt_kind(),
        DataReceiptKind::LinkData
    );
    assert_eq!(prepared.interface(), descriptors[1].lease().interface());
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(111)),
        DataRouterStep::Routed(prepared)
    );
    assert!(actors[0].try_receive_job().is_none());

    let (ticket, job) = take_data_job(&mut actors[1]).into_parts();
    let completion = ticket
        .complete(
            job.return_unpermitted()
                .complete(TxCompletionCode::new(0x520)),
        )
        .expect("bound-interface ticket must retain the exact owner");
    actors[1]
        .try_send_completion(completion)
        .expect("direct DATA completion must fit");
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| {
            panic!(
                "direct DATA completion must correlate: {:?}",
                failure.reason()
            )
        });
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(112)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(prepared.slot_id()))
    );
}

#[test]
fn all_target_serializes_two_exact_interface_hops_before_buffer_reuse() {
    let (mut node, destination) = sender::<1>(15);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actors, _) = two_interface_router::<1>();
    let mut rng = CounterRng::default();

    let first = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"two-interface DATA fanout", 1_000),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("fanout preparation did not stage: {other:?}"),
    };
    assert_eq!(first.interface(), PacketInterfaceId::new(2));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(11)),
        DataRouterStep::Routed(first)
    );

    let (first_ticket, first_job) = take_data_job(&mut actors[0]).into_parts();
    assert_eq!(DataPreparedHop::from_job(&first_job), first);
    let first_completion = first_ticket
        .complete(
            first_job
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x516)),
        )
        .unwrap_or_else(|_| panic!("first fanout ticket must remain exact"));
    actors[0]
        .try_send_completion(first_completion)
        .unwrap_or_else(|_| panic!("first fanout completion must fit"));
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| panic!("first fanout completion failed: {:?}", failure.reason()));
    let second = match coordinator.step(&mut node, &mut router, MonotonicMillis::new(12)) {
        DataRouterStep::Completion(DataRouterCompletionProgress::Next(hop)) => hop,
        other => panic!("first fanout hop did not advance: {other:?}"),
    };
    assert_eq!(second.slot_id(), first.slot_id());
    assert_eq!(
        second.prepared().attempt(),
        first.prepared().attempt(),
        "serialized fanout must retain the exact DATA attempt"
    );
    assert_eq!(second.interface(), PacketInterfaceId::new(9));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(13)),
        DataRouterStep::Routed(second)
    );

    let (second_ticket, second_job) = take_data_job(&mut actors[1]).into_parts();
    assert_eq!(DataPreparedHop::from_job(&second_job), second);
    let second_completion = second_ticket
        .complete(
            second_job
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x517)),
        )
        .unwrap_or_else(|_| panic!("second fanout ticket must remain exact"));
    actors[1]
        .try_send_completion(second_completion)
        .unwrap_or_else(|_| panic!("second fanout completion must fit"));
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| {
            panic!("second fanout completion failed: {:?}", failure.reason())
        });
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(14)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(first.slot_id()))
    );
    assert_eq!(coordinator.parked_counts().available(), 1);
    assert_eq!(coordinator.completion_correlated_count(), 0);
}

#[test]
fn local_first_hop_registry_rejection_advances_the_same_attempt_to_second_interface() {
    let (mut node, destination) = sender::<1>(18);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actors, descriptors) = two_interface_router::<1>();
    let mut rng = CounterRng::default();

    let first = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"offline first fanout hop", 1_000),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("offline-hop preparation did not stage: {other:?}"),
    };
    router
        .set_online(descriptors[0].lease(), false)
        .expect("current first-hop lease must go offline");
    assert!(matches!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(11)),
        DataRouterStep::RouteRejected {
            hop,
            reason: RouteError::Offline(_),
        } if hop == first
    ));
    assert_eq!(actors[0].pending_jobs(), 0);
    assert_eq!(coordinator.completion_correlated_count(), 1);

    let second = match coordinator.step(&mut node, &mut router, MonotonicMillis::new(12)) {
        DataRouterStep::Completion(DataRouterCompletionProgress::Next(hop)) => hop,
        other => panic!("local rejection did not advance fanout: {other:?}"),
    };
    assert_eq!(second.slot_id(), first.slot_id());
    assert_eq!(second.prepared().attempt(), first.prepared().attempt());
    assert_eq!(second.interface(), PacketInterfaceId::new(9));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(13)),
        DataRouterStep::Routed(second)
    );

    let (ticket, job) = take_data_job(&mut actors[1]).into_parts();
    let completion = ticket
        .complete(
            job.return_unpermitted()
                .complete(TxCompletionCode::new(0x51a)),
        )
        .unwrap_or_else(|_| panic!("second-interface ticket must remain exact"));
    actors[1]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("second-interface completion must fit"));
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| {
            panic!("second-interface completion failed: {:?}", failure.reason())
        });
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(14)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(first.slot_id()))
    );
    assert_eq!(coordinator.parked_counts().available(), 1);
}

#[test]
fn full_actor_queue_retains_the_second_owner_until_capacity_returns() {
    let (mut node, destination) = sender::<2>(20);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actor, _) = one_interface_router::<1>(true);
    let mut rng = CounterRng::default();

    let first = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"first DATA owner", 1_000),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("first preparation failed: {other:?}"),
    };
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(10)),
        DataRouterStep::Routed(first)
    );
    let second = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"second DATA owner", 1_000),
        MonotonicMillis::new(11),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("second preparation failed: {other:?}"),
    };
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(11)),
        DataRouterStep::RouteBackpressured(second)
    );
    assert_eq!(coordinator.pending_job_count(), 1);

    {
        let mut wait = pin!(coordinator.wait_route_progress(&mut router));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
    }
    assert_eq!(
        coordinator.pending_job_count(),
        1,
        "cancelling a pending capacity wait must not move the DATA owner"
    );

    let (first_ticket, first_job) = take_data_job(&mut actor).into_parts();
    assert_eq!(DataPreparedHop::from_job(&first_job), first);
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(12)),
        DataRouterStep::Routed(second)
    );
    let (second_ticket, second_job) = take_data_job(&mut actor).into_parts();
    assert_eq!(DataPreparedHop::from_job(&second_job), second);

    let first_completion = first_ticket
        .complete(
            first_job
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x511)),
        )
        .unwrap_or_else(|_| panic!("first ticket must remain exact"));
    actor
        .try_send_completion(first_completion)
        .unwrap_or_else(|_| panic!("first completion must fit"));
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| panic!("first return failed: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(13)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(first.slot_id()))
    );

    let second_completion = second_ticket
        .complete(
            second_job
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x512)),
        )
        .unwrap_or_else(|_| panic!("second ticket must remain exact"));
    actor
        .try_send_completion(second_completion)
        .unwrap_or_else(|_| panic!("second completion must fit"));
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| panic!("second return failed: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(14)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(second.slot_id()))
    );
    assert_eq!(coordinator.parked_counts().available(), 2);
}

#[test]
fn direct_lxmf_backpressure_preserves_both_exact_packet_owners() {
    let mut rng = CounterRng::default();
    let (mut node, _responder, destination, link, wire, wire_len) =
        direct_lxmf_pair::<2>(22, PacketInterfaceId::new(9), &mut rng);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actors, _) = two_interface_router::<1>();
    let request = |rns_now: u64| DataRouterPrepareRequest {
        destination,
        plaintext: &wire[..wire_len],
        rns_now: MonotonicSeconds::new(rns_now),
        deadline: TxLeaseDeadline::new(MonotonicMillis::new(1_000)),
    };

    let first = match coordinator.try_prepare_rehydrated_direct_lxmf(
        &mut node,
        &router,
        link,
        request(110),
        MonotonicMillis::new(110),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("first direct owner did not prepare: {other:?}"),
    };
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(111)),
        DataRouterStep::Routed(first)
    );

    let second = match coordinator.try_prepare_rehydrated_direct_lxmf(
        &mut node,
        &router,
        link,
        request(112),
        MonotonicMillis::new(112),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("second direct owner did not prepare: {other:?}"),
    };
    assert_ne!(first.slot_id(), second.slot_id());
    assert_eq!(first.interface(), PacketInterfaceId::new(9));
    assert_eq!(second.interface(), PacketInterfaceId::new(9));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(113)),
        DataRouterStep::RouteBackpressured(second)
    );
    assert_eq!(coordinator.pending_job_count(), 1);
    assert_eq!(coordinator.parked_counts().available(), 0);
    assert!(actors[0].try_receive_job().is_none());

    let (first_ticket, first_job) = take_data_job(&mut actors[1]).into_parts();
    assert_eq!(DataPreparedHop::from_job(&first_job), first);
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(114)),
        DataRouterStep::Routed(second)
    );
    let (second_ticket, second_job) = take_data_job(&mut actors[1]).into_parts();
    assert_eq!(DataPreparedHop::from_job(&second_job), second);

    for (ticket, job, code, expected) in [
        (first_ticket, first_job, 0x521, first),
        (second_ticket, second_job, 0x522, second),
    ] {
        let completion = ticket
            .complete(
                job.return_unpermitted()
                    .complete(TxCompletionCode::new(code)),
            )
            .expect("direct ticket must retain its exact owner");
        actors[1]
            .try_send_completion(completion)
            .expect("direct completion must fit");
        coordinator
            .try_accept_completion(receive_data_completion(&mut router))
            .unwrap_or_else(|failure| {
                panic!("direct completion must correlate: {:?}", failure.reason())
            });
        assert_eq!(
            coordinator.step(&mut node, &mut router, MonotonicMillis::new(115)),
            DataRouterStep::Completion(DataRouterCompletionProgress::Available(expected.slot_id()))
        );
    }
    assert_eq!(coordinator.parked_counts().available(), 2);
}

#[test]
fn direct_lxmf_proof_terminates_the_shared_attempt_after_owner_return() {
    let mut rng = CounterRng::default();
    let (mut node, mut responder, destination, link, wire, wire_len) =
        direct_lxmf_pair::<1>(24, PacketInterfaceId::new(2), &mut rng);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actor, _) = one_interface_router::<1>(true);

    let prepared = match coordinator.try_prepare_rehydrated_direct_lxmf(
        &mut node,
        &router,
        link,
        DataRouterPrepareRequest {
            destination,
            plaintext: &wire[..wire_len],
            rns_now: MonotonicSeconds::new(110),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(1_000)),
        },
        MonotonicMillis::new(110),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("proof-bound direct DATA did not prepare: {other:?}"),
    };
    let attempt = prepared.prepared().handle();
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(111)),
        DataRouterStep::Routed(prepared)
    );
    let (ticket, job) = take_data_job(&mut actor).into_parts();
    let requirements = TxPermitRequirements::try_new(TxPermitResourceId::new([0x52; 16]), 1)
        .expect("test permit requirements are nonzero");
    let (pending, permit_request) = job.begin_permit(requirements);
    let mut policy = CountingAllow { calls: 0 };
    let reply = coordinator
        .authorize_tx(
            &mut node,
            permit_request,
            MonotonicMillis::new(112),
            &mut policy,
        )
        .unwrap_or_else(|failure| {
            panic!("direct DATA authorization failed: {:?}", failure.reason())
        });
    assert_eq!(policy.calls, 1);
    let PermitResolution::Authorized(mut authorized) = pending
        .resolve(reply, MonotonicMillis::new(113))
        .unwrap_or_else(|_| panic!("matching direct DATA permit reply must resolve"))
    else {
        panic!("allow policy must authorize direct DATA")
    };

    let received = {
        let frame = authorized
            .frame(MonotonicMillis::new(114))
            .expect("authorized direct DATA frame must be exposed once");
        assert_eq!(frame.interface(), PacketInterfaceId::new(2));
        responder
            .ingest(
                frame.bytes(),
                MonotonicSeconds::new(114),
                PacketInterfaceId::new(7),
                &mut rng,
            )
            .expect("responder must accept direct Link DATA")
    };
    assert_eq!(received.metadata.generated_proof_actions(), 1);
    let proof = received
        .actions
        .packets
        .iter()
        .find(|packet| {
            parse_packet(packet.bytes()).is_ok_and(|parsed| parsed.packet_type == PacketType::Proof)
        })
        .expect("direct Link DATA must produce an explicit proof");

    let completion = ticket
        .complete(authorized.complete(TxCompletionCode::new(0x523)))
        .expect("authorized direct owner must match its actor ticket");
    actor
        .try_send_completion(completion)
        .expect("authorized direct completion must fit");
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| {
            panic!(
                "authorized direct completion must correlate: {:?}",
                failure.reason()
            )
        });
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(115)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(prepared.slot_id()))
    );

    let delivered = node
        .ingest(
            proof.bytes(),
            MonotonicSeconds::new(116),
            PacketInterfaceId::new(2),
            &mut rng,
        )
        .expect("direct Link DATA proof must correlate");
    assert_eq!(delivered.metadata.delivered_receipt_terminals(), 1);
    let terminal = node
        .terminal_attempts()
        .next()
        .expect("proof must retain one terminal attempt");
    assert_eq!(terminal.handle(), attempt);
    assert_eq!(terminal.receipt_kind(), DataReceiptKind::LinkData);
    assert_eq!(terminal.outcome(), AttemptOutcome::Delivered);
    assert_eq!(
        node.acknowledge_terminal(attempt)
            .expect("projected direct terminal must acknowledge"),
        terminal
    );
}

#[test]
fn preparation_uses_fresh_router_eligibility_and_fresh_owner_clock() {
    let (mut node, destination) = sender::<1>(30);
    let mut coordinator = coordinator(&mut node);
    let (mut router, _actor, descriptor) = one_interface_router::<1>(false);
    let mut rng = CounterRng::default();

    assert!(matches!(
        coordinator.try_prepare_data(
            &mut node,
            &router,
            prepare_request(destination, b"offline route", 1_000),
            MonotonicMillis::new(10),
            &mut rng,
        ),
        DataRouterPrepareResult::Rejected {
            reason: SubmitError::NoEligibleInterface { .. },
            ..
        }
    ));
    assert_eq!(coordinator.parked_counts().available(), 1);

    router
        .set_online(descriptor.lease(), true)
        .expect("current interface lease must go online");
    assert!(matches!(
        coordinator.try_prepare_data(
            &mut node,
            &router,
            prepare_request(destination, b"expired owner clock", 20),
            MonotonicMillis::new(20),
            &mut rng,
        ),
        DataRouterPrepareResult::Rejected {
            reason: SubmitError::LeaseDeadlineExpired { .. },
            ..
        }
    ));
    assert!(matches!(
        coordinator.try_prepare_data(
            &mut node,
            &router,
            prepare_request(destination, b"fresh route snapshot", 1_000),
            MonotonicMillis::new(21),
            &mut rng,
        ),
        DataRouterPrepareResult::Prepared(_)
    ));
}

#[test]
fn pending_deadline_enters_recovery_and_requires_exact_ack_before_reuse() {
    let (mut node, destination) = sender::<1>(40);
    let mut coordinator = coordinator(&mut node);
    let (mut router, _actor, _) = one_interface_router::<1>(true);
    let mut rng = CounterRng::default();

    let hop = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"deadline recovery", 100),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("deadline preparation failed: {other:?}"),
    };
    assert_eq!(
        coordinator
            .maintain_tx(&mut node, MonotonicMillis::new(100))
            .expect("bound owner must maintain")
            .newly_recovery_required,
        1
    );
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(100)),
        DataRouterStep::PendingJobExpired(hop)
    );
    let observation = match coordinator.step(&mut node, &mut router, MonotonicMillis::new(101)) {
        DataRouterStep::Completion(DataRouterCompletionProgress::Recovered(observation)) => {
            observation
        }
        other => panic!("expired owner did not park recovered: {other:?}"),
    };
    assert_eq!(coordinator.parked_counts().recovered(), 1);
    assert_eq!(
        coordinator.try_prepare_data(
            &mut node,
            &router,
            prepare_request(destination, b"blocked pending ack", 1_000),
            MonotonicMillis::new(102),
            &mut rng,
        ),
        DataRouterPrepareResult::NoAvailable
    );
    coordinator
        .acknowledge_recovered(observation)
        .expect("exact recovery observation must release the buffer");
    assert_eq!(coordinator.parked_counts().available(), 1);
}

#[test]
fn stale_interface_lease_still_reconciles_the_exact_data_owner() {
    let (mut node, destination) = sender::<1>(50);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actor, descriptor) = one_interface_router::<1>(true);
    let mut rng = CounterRng::default();

    let hop = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"stale lease recovery", 1_000),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("stale-lease preparation failed: {other:?}"),
    };
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(11)),
        DataRouterStep::Routed(hop)
    );
    let (ticket, job) = take_data_job(&mut actor).into_parts();
    let completion = ticket
        .complete(
            job.return_unpermitted()
                .complete(TxCompletionCode::new(0x513)),
        )
        .unwrap_or_else(|_| panic!("stale ticket must remain exact"));
    actor
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("stale completion must fit"));
    router
        .reconfigure(descriptor.lease(), properties(52), true)
        .expect("test reconfiguration must supersede the lease");
    let recovery = router
        .try_receive_completion()
        .expect_err("superseded completion must use recovery")
        .into_node_recovery();
    let OutboundCompletion::Data(completion) = recovery.into_outbound() else {
        panic!("stale DATA completion changed family")
    };
    coordinator
        .try_accept_completion(completion)
        .unwrap_or_else(|failure| panic!("recovery failed: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(12)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(hop.slot_id()))
    );
    assert_eq!(coordinator.fault(), None);
}

#[test]
fn crossed_foreign_completion_is_returned_unchanged_and_both_owners_reconcile() {
    let (mut first_node, first_destination) = sender::<1>(53);
    let mut first_coordinator = coordinator(&mut first_node);
    let (mut first_router, mut first_actor, _) = one_interface_router::<1>(true);
    let mut first_rng = CounterRng::default();
    let first = match first_coordinator.try_prepare_data(
        &mut first_node,
        &first_router,
        prepare_request(first_destination, b"first crossed owner", 1_000),
        MonotonicMillis::new(10),
        &mut first_rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("first crossed preparation failed: {other:?}"),
    };
    assert_eq!(
        first_coordinator.step(&mut first_node, &mut first_router, MonotonicMillis::new(11),),
        DataRouterStep::Routed(first)
    );

    let (mut second_node, second_destination) = sender::<1>(56);
    let mut second_coordinator = coordinator(&mut second_node);
    let (mut second_router, mut second_actor, _) = one_interface_router::<1>(true);
    let mut second_rng = CounterRng(100);
    let second = match second_coordinator.try_prepare_data(
        &mut second_node,
        &second_router,
        prepare_request(second_destination, b"second crossed owner", 1_000),
        MonotonicMillis::new(10),
        &mut second_rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("second crossed preparation failed: {other:?}"),
    };
    assert_eq!(
        second_coordinator.step(
            &mut second_node,
            &mut second_router,
            MonotonicMillis::new(11),
        ),
        DataRouterStep::Routed(second)
    );

    let (first_ticket, first_job) = take_data_job(&mut first_actor).into_parts();
    let first_completion = first_ticket
        .complete(
            first_job
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x518)),
        )
        .unwrap_or_else(|_| panic!("first crossed ticket must remain exact"));
    first_actor
        .try_send_completion(first_completion)
        .unwrap_or_else(|_| panic!("first crossed completion must fit"));
    let first_completion = receive_data_completion(&mut first_router);

    let (second_ticket, second_job) = take_data_job(&mut second_actor).into_parts();
    let second_completion = second_ticket
        .complete(
            second_job
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x519)),
        )
        .unwrap_or_else(|_| panic!("second crossed ticket must remain exact"));
    second_actor
        .try_send_completion(second_completion)
        .unwrap_or_else(|_| panic!("second crossed completion must fit"));
    let second_completion = receive_data_completion(&mut second_router);
    let second_binding = DataPreparedHop::from_completion(&second_completion);

    let failure = first_coordinator
        .try_accept_completion(second_completion)
        .expect_err("foreign completion must not consume the first binding");
    assert!(matches!(
        failure.reason(),
        DataCompletionAcceptError::OwnerMismatch { slot, .. } if slot == first.slot_id()
    ));
    let second_completion = failure.into_completion();
    assert_eq!(
        DataPreparedHop::from_completion(&second_completion),
        second_binding,
        "rejected foreign completion must return unchanged"
    );
    second_coordinator
        .try_accept_completion(second_completion)
        .unwrap_or_else(|failure| panic!("true second owner rejected: {:?}", failure.reason()));
    assert_eq!(
        second_coordinator.step(
            &mut second_node,
            &mut second_router,
            MonotonicMillis::new(12),
        ),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(second.slot_id()))
    );

    first_coordinator
        .try_accept_completion(first_completion)
        .unwrap_or_else(|failure| panic!("true first owner rejected: {:?}", failure.reason()));
    assert_eq!(
        first_coordinator.step(&mut first_node, &mut first_router, MonotonicMillis::new(12),),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(first.slot_id()))
    );
    assert_eq!(first_coordinator.parked_counts().available(), 1);
    assert_eq!(second_coordinator.parked_counts().available(), 1);
}

#[test]
fn one_quarantined_buffer_does_not_disable_other_data_slots() {
    let (mut node, destination) = sender::<2>(60);
    let mut coordinator = coordinator(&mut node);
    let (mut router, mut actor, _) = one_interface_router::<1>(true);
    let mut rng = CounterRng::default();

    let first = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"quarantine one slot", 1_000),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("quarantine preparation failed: {other:?}"),
    };
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(11)),
        DataRouterStep::Routed(first)
    );
    let (ticket, job) = take_data_job(&mut actor).into_parts();
    let completion = ticket
        .complete(job.recovery_fault(TxCompletionCode::new(0x514)))
        .unwrap_or_else(|_| panic!("recovery ticket must remain exact"));
    actor
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("recovery completion must fit"));
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| panic!("recovery failed: {:?}", failure.reason()));
    let observation = match coordinator.step(&mut node, &mut router, MonotonicMillis::new(12)) {
        DataRouterStep::Completion(DataRouterCompletionProgress::Quarantined(observation)) => {
            observation
        }
        other => panic!("recovery fault did not quarantine: {other:?}"),
    };
    assert_eq!(observation.record().slot_id(), first.slot_id());
    assert_eq!(coordinator.parked_counts().quarantined(), 1);
    assert_eq!(coordinator.parked_counts().available(), 1);
    assert_eq!(coordinator.fault(), None);
    assert!(matches!(
        coordinator.try_prepare_data(
            &mut node,
            &router,
            prepare_request(destination, b"other slot remains usable", 1_000),
            MonotonicMillis::new(13),
            &mut rng,
        ),
        DataRouterPrepareResult::Prepared(_)
    ));
}

struct CountingAllow {
    calls: usize,
}

impl TxAuthorizationPolicy for CountingAllow {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        self.calls += 1;
        TxPolicyDecision::Authorize(
            reticulum_node_core::TxPermitReservation::try_new(
                candidate.requirements.resource(),
                candidate.requirements.required_units(),
            )
            .expect("test reservation must be nonzero"),
        )
    }
}

#[test]
fn registry_profile_fault_drains_local_pending_and_forces_actor_permit_denial() {
    let (mut node, destination) = sender::<3>(70);
    let mut coordinator = coordinator(&mut node);
    let fabric: &'static mut InterfaceFabric<NoopRawMutex, 2, 1> =
        Box::leak(Box::new(InterfaceFabric::new()));
    let (mut router, mut actors) = fabric.split();
    router
        .register(
            actors[0].queue_id(),
            PacketInterfaceId::new(2),
            properties(2),
            true,
        )
        .expect("primary interface must register");
    let mut rng = CounterRng::default();

    let actor_owned = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"actor owner before fault", 1_000),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("actor preparation failed: {other:?}"),
    };
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(11)),
        DataRouterStep::Routed(actor_owned)
    );
    let (ticket, job) = take_data_job(&mut actors[0]).into_parts();
    let requirements = TxPermitRequirements::try_new(TxPermitResourceId::new([7; 16]), 1)
        .expect("test requirements must be nonzero");
    let (pending, permit_request) = job.begin_permit(requirements);

    let locally_pending = match coordinator.try_prepare_data(
        &mut node,
        &router,
        prepare_request(destination, b"local owner before fault", 1_000),
        MonotonicMillis::new(12),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("local pending preparation failed: {other:?}"),
    };
    assert_eq!(coordinator.pending_job_count(), 1);

    router
        .register(
            actors[1].queue_id(),
            PacketInterfaceId::new(80),
            properties(80),
            true,
        )
        .expect("out-of-profile interface remains representable by registry");
    assert!(matches!(
        coordinator.try_prepare_data(
            &mut node,
            &router,
            prepare_request(destination, b"fault trigger", 1_000),
            MonotonicMillis::new(13),
            &mut rng,
        ),
        DataRouterPrepareResult::Disabled(DataRouterFault::RegistryProfile(_))
    ));
    let mut policy = CountingAllow { calls: 0 };
    let reply = coordinator
        .authorize_tx(
            &mut node,
            permit_request,
            MonotonicMillis::new(14),
            &mut policy,
        )
        .unwrap_or_else(|failure| {
            panic!("valid fault-drain request failed: {:?}", failure.reason())
        });
    assert_eq!(policy.calls, 0);
    assert!(matches!(
        &reply,
        TxPermitReply::Denied(denial)
            if denial.reason()
                == TxPermitDenialReason::Policy(TxPolicyDenial::ResourceUnavailable)
    ));
    let unpermitted = match pending.resolve(reply, MonotonicMillis::new(15)) {
        Ok(reticulum_node_core::PermitResolution::Unpermitted(owner)) => owner,
        Ok(_) => panic!("fault-drain permit unexpectedly authorized"),
        Err(_) => panic!("matching fault-drain reply was rejected"),
    };
    let completion = ticket
        .complete(unpermitted.complete(TxCompletionCode::new(0x515)))
        .unwrap_or_else(|_| panic!("fault-drain ticket must remain exact"));
    actors[0]
        .try_send_completion(completion)
        .unwrap_or_else(|_| panic!("fault-drain completion must fit"));
    coordinator
        .try_accept_completion(receive_data_completion(&mut router))
        .unwrap_or_else(|failure| panic!("fault-drain return failed: {:?}", failure.reason()));
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(16)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(
            actor_owned.slot_id()
        ))
    );
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(17)),
        DataRouterStep::PendingJobCancelledForFault(locally_pending)
    );
    assert_eq!(coordinator.pending_job_count(), 0);
    assert_eq!(coordinator.completion_correlated_count(), 1);
    assert_eq!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(18)),
        DataRouterStep::Completion(DataRouterCompletionProgress::Available(
            locally_pending.slot_id()
        ))
    );
    assert!(matches!(
        coordinator.step(&mut node, &mut router, MonotonicMillis::new(19)),
        DataRouterStep::Disabled(DataRouterFault::RegistryProfile(_))
    ));
    assert_eq!(coordinator.parked_counts().available(), 3);
}
