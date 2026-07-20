//! Permanent transport-neutral node and interface orchestration.

use core::mem;

use embassy_sync::blocking_mutex::raw::RawMutex;
use rand_core::{CryptoRng, RngCore};
use reticulum_interface_router::{
    CompletionRouteError, IngressBufferId, IngressBufferReturnError, IngressBufferReturnFailure,
    IngressRouteError, InterfaceActorHandoff, InterfaceDescriptor, InterfaceFabric, InterfaceLease,
    InterfaceLeaseError, InterfaceProperties, InterfaceQueueId, InterfaceRegistrationError,
    InterfaceRegistry, OutboundCompletion, OutboundRouter, SealedIngressPacket,
};
use reticulum_node_core::{
    AcknowledgeError, AnnounceAdmissionError, AnnounceEmissionTime, AttemptHandle,
    CapacitySnapshot, DestinationHash, IngressReport, MaintenanceReport, MonotonicInstant,
    MonotonicMillis, MonotonicSeconds, NodeActions, NodeCore, OrdinaryActionCapacitySnapshot,
    OrdinaryPreparedPacket, OutboundDispatchInterval, OutboundProtocolToken, PacketInterfaceId,
    PreparedPacket, ReceiptCorrelationError, TerminalAttempt, TerminalAttempts,
    TxAuthorizationPolicy, TxLeaseDeadline, TxMaintenanceReport, TxOwnerScope,
    TxRecoveryObservation,
};
use reticulum_tx_handoff::{
    DataPairedPermitHandoff, DispatcherPermitHandoff, OrdinaryDispatcherPermitHandoff,
    OrdinaryPairedPermitHandoff,
};

use crate::{
    DataCompletionAcceptError, DataPermitServer, DataPermitServerPhase, DataPermitServerStep,
    DataRecoveryAckError, DataRouterCoordinator, DataRouterFault, DataRouterOwnerMismatch,
    DataRouterParkedCounts, DataRouterPrepareRequest, DataRouterPrepareResult, DataRouterStep,
    OrdinaryCompletionAcceptError, OrdinaryPermitServer, OrdinaryPermitServerPhase,
    OrdinaryPermitServerStep, OrdinaryRouterAdmission, OrdinaryRouterCoordinator,
    OrdinaryRouterFault, OrdinaryRouterOfferError, OrdinaryRouterOfferFailure,
    OrdinaryRouterRejectedActions, OrdinaryRouterStep,
};

/// Why permanent node/interface aggregate construction failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceSupervisorBuildError {
    /// A permanent aggregate requires at least one concrete interface slot.
    ZeroInterfaceSlots,
    /// The DATA coordinator belongs to another node incarnation.
    DataOwnerMismatch {
        /// Scope supplied by the node owner.
        node: TxOwnerScope,
        /// Scope retained by the DATA coordinator.
        coordinator: TxOwnerScope,
    },
    /// The ordinary coordinator belongs to another node incarnation.
    OrdinaryOwnerMismatch {
        /// Scope supplied by the node owner.
        node: TxOwnerScope,
        /// Scope retained by the ordinary coordinator.
        coordinator: TxOwnerScope,
    },
}

/// Failed construction retaining every supplied non-copy component unchanged.
#[must_use = "failed aggregate construction retains every permanent owner"]
pub struct NodeInterfaceSupervisorBuildFailure<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const DATA_PACKET_BUFFERS: usize,
    const ORDINARY_PACKET_BUFFERS: usize,
    const SLOTS: usize,
    const QUEUE_DEPTH: usize,
> where
    M: RawMutex + 'static,
{
    reason: NodeInterfaceSupervisorBuildError,
    node: NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
    fabric: &'static mut InterfaceFabric<M, SLOTS, QUEUE_DEPTH>,
    data: DataRouterCoordinator<DATA_PACKET_BUFFERS>,
    ordinary: OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
    data_permits: [DataPairedPermitHandoff<M>; SLOTS],
    ordinary_permits: [OrdinaryPairedPermitHandoff<M>; SLOTS],
    policy: P,
}

impl<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const DATA_PACKET_BUFFERS: usize,
    const ORDINARY_PACKET_BUFFERS: usize,
    const SLOTS: usize,
    const QUEUE_DEPTH: usize,
>
    NodeInterfaceSupervisorBuildFailure<
        M,
        P,
        PATHS,
        ANNOUNCES,
        DEDUPLICATION,
        LINKS,
        DATA_PACKET_BUFFERS,
        ORDINARY_PACKET_BUFFERS,
        SLOTS,
        QUEUE_DEPTH,
    >
