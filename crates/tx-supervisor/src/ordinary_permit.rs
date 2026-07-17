//! Persistent permit service for ticket-routed ordinary packets.

use core::{future::poll_fn, mem, task::Poll};

use embassy_sync::blocking_mutex::raw::RawMutex;
use reticulum_node_core::{
    MonotonicMillis, OrdinaryTxPermitReply, OrdinaryTxPermitRequest, TxAuthorizationPolicy,
};
use reticulum_tx_handoff::OrdinaryNodePermitHandoff;

use crate::{
    OrdinaryPermitAuthorizationError, OrdinaryPermitAuthorizationFailure, OrdinaryRouterCoordinator,
};

enum OrdinaryPermitServerState {
    Idle,
    Request(OrdinaryTxPermitRequest),
    Reply(OrdinaryTxPermitReply),
    Disabled(OrdinaryPermitAuthorizationFailure),
    Poisoned,
}

/// Persistent phase of the ordinary permit service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryPermitServerPhase {
    /// No scalar request is retained.
    Idle,
    /// One request is stored for authorization with a fresh clock sample.
    Request,
    /// One reply is stored until the concrete actor accepts it.
    Reply,
    /// Request validation or a private invariant failed permanently.
    Disabled,
}

/// Result of one synchronous ordinary permit-service transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "ordinary permit-service progress and faults must be handled"]
pub enum OrdinaryPermitServerStep {
    /// One ownership-preserving transition completed.
    Advanced,
    /// No permit request is currently queued.
    NeedRequest,
    /// The full reply channel returned the exact reply to persistent state.
    ReplyBackpressured,
    /// Coordinator request validation failed and the exact request is retained.
    Disabled(OrdinaryPermitAuthorizationError),
    /// Private service state was internally inconsistent.
    InternalInvariant,
}

/// Result of the short cancellation-safe ordinary permit-request wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "the caller must resume ordinary permit-service stepping"]
pub enum OrdinaryPermitServerWait {
    /// One exact request was stored in persistent service state.
    RequestStored,
    /// The actor reply queue has advisory capacity for the retained reply.
    ReplyCapacityReady,
    /// Synchronous authorization is immediately possible; call [`OrdinaryPermitServer::step`].
    NotWaiting,
    /// The service was already disabled by request validation.
    Disabled(OrdinaryPermitAuthorizationError),
    /// Private service state was internally inconsistent.
    InternalInvariant,
}

/// Kind of exact non-`Copy` control value retained after service disablement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryPermitServerFaultResidueKind {
    /// The request rejected by the coordinator remains retained unchanged.
    Request,
}

/// Node-side persistent server for ordinary permit requests and replies.
///
/// The concrete interface actor keeps its `OrdinaryPermitPendingTx` and router
/// completion ticket while only the opaque request crosses this service. The
/// server calls the coordinator's sole authorization facade at most once per
/// request and retains a non-`Copy` reply unchanged across channel pressure.
/// Store this machine outside any short executor future; only
/// [`Self::wait_for_progress`] is cancellation-safe to abandon.
#[must_use = "dropping the ordinary permit server abandons its ports and retained control value"]
pub struct OrdinaryPermitServer<M>
where
    M: RawMutex + 'static,
{
    handoff: OrdinaryNodePermitHandoff<M>,
    state: OrdinaryPermitServerState,
}

