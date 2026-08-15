extern crate std;

use core::{
    future::Future,
    task::{Context, Poll},
};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_node_core::{
    DestinationHash, InterfaceSet, MonotonicMillis, MonotonicSeconds, NodeConfig, NodeCore,
    NodeIdentity, NodeInstanceId, OrdinaryActionAdmissionRequest, OrdinaryBufferPool,
    OrdinaryCompletionDisposition, OrdinaryPacketBuffer, PACKET_CAPACITY, PrepareDataRequest,
    TxCompletionCode, TxCompletionDisposition, TxLeaseDeadline, TxPacketBuffer, TxTarget,
};
use reticulum_rns_rete::{
    EmbeddedNode, EmbeddedNodeConfig, InboundProofPolicy, InterfaceId as RnsInterfaceId,
    NodeActions,
};
use std::{
    boxed::Box,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Wake, Waker},
};

use super::*;

type TestNode<const BUFFERS: usize> = NodeCore<8, 4, 8, 8, BUFFERS>;

#[derive(Default)]
struct WakeCounter(AtomicUsize);

impl Wake for WakeCounter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

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

fn interfaces(ids: &[u8]) -> InterfaceSet {
    ids.iter().fold(InterfaceSet::empty(), |set, id| {
        set.with(PacketInterfaceId::new(*id))
            .expect("test interface must fit compact profile")
    })
}

fn register_peer<const BUFFERS: usize>(sender: &mut TestNode<BUFFERS>, tag: u8, aspect: &str) {
    sender
        .register_peer(
            &identity(tag),
            "reticulum",
            &[aspect],
            MonotonicSeconds::new(0),
        )
        .expect("test peer must register");
}

fn prepare(
    sender: &mut TestNode<3>,
    buffer: &'static mut TxPacketBuffer,
    destination: DestinationHash,
    enabled_interfaces: InterfaceSet,
    rng: &mut CounterRng,
) -> RoutedTxJob<'static> {
    match sender.prepare_data_into_slot(
        buffer,
        PrepareDataRequest {
            destination,
            plaintext: b"interface router",
            rns_now: MonotonicSeconds::new(1),
            owner_now: MonotonicMillis::new(10),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(10_000)),
            enabled_interfaces,
        },
        rng,
    ) {
        Ok(job) => job,
        Err(failure) => panic!("test preparation failed: {:?}", failure.reason()),
    }
}

fn fabric<const DEPTH: usize>() -> (
    OutboundRouter<NoopRawMutex, 2, DEPTH>,
    [InterfaceActorHandoff<NoopRawMutex, DEPTH>; 2],
) {
    Box::leak(Box::new(InterfaceFabric::new())).split()
}

fn configure<const DEPTH: usize>(
    router: &mut OutboundRouter<NoopRawMutex, 2, DEPTH>,
    actors: &[InterfaceActorHandoff<NoopRawMutex, DEPTH>; 2],
) -> (InterfaceDescriptor, InterfaceDescriptor) {
    let mtu = LogicalMtu::try_new(500).expect("test MTU must be nonzero");
    let first_properties = InterfaceProperties::new(
        mtu,
        InterfaceConfigId::new(20),
        AdvertisedBitrate::try_new(1_000_000).ok(),
        InterfaceCost::new(10),
    );
    let second_properties = InterfaceProperties::new(
        mtu,
        InterfaceConfigId::new(90),
        AdvertisedBitrate::try_new(5_400).ok(),
        InterfaceCost::new(80),
    );
    let first = router
        .register(
            actors[0].queue_id(),
            PacketInterfaceId::new(2),
            first_properties,
            true,
        )
        .expect("first interface must register");
    let second = router
        .register(
            actors[1].queue_id(),
            PacketInterfaceId::new(9),
            second_properties,
            true,
        )
        .expect("second interface must register");
    (first, second)
}

fn setup_sender() -> (TestNode<3>, DestinationHash, CounterRng) {
    let mut sender = node::<3>(1, "interface-router");
    let receiver = node::<3>(2, "peer");
    register_peer(&mut sender, 2, "peer");
    let destination = receiver.destination_hash();
    (sender, destination, CounterRng(3))
}

#[test]
fn announce_and_recursive_path_egress_use_independent_rns_policies() {
    let mtu = LogicalMtu::try_new(500).expect("test MTU must be nonzero");
    let properties = |config| {
        InterfaceProperties::new(
            mtu,
            InterfaceConfigId::new(config),
            None,
            InterfaceCost::new(0),
        )
    };
    let mut registry = InterfaceRegistry::<3>::new();
    registry
        .register(
            InterfaceQueueId::new(0),
            PacketInterfaceId::new(1),
            properties(1)
                .with_announce_mode(AnnouncePropagationMode::Internal)
                .with_recursive_path_search_mode(RecursivePathSearchMode::Unrestricted),
            true,
        )
        .expect("internal shared-medium interface registers");
    registry
        .register(
            InterfaceQueueId::new(1),
            PacketInterfaceId::new(2),
            properties(2)
                .with_topology(InterfaceTopology::PointToPoint)
                .with_announce_mode(AnnouncePropagationMode::Boundary)
                .with_recursive_path_search_mode(RecursivePathSearchMode::Boundary),
            true,
        )
        .expect("boundary point-to-point interface registers");
    registry
        .register(
            InterfaceQueueId::new(2),
            PacketInterfaceId::new(3),
            properties(3).with_announce_mode(AnnouncePropagationMode::Full),
            true,
        )
        .expect("full interface registers");

    assert_eq!(
        registry
            .announce_egress_interfaces(PacketInterfaceId::new(2))
            .expect("registered boundary source resolves"),
        interfaces(&[3]),
        "a boundary announce cannot enter Internal and cannot reflect to its point-to-point source"
    );
    assert_eq!(
        registry
            .announce_egress_interfaces(PacketInterfaceId::new(1))
            .expect("registered internal source resolves"),
        interfaces(&[1, 2, 3]),
        "an Internal announce can repeat on its shared medium and cross the boundary"
    );
    assert_eq!(
        registry.announce_egress_interfaces(PacketInterfaceId::new(9)),
        Err(AnnounceEgressSetError::SourceUnavailable(
            PacketInterfaceId::new(9)
        ))
    );

    assert_eq!(
        registry
            .recursive_path_search_egress_interfaces(PacketInterfaceId::new(2))
            .expect("registered boundary source resolves"),
        Some(InterfaceSet::empty()),
        "Boundary searches do not use the announce matrix and therefore do not enter Full or Internal"
    );
    assert_eq!(
        registry
            .recursive_path_search_egress_interfaces(PacketInterfaceId::new(1))
            .expect("registered internal source resolves"),
        Some(interfaces(&[2, 3])),
        "an Internal search reaches every online non-source interface and never reflects on its shared medium"
    );
    assert_eq!(
        registry
            .recursive_path_search_egress_interfaces(PacketInterfaceId::new(3))
            .expect("registered Full source resolves"),
        None,
        "Full does not initiate recursive searches without recursive_prs"
    );
}

fn rns_identity(tag: u8) -> reticulum_rns_rete::Identity {
    reticulum_rns_rete::identity_from_private_key(&[tag; 64])
        .expect("RNS test identity must be valid")
}

fn rns_node(tag: u8) -> EmbeddedNode<4, 4, 8, 2> {
    EmbeddedNode::new(
        rns_identity(tag),
        "reticulum",
        &["interface-router-ordinary"],
        EmbeddedNodeConfig::endpoint(),
    )
    .expect("RNS test node must construct")
}