where
    M: RawMutex + 'static,
{
    /// Scalar construction rejection.
    pub const fn reason(&self) -> NodeInterfaceSupervisorBuildError {
        self.reason
    }

    /// Recover every unchanged construction input in argument order.
    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
        &'static mut InterfaceFabric<M, SLOTS, QUEUE_DEPTH>,
        DataRouterCoordinator<DATA_PACKET_BUFFERS>,
        OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
        [DataPairedPermitHandoff<M>; SLOTS],
        [OrdinaryPairedPermitHandoff<M>; SLOTS],
        P,
    ) {
        (
            self.node,
            self.fabric,
            self.data,
            self.ordinary,
            self.data_permits,
            self.ordinary_permits,
            self.policy,
        )
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use reticulum_interface_router::{
        InterfaceConfigId, InterfaceCost, InterfaceTxJob, LogicalMtu,
    };
    use reticulum_node_core::{
        DestinationHash, NodeConfig, NodeIdentity, NodeInstanceId, OrdinaryBufferPool,
        OrdinaryPacketBuffer, PermitResolution, TxAuthorizationCandidate, TxCompletionCode,
        TxPacketBuffer, TxPermitRequirements, TxPermitReservation, TxPermitResourceId,
        TxPolicyDecision,
    };
    use reticulum_rns_rete::{IngressDisposition, PacketType};
    use reticulum_tx_handoff::{DataPermitHandoff, OrdinaryPermitHandoff};
    use std::boxed::Box;
    use std::vec::Vec;

    use super::*;
    use crate::{DataPreparedHop, DataRouterConfig, OrdinaryRouterConfig};

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
                .unwrap_or_else(|failure| {
                    panic!("ordinary buffer must park: {:?}", failure.reason())
                });
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
        let (mut node, destination) = sender(tag);
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
                .register_interface(
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
            NodeInterfaceDataPrepareResult::Coordinator(DataRouterPrepareResult::Prepared(hop)) => {
                hop
            }
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

    fn admission() -> OrdinaryRouterAdmission {
        OrdinaryRouterAdmission::new(TxLeaseDeadline::new(MonotonicMillis::new(100_000)))
    }

    fn requirements(tag: u8) -> TxPermitRequirements {
        TxPermitRequirements::try_new(TxPermitResourceId::new([tag; 16]), 1)
            .expect("permit requirements are nonzero")
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
                    let output = supervisor
                        .take_non_packet_actions()
                        .expect("non-packet output remains explicit");
                    assert!(output.packets.is_empty());
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
        supervisor
            .queue_announce(
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
                    let _ = supervisor.take_non_packet_actions();
                }
                NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Routed {
                    ..
                }) => {
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
        let failure =
            match supervisor.flush_announces(MonotonicSeconds::new(21), admission(), &mut rng) {
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
                NodeInterfaceSupervisorTransition::Ordinary(
                    OrdinaryRouterStep::NonPacketActionsReady
                )
            ) {
                let _ = supervisor.take_non_packet_actions();
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
        let (_tx, mut ingress) = interface.into_parts();
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
    fn queued_ingress_action_pressure_recycles_buffer_and_retries_exact_actions() {
        let (mut supervisor, [actor], _) = build::<1, 1>(18);
        let [descriptor] = register(
            &mut supervisor,
            core::slice::from_ref(&actor).try_into().unwrap(),
            [true],
        );
        let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
        let (mut tx, mut ingress) = interface.into_parts();
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

        let processed =
            match supervisor.step_ingress(MonotonicSeconds::new(40), admission(), &mut rng) {
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
                NodeInterfaceSupervisorTransition::Ordinary(
                    OrdinaryRouterStep::NonPacketActionsReady
                )
            ) {
                let _ = supervisor.take_non_packet_actions();
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
    fn oversized_queued_ingress_actions_are_terminal_and_takeable_without_loss() {
        let (mut supervisor, [actor], _) = build::<1, 1>(19);
        let (interface, _data_permit, _ordinary_permit) = actor.into_parts();
        let (_tx, mut ingress) = interface.into_parts();
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
                NodeInterfaceOrdinaryOfferError::Fault(
                    NodeInterfaceSupervisorFault::DataOwnerMismatch,
                ),
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
                actions: NodeActions {
                    unroutable_packets: index + 1,
                    ..NodeActions::default()
                },
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
        let (_tx, mut ingress) = interface.into_parts();
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
        let (_foreign_tx, mut foreign_ingress) = foreign_actor.into_parts();
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
        let (_tx, mut ingress) = interface.into_parts();
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
        let (_tx, mut ingress) = interface.into_parts();
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
        let (_tx, mut ingress) = interface.into_parts();
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
        let InterfaceTxJob::Data(job) =
            interface.try_receive_job().expect("DATA job reaches actor")
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
                NodeInterfaceSupervisorTransition::Ordinary(
                    OrdinaryRouterStep::NonPacketActionsReady
                )
            ) {
                events = supervisor
                    .take_non_packet_actions()
                    .expect("tick events remain explicit")
                    .events
                    .len();
                break;
            }
        }
        assert_eq!(events, 1);
    }

    #[test]
    fn newly_disabled_coordinator_is_surfaced_then_skipped_for_permit_drain() {
        let (mut supervisor, [actor0, actor1], _) = build::<2, 1>(10);
        let descriptor0 = supervisor
            .register_interface(
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
                NodeInterfaceSupervisorTransition::Ordinary(
                    OrdinaryRouterStep::NonPacketActionsReady
                )
            ) {
                let _ = supervisor.take_non_packet_actions();
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
            .register_interface(
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
                NodeInterfaceSupervisorTransition::Ordinary(
                    OrdinaryRouterStep::NonPacketActionsReady
                )
            ) {
                let _ = supervisor.take_non_packet_actions();
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
            .set_interface_online(descriptors[0].lease(), true)
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
                NodeInterfaceSupervisorTransition::Ordinary(
                    OrdinaryRouterStep::NonPacketActionsReady
                )
            ) {
                let _ = supervisor.take_non_packet_actions();
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
                    let _ = supervisor.take_non_packet_actions();
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
        let (node, fabric, data, ordinary, data_pairs, ordinary_pairs, policy) =
            failure.into_parts();
        assert_eq!(node.tx_owner_scope(), foreign_scope);
        assert_eq!(data.owner_scope(), owner_scope);
        assert_eq!(ordinary.owner_scope(), owner_scope);
        assert_eq!(core::ptr::from_mut(fabric), fabric_pointer);
        assert_eq!(data_pairs.len(), 1);
        assert_eq!(ordinary_pairs.len(), 1);
        assert_eq!(policy.calls, 7);
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
}

/// Interface-router and permit capabilities permanently associated with one
/// actor slot.
#[must_use = "dropping actor ports abandons its router and permit capabilities"]
pub struct NodeInterfaceActorPorts<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    interface: InterfaceActorHandoff<M, QUEUE_DEPTH>,
    data_permit: DispatcherPermitHandoff<M>,
    ordinary_permit: OrdinaryDispatcherPermitHandoff<M>,
}

impl<M, const QUEUE_DEPTH: usize> NodeInterfaceActorPorts<M, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Fixed interface queue associated with both permit pairs.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.interface.queue_id()
    }

    /// Consume this common-slot group into the concrete actor capabilities.
    pub fn into_parts(
        self,
    ) -> (
        InterfaceActorHandoff<M, QUEUE_DEPTH>,
        DispatcherPermitHandoff<M>,
        OrdinaryDispatcherPermitHandoff<M>,
    ) {
        (self.interface, self.data_permit, self.ordinary_permit)
    }
}

/// Successful checked construction and the actor capabilities split from the
/// same interface fabric and permit stores.
#[must_use = "the permanent supervisor and every actor capability must be retained"]
pub struct NodeInterfaceSupervisorBuildSuccess<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const DATA_PACKET_BUFFERS: usize,
    const ORDINARY_PACKET_BUFFERS: usize,
    const SLOTS: usize,
    const QUEUE_DEPTH: usize,
> where
    M: RawMutex + 'static,
{
    supervisor: NodeInterfaceSupervisor<
        M,
        P,
        PATHS,
        ANNOUNCES,
        DEDUPLICATION,
        LINKS,
        DATA_PACKET_BUFFERS,
        ORDINARY_PACKET_BUFFERS,
        SLOTS,
        QUEUE_DEPTH,
    >,
    actors: [NodeInterfaceActorPorts<M, QUEUE_DEPTH>; SLOTS],
}

impl<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const DATA_PACKET_BUFFERS: usize,
    const ORDINARY_PACKET_BUFFERS: usize,
    const SLOTS: usize,
    const QUEUE_DEPTH: usize,
>
    NodeInterfaceSupervisorBuildSuccess<
        M,
        P,
        PATHS,
        ANNOUNCES,
        DEDUPLICATION,
        LINKS,
        DATA_PACKET_BUFFERS,
        ORDINARY_PACKET_BUFFERS,
        SLOTS,
        QUEUE_DEPTH,
    >
where
    M: RawMutex + 'static,
{
    /// Consume success into the permanent aggregate and per-slot actor ports.
    pub fn into_parts(
        self,
    ) -> (
        NodeInterfaceSupervisor<
            M,
            P,
            PATHS,
            ANNOUNCES,
            DEDUPLICATION,
            LINKS,
            DATA_PACKET_BUFFERS,
            ORDINARY_PACKET_BUFFERS,
            SLOTS,
            QUEUE_DEPTH,
        >,
        [NodeInterfaceActorPorts<M, QUEUE_DEPTH>; SLOTS],
    ) {
        (self.supervisor, self.actors)
    }
}

/// Packet family of a router-demultiplexed completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceCompletionFamily {
    /// Destination-DATA owner.
    Data,
    /// Allocation-backed ordinary protocol-action owner.
    Ordinary,
}

/// How one exact completion reached node-owner reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceCompletionOrigin {
    /// The completion still named the current registry generation.
    Current,
    /// Router attribution failed, but the exact owner entered explicit node
    /// recovery as required.
    Recovery(CompletionRouteError),
}

/// A coordinator rejected an exact completion after central demultiplexing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceSupervisorFault {
    /// The owned DATA coordinator and node incarnation unexpectedly diverged
    /// after successful construction.
    DataOwnerMismatch,
    /// DATA completion correlation rejected the retained owner.
    DataCompletionRejected(DataCompletionAcceptError),
    /// Ordinary completion correlation rejected the retained owner.
    OrdinaryCompletionRejected(OrdinaryCompletionAcceptError),
}

/// Copy-only identity of the exact completion retained behind an aggregate
/// fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceCompletionResidue {
    /// Destination-DATA completion binding.
    Data {
        /// Exact prepared DATA attempt metadata.
        prepared: PreparedPacket,
        /// Interface selected for this serialized hop.
        interface: PacketInterfaceId,
    },
    /// Ordinary completion binding.
    Ordinary(OrdinaryPreparedPacket),
}