impl<M> OrdinaryPermitServer<M>
where
    M: RawMutex + 'static,
{
    /// Consume the sole node-side ordinary permit roles into an empty server.
    pub const fn new(handoff: OrdinaryNodePermitHandoff<M>) -> Self {
        Self {
            handoff,
            state: OrdinaryPermitServerState::Idle,
        }
    }

    /// Current scalar phase.
    pub const fn phase(&self) -> OrdinaryPermitServerPhase {
        match &self.state {
            OrdinaryPermitServerState::Idle => OrdinaryPermitServerPhase::Idle,
            OrdinaryPermitServerState::Request(_) => OrdinaryPermitServerPhase::Request,
            OrdinaryPermitServerState::Reply(_) => OrdinaryPermitServerPhase::Reply,
            OrdinaryPermitServerState::Disabled(_) | OrdinaryPermitServerState::Poisoned => {
                OrdinaryPermitServerPhase::Disabled
            }
        }
    }

    /// Retained request-validation failure, if disabled for that reason.
    pub fn fault(&self) -> Option<OrdinaryPermitAuthorizationError> {
        match &self.state {
            OrdinaryPermitServerState::Disabled(failure) => Some(failure.reason()),
            OrdinaryPermitServerState::Idle
            | OrdinaryPermitServerState::Request(_)
            | OrdinaryPermitServerState::Reply(_)
            | OrdinaryPermitServerState::Poisoned => None,
        }
    }

    /// Kind of exact non-`Copy` residue retained after a validation fault.
    pub fn fault_residue_kind(&self) -> Option<OrdinaryPermitServerFaultResidueKind> {
        match &self.state {
            OrdinaryPermitServerState::Disabled(_) => {
                Some(OrdinaryPermitServerFaultResidueKind::Request)
            }
            OrdinaryPermitServerState::Idle
            | OrdinaryPermitServerState::Request(_)
            | OrdinaryPermitServerState::Reply(_)
            | OrdinaryPermitServerState::Poisoned => None,
        }
    }

    /// Run exactly one non-awaiting service transition with a fresh clock.
    ///
    /// Product policy is called only while moving `Request -> Reply`; retrying
    /// a full reply channel cannot authorize or consume the reservation twice.
    /// If the coordinator is already faulted, its facade validates a still-live
    /// request and forces an unpermitted drain reply without invoking `policy`.
    pub fn step<P, const PACKET_BUFFERS: usize>(
        &mut self,
        coordinator: &mut OrdinaryRouterCoordinator<PACKET_BUFFERS>,
        now: MonotonicMillis,
        policy: &mut P,
    ) -> OrdinaryPermitServerStep
    where
        P: TxAuthorizationPolicy,
    {
        let state = mem::replace(&mut self.state, OrdinaryPermitServerState::Poisoned);
        match state {
            OrdinaryPermitServerState::Idle => match self.handoff.requests().try_receive() {
                Some(request) => {
                    self.state = OrdinaryPermitServerState::Request(request);
                    OrdinaryPermitServerStep::Advanced
                }
                None => {
                    self.state = OrdinaryPermitServerState::Idle;
                    OrdinaryPermitServerStep::NeedRequest
                }
            },
            OrdinaryPermitServerState::Request(request) => {
                match coordinator.authorize_tx(request, now, policy) {
                    Ok(reply) => {
                        self.state = OrdinaryPermitServerState::Reply(reply);
                        OrdinaryPermitServerStep::Advanced
                    }
                    Err(failure) => {
                        let reason = failure.reason();
                        self.state = OrdinaryPermitServerState::Disabled(failure);
                        OrdinaryPermitServerStep::Disabled(reason)
                    }
                }
            }
            OrdinaryPermitServerState::Reply(reply) => {
                match self.handoff.replies().try_send(reply) {
                    Ok(()) => {
                        self.state = OrdinaryPermitServerState::Idle;
                        OrdinaryPermitServerStep::Advanced
                    }
                    Err(full) => {
                        self.state = OrdinaryPermitServerState::Reply(full.into_inner());
                        OrdinaryPermitServerStep::ReplyBackpressured
                    }
                }
            }
            OrdinaryPermitServerState::Disabled(failure) => {
                let reason = failure.reason();
                self.state = OrdinaryPermitServerState::Disabled(failure);
                OrdinaryPermitServerStep::Disabled(reason)
            }
            OrdinaryPermitServerState::Poisoned => {
                self.state = OrdinaryPermitServerState::Poisoned;
                OrdinaryPermitServerStep::InternalInvariant
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
    ) -> Poll<OrdinaryPermitServerWait> {
        match self.phase() {
            OrdinaryPermitServerPhase::Idle => {
                match self.handoff.requests().poll_receive(context) {
                    Poll::Ready(request) => {
                        self.state = OrdinaryPermitServerState::Request(request);
                        Poll::Ready(OrdinaryPermitServerWait::RequestStored)
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
            OrdinaryPermitServerPhase::Request => Poll::Ready(OrdinaryPermitServerWait::NotWaiting),
            OrdinaryPermitServerPhase::Reply => self
                .handoff
                .replies()
                .poll_ready_to_send(context)
                .map(|()| OrdinaryPermitServerWait::ReplyCapacityReady),
            OrdinaryPermitServerPhase::Disabled => match &self.state {
                OrdinaryPermitServerState::Disabled(failure) => {
                    Poll::Ready(OrdinaryPermitServerWait::Disabled(failure.reason()))
                }
                OrdinaryPermitServerState::Poisoned => {
                    Poll::Ready(OrdinaryPermitServerWait::InternalInvariant)
                }
                OrdinaryPermitServerState::Idle
                | OrdinaryPermitServerState::Request(_)
                | OrdinaryPermitServerState::Reply(_) => {
                    Poll::Ready(OrdinaryPermitServerWait::InternalInvariant)
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
    pub async fn wait_for_progress(&mut self) -> OrdinaryPermitServerWait {
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
        InterfaceConfigId, InterfaceCost, InterfaceFabric, InterfaceProperties, InterfaceTxJob,
        LogicalMtu, OutboundCompletion, OutboundRouter,
    };
    use reticulum_node_core::{
        AnnounceEmissionTime, MonotonicSeconds, NodeActions, NodeConfig, NodeCore, NodeIdentity,
        NodeInstanceId, OrdinaryAuthorizationErrorKind, OrdinaryBufferPool, OrdinaryPacketBuffer,
        OrdinaryPermitResolution, OrdinaryTxPermitDenialReason, PacketInterfaceId,
        TxAuthorizationCandidate, TxCompletionCode, TxLeaseDeadline, TxPermitRequirements,
        TxPermitReservation, TxPermitResourceId, TxPolicyDecision, TxPolicyDenial,
    };
    use reticulum_tx_handoff::{OrdinaryDispatcherPermitHandoff, OrdinaryPermitHandoff};
    use std::boxed::Box;

    use super::*;
    use crate::{
        OrdinaryRouterAdmission, OrdinaryRouterCompletionProgress, OrdinaryRouterConfig,
        OrdinaryRouterStep,
    };

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

    fn node(tag: u8) -> TestNode {
        TestNode::new(
            NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid"),
            "reticulum",
            &["ordinary-permit"],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .expect("test node must construct")
    }

    fn coordinator<const BUFFERS: usize>(
        node: &mut TestNode,
    ) -> OrdinaryRouterCoordinator<BUFFERS> {
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
                TxCompletionCode::new(0x501),
                TxCompletionCode::new(0x509),
                TxCompletionCode::new(0x50a),
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

    fn configure_router<const SLOTS: usize, const DEPTH: usize>() -> (
        OutboundRouter<NoopRawMutex, SLOTS, DEPTH>,
        [reticulum_interface_router::InterfaceActorHandoff<NoopRawMutex, DEPTH>; SLOTS],
    ) {
        let fabric = Box::leak(Box::new(InterfaceFabric::new()));
        fabric.split()
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

    fn admission() -> OrdinaryRouterAdmission {
        OrdinaryRouterAdmission::new(TxLeaseDeadline::new(MonotonicMillis::new(100_000)))
    }

    fn route_one_announce<const BUFFERS: usize, const SLOTS: usize, const DEPTH: usize>(
        coordinator: &mut OrdinaryRouterCoordinator<BUFFERS>,
        router: &mut OutboundRouter<NoopRawMutex, SLOTS, DEPTH>,
        actions: NodeActions,
    ) {
        coordinator
            .try_offer_actions(actions, admission())
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
            OrdinaryRouterStep::Routed { interface, .. }
                if interface == PacketInterfaceId::new(2)
        ));
    }

    fn permit_pair() -> (
        OrdinaryPermitServer<NoopRawMutex>,
        OrdinaryDispatcherPermitHandoff<NoopRawMutex>,
    ) {
        let store = Box::leak(Box::new(OrdinaryPermitHandoff::new()));
        let (node, dispatcher) = store.split();
        (OrdinaryPermitServer::new(node), dispatcher)
    }

    fn requirements(tag: u8) -> TxPermitRequirements {
        TxPermitRequirements::try_new(TxPermitResourceId::new([tag; 16]), 1)
            .expect("test requirements must be nonzero")
    }

    #[test]
    fn real_router_ticket_grant_roundtrips_through_permit_only_handoff() {
        let mut node = node(1);
        let mut coordinator = coordinator::<1>(&mut node);
        let (mut router, mut actors) = configure_router::<1, 1>();
        router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                properties(2),
                true,
            )
            .expect("interface must register");
        let mut rng = CounterRng::default();
        let actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, actions);

        let InterfaceTxJob::Ordinary(job) = actors[0]
            .try_receive_job()
            .expect("actor must receive the routed owner")
        else {
            panic!("ordinary route changed owner family")
        };
        let (ticket, job) = job.into_parts();
        let slot = job.slot_id();
        let (pending, request) = job.begin_permit(requirements(1));
        let (mut server, mut dispatcher) = permit_pair();
        let mut policy = CountingAllow::default();
        dispatcher
            .requests()
            .try_send(request)
            .unwrap_or_else(|_| panic!("empty request channel must accept the request"));
        assert_eq!(
            server.step(&mut coordinator, MonotonicMillis::new(20), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        assert_eq!(server.phase(), OrdinaryPermitServerPhase::Request);
        assert_eq!(
            server.step(&mut coordinator, MonotonicMillis::new(20), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        assert_eq!(server.phase(), OrdinaryPermitServerPhase::Reply);
        assert_eq!(policy.calls, 1);
        assert_eq!(
            server.step(&mut coordinator, MonotonicMillis::new(20), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        assert_eq!(server.phase(), OrdinaryPermitServerPhase::Idle);
        assert_eq!(policy.calls, 1);

        let reply = dispatcher
            .replies()
            .try_receive()
            .expect("actor must receive the exact grant");
        let mut authorized = match pending
            .resolve(reply, MonotonicMillis::new(20))
            .unwrap_or_else(|_| panic!("grant must bind the exact pending owner"))
        {
            OrdinaryPermitResolution::Authorized(authorized) => authorized,
            OrdinaryPermitResolution::Expired(_) => panic!("fresh grant unexpectedly expired"),
            OrdinaryPermitResolution::Unpermitted(_) => panic!("allow policy denied"),
        };
        assert!(
            !authorized
                .frame(MonotonicMillis::new(20))
                .expect("authorized owner must expose its frame")
                .bytes()
                .is_empty()
        );
        let completion = ticket
            .complete(authorized.complete(TxCompletionCode::new(0x502)))
            .unwrap_or_else(|_| panic!("completion must retain its real router ticket"));
        actors[0]
            .try_send_completion(completion)
            .unwrap_or_else(|_| panic!("actor completion channel must accept the owner"));
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("current ticket completion must route")
        else {
            panic!("ordinary completion changed owner family")
        };
        coordinator
            .try_accept_completion(completion)
            .unwrap_or_else(|failure| panic!("completion failed: {:?}", failure.reason()));
        assert_eq!(
            coordinator.step(&mut router, MonotonicMillis::new(30)),
            OrdinaryRouterStep::Completion(OrdinaryRouterCompletionProgress::Returned { slot })
        );
        assert_eq!(coordinator.parked_count(), 1);
    }

    #[test]
    fn cancelling_pending_request_wait_leaves_exact_request_for_retry() {
        let mut node = node(2);
        let mut coordinator = coordinator::<1>(&mut node);
        let (mut router, mut actors) = configure_router::<1, 1>();
        router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                properties(2),
                true,
            )
            .expect("interface must register");
        let mut rng = CounterRng::default();
        let actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, actions);
        let InterfaceTxJob::Ordinary(job) = actors[0]
            .try_receive_job()
            .expect("actor must receive one owner")
        else {
            panic!("ordinary route changed owner family")
        };
        let (_ticket, job) = job.into_parts();
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
            Poll::Ready(OrdinaryPermitServerWait::RequestStored)
        ));
        assert_eq!(dispatcher.requests().len(), 0);
        let mut policy = CountingAllow::default();
        assert_eq!(
            server.step(&mut coordinator, MonotonicMillis::new(20), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        assert_eq!(
            server.step(&mut coordinator, MonotonicMillis::new(20), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        let reply = dispatcher
            .replies()
            .try_receive()
            .expect("retried exact request must receive a reply");
        assert!(matches!(
            pending
                .resolve(reply, MonotonicMillis::new(20))
                .unwrap_or_else(|_| panic!("reply must match the request retained across cancel")),
            OrdinaryPermitResolution::Authorized(_)
        ));
        assert_eq!(policy.calls, 1);
    }

    #[test]
    fn reply_pressure_retains_exact_reply_and_does_not_repeat_policy() {
        let mut node = node(3);
        let mut coordinator = coordinator::<2>(&mut node);
        let (mut router, mut actors) = configure_router::<1, 2>();
        router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                properties(2),
                true,
            )
            .expect("interface must register");
        let mut rng = CounterRng::default();
        let first_actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, first_actions);
        let second_actions = standalone_announce_actions(4, &mut rng);
        route_one_announce(&mut coordinator, &mut router, second_actions);

        let InterfaceTxJob::Ordinary(first) = actors[0]
            .try_receive_job()
            .expect("first owner must remain queued")
        else {
            panic!("first route changed owner family")
        };
        let InterfaceTxJob::Ordinary(second) = actors[0]
            .try_receive_job()
            .expect("second owner must remain queued")
        else {
            panic!("second route changed owner family")
        };
        let (_first_ticket, first) = first.into_parts();
        let (_second_ticket, second) = second.into_parts();
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
                server.step(&mut coordinator, MonotonicMillis::new(20), &mut policy),
                OrdinaryPermitServerStep::Advanced
            );
        }
        assert_eq!(policy.calls, 1);
        assert_eq!(dispatcher.replies().len(), 1);

        dispatcher
            .requests()
            .try_send(second_request)
            .unwrap_or_else(|_| panic!("second request must fit after the first was consumed"));
        assert_eq!(
            server.step(&mut coordinator, MonotonicMillis::new(21), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        assert_eq!(
            server.step(&mut coordinator, MonotonicMillis::new(21), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        assert_eq!(policy.calls, 2);
        assert_eq!(server.phase(), OrdinaryPermitServerPhase::Reply);
        for _ in 0..3 {
            assert_eq!(
                server.step(&mut coordinator, MonotonicMillis::new(21), &mut policy),
                OrdinaryPermitServerStep::ReplyBackpressured
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
            Poll::Ready(OrdinaryPermitServerWait::ReplyCapacityReady)
        ));
        assert_eq!(server.phase(), OrdinaryPermitServerPhase::Reply);
        assert!(dispatcher.replies().is_empty());
        assert_eq!(policy.calls, 2);
        assert_eq!(
            server.step(&mut coordinator, MonotonicMillis::new(21), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        let second_reply = dispatcher
            .replies()
            .try_receive()
            .expect("retained second reply must be sent after capacity returns");
        assert!(matches!(
            first_pending
                .resolve(first_reply, MonotonicMillis::new(22))
                .unwrap_or_else(|_| panic!("first reply must remain exact")),
            OrdinaryPermitResolution::Authorized(_)
        ));
        assert!(matches!(
            second_pending
                .resolve(second_reply, MonotonicMillis::new(22))
                .unwrap_or_else(|_| panic!("second reply must remain exact across retries")),
            OrdinaryPermitResolution::Authorized(_)
        ));
        assert_eq!(policy.calls, 2);
    }

    #[test]
    fn foreign_request_disables_with_retained_residue_without_policy_call() {
        let mut primary_node = node(5);
        let mut primary = coordinator::<1>(&mut primary_node);
        let mut foreign_node = node(6);
        let mut foreign = coordinator::<1>(&mut foreign_node);
        let (mut router, mut actors) = configure_router::<1, 1>();
        router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                properties(2),
                true,
            )
            .expect("interface must register");
        let mut rng = CounterRng::default();
        let actions = announce_actions(&mut foreign_node, 10, &mut rng);
        route_one_announce(&mut foreign, &mut router, actions);
        let InterfaceTxJob::Ordinary(job) = actors[0]
            .try_receive_job()
            .expect("foreign actor owner must route")
        else {
            panic!("foreign route changed owner family")
        };
        let (_ticket, job) = job.into_parts();
        let (_pending, request) = job.begin_permit(requirements(5));
        let (mut server, mut dispatcher) = permit_pair();
        let mut policy = CountingAllow::default();
        dispatcher
            .requests()
            .try_send(request)
            .unwrap_or_else(|_| panic!("foreign request must reach the server"));
        assert_eq!(
            server.step(&mut primary, MonotonicMillis::new(20), &mut policy),
            OrdinaryPermitServerStep::Advanced
        );
        let reason =
            OrdinaryPermitAuthorizationError::Owner(OrdinaryAuthorizationErrorKind::WrongOwner);
        assert_eq!(
            server.step(&mut primary, MonotonicMillis::new(20), &mut policy),
            OrdinaryPermitServerStep::Disabled(reason)
        );
        assert_eq!(server.phase(), OrdinaryPermitServerPhase::Disabled);
        assert_eq!(server.fault(), Some(reason));
        assert_eq!(
            server.fault_residue_kind(),
            Some(OrdinaryPermitServerFaultResidueKind::Request)
        );
        assert_eq!(policy.calls, 0);
        assert_eq!(
            server.step(&mut primary, MonotonicMillis::new(21), &mut policy),
            OrdinaryPermitServerStep::Disabled(reason)
        );
        assert_eq!(policy.calls, 0);
    }

    #[test]
    fn faulted_coordinator_forces_resource_unavailable_without_product_policy() {
        let mut node = node(7);
        let mut coordinator = coordinator::<2>(&mut node);
        let (mut router, mut actors) = configure_router::<2, 1>();
        router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                properties(2),
                true,
            )
            .expect("primary interface must register");
        let mut rng = CounterRng::default();
        let actions = announce_actions(&mut node, 10, &mut rng);
        route_one_announce(&mut coordinator, &mut router, actions);
        router
            .register(
                actors[1].queue_id(),
                PacketInterfaceId::new(64),
                properties(64),
                true,
            )
            .expect("out-of-profile identity remains registrable");
        coordinator
            .try_offer_actions(standalone_announce_actions(8, &mut rng), admission())
            .unwrap_or_else(|failure| panic!("second actions failed: {:?}", failure.reason()));
        assert!(matches!(
            coordinator.step(&mut router, MonotonicMillis::new(20)),
            OrdinaryRouterStep::Disabled(_)
        ));

        let InterfaceTxJob::Ordinary(job) = actors[0]
            .try_receive_job()
            .expect("already-issued actor owner must survive coordinator fault")
        else {
            panic!("active route changed owner family")
        };
        let (_ticket, job) = job.into_parts();
        let (pending, request) = job.begin_permit(requirements(7));
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
                    MonotonicMillis::new(25),
                    &mut product_policy,
                ),
                OrdinaryPermitServerStep::Advanced
            );
        }
        assert_eq!(product_policy.calls, 0);
        let reply = dispatcher
            .replies()
            .try_receive()
            .expect("faulted coordinator must return a forced denial");
        assert!(matches!(
            &reply,
            OrdinaryTxPermitReply::Denied(denial)
                if denial.reason()
                    == OrdinaryTxPermitDenialReason::Policy(
                        TxPolicyDenial::ResourceUnavailable
                    )
        ));
        let unpermitted = match pending
            .resolve(reply, MonotonicMillis::new(25))
            .unwrap_or_else(|_| panic!("forced denial must bind the exact active owner"))
        {
            OrdinaryPermitResolution::Unpermitted(owner) => owner,
            OrdinaryPermitResolution::Authorized(_) => {
                panic!("faulted coordinator exposed authorization")
            }
            OrdinaryPermitResolution::Expired(_) => panic!("forced denial unexpectedly expired"),
        };
        assert_eq!(
            unpermitted.denial(),
            Some(OrdinaryTxPermitDenialReason::Policy(
                TxPolicyDenial::ResourceUnavailable
            ))
        );
        assert_eq!(product_policy.calls, 0);
    }
}