fn source_only_actions(source_interface: u8, rng: &mut CounterRng) -> NodeActions {
    let mut sender = rns_node(41);
    let mut receiver = rns_node(42);
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
            b"ordinary proof",
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

fn seal_bytes(mut buffer: AvailableIngressBuffer, bytes: &[u8]) -> SealedIngressPacket {
    buffer.capacity_mut()[..bytes.len()].copy_from_slice(bytes);
    buffer
        .seal(bytes.len())
        .expect("test ingress length must fit")
}

#[test]
fn cancelled_woken_receive_leaves_exact_job_queued() {
    let (mut router, mut actors) = fabric::<1>();
    configure(&mut router, &actors);
    let (mut sender, destination, mut rng) = setup_sender();
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let job = prepare(&mut sender, buffer, destination, interfaces(&[9]), &mut rng);
    let expected = job.prepared();

    let wake_counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake_counter));
    let mut context = Context::from_waker(&waker);
    let mut receive = Box::pin(actors[1].receive_job());
    assert!(matches!(receive.as_mut().poll(&mut context), Poll::Pending));

    router
        .try_route_data(job)
        .expect("queued owner must wake the pending receiver");
    assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
    drop(receive);

    assert_eq!(actors[1].pending_jobs(), 1);
    let InterfaceTxJob::Data(received) = actors[1]
        .try_receive_job()
        .expect("cancellation must leave the exact owner queued")
    else {
        panic!("cancellation changed the queued owner family")
    };
    let (ticket, received) = received.into_parts();
    assert_eq!(received.prepared(), expected);
    let completion = ticket
        .complete(
            received
                .return_unpermitted()
                .complete(TxCompletionCode::new(90)),
        )
        .expect("cancelled receive must preserve the ticket binding");
    actors[1]
        .try_send_completion(completion)
        .expect("completion must return through the exact actor");
    let Some(OutboundCompletion::Data(completion)) = router
        .try_receive_completion()
        .expect("current completion lease must remain valid")
    else {
        panic!("cancelled receive did not return the DATA completion")
    };
    assert!(matches!(
        sender.complete_tx(completion, MonotonicMillis::new(20)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

#[test]
fn ready_poll_returns_exact_non_copy_owner() {
    let (mut router, mut actors) = fabric::<1>();
    configure(&mut router, &actors);
    let (mut sender, destination, mut rng) = setup_sender();
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let job = prepare(&mut sender, buffer, destination, interfaces(&[9]), &mut rng);
    let expected = job.prepared();
    router
        .try_route_data(job)
        .expect("exact owner must fit the actor queue");

    let waker = Waker::from(Arc::new(WakeCounter::default()));
    let mut context = Context::from_waker(&waker);
    let Poll::Ready(InterfaceTxJob::Data(received)) = actors[1].poll_receive_job(&mut context)
    else {
        panic!("ready polling did not return the queued DATA owner")
    };
    assert_eq!(actors[1].pending_jobs(), 0);
    let (ticket, received) = received.into_parts();
    assert_eq!(received.prepared(), expected);
    let completion = ticket
        .complete(
            received
                .return_unpermitted()
                .complete(TxCompletionCode::new(91)),
        )
        .expect("polled owner must retain its exact ticket binding");
    actors[1]
        .try_send_completion(completion)
        .expect("completion must return through the exact actor");
    let Some(OutboundCompletion::Data(completion)) = router
        .try_receive_completion()
        .expect("current completion lease must remain valid")
    else {
        panic!("ready poll did not return the DATA completion")
    };
    assert!(matches!(
        sender.complete_tx(completion, MonotonicMillis::new(20)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

#[test]
fn route_capacity_and_completion_polls_wake_without_reserving_owners() {
    let (mut router, mut actors) = fabric::<1>();
    configure(&mut router, &actors);
    let (mut sender, destination, mut rng) = setup_sender();
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let job = prepare(&mut sender, buffer, destination, interfaces(&[9]), &mut rng);
    let expected = job.prepared();
    let packet_len = job.packet_len();
    router
        .try_route_data(job)
        .expect("first owner must fill the second actor queue");

    let wake_counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake_counter));
    let mut context = Context::from_waker(&waker);
    assert!(matches!(
        router.poll_route_capacity(PacketInterfaceId::new(9), packet_len, &mut context),
        Poll::Pending
    ));
    let InterfaceTxJob::Data(job) = actors[1]
        .try_receive_job()
        .expect("actor dequeue must return the exact queued owner")
    else {
        panic!("DATA owner changed family")
    };
    assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
    assert!(matches!(
        router.poll_route_capacity(PacketInterfaceId::new(9), packet_len, &mut context),
        Poll::Ready(Ok(descriptor))
            if descriptor.lease().interface() == PacketInterfaceId::new(9)
    ));

    let before_completion = wake_counter.0.load(Ordering::SeqCst);
    assert!(matches!(
        router.poll_receive_completion(&mut context),
        Poll::Pending
    ));
    let (ticket, job) = job.into_parts();
    assert_eq!(job.prepared(), expected);
    let completion = ticket
        .complete(job.return_unpermitted().complete(TxCompletionCode::new(92)))
        .expect("ticket must bind the exact DATA owner");
    actors[1]
        .try_send_completion(completion)
        .expect("completion must fit");
    assert!(wake_counter.0.load(Ordering::SeqCst) > before_completion);

    // Abandon the pending poll and use the immediate API. The completion
    // must still be queued because a pending poll only registered a waker.
    let Some(OutboundCompletion::Data(completion)) = router
        .try_receive_completion()
        .expect("current completion must remain routable")
    else {
        panic!("cancelled completion poll removed or changed the owner")
    };
    assert!(matches!(
        sender.complete_tx(completion, MonotonicMillis::new(20)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

#[test]
fn completion_capacity_wait_is_advisory_and_preserves_the_rejected_owner() {
    let (mut router, mut actors) = fabric::<1>();
    configure(&mut router, &actors);
    let (mut sender, destination, mut rng) = setup_sender();
    let first_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    let second_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender
        .register_packet_buffer(first_buffer)
        .expect("first buffer must register");
    sender
        .register_packet_buffer(second_buffer)
        .expect("second buffer must register");

    let first = prepare(
        &mut sender,
        first_buffer,
        destination,
        interfaces(&[9]),
        &mut rng,
    );
    router
        .try_route_data(first)
        .expect("first owner must enter the actor queue");
    let InterfaceTxJob::Data(first) = actors[1]
        .try_receive_job()
        .expect("actor must receive the first owner")
    else {
        panic!("first owner changed family")
    };

    let second = prepare(
        &mut sender,
        second_buffer,
        destination,
        interfaces(&[9]),
        &mut rng,
    );
    router
        .try_route_data(second)
        .expect("second owner must enter the vacated actor queue");
    let InterfaceTxJob::Data(second) = actors[1]
        .try_receive_job()
        .expect("actor must receive the second owner")
    else {
        panic!("second owner changed family")
    };

    let (first_ticket, first) = first.into_parts();
    let first_completion = first_ticket
        .complete(
            first
                .return_unpermitted()
                .complete(TxCompletionCode::new(93)),
        )
        .expect("first ticket must bind its exact owner");
    actors[1]
        .try_send_completion(first_completion)
        .expect("first completion must fill the actor queue");

    let (second_ticket, second) = second.into_parts();
    let expected_second = second.prepared();
    let second_completion = second_ticket
        .complete(
            second
                .return_unpermitted()
                .complete(TxCompletionCode::new(94)),
        )
        .expect("second ticket must bind its exact owner");
    let rejected = actors[1]
        .try_send_completion(second_completion)
        .expect_err("full completion queue must return the second owner");
    assert_eq!(
        rejected.reason(),
        ActorCompletionSendError::QueueFull(actors[1].queue_id())
    );
    let retained_second = rejected.into_completion();

    let wake_counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake_counter));
    let mut context = Context::from_waker(&waker);
    let mut capacity_wait = Box::pin(actors[1].wait_completion_capacity());
    assert!(matches!(
        capacity_wait.as_mut().poll(&mut context),
        Poll::Pending
    ));

    let Some(OutboundCompletion::Data(first_completion)) = router
        .try_receive_completion()
        .expect("first completion lease must remain current")
    else {
        panic!("first completion changed family")
    };
    assert!(wake_counter.0.load(Ordering::SeqCst) > 0);

    // Cancel after the registered wait was woken but before polling it
    // again. The wait owns no completion and cannot reserve the capacity.
    drop(capacity_wait);
    assert!(matches!(
        actors[1].poll_completion_capacity(&mut context),
        Poll::Ready(())
    ));
    assert!(matches!(
        sender.complete_tx(first_completion, MonotonicMillis::new(20)),
        Ok(TxCompletionDisposition::Available(_))
    ));

    actors[1]
        .try_send_completion(retained_second)
        .expect("cancelled advisory wait must leave capacity unreserved");
    let Some(OutboundCompletion::Data(second_completion)) = router
        .try_receive_completion()
        .expect("second completion lease must remain current")
    else {
        panic!("second completion changed family")
    };
    assert_eq!(second_completion.prepared(), expected_second);
    assert!(matches!(
        sender.complete_tx(second_completion, MonotonicMillis::new(20)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

#[test]
fn only_routes_to_exact_actor_and_foreign_return_is_rejected_unchanged() {
    let (mut router, mut actors) = fabric::<1>();
    configure(&mut router, &actors);
    let mut node = node::<3>(51, "interface-router-ordinary");
    let mut owner = node
        .take_ordinary_action_owner::<1>()
        .expect("ordinary owner must be issued once");
    let mut pool = OrdinaryBufferPool::<1>::new();
    if let Err(failure) =
        owner.register_and_park(&mut pool, Box::leak(Box::new(OrdinaryPacketBuffer::new())))
    {
        panic!(
            "ordinary buffer did not register and park: {:?}",
            failure.reason()
        );
    }
    let mut rng = CounterRng(70);
    let actions = source_only_actions(9, &mut rng);
    let mut batch = owner
        .admit_from_pool(
            actions,
            &mut pool,
            OrdinaryActionAdmissionRequest {
                owner_now: MonotonicMillis::new(10),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(10_000)),
                enabled_interfaces: interfaces(&[2, 9]),
            },
        )
        .expect("source-relative ordinary action must admit");
    let job = batch
        .take_next_packet()
        .expect("proof action must own one packet");
    assert_eq!(job.target(), TxTarget::Only(PacketInterfaceId::new(9)));

    let receipt = router.try_route_ordinary(job).expect("Only route must fit");
    assert_eq!(
        receipt.context().lease().interface(),
        PacketInterfaceId::new(9)
    );
    assert!(actors[0].try_receive_job().is_none());
    let InterfaceTxJob::Ordinary(job) = actors[1]
        .try_receive_job()
        .expect("second actor must receive exact owner")
    else {
        panic!("ordinary route changed owner family")
    };
    let (ticket, job) = job.into_parts();
    let completion = ticket
        .complete(job.return_unpermitted().complete(TxCompletionCode::new(1)))
        .expect("exact completion must match ticket");
    let crossed = actors[0]
        .try_send_completion(completion)
        .expect_err("foreign actor must reject completion");
    assert_eq!(
        crossed.reason(),
        ActorCompletionSendError::ForeignQueue {
            expected: actors[0].queue_id(),
            supplied: actors[1].queue_id(),
        }
    );
    actors[1]
        .try_send_completion(crossed.into_completion())
        .expect("correct actor must return exact completion");
    let Some(OutboundCompletion::Ordinary(completion)) = router
        .try_receive_completion()
        .expect("current lease must accept completion")
    else {
        panic!("router did not return ordinary completion")
    };
    assert!(matches!(
        owner.complete_tx(completion, MonotonicMillis::new(20)),
        Ok(OrdinaryCompletionDisposition::Returned(_))
    ));
}

#[test]
fn all_remains_serialized_and_node_core_selects_next_actor() {
    let (mut router, mut actors) = fabric::<1>();
    configure(&mut router, &actors);
    let (mut sender, destination, mut rng) = setup_sender();
    let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender
        .register_packet_buffer(buffer)
        .expect("buffer must register");
    let first = prepare(
        &mut sender,
        buffer,
        destination,
        interfaces(&[2, 9]),
        &mut rng,
    );
    assert_eq!(first.target(), TxTarget::All);
    assert_eq!(first.interface(), PacketInterfaceId::new(2));
    router.try_route_data(first).expect("first hop must fit");
    assert!(actors[1].try_receive_job().is_none());

    let InterfaceTxJob::Data(first) = actors[0]
        .try_receive_job()
        .expect("first actor must receive first hop")
    else {
        panic!("first hop changed owner family")
    };
    let (ticket, first) = first.into_parts();
    let completion = ticket
        .complete(
            first
                .return_unpermitted()
                .complete(TxCompletionCode::new(2)),
        )
        .expect("first completion must match");
    actors[0]
        .try_send_completion(completion)
        .expect("first completion must fit");
    let Some(OutboundCompletion::Data(completion)) = router
        .try_receive_completion()
        .expect("first completion lease must remain current")
    else {
        panic!("first completion missing")
    };
    let next = match sender.complete_tx(completion, MonotonicMillis::new(20)) {
        Ok(TxCompletionDisposition::Next(next)) => next,
        Ok(_) => panic!("All target did not produce serialized Next"),
        Err(failure) => panic!("first completion did not reconcile: {:?}", failure.reason()),
    };
    assert_eq!(next.interface(), PacketInterfaceId::new(9));
    router.try_route_data(next).expect("Next hop must fit");
    assert!(actors[0].try_receive_job().is_none());

    let InterfaceTxJob::Data(second) = actors[1]
        .try_receive_job()
        .expect("second actor must receive Next")
    else {
        panic!("Next changed owner family")
    };
    let (ticket, second) = second.into_parts();
    let completion = ticket
        .complete(
            second
                .return_unpermitted()
                .complete(TxCompletionCode::new(3)),
        )
        .expect("second completion must match");
    actors[1]
        .try_send_completion(completion)
        .expect("second completion must fit");
    let Some(OutboundCompletion::Data(completion)) = router
        .try_receive_completion()
        .expect("second completion lease must remain current")
    else {
        panic!("second completion missing")
    };
    assert!(matches!(
        sender.complete_tx(completion, MonotonicMillis::new(30)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

#[test]
fn offline_and_queue_pressure_return_exact_data_owners() {
    let (mut router, actors) = fabric::<1>();
    let (_, second) = configure(&mut router, &actors);
    let (mut sender, destination, mut rng) = setup_sender();
    let first_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    let second_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    let third_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender
        .register_packet_buffer(first_buffer)
        .expect("first buffer must register");
    sender
        .register_packet_buffer(second_buffer)
        .expect("second buffer must register");
    sender
        .register_packet_buffer(third_buffer)
        .expect("third buffer must register");

    let first = prepare(
        &mut sender,
        first_buffer,
        destination,
        interfaces(&[9]),
        &mut rng,
    );
    router
        .try_route_data(first)
        .expect("first queue slot must fit");
    let pressured = prepare(
        &mut sender,
        second_buffer,
        destination,
        interfaces(&[9]),
        &mut rng,
    );
    let pressured_slot = pressured.slot_id();
    let failure = router
        .try_route_data(pressured)
        .expect_err("depth-one queue must retain pressure");
    assert_eq!(failure.reason(), RouteError::QueueFull(second.lease()));
    assert_eq!(failure.into_job().slot_id(), pressured_slot);

    router
        .set_online(second.lease(), false)
        .expect("current lease must go offline");
    let offline = prepare(
        &mut sender,
        third_buffer,
        destination,
        interfaces(&[9]),
        &mut rng,
    );
    let offline_slot = offline.slot_id();
    let failure = router
        .try_route_data(offline)
        .expect_err("offline interface must reject new owner");
    assert_eq!(failure.reason(), RouteError::Offline(second.lease()));
    assert_eq!(failure.into_job().slot_id(), offline_slot);
}

#[test]
fn offline_inflight_completion_is_valid_but_reconfigured_generation_is_retained_stale() {
    let (mut router, mut actors) = fabric::<2>();
    let (_, second) = configure(&mut router, &actors);
    let (mut sender, destination, mut rng) = setup_sender();
    let first_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    let stale_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
    sender
        .register_packet_buffer(first_buffer)
        .expect("first buffer must register");
    sender
        .register_packet_buffer(stale_buffer)
        .expect("stale buffer must register");

    let first = prepare(
        &mut sender,
        first_buffer,
        destination,
        interfaces(&[9]),
        &mut rng,
    );
    router.try_route_data(first).expect("first owner must fit");
    router
        .set_online(second.lease(), false)
        .expect("current interface must go offline");
    let InterfaceTxJob::Data(first) = actors[1]
        .try_receive_job()
        .expect("accepted owner remains queued")
    else {
        panic!("accepted owner changed family")
    };
    let (ticket, first) = first.into_parts();
    let completion = ticket
        .complete(
            first
                .return_unpermitted()
                .complete(TxCompletionCode::new(4)),
        )
        .expect("exact completion must match");
    actors[1]
        .try_send_completion(completion)
        .expect("offline actor may finish accepted owner");
    let Ok(Some(OutboundCompletion::Data(completion))) = router.try_receive_completion() else {
        panic!("offline in-flight completion did not return to node owner")
    };
    assert!(matches!(
        sender.complete_tx(completion, MonotonicMillis::new(20)),
        Ok(TxCompletionDisposition::Available(_))
    ));

    router
        .set_online(second.lease(), true)
        .expect("same generation must return online");
    let stale = prepare(
        &mut sender,
        stale_buffer,
        destination,
        interfaces(&[9]),
        &mut rng,
    );
    router
        .try_route_data(stale)
        .expect("stale candidate must queue");
    let InterfaceTxJob::Data(stale) = actors[1]
        .try_receive_job()
        .expect("actor must receive stale candidate")
    else {
        panic!("stale candidate changed family")
    };
    let (ticket, stale) = stale.into_parts();
    let old_lease = ticket.context().lease();
    let completion = ticket
        .complete(
            stale
                .return_unpermitted()
                .complete(TxCompletionCode::new(5)),
        )
        .expect("exact completion must match old ticket");
    let current = router
        .reconfigure(
            old_lease,
            InterfaceProperties::new(
                LogicalMtu::try_new(480).expect("MTU must be nonzero"),
                InterfaceConfigId::new(91),
                AdvertisedBitrate::try_new(7_200).ok(),
                InterfaceCost::new(75),
            ),
            true,
        )
        .expect("reconfiguration must advance generation");
    assert_ne!(current.lease().generation(), old_lease.generation());
    actors[1]
        .try_send_completion(completion)
        .expect("old actor ticket still belongs to its queue");
    let failure = router
        .try_receive_completion()
        .expect_err("superseded generation must be retained stale");
    assert!(matches!(
        failure.reason(),
        CompletionRouteError::StaleLease(InterfaceLeaseError::Stale { supplied, current: observed })
            if supplied == old_lease && observed == current
    ));
    let recovery = failure.into_node_recovery();
    assert!(matches!(
        recovery.reason(),
        CompletionRouteError::StaleLease(InterfaceLeaseError::Stale { .. })
    ));
    let OutboundCompletion::Data(completion) = recovery.into_outbound() else {
        panic!("stale failure changed owner family")
    };
    assert_eq!(completion.interface(), PacketInterfaceId::new(9));
    assert!(matches!(
        sender.complete_tx(completion, MonotonicMillis::new(30)),
        Ok(TxCompletionDisposition::Available(_))
    ));
}

#[test]
fn registry_is_the_enabled_snapshot_and_actor_capability_is_queue_bound() {
    let (mut router, actors) = fabric::<1>();
    let (first, second) = configure(&mut router, &actors);
    assert_eq!(
        first.advertised_bitrate().map(AdvertisedBitrate::get),
        Some(1_000_000)
    );
    assert_eq!(first.cost(), InterfaceCost::new(10));
    let eligible = router
        .eligible_interfaces()
        .expect("configured IDs must fit compact profile");
    assert!(eligible.contains(PacketInterfaceId::new(2)));
    assert!(eligible.contains(PacketInterfaceId::new(9)));

    router
        .set_online(second.lease(), false)
        .expect("second lease must go offline");
    let eligible = router
        .eligible_interfaces()
        .expect("offline ID must still fit compact profile");
    assert!(eligible.contains(PacketInterfaceId::new(2)));
    assert!(!eligible.contains(PacketInterfaceId::new(9)));

    assert_eq!(
        actors[1].bind_ingress(first),
        Err(ActorIngressBindingError::ForeignQueue {
            expected: actors[1].queue_id(),
            supplied: actors[0].queue_id(),
        })
    );
    let authority = actors[0]
        .bind_ingress(first)
        .expect("matching actor must bind registry provenance");
    assert_eq!(authority.lease(), first.lease());
}

#[test]
fn native_ingress_roundtrip_preserves_provenance_bytes_and_exact_reuse() {
    let (mut router, mut actors) = fabric::<1>();
    let (first, _) = configure(&mut router, &actors);
    assert_eq!(actors[0].available_ingress_buffers(), 1);
    let authority = actors[0]
        .bind_ingress(first)
        .expect("matching actor must bind ingress authority");
    let mut buffer = actors[0]
        .try_receive_ingress_buffer()
        .expect("split must seed the first reusable buffer");
    let id = buffer.id();
    buffer.capacity_mut()[..b"native ingress".len()].copy_from_slice(b"native ingress");
    let signal = IngressSignalObservation::new(-93, 5);
    let packet = buffer
        .seal_with_signal(b"native ingress".len(), Some(signal))
        .expect("valid signalled ingress must seal");
    assert_eq!(packet.id(), id);
    assert_eq!(packet.as_bytes(), b"native ingress");
    assert_eq!(packet.signal(), Some(signal));
    actors[0]
        .try_send_ingress(authority, packet)
        .expect("current ingress must enter the actor queue");

    let validated = router
        .try_receive_ingress()
        .expect("current ingress must validate")
        .expect("completed ingress must be queued");
    assert_eq!(validated.interface(), first.lease().interface());
    assert_eq!(validated.descriptor(), first);
    assert_eq!(validated.signal(), Some(signal));
    let packet = validated.into_packet();
    assert_eq!(packet.id(), id);
    assert_eq!(packet.as_ref(), b"native ingress");
    router
        .try_return_ingress_buffer(packet)
        .expect("paired pool accounting must admit exact return");

    let reused = actors[0]
        .try_receive_ingress_buffer()
        .expect("returned exact buffer must be reusable");
    assert_eq!(reused.id(), id);
    assert_eq!(actors[0].available_ingress_buffers(), 0);
    let packet = seal_bytes(reused, b"again");
    assert_eq!(packet.as_bytes(), b"again");
    assert_eq!(packet.signal(), None);
    router
        .try_return_ingress_buffer(packet)
        .expect("unsubmitted acquired buffer may be explicitly recycled");
    assert_eq!(actors[0].available_ingress_buffers(), 1);
}

#[test]
fn split_tx_and_ingress_capabilities_share_origin_across_two_slots() {
    let (mut router, actors) = fabric::<1>();
    let (first, second) = configure(&mut router, &actors);
    let [first_actor, second_actor] = actors;
    let (first_tx, mut first_rx, _first_lifecycle) = first_actor.into_parts();
    let (second_tx, mut second_rx, _second_lifecycle) = second_actor.into_parts();
    assert_eq!(first_tx.queue_id(), first.lease().queue());
    assert_eq!(first_rx.queue_id(), first.lease().queue());
    assert_eq!(second_tx.queue_id(), second.lease().queue());
    assert_eq!(second_rx.queue_id(), second.lease().queue());
    assert_eq!(first_rx.available_buffers(), 1);
    assert_eq!(second_rx.available_buffers(), 1);

    let first_authority = first_rx
        .bind_ingress(first)
        .expect("first split RX capability must bind its descriptor");
    let second_authority = second_rx
        .bind_ingress(second)
        .expect("second split RX capability must bind its descriptor");
    let first_buffer = first_rx
        .try_receive_buffer()
        .expect("first split pool must be seeded");
    first_rx
        .try_send(first_authority, seal_bytes(first_buffer, b"first"))
        .expect("first split endpoint must share router origin");
    let second_buffer = second_rx
        .try_receive_buffer()
        .expect("second split pool must be seeded");
    second_rx
        .try_send(second_authority, seal_bytes(second_buffer, b"second"))
        .expect("second split endpoint must share router origin");

    let first_packet = router
        .try_receive_ingress()
        .expect("first packet must validate")
        .expect("first packet must be queued")
        .into_packet();
    let second_packet = router
        .try_receive_ingress()
        .expect("second packet must validate")
        .expect("second packet must be queued")
        .into_packet();
    assert_eq!(first_packet.as_bytes(), b"first");
    assert_eq!(second_packet.as_bytes(), b"second");
    router
        .try_return_ingress_buffer(first_packet)
        .expect("first common-origin buffer must return");
    router
        .try_return_ingress_buffer(second_packet)
        .expect("second common-origin buffer must return");
    assert_eq!(first_rx.available_buffers(), 1);
    assert_eq!(second_rx.available_buffers(), 1);
}

#[test]
fn stale_ingress_retains_exact_packet_for_recycle() {
    let (mut router, mut actors) = fabric::<1>();
    let (first, _) = configure(&mut router, &actors);
    let authority = actors[0]
        .bind_ingress(first)
        .expect("matching actor must bind old authority");
    let packet = seal_bytes(
        actors[0]
            .try_receive_ingress_buffer()
            .expect("pool must be seeded"),
        b"stale but owned",
    );
    let id = packet.id();
    actors[0]
        .try_send_ingress(authority, packet)
        .expect("old authority is current at actor submission");
    let current = router
        .reconfigure(
            first.lease(),
            InterfaceProperties::new(
                first.logical_mtu(),
                InterfaceConfigId::new(22),
                first.advertised_bitrate(),
                first.cost(),
            ),
            true,
        )
        .expect("reconfiguration must advance generation");

    let failure = match router.try_receive_ingress() {
        Err(failure) => failure,
        Ok(_) => panic!("superseded ingress authority reached node-core"),
    };
    assert!(matches!(
        failure.reason(),
        IngressRouteError::StaleLease(InterfaceLeaseError::Stale { supplied, current: observed })
            if supplied == first.lease() && observed == current
    ));
    let packet = failure.into_packet();
    assert_eq!(packet.id(), id);
    assert_eq!(packet.as_bytes(), b"stale but owned");
    router
        .try_return_ingress_buffer(packet)
        .expect("stale provenance must not prevent buffer recycle");
    let recovered = actors[0]
        .try_receive_ingress_buffer()
        .expect("stale exact buffer must return to its actor");
    assert_eq!(recovered.id(), id);
    router
        .try_return_ingress_buffer(seal_bytes(recovered, b"recycled"))
        .expect("recovered buffer remains usable");
}

#[test]
fn actor_and_router_reject_crossed_ingress_authority_and_queue() {
    let (mut router, mut actors) = fabric::<2>();
    let (first, second) = configure(&mut router, &actors);
    let first_authority = actors[0]
        .bind_ingress(first)
        .expect("first authority must bind");
    let second_authority = actors[1]
        .bind_ingress(second)
        .expect("second authority must bind");

    let second_packet = seal_bytes(
        actors[1]
            .try_receive_ingress_buffer()
            .expect("second pool must be seeded"),
        b"wrong authority",
    );
    let second_id = second_packet.id();
    let failure = actors[1]
        .try_send_ingress(first_authority, second_packet)
        .expect_err("actor must reject authority for another queue");
    assert_eq!(
        failure.reason(),
        ActorIngressSendError::ForeignAuthorityQueue {
            expected: second.lease().queue(),
            supplied: first.lease().queue(),
        }
    );
    let (_, second_packet) = failure.into_parts();
    assert_eq!(second_packet.id(), second_id);
    router
        .try_return_ingress_buffer(second_packet)
        .expect("crossed-authority packet must recycle to its own pool");

    let first_packet = seal_bytes(
        actors[0]
            .try_receive_ingress_buffer()
            .expect("first pool must be seeded"),
        b"wrong actor",
    );
    let first_id = first_packet.id();
    let failure = actors[1]
        .try_send_ingress(second_authority, first_packet)
        .expect_err("actor must reject a buffer from another queue");
    assert_eq!(
        failure.reason(),
        ActorIngressSendError::ForeignBufferQueue {
            expected: second.lease().queue(),
            supplied: first.lease().queue(),
        }
    );
    let (_, first_packet) = failure.into_parts();
    assert_eq!(first_packet.id(), first_id);
    router
        .try_return_ingress_buffer(first_packet)
        .expect("crossed-buffer packet must recycle to its own pool");

    let injected = seal_bytes(
        actors[1]
            .try_receive_ingress_buffer()
            .expect("second buffer must return"),
        b"router defense",
    );
    if actors[1]
        .completed_ingress
        .try_send(InterfaceIngress {
            authority: first_authority,
            packet: injected,
        })
        .is_err()
    {
        panic!("test-only crossed injection must fit");
    }
    let failure = match router.try_receive_ingress() {
        Err(failure) => failure,
        Ok(_) => panic!("router accepted crossed authority"),
    };
    assert_eq!(
        failure.reason(),
        IngressRouteError::ForeignAuthorityQueue {
            observed: second.lease().queue(),
            supplied: first.lease().queue(),
        }
    );
    router
        .try_return_ingress_buffer(failure.into_packet())
        .expect("router-rejected packet must retain its return origin");

    let injected = seal_bytes(
        actors[0]
            .try_receive_ingress_buffer()
            .expect("first buffer must remain reusable"),
        b"router buffer defense",
    );
    if actors[1]
        .completed_ingress
        .try_send(InterfaceIngress {
            authority: second_authority,
            packet: injected,
        })
        .is_err()
    {
        panic!("test-only crossed-buffer injection must fit");
    }
    let failure = match router.try_receive_ingress() {
        Err(failure) => failure,
        Ok(_) => panic!("router accepted crossed buffer origin"),
    };
    assert_eq!(
        failure.reason(),
        IngressRouteError::ForeignBufferQueue {
            observed: second.lease().queue(),
            supplied: first.lease().queue(),
        }
    );
    router
        .try_return_ingress_buffer(failure.into_packet())
        .expect("crossed-buffer rejection must preserve original return queue");
}

#[test]
fn crossed_static_fabric_return_fails_closed_with_exact_owner() {
    let first_fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
        Box::leak(Box::new(InterfaceFabric::new()));
    let second_fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
        Box::leak(Box::new(InterfaceFabric::new()));
    let (mut first_router, [mut first_actor]) = first_fabric.split();
    let (mut second_router, [second_actor]) = second_fabric.split();
    let properties = InterfaceProperties::new(
        LogicalMtu::try_new(500).expect("MTU must be nonzero"),
        InterfaceConfigId::new(1),
        None,
        InterfaceCost::new(0),
    );
    first_router
        .register(
            first_actor.queue_id(),
            PacketInterfaceId::new(1),
            properties,
            true,
        )
        .expect("first fabric must register");
    second_router
        .register(
            second_actor.queue_id(),
            PacketInterfaceId::new(1),
            properties,
            true,
        )
        .expect("second fabric must register independently");
    let packet = seal_bytes(
        first_actor
            .try_receive_ingress_buffer()
            .expect("first fabric pool must be seeded"),
        b"foreign fabric",
    );
    let id = packet.id();
    let failure = second_router
        .try_return_ingress_buffer(packet)
        .expect_err("another fabric must reject the exact buffer owner");
    assert_eq!(
        failure.reason(),
        IngressBufferReturnError::ForeignFabricOrigin(id.queue())
    );
    let packet = failure.into_packet();
    assert_eq!(packet.id(), id);
    assert_eq!(packet.as_bytes(), b"foreign fabric");
    first_router
        .try_return_ingress_buffer(packet)
        .expect("origin router must accept the recovered exact owner");
    assert_eq!(first_actor.available_ingress_buffers(), 1);
}

#[test]
fn ingress_poll_and_wait_are_pressure_and_cancellation_safe() {
    let (mut router, mut actors) = fabric::<1>();
    let (first, _) = configure(&mut router, &actors);
    let authority = actors[0]
        .bind_ingress(first)
        .expect("matching authority must bind");
    let wake_counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake_counter));
    let mut context = Context::from_waker(&waker);

    let buffer = actors[0]
        .try_receive_ingress_buffer()
        .expect("pool must be seeded");
    let id = buffer.id();
    let mut wait_for_buffer = Box::pin(actors[0].receive_ingress_buffer());
    assert!(matches!(
        wait_for_buffer.as_mut().poll(&mut context),
        Poll::Pending
    ));
    drop(wait_for_buffer);

    let mut wait_for_ingress = Box::pin(router.receive_ingress());
    assert!(matches!(
        wait_for_ingress.as_mut().poll(&mut context),
        Poll::Pending
    ));
    actors[0]
        .try_send_ingress(authority, seal_bytes(buffer, b"wake router"))
        .expect("completed packet must wake router wait");
    assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
    drop(wait_for_ingress);
    let packet = router
        .try_receive_ingress()
        .expect("cancelled wait must leave exact packet queued")
        .expect("packet must remain queued")
        .into_packet();
    assert_eq!(packet.id(), id);
    router
        .try_return_ingress_buffer(packet)
        .expect("packet must return after cancelled router wait");
    let recovered = actors[0]
        .try_receive_ingress_buffer()
        .expect("cancelled actor wait must not reserve returned buffer");
    assert_eq!(recovered.id(), id);

    actors[0]
        .try_send_ingress(authority, seal_bytes(recovered, b"fill queue"))
        .expect("completed queue must accept its sole packet");
    let mut wait_for_capacity = Box::pin(actors[0].wait_ingress_send_capacity());
    assert!(matches!(
        wait_for_capacity.as_mut().poll(&mut context),
        Poll::Pending
    ));
    let packet = router
        .try_receive_ingress()
        .expect("router must release completed-queue pressure")
        .expect("pressure packet must exist")
        .into_packet();
    assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
    drop(wait_for_capacity);
    router
        .try_return_ingress_buffer(packet)
        .expect("capacity wait cancellation must not affect buffer return");
    assert_eq!(actors[0].available_ingress_buffers(), 1);
}

#[test]
fn completed_ingress_remains_valid_after_online_to_offline_transition() {
    let (mut router, mut actors) = fabric::<1>();
    let (first, _) = configure(&mut router, &actors);
    let authority = actors[0]
        .bind_ingress(first)
        .expect("matching authority must bind");
    let buffer = actors[0]
        .try_receive_ingress_buffer()
        .expect("pool must be seeded");
    actors[0]
        .try_send_ingress(authority, seal_bytes(buffer, b"already received"))
        .expect("completed RX must queue while online");
    let offline = router
        .set_online(first.lease(), false)
        .expect("current lease must transition offline");
    let validated = router
        .try_receive_ingress()
        .expect("offline transition must not stale completed RX")
        .expect("completed packet must remain queued");
    assert_eq!(validated.descriptor(), offline);
    assert!(!validated.descriptor().is_online());
    let packet = validated.into_packet();
    assert_eq!(packet.as_bytes(), b"already received");
    router
        .try_return_ingress_buffer(packet)
        .expect("offline ingress buffer must recycle normally");
}

#[test]
fn ingress_seal_enforces_nonzero_native_packet_capacity() {
    let (mut router, mut actors) = fabric::<1>();
    configure(&mut router, &actors);
    let buffer = actors[0]
        .try_receive_ingress_buffer()
        .expect("pool must be seeded");
    let id = buffer.id();
    let failure = match buffer.seal(0) {
        Err(failure) => failure,
        Ok(_) => panic!("zero-length native packet was accepted"),
    };
    assert_eq!(failure.reason(), IngressPacketLengthError::Zero);
    let buffer = failure.into_buffer();
    assert_eq!(buffer.id(), id);
    let failure = match buffer.seal(PACKET_CAPACITY + 1) {
        Err(failure) => failure,
        Ok(_) => panic!("oversized native packet was accepted"),
    };
    assert_eq!(
        failure.reason(),
        IngressPacketLengthError::TooLong {
            actual: PACKET_CAPACITY + 1,
            maximum: PACKET_CAPACITY,
        }
    );
    let mut buffer = failure.into_buffer();
    buffer.capacity_mut().fill(0x5a);
    let packet = buffer
        .seal(PACKET_CAPACITY)
        .expect("full native packet capacity must be valid");
    assert_eq!(packet.len(), PACKET_CAPACITY);
    assert!(!packet.is_empty());
    assert!(packet.as_bytes().iter().all(|byte| *byte == 0x5a));
    router
        .try_return_ingress_buffer(packet)
        .expect("maximum-length exact buffer must recycle");
}

#[test]
fn actor_rejects_packet_above_bound_logical_mtu_with_exact_owner() {
    let fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
        Box::leak(Box::new(InterfaceFabric::new()));
    let (mut router, [actor]) = fabric.split();
    let queue = actor.queue_id();
    let descriptor = router
        .register(
            queue,
            PacketInterfaceId::new(1),
            InterfaceProperties::new(
                LogicalMtu::try_new(8).expect("test MTU must be nonzero"),
                InterfaceConfigId::new(1),
                None,
                InterfaceCost::new(0),
            ),
            true,
        )
        .expect("test interface must register");
    let (_tx, mut ingress_actor, _lifecycle) = actor.into_parts();
    let authority = ingress_actor
        .bind_ingress(descriptor)
        .expect("same-queue descriptor must bind");
    let packet = seal_bytes(
        ingress_actor
            .try_receive_buffer()
            .expect("ingress pool must be seeded"),
        b"ninebytes",
    );
    let id = packet.id();

    let failure = ingress_actor
        .try_send(authority, packet)
        .expect_err("actor must enforce its bound logical MTU");
    assert_eq!(
        failure.reason(),
        ActorIngressSendError::PacketExceedsBoundMtu {
            actual: 9,
            maximum: LogicalMtu::try_new(8).expect("test MTU must be nonzero"),
        }
    );
    let (returned_authority, packet) = failure.into_parts();
    assert_eq!(returned_authority, authority);
    assert_eq!(packet.id(), id);
    assert_eq!(packet.as_bytes(), b"ninebytes");
    assert!(
        router
            .try_receive_ingress()
            .expect("empty ingress scan must succeed")
            .is_none()
    );
    router
        .try_return_ingress_buffer(packet)
        .expect("actor-rejected exact buffer must recycle");
    assert_eq!(ingress_actor.available_buffers(), 1);
}

#[test]
fn router_rechecks_current_logical_mtu_and_retains_exact_owner() {
    let current_fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
        Box::leak(Box::new(InterfaceFabric::new()));
    let authority_fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
        Box::leak(Box::new(InterfaceFabric::new()));
    let (mut current_router, [mut current_actor]) = current_fabric.split();
    let (mut authority_router, [authority_actor]) = authority_fabric.split();
    let current_descriptor = current_router
        .register(
            current_actor.queue_id(),
            PacketInterfaceId::new(1),
            InterfaceProperties::new(
                LogicalMtu::try_new(8).expect("test MTU must be nonzero"),
                InterfaceConfigId::new(1),
                None,
                InterfaceCost::new(0),
            ),
            true,
        )
        .expect("current interface must register");
    let foreign_descriptor = authority_router
        .register(
            authority_actor.queue_id(),
            PacketInterfaceId::new(1),
            InterfaceProperties::new(
                LogicalMtu::try_new(500).expect("test MTU must be nonzero"),
                InterfaceConfigId::new(99),
                None,
                InterfaceCost::new(0),
            ),
            true,
        )
        .expect("authority fixture must register");
    assert_eq!(foreign_descriptor.lease(), current_descriptor.lease());
    let authority = current_actor
        .bind_ingress(foreign_descriptor)
        .expect("same lease-shaped descriptor must bind to the queue");
    let packet = seal_bytes(
        current_actor
            .try_receive_ingress_buffer()
            .expect("current pool must be seeded"),
        b"ninebytes",
    );
    let id = packet.id();
    current_actor
        .try_send_ingress(authority, packet)
        .expect("actor-bound larger MTU admits the defense-in-depth fixture");

    let failure = match current_router.try_receive_ingress() {
        Err(failure) => failure,
        Ok(_) => panic!("router accepted a packet above its authoritative current MTU"),
    };
    assert_eq!(
        failure.reason(),
        IngressRouteError::PacketExceedsCurrentMtu {
            actual: 9,
            maximum: LogicalMtu::try_new(8).expect("test MTU must be nonzero"),
        }
    );
    let packet = failure.into_packet();
    assert_eq!(packet.id(), id);
    assert_eq!(packet.as_bytes(), b"ninebytes");
    current_router
        .try_return_ingress_buffer(packet)
        .expect("router-rejected exact buffer must recycle");
    assert_eq!(current_actor.available_ingress_buffers(), 1);
}

#[test]
fn ingress_receive_scan_is_fair_across_two_slots() {
    let (mut router, mut actors) = fabric::<2>();
    let (first, second) = configure(&mut router, &actors);
    let first_authority = actors[0]
        .bind_ingress(first)
        .expect("first authority must bind");
    let second_authority = actors[1]
        .bind_ingress(second)
        .expect("second authority must bind");
    let first_a = actors[0]
        .try_receive_ingress_buffer()
        .expect("first pool must have buffer A");
    actors[0]
        .try_send_ingress(first_authority, seal_bytes(first_a, b"first-a"))
        .expect("first A must queue");
    let second_a = actors[1]
        .try_receive_ingress_buffer()
        .expect("second pool must have buffer A");
    actors[1]
        .try_send_ingress(second_authority, seal_bytes(second_a, b"second-a"))
        .expect("second A must queue");

    let first_packet = router
        .try_receive_ingress()
        .expect("first scan must validate")
        .expect("first scan must find a packet");
    assert_eq!(first_packet.interface(), first.lease().interface());
    let first_b = actors[0]
        .try_receive_ingress_buffer()
        .expect("first pool must have buffer B");
    actors[0]
        .try_send_ingress(first_authority, seal_bytes(first_b, b"first-b"))
        .expect("first B must queue behind first scan");

    let second_packet = router
        .try_receive_ingress()
        .expect("second scan must validate")
        .expect("second scan must find a packet");
    assert_eq!(second_packet.interface(), second.lease().interface());
    let second_a = second_packet.into_packet();
    assert_eq!(second_a.as_bytes(), b"second-a");
    let next_first = router
        .try_receive_ingress()
        .expect("third scan must validate")
        .expect("third scan must find first B");
    assert_eq!(next_first.interface(), first.lease().interface());
    let first_a = first_packet.into_packet();
    let first_b = next_first.into_packet();
    router
        .try_return_ingress_buffer(first_a)
        .expect("first A must return");
    router
        .try_return_ingress_buffer(first_b)
        .expect("first B must return");
    router
        .try_return_ingress_buffer(second_a)
        .expect("second A must return");
}

#[test]
fn lifecycle_requests_apply_ready_and_offline_under_the_same_generation() {
    let (mut router, actors) = fabric::<1>();
    let (first, second) = configure(&mut router, &actors);
    let [first_actor, second_actor] = actors;
    router
        .set_online(first.lease(), false)
        .expect("first current lease must transition offline");
    router
        .set_online(second.lease(), false)
        .expect("second current lease must transition offline");
    let (_, _, mut first_lifecycle) = first_actor.into_parts();
    let (_, _, mut second_lifecycle) = second_actor.into_parts();

    let first_request = first_lifecycle
        .try_request_state(first.lease(), InterfaceLifecycleState::Ready)
        .expect("first actor request queue must be empty");
    let second_request = second_lifecycle
        .try_request_state(second.lease(), InterfaceLifecycleState::Ready)
        .expect("second actor request queue must be empty");

    let first_transition = router
        .try_process_lifecycle()
        .expect("first fair lifecycle pass must progress");
    assert_eq!(first_transition.queue(), first.lease().queue());
    assert_eq!(first_transition.acknowledgement().request(), first_request);
    assert!(
        first_transition
            .acknowledgement()
            .result()
            .expect("first ready request must apply")
            .is_online()
    );
    assert_eq!(
        first_lifecycle
            .try_finish_request()
            .expect("first acknowledgement must match")
            .expect("first acknowledgement must be retained"),
        first_transition
            .acknowledgement()
            .result()
            .expect("first transition already applied")
    );

    let second_transition = router
        .try_process_lifecycle()
        .expect("cursor must advance to the second actor");
    assert_eq!(second_transition.queue(), second.lease().queue());
    assert_eq!(
        second_transition.acknowledgement().request(),
        second_request
    );
    assert_eq!(
        second_lifecycle
            .try_finish_request()
            .expect("second acknowledgement must match")
            .expect("second acknowledgement must be retained"),
        second_transition
            .acknowledgement()
            .result()
            .expect("second transition already applied")
    );

    let offline_request = first_lifecycle
        .try_request_state(first.lease(), InterfaceLifecycleState::Offline)
        .expect("acknowledged actor can request another transition");
    let offline_transition = router
        .try_process_lifecycle()
        .expect("offline request must progress");
    assert_eq!(
        offline_transition.acknowledgement().request(),
        offline_request
    );
    assert!(
        !offline_transition
            .acknowledgement()
            .result()
            .expect("current offline request must apply")
            .is_online()
    );
    assert!(
        !first_lifecycle
            .try_finish_request()
            .expect("offline acknowledgement must match")
            .expect("offline acknowledgement must be retained")
            .is_online()
    );
    assert!(
        router
            .registry()
            .descriptor(second.lease().interface())
            .expect("second interface remains registered")
            .is_online()
    );
}

#[test]
fn lifecycle_requests_reject_crossed_queues_and_stale_generations() {
    let (mut router, actors) = fabric::<1>();
    let (first, second) = configure(&mut router, &actors);
    let [first_actor, second_actor] = actors;
    let (_, _, mut first_lifecycle) = first_actor.into_parts();
    let (_, _, _second_lifecycle) = second_actor.into_parts();

    assert_eq!(
        first_lifecycle.try_request_state(second.lease(), InterfaceLifecycleState::Offline,),
        Err(InterfaceLifecycleTryRequestError::ForeignQueue {
            actor: first.lease().queue(),
            supplied: second.lease().queue(),
        })
    );

    // Inject a malformed envelope behind the public actor guard to prove
    // the router independently treats its observed channel as authority.
    let crossed = InterfaceLifecycleRequest {
        lease: second.lease(),
        state: InterfaceLifecycleState::Offline,
    };
    first_lifecycle
        .requests
        .try_send(crossed)
        .expect("test injection queue must be empty");
    let unsent = InterfaceLifecycleRequest {
        lease: first.lease(),
        state: InterfaceLifecycleState::Ready,
    };
    assert_eq!(
        first_lifecycle.try_request_state(first.lease(), InterfaceLifecycleState::Ready),
        Err(InterfaceLifecycleTryRequestError::RequestQueueFull(unsent))
    );
    let crossed_transition = router
        .try_process_lifecycle()
        .expect("crossed request must be acknowledged as rejected");
    assert_eq!(
        crossed_transition.acknowledgement().result(),
        Err(InterfaceLifecycleRouteError::ForeignQueue {
            observed: first.lease().queue(),
            supplied: second.lease().queue(),
        })
    );
    assert!(
        router
            .registry()
            .descriptor(second.lease().interface())
            .expect("crossed request must not remove the second interface")
            .is_online()
    );
    assert_eq!(
        first_lifecycle.try_finish_request(),
        Err(InterfaceLifecycleActorError::UnexpectedAcknowledgement(
            crossed
        ))
    );

    let replacement = router
        .reconfigure(first.lease(), first.properties(), false)
        .expect("current first lease must reconfigure");
    first_lifecycle
        .try_request_state(first.lease(), InterfaceLifecycleState::Ready)
        .expect("stale request still names the actor's fixed queue");
    let stale_transition = router
        .try_process_lifecycle()
        .expect("stale request must be acknowledged as rejected");
    assert!(matches!(
        stale_transition.acknowledgement().result(),
        Err(InterfaceLifecycleRouteError::InvalidLease(
            InterfaceLeaseError::Stale { supplied, current }
        )) if supplied == first.lease() && current == replacement
    ));
    assert!(
        !router
            .registry()
            .descriptor(first.lease().interface())
            .expect("replacement remains registered")
            .is_online()
    );
    assert!(matches!(
        first_lifecycle.try_finish_request(),
        Err(InterfaceLifecycleActorError::Rejected(
            InterfaceLifecycleRouteError::InvalidLease(InterfaceLeaseError::Stale {
                supplied,
                current,
            })
        )) if supplied == first.lease() && current == replacement
    ));
}

#[test]
fn lifecycle_acknowledgement_pressure_retains_the_next_request() {
    let (mut router, actors) = fabric::<1>();
    let (first, _) = configure(&mut router, &actors);
    let [first_actor, _second_actor] = actors;
    let (_, _, mut lifecycle) = first_actor.into_parts();

    let first_request = lifecycle
        .try_request_state(first.lease(), InterfaceLifecycleState::Offline)
        .expect("first request must enqueue");
    router
        .try_process_lifecycle()
        .expect("first request must be applied and acknowledged");
    let second_request = InterfaceLifecycleRequest {
        lease: first.lease(),
        state: InterfaceLifecycleState::Ready,
    };
    assert_eq!(
        lifecycle.try_request_state(first.lease(), InterfaceLifecycleState::Ready),
        Err(InterfaceLifecycleTryRequestError::ExchangePending {
            pending: first_request,
            unsent: second_request,
        })
    );
    lifecycle
        .requests
        .try_send(second_request)
        .expect("private injection isolates router acknowledgement pressure");

    assert_eq!(router.try_process_lifecycle(), None);
    assert!(
        !router
            .registry()
            .descriptor(first.lease().interface())
            .expect("first interface remains registered")
            .is_online()
    );
    assert!(
        !lifecycle
            .try_finish_request()
            .expect("first acknowledgement must match")
            .expect("first acknowledgement remains exact")
            .is_online()
    );
    assert!(
        router
            .try_process_lifecycle()
            .expect("retained second request progresses after ack capacity returns")
            .acknowledgement()
            .result()
            .expect("second request is current")
            .is_online()
    );
    assert_eq!(
        lifecycle.try_finish_request(),
        Err(InterfaceLifecycleActorError::UnexpectedAcknowledgement(
            second_request
        ))
    );
}

#[test]
fn crossed_acknowledgement_preserves_the_pending_single_flight_exchange() {
    let (mut router, actors) = fabric::<1>();
    let (first, _) = configure(&mut router, &actors);
    let [first_actor, _second_actor] = actors;
    let (_, _, mut lifecycle) = first_actor.into_parts();
    let pending = lifecycle
        .try_request_state(first.lease(), InterfaceLifecycleState::Offline)
        .expect("first request must enqueue");
    let crossed = InterfaceLifecycleRequest {
        lease: first.lease(),
        state: InterfaceLifecycleState::Ready,
    };
    lifecycle
        .acknowledgements
        .try_send(InterfaceLifecycleAcknowledgement {
            request: crossed,
            result: Ok(first),
        })
        .expect("private crossed acknowledgement injection must fit");

    assert_eq!(
        lifecycle.try_finish_request(),
        Err(InterfaceLifecycleActorError::CrossedAcknowledgement {
            expected: pending,
            received: crossed,
        })
    );
    assert_eq!(
        lifecycle.try_request_state(first.lease(), InterfaceLifecycleState::Ready),
        Err(InterfaceLifecycleTryRequestError::ExchangePending {
            pending,
            unsent: crossed,
        })
    );

    let transition = router
        .try_process_lifecycle()
        .expect("original pending request must still progress");
    assert_eq!(transition.acknowledgement().request(), pending);
    assert!(
        !lifecycle
            .try_finish_request()
            .expect("original acknowledgement must correlate")
            .expect("original acknowledgement must be retained")
            .is_online()
    );
    lifecycle
        .try_request_state(first.lease(), InterfaceLifecycleState::Ready)
        .expect("exact completion must reopen the single-flight phase");
}

#[test]
fn cancelled_async_wait_can_resume_the_exact_single_flight_exchange() {
    let (mut router, actors) = fabric::<1>();
    let (first, _) = configure(&mut router, &actors);
    let [first_actor, _second_actor] = actors;
    let (_, _, mut lifecycle) = first_actor.into_parts();
    assert_eq!(
        lifecycle.try_finish_request(),
        Err(InterfaceLifecycleActorError::NoPendingRequest)
    );
    let pending = InterfaceLifecycleRequest {
        lease: first.lease(),
        state: InterfaceLifecycleState::Offline,
    };
    let unsent = InterfaceLifecycleRequest {
        lease: first.lease(),
        state: InterfaceLifecycleState::Ready,
    };

    let mut future =
        Box::pin(lifecycle.request_state(first.lease(), InterfaceLifecycleState::Offline));
    let mut context = core::task::Context::from_waker(core::task::Waker::noop());
    assert!(matches!(
        core::future::Future::poll(future.as_mut(), &mut context),
        core::task::Poll::Pending
    ));
    let transition = router
        .try_process_lifecycle()
        .expect("polled request must be visible to the router");
    assert_eq!(transition.acknowledgement().request(), pending);
    drop(future);

    assert_eq!(
        lifecycle.try_request_state(first.lease(), InterfaceLifecycleState::Ready),
        Err(InterfaceLifecycleTryRequestError::ExchangePending { pending, unsent })
    );
    assert!(
        !lifecycle
            .try_finish_request()
            .expect("cancelled wait must preserve exact correlation")
            .expect("router acknowledgement must remain retained")
            .is_online()
    );
    lifecycle
        .try_request_state(first.lease(), InterfaceLifecycleState::Ready)
        .expect("resumed completion must reopen the single-flight phase");
}

#[test]
fn registry_rejects_duplicate_ids_and_preserves_generation_across_vacancy() {
    let mut registry = InterfaceRegistry::<2>::new();
    let mtu = LogicalMtu::try_new(500).expect("MTU must be nonzero");
    let first = registry
        .register(
            InterfaceQueueId::new(0),
            PacketInterfaceId::new(7),
            InterfaceProperties::new(mtu, InterfaceConfigId::new(1), None, InterfaceCost::new(0)),
            true,
        )
        .expect("first registration must fit");
    assert_eq!(
        registry.register(
            InterfaceQueueId::new(1),
            PacketInterfaceId::new(7),
            InterfaceProperties::new(mtu, InterfaceConfigId::new(2), None, InterfaceCost::new(1),),
            true,
        ),
        Err(InterfaceRegistrationError::DuplicateInterface {
            interface: PacketInterfaceId::new(7),
            queue: InterfaceQueueId::new(0),
        })
    );
    registry
        .unregister(first.lease())
        .expect("current lease must unregister");
    let replacement = registry
        .register(
            InterfaceQueueId::new(0),
            PacketInterfaceId::new(8),
            InterfaceProperties::new(mtu, InterfaceConfigId::new(3), None, InterfaceCost::new(2)),
            true,
        )
        .expect("vacant queue must re-register");
    assert!(replacement.lease().generation() > first.lease().generation());
    assert!(matches!(
        registry.validate(first.lease()),
        Err(InterfaceLeaseError::Stale { .. })
    ));

    let mut outside = InterfaceRegistry::<1>::new();
    let outside_descriptor = outside
        .register(
            InterfaceQueueId::new(0),
            PacketInterfaceId::new(64),
            InterfaceProperties::new(mtu, InterfaceConfigId::new(4), None, InterfaceCost::new(3)),
            false,
        )
        .expect("registry can represent native one-byte interface IDs");
    assert_eq!(
        outside.eligible_interfaces(),
        Err(EligibleInterfaceSetError::InterfaceOutsideProfile(
            outside_descriptor
        ))
    );
}
