//! Persistent permit service for ticket-routed destination-DATA packets.

use core::{future::poll_fn, mem, task::Poll};

use embassy_sync::blocking_mutex::raw::RawMutex;
use reticulum_node_core::{
    MonotonicMillis, NodeCore, TxAuthorizationPolicy, TxPermitReply, TxPermitRequest,
};
use reticulum_tx_handoff::DataNodePermitHandoff;

use crate::{DataPermitAuthorizationError, DataPermitAuthorizationFailure, DataRouterCoordinator};

enum DataPermitServerState {
    Idle,
    Request(TxPermitRequest),
    Reply(TxPermitReply),
    Disabled(DataPermitAuthorizationFailure),
    Poisoned,
}

/// Persistent phase of one actor-specific DATA permit service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPermitServerPhase {
    /// No scalar request is retained.
    Idle,
    /// One exact request is stored for authorization with a fresh clock sample.
    Request,
    /// One exact reply is stored until the concrete actor accepts it.
    Reply,
    /// Request validation or a private invariant failed permanently.
    Disabled,
}

/// Result of one synchronous DATA permit-service transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "DATA permit-service progress and faults must be handled"]
pub enum DataPermitServerStep {
    /// One ownership-preserving transition completed.
    Advanced,
    /// No permit request is currently queued.
    NeedRequest,
    /// The full reply channel returned the exact reply to persistent state.
    ReplyBackpressured,
    /// Coordinator request validation failed and the exact request is retained.
    Disabled(DataPermitAuthorizationError),
    /// Private service state was internally inconsistent.
    InternalInvariant,
}

/// Result of the short cancellation-safe DATA permit-service wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "the caller must resume DATA permit-service stepping"]
pub enum DataPermitServerWait {
    /// One exact request was stored in persistent service state.
    RequestStored,
    /// The actor reply queue has advisory capacity for the retained reply.
    ReplyCapacityReady,
    /// Synchronous authorization is immediately possible; call [`DataPermitServer::step`].
    NotWaiting,
    /// The service was already disabled by request validation.
    Disabled(DataPermitAuthorizationError),
    /// Private service state was internally inconsistent.
    InternalInvariant,
}

/// Kind of exact non-`Copy` control value retained after service disablement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPermitServerFaultResidueKind {
    /// The request rejected by the DATA coordinator remains retained unchanged.
    Request,
}

/// Node-side persistent server for one concrete actor's DATA permit exchange.
///
/// The actor keeps its [`reticulum_node_core::PermitPendingTx`] and router
/// completion ticket while only the opaque request crosses this service. The
/// server calls the DATA coordinator's authorization facade at most once per
/// request and retains a non-`Copy` reply unchanged across channel pressure.
/// Store this machine outside any short executor future; only
/// [`Self::wait_for_progress`] is cancellation-safe to abandon.
#[must_use = "dropping the DATA permit server abandons its ports and retained control value"]
pub struct DataPermitServer<M>
where
    M: RawMutex + 'static,
{
    handoff: DataNodePermitHandoff<M>,
    state: DataPermitServerState,
}

