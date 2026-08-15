extern crate std;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use reticulum_interface_router::{
    AdvertisedBitrate, IngressSignalObservation as RouterIngressSignalObservation,
    InterfaceConfigId, InterfaceCost, InterfaceTxJob, LogicalMtu,
};
use reticulum_node_core::{
    ApplicationEvent, ApplicationEventDiscardReason, ApplicationEventSlot, ApplicationLinkRole,
    DestinationHash, MAX_DIRECT_LXMF_WIRE, MAX_OPPORTUNISTIC_LXMF_CARRIER, NodeConfig,
    NodeIdentity, NodeInstanceId, OrdinaryBufferPool, OrdinaryPacketBuffer, PermitResolution,
    TxAuthorizationCandidate, TxCompletionCode, TxPacketBuffer, TxPermitRequirements,
    TxPermitReservation, TxPermitResourceId, TxPolicyDecision, TxTarget as NodeTxTarget,
};
use reticulum_rns_rete::{
    IngressDisposition, InterfaceId as RnsInterfaceId, PacketType, TxTarget as RnsTxTarget,
};
use reticulum_tx_handoff::{DataPermitHandoff, OrdinaryPermitHandoff};
use std::boxed::Box;
use std::{vec, vec::Vec};

use super::*;
use crate::{
    DataPreparedHop, DataRouterCompletionProgress, DataRouterConfig,
    OrdinaryRouterCompletionProgress, OrdinaryRouterConfig,
};

const DATA_BUFFERS: usize = 1;
const ORDINARY_BUFFERS: usize = 2;

type TestNode = NodeCore<8, 4, 8, 8, DATA_BUFFERS>;
type TestSupervisor<const SLOTS: usize, const DEPTH: usize> = NodeInterfaceSupervisor<
    NoopRawMutex,
    CountingAllow,
    8,
    4,
    8,
    8,
    DATA_BUFFERS,
    ORDINARY_BUFFERS,
    SLOTS,
    DEPTH,
>;

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

#[derive(Default)]
struct CountingAllow {
    calls: usize,
}

impl TxAuthorizationPolicy for CountingAllow {
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
        self.calls += 1;
        TxPolicyDecision::Authorize(
            TxPermitReservation::try_new(
                candidate.requirements.resource(),
                candidate.requirements.required_units(),
            )
            .expect("test policy covers the exact nonzero request"),
        )
    }
}

fn identity(tag: u8) -> NodeIdentity {
    NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid")
}