struct CompletionFaultResidue {
    fault: NodeInterfaceSupervisorFault,
    completion: OutboundCompletion,
}

impl CompletionFaultResidue {
    fn binding(&self) -> NodeInterfaceCompletionResidue {
        match &self.completion {
            OutboundCompletion::Data(completion) => NodeInterfaceCompletionResidue::Data {
                prepared: completion.prepared(),
                interface: completion.interface(),
            },
            OutboundCompletion::Ordinary(completion) => {
                NodeInterfaceCompletionResidue::Ordinary(completion.prepared())
            }
        }
    }
}

/// One useful ownership transition completed by a bounded aggregate pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceSupervisorTransition {
    /// A current or recovery-path completion entered its matching coordinator.
    CompletionAccepted {
        /// Completion owner family.
        family: NodeInterfaceCompletionFamily,
        /// Current-generation or explicit recovery path.
        origin: NodeInterfaceCompletionOrigin,
    },
    /// One DATA coordinator transition progressed or entered a new permanent
    /// fault.
    Data(DataRouterStep),
    /// One ordinary coordinator transition progressed or entered a new
    /// permanent fault.
    Ordinary(OrdinaryRouterStep),
    /// One concrete actor's DATA permit service advanced or entered a new
    /// terminal fault.
    DataPermit {
        /// Stable interface actor slot.
        actor: usize,
        /// Observable permit transition.
        step: DataPermitServerStep,
    },
    /// One concrete actor's ordinary permit service advanced or entered a new
    /// terminal fault.
    OrdinaryPermit {
        /// Stable interface actor slot.
        actor: usize,
        /// Observable permit transition.
        step: OrdinaryPermitServerStep,
    },
    /// A coordinator rejected the exact router completion now retained by the
    /// aggregate.
    Fault(NodeInterfaceSupervisorFault),
    /// No lane could complete a useful synchronous transition.
    Idle,
}

/// Result of one bounded synchronous aggregate pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "maintenance and ownership progress must be observed"]
pub struct NodeInterfaceSupervisorPass {
    maintenance: Result<TxMaintenanceReport, DataRouterOwnerMismatch>,
    transition: NodeInterfaceSupervisorTransition,
}

impl NodeInterfaceSupervisorPass {
    /// DATA owner maintenance performed exactly once at the start of the pass.
    pub const fn maintenance(self) -> Result<TxMaintenanceReport, DataRouterOwnerMismatch> {
        self.maintenance
    }

    /// At most one useful ownership transition selected by the fair scan.
    pub const fn transition(self) -> NodeInterfaceSupervisorTransition {
        self.transition
    }
}

/// Aggregate-level DATA preparation result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "DATA preparation and aggregate disablement must be observed"]
pub enum NodeInterfaceDataPrepareResult {
    /// The aggregate is retaining a rejected completion and accepts no new
    /// outbound owners.
    Fault(NodeInterfaceSupervisorFault),
    /// Result produced by the permanently owned DATA coordinator.
    Coordinator(DataRouterPrepareResult),
}

/// Why an ordinary action envelope was not accepted by the aggregate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceOrdinaryOfferError {
    /// The aggregate is retaining a rejected completion.
    Fault(NodeInterfaceSupervisorFault),
    /// Ordinary coordinator-local offer rejection.
    Coordinator(OrdinaryRouterOfferError),
}

/// Failed aggregate ordinary offer retaining the exact action envelope.
#[must_use = "an unaccepted ordinary action envelope remains exactly owned"]
pub struct NodeInterfaceOrdinaryOfferFailure {
    reason: NodeInterfaceOrdinaryOfferError,
    actions: NodeActions,
    admission: OrdinaryRouterAdmission,
}

/// Timer maintenance whose returned actions entered the ordinary coordinator.
#[must_use = "timer outcomes and receipt faults must be observed"]
pub struct NodeInterfaceTickAccepted {
    report: MaintenanceReport,
}

impl NodeInterfaceTickAccepted {
    /// Recover the timer report. `report.actions` is empty because the exact
    /// envelope already entered the ordinary coordinator.
    pub fn into_report(self) -> MaintenanceReport {
        self.report
    }
}

/// Timer maintenance completed, but its exact returned action envelope did
/// not enter the ordinary coordinator.
#[must_use = "failed timer action admission retains every returned action"]
pub struct NodeInterfaceTickActionFailure {
    report: MaintenanceReport,
    offer: NodeInterfaceOrdinaryOfferFailure,
}

impl NodeInterfaceTickActionFailure {
    /// Scalar ordinary-action admission reason.
    pub const fn reason(&self) -> NodeInterfaceOrdinaryOfferError {
        self.offer.reason()
    }

    /// Recover the action-free timer report and exact rejected action
    /// envelope.
    pub fn into_parts(self) -> (MaintenanceReport, NodeInterfaceOrdinaryOfferFailure) {
        (self.report, self.offer)
    }
}

/// Result of one protocol timer tick and immediate action admission attempt.
#[must_use = "timer actions and correlation faults require explicit handling"]
// The portable no_std boundary must return the exact allocation-backed action
// owner inline on rejection; boxing would hide the ownership contract.
#[allow(clippy::large_enum_variant)]
pub enum NodeInterfaceTickResult {
    /// Every returned action entered the ordinary coordinator.
    Accepted(NodeInterfaceTickAccepted),
    /// The exact returned actions remain in the failure envelope.
    ActionOfferRejected(NodeInterfaceTickActionFailure),
}

/// Result of flushing queued local announces and immediately admitting their
/// exact action envelope.
#[must_use = "flushed announce actions require explicit admission handling"]
// The portable no_std boundary must return the exact allocation-backed action
// owner inline on rejection; boxing would hide the ownership contract.
#[allow(clippy::large_enum_variant)]
pub enum NodeInterfaceAnnounceFlushResult {
    /// Every flushed announce action entered the ordinary coordinator.
    Accepted,
    /// The exact flushed action envelope remains in this failure.
    ActionOfferRejected(NodeInterfaceOrdinaryOfferFailure),
}

/// Non-retryable reason an ingress-produced action envelope could not enter
/// the ordinary coordinator.
///
/// [`OrdinaryRouterOfferError::Busy`] is deliberately absent: transient
/// pressure remains in the aggregate's retry slot instead of becoming a
/// terminal owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeInterfaceIngressActionFault {
    /// An aggregate completion/owner fault has stopped new ordinary intake.
    Aggregate(NodeInterfaceSupervisorFault),
    /// The ordinary coordinator is permanently disabled.
    CoordinatorDisabled(OrdinaryRouterFault),
    /// The exact action envelope exceeds the configured fixed packet pool.
    EnvelopeExceedsPool {
        /// Packets retained in the exact action envelope.
        packet_count: usize,
        /// Fixed ordinary packet-owner limit.
        limit: usize,
    },
}

/// Exact ingress-produced action envelope retained behind a non-retryable
/// admission fault.
#[must_use = "terminal ingress actions must be quarantined or recovered explicitly"]
pub struct NodeInterfaceTerminalIngressActions {
    fault: NodeInterfaceIngressActionFault,
    actions: NodeActions,
    admission: OrdinaryRouterAdmission,
}

impl NodeInterfaceTerminalIngressActions {
    /// Copy-only terminal classification for fail-stop policy.
    pub const fn fault(&self) -> NodeInterfaceIngressActionFault {
        self.fault
    }

    /// Recover the exact unchanged action envelope and admission request.
    pub fn into_parts(self) -> (NodeActions, OrdinaryRouterAdmission) {
        (self.actions, self.admission)
    }
}

/// Exact reusable-buffer return failure retained for retry or explicit
/// recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeInterfaceIngressRecycleFault {
    reason: IngressBufferReturnError,
    buffer: IngressBufferId,
}