impl<M> DataPermitServer<M>
where
    M: RawMutex + 'static,
{
    /// Consume the sole node-side DATA permit roles into an empty server.
    pub const fn new(handoff: DataNodePermitHandoff<M>) -> Self {
        Self {
            handoff,
            state: DataPermitServerState::Idle,
        }
    }

    /// Current scalar phase.
    pub const fn phase(&self) -> DataPermitServerPhase {
        match &self.state {
            DataPermitServerState::Idle => DataPermitServerPhase::Idle,
            DataPermitServerState::Request(_) => DataPermitServerPhase::Request,
            DataPermitServerState::Reply(_) => DataPermitServerPhase::Reply,
            DataPermitServerState::Disabled(_) | DataPermitServerState::Poisoned => {
                DataPermitServerPhase::Disabled
            }
        }
    }

    /// Retained request-validation failure, if disabled for that reason.
    pub fn fault(&self) -> Option<DataPermitAuthorizationError> {
        match &self.state {
            DataPermitServerState::Disabled(failure) => Some(failure.reason()),
            DataPermitServerState::Idle
            | DataPermitServerState::Request(_)
            | DataPermitServerState::Reply(_)
            | DataPermitServerState::Poisoned => None,
        }
    }

    /// Kind of exact non-`Copy` residue retained after a validation fault.
    pub fn fault_residue_kind(&self) -> Option<DataPermitServerFaultResidueKind> {
        match &self.state {
            DataPermitServerState::Disabled(_) => Some(DataPermitServerFaultResidueKind::Request),
            DataPermitServerState::Idle
            | DataPermitServerState::Request(_)
            | DataPermitServerState::Reply(_)
            | DataPermitServerState::Poisoned => None,
        }
    }

    /// Run exactly one non-awaiting service transition with a fresh clock.
    ///
    /// Product policy is called only while moving `Request -> Reply`; retrying
    /// a full reply channel cannot authorize or consume a reservation twice.
    /// If the coordinator is already faulted, its facade validates a still-live
    /// request and forces an unpermitted drain reply without invoking `policy`.
    #[allow(clippy::too_many_arguments)]
    pub fn step<
        P,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const PACKET_BUFFERS: usize,
    >(
        &mut self,
        coordinator: &mut DataRouterCoordinator<PACKET_BUFFERS>,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        now: MonotonicMillis,
        policy: &mut P,
    ) -> DataPermitServerStep
    where
        P: TxAuthorizationPolicy,
    {
        let state = mem::replace(&mut self.state, DataPermitServerState::Poisoned);
        match state {
            DataPermitServerState::Idle => match self.handoff.requests().try_receive() {
                Some(request) => {
                    self.state = DataPermitServerState::Request(request);
                    DataPermitServerStep::Advanced
                }
                None => {
                    self.state = DataPermitServerState::Idle;
                    DataPermitServerStep::NeedRequest
                }
            },
            DataPermitServerState::Request(request) => {
                match coordinator.authorize_tx(owner, request, now, policy) {
                    Ok(reply) => {
                        self.state = DataPermitServerState::Reply(reply);
                        DataPermitServerStep::Advanced
                    }
                    Err(failure) => {
                        let reason = failure.reason();
                        self.state = DataPermitServerState::Disabled(failure);
                        DataPermitServerStep::Disabled(reason)
                    }
                }
            }
            DataPermitServerState::Reply(reply) => match self.handoff.replies().try_send(reply) {
                Ok(()) => {
                    self.state = DataPermitServerState::Idle;
                    DataPermitServerStep::Advanced
                }
                Err(full) => {
                    self.state = DataPermitServerState::Reply(full.into_inner());
                    DataPermitServerStep::ReplyBackpressured
                }
            },
            DataPermitServerState::Disabled(failure) => {
                let reason = failure.reason();
                self.state = DataPermitServerState::Disabled(failure);
                DataPermitServerStep::Disabled(reason)
            }
            DataPermitServerState::Poisoned => {
                self.state = DataPermitServerState::Poisoned;
                DataPermitServerStep::InternalInvariant
            }
        }
    }

    /// Poll the phase-compatible channel wake without moving retained replies.
    ///
    /// In `Idle`, a ready request moves directly into persistent state before
    /// readiness is returned. In `Reply`, only scalar capacity is observed;
    /// the exact reply remains stored until a later [`Self::step`] retry.
    pub fn poll_progress(
        &mut self,
        context: &mut core::task::Context<'_>,
    ) -> Poll<DataPermitServerWait> {
        match self.phase() {
            DataPermitServerPhase::Idle => match self.handoff.requests().poll_receive(context) {
                Poll::Ready(request) => {
                    self.state = DataPermitServerState::Request(request);
                    Poll::Ready(DataPermitServerWait::RequestStored)
                }
                Poll::Pending => Poll::Pending,
            },
            DataPermitServerPhase::Request => Poll::Ready(DataPermitServerWait::NotWaiting),
            DataPermitServerPhase::Reply => self
                .handoff
                .replies()
                .poll_ready_to_send(context)
                .map(|()| DataPermitServerWait::ReplyCapacityReady),
            DataPermitServerPhase::Disabled => match &self.state {
                DataPermitServerState::Disabled(failure) => {
                    Poll::Ready(DataPermitServerWait::Disabled(failure.reason()))
                }
                DataPermitServerState::Poisoned => {
                    Poll::Ready(DataPermitServerWait::InternalInvariant)
                }
                DataPermitServerState::Idle
                | DataPermitServerState::Request(_)
                | DataPermitServerState::Reply(_) => {
                    Poll::Ready(DataPermitServerWait::InternalInvariant)
                }
            },
        }
    }

    /// Await phase-compatible request input or reply-channel capacity.
    ///
    /// A pending poll moves no request. Once a request is ready, it is assigned
    /// to persistent server state in that same poll before readiness is
    /// reported, so cancelling this short future cannot strand it in a future
    /// local.
    pub async fn wait_for_progress(&mut self) -> DataPermitServerWait {
        poll_fn(|context| self.poll_progress(context)).await
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };

    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rand_core::{CryptoRng, RngCore};
    use reticulum_interface_router::{
        InterfaceActorHandoff, InterfaceConfigId, InterfaceCost, InterfaceFabric,
        InterfaceProperties, InterfaceTxJob, LogicalMtu, OutboundCompletion, OutboundRouter,
    };
    use reticulum_node_core::{
        DestinationHash, MonotonicSeconds, NodeConfig, NodeIdentity, NodeInstanceId,
        PacketInterfaceId, PermitResolution, TxAuthorizationCandidate, TxAuthorizationErrorKind,
        TxAuthorizationPolicy, TxCompletion, TxCompletionCode, TxLeaseDeadline, TxPacketBuffer,
        TxPermitDenialReason, TxPermitRequirements, TxPermitReservation, TxPermitResourceId,
        TxPolicyDecision, TxPolicyDenial,
    };
    use reticulum_tx_handoff::{DataPermitHandoff, DispatcherPermitHandoff};
    use std::boxed::Box;

    use super::*;
    use crate::{
        DataPreparedHop, DataRouterCompletionProgress, DataRouterConfig, DataRouterFault,
        DataRouterPrepareRequest, DataRouterPrepareResult, DataRouterStep,
    };

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
                .expect("test reservation must cover the nonzero request"),
            )
        }
    }

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid")
    }

    fn sender<const BUFFERS: usize>(tag: u8) -> (TestNode<BUFFERS>, DestinationHash) {
        let receiver_tag = tag.wrapping_add(1);
        let receiver = TestNode::<0>::new(
            identity(receiver_tag),
            "reticulum",
            &["data-permit-receiver"],
            NodeInstanceId::new([receiver_tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .expect("test receiver must construct");
        let destination = receiver.destination_hash();
        let mut sender = TestNode::<BUFFERS>::new(
            identity(tag),
            "reticulum",
            &["data-permit-sender"],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .expect("test sender must construct");
        sender
            .register_peer(
                &identity(receiver_tag),
                "reticulum",
                &["data-permit-receiver"],
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
        DataRouterCoordinator::try_new(
            node,
            buffers,
            DataRouterConfig::new(
                TxCompletionCode::new(0x601),
                TxCompletionCode::new(0x602),
                TxCompletionCode::new(0x603),
            ),
        )
        .unwrap_or_else(|failure| panic!("DATA coordinator must build: {:?}", failure.reason()))
    }

    fn properties(config: u32) -> InterfaceProperties {
        InterfaceProperties::new(
            LogicalMtu::try_new(500).expect("test MTU must be nonzero"),
            InterfaceConfigId::new(config),
            None,
            InterfaceCost::new(0),
        )
    }

    fn configure_router<const SLOTS: usize, const DEPTH: usize>() -> (
        OutboundRouter<NoopRawMutex, SLOTS, DEPTH>,
        [InterfaceActorHandoff<NoopRawMutex, DEPTH>; SLOTS],
    ) {
        let fabric = Box::leak(Box::new(InterfaceFabric::new()));
        let (mut router, actors) = fabric.split();
        router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                properties(2),
                true,
            )
            .expect("primary interface must register");
        (router, actors)
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

    fn prepare_and_route<const BUFFERS: usize, const SLOTS: usize, const DEPTH: usize>(
        coordinator: &mut DataRouterCoordinator<BUFFERS>,
        node: &mut TestNode<BUFFERS>,
        router: &mut OutboundRouter<NoopRawMutex, SLOTS, DEPTH>,
        destination: DestinationHash,
        plaintext: &'static [u8],
        now: u64,
        rng: &mut CounterRng,
    ) -> DataPreparedHop {
        let hop = match coordinator.try_prepare_data(
            node,
            router,
            prepare_request(destination, plaintext),
            MonotonicMillis::new(now),
            rng,
        ) {
            DataRouterPrepareResult::Prepared(hop) => hop,
            other => panic!("DATA preparation failed: {other:?}"),
        };
        assert_eq!(
            coordinator.step(node, router, MonotonicMillis::new(now)),
            DataRouterStep::Routed(hop)
        );
        hop
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
            .expect("current completion ticket must route")
            .expect("one DATA completion must be queued")
        {
            OutboundCompletion::Data(completion) => completion,
            OutboundCompletion::Ordinary(_) => panic!("DATA completion changed family"),
        }
    }

    fn permit_pair() -> (
        DataPermitServer<NoopRawMutex>,
        DispatcherPermitHandoff<NoopRawMutex>,
    ) {
        let store = Box::leak(Box::new(DataPermitHandoff::new()));
        let (node, dispatcher) = store.split();
        (DataPermitServer::new(node), dispatcher)
    }

    fn requirements(tag: u8) -> TxPermitRequirements {
        TxPermitRequirements::try_new(TxPermitResourceId::new([tag; 16]), 1)
            .expect("test requirements must be nonzero")
    }

    #[test]
    fn real_router_ticket_grant_exposes_bytes_and_reconciles_exact_completion() {
        let (mut node, destination) = sender::<1>(1);
        let mut coordinator = coordinator(&mut node);
        let (mut router, mut actors) = configure_router::<1, 1>();
        let mut rng = CounterRng::default();
        let hop = prepare_and_route(
            &mut coordinator,
            &mut node,
            &mut router,
            destination,
            b"ticketed DATA permit",
            10,
            &mut rng,
        );
        let (ticket, job) = take_data_job(&mut actors[0]).into_parts();
        let (pending, request) = job.begin_permit(requirements(1));
        let (mut server, mut dispatcher) = permit_pair();
        let mut policy = CountingAllow::default();

        dispatcher
            .requests()
            .try_send(request)
            .unwrap_or_else(|_| panic!("empty request channel must accept the request"));
        for _ in 0..3 {
            assert_eq!(
                server.step(
                    &mut coordinator,
                    &mut node,
                    MonotonicMillis::new(20),
                    &mut policy,
                ),
                DataPermitServerStep::Advanced
            );
        }
        assert_eq!(server.phase(), DataPermitServerPhase::Idle);
        assert_eq!(policy.calls, 1);

        let reply = dispatcher
            .replies()
            .try_receive()
            .expect("actor must receive the exact grant");
        let mut authorized = match pending
            .resolve(reply, MonotonicMillis::new(20))
            .unwrap_or_else(|_| panic!("grant must bind the exact pending DATA owner"))
        {
            PermitResolution::Authorized(authorized) => authorized,
            PermitResolution::Expired(_) => panic!("fresh DATA grant unexpectedly expired"),
            PermitResolution::Unpermitted(_) => panic!("allow policy denied DATA"),
        };
        {
            let frame = authorized
                .frame(MonotonicMillis::new(20))
                .expect("authorized DATA owner must expose its frame once");
            assert!(!frame.bytes().is_empty());
            assert_eq!(frame.interface(), hop.interface());
        }
        let completion = ticket
            .complete(authorized.complete(TxCompletionCode::new(0x610)))
            .unwrap_or_else(|_| panic!("completion must retain its real router ticket"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("actor completion channel must accept the owner"));
        coordinator
            .try_accept_completion(receive_data_completion(&mut router))
            .unwrap_or_else(|failure| panic!("DATA completion failed: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(&mut node, &mut router, MonotonicMillis::new(30)),
            DataRouterStep::Completion(DataRouterCompletionProgress::Available(hop.slot_id()))
        );
        assert_eq!(coordinator.parked_counts().available(), 1);
    }

    #[test]
    fn cancelling_pending_request_wait_preserves_the_exact_request() {
        let (mut node, destination) = sender::<1>(2);
        let mut coordinator = coordinator(&mut node);
        let (mut router, mut actors) = configure_router::<1, 1>();
        let mut rng = CounterRng::default();
        prepare_and_route(
            &mut coordinator,
            &mut node,
            &mut router,
            destination,
            b"cancelled DATA request wait",
            10,
            &mut rng,
        );
        let (_ticket, job) = take_data_job(&mut actors[0]).into_parts();
        let (pending, request) = job.begin_permit(requirements(2));
        let (mut server, mut dispatcher) = permit_pair();
        let mut context = Context::from_waker(Waker::noop());
        {
            let mut wait = pin!(server.wait_for_progress());
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        }

        dispatcher
            .requests()
            .try_send(request)
            .unwrap_or_else(|_| panic!("cancelled wait must leave request capacity available"));
        assert_eq!(dispatcher.requests().len(), 1);
        assert!(matches!(
            server.poll_progress(&mut context),
            Poll::Ready(DataPermitServerWait::RequestStored)
        ));
        assert_eq!(dispatcher.requests().len(), 0);
        let mut policy = CountingAllow::default();
        for _ in 0..2 {
            assert_eq!(
                server.step(
                    &mut coordinator,
                    &mut node,
                    MonotonicMillis::new(20),
                    &mut policy,
                ),
                DataPermitServerStep::Advanced
            );
        }
        let reply = dispatcher
            .replies()
            .try_receive()
            .expect("retried exact request must receive a reply");
        assert!(matches!(
            pending
                .resolve(reply, MonotonicMillis::new(20))
                .unwrap_or_else(|_| panic!("reply must match the request retained across cancel")),
            PermitResolution::Authorized(_)
        ));
        assert_eq!(policy.calls, 1);
    }

    #[test]
    fn reply_pressure_retains_exact_reply_and_calls_policy_once_per_request() {
        let (mut node, destination) = sender::<2>(3);
        let mut coordinator = coordinator(&mut node);
        let (mut router, mut actors) = configure_router::<1, 2>();
        let mut rng = CounterRng::default();
        prepare_and_route(
            &mut coordinator,
            &mut node,
            &mut router,
            destination,
            b"first DATA reply",
            10,
            &mut rng,
        );
        prepare_and_route(
            &mut coordinator,
            &mut node,
            &mut router,
            destination,
            b"second DATA reply",
            11,
            &mut rng,
        );
        let (_first_ticket, first) = take_data_job(&mut actors[0]).into_parts();
        let (_second_ticket, second) = take_data_job(&mut actors[0]).into_parts();
        let (first_pending, first_request) = first.begin_permit(requirements(3));
        let (second_pending, second_request) = second.begin_permit(requirements(4));
        let (mut server, mut dispatcher) = permit_pair();
        let mut policy = CountingAllow::default();

        dispatcher
            .requests()
            .try_send(first_request)
            .unwrap_or_else(|_| panic!("first request must fit"));
        for _ in 0..3 {
            assert_eq!(
                server.step(
                    &mut coordinator,
                    &mut node,
                    MonotonicMillis::new(20),
                    &mut policy,
                ),
                DataPermitServerStep::Advanced
            );
        }
        assert_eq!(policy.calls, 1);
        assert_eq!(dispatcher.replies().len(), 1);

        dispatcher
            .requests()
            .try_send(second_request)
            .unwrap_or_else(|_| panic!("second request must fit after the first was consumed"));
        for _ in 0..2 {
            assert_eq!(
                server.step(
                    &mut coordinator,
                    &mut node,
                    MonotonicMillis::new(21),
                    &mut policy,
                ),
                DataPermitServerStep::Advanced
            );
        }
        assert_eq!(policy.calls, 2);
        assert_eq!(server.phase(), DataPermitServerPhase::Reply);
        for _ in 0..3 {
            assert_eq!(
                server.step(
                    &mut coordinator,
                    &mut node,
                    MonotonicMillis::new(21),
                    &mut policy,
                ),
                DataPermitServerStep::ReplyBackpressured
            );
            assert_eq!(policy.calls, 2);
        }

        {
            let mut context = Context::from_waker(Waker::noop());
            let mut wait = pin!(server.wait_for_progress());
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
        }
        let first_reply = dispatcher
            .replies()
            .try_receive()
            .expect("first reply must remain ahead of the retained reply");
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            server.poll_progress(&mut context),
            Poll::Ready(DataPermitServerWait::ReplyCapacityReady)
        ));
        assert_eq!(server.phase(), DataPermitServerPhase::Reply);
        assert!(dispatcher.replies().is_empty());
        assert_eq!(policy.calls, 2);
        assert_eq!(
            server.step(
                &mut coordinator,
                &mut node,
                MonotonicMillis::new(21),
                &mut policy,
            ),
            DataPermitServerStep::Advanced
        );
        let second_reply = dispatcher
            .replies()
            .try_receive()
            .expect("retained second reply must be sent after capacity returns");
        assert!(matches!(
            first_pending
                .resolve(first_reply, MonotonicMillis::new(22))
                .unwrap_or_else(|_| panic!("first reply must remain exact")),
            PermitResolution::Authorized(_)
        ));
        assert!(matches!(
            second_pending
                .resolve(second_reply, MonotonicMillis::new(22))
                .unwrap_or_else(|_| panic!("second reply must remain exact across retries")),
            PermitResolution::Authorized(_)
        ));
        assert_eq!(policy.calls, 2);
    }

    #[test]
    fn foreign_request_disables_with_retained_residue_without_policy_call() {
        let (mut primary_node, _primary_destination) = sender::<1>(5);
        let mut primary = coordinator(&mut primary_node);
        let (mut foreign_node, foreign_destination) = sender::<1>(7);
        let mut foreign = coordinator(&mut foreign_node);
        let (mut router, mut actors) = configure_router::<1, 1>();
        let mut rng = CounterRng::default();
        prepare_and_route(
            &mut foreign,
            &mut foreign_node,
            &mut router,
            foreign_destination,
            b"foreign DATA permit request",
            10,
            &mut rng,
        );
        let (_ticket, job) = take_data_job(&mut actors[0]).into_parts();
        let (_pending, request) = job.begin_permit(requirements(5));
        let (mut server, mut dispatcher) = permit_pair();
        let mut policy = CountingAllow::default();
        dispatcher
            .requests()
            .try_send(request)
            .unwrap_or_else(|_| panic!("foreign request must reach the server"));
        assert_eq!(
            server.step(
                &mut primary,
                &mut primary_node,
                MonotonicMillis::new(20),
                &mut policy,
            ),
            DataPermitServerStep::Advanced
        );
        let reason = DataPermitAuthorizationError::Owner(TxAuthorizationErrorKind::WrongOwner);
        assert_eq!(
            server.step(
                &mut primary,
                &mut primary_node,
                MonotonicMillis::new(20),
                &mut policy,
            ),
            DataPermitServerStep::Disabled(reason)
        );
        assert_eq!(server.phase(), DataPermitServerPhase::Disabled);
        assert_eq!(server.fault(), Some(reason));
        assert_eq!(
            server.fault_residue_kind(),
            Some(DataPermitServerFaultResidueKind::Request)
        );
        assert_eq!(policy.calls, 0);
        assert_eq!(
            server.step(
                &mut primary,
                &mut primary_node,
                MonotonicMillis::new(21),
                &mut policy,
            ),
            DataPermitServerStep::Disabled(reason)
        );
        assert_eq!(policy.calls, 0);
    }

    #[test]
    fn faulted_coordinator_forces_resource_unavailable_without_product_policy() {
        let (mut node, destination) = sender::<2>(9);
        let mut coordinator = coordinator(&mut node);
        let (mut router, mut actors) = configure_router::<2, 1>();
        let mut rng = CounterRng::default();
        let hop = prepare_and_route(
            &mut coordinator,
            &mut node,
            &mut router,
            destination,
            b"DATA actor owner before fault",
            10,
            &mut rng,
        );
        let (ticket, job) = take_data_job(&mut actors[0]).into_parts();
        let (pending, request) = job.begin_permit(requirements(9));

        router
            .register(
                actors[1].queue_id(),
                PacketInterfaceId::new(80),
                properties(80),
                true,
            )
            .expect("out-of-profile interface remains registrable");
        assert!(matches!(
            coordinator.try_prepare_data(
                &mut node,
                &router,
                prepare_request(destination, b"DATA registry fault trigger"),
                MonotonicMillis::new(12),
                &mut rng,
            ),
            DataRouterPrepareResult::Disabled(DataRouterFault::RegistryProfile(_))
        ));

        let (mut server, mut dispatcher) = permit_pair();
        let mut product_policy = CountingAllow::default();
        dispatcher
            .requests()
            .try_send(request)
            .unwrap_or_else(|_| panic!("fault-drain request must fit"));
        for _ in 0..3 {
            assert_eq!(
                server.step(
                    &mut coordinator,
                    &mut node,
                    MonotonicMillis::new(13),
                    &mut product_policy,
                ),
                DataPermitServerStep::Advanced
            );
        }
        assert_eq!(product_policy.calls, 0);
        let reply = dispatcher
            .replies()
            .try_receive()
            .expect("faulted coordinator must return a forced denial");
        assert!(matches!(
            &reply,
            TxPermitReply::Denied(denial)
                if denial.reason()
                    == TxPermitDenialReason::Policy(TxPolicyDenial::ResourceUnavailable)
        ));
        let unpermitted = match pending
            .resolve(reply, MonotonicMillis::new(13))
            .unwrap_or_else(|_| panic!("forced denial must bind the exact active DATA owner"))
        {
            PermitResolution::Unpermitted(owner) => owner,
            PermitResolution::Authorized(_) => {
                panic!("faulted coordinator exposed DATA authorization")
            }
            PermitResolution::Expired(_) => panic!("forced DATA denial unexpectedly expired"),
        };
        assert_eq!(
            unpermitted.denial(),
            Some(TxPermitDenialReason::Policy(
                TxPolicyDenial::ResourceUnavailable
            ))
        );
        let completion = ticket
            .complete(unpermitted.complete(TxCompletionCode::new(0x611)))
            .unwrap_or_else(|_| panic!("fault-drain completion must retain the real ticket"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("fault-drain completion must fit"));
        coordinator
            .try_accept_completion(receive_data_completion(&mut router))
            .unwrap_or_else(|failure| panic!("fault-drain return failed: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(&mut node, &mut router, MonotonicMillis::new(14)),
            DataRouterStep::Completion(DataRouterCompletionProgress::Available(hop.slot_id()))
        );
        assert_eq!(product_policy.calls, 0);
    }
}