fn node(tag: u8, aspect: &str) -> TestNode {
    TestNode::new(
        identity(tag),
        "reticulum",
        &[aspect],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .expect("test node must construct")
}

fn sender(tag: u8) -> (TestNode, DestinationHash) {
    let receiver_tag = tag.wrapping_add(1);
    let receiver = NodeCore::<8, 4, 8, 8, 0>::new(
        identity(receiver_tag),
        "reticulum",
        &["aggregate-receiver"],
        NodeInstanceId::new([receiver_tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .expect("test receiver must construct");
    let destination = receiver.destination_hash();
    let mut sender = node(tag, "aggregate-sender");
    sender
        .register_peer(
            &identity(receiver_tag),
            "reticulum",
            &["aggregate-receiver"],
            MonotonicSeconds::new(0),
        )
        .expect("receiver identity must cache");
    (sender, destination)
}

fn active_request_sender(tag: u8) -> (TestNode, DestinationHash, LinkHandle) {
    let responder_tag = tag.wrapping_add(1);
    let responder_aspect = "aggregate-request-responder";
    let mut responder = NodeCore::<8, 4, 8, 8, 0>::new(
        identity(responder_tag),
        "reticulum",
        &[responder_aspect],
        NodeInstanceId::new([responder_tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .expect("request responder must construct");
    let destination = responder.destination_hash();
    responder
        .set_destination_accepts_links(&destination, true)
        .expect("request responder must accept Links");
    let mut initiator = node(tag, "aggregate-request-initiator");
    initiator
        .register_peer(
            &identity(responder_tag),
            "reticulum",
            &[responder_aspect],
            MonotonicSeconds::new(1),
        )
        .expect("request responder identity must cache");
    let mut rng = CounterRng::default();
    let (request, link) = initiator
        .initiate_link(&destination, MonotonicSeconds::new(2), &mut rng)
        .expect("request Link must initiate");
    let response = responder
        .ingest(
            request.packets[0].bytes(),
            MonotonicSeconds::new(2),
            PacketInterfaceId::new(7),
            &mut rng,
        )
        .expect("request responder must accept LINKREQUEST");
    let established = initiator
        .ingest(
            response.actions.packets[0].bytes(),
            MonotonicSeconds::new(3),
            PacketInterfaceId::new(2),
            &mut rng,
        )
        .expect("request initiator must accept LRPROOF");
    responder
        .ingest(
            established.actions.packets[0].bytes(),
            MonotonicSeconds::new(4),
            PacketInterfaceId::new(7),
            &mut rng,
        )
        .expect("request responder must accept LRRTT");
    assert_eq!(initiator.link_state(link), Some(LinkState::Active));
    (initiator, destination, link)
}

fn active_response_nodes(tag: u8) -> (TestNode, TestNode, DestinationHash, LinkHandle) {
    let responder_tag = tag.wrapping_add(1);
    let responder_aspect = "aggregate-response-responder";
    let mut responder = node(responder_tag, responder_aspect);
    let destination = responder.destination_hash();
    responder
        .set_destination_accepts_links(&destination, true)
        .expect("response responder must accept Links");
    let mut initiator = node(tag, "aggregate-response-initiator");
    initiator
        .register_peer(
            &identity(responder_tag),
            "reticulum",
            &[responder_aspect],
            MonotonicSeconds::new(1),
        )
        .expect("response responder identity must cache");
    let mut rng = CounterRng::default();
    let (request, link) = initiator
        .initiate_link(&destination, MonotonicSeconds::new(2), &mut rng)
        .expect("response Link must initiate");
    let response = responder
        .ingest(
            request.packets[0].bytes(),
            MonotonicSeconds::new(2),
            PacketInterfaceId::new(7),
            &mut rng,
        )
        .expect("response responder must accept LINKREQUEST");
    let established = initiator
        .ingest(
            response.actions.packets[0].bytes(),
            MonotonicSeconds::new(3),
            PacketInterfaceId::new(2),
            &mut rng,
        )
        .expect("response initiator must accept LRPROOF");
    responder
        .ingest(
            established.actions.packets[0].bytes(),
            MonotonicSeconds::new(4),
            PacketInterfaceId::new(7),
            &mut rng,
        )
        .expect("response responder must accept LRRTT");
    assert_eq!(initiator.link_state(link), Some(LinkState::Active));
    assert_eq!(
        responder.link_state(LinkHandle::new(*link.as_bytes())),
        Some(LinkState::Active)
    );
    (initiator, responder, destination, link)
}

fn data_coordinator(node: &mut TestNode) -> DataRouterCoordinator<DATA_BUFFERS> {
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    node.register_packet_buffer(buffer)
        .expect("DATA buffer must register");
    DataRouterCoordinator::try_new(
        node,
        [buffer],
        DataRouterConfig::new(
            TxCompletionCode::new(0x701),
            TxCompletionCode::new(0x702),
            TxCompletionCode::new(0x703),
        ),
    )
    .unwrap_or_else(|failure| panic!("DATA coordinator: {:?}", failure.reason()))
}

fn ordinary_coordinator(node: &mut TestNode) -> OrdinaryRouterCoordinator<ORDINARY_BUFFERS> {
    let mut owner = node
        .take_ordinary_action_owner::<ORDINARY_BUFFERS>()
        .expect("ordinary owner may be issued once");
    let mut pool = OrdinaryBufferPool::new();
    for _ in 0..ORDINARY_BUFFERS {
        owner
            .register_and_park(&mut pool, Box::leak(Box::new(OrdinaryPacketBuffer::new())))
            .unwrap_or_else(|failure| panic!("ordinary buffer must park: {:?}", failure.reason()));
    }
    OrdinaryRouterCoordinator::try_new(
        owner,
        pool,
        OrdinaryRouterConfig::new(
            TxCompletionCode::new(0x711),
            TxCompletionCode::new(0x712),
            TxCompletionCode::new(0x713),
        ),
    )
    .unwrap_or_else(|failure| panic!("ordinary coordinator: {:?}", failure.reason()))
}

fn data_pairs<const SLOTS: usize>() -> [DataPairedPermitHandoff<NoopRawMutex>; SLOTS] {
    core::array::from_fn(|_| {
        let store = Box::leak(Box::new(DataPermitHandoff::new()));
        store.split_paired()
    })
}

fn ordinary_pairs<const SLOTS: usize>() -> [OrdinaryPairedPermitHandoff<NoopRawMutex>; SLOTS] {
    core::array::from_fn(|_| {
        let store = Box::leak(Box::new(OrdinaryPermitHandoff::new()));
        store.split_paired()
    })
}

fn build<const SLOTS: usize, const DEPTH: usize>(
    tag: u8,
) -> (
    TestSupervisor<SLOTS, DEPTH>,
    [NodeInterfaceActorPorts<NoopRawMutex, DEPTH>; SLOTS],
    DestinationHash,
) {
    let (node, destination) = sender(tag);
    build_from_node(node, destination)
}

fn build_from_node<const SLOTS: usize, const DEPTH: usize>(
    mut node: TestNode,
    destination: DestinationHash,
) -> (
    TestSupervisor<SLOTS, DEPTH>,
    [NodeInterfaceActorPorts<NoopRawMutex, DEPTH>; SLOTS],
    DestinationHash,
) {
    let data = data_coordinator(&mut node);
    let ordinary = ordinary_coordinator(&mut node);
    let fabric = Box::leak(Box::new(InterfaceFabric::new()));
    match TestSupervisor::<SLOTS, DEPTH>::try_new(
        node,
        fabric,
        data,
        ordinary,
        data_pairs(),
        ordinary_pairs(),
        CountingAllow::default(),
    ) {
        Ok(success) => {
            let (supervisor, actors) = success.into_parts();
            (supervisor, actors, destination)
        }
        Err(failure) => panic!("aggregate must build: {:?}", failure.reason()),
    }
}

#[test]
fn permanent_aggregate_forwards_only_a_copied_identity_lookup() {
    let (supervisor, _actors, destination) = build::<1, 1>(60);

    assert_eq!(
        supervisor.recall_identity(&destination),
        Some(identity(61).public_key())
    );
    assert_eq!(
        supervisor.recall_identity(&DestinationHash::new([0xee; 16])),
        None
    );
}

#[test]
fn route_diagnostics_are_complete_sorted_and_resolve_current_interfaces() {
    let (mut owner, fallback_destination) = sender(110);
    let mut exact_peer = NodeCore::<8, 4, 8, 8, 0>::new(
        identity(112),
        "reticulum",
        &["route-diagnostics-exact-peer"],
        NodeInstanceId::new([0xf2; 16]),
        NodeConfig::endpoint(),
    )
    .expect("exact route peer must construct");
    let exact_destination = exact_peer.destination_hash();
    let mut rng = CounterRng::default();
    exact_peer
        .queue_announce(None, AnnounceEmissionTime::new(1).unwrap(), &mut rng)
        .expect("exact route announce must queue");
    let announce = exact_peer.flush_announces(MonotonicSeconds::new(1), &mut rng);
    owner
        .ingest(
            announce.packets[0].bytes(),
            MonotonicSeconds::new(10),
            PacketInterfaceId::new(2),
            &mut rng,
        )
        .expect("owner must learn exact route");

    let (mut supervisor, actor_ports, _) = build_from_node::<2, 1>(owner, fallback_destination);
    let mut snapshot = RouteDiagnosticsSnapshot::<8>::empty();
    supervisor.snapshot_route_diagnostics(MonotonicSeconds::new(50), &mut snapshot);
    assert_eq!(snapshot.captured_at(), MonotonicSeconds::new(50));
    assert_eq!(snapshot.capacity(), 8);
    assert_eq!(snapshot.len(), 2);
    assert_eq!(supervisor.retained_route_count(), 2);
    assert_eq!(supervisor.node_metrics().capacity.paths.used, 2);
    assert_eq!(supervisor.node_metrics().transport.paths_learned, 1);

    let missing: Vec<_> = snapshot.entries().copied().collect();
    assert!(
        missing
            .windows(2)
            .all(|pair| pair[0].route().destination().as_bytes()
                <= pair[1].route().destination().as_bytes())
    );
    let fallback = missing
        .iter()
        .copied()
        .find(|entry| entry.route().destination() == fallback_destination)
        .expect("direct route must be copied");
    assert_eq!(
        fallback.resolution(),
        RouteInterfaceResolution::BroadcastFallback { usable: false }
    );
    let exact = missing
        .iter()
        .copied()
        .find(|entry| entry.route().destination() == exact_destination)
        .expect("exact route must be copied");
    assert_eq!(exact.route().received_on(), Some(PacketInterfaceId::new(2)));
    assert_eq!(exact.route().learned_at(), MonotonicSeconds::new(10));
    assert_eq!(exact.route().last_accessed_at(), MonotonicSeconds::new(10));
    assert_eq!(exact.learned_age_seconds(snapshot.captured_at()), Some(40));
    assert_eq!(exact.lru_age_seconds(snapshot.captured_at()), Some(40));
    assert_eq!(
        exact.resolution(),
        RouteInterfaceResolution::Exact {
            interface: PacketInterfaceId::new(2),
            current: None,
            usable: false,
        }
    );
    let expiry = exact.route().expires_after_seconds();
    assert_eq!(
        exact.expiry_state(MonotonicSeconds::new(10 + expiry)),
        RouteExpiryState::Retained {
            remaining_seconds: 0
        }
    );
    assert_eq!(
        exact.expiry_state(MonotonicSeconds::new(11 + expiry)),
        RouteExpiryState::ExpiredPendingMaintenance { overdue_seconds: 1 }
    );
    assert_eq!(
        exact.expiry_state(MonotonicSeconds::new(9)),
        RouteExpiryState::ClockRegression
    );

    let descriptors = register(&mut supervisor, &actor_ports, [true, true]);
    supervisor.snapshot_route_diagnostics(MonotonicSeconds::new(51), &mut snapshot);
    let registered: Vec<_> = snapshot.entries().copied().collect();
    let fallback = registered
        .iter()
        .copied()
        .find(|entry| entry.route().destination() == fallback_destination)
        .expect("direct route must remain copied");
    assert_eq!(
        fallback.resolution(),
        RouteInterfaceResolution::BroadcastFallback { usable: true }
    );
    let exact = registered
        .iter()
        .copied()
        .find(|entry| entry.route().destination() == exact_destination)
        .expect("exact route must remain copied");
    assert_eq!(
        exact.resolution(),
        RouteInterfaceResolution::Exact {
            interface: PacketInterfaceId::new(2),
            current: Some(descriptors[0]),
            usable: true,
        }
    );
    assert_eq!(exact.resolution().current_online(), Some(true));

    let offline = supervisor
        .router
        .set_online(descriptors[0].lease(), false)
        .expect("exact route interface can go offline");
    supervisor.snapshot_route_diagnostics(MonotonicSeconds::new(52), &mut snapshot);
    let refreshed: Vec<_> = snapshot.entries().copied().collect();
    let fallback = refreshed
        .iter()
        .copied()
        .find(|entry| entry.route().destination() == fallback_destination)
        .expect("direct route must remain copied");
    assert!(fallback.resolution().is_usable());
    let exact = refreshed
        .iter()
        .copied()
        .find(|entry| entry.route().destination() == exact_destination)
        .expect("exact route must remain copied");
    assert_eq!(
        exact.resolution(),
        RouteInterfaceResolution::Exact {
            interface: PacketInterfaceId::new(2),
            current: Some(offline),
            usable: false,
        }
    );
    assert_eq!(exact.resolution().current_online(), Some(false));
}

#[test]
fn permanent_aggregate_retains_and_safely_aborts_an_unestablished_link() {
    let (mut supervisor, _actors, destination) = build::<1, 1>(61);
    let mut rng = CounterRng::default();
    assert!(supervisor.can_initiate_link());
    let (actions, link) = supervisor
        .initiate_link(&destination, MonotonicSeconds::new(100), &mut rng)
        .expect("aggregate-owned Link request must construct");

    assert_eq!(actions.packets.len(), 1);
    assert_eq!(supervisor.link_state(link), Some(LinkState::Handshake));
    assert!(supervisor.abort_unestablished_link(link));
    assert_eq!(supervisor.link_state(link), None);
    assert!(supervisor.can_initiate_link());
    assert!(!supervisor.abort_unestablished_link(link));
}

#[test]
fn request_preparation_preflights_fault_while_exact_discharge_remains_available() {
    let (initiator, destination, link) = active_request_sender(72);
    let (mut supervisor, _actors, _) = build_from_node::<1, 1>(initiator, destination);
    let mut rng = CounterRng::default();

    supervisor.owner_mismatch_fault = true;
    let entropy_before_fault = rng.0;
    assert!(matches!(
        supervisor.prepare_anonymous_request(link, "/page/faulted.mu", 1_700_000_000.0, &mut rng,),
        Err(NodeInterfaceSupervisorFault::DataOwnerMismatch)
    ));
    assert_eq!(rng.0, entropy_before_fault);

    supervisor.owner_mismatch_fault = false;
    let prepared = supervisor
        .prepare_anonymous_request(link, "/page/prepared.mu", 1_700_000_001.0, &mut rng)
        .expect("cleared aggregate fault permits preparation")
        .expect("active Link request must prepare");
    assert_eq!(prepared.actions().packets.len(), 1);
    assert_eq!(prepared.handle().link(), link.as_bytes());
    let (actions, prepared_handle) = prepared.into_parts();
    assert_eq!(actions.discard().packets(), 1);

    supervisor.owner_mismatch_fault = true;
    assert_eq!(
        supervisor.cancel_prepared_request(prepared_handle),
        Ok(()),
        "faulted aggregate must still discharge prepared authority"
    );

    supervisor.owner_mismatch_fault = false;
    let confirmed = supervisor
        .prepare_anonymous_request(link, "/page/confirmed.mu", 1_700_000_002.0, &mut rng)
        .expect("cleared aggregate fault permits another preparation")
        .expect("second active Link request must prepare");
    let (actions, confirmed_handle) = confirmed.into_parts();
    assert_eq!(actions.discard().packets(), 1);

    supervisor.owner_mismatch_fault = true;
    assert_eq!(
        supervisor.confirm_request_dispatch(confirmed_handle, MonotonicSeconds::new(100), true,),
        Ok(RequestDispatchConfirmation::Confirmed),
        "faulted aggregate must still correlate an already-routed request"
    );
    assert_eq!(
        supervisor.cancel_confirmed_request(confirmed_handle),
        Ok(()),
        "faulted aggregate must still discharge confirmed authority"
    );

    supervisor.owner_mismatch_fault = false;
    let reconciled = supervisor
        .prepare_anonymous_request(link, "/page/reconciled.mu", 1_700_000_003.0, &mut rng)
        .expect("cleared aggregate fault permits reconciliation fixture")
        .expect("third active Link request must prepare");
    let (actions, reconciled_handle) = reconciled.into_parts();
    assert_eq!(actions.discard().packets(), 1);

    supervisor.owner_mismatch_fault = true;
    assert_eq!(
        supervisor.reconcile_request_dispatch(reconciled_handle),
        RequestDispatchReconciliation::ReclaimedPrepared,
        "faulted aggregate must still reconcile every exact native owner"
    );
    assert_eq!(
        supervisor.reconcile_request_dispatch(reconciled_handle),
        RequestDispatchReconciliation::Absent
    );
}

#[test]
fn response_preparation_preflights_aggregate_fault_and_preserves_exact_action() {
    let (mut initiator, mut responder, destination, link) = active_response_nodes(74);
    let mut rng = CounterRng::default();
    let prepared = initiator
        .prepare_anonymous_request(link, "/page/index.mu", 1_700_000_000.0, &mut rng)
        .expect("response request must prepare");
    let (request_actions, handle) = prepared.into_parts();
    assert_eq!(
        initiator.confirm_request_dispatch(handle, MonotonicSeconds::new(5), true),
        Ok(RequestDispatchConfirmation::Confirmed)
    );
    let inbound = responder
        .ingest(
            request_actions.packets[0].bytes(),
            MonotonicSeconds::new(5),
            PacketInterfaceId::new(7),
            &mut rng,
        )
        .expect("response request must reach its retained Link");
    let [
        ApplicationEvent::RequestValueReceived {
            binding, request, ..
        },
    ] = inbound.actions.events.as_slice()
    else {
        panic!("response request must retain exact application provenance")
    };
    assert_eq!(binding.link(), link.as_bytes());
    assert_eq!(binding.destination(), destination.as_bytes());
    assert_eq!(binding.role(), ApplicationLinkRole::Responder);
    assert_eq!(request, handle.request());

    let (mut supervisor, _actors, _) = build_from_node::<1, 1>(responder, destination);
    supervisor.owner_mismatch_fault = true;
    let entropy_before_fault = rng.0;
    assert!(matches!(
        supervisor.prepare_response_actions(
            binding,
            request,
            &[0x5a; 400],
            MonotonicSeconds::new(6),
            &mut rng,
        ),
        Err(NodeInterfaceSupervisorFault::DataOwnerMismatch)
    ));
    assert_eq!(rng.0, entropy_before_fault);

    supervisor.owner_mismatch_fault = false;
    let response = supervisor
        .prepare_response_actions(
            binding,
            request,
            &[0x5a; 400],
            MonotonicSeconds::new(7),
            &mut rng,
        )
        .expect("cleared aggregate fault permits response preparation")
        .expect("bounded exact response must prepare");
    assert!(response.events.is_empty());
    assert_eq!(response.packets.len(), 1);
    assert_eq!(response.unroutable_packets, 0);
    assert_eq!(
        response.packets[0].target(),
        RnsTxTarget::Only(RnsInterfaceId(7))
    );
    assert_eq!(response.packets[0].protocol_token(), None);

    let received = initiator
        .ingest(
            response.packets[0].bytes(),
            MonotonicSeconds::new(7),
            PacketInterfaceId::new(2),
            &mut rng,
        )
        .expect("exact response must return to the request initiator");
    assert!(matches!(
        received.actions.events.as_slice(),
        [ApplicationEvent::ResponseReceived {
            link: event_link,
            request: event_request,
            data,
        }] if event_link == handle.link()
            && event_request == handle.request()
            && data.as_slice() == [0x5a; 400]
    ));
}

#[test]
fn permanent_aggregate_reports_exact_link_attempt_occupancy() {
    let mut initiator = node(70, "aggregate-direct-query-sender");
    initiator
        .register_inbound_single_destination("lxmf", &["delivery"])
        .expect("the sender LXMF delivery destination registers");
    let mut responder = NodeCore::<8, 4, 8, 8, 0>::new(
        identity(71),
        "reticulum",
        &["aggregate-direct-query-responder"],
        NodeInstanceId::new([0xd1; 16]),
        NodeConfig::endpoint(),
    )
    .expect("test responder must construct");
    let destination = responder
        .register_inbound_single_destination("lxmf", &["delivery"])
        .expect("the responder LXMF delivery destination registers");
    initiator
        .register_peer(
            &identity(71),
            "lxmf",
            &["delivery"],
            MonotonicSeconds::new(1),
        )
        .expect("the responder identity must cache");
    responder
        .set_destination_accepts_links(&destination, true)
        .expect("the responder delivery destination accepts Links");

    let mut rng = CounterRng::default();
    let (request, link) = initiator
        .initiate_link(&destination, MonotonicSeconds::new(2), &mut rng)
        .expect("the Link request must construct");
    let response = responder
        .ingest(
            request.packets[0].bytes(),
            MonotonicSeconds::new(2),
            PacketInterfaceId::new(7),
            &mut rng,
        )
        .expect("the responder accepts the Link request");
    let established = initiator
        .ingest(
            response.actions.packets[0].bytes(),
            MonotonicSeconds::new(3),
            PacketInterfaceId::new(2),
            &mut rng,
        )
        .expect("the initiator accepts the Link response");
    responder
        .ingest(
            established.actions.packets[0].bytes(),
            MonotonicSeconds::new(4),
            PacketInterfaceId::new(7),
            &mut rng,
        )
        .expect("the responder accepts the Link proof");
    assert_eq!(initiator.link_state(link), Some(LinkState::Active));

    let (mut supervisor, actors, _) = build_from_node::<1, 1>(initiator, destination);
    register(&mut supervisor, &actors, [true]);
    assert!(!supervisor.link_has_unacknowledged_attempt(link));

    let mut wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
    let wire_len = usize::from(
        supervisor
            .prepare_basic_direct_lxmf_into(
                &destination,
                1_700_000_000_000,
                b"occupancy",
                b"one exact Link attempt",
                &mut wire,
            )
            .expect("direct LXMF wire must compose")
            .wire_len(),
    );
    let prepared = supervisor.try_prepare_rehydrated_direct_lxmf(
        link,
        DataRouterPrepareRequest {
            destination,
            plaintext: &wire[..wire_len],
            rns_now: MonotonicSeconds::new(5),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(100_000)),
        },
        MonotonicMillis::new(5_000),
        &mut rng,
    );
    assert!(matches!(
        prepared,
        NodeInterfaceDataPrepareResult::Coordinator(DataRouterPrepareResult::Prepared(_))
    ));
    assert!(supervisor.link_has_unacknowledged_attempt(link));
    assert!(!supervisor.link_has_unacknowledged_attempt(LinkHandle::new([0xee; 16])));
}

#[test]
fn permanent_aggregate_composes_basic_lxmf_with_only_the_registered_local_source() {
    let (supervisor, _actors, destination) = build::<1, 1>(62);
    let mut unavailable = [0x4c_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    assert_eq!(
        supervisor.prepare_basic_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"title",
            b"content",
            &mut unavailable,
        ),
        Err(PrepareBasicLxmfError::DeliveryDestinationUnavailable)
    );
    assert!(unavailable.iter().all(|byte| *byte == 0x4c));

    let (mut owner, destination) = sender(64);
    let source = owner
        .register_inbound_single_destination("lxmf", &["delivery"])
        .expect("the node-owned LXMF delivery destination registers");
    let (supervisor, _actors, _) = build_from_node::<1, 1>(owner, destination);
    let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
    let prepared = supervisor
        .prepare_basic_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"title",
            b"content",
            &mut carrier,
        )
        .expect("the supervisor forwards the source-free composer call");

    assert_eq!(&carrier[..16], source.as_bytes());
    assert_ne!(prepared.message_id(), [0; 32]);
    assert!(usize::from(prepared.carrier_len()) <= carrier.len());
}

#[test]
fn permanent_aggregate_composes_complete_direct_lxmf_wire() {
    let (mut owner, destination) = sender(66);
    owner
        .register_inbound_single_destination("lxmf", &["delivery"])
        .expect("the node-owned LXMF delivery destination registers");
    let (supervisor, _actors, _) = build_from_node::<1, 1>(owner, destination);
    let mut wire = [0x4d_u8; MAX_DIRECT_LXMF_WIRE];
    let prepared = supervisor
        .prepare_basic_direct_lxmf_into(
            &destination,
            1_700_000_000_000,
            b"title",
            b"content",
            &mut wire,
        )
        .expect("the supervisor forwards complete direct composition");

    let wire_len = usize::from(prepared.wire_len());
    assert_eq!(&wire[..16], destination.as_bytes());
    assert_ne!(prepared.message_id(), [0; 32]);
    assert!(wire_len <= wire.len());
    assert!(wire[wire_len..].iter().all(|byte| *byte == 0x4d));
}

fn properties(config: u32) -> InterfaceProperties {
    InterfaceProperties::new(
        LogicalMtu::try_new(500).expect("test MTU is nonzero"),
        InterfaceConfigId::new(config),
        None,
        InterfaceCost::new(0),
    )
}

fn register<const SLOTS: usize, const DEPTH: usize>(
    supervisor: &mut TestSupervisor<SLOTS, DEPTH>,
    actors: &[NodeInterfaceActorPorts<NoopRawMutex, DEPTH>; SLOTS],
    online: [bool; SLOTS],
) -> [InterfaceDescriptor; SLOTS] {
    core::array::from_fn(|index| {
        supervisor
            .router
            .register(
                actors[index].queue_id(),
                PacketInterfaceId::new(
                    u8::try_from(index + 2).expect("test interface fits compact profile"),
                ),
                properties(index as u32 + 1),
                online[index],
            )
            .expect("interface must register")
    })
}

fn prepare_request(
    destination: DestinationHash,
    plaintext: &'static [u8],
) -> DataRouterPrepareRequest<'static> {
    DataRouterPrepareRequest {
        destination,
        plaintext,
        rns_now: MonotonicSeconds::new(1),
        deadline: TxLeaseDeadline::new(MonotonicMillis::new(100_000)),
    }
}

fn prepare_data<const SLOTS: usize, const DEPTH: usize>(
    supervisor: &mut TestSupervisor<SLOTS, DEPTH>,
    destination: DestinationHash,
    plaintext: &'static [u8],
    rng: &mut CounterRng,
) -> DataPreparedHop {
    match supervisor.try_prepare_data(
        prepare_request(destination, plaintext),
        MonotonicMillis::new(10),
        rng,
    ) {
        NodeInterfaceDataPrepareResult::Coordinator(DataRouterPrepareResult::Prepared(hop)) => hop,
        other => panic!("DATA did not prepare: {other:?}"),
    }
}

fn standalone_announce(tag: u8, rng: &mut CounterRng) -> NodeActions {
    let mut source = NodeCore::<8, 4, 8, 8, 0>::new(
        identity(tag),
        "reticulum",
        &["aggregate-announce"],
        NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .expect("announce source must construct");
    source
        .queue_announce(None, AnnounceEmissionTime::new(10).unwrap(), rng)
        .expect("announce must queue");
    source.flush_announces(MonotonicSeconds::new(10), rng)
}

fn native_announce_packet(tag: u8, rng: &mut CounterRng) -> Vec<u8> {
    let actions = standalone_announce(tag, rng);
    actions.packets[0].bytes().to_vec()
}

fn first_forwarded_announce_job(
    through_point_to_point: bool,
    boundary_roles: bool,
) -> Option<(NodeTxTarget, PacketInterfaceId)> {
    let relay = TestNode::new(
        identity(90),
        "reticulum",
        &["aggregate-border-relay"],
        NodeInstanceId::new([0xea; 16]),
        NodeConfig::transport(),
    )
    .expect("transport node must construct");
    let destination = relay.destination_hash();
    let (mut supervisor, [lora_actor, tcp_actor], _) = build_from_node::<2, 2>(relay, destination);
    let mut lora_properties = properties(1).with_topology(InterfaceTopology::SharedMedium);
    let mut tcp_properties = properties(2).with_topology(InterfaceTopology::PointToPoint);
    if boundary_roles {
        lora_properties = lora_properties
            .with_announce_mode(reticulum_interface_router::AnnouncePropagationMode::Internal);
        tcp_properties = tcp_properties
            .with_announce_mode(reticulum_interface_router::AnnouncePropagationMode::Boundary);
    }
    let lora_descriptor = supervisor
        .router
        .register(
            lora_actor.queue_id(),
            PacketInterfaceId::new(1),
            lora_properties,
            true,
        )
        .expect("shared-medium LoRa fixture registers");
    let tcp_descriptor = supervisor
        .router
        .register(
            tcp_actor.queue_id(),
            PacketInterfaceId::new(2),
            tcp_properties,
            true,
        )
        .expect("point-to-point TCP fixture registers");
    let (lora_interface, _lora_data, _lora_ordinary) = lora_actor.into_parts();
    let (tcp_interface, _tcp_data, _tcp_ordinary) = tcp_actor.into_parts();
    let (mut lora_tx, mut lora_ingress, _lora_lifecycle) = lora_interface.into_parts();
    let (mut tcp_tx, mut tcp_ingress, _tcp_lifecycle) = tcp_interface.into_parts();
    let (source_ingress, source_descriptor) = if through_point_to_point {
        (&mut tcp_ingress, tcp_descriptor)
    } else {
        (&mut lora_ingress, lora_descriptor)
    };
    let authority = source_ingress
        .bind_ingress(source_descriptor)
        .expect("current descriptor binds to its ingress actor");
    let mut rng = CounterRng::default();
    let packet = native_announce_packet(91, &mut rng);
    let mut buffer = source_ingress
        .try_receive_buffer()
        .expect("source actor owns a reusable ingress buffer");
    buffer.capacity_mut()[..packet.len()].copy_from_slice(&packet);
    let sealed = buffer.seal(packet.len()).expect("announce packet seals");
    source_ingress
        .try_send(authority, sealed)
        .expect("completed ingress queue has capacity");

    let processed = match supervisor.step_ingress(MonotonicSeconds::new(40), admission(), &mut rng)
    {
        NodeInterfaceIngressStep::Processed(processed) => processed,
        _ => panic!("announce ingress did not process"),
    };
    assert_eq!(processed.actions_backpressured(), None);
    assert_eq!(processed.terminal_action_fault(), None);
    assert_eq!(processed.recycle_fault(), None);

    let mut routed = None;
    for _ in 0..64 {
        if let Some(job) = lora_tx.try_receive_job() {
            routed = Some(job);
            break;
        }
        assert!(
            tcp_tx.try_receive_job().is_none(),
            "interface ordering must route this fixture through LoRa first"
        );
        if matches!(
            supervisor.step(MonotonicMillis::new(40_000)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            let _ = drain_and_discard_application_events(&mut supervisor);
        }
    }
    assert!(
        tcp_tx.try_receive_job().is_none(),
        "the serialized first hop must not skip the lower-numbered LoRa interface"
    );
    let routed = routed?;
    let InterfaceTxJob::Ordinary(job) = routed else {
        panic!("forwarded announce changed owner family")
    };
    let (_ticket, job) = job.into_parts();
    let prepared = job.prepared();
    Some((prepared.target(), prepared.interface()))
}

fn admission() -> OrdinaryRouterAdmission {
    OrdinaryRouterAdmission::new(TxLeaseDeadline::new(MonotonicMillis::new(100_000)))
}

fn requirements(tag: u8) -> TxPermitRequirements {
    TxPermitRequirements::try_new(TxPermitResourceId::new([tag; 16]), 1)
        .expect("permit requirements are nonzero")
}

fn drain_and_discard_application_events<const SLOTS: usize, const DEPTH: usize>(
    supervisor: &mut TestSupervisor<SLOTS, DEPTH>,
) -> ApplicationEventOfferReport {
    let mut slots = [const { ApplicationEventSlot::new() }; 16];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let NodeInterfaceApplicationEventDrain::Drained(report) =
        supervisor.drain_application_events(&mut owner)
    else {
        panic!("ready test events must fit the temporary owner")
    };
    while let Some(lease) = owner.lease_next() {
        lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
    }
    report
}

#[test]
fn shared_queue_pressure_does_not_starve_the_other_family() {
    let (mut supervisor, [actor], destination) = build::<1, 1>(1);
    register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (mut interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let mut rng = CounterRng::default();
    let data_hop = prepare_data(&mut supervisor, destination, b"shared queue DATA", &mut rng);
    supervisor
        .try_offer_actions(standalone_announce(30, &mut rng), admission())
        .unwrap_or_else(|failure| panic!("ordinary offer: {:?}", failure.reason()));

    let first = supervisor.step(MonotonicMillis::new(10));
    assert_eq!(
        first.transition(),
        NodeInterfaceSupervisorTransition::Data(DataRouterStep::Routed(data_hop))
    );

    let mut packet_staged = false;
    for _ in 0..16 {
        match supervisor.step(MonotonicMillis::new(10)).transition() {
            NodeInterfaceSupervisorTransition::Ordinary(
                OrdinaryRouterStep::NonPacketActionsReady,
            ) => {
                let _ = drain_and_discard_application_events(&mut supervisor);
            }
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::PacketStaged {
                ..
            }) => packet_staged = true,
            _ => {}
        }
        if packet_staged {
            break;
        }
    }
    assert!(packet_staged);
    assert!(matches!(
        interface.try_receive_job(),
        Some(InterfaceTxJob::Data(_))
    ));

    let mut ordinary_routed = false;
    for _ in 0..16 {
        if matches!(
            supervisor.step(MonotonicMillis::new(10)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Routed { .. })
        ) {
            ordinary_routed = true;
            break;
        }
    }
    assert!(ordinary_routed);
    assert!(matches!(
        interface.try_receive_job(),
        Some(InterfaceTxJob::Ordinary(_))
    ));
}

#[test]
fn retained_path_timeout_allowance_uses_authoritative_interface_serialization() {
    let (mut supervisor, [actor], destination) = build::<1, 1>(39);
    assert_eq!(
        supervisor.retained_path_first_hop_serialization_ms(&destination),
        None,
        "a retained path is not usable before an interface is online"
    );
    supervisor
        .router
        .register(
            actor.queue_id(),
            PacketInterfaceId::new(2),
            InterfaceProperties::new(
                LogicalMtu::try_new(500).unwrap(),
                InterfaceConfigId::new(1),
                Some(AdvertisedBitrate::try_new(5_468).unwrap()),
                InterfaceCost::new(0),
            ),
            true,
        )
        .unwrap();
    assert_eq!(
        supervisor.retained_path_first_hop_serialization_ms(&destination),
        Some(732),
        "500-byte E290 frames at 5,468 bps need a rounded-up 732 ms allowance"
    );
}

#[test]
fn offline_tcp_pinned_path_cannot_capture_fresh_lora_path_discovery() {
    let sender_tag = 40;
    let receiver_tag = sender_tag + 1;
    let mut receiver = NodeCore::<8, 4, 8, 8, 0>::new(
        identity(receiver_tag),
        "reticulum",
        &["offline-tcp-path-receiver"],
        NodeInstanceId::new([receiver_tag.wrapping_add(0x80); 16]),
        NodeConfig::endpoint(),
    )
    .expect("path receiver must construct");
    let destination = receiver.destination_hash();
    let mut sender = node(sender_tag, "offline-tcp-path-sender");
    let mut rng = CounterRng::default();
    receiver
        .queue_announce(None, AnnounceEmissionTime::new(1).unwrap(), &mut rng)
        .expect("receiver announce must queue");
    let announce = receiver.flush_announces(MonotonicSeconds::new(1), &mut rng);
    sender
        .ingest(
            announce.packets[0].bytes(),
            MonotonicSeconds::new(2),
            PacketInterfaceId::new(2),
            &mut rng,
        )
        .expect("sender must learn a path received on TCP");

    let (mut supervisor, [lora_actor, tcp_actor], _) = build_from_node::<2, 1>(sender, destination);
    let lora_descriptor = supervisor
        .router
        .register(
            lora_actor.queue_id(),
            PacketInterfaceId::new(1),
            properties(1).with_topology(InterfaceTopology::SharedMedium),
            true,
        )
        .expect("LoRa interface must register online");
    let tcp_descriptor = supervisor
        .router
        .register(
            tcp_actor.queue_id(),
            PacketInterfaceId::new(2),
            properties(2).with_topology(InterfaceTopology::PointToPoint),
            true,
        )
        .expect("TCP interface must register online");
    let (lora_interface, _lora_data, _lora_ordinary) = lora_actor.into_parts();
    let (tcp_interface, _tcp_data, _tcp_ordinary) = tcp_actor.into_parts();
    let (mut lora_tx, _lora_ingress, _lora_lifecycle) = lora_interface.into_parts();
    let (mut tcp_tx, _tcp_ingress, mut tcp_lifecycle) = tcp_interface.into_parts();

    assert_eq!(
        supervisor.node.retained_path_target(&destination),
        Some(NodeTxTarget::Only(PacketInterfaceId::new(2)))
    );
    assert!(supervisor.has_usable_path(&destination));

    let offline = tcp_lifecycle
        .try_request_state(tcp_descriptor.lease(), InterfaceLifecycleState::Offline)
        .expect("TCP Offline request must fit");
    let mut offline_applied = false;
    for _ in 0..16 {
        if let NodeInterfaceSupervisorTransition::Lifecycle(transition) =
            supervisor.step(MonotonicMillis::new(3)).transition()
            && transition.acknowledgement().request() == offline
        {
            assert!(
                !transition
                    .acknowledgement()
                    .result()
                    .expect("TCP Offline request must apply")
                    .is_online()
            );
            offline_applied = true;
            break;
        }
    }
    assert!(offline_applied);
    assert!(
        !tcp_lifecycle
            .try_finish_request()
            .expect("TCP Offline acknowledgement must correlate")
            .expect("TCP Offline acknowledgement must be retained")
            .is_online()
    );
    assert!(supervisor.has_path(&destination));
    assert!(
        !supervisor.has_usable_path(&destination),
        "the retained TCP path must not resolve through an offline interface"
    );

    let path_request = supervisor
        .request_path(&destination, &mut rng)
        .expect("offline pinned path must permit fresh discovery");
    assert_eq!(path_request.packets.len(), 1);
    assert_eq!(path_request.packets[0].target(), RnsTxTarget::All);
    supervisor
        .try_offer_actions(path_request, admission())
        .unwrap_or_else(|failure| {
            panic!(
                "fresh path discovery must enter the ordinary router: {:?}",
                failure.reason()
            )
        });

    let mut routed_lora = false;
    for _ in 0..24 {
        if let NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Routed {
            interface,
            ..
        }) = supervisor.step(MonotonicMillis::new(4)).transition()
        {
            assert_eq!(interface, lora_descriptor.lease().interface());
            routed_lora = true;
            break;
        }
    }
    assert!(routed_lora);
    let InterfaceTxJob::Ordinary(lora_job) = lora_tx
        .try_receive_job()
        .expect("fresh path discovery must reach LoRa")
    else {
        panic!("fresh path discovery changed packet family")
    };
    let (ticket, lora_job) = lora_job.into_parts();
    let completion = ticket
        .complete(lora_job.cancel(TxCompletionCode::new(0x782)))
        .expect("LoRa completion ticket must remain exact");
    lora_tx
        .try_send_completion(completion)
        .expect("LoRa completion must fit");

    let mut returned = false;
    for _ in 0..24 {
        match supervisor.step(MonotonicMillis::new(5)).transition() {
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Completion(
                OrdinaryRouterCompletionProgress::Returned { .. },
            )) => {
                returned = true;
                break;
            }
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Routed {
                interface,
                ..
            }) if interface == tcp_descriptor.lease().interface() => {
                panic!("offline TCP captured fresh path discovery")
            }
            _ => {}
        }
    }
    assert!(
        returned,
        "LoRa was the fresh discovery's only eligible route"
    );
    assert!(tcp_tx.try_receive_job().is_none());
}

#[test]
fn oversized_application_batch_stays_exact_while_ordinary_packet_progresses() {
    let (mut supervisor, [actor], _) = build::<1, 1>(2);
    register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (mut interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let mut rng = CounterRng::default();
    let payload = vec![0x51, 0x52, 0x53];
    let payload_pointer = payload.as_ptr();
    let mut announce = standalone_announce(32, &mut rng);
    let actions = NodeActions::without_retained_proofs(
        vec![ApplicationEvent::DataReceived {
            destination: [0x41; 16],
            payload,
            ingress: None,
        }],
        core::mem::take(&mut announce.packets),
        announce.unroutable_packets,
    );
    supervisor
        .try_offer_actions(actions, admission())
        .unwrap_or_else(|failure| panic!("ordinary offer: {:?}", failure.reason()));

    let mut non_packet_ready = false;
    for _ in 0..16 {
        if matches!(
            supervisor.step(MonotonicMillis::new(10)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            non_packet_ready = true;
            break;
        }
    }
    assert!(non_packet_ready);

    let mut no_slots: [ApplicationEventSlot; 0] = [];
    let mut zero_capacity = ApplicationEventOwner::new(&mut no_slots);
    assert_eq!(
        supervisor.drain_application_events(&mut zero_capacity),
        NodeInterfaceApplicationEventDrain::Retained(ApplicationEventOfferError::BatchTooLarge {
            required: 1,
            capacity: 0,
        })
    );

    let mut packet_progressed = false;
    let mut routed_job = None;
    for _ in 0..16 {
        if matches!(
            supervisor.step(MonotonicMillis::new(11)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::PacketStaged { .. })
                | NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Routed { .. })
        ) {
            packet_progressed = true;
        }
        if let Some(job) = interface.try_receive_job() {
            routed_job = Some(job);
            break;
        }
    }
    assert!(packet_progressed);

    let InterfaceTxJob::Ordinary(job) =
        routed_job.expect("ordinary packet reaches its actor despite event pressure")
    else {
        panic!("ordinary packet changed owner family")
    };
    let (ticket, job) = job.into_parts();
    let completion = ticket
        .complete(
            job.return_unpermitted()
                .complete(TxCompletionCode::new(0x734)),
        )
        .expect("ticket binds the exact ordinary completion");
    interface
        .try_send_completion(completion)
        .expect("completion queue has capacity");
    let mut completion_accepted = false;
    let mut completion_reconciled = false;
    for _ in 0..16 {
        match supervisor.step(MonotonicMillis::new(12)).transition() {
            NodeInterfaceSupervisorTransition::CompletionAccepted {
                family: NodeInterfaceCompletionFamily::Ordinary,
                ..
            } => completion_accepted = true,
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Completion(_)) => {
                completion_reconciled = true;
            }
            _ => {}
        }
        if completion_accepted && completion_reconciled {
            break;
        }
    }
    assert!(completion_accepted);
    assert!(completion_reconciled);

    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    let NodeInterfaceApplicationEventDrain::Drained(report) =
        supervisor.drain_application_events(&mut owner)
    else {
        panic!("retained event must drain after replacing owner capacity")
    };
    assert_eq!(report.accepted_events(), 1);
    let lease = owner.lease_next().expect("exact retained event is ready");
    let ApplicationEvent::DataReceived { payload, .. } = lease.event() else {
        panic!("application event changed variant")
    };
    assert_eq!(payload.as_ptr(), payload_pointer);
    let _ = lease.acknowledge();
}

#[test]
fn full_application_owner_retains_supervisor_envelope_for_retry() {
    let (mut supervisor, _actors, _) = build::<1, 1>(4);
    let mut slots = [ApplicationEventSlot::new()];
    let mut owner = ApplicationEventOwner::new(&mut slots);
    owner
        .try_offer_actions(NodeActions::without_retained_proofs(
            vec![ApplicationEvent::Tick {
                expired_paths: 1,
                closed_links: 0,
            }],
            vec![],
            0,
        ))
        .expect("first event fills the owner");

    supervisor
        .try_offer_actions(
            NodeActions::without_retained_proofs(
                vec![ApplicationEvent::DataReceived {
                    destination: [0x61; 16],
                    payload: vec![0x71, 0x72],
                    ingress: None,
                }],
                vec![],
                0,
            ),
            admission(),
        )
        .unwrap_or_else(|failure| panic!("ordinary offer: {:?}", failure.reason()));
    assert!(
        supervisor.ordinary_application_events_pending(),
        "the offered event must remain visible before ordinary admission"
    );
    for _ in 0..16 {
        if matches!(
            supervisor.step(MonotonicMillis::new(10)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            break;
        }
    }
    assert_eq!(
        supervisor.drain_application_events(&mut owner),
        NodeInterfaceApplicationEventDrain::Retained(ApplicationEventOfferError::Full {
            required: 1,
            available: 0,
        })
    );
    assert!(
        supervisor.ordinary_application_events_pending(),
        "owner pressure must not hide the retained event"
    );
    owner
        .lease_next()
        .expect("existing event remains ready")
        .discard(ApplicationEventDiscardReason::MaintenanceCoalesced);
    assert!(matches!(
        supervisor.drain_application_events(&mut owner),
        NodeInterfaceApplicationEventDrain::Drained(report)
            if report.accepted_events() == 1
    ));
    assert!(
        !supervisor.ordinary_application_events_pending(),
        "the supervisor no longer owns an event after a successful drain"
    );
    let lease = owner.lease_next().expect("retried event is ready");
    let ApplicationEvent::DataReceived { payload, .. } = lease.event() else {
        panic!("application event changed variant")
    };
    assert_eq!(payload.as_slice(), [0x71, 0x72]);
    lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
    assert_eq!(owner.counters().full_rejections, 1);
}

#[test]
fn stale_router_completion_enters_data_recovery_and_reconciles() {
    let (mut supervisor, [actor], destination) = build::<1, 1>(3);
    let [descriptor] = register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (mut interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let mut rng = CounterRng::default();
    let hop = prepare_data(&mut supervisor, destination, b"stale completion", &mut rng);
    assert_eq!(
        supervisor.step(MonotonicMillis::new(10)).transition(),
        NodeInterfaceSupervisorTransition::Data(DataRouterStep::Routed(hop))
    );
    let InterfaceTxJob::Data(job) = interface
        .try_receive_job()
        .expect("DATA actor must receive its job")
    else {
        panic!("DATA route changed family")
    };
    let (ticket, job) = job.into_parts();
    let completion = job
        .return_unpermitted()
        .complete(TxCompletionCode::new(0x720));
    let completion = ticket
        .complete(completion)
        .expect("ticket must bind its exact completion");
    supervisor
        .reconfigure_interface(descriptor.lease(), properties(99), true)
        .expect("test reconfiguration must advance generation");
    interface
        .try_send_completion(completion)
        .expect("actor completion queue has capacity");

    let accepted = supervisor.step(MonotonicMillis::new(11));
    assert!(matches!(
        accepted.transition(),
        NodeInterfaceSupervisorTransition::CompletionAccepted {
            family: NodeInterfaceCompletionFamily::Data,
            origin: NodeInterfaceCompletionOrigin::Recovery(CompletionRouteError::StaleLease(
                InterfaceLeaseError::Stale { .. }
            )),
        }
    ));
    assert!(matches!(
        supervisor.step(MonotonicMillis::new(11)).transition(),
        NodeInterfaceSupervisorTransition::Data(DataRouterStep::Completion(_))
    ));
    assert_eq!(supervisor.data_parked_counts().available(), 1);
}

#[test]
fn queued_local_announce_flushes_into_the_owned_ordinary_path() {
    let (mut supervisor, [actor], _) = build::<1, 1>(12);
    register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (mut interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let mut rng = CounterRng::default();
    let destination = supervisor.destination_hash();
    supervisor
        .queue_announce_for(
            &destination,
            Some(b"local announce"),
            AnnounceEmissionTime::new(20).unwrap(),
            &mut rng,
        )
        .expect("local announce queues");
    assert!(matches!(
        supervisor.flush_announces(MonotonicSeconds::new(20), admission(), &mut rng),
        NodeInterfaceAnnounceFlushResult::Accepted
    ));

    let mut routed = false;
    for _ in 0..16 {
        match supervisor.step(MonotonicMillis::new(20)).transition() {
            NodeInterfaceSupervisorTransition::Ordinary(
                OrdinaryRouterStep::NonPacketActionsReady,
            ) => {
                let _ = drain_and_discard_application_events(&mut supervisor);
            }
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Routed { .. }) => {
                routed = true;
                break;
            }
            _ => {}
        }
    }
    assert!(routed);
    assert!(matches!(
        interface.try_receive_job(),
        Some(InterfaceTxJob::Ordinary(_))
    ));
}

#[test]
fn pressured_announce_flush_returns_exact_actions_for_retry() {
    let (mut supervisor, [actor], _) = build::<1, 1>(14);
    register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (mut interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let mut rng = CounterRng::default();
    supervisor
        .queue_announce(None, AnnounceEmissionTime::new(20).unwrap(), &mut rng)
        .expect("first announce queues");
    assert!(matches!(
        supervisor.flush_announces(MonotonicSeconds::new(20), admission(), &mut rng),
        NodeInterfaceAnnounceFlushResult::Accepted
    ));
    supervisor
        .queue_announce(None, AnnounceEmissionTime::new(21).unwrap(), &mut rng)
        .expect("second announce queues");
    let failure = match supervisor.flush_announces(MonotonicSeconds::new(21), admission(), &mut rng)
    {
        NodeInterfaceAnnounceFlushResult::ActionOfferRejected(failure) => failure,
        NodeInterfaceAnnounceFlushResult::Accepted => {
            panic!("busy ordinary input unexpectedly accepted second flush")
        }
    };
    assert!(matches!(
        failure.reason(),
        NodeInterfaceOrdinaryOfferError::Coordinator(OrdinaryRouterOfferError::Busy(_))
    ));
    let (actions, returned_admission) = failure.into_parts();
    assert_eq!(actions.packets.len(), 1);

    let mut retained = Some((actions, returned_admission));
    let mut held_jobs = Vec::new();
    for _ in 0..32 {
        let (actions, admission) = retained
            .take()
            .expect("retry loop retains flushed announce actions");
        match supervisor.try_offer_actions(actions, admission) {
            Ok(()) => break,
            Err(failure) => retained = Some(failure.into_parts()),
        }
        if matches!(
            supervisor.step(MonotonicMillis::new(22)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            let _ = drain_and_discard_application_events(&mut supervisor);
        }
        if let Some(job) = interface.try_receive_job() {
            held_jobs.push(job);
        }
    }
    assert!(retained.is_none(), "flushed announce actions retry exactly");
}

#[test]
fn queued_ingress_recycles_current_and_stale_exact_buffers() {
    let (mut supervisor, [actor], _) = build::<1, 1>(16);
    let [descriptor] = register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let (_tx, mut ingress, _lifecycle) = interface.into_parts();
    let authority = ingress
        .bind_ingress(descriptor)
        .expect("current descriptor binds to ingress actor");
    let mut rng = CounterRng::default();
    let packet = native_announce_packet(63, &mut rng);
    let mut buffer = ingress
        .try_receive_buffer()
        .expect("actor owns one seeded ingress buffer");
    let first_id = buffer.id();
    buffer.capacity_mut()[..packet.len()].copy_from_slice(&packet);
    let sealed = buffer
        .seal(packet.len())
        .expect("native packet fits exact buffer");
    ingress
        .try_send(authority, sealed)
        .expect("completed packet queue has capacity");

    let processed = match supervisor.step_ingress_at(
        MonotonicSeconds::new(30),
        MonotonicInstant::from_micros(30_125_000),
        admission(),
        &mut rng,
    ) {
        NodeInterfaceIngressStep::Processed(processed) => processed,
        _ => panic!("current queued ingress must process"),
    };
    assert_eq!(processed.buffer(), first_id);
    assert_eq!(processed.recycle_fault(), None);
    assert_eq!(processed.actions_backpressured(), None);
    assert!(processed.into_report().actions.events.is_empty());
    let mut recycled = ingress
        .try_receive_buffer()
        .expect("processed exact buffer must recycle immediately");
    assert_eq!(recycled.id(), first_id);

    supervisor
        .reconfigure_interface(descriptor.lease(), properties(90), true)
        .expect("test-only generation change succeeds");
    let stale_bytes = [0x01, 0x02, 0x03];
    recycled.capacity_mut()[..stale_bytes.len()].copy_from_slice(&stale_bytes);
    let stale = recycled
        .seal(stale_bytes.len())
        .expect("stale test packet seals");
    ingress
        .try_send(authority, stale)
        .expect("stale authority is checked by the router, not actor send");
    assert!(matches!(
        supervisor.step_ingress(
            MonotonicSeconds::new(31),
            admission(),
            &mut rng,
        ),
        NodeInterfaceIngressStep::RouteRejected {
            reason: IngressRouteError::StaleLease(InterfaceLeaseError::Stale { .. }),
            buffer,
            recycle_fault: None,
        } if buffer == first_id
    ));
    assert_eq!(
        ingress
            .try_receive_buffer()
            .expect("stale exact buffer also recycles")
            .id(),
        first_id
    );
}

#[test]
fn queued_announce_preserves_optional_interface_signal_into_application_event() {
    let (mut supervisor, [actor], _) = build::<1, 1>(17);
    let [descriptor] = register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let (_tx, mut ingress, _lifecycle) = interface.into_parts();
    let authority = ingress
        .bind_ingress(descriptor)
        .expect("current descriptor binds to ingress actor");
    let mut rng = CounterRng::default();
    let signal = RouterIngressSignalObservation::new(-92, 6);

    for (tag, expected_signal) in [(67, Some(signal)), (68, None)] {
        let packet = native_announce_packet(tag, &mut rng);
        let mut buffer = ingress
            .try_receive_buffer()
            .expect("the previous exact ingress buffer must be available");
        buffer.capacity_mut()[..packet.len()].copy_from_slice(&packet);
        let sealed = buffer
            .seal_with_signal(packet.len(), expected_signal)
            .expect("native announce fits the exact buffer");
        ingress
            .try_send(authority, sealed)
            .expect("completed ingress queue has capacity");

        let processed =
            match supervisor.step_ingress(MonotonicSeconds::new(40), admission(), &mut rng) {
                NodeInterfaceIngressStep::Processed(processed) => processed,
                _ => panic!("queued announce must process"),
            };
        assert_eq!(processed.recycle_fault(), None);
        assert_eq!(processed.actions_backpressured(), None);
        drop(processed.into_report());

        let mut non_packet_ready = false;
        for _ in 0..16 {
            if matches!(
                supervisor.step(MonotonicMillis::new(40_000)).transition(),
                NodeInterfaceSupervisorTransition::Ordinary(
                    OrdinaryRouterStep::NonPacketActionsReady
                )
            ) {
                non_packet_ready = true;
                break;
            }
        }
        assert!(non_packet_ready, "announce event must reach the drain edge");

        let mut slots = [ApplicationEventSlot::new()];
        let mut owner = ApplicationEventOwner::new(&mut slots);
        assert!(matches!(
            supervisor.drain_application_events(&mut owner),
            NodeInterfaceApplicationEventDrain::Drained(report)
                if report.accepted_events() == 1
        ));
        let lease = owner.lease_next().expect("announce event must be ready");
        let ApplicationEvent::AnnounceReceived {
            ingress: Some(observation),
            ..
        } = lease.event()
        else {
            panic!("announce event must retain ingress observation")
        };
        assert_eq!(
            observation.interface().0,
            descriptor.lease().interface().get()
        );
        assert_eq!(
            observation
                .signal()
                .map(|signal| { (signal.rssi_dbm(), signal.snr_db()) }),
            expected_signal.map(|signal| (signal.rssi_dbm(), signal.snr_db()))
        );
        lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
    }
}

#[test]
fn queued_ingress_action_pressure_recycles_buffer_and_retries_exact_actions() {
    let (mut supervisor, [actor], _) = build::<1, 1>(18);
    let [descriptor] = register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let (mut tx, mut ingress, _lifecycle) = interface.into_parts();
    let authority = ingress
        .bind_ingress(descriptor)
        .expect("current descriptor binds to ingress actor");
    let mut rng = CounterRng::default();
    supervisor
        .try_offer_actions(standalone_announce(64, &mut rng), admission())
        .unwrap_or_else(|failure| panic!("first offer: {:?}", failure.reason()));
    let packet = native_announce_packet(65, &mut rng);
    let mut buffer = ingress
        .try_receive_buffer()
        .expect("actor owns a reusable ingress buffer");
    let buffer_id = buffer.id();
    buffer.capacity_mut()[..packet.len()].copy_from_slice(&packet);
    let sealed = buffer.seal(packet.len()).expect("packet seals");
    ingress
        .try_send(authority, sealed)
        .expect("completed ingress queue has capacity");

    let processed = match supervisor.step_ingress(MonotonicSeconds::new(40), admission(), &mut rng)
    {
        NodeInterfaceIngressStep::Processed(processed) => processed,
        _ => panic!("queued ingress must process despite ordinary pressure"),
    };
    assert!(matches!(
        processed.actions_backpressured(),
        Some(NodeInterfaceOrdinaryOfferError::Coordinator(
            OrdinaryRouterOfferError::Busy(_)
        ))
    ));
    assert_eq!(processed.recycle_fault(), None);
    assert_eq!(processed.terminal_action_fault(), None);
    let action_free_report = processed.into_report();
    assert_eq!(
        action_free_report.metadata.wire_packet_type(),
        Some(PacketType::Announce),
        "metadata must survive after the exact action envelope moves into retry ownership"
    );
    assert!(action_free_report.actions.events.is_empty());
    assert!(action_free_report.actions.packets.is_empty());
    assert_eq!(action_free_report.actions.unroutable_packets, 0);
    assert!(supervisor.ingress_actions_pending());
    assert_eq!(supervisor.terminal_ingress_action_fault(), None);
    let retained = supervisor
        .pending_ingress_actions
        .as_ref()
        .expect("busy ingress actions remain exactly retained");
    assert!(!retained.actions.events.is_empty());
    let retained_events = retained.actions.events.as_ptr();
    let retained_event_count = retained.actions.events.len();
    let retained_packet_count = retained.actions.packets.len();
    let retained_unroutable = retained.actions.unroutable_packets;
    assert_eq!(
        ingress
            .try_receive_buffer()
            .expect("ordinary pressure cannot strand RX buffer")
            .id(),
        buffer_id
    );
    assert!(matches!(
        supervisor.step_ingress(MonotonicSeconds::new(41), admission(), &mut rng,),
        NodeInterfaceIngressStep::ActionsBackpressured(
            NodeInterfaceOrdinaryOfferError::Coordinator(OrdinaryRouterOfferError::Busy(_))
        )
    ));
    let retried = supervisor
        .pending_ingress_actions
        .as_ref()
        .expect("retryable busy failure remains retained");
    assert_eq!(retried.actions.events.as_ptr(), retained_events);
    assert_eq!(retried.actions.events.len(), retained_event_count);
    assert_eq!(retried.actions.packets.len(), retained_packet_count);
    assert_eq!(retried.actions.unroutable_packets, retained_unroutable);
    assert_eq!(supervisor.terminal_ingress_action_fault(), None);

    let mut held_jobs = Vec::new();
    for _ in 0..32 {
        if matches!(
            supervisor.step(MonotonicMillis::new(41)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            let _ = drain_and_discard_application_events(&mut supervisor);
        }
        if let Some(job) = tx.try_receive_job() {
            held_jobs.push(job);
        }
        if matches!(
            supervisor.step_ingress(MonotonicSeconds::new(41), admission(), &mut rng,),
            NodeInterfaceIngressStep::RetainedActionsAdmitted
        ) {
            break;
        }
    }
    assert!(!supervisor.ingress_actions_pending());
    assert_eq!(supervisor.terminal_ingress_action_fault(), None);
}

#[test]
fn point_to_point_ingress_broadcast_excludes_its_source_interface() {
    let (target, first_interface) =
        first_forwarded_announce_job(true, false).expect("Full-mode announce forwards");

    assert_eq!(target, NodeTxTarget::AllExcept(PacketInterfaceId::new(2)));
    assert_eq!(first_interface, PacketInterfaceId::new(1));
}

#[test]
fn shared_medium_ingress_broadcast_preserves_same_interface_relay() {
    let (target, first_interface) =
        first_forwarded_announce_job(false, false).expect("Full-mode announce forwards");

    assert_eq!(target, NodeTxTarget::All);
    assert_eq!(
        first_interface,
        PacketInterfaceId::new(1),
        "the shared-medium ingress slot remains a valid first relay hop"
    );
}

#[test]
fn boundary_tcp_announce_does_not_enter_internal_lora() {
    assert_eq!(
        first_forwarded_announce_job(true, true),
        None,
        "the exact Boundary ingress announce has no eligible Internal LoRa egress"
    );
}

#[test]
fn internal_lora_announce_can_relay_locally_and_cross_boundary() {
    let (target, first_interface) =
        first_forwarded_announce_job(false, true).expect("Internal announce forwards");
    assert_eq!(target, NodeTxTarget::All);
    assert_eq!(first_interface, PacketInterfaceId::new(1));
}

#[test]
fn oversized_queued_ingress_actions_are_terminal_and_takeable_without_loss() {
    let (mut supervisor, [actor], _) = build::<1, 1>(19);
    let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let (_tx, mut ingress, _lifecycle) = interface.into_parts();
    let mut buffer = ingress
        .try_receive_buffer()
        .expect("actor owns its seeded ingress buffer");
    let buffer_id = buffer.id();
    buffer.capacity_mut()[0] = 0x01;
    let packet = buffer.seal(1).expect("ownership fixture seals");

    let mut actions = NodeActions::default();
    for tag in [80, 81, 82] {
        let mut announce = standalone_announce(tag, &mut CounterRng::default());
        assert_eq!(announce.packets.len(), 1);
        actions.packets.append(&mut announce.packets);
    }
    let expected_packets: Vec<Vec<u8>> = actions
        .packets
        .iter()
        .map(|packet| packet.bytes().to_vec())
        .collect();
    let expected_deadline = admission().deadline();
    let report = IngressReport {
        disposition: IngressDisposition::Processed,
        metadata: Default::default(),
        actions,
    };

    let processed = match supervisor.finish_queued_ingress(
        Ok(report),
        packet,
        OrdinaryRouterAdmission::new(expected_deadline),
    ) {
        NodeInterfaceIngressStep::Processed(processed) => processed,
        _ => panic!("queued ingress completion must report its terminal action owner"),
    };
    let expected_fault = NodeInterfaceIngressActionFault::EnvelopeExceedsPool {
        packet_count: 3,
        limit: ORDINARY_BUFFERS,
    };
    assert_eq!(processed.buffer(), buffer_id);
    assert_eq!(processed.recycle_fault(), None);
    assert_eq!(processed.actions_backpressured(), None);
    assert_eq!(processed.terminal_action_fault(), Some(expected_fault));
    assert_eq!(
        ingress
            .try_receive_buffer()
            .expect("terminal action failure cannot strand the exact RX buffer")
            .id(),
        buffer_id
    );
    assert!(!supervisor.ingress_actions_pending());
    assert_eq!(
        supervisor.terminal_ingress_action_fault(),
        Some(expected_fault)
    );
    assert!(matches!(
        supervisor.step_ingress(
            MonotonicSeconds::new(42),
            admission(),
            &mut CounterRng::default(),
        ),
        NodeInterfaceIngressStep::TerminalActionsPending(fault) if fault == expected_fault
    ));

    let terminal = supervisor
        .take_terminal_ingress_actions()
        .expect("firmware can recover the exact terminal envelope");
    assert_eq!(terminal.fault(), expected_fault);
    let (recovered, recovered_admission) = terminal.into_parts();
    assert_eq!(recovered_admission.deadline(), expected_deadline);
    assert_eq!(recovered.packets.len(), expected_packets.len());
    for (packet, expected) in recovered.packets.iter().zip(expected_packets) {
        assert_eq!(packet.bytes(), expected);
    }
    assert_eq!(supervisor.terminal_ingress_action_fault(), None);
}

#[test]
fn every_nonretryable_ingress_action_error_enters_terminal_recovery() {
    let (mut supervisor, _actors, _) = build::<1, 1>(21);
    let cases = [
        (
            NodeInterfaceOrdinaryOfferError::Fault(NodeInterfaceSupervisorFault::DataOwnerMismatch),
            NodeInterfaceIngressActionFault::Aggregate(
                NodeInterfaceSupervisorFault::DataOwnerMismatch,
            ),
        ),
        (
            NodeInterfaceOrdinaryOfferError::Coordinator(OrdinaryRouterOfferError::Disabled(
                OrdinaryRouterFault::InternalInvariant,
            )),
            NodeInterfaceIngressActionFault::CoordinatorDisabled(
                OrdinaryRouterFault::InternalInvariant,
            ),
        ),
        (
            NodeInterfaceOrdinaryOfferError::Coordinator(
                OrdinaryRouterOfferError::EnvelopeExceedsPool {
                    packet_count: 9,
                    limit: 2,
                },
            ),
            NodeInterfaceIngressActionFault::EnvelopeExceedsPool {
                packet_count: 9,
                limit: 2,
            },
        ),
    ];

    for (index, (reason, expected)) in cases.into_iter().enumerate() {
        let failure = NodeInterfaceOrdinaryOfferFailure {
            reason,
            actions: NodeActions::without_retained_proofs(vec![], vec![], index + 1),
            admission: admission(),
        };
        assert!(matches!(
            supervisor.retain_ingress_action_failure(failure),
            IngressActionRetention::Terminal(fault) if fault == expected
        ));
        assert_eq!(supervisor.terminal_ingress_action_fault(), Some(expected));
        let terminal = supervisor
            .take_terminal_ingress_actions()
            .expect("each non-retryable owner is explicitly takeable");
        assert_eq!(terminal.fault(), expected);
        let (actions, _) = terminal.into_parts();
        assert_eq!(actions.unroutable_packets, index + 1);
    }
}

#[test]
fn correlation_rejection_recycles_exact_buffer_before_returning() {
    let (mut supervisor, [actor], _) = build::<1, 1>(20);
    let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let (_tx, mut ingress, _lifecycle) = interface.into_parts();
    let mut buffer = ingress
        .try_receive_buffer()
        .expect("actor owns its seeded exact buffer");
    let buffer_id = buffer.id();
    buffer.capacity_mut()[0] = 0x01;
    let packet = buffer.seal(1).expect("one-byte ownership fixture seals");
    assert!(matches!(
        supervisor.finish_queued_ingress(
            Err(ReceiptCorrelationError::ReservationInvariant),
            packet,
            admission(),
        ),
        NodeInterfaceIngressStep::CorrelationRejected {
            reason: ReceiptCorrelationError::ReservationInvariant,
            buffer,
            recycle_fault: None,
        } if buffer == buffer_id
    ));
    assert_eq!(
        ingress
            .try_receive_buffer()
            .expect("correlation failure must recycle exact buffer")
            .id(),
        buffer_id
    );
}

#[test]
fn permanent_recycle_failure_is_terminal_and_exact_buffer_is_takeable() {
    let (mut supervisor, _actors, _) = build::<1, 1>(22);
    let foreign_fabric = Box::leak(Box::new(InterfaceFabric::<NoopRawMutex, 1, 1>::new()));
    let (mut foreign_router, [foreign_actor]) = foreign_fabric.split();
    let (_foreign_tx, mut foreign_ingress, _foreign_lifecycle) = foreign_actor.into_parts();
    let mut buffer = foreign_ingress
        .try_receive_buffer()
        .expect("foreign actor owns its seeded buffer");
    let buffer_id = buffer.id();
    buffer.capacity_mut()[0] = 0x01;
    let packet = buffer.seal(1).expect("foreign packet seals");
    let initial = supervisor.finish_queued_ingress(
        Err(ReceiptCorrelationError::ReservationInvariant),
        packet,
        admission(),
    );
    let fault = match &initial {
        NodeInterfaceIngressStep::CorrelationRejected {
            reason: ReceiptCorrelationError::ReservationInvariant,
            buffer,
            recycle_fault: Some(fault),
        } if *buffer == buffer_id => *fault,
        _ => panic!("initial correlation result must expose terminal recycle status"),
    };
    assert_eq!(initial.terminal_recycle_fault(), Some(fault));
    assert_eq!(fault.buffer(), buffer_id);
    assert!(matches!(
        fault.reason(),
        IngressBufferReturnError::ForeignFabricOrigin(_)
    ));
    assert!(fault.is_terminal());
    assert_eq!(supervisor.ingress_recycle_fault(), None);
    assert_eq!(supervisor.terminal_ingress_recycle_fault(), Some(fault));
    let mut rng = CounterRng::default();
    assert!(matches!(
        supervisor.step_ingress(
            MonotonicSeconds::new(50),
            admission(),
            &mut rng,
        ),
        NodeInterfaceIngressStep::TerminalRecyclePending(observed) if observed == fault
    ));
    assert_eq!(supervisor.terminal_ingress_recycle_fault(), Some(fault));
    let (taken_fault, packet) = supervisor
        .take_terminal_ingress_buffer()
        .expect("firmware can take the exact terminal packet owner");
    assert_eq!(taken_fault, fault);
    assert_eq!(packet.id(), buffer_id);
    assert_eq!(packet.as_bytes(), &[0x01]);
    assert_eq!(supervisor.terminal_ingress_recycle_fault(), None);
    foreign_router
        .try_return_ingress_buffer(packet)
        .expect("taken packet remains the exact recoverable foreign owner");
    assert_eq!(
        foreign_ingress
            .try_receive_buffer()
            .expect("foreign pool receives its recovered owner")
            .id(),
        buffer_id
    );
}

#[test]
fn queue_full_recycle_residue_is_retried_and_returns_exact_buffer() {
    let (mut supervisor, [actor], _) = build::<1, 1>(23);
    let queue = actor.queue_id();
    let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let (_tx, mut ingress, _lifecycle) = interface.into_parts();
    let mut buffer = ingress
        .try_receive_buffer()
        .expect("actor owns its seeded exact buffer");
    let buffer_id = buffer.id();
    buffer.capacity_mut()[..2].copy_from_slice(&[0x02, 0x03]);
    let packet = buffer.seal(2).expect("queue-full fixture seals");
    let status = match supervisor
        .retain_ingress_recycle(IngressBufferReturnError::QueueFull(queue), packet)
    {
        IngressRecycleRetention::Retryable(status) => status,
        IngressRecycleRetention::Terminal(_) => panic!("queue-full must remain retryable"),
    };
    assert!(status.is_retryable());
    assert_eq!(status.buffer(), buffer_id);
    assert_eq!(supervisor.ingress_recycle_fault(), Some(status));
    assert_eq!(supervisor.terminal_ingress_recycle_fault(), None);

    assert!(matches!(
        supervisor.step_ingress(
            MonotonicSeconds::new(51),
            admission(),
            &mut CounterRng::default(),
        ),
        NodeInterfaceIngressStep::RecycleRetried(buffer) if buffer == buffer_id
    ));
    assert_eq!(supervisor.ingress_recycle_fault(), None);
    assert_eq!(
        ingress
            .try_receive_buffer()
            .expect("retry returns the exact buffer to its actor")
            .id(),
        buffer_id
    );
}

#[test]
fn terminal_recycle_status_is_visible_on_every_initial_ingress_result() {
    let (mut supervisor, [actor], _) = build::<1, 1>(24);
    let queue = actor.queue_id();
    let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let (_tx, mut ingress, _lifecycle) = interface.into_parts();
    let mut buffer = ingress
        .try_receive_buffer()
        .expect("actor owns its seeded status-fixture buffer");
    let buffer_id = buffer.id();
    buffer.capacity_mut()[0] = 0x01;
    supervisor
        .router
        .try_return_ingress_buffer(buffer.seal(1).expect("status fixture seals"))
        .expect("status fixture buffer returns before synthetic observations");
    let fault = NodeInterfaceIngressRecycleFault {
        reason: IngressBufferReturnError::ForeignFabricOrigin(queue),
        buffer: buffer_id,
    };
    let observations = [
        NodeInterfaceIngressStep::Processed(NodeInterfaceQueuedIngressProcessed {
            buffer: buffer_id,
            report: IngressReport {
                disposition: IngressDisposition::Processed,
                metadata: Default::default(),
                actions: NodeActions::default(),
            },
            actions_backpressured: None,
            terminal_action_fault: None,
            recycle_fault: Some(fault),
        }),
        NodeInterfaceIngressStep::RouteRejected {
            reason: IngressRouteError::ForeignFabricOrigin(queue),
            buffer: buffer_id,
            recycle_fault: Some(fault),
        },
        NodeInterfaceIngressStep::CorrelationRejected {
            reason: ReceiptCorrelationError::ReservationInvariant,
            buffer: buffer_id,
            recycle_fault: Some(fault),
        },
    ];
    for observation in observations {
        assert_eq!(observation.terminal_recycle_fault(), Some(fault));
    }
}

#[test]
fn every_non_queue_full_recycle_reason_is_terminal() {
    let (mut supervisor, [actor], _) = build::<1, 1>(25);
    let queue = actor.queue_id();
    let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
    let (_tx, mut ingress, _lifecycle) = interface.into_parts();
    let mut buffer = ingress
        .try_receive_buffer()
        .expect("actor owns its seeded classification buffer");
    let buffer_id = buffer.id();
    buffer.capacity_mut()[0] = 0x01;
    supervisor
        .router
        .try_return_ingress_buffer(buffer.seal(1).expect("classification fixture seals"))
        .expect("classification fixture returns to its pool");

    for reason in [
        IngressBufferReturnError::QueueOutsideFabric(InterfaceQueueId::new(99)),
        IngressBufferReturnError::SlotOutsidePool(buffer_id),
        IngressBufferReturnError::ForeignFabricOrigin(queue),
    ] {
        let fault = NodeInterfaceIngressRecycleFault {
            reason,
            buffer: buffer_id,
        };
        assert!(fault.is_terminal());
        assert!(!fault.is_retryable());
    }
}

#[test]
fn tick_timeout_actions_are_admitted_and_reported_without_duplication() {
    let (mut supervisor, [actor], destination) = build::<1, 1>(8);
    register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (mut interface, mut data_permit, _ordinary_permit) = actor.into_parts();
    let mut rng = CounterRng::default();
    let hop = prepare_data(&mut supervisor, destination, b"tick timeout", &mut rng);
    assert_eq!(
        supervisor.step(MonotonicMillis::new(10)).transition(),
        NodeInterfaceSupervisorTransition::Data(DataRouterStep::Routed(hop))
    );
    let InterfaceTxJob::Data(job) = interface.try_receive_job().expect("DATA job reaches actor")
    else {
        panic!("DATA route changed family")
    };
    let (ticket, job) = job.into_parts();
    let (pending, request) = job.begin_permit(requirements(8));
    data_permit
        .requests()
        .try_send(request)
        .unwrap_or_else(|_| panic!("DATA permit request fits"));
    let mut reply = None;
    for _ in 0..32 {
        if let Some(received) = data_permit.replies().try_receive() {
            reply = Some(received);
            break;
        }
        let _ = supervisor.step(MonotonicMillis::new(11));
    }
    let reply = reply.expect("DATA permit reply must arrive within the bounded pass budget");
    let PermitResolution::Authorized(mut authorized) = pending
        .resolve(reply, MonotonicMillis::new(12))
        .unwrap_or_else(|_| panic!("matching permit reply must resolve"))
    else {
        panic!("allow policy must authorize before deadline")
    };
    assert!(
        !authorized
            .frame(MonotonicMillis::new(12))
            .expect("authorized frame must be available")
            .bytes()
            .is_empty()
    );
    let completion = ticket
        .complete(authorized.complete(TxCompletionCode::new(0x721)))
        .expect("ticket binds authorized completion");
    interface
        .try_send_completion(completion)
        .expect("completion queue has capacity");
    assert!(matches!(
        supervisor.step(MonotonicMillis::new(13)).transition(),
        NodeInterfaceSupervisorTransition::CompletionAccepted {
            family: NodeInterfaceCompletionFamily::Data,
            ..
        }
    ));
    assert!(matches!(
        supervisor.step(MonotonicMillis::new(13)).transition(),
        NodeInterfaceSupervisorTransition::Data(DataRouterStep::Completion(_))
    ));

    let accepted = match supervisor.tick_at(
        MonotonicSeconds::new(1_000_000),
        MonotonicInstant::from_micros(1_000_000_875_000),
        admission(),
        &mut rng,
    ) {
        NodeInterfaceTickResult::Accepted(accepted) => accepted,
        NodeInterfaceTickResult::ActionOfferRejected(_) => {
            panic!("idle ordinary coordinator accepts tick actions")
        }
    };
    let report = accepted.into_report();
    assert_eq!(report.timed_out_attempts, 1);
    assert_eq!(report.correlation_fault, None);
    assert!(report.actions.events.is_empty());
    assert!(report.actions.packets.is_empty());

    let mut events = 0;
    for _ in 0..16 {
        if matches!(
            supervisor.step(MonotonicMillis::new(14)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            events = drain_and_discard_application_events(&mut supervisor).accepted_events();
            break;
        }
    }
    assert_eq!(events, 1);
}

#[test]
fn newly_disabled_coordinator_is_surfaced_then_skipped_for_permit_drain() {
    let (mut supervisor, [actor0, actor1], _) = build::<2, 1>(10);
    let descriptor0 = supervisor
        .router
        .register(
            actor0.queue_id(),
            PacketInterfaceId::new(2),
            properties(1),
            true,
        )
        .expect("primary interface registers");
    let (mut interface0, _data0, mut ordinary0) = actor0.into_parts();
    let mut rng = CounterRng::default();
    supervisor
        .try_offer_actions(standalone_announce(70, &mut rng), admission())
        .unwrap_or_else(|failure| panic!("first ordinary offer: {:?}", failure.reason()));
    let mut first_job = None;
    for _ in 0..32 {
        if let Some(InterfaceTxJob::Ordinary(job)) = interface0.try_receive_job() {
            first_job = Some(job);
            break;
        }
        if matches!(
            supervisor.step(MonotonicMillis::new(10)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            let _ = drain_and_discard_application_events(&mut supervisor);
        }
    }
    let job = first_job.unwrap_or_else(|| {
        panic!(
            "first ordinary job did not route within the bounded pass budget; fault={:?}",
            supervisor.ordinary_fault()
        )
    });
    let (_ticket, job) = job.into_parts();
    let (_pending, request) = job.begin_permit(requirements(10));
    supervisor
        .router
        .register(
            actor1.queue_id(),
            PacketInterfaceId::new(64),
            properties(2),
            false,
        )
        .expect("out-of-profile interface can register before routing validates the profile");
    let (_interface1, _data1, _ordinary1) = actor1.into_parts();
    supervisor
        .try_offer_actions(standalone_announce(71, &mut rng), admission())
        .unwrap_or_else(|failure| panic!("second ordinary offer: {:?}", failure.reason()));

    let mut newly_disabled = None;
    for _ in 0..32 {
        let transition = supervisor.step(MonotonicMillis::new(11)).transition();
        if matches!(
            transition,
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Disabled(
                OrdinaryRouterFault::RegistryProfile(_)
            ))
        ) {
            newly_disabled = Some(transition);
            break;
        }
        if matches!(
            transition,
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            let _ = drain_and_discard_application_events(&mut supervisor);
        }
    }
    let newly_disabled = newly_disabled.unwrap_or_else(|| {
            panic!(
                "newly disabled coordinator was not surfaced within the bounded pass budget; fault={:?}",
                supervisor.ordinary_fault()
            )
        });
    assert!(matches!(
        newly_disabled,
        NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Disabled(_))
    ));
    ordinary0
        .requests()
        .try_send(request)
        .unwrap_or_else(|_| panic!("issued ordinary request fits"));

    let mut permit_advances = 0;
    let mut repeated_disabled = 0;
    for _ in 0..24 {
        match supervisor.step(MonotonicMillis::new(12)).transition() {
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Disabled(_)) => {
                repeated_disabled += 1;
            }
            NodeInterfaceSupervisorTransition::OrdinaryPermit {
                actor: 0,
                step: OrdinaryPermitServerStep::Advanced,
            } => permit_advances += 1,
            _ => {}
        }
        if permit_advances == 3 {
            break;
        }
    }
    assert_eq!(repeated_disabled, 0);
    assert_eq!(permit_advances, 3);
    assert!(ordinary0.replies().try_receive().is_some());
    assert_eq!(supervisor.policy().calls, 0);
    assert_eq!(descriptor0.lease().interface(), PacketInterfaceId::new(2));
}

#[test]
fn actor_specific_data_and_ordinary_permit_lanes_both_progress() {
    let (mut supervisor, actor_ports, destination) = build::<2, 1>(5);
    let descriptors = register(&mut supervisor, &actor_ports, [false, true]);
    let [actor0, actor1] = actor_ports;
    let (mut interface0, _data0, mut ordinary0) = actor0.into_parts();
    let (mut interface1, mut data1, _ordinary1) = actor1.into_parts();
    let mut rng = CounterRng::default();

    let data_hop = prepare_data(&mut supervisor, destination, b"actor one", &mut rng);
    assert_eq!(
        supervisor.step(MonotonicMillis::new(10)).transition(),
        NodeInterfaceSupervisorTransition::Data(DataRouterStep::Routed(data_hop))
    );
    let InterfaceTxJob::Data(data_job) = interface1
        .try_receive_job()
        .expect("online actor one receives DATA")
    else {
        panic!("DATA route changed family")
    };
    let (_data_ticket, data_job) = data_job.into_parts();
    let (_data_pending, data_request) = data_job.begin_permit(requirements(1));

    supervisor
        .router
        .set_online(descriptors[0].lease(), true)
        .expect("actor zero can come online");
    supervisor
        .try_offer_actions(standalone_announce(31, &mut rng), admission())
        .unwrap_or_else(|failure| panic!("ordinary offer: {:?}", failure.reason()));
    let mut ordinary_job = None;
    for _ in 0..32 {
        if let Some(InterfaceTxJob::Ordinary(job)) = interface0.try_receive_job() {
            ordinary_job = Some(job);
            break;
        }
        if matches!(
            supervisor.step(MonotonicMillis::new(12)).transition(),
            NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::NonPacketActionsReady)
        ) {
            let _ = drain_and_discard_application_events(&mut supervisor);
        }
    }
    let ordinary_job = ordinary_job.unwrap_or_else(|| {
        panic!(
            "ordinary actor job did not route within the bounded pass budget; fault={:?}",
            supervisor.ordinary_fault()
        )
    });
    let (_ordinary_ticket, ordinary_job) = ordinary_job.into_parts();
    let (_ordinary_pending, ordinary_request) = ordinary_job.begin_permit(requirements(2));
    ordinary0
        .requests()
        .try_send(ordinary_request)
        .unwrap_or_else(|_| panic!("actor zero ordinary request fits"));
    data1
        .requests()
        .try_send(data_request)
        .unwrap_or_else(|_| panic!("actor one DATA request fits"));

    let mut data_advances = 0;
    let mut ordinary_advances = 0;
    for _ in 0..32 {
        match supervisor.step(MonotonicMillis::new(13)).transition() {
            NodeInterfaceSupervisorTransition::DataPermit {
                actor: 1,
                step: DataPermitServerStep::Advanced,
            } => data_advances += 1,
            NodeInterfaceSupervisorTransition::OrdinaryPermit {
                actor: 0,
                step: OrdinaryPermitServerStep::Advanced,
            } => ordinary_advances += 1,
            NodeInterfaceSupervisorTransition::Ordinary(
                OrdinaryRouterStep::NonPacketActionsReady,
            ) => {
                let _ = drain_and_discard_application_events(&mut supervisor);
            }
            _ => {}
        }
        if data_advances == 3 && ordinary_advances == 3 {
            break;
        }
    }
    assert_eq!(data_advances, 3);
    assert_eq!(ordinary_advances, 3);
    assert!(data1.replies().try_receive().is_some());
    assert!(ordinary0.replies().try_receive().is_some());
    assert_eq!(supervisor.policy().calls, 2);
}

#[test]
fn graceful_actor_offline_preserves_inflight_completion_and_healthy_transport_progress() {
    let (mut supervisor, actor_ports, destination) = build::<2, 1>(35);
    let descriptors = register(&mut supervisor, &actor_ports, [false, false]);
    let [actor0, actor1] = actor_ports;
    let (interface0, _data0, _ordinary0) = actor0.into_parts();
    let (interface1, _data1, _ordinary1) = actor1.into_parts();
    let (mut tx0, _ingress0, mut lifecycle0) = interface0.into_parts();
    let (mut tx1, mut ingress1, mut lifecycle1) = interface1.into_parts();
    let ingress1_authority = ingress1
        .bind_ingress(descriptors[1])
        .expect("actor one descriptor must bind to its ingress queue");

    let ready0 = lifecycle0
        .try_request_state(descriptors[0].lease(), InterfaceLifecycleState::Ready)
        .expect("actor zero Ready request must fit");
    let ready1 = lifecycle1
        .try_request_state(descriptors[1].lease(), InterfaceLifecycleState::Ready)
        .expect("actor one Ready request must fit");
    let mut observed_ready = [false; 2];
    for _ in 0..16 {
        if let NodeInterfaceSupervisorTransition::Lifecycle(transition) =
            supervisor.step(MonotonicMillis::new(9)).transition()
        {
            let acknowledgement = transition.acknowledgement();
            let interface = acknowledgement
                .result()
                .expect("both current Ready requests must apply")
                .lease()
                .interface();
            observed_ready[usize::from(interface.get() - 2)] = true;
        }
        if observed_ready == [true, true] {
            break;
        }
    }
    assert_eq!(observed_ready, [true, true]);
    let ready0_descriptor = lifecycle0
        .try_finish_request()
        .expect("actor zero Ready acknowledgement must correlate")
        .expect("actor zero Ready acknowledgement must be retained");
    assert_eq!(ready0_descriptor.lease(), descriptors[0].lease());
    assert!(ready0_descriptor.is_online());
    let ready1_descriptor = lifecycle1
        .try_finish_request()
        .expect("actor one Ready acknowledgement must correlate")
        .expect("actor one Ready acknowledgement must be retained");
    assert_eq!(ready1_descriptor.lease(), descriptors[1].lease());
    assert!(ready1_descriptor.is_online());
    assert_eq!(ready0.lease(), descriptors[0].lease());
    assert_eq!(ready1.lease(), descriptors[1].lease());

    let mut rng = CounterRng::default();
    let first = prepare_data(
        &mut supervisor,
        destination,
        b"lifecycle in-flight fanout",
        &mut rng,
    );
    assert_eq!(first.interface(), descriptors[0].lease().interface());
    let mut first_routed = false;
    for _ in 0..16 {
        if supervisor.step(MonotonicMillis::new(10)).transition()
            == NodeInterfaceSupervisorTransition::Data(DataRouterStep::Routed(first))
        {
            first_routed = true;
            break;
        }
    }
    assert!(first_routed);
    let InterfaceTxJob::Data(first_job) = tx0.try_receive_job().expect("actor zero owns first hop")
    else {
        panic!("first fanout hop changed family")
    };
    let (first_ticket, first_job) = first_job.into_parts();

    let offline = lifecycle0
        .try_request_state(descriptors[0].lease(), InterfaceLifecycleState::Offline)
        .expect("actor zero Offline request must fit while its job is in flight");
    let mut offline_applied = false;
    for _ in 0..16 {
        if let NodeInterfaceSupervisorTransition::Lifecycle(transition) =
            supervisor.step(MonotonicMillis::new(11)).transition()
            && transition.acknowledgement().request() == offline
        {
            assert!(
                !transition
                    .acknowledgement()
                    .result()
                    .expect("current Offline request must apply")
                    .is_online()
            );
            offline_applied = true;
            break;
        }
    }
    assert!(offline_applied);
    assert!(
        !lifecycle0
            .try_finish_request()
            .expect("actor zero Offline acknowledgement must correlate")
            .expect("actor zero Offline acknowledgement must be retained")
            .is_online()
    );

    let first_completion = first_ticket
        .complete(
            first_job
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x780)),
        )
        .expect("offline transition must not invalidate the accepted ticket");
    tx0.try_send_completion(first_completion)
        .expect("offline actor may return its already-owned completion");

    let mut second = None;
    for _ in 0..32 {
        match supervisor.step(MonotonicMillis::new(12)).transition() {
            NodeInterfaceSupervisorTransition::Data(DataRouterStep::Completion(
                DataRouterCompletionProgress::Next(hop),
            )) => second = Some(hop),
            NodeInterfaceSupervisorTransition::Data(DataRouterStep::Routed(hop))
                if second == Some(hop) =>
            {
                break;
            }
            _ => {}
        }
    }
    let second = second.expect("legitimate offline completion must advance fanout");
    assert_eq!(second.interface(), descriptors[1].lease().interface());
    let InterfaceTxJob::Data(second_job) = tx1
        .try_receive_job()
        .expect("healthy actor owns second hop")
    else {
        panic!("second fanout hop changed family")
    };
    let (second_ticket, second_job) = second_job.into_parts();
    let second_completion = second_ticket
        .complete(
            second_job
                .return_unpermitted()
                .complete(TxCompletionCode::new(0x781)),
        )
        .expect("healthy actor ticket must remain exact");
    tx1.try_send_completion(second_completion)
        .expect("healthy actor completion must fit");
    let mut available = false;
    for _ in 0..24 {
        if matches!(
            supervisor.step(MonotonicMillis::new(13)).transition(),
            NodeInterfaceSupervisorTransition::Data(DataRouterStep::Completion(
                DataRouterCompletionProgress::Available(_)
            ))
        ) {
            available = true;
            break;
        }
    }
    assert!(available);

    let packet = native_announce_packet(96, &mut rng);
    let mut buffer = ingress1
        .try_receive_buffer()
        .expect("healthy actor retains ingress capacity");
    buffer.capacity_mut()[..packet.len()].copy_from_slice(&packet);
    let sealed = buffer
        .seal(packet.len())
        .expect("healthy ingress packet seals");
    ingress1
        .try_send(ingress1_authority, sealed)
        .expect("healthy actor ingress queue remains live");
    assert!(matches!(
        supervisor.step_ingress(MonotonicSeconds::new(14), admission(), &mut rng),
        NodeInterfaceIngressStep::Processed(_)
    ));

    let fresh = prepare_data(
        &mut supervisor,
        destination,
        b"fresh route excludes offline actor",
        &mut rng,
    );
    assert_eq!(fresh.interface(), descriptors[1].lease().interface());
    assert_eq!(tx0.pending_jobs(), 0);
}

#[test]
fn offline_lifecycle_preempts_a_ready_data_lane_before_actor_ownership_transfer() {
    let (mut supervisor, actor_ports, destination) = build::<2, 1>(37);
    let descriptors = register(&mut supervisor, &actor_ports, [true, true]);
    let [actor0, actor1] = actor_ports;
    let (interface0, _data0, _ordinary0) = actor0.into_parts();
    let (interface1, _data1, _ordinary1) = actor1.into_parts();
    let (tx0, _ingress0, mut lifecycle0) = interface0.into_parts();
    let (mut tx1, _ingress1, _lifecycle1) = interface1.into_parts();
    let mut rng = CounterRng::default();
    let first = prepare_data(
        &mut supervisor,
        destination,
        b"offline must preempt ready DATA",
        &mut rng,
    );
    assert_eq!(first.interface(), descriptors[0].lease().interface());

    // Position the fair orchestration cursor at DATA. Before lifecycle
    // became a pre-routing gate, this allowed one fresh owner to enter a
    // stopped actor before its queued Offline transition was observed.
    supervisor.lane_cursor = 1;
    let offline = lifecycle0
        .try_request_state(descriptors[0].lease(), InterfaceLifecycleState::Offline)
        .expect("actor zero Offline request must fit");
    let first_pass = supervisor.step(MonotonicMillis::new(20));
    assert!(matches!(
        first_pass.transition(),
        NodeInterfaceSupervisorTransition::Lifecycle(transition)
            if transition.acknowledgement().request() == offline
    ));
    assert_eq!(tx0.pending_jobs(), 0);
    assert!(
        !lifecycle0
            .try_finish_request()
            .expect("Offline acknowledgement must correlate")
            .expect("Offline acknowledgement must be retained")
            .is_online()
    );

    assert!(matches!(
        supervisor.step(MonotonicMillis::new(21)).transition(),
        NodeInterfaceSupervisorTransition::Data(DataRouterStep::RouteRejected {
            hop,
            reason: reticulum_interface_router::RouteError::Offline(lease),
        }) if hop == first && lease == descriptors[0].lease()
    ));
    assert_eq!(tx0.pending_jobs(), 0);

    let mut second = None;
    for _ in 0..16 {
        match supervisor.step(MonotonicMillis::new(22)).transition() {
            NodeInterfaceSupervisorTransition::Data(DataRouterStep::Completion(
                DataRouterCompletionProgress::Next(hop),
            )) => second = Some(hop),
            NodeInterfaceSupervisorTransition::Data(DataRouterStep::Routed(hop))
                if second == Some(hop) =>
            {
                break;
            }
            _ => {}
        }
    }
    let second = second.expect("local Offline rejection must continue through actor one");
    assert_eq!(second.interface(), descriptors[1].lease().interface());
    assert_eq!(tx0.pending_jobs(), 0);
    assert!(matches!(
        tx1.try_receive_job(),
        Some(InterfaceTxJob::Data(_))
    ));
}

#[test]
fn lifecycle_offline_is_acknowledged_after_aggregate_owner_mismatch() {
    let (mut supervisor, [actor], _) = build::<1, 1>(36);
    let [descriptor] = register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (interface, _data, _ordinary) = actor.into_parts();
    let (_tx, _ingress, mut lifecycle) = interface.into_parts();
    let request = lifecycle
        .try_request_state(descriptor.lease(), InterfaceLifecycleState::Offline)
        .expect("Offline request must fit before aggregate fault observation");

    let (foreign_node, _) = sender(91);
    supervisor.node = foreign_node;
    assert_eq!(
        supervisor.step(MonotonicMillis::new(20)).transition(),
        NodeInterfaceSupervisorTransition::Fault(NodeInterfaceSupervisorFault::DataOwnerMismatch)
    );
    let second = supervisor.step(MonotonicMillis::new(21));
    assert!(second.maintenance().is_err());
    let NodeInterfaceSupervisorTransition::Lifecycle(transition) = second.transition() else {
        panic!("faulted aggregate must still acknowledge actor lifecycle")
    };
    assert_eq!(transition.acknowledgement().request(), request);
    assert!(
        !transition
            .acknowledgement()
            .result()
            .expect("current Offline request remains authoritative")
            .is_online()
    );
    assert_eq!(
        lifecycle
            .try_finish_request()
            .expect("faulted aggregate acknowledgement must correlate")
            .expect("faulted aggregate retains the exact acknowledgement"),
        transition
            .acknowledgement()
            .result()
            .expect("current Offline transition already applied")
    );
}

#[test]
fn construction_mismatch_returns_every_unsplit_input() {
    let (mut owner_node, _) = sender(7);
    let data = data_coordinator(&mut owner_node);
    let ordinary = ordinary_coordinator(&mut owner_node);
    let owner_scope = owner_node.tx_owner_scope();
    let foreign_node = node(40, "foreign-aggregate");
    let foreign_scope = foreign_node.tx_owner_scope();
    let fabric = Box::leak(Box::new(InterfaceFabric::<NoopRawMutex, 1, 1>::new()));
    let fabric_pointer = core::ptr::from_mut(&mut *fabric);

    let failure = match TestSupervisor::<1, 1>::try_new(
        foreign_node,
        fabric,
        data,
        ordinary,
        data_pairs(),
        ordinary_pairs(),
        CountingAllow { calls: 7 },
    ) {
        Ok(_) => panic!("foreign aggregate unexpectedly built"),
        Err(failure) => failure,
    };
    assert_eq!(
        failure.reason(),
        NodeInterfaceSupervisorBuildError::DataOwnerMismatch {
            node: foreign_scope,
            coordinator: owner_scope,
        }
    );
    let (node, fabric, data, ordinary, data_pairs, ordinary_pairs, policy) = failure.into_parts();
    assert_eq!(node.tx_owner_scope(), foreign_scope);
    assert_eq!(data.owner_scope(), owner_scope);
    assert_eq!(ordinary.owner_scope(), owner_scope);
    assert_eq!(core::ptr::from_mut(fabric), fabric_pointer);
    assert_eq!(data_pairs.len(), 1);
    assert_eq!(ordinary_pairs.len(), 1);
    assert_eq!(policy.calls, 7);
}

#[test]
fn staged_in_place_construction_configures_the_final_node_before_finish() {
    let storage = Box::leak(Box::new(MaybeUninit::<TestSupervisor<1, 1>>::uninit()));
    let mut stage = TestSupervisor::<1, 1>::begin_node_in(
        storage,
        identity(41),
        "reticulum",
        &["staged-aggregate"],
        NodeInstanceId::new([0xc1; 16]),
        NodeConfig::endpoint(),
    )
    .expect("the in-place node must construct");
    let destination = stage.node_mut().destination_hash();
    let data = data_coordinator(stage.node_mut());
    let ordinary = ordinary_coordinator(stage.node_mut());
    let fabric = Box::leak(Box::new(InterfaceFabric::new()));

    let (supervisor, actors) = stage
        .finish(
            fabric,
            data,
            ordinary,
            data_pairs(),
            ordinary_pairs(),
            CountingAllow::default(),
        )
        .expect("the validated stage must finish");

    assert_eq!(supervisor.destination_hash(), destination);
    assert_eq!(actors.len(), 1);
    assert_eq!(supervisor.fault(), None);
}

#[test]
fn rejected_demultiplexed_completion_is_retained_as_exact_residue() {
    let (mut supervisor, [actor], _) = build::<1, 1>(9);
    register(
        &mut supervisor,
        core::slice::from_ref(&actor).try_into().unwrap(),
        [true],
    );
    let (mut interface, _data_permit, _ordinary_permit) = actor.into_parts();

    let (mut foreign_node, destination) = sender(50);
    let mut foreign_data = data_coordinator(&mut foreign_node);
    let mut rng = CounterRng::default();
    let hop = match foreign_data.try_prepare_data(
        &mut foreign_node,
        &supervisor.router,
        prepare_request(destination, b"foreign exact owner"),
        MonotonicMillis::new(10),
        &mut rng,
    ) {
        DataRouterPrepareResult::Prepared(hop) => hop,
        other => panic!("foreign DATA did not prepare: {other:?}"),
    };
    assert_eq!(
        foreign_data.step(
            &mut foreign_node,
            &mut supervisor.router,
            MonotonicMillis::new(10),
        ),
        DataRouterStep::Routed(hop)
    );
    let InterfaceTxJob::Data(job) = interface
        .try_receive_job()
        .expect("foreign job must enter aggregate fabric")
    else {
        panic!("foreign DATA changed family")
    };
    let (ticket, job) = job.into_parts();
    let prepared = job.prepared();
    let selected = job.interface();
    let completion = ticket
        .complete(
            job.return_unpermitted()
                .complete(TxCompletionCode::new(0x730)),
        )
        .expect("ticket binds the foreign exact completion");
    interface
        .try_send_completion(completion)
        .expect("completion queue has capacity");

    let pass = supervisor.step(MonotonicMillis::new(11));
    assert_eq!(
        pass.transition(),
        NodeInterfaceSupervisorTransition::Fault(
            NodeInterfaceSupervisorFault::DataCompletionRejected(
                DataCompletionAcceptError::NoActiveOwner(prepared.slot_id())
            )
        )
    );
    assert_eq!(
        supervisor.completion_fault_residue(),
        Some(NodeInterfaceCompletionResidue::Data {
            prepared,
            interface: selected,
        })
    );
}