impl NodeInterfaceIngressRecycleFault {
    /// Typed router rejection for the exact retained sealed buffer.
    pub const fn reason(self) -> IngressBufferReturnError {
        self.reason
    }

    /// Stable queue-local identity of the retained exact buffer.
    pub const fn buffer(self) -> IngressBufferId {
        self.buffer
    }

    /// Whether retrying the exact buffer return can recover from this reason.
    pub const fn is_retryable(self) -> bool {
        matches!(self.reason, IngressBufferReturnError::QueueFull(_))
    }

    /// Whether this reason requires explicit packet quarantine or recovery.
    pub const fn is_terminal(self) -> bool {
        !self.is_retryable()
    }
}

/// Successfully ingested queued packet and its action-free protocol report.
#[must_use = "queued ingress report and retained-action status must be observed"]
pub struct NodeInterfaceQueuedIngressProcessed {
    buffer: IngressBufferId,
    report: IngressReport,
    actions_backpressured: Option<NodeInterfaceOrdinaryOfferError>,
    terminal_action_fault: Option<NodeInterfaceIngressActionFault>,
    recycle_fault: Option<NodeInterfaceIngressRecycleFault>,
}

impl NodeInterfaceQueuedIngressProcessed {
    /// Stable exact buffer identity already recycled or retained for retry.
    pub const fn buffer(&self) -> IngressBufferId {
        self.buffer
    }

    /// Retryable action-admission pressure retained internally, if any.
    ///
    /// This is always [`OrdinaryRouterOfferError::Busy`]. Non-retryable
    /// failures are reported by [`Self::terminal_action_fault`].
    pub const fn actions_backpressured(&self) -> Option<NodeInterfaceOrdinaryOfferError> {
        self.actions_backpressured
    }

    /// Non-retryable action owner retained for explicit recovery, if any.
    pub const fn terminal_action_fault(&self) -> Option<NodeInterfaceIngressActionFault> {
        self.terminal_action_fault
    }

    /// Exact sealed-buffer recycle failure retained internally, if any.
    pub const fn recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        self.recycle_fault
    }

    /// Terminal exact-buffer residue created while processing this packet, if
    /// any.
    pub const fn terminal_recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        match self.recycle_fault {
            Some(fault) if fault.is_terminal() => Some(fault),
            _ => None,
        }
    }

    /// Recover the action-free node ingress report.
    pub fn into_report(self) -> IngressReport {
        self.report
    }
}

/// One bounded queued-ingress transition.
#[must_use = "queued ingress ownership and pressure must be observed"]
// The portable no_std boundary cannot heap-box the allocation-backed ingress
// report; shrinking this variant would otherwise hide or duplicate the exact
// result owner instead of returning it explicitly.
#[allow(clippy::large_enum_variant)]
pub enum NodeInterfaceIngressStep {
    /// A previously retained exact sealed buffer returned to its actor pool.
    RecycleRetried(IngressBufferId),
    /// Exact sealed buffer remains retained for a later recycle retry.
    RecycleBackpressured(NodeInterfaceIngressRecycleFault),
    /// A non-retryable exact sealed buffer remains retained until explicitly
    /// recovered from the aggregate.
    TerminalRecyclePending(NodeInterfaceIngressRecycleFault),
    /// A previously retained protocol-action envelope entered the ordinary
    /// coordinator.
    RetainedActionsAdmitted,
    /// A retained protocol-action envelope remains backpressured.
    ActionsBackpressured(NodeInterfaceOrdinaryOfferError),
    /// A non-retryable action envelope remains retained until explicitly
    /// recovered from the aggregate.
    TerminalActionsPending(NodeInterfaceIngressActionFault),
    /// Router validation rejected one sealed packet and its exact buffer was
    /// recycled or retained as reported.
    RouteRejected {
        /// Router provenance/origin rejection.
        reason: IngressRouteError,
        /// Stable exact buffer identity.
        buffer: IngressBufferId,
        /// Recycle fault retained internally, if exact return failed.
        recycle_fault: Option<NodeInterfaceIngressRecycleFault>,
    },
    /// Node receipt correlation rejected a packet after synchronous byte
    /// consumption; its exact buffer was recycled or retained as reported.
    CorrelationRejected {
        /// Fixed-ledger correlation reason.
        reason: ReceiptCorrelationError,
        /// Stable exact buffer identity.
        buffer: IngressBufferId,
        /// Recycle fault retained internally, if exact return failed.
        recycle_fault: Option<NodeInterfaceIngressRecycleFault>,
    },
    /// Node processing completed; exact action and recycle pressure are
    /// retained inside the aggregate and reported by copy-only status.
    Processed(NodeInterfaceQueuedIngressProcessed),
    /// No queued packet or retained ingress work was observable.
    Idle,
}

impl NodeInterfaceIngressStep {
    /// Terminal exact-buffer status reported by this transition, including an
    /// initial processed or rejected ingress result.
    pub const fn terminal_recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        let fault = match self {
            Self::TerminalRecyclePending(fault) => Some(*fault),
            Self::RouteRejected { recycle_fault, .. }
            | Self::CorrelationRejected { recycle_fault, .. } => *recycle_fault,
            Self::Processed(processed) => processed.recycle_fault(),
            _ => None,
        };
        match fault {
            Some(fault) if fault.is_terminal() => Some(fault),
            _ => None,
        }
    }
}

struct PendingIngressRecycle {
    status: NodeInterfaceIngressRecycleFault,
    packet: SealedIngressPacket,
}

enum IngressRecycleRetention {
    Retryable(NodeInterfaceIngressRecycleFault),
    Terminal(NodeInterfaceIngressRecycleFault),
}

enum IngressActionRetention {
    Retryable(NodeInterfaceOrdinaryOfferError),
    Terminal(NodeInterfaceIngressActionFault),
}

impl NodeInterfaceOrdinaryOfferFailure {
    /// Scalar rejection reason.
    pub const fn reason(&self) -> NodeInterfaceOrdinaryOfferError {
        self.reason
    }

    /// Recover the unchanged action envelope and admission policy.
    pub fn into_parts(self) -> (NodeActions, OrdinaryRouterAdmission) {
        (self.actions, self.admission)
    }
}

/// Permanent synchronous owner of one node, the shared interface router, both
/// outbound coordinator families, per-actor permit services, and product
/// authorization policy.
///
/// The aggregate exposes narrow identity, submission, and status methods only.
/// Concrete interface actors retain their router and permit capabilities
/// outside this value and remain free to implement any transport.
#[must_use = "dropping the permanent aggregate abandons exact outbound owners"]
pub struct NodeInterfaceSupervisor<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const DATA_PACKET_BUFFERS: usize,
    const ORDINARY_PACKET_BUFFERS: usize,
    const SLOTS: usize,
    const QUEUE_DEPTH: usize,
> where
    M: RawMutex + 'static,
{
    node: NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
    router: OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
    data: DataRouterCoordinator<DATA_PACKET_BUFFERS>,
    ordinary: OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
    data_permits: [DataPermitServer<M>; SLOTS],
    ordinary_permits: [OrdinaryPermitServer<M>; SLOTS],
    policy: P,
    lane_cursor: usize,
    completion_fault: Option<CompletionFaultResidue>,
    owner_mismatch_fault: bool,
    pending_ingress_actions: Option<NodeInterfaceOrdinaryOfferFailure>,
    terminal_ingress_actions: Option<NodeInterfaceTerminalIngressActions>,
    pending_ingress_recycle: Option<PendingIngressRecycle>,
    terminal_ingress_recycle: Option<PendingIngressRecycle>,
}

impl<
    M,
    P,
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const DATA_PACKET_BUFFERS: usize,
    const ORDINARY_PACKET_BUFFERS: usize,
    const SLOTS: usize,
    const QUEUE_DEPTH: usize,
>
    NodeInterfaceSupervisor<
        M,
        P,
        PATHS,
        ANNOUNCES,
        DEDUPLICATION,
        LINKS,
        DATA_PACKET_BUFFERS,
        ORDINARY_PACKET_BUFFERS,
        SLOTS,
        QUEUE_DEPTH,
    >
where
    M: RawMutex + 'static,
    P: TxAuthorizationPolicy,
{
    /// Consume every permanent component after validating common node
    /// ownership. Every failure returns every non-copy input unchanged.
    #[allow(clippy::too_many_arguments, clippy::result_large_err)]
    pub fn try_new(
        node: NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, DATA_PACKET_BUFFERS>,
        fabric: &'static mut InterfaceFabric<M, SLOTS, QUEUE_DEPTH>,
        data: DataRouterCoordinator<DATA_PACKET_BUFFERS>,
        ordinary: OrdinaryRouterCoordinator<ORDINARY_PACKET_BUFFERS>,
        data_permits: [DataPairedPermitHandoff<M>; SLOTS],
        ordinary_permits: [OrdinaryPairedPermitHandoff<M>; SLOTS],
        policy: P,
    ) -> Result<
        NodeInterfaceSupervisorBuildSuccess<
            M,
            P,
            PATHS,
            ANNOUNCES,
            DEDUPLICATION,
            LINKS,
            DATA_PACKET_BUFFERS,
            ORDINARY_PACKET_BUFFERS,
            SLOTS,
            QUEUE_DEPTH,
        >,
        NodeInterfaceSupervisorBuildFailure<
            M,
            P,
            PATHS,
            ANNOUNCES,
            DEDUPLICATION,
            LINKS,
            DATA_PACKET_BUFFERS,
            ORDINARY_PACKET_BUFFERS,
            SLOTS,
            QUEUE_DEPTH,
        >,
    > {
        let node_scope = node.tx_owner_scope();
        let reason = if SLOTS == 0 {
            Some(NodeInterfaceSupervisorBuildError::ZeroInterfaceSlots)
        } else if data.owner_scope() != node_scope {
            Some(NodeInterfaceSupervisorBuildError::DataOwnerMismatch {
                node: node_scope,
                coordinator: data.owner_scope(),
            })
        } else if ordinary.owner_scope() != node_scope {
            Some(NodeInterfaceSupervisorBuildError::OrdinaryOwnerMismatch {
                node: node_scope,
                coordinator: ordinary.owner_scope(),
            })
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(NodeInterfaceSupervisorBuildFailure {
                reason,
                node,
                fabric,
                data,
                ordinary,
                data_permits,
                ordinary_permits,
                policy,
            });
        }

        let (router, interface_actors) = fabric.split();
        let mut data_servers = core::array::from_fn(|_| None);
        let mut ordinary_servers = core::array::from_fn(|_| None);
        let mut data_pairs = data_permits.map(Some);
        let mut ordinary_pairs = ordinary_permits.map(Some);
        let mut interface_actors = interface_actors.map(Some);
        let actors = core::array::from_fn(|index| {
            let (data_node, data_permit) = data_pairs[index]
                .take()
                .expect("each validated DATA pair is split exactly once")
                .into_parts();
            let (ordinary_node, ordinary_permit) = ordinary_pairs[index]
                .take()
                .expect("each validated ordinary pair is split exactly once")
                .into_parts();
            data_servers[index] = Some(DataPermitServer::new(data_node));
            ordinary_servers[index] = Some(OrdinaryPermitServer::new(ordinary_node));
            NodeInterfaceActorPorts {
                interface: interface_actors[index]
                    .take()
                    .expect("each interface actor is moved exactly once"),
                data_permit,
                ordinary_permit,
            }
        });
        let data_permits = data_servers
            .map(|server| server.expect("every interface slot receives one DATA permit server"));
        let ordinary_permits = ordinary_servers.map(|server| {
            server.expect("every interface slot receives one ordinary permit server")
        });

        Ok(NodeInterfaceSupervisorBuildSuccess {
            supervisor: Self {
                node,
                router,
                data,
                ordinary,
                data_permits,
                ordinary_permits,
                policy,
                lane_cursor: 0,
                completion_fault: None,
                owner_mismatch_fault: false,
                pending_ingress_actions: None,
                terminal_ingress_actions: None,
                pending_ingress_recycle: None,
                terminal_ingress_recycle: None,
            },
            actors,
        })
    }

    /// Primary local Reticulum destination owned by this aggregate.
    pub fn destination_hash(&self) -> DestinationHash {
        self.node.destination_hash()
    }

    /// Register one stable interface identity in a vacant actor slot.
    pub fn register_interface(
        &mut self,
        queue: InterfaceQueueId,
        interface: PacketInterfaceId,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceRegistrationError> {
        self.router.register(queue, interface, properties, online)
    }

    /// Change whether one current interface accepts newly routed owners.
    pub fn set_interface_online(
        &mut self,
        lease: InterfaceLease,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.router.set_online(lease, online)
    }

    /// Confirm the bounded synchronous egress-handoff interval for one
    /// token-bearing ordinary protocol packet.
    ///
    /// This edge is successful interface/router acceptance, not physical
    /// transmission completion. The first matching serialized fan-out hop
    /// wins; callers use the `first_dispatch` field on the corresponding
    /// [`OrdinaryRouterStep::Routed`] transition to distinguish a rejected
    /// first confirmation from an expected false result on a later hop.
    pub fn confirm_outbound_protocol(
        &mut self,
        token: OutboundProtocolToken,
        interface: PacketInterfaceId,
        interval: OutboundDispatchInterval,
    ) -> bool {
        self.node
            .confirm_outbound_protocol(token, interface, interval)
    }

    /// Test-only generation change used to exercise stale completion recovery.
    ///
    /// Production code cannot safely reuse a generation while native RNS path
    /// state still refers only to the stable interface ID.
    #[cfg(test)]
    fn reconfigure_interface(
        &mut self,
        lease: InterfaceLease,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.router.reconfigure(lease, properties, online)
    }

    /// Prepare one destination-DATA packet using the router's authoritative
    /// eligible-interface snapshot.
    pub fn try_prepare_data<R>(
        &mut self,
        request: DataRouterPrepareRequest<'_>,
        now: MonotonicMillis,
        rng: &mut R,
    ) -> NodeInterfaceDataPrepareResult
    where
        R: RngCore + CryptoRng,
    {
        if let Some(fault) = self.fault() {
            return NodeInterfaceDataPrepareResult::Fault(fault);
        }
        NodeInterfaceDataPrepareResult::Coordinator(self.data.try_prepare_data(
            &mut self.node,
            &self.router,
            request,
            now,
            rng,
        ))
    }

    /// Offer one complete native ordinary-action envelope.
    // The allocation-backed action envelope must remain inline on failure;
    // this portable boundary cannot box or discard its exact owner.
    #[allow(clippy::result_large_err)]
    pub fn try_offer_actions(
        &mut self,
        actions: NodeActions,
        admission: OrdinaryRouterAdmission,
    ) -> Result<(), NodeInterfaceOrdinaryOfferFailure> {
        if let Some(fault) = self.fault() {
            return Err(NodeInterfaceOrdinaryOfferFailure {
                reason: NodeInterfaceOrdinaryOfferError::Fault(fault),
                actions,
                admission,
            });
        }
        self.ordinary.try_offer_actions(actions, admission).map_err(
            |failure: OrdinaryRouterOfferFailure| {
                let reason = failure.reason();
                let (actions, admission) = failure.into_parts();
                NodeInterfaceOrdinaryOfferFailure {
                    reason: NodeInterfaceOrdinaryOfferError::Coordinator(reason),
                    actions,
                    admission,
                }
            },
        )
    }

    /// Process at most one router-observed sealed ingress packet.
    ///
    /// A retryable exact buffer-return failure is retried first. A terminal
    /// buffer residue instead gates ingress until firmware explicitly takes
    /// its exact packet owner. Retained action work follows; only when all are
    /// clear can another actor packet be dequeued. Packet bytes are consumed
    /// synchronously, then the exact sealed owner is recycled before action
    /// admission is attempted, so ordinary backpressure cannot starve the
    /// actor's fixed RX pool.
    /// Permanent node tasks should fairly interleave this bounded lane with
    /// [`Self::step`].
    pub fn step_ingress<R>(
        &mut self,
        rns_now: MonotonicSeconds,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceIngressStep
    where
        R: RngCore + CryptoRng,
    {
        self.step_ingress_at(
            rns_now,
            MonotonicInstant::from_secs(rns_now.get()),
            admission,
            rng,
        )
    }

    /// Precise Link-clock variant of [`Self::step_ingress`].
    pub fn step_ingress_at<R>(
        &mut self,
        rns_now: MonotonicSeconds,
        link_now: MonotonicInstant,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceIngressStep
    where
        R: RngCore + CryptoRng,
    {
        if let Some(pending) = self.pending_ingress_recycle.take() {
            let buffer = pending.status.buffer;
            return match self.router.try_return_ingress_buffer(pending.packet) {
                Ok(()) => NodeInterfaceIngressStep::RecycleRetried(buffer),
                Err(failure) => match self.retain_ingress_recycle_failure(failure) {
                    IngressRecycleRetention::Retryable(status) => {
                        NodeInterfaceIngressStep::RecycleBackpressured(status)
                    }
                    IngressRecycleRetention::Terminal(status) => {
                        NodeInterfaceIngressStep::TerminalRecyclePending(status)
                    }
                },
            };
        }

        if let Some(terminal) = self.terminal_ingress_recycle.as_ref() {
            return NodeInterfaceIngressStep::TerminalRecyclePending(terminal.status);
        }

        if let Some(terminal) = self.terminal_ingress_actions.as_ref() {
            return NodeInterfaceIngressStep::TerminalActionsPending(terminal.fault());
        }

        if let Some(failure) = self.pending_ingress_actions.take() {
            let (actions, retained_admission) = failure.into_parts();
            return match self.try_offer_actions(actions, retained_admission) {
                Ok(()) => NodeInterfaceIngressStep::RetainedActionsAdmitted,
                Err(failure) => match self.retain_ingress_action_failure(failure) {
                    IngressActionRetention::Retryable(reason) => {
                        NodeInterfaceIngressStep::ActionsBackpressured(reason)
                    }
                    IngressActionRetention::Terminal(fault) => {
                        NodeInterfaceIngressStep::TerminalActionsPending(fault)
                    }
                },
            };
        }

        match self.router.try_receive_ingress() {
            Ok(Some(ingress)) => {
                let interface = ingress.interface();
                let packet = ingress.into_packet();
                let result =
                    self.node
                        .ingest_at(packet.as_ref(), rns_now, link_now, interface, rng);
                self.finish_queued_ingress(result, packet, admission)
            }
            Ok(None) => NodeInterfaceIngressStep::Idle,
            Err(failure) => {
                let reason = failure.reason();
                let packet = failure.into_packet();
                let buffer = packet.id();
                let recycle_fault = self.recycle_ingress_packet(packet);
                NodeInterfaceIngressStep::RouteRejected {
                    reason,
                    buffer,
                    recycle_fault,
                }
            }
        }
    }

    fn finish_queued_ingress(
        &mut self,
        result: Result<IngressReport, ReceiptCorrelationError>,
        packet: SealedIngressPacket,
        admission: OrdinaryRouterAdmission,
    ) -> NodeInterfaceIngressStep {
        let buffer = packet.id();
        let recycle_fault = self.recycle_ingress_packet(packet);
        match result {
            Err(reason) => NodeInterfaceIngressStep::CorrelationRejected {
                reason,
                buffer,
                recycle_fault,
            },
            Ok(mut report) => {
                let actions = mem::take(&mut report.actions);
                let (actions_backpressured, terminal_action_fault) =
                    if Self::actions_are_empty(&actions) {
                        (None, None)
                    } else {
                        match self.try_offer_actions(actions, admission) {
                            Ok(()) => (None, None),
                            Err(failure) => match self.retain_ingress_action_failure(failure) {
                                IngressActionRetention::Retryable(reason) => (Some(reason), None),
                                IngressActionRetention::Terminal(fault) => (None, Some(fault)),
                            },
                        }
                    };
                NodeInterfaceIngressStep::Processed(NodeInterfaceQueuedIngressProcessed {
                    buffer,
                    report,
                    actions_backpressured,
                    terminal_action_fault,
                    recycle_fault,
                })
            }
        }
    }

    fn retain_ingress_action_failure(
        &mut self,
        failure: NodeInterfaceOrdinaryOfferFailure,
    ) -> IngressActionRetention {
        match failure.reason() {
            reason @ NodeInterfaceOrdinaryOfferError::Coordinator(
                OrdinaryRouterOfferError::Busy(_),
            ) => {
                debug_assert!(self.pending_ingress_actions.is_none());
                debug_assert!(self.terminal_ingress_actions.is_none());
                self.pending_ingress_actions = Some(failure);
                IngressActionRetention::Retryable(reason)
            }
            NodeInterfaceOrdinaryOfferError::Fault(fault) => self.retain_terminal_ingress_actions(
                NodeInterfaceIngressActionFault::Aggregate(fault),
                failure,
            ),
            NodeInterfaceOrdinaryOfferError::Coordinator(OrdinaryRouterOfferError::Disabled(
                fault,
            )) => self.retain_terminal_ingress_actions(
                NodeInterfaceIngressActionFault::CoordinatorDisabled(fault),
                failure,
            ),
            NodeInterfaceOrdinaryOfferError::Coordinator(
                OrdinaryRouterOfferError::EnvelopeExceedsPool {
                    packet_count,
                    limit,
                },
            ) => self.retain_terminal_ingress_actions(
                NodeInterfaceIngressActionFault::EnvelopeExceedsPool {
                    packet_count,
                    limit,
                },
                failure,
            ),
        }
    }

    fn retain_terminal_ingress_actions(
        &mut self,
        fault: NodeInterfaceIngressActionFault,
        failure: NodeInterfaceOrdinaryOfferFailure,
    ) -> IngressActionRetention {
        debug_assert!(self.pending_ingress_actions.is_none());
        debug_assert!(self.terminal_ingress_actions.is_none());
        let (actions, admission) = failure.into_parts();
        self.terminal_ingress_actions = Some(NodeInterfaceTerminalIngressActions {
            fault,
            actions,
            admission,
        });
        IngressActionRetention::Terminal(fault)
    }

    fn recycle_ingress_packet(
        &mut self,
        packet: SealedIngressPacket,
    ) -> Option<NodeInterfaceIngressRecycleFault> {
        match self.router.try_return_ingress_buffer(packet) {
            Ok(()) => None,
            Err(failure) => {
                let status = match self.retain_ingress_recycle_failure(failure) {
                    IngressRecycleRetention::Retryable(status)
                    | IngressRecycleRetention::Terminal(status) => status,
                };
                Some(status)
            }
        }
    }

    fn retain_ingress_recycle_failure(
        &mut self,
        failure: IngressBufferReturnFailure,
    ) -> IngressRecycleRetention {
        let reason = failure.reason();
        self.retain_ingress_recycle(reason, failure.into_packet())
    }

    fn retain_ingress_recycle(
        &mut self,
        reason: IngressBufferReturnError,
        packet: SealedIngressPacket,
    ) -> IngressRecycleRetention {
        let status = NodeInterfaceIngressRecycleFault {
            reason,
            buffer: packet.id(),
        };
        let retained = PendingIngressRecycle { status, packet };
        if status.is_retryable() {
            debug_assert!(self.pending_ingress_recycle.is_none());
            debug_assert!(self.terminal_ingress_recycle.is_none());
            self.pending_ingress_recycle = Some(retained);
            IngressRecycleRetention::Retryable(status)
        } else {
            debug_assert!(self.pending_ingress_recycle.is_none());
            debug_assert!(self.terminal_ingress_recycle.is_none());
            self.terminal_ingress_recycle = Some(retained);
            IngressRecycleRetention::Terminal(status)
        }
    }

    fn actions_are_empty(actions: &NodeActions) -> bool {
        actions.events.is_empty() && actions.packets.is_empty() && actions.unroutable_packets == 0
    }

    /// Run one protocol timer tick and immediately offer every returned action
    /// to the ordinary coordinator.
    pub fn tick<R>(
        &mut self,
        now: MonotonicSeconds,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceTickResult
    where
        R: RngCore + CryptoRng,
    {
        self.tick_at(now, MonotonicInstant::from_secs(now.get()), admission, rng)
    }

    /// Precise Link-clock variant of [`Self::tick`].
    pub fn tick_at<R>(
        &mut self,
        now: MonotonicSeconds,
        link_now: MonotonicInstant,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceTickResult
    where
        R: RngCore + CryptoRng,
    {
        let mut report = self.node.tick_at(now, link_now, rng);
        let actions = mem::take(&mut report.actions);
        if Self::actions_are_empty(&actions) {
            return NodeInterfaceTickResult::Accepted(NodeInterfaceTickAccepted { report });
        }
        match self.try_offer_actions(actions, admission) {
            Ok(()) => NodeInterfaceTickResult::Accepted(NodeInterfaceTickAccepted { report }),
            Err(offer) => {
                NodeInterfaceTickResult::ActionOfferRejected(NodeInterfaceTickActionFailure {
                    report,
                    offer,
                })
            }
        }
    }

    /// Queue one signed local announce in the owned protocol node.
    pub fn queue_announce<R>(
        &mut self,
        app_data: Option<&[u8]>,
        emitted_at: AnnounceEmissionTime,
        rng: &mut R,
    ) -> Result<(), AnnounceAdmissionError>
    where
        R: RngCore + CryptoRng,
    {
        self.node.queue_announce(app_data, emitted_at, rng)
    }

    /// Flush every ready local announce and immediately offer the exact action
    /// envelope to the ordinary coordinator.
    pub fn flush_announces<R>(
        &mut self,
        now: MonotonicSeconds,
        admission: OrdinaryRouterAdmission,
        rng: &mut R,
    ) -> NodeInterfaceAnnounceFlushResult
    where
        R: RngCore,
    {
        let actions = self.node.flush_announces(now, rng);
        if Self::actions_are_empty(&actions) {
            return NodeInterfaceAnnounceFlushResult::Accepted;
        }
        match self.try_offer_actions(actions, admission) {
            Ok(()) => NodeInterfaceAnnounceFlushResult::Accepted,
            Err(failure) => NodeInterfaceAnnounceFlushResult::ActionOfferRejected(failure),
        }
    }

    /// Take events and unroutable counts from the last admitted ordinary
    /// envelope. Its packet vector is empty.
    pub fn take_non_packet_actions(&mut self) -> Option<NodeActions> {
        self.ordinary.take_non_packet_actions()
    }

    /// Take one recoverably rejected ordinary envelope.
    pub fn take_rejected_actions(&mut self) -> Option<OrdinaryRouterRejectedActions> {
        self.ordinary.take_rejected_actions()
    }

    /// Perform DATA owner maintenance once, then select at most one useful
    /// transition by bounded round-robin scan across every orchestration lane.
    ///
    /// Idle, pressured, or disabled lanes never stop the scan. In particular,
    /// a coordinator-local fault does not prevent an already-issued actor
    /// request from reaching that coordinator's forced-denial authorization
    /// path through its permit server.
    pub fn step(&mut self, now: MonotonicMillis) -> NodeInterfaceSupervisorPass {
        let maintenance = self.data.maintain_tx(&mut self.node, now);
        if maintenance.is_err() {
            self.owner_mismatch_fault = true;
            return NodeInterfaceSupervisorPass {
                maintenance,
                transition: NodeInterfaceSupervisorTransition::Fault(
                    NodeInterfaceSupervisorFault::DataOwnerMismatch,
                ),
            };
        }

        let lane_count = 3 + (2 * SLOTS);
        for offset in 0..lane_count {
            let lane = (self.lane_cursor + offset) % lane_count;
            let transition = match lane {
                0 => self.step_completion_lane(),
                1 => {
                    let fault_before = self.data.fault();
                    let step = self.data.step(&mut self.node, &mut self.router, now);
                    if matches!(step, DataRouterStep::OwnerMismatch) {
                        self.owner_mismatch_fault = true;
                        Some(NodeInterfaceSupervisorTransition::Fault(
                            NodeInterfaceSupervisorFault::DataOwnerMismatch,
                        ))
                    } else {
                        (step.progressed()
                            || (fault_before.is_none() && self.data.fault().is_some()))
                        .then_some(NodeInterfaceSupervisorTransition::Data(step))
                    }
                }
                2 => {
                    let fault_before = self.ordinary.fault();
                    let step = self.ordinary.step(&mut self.router, now);
                    (step.progressed()
                        || (fault_before.is_none() && self.ordinary.fault().is_some()))
                    .then_some(NodeInterfaceSupervisorTransition::Ordinary(step))
                }
                lane if lane < 3 + SLOTS => {
                    let actor = lane - 3;
                    let phase_before = self.data_permits[actor].phase();
                    let step = self.data_permits[actor].step(
                        &mut self.data,
                        &mut self.node,
                        now,
                        &mut self.policy,
                    );
                    (matches!(step, DataPermitServerStep::Advanced)
                        || (phase_before != DataPermitServerPhase::Disabled
                            && matches!(
                                step,
                                DataPermitServerStep::Disabled(_)
                                    | DataPermitServerStep::InternalInvariant
                            )))
                    .then_some(NodeInterfaceSupervisorTransition::DataPermit { actor, step })
                }
                lane => {
                    let actor = lane - 3 - SLOTS;
                    let phase_before = self.ordinary_permits[actor].phase();
                    let step = self.ordinary_permits[actor].step(
                        &mut self.ordinary,
                        now,
                        &mut self.policy,
                    );
                    (matches!(step, OrdinaryPermitServerStep::Advanced)
                        || (phase_before != OrdinaryPermitServerPhase::Disabled
                            && matches!(
                                step,
                                OrdinaryPermitServerStep::Disabled(_)
                                    | OrdinaryPermitServerStep::InternalInvariant
                            )))
                    .then_some(NodeInterfaceSupervisorTransition::OrdinaryPermit { actor, step })
                }
            };
            if let Some(transition) = transition {
                self.lane_cursor = (lane + 1) % lane_count;
                return NodeInterfaceSupervisorPass {
                    maintenance,
                    transition,
                };
            }
        }
        self.lane_cursor = (self.lane_cursor + 1) % lane_count;
        NodeInterfaceSupervisorPass {
            maintenance,
            transition: NodeInterfaceSupervisorTransition::Idle,
        }
    }

    /// Aggregate completion-correlation fault, if one exact owner was
    /// rejected and retained.
    pub fn fault(&self) -> Option<NodeInterfaceSupervisorFault> {
        self.owner_mismatch_fault
            .then_some(NodeInterfaceSupervisorFault::DataOwnerMismatch)
            .or_else(|| self.completion_fault.as_ref().map(|residue| residue.fault))
    }

    /// Copy-only binding of the exact completion retained behind the aggregate
    /// fault.
    pub fn completion_fault_residue(&self) -> Option<NodeInterfaceCompletionResidue> {
        self.completion_fault
            .as_ref()
            .map(CompletionFaultResidue::binding)
    }

    /// Read scalar node-owner occupancy.
    pub fn node_capacities(&self) -> CapacitySnapshot {
        self.node.capacities()
    }

    /// Immutable authoritative interface registry.
    pub const fn interface_registry(&self) -> &InterfaceRegistry<SLOTS> {
        self.router.registry()
    }

    /// DATA coordinator-local permanent fault.
    pub const fn data_fault(&self) -> Option<DataRouterFault> {
        self.data.fault()
    }

    /// Ordinary coordinator-local permanent fault.
    pub const fn ordinary_fault(&self) -> Option<OrdinaryRouterFault> {
        self.ordinary.fault()
    }

    /// Copy-only DATA parked-owner occupancy.
    pub fn data_parked_counts(&self) -> DataRouterParkedCounts {
        self.data.parked_counts()
    }

    /// Iterate terminal DATA attempts retained by the sole node owner until a
    /// durable application projection acknowledges them.
    pub fn terminal_attempts(&self) -> TerminalAttempts<'_> {
        self.node.terminal_attempts()
    }

    /// Release one exact terminal tombstone after durable application state
    /// has been committed.
    pub fn acknowledge_terminal(
        &mut self,
        handle: AttemptHandle,
    ) -> Result<TerminalAttempt, AcknowledgeError> {
        self.node.acknowledge_terminal(handle)
    }

    /// Iterate generation-safe recovered DATA-owner observations awaiting a
    /// durable transport audit and exact release acknowledgement.
    pub fn recovered_data_observations(&self) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.data.recovered_observations()
    }

    /// Iterate generation-safe fail-closed DATA quarantines without exposing
    /// their retained packet owners.
    pub fn quarantined_data_observations(
        &self,
    ) -> impl Iterator<Item = TxRecoveryObservation> + '_ {
        self.data.quarantine_observations()
    }

    /// Release one exact recovered DATA owner after its transport audit is
    /// durable. Quarantined owners deliberately have no corresponding release.
    pub fn acknowledge_recovered_data(
        &mut self,
        observation: TxRecoveryObservation,
    ) -> Result<(), DataRecoveryAckError> {
        self.data.acknowledge_recovered(observation)
    }

    /// Copy-only ordinary owner occupancy.
    pub fn ordinary_capacities(&self) -> OrdinaryActionCapacitySnapshot {
        self.ordinary.capacities()
    }

    /// Number of ordinary packet buffers currently parked for reuse.
    pub fn ordinary_parked_count(&self) -> usize {
        self.ordinary.parked_count()
    }

    /// Phase of one concrete actor's DATA permit service.
    pub fn data_permit_phase(&self, actor: usize) -> Option<DataPermitServerPhase> {
        self.data_permits.get(actor).map(DataPermitServer::phase)
    }

    /// Phase of one concrete actor's ordinary permit service.
    pub fn ordinary_permit_phase(&self, actor: usize) -> Option<OrdinaryPermitServerPhase> {
        self.ordinary_permits
            .get(actor)
            .map(OrdinaryPermitServer::phase)
    }

    /// Whether one exact ingress-produced action envelope is retained for a
    /// retryable busy admission.
    pub const fn ingress_actions_pending(&self) -> bool {
        self.pending_ingress_actions.is_some()
    }

    /// Copy-only non-retryable ingress-action status, if an exact envelope is
    /// retained for explicit recovery.
    pub fn terminal_ingress_action_fault(&self) -> Option<NodeInterfaceIngressActionFault> {
        self.terminal_ingress_actions
            .as_ref()
            .map(NodeInterfaceTerminalIngressActions::fault)
    }

    /// Take the exact ingress-produced action envelope retained behind a
    /// non-retryable admission fault.
    pub fn take_terminal_ingress_actions(&mut self) -> Option<NodeInterfaceTerminalIngressActions> {
        self.terminal_ingress_actions.take()
    }

    /// Copy-only status of an exact sealed ingress buffer retained for a
    /// retryable queue-full recycle.
    pub fn ingress_recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        self.pending_ingress_recycle
            .as_ref()
            .map(|pending| pending.status)
    }

    /// Copy-only terminal status of an exact sealed ingress buffer retained
    /// for explicit quarantine or recovery.
    pub fn terminal_ingress_recycle_fault(&self) -> Option<NodeInterfaceIngressRecycleFault> {
        self.terminal_ingress_recycle
            .as_ref()
            .map(|terminal| terminal.status)
    }

    /// Take the exact sealed ingress packet and its terminal recycle status.
    pub fn take_terminal_ingress_buffer(
        &mut self,
    ) -> Option<(NodeInterfaceIngressRecycleFault, SealedIngressPacket)> {
        self.terminal_ingress_recycle
            .take()
            .map(|terminal| (terminal.status, terminal.packet))
    }

    /// Shared authorization policy as an immutable status surface.
    pub const fn policy(&self) -> &P {
        &self.policy
    }

    /// Earliest locally known DATA or ordinary owner deadline.
    pub fn next_deadline(&self) -> Option<TxLeaseDeadline> {
        [
            self.node.next_tx_deadline(),
            self.data.next_deadline(),
            self.ordinary.next_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn step_completion_lane(&mut self) -> Option<NodeInterfaceSupervisorTransition> {
        if self.completion_fault.is_some() {
            return None;
        }
        match self.router.try_receive_completion() {
            Ok(Some(completion)) => {
                self.accept_completion(completion, NodeInterfaceCompletionOrigin::Current)
            }
            Ok(None) => None,
            Err(failure) => {
                let recovery = failure.into_node_recovery();
                let origin = NodeInterfaceCompletionOrigin::Recovery(recovery.reason());
                self.accept_completion(recovery.into_outbound(), origin)
            }
        }
    }

    fn accept_completion(
        &mut self,
        completion: OutboundCompletion,
        origin: NodeInterfaceCompletionOrigin,
    ) -> Option<NodeInterfaceSupervisorTransition> {
        match completion {
            OutboundCompletion::Data(completion) => {
                match self.data.try_accept_completion(completion) {
                    Ok(()) => Some(NodeInterfaceSupervisorTransition::CompletionAccepted {
                        family: NodeInterfaceCompletionFamily::Data,
                        origin,
                    }),
                    Err(failure) => {
                        let fault =
                            NodeInterfaceSupervisorFault::DataCompletionRejected(failure.reason());
                        self.completion_fault = Some(CompletionFaultResidue {
                            fault,
                            completion: OutboundCompletion::Data(failure.into_completion()),
                        });
                        Some(NodeInterfaceSupervisorTransition::Fault(fault))
                    }
                }
            }
            OutboundCompletion::Ordinary(completion) => {
                match self.ordinary.try_accept_completion(completion) {
                    Ok(()) => Some(NodeInterfaceSupervisorTransition::CompletionAccepted {
                        family: NodeInterfaceCompletionFamily::Ordinary,
                        origin,
                    }),
                    Err(failure) => {
                        let fault = NodeInterfaceSupervisorFault::OrdinaryCompletionRejected(
                            failure.reason(),
                        );
                        self.completion_fault = Some(CompletionFaultResidue {
                            fault,
                            completion: OutboundCompletion::Ordinary(failure.into_completion()),
                        });
                        Some(NodeInterfaceSupervisorTransition::Fault(fault))
                    }
                }
            }
        }
    }
}
