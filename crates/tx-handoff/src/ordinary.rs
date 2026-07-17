//! Permit-only handoff for ordinary Rete action packets.
//!
//! Ordinary packet owners and ticket-bound completions move through
//! `reticulum-interface-router`. This module carries only the scalar
//! request/reply exchange used while one concrete interface actor retains an
//! exact ordinary owner.

use core::task::{Context, Poll};

use embassy_sync::{blocking_mutex::raw::RawMutex, channel::Channel};
use reticulum_node_core::{OrdinaryTxPermitReply, OrdinaryTxPermitRequest};

use super::{ChannelFull, try_enqueue};

/// Sole dispatcher-side producer of ordinary scalar permit requests.
#[must_use = "dropping this sender abandons the ordinary request producer role"]
pub struct OrdinaryPermitRequestSender<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, OrdinaryTxPermitRequest, 1>,
}

impl<M> OrdinaryPermitRequestSender<M>
where
    M: RawMutex + 'static,
{
    /// Try to enqueue one opaque ordinary request without awaiting.
    pub fn try_send(
        &mut self,
        request: OrdinaryTxPermitRequest,
    ) -> Result<(), ChannelFull<OrdinaryTxPermitRequest>> {
        try_enqueue(self.channel, request)
    }

    /// Poll until one request can be retried through [`Self::try_send`].
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.channel.poll_ready_to_send(context)
    }

    /// Fixed scalar-control channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued ordinary requests.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no ordinary requests are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side consumer of ordinary scalar permit requests.
#[must_use = "dropping this receiver abandons the ordinary request consumer role"]
pub struct OrdinaryPermitRequestReceiver<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, OrdinaryTxPermitRequest, 1>,
}

impl<M> OrdinaryPermitRequestReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Await the oldest opaque ordinary permit request.
    pub async fn receive(&mut self) -> OrdinaryTxPermitRequest {
        self.channel.receive().await
    }

    /// Poll for the oldest request without constructing a receive future.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<OrdinaryTxPermitRequest> {
        self.channel.poll_receive(context)
    }

    /// Receive the oldest ordinary request immediately, if queued.
    pub fn try_receive(&mut self) -> Option<OrdinaryTxPermitRequest> {
        self.channel.try_receive().ok()
    }

    /// Fixed scalar-control channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued ordinary requests.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no ordinary requests are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side producer of ordinary scalar permit replies.
#[must_use = "dropping this sender abandons the ordinary reply producer role"]
pub struct OrdinaryPermitReplySender<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, OrdinaryTxPermitReply, 1>,
}

impl<M> OrdinaryPermitReplySender<M>
where
    M: RawMutex + 'static,
{
    /// Try to enqueue one opaque ordinary reply without awaiting.
    pub fn try_send(
        &mut self,
        reply: OrdinaryTxPermitReply,
    ) -> Result<(), ChannelFull<OrdinaryTxPermitReply>> {
        try_enqueue(self.channel, reply)
    }

    /// Poll until one reply can be retried through [`Self::try_send`].
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.channel.poll_ready_to_send(context)
    }

    /// Fixed scalar-control channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued ordinary replies.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no ordinary replies are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole dispatcher-side consumer of ordinary scalar permit replies.
#[must_use = "dropping this receiver abandons the ordinary reply consumer role"]
pub struct OrdinaryPermitReplyReceiver<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, OrdinaryTxPermitReply, 1>,
}

impl<M> OrdinaryPermitReplyReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Await the oldest opaque ordinary permit reply.
    pub async fn receive(&mut self) -> OrdinaryTxPermitReply {
        self.channel.receive().await
    }

    /// Poll for the oldest reply without constructing a receive future.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<OrdinaryTxPermitReply> {
        self.channel.poll_receive(context)
    }

    /// Receive the oldest ordinary reply immediately, if queued.
    pub fn try_receive(&mut self) -> Option<OrdinaryTxPermitReply> {
        self.channel.try_receive().ok()
    }

    /// Fixed scalar-control channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued ordinary replies.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no ordinary replies are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Ordinary node port group for the scalar permit exchange.
///
/// Ordinary packet owners and completions use `reticulum-interface-router`;
/// this group contains only the node side of one actor's scalar permit pair.
#[must_use = "dropping ordinary node permit roles abandons their channel capabilities"]
pub struct OrdinaryNodePermitHandoff<M>
where
    M: RawMutex + 'static,
{
    requests: OrdinaryPermitRequestReceiver<M>,
    replies: OrdinaryPermitReplySender<M>,
}

impl<M> OrdinaryNodePermitHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Borrow the sole node-side ordinary permit-request consumer.
    pub fn requests(&mut self) -> &mut OrdinaryPermitRequestReceiver<M> {
        &mut self.requests
    }

    /// Borrow the sole node-side ordinary permit-reply producer.
    pub fn replies(&mut self) -> &mut OrdinaryPermitReplySender<M> {
        &mut self.replies
    }
}

/// Ordinary dispatcher port group for the scalar permit exchange.
///
/// This group contains only the interface actor side of one scalar permit
/// pair. The interface router carries that actor's ordinary packet owners.
#[must_use = "dropping ordinary dispatcher permit roles abandons their channel capabilities"]
pub struct OrdinaryDispatcherPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    requests: OrdinaryPermitRequestSender<M>,
    replies: OrdinaryPermitReplyReceiver<M>,
}

impl<M> OrdinaryDispatcherPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Borrow the sole dispatcher-side ordinary permit-request producer.
    pub fn requests(&mut self) -> &mut OrdinaryPermitRequestSender<M> {
        &mut self.requests
    }

    /// Borrow the sole dispatcher-side ordinary permit-reply consumer.
    pub fn replies(&mut self) -> &mut OrdinaryPermitReplyReceiver<M> {
        &mut self.replies
    }
}

/// One inseparable ordinary permit role pair from a single static store.
///
/// A permanent aggregate can consume this proof instead of constructing node
/// and actor control ports from unrelated stores.
#[must_use = "dropping paired ordinary permit roles abandons both channel capabilities"]
pub struct OrdinaryPairedPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    node: OrdinaryNodePermitHandoff<M>,
    dispatcher: OrdinaryDispatcherPermitHandoff<M>,
}

impl<M> OrdinaryPairedPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Consume common-origin proof into the node and dispatcher permit roles.
    pub fn into_parts(
        self,
    ) -> (
        OrdinaryNodePermitHandoff<M>,
        OrdinaryDispatcherPermitHandoff<M>,
    ) {
        (self.node, self.dispatcher)
    }
}

/// Permit-only channel storage for the heterogeneous ordinary-action path.
///
/// Jobs and ticket-bound completions move directly through
/// `reticulum-interface-router`, so permanent composition needs only this
/// scalar request/reply pair. Both channels have depth one because one
/// concrete actor serializes one active packet owner through its permit
/// exchange.
pub struct OrdinaryPermitHandoff<M>
where
    M: RawMutex,
{
    requests: Channel<M, OrdinaryTxPermitRequest, 1>,
    replies: Channel<M, OrdinaryTxPermitReply, 1>,
}

impl<M> OrdinaryPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Construct an empty ordinary permit handoff store.
    pub const fn new() -> Self {
        Self {
            requests: Channel::new(),
            replies: Channel::new(),
        }
    }

    /// Split the store into its only live node and dispatcher permit roles.
    pub fn split(
        &'static mut self,
    ) -> (
        OrdinaryNodePermitHandoff<M>,
        OrdinaryDispatcherPermitHandoff<M>,
    ) {
        self.split_paired().into_parts()
    }

    /// Split into an unforgeable common-origin permit role pair.
    pub fn split_paired(&'static mut self) -> OrdinaryPairedPermitHandoff<M> {
        OrdinaryPairedPermitHandoff {
            node: OrdinaryNodePermitHandoff {
                requests: OrdinaryPermitRequestReceiver {
                    channel: &self.requests,
                },
                replies: OrdinaryPermitReplySender {
                    channel: &self.replies,
                },
            },
            dispatcher: OrdinaryDispatcherPermitHandoff {
                requests: OrdinaryPermitRequestSender {
                    channel: &self.requests,
                },
                replies: OrdinaryPermitReplyReceiver {
                    channel: &self.replies,
                },
            },
        }
    }
}

impl<M> Default for OrdinaryPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
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

    use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
    use rand_core::{CryptoRng, RngCore};
    use reticulum_node_core::{
        AnnounceEmissionTime, InterfaceSet, MonotonicMillis, MonotonicSeconds, NodeConfig,
        NodeCore, NodeIdentity, NodeInstanceId, OrdinaryActionAdmissionRequest,
        OrdinaryActionOwner, OrdinaryCompletionDisposition, OrdinaryPacketBuffer,
        OrdinaryPermitResolution, TxAuthorizationCandidate, TxAuthorizationPolicy,
        TxCompletionCode, TxLeaseDeadline, TxPermitRequirements, TxPermitReservation,
        TxPermitResourceId, TxPolicyDecision,
    };
    use static_cell::{ConstStaticCell, StaticCell};

    use super::OrdinaryPermitHandoff;

    type TestNode = NodeCore<4, 4, 8, 2, 0>;

    const TEST_PERMIT_RESOURCE: TxPermitResourceId = TxPermitResourceId::new([0x4f; 16]);

    fn test_permit_requirements() -> TxPermitRequirements {
        TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1)
            .expect("test permit units must be nonzero")
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

    struct Allow;

    impl TxAuthorizationPolicy for Allow {
        fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            TxPolicyDecision::Authorize(
                TxPermitReservation::try_new(
                    candidate.requirements.resource(),
                    candidate.requirements.required_units(),
                )
                .expect("test policy must mirror valid requirements"),
            )
        }
    }

    fn node(tag: u8) -> TestNode {
        TestNode::new(
            NodeIdentity::from_private_key(&[tag; 64]).unwrap(),
            "reticulum",
            &["ordinary-handoff"],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .unwrap()
    }

    fn actions(
        node: &mut TestNode,
        count: usize,
        rng: &mut CounterRng,
    ) -> reticulum_node_core::NodeActions {
        for index in 0..count {
            node.queue_announce(
                Some(&[u8::try_from(index).unwrap()]),
                AnnounceEmissionTime::new(1).unwrap(),
                rng,
            )
            .unwrap();
        }
        node.flush_announces(MonotonicSeconds::new(1), rng)
    }

    const fn admission(deadline_ms: u64) -> OrdinaryActionAdmissionRequest {
        OrdinaryActionAdmissionRequest {
            owner_now: MonotonicMillis::new(1_000),
            deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline_ms)),
            enabled_interfaces: InterfaceSet::from_bits(1 << 1),
        }
    }

    fn reconcile<const N: usize>(
        owner: &mut OrdinaryActionOwner<N>,
        completion: reticulum_node_core::OrdinaryTxCompletion<'static>,
    ) -> &'static mut OrdinaryPacketBuffer {
        match owner.complete_tx(completion, MonotonicMillis::new(2_000)) {
            Ok(OrdinaryCompletionDisposition::Returned(returned)) => returned.into_buffer(),
            Ok(OrdinaryCompletionDisposition::Next(_)) => panic!("single route fanned out"),
            Ok(OrdinaryCompletionDisposition::Quarantined(_)) => {
                panic!("valid ordinary completion quarantined")
            }
            Err(_) => panic!("valid ordinary completion was rejected"),
        }
    }

    static PRODUCTION_PERMIT_HANDOFF: ConstStaticCell<
        OrdinaryPermitHandoff<CriticalSectionRawMutex>,
    > = ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn production_mutex_static_store_splits_common_origin_permit_roles() {
        let permit_pair = PRODUCTION_PERMIT_HANDOFF.take().split_paired();
        let (mut node, mut dispatcher) = permit_pair.into_parts();
        assert_eq!(node.requests().capacity(), 1);
        assert_eq!(node.replies().capacity(), 1);
        assert_eq!(dispatcher.requests().capacity(), 1);
        assert_eq!(dispatcher.replies().capacity(), 1);
    }

    static PERMIT_BUFFER_A: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static PERMIT_BUFFER_B: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static PERMIT_BUFFER_C: ConstStaticCell<OrdinaryPacketBuffer> =
        ConstStaticCell::new(OrdinaryPacketBuffer::new());
    static PERMIT_BUFFER_REFS: StaticCell<[&'static mut OrdinaryPacketBuffer; 3]> =
        StaticCell::new();
    static PERMIT_HANDOFF: ConstStaticCell<OrdinaryPermitHandoff<NoopRawMutex>> =
        ConstStaticCell::new(OrdinaryPermitHandoff::new());

    #[test]
    fn permit_channels_roundtrip_crossed_replies_and_prove_exact_pressure_and_cancellation() {
        let mut node_core = node(40);
        let mut owner = node_core.take_ordinary_action_owner::<3>().unwrap();
        let first_buffer = PERMIT_BUFFER_A.take();
        let second_buffer = PERMIT_BUFFER_B.take();
        let third_buffer = PERMIT_BUFFER_C.take();
        owner.register_packet_buffer(first_buffer).unwrap();
        owner.register_packet_buffer(second_buffer).unwrap();
        owner.register_packet_buffer(third_buffer).unwrap();
        let buffers = PERMIT_BUFFER_REFS.init([first_buffer, second_buffer, third_buffer]);
        let mut rng = CounterRng::default();
        let mut batch = owner
            .admit(
                actions(&mut node_core, 3, &mut rng),
                buffers,
                admission(10_000),
            )
            .unwrap();
        let first = batch.take_next_packet().unwrap();
        let second = batch.take_next_packet().unwrap();
        let third = batch.take_next_packet().unwrap();
        let (first_pending, first_request) = first.begin_permit(test_permit_requirements());
        let (second_pending, second_request) = second.begin_permit(test_permit_requirements());
        let (third_pending, third_request) = third.begin_permit(test_permit_requirements());
        let (mut node, mut dispatcher) = PERMIT_HANDOFF.take().split();
        let mut context = Context::from_waker(Waker::noop());
        assert!(node.requests().is_empty());
        assert!(dispatcher.requests().is_empty());
        assert!(node.replies().is_empty());
        assert!(dispatcher.replies().is_empty());
        assert!(matches!(
            dispatcher.requests().poll_ready_to_send(&mut context),
            Poll::Ready(())
        ));
        assert!(matches!(
            node.replies().poll_ready_to_send(&mut context),
            Poll::Ready(())
        ));
        assert!(matches!(
            node.requests().poll_receive(&mut context),
            Poll::Pending
        ));
        assert!(matches!(
            dispatcher.replies().poll_receive(&mut context),
            Poll::Pending
        ));

        {
            let mut receive = pin!(node.requests().receive());
            assert!(matches!(receive.as_mut().poll(&mut context), Poll::Pending));
            assert!(dispatcher.requests().try_send(first_request).is_ok());
        }
        let second_request = match dispatcher.requests().try_send(second_request) {
            Err(full) => full.into_inner(),
            Ok(()) => panic!("depth-one request channel accepted a second request"),
        };
        let second_completion = second_pending
            .cancel_before_send(second_request, TxCompletionCode::new(20))
            .unwrap_or_else(|_| panic!("exact full-channel request did not prove cancellation"));
        let first_request = node
            .requests()
            .try_receive()
            .expect("cancelled request receive removed the first request");
        let first_reply = owner
            .authorize_tx(first_request, MonotonicMillis::new(1_100), &mut Allow)
            .unwrap_or_else(|failure| panic!("first request failed: {:?}", failure.reason()));

        assert!(dispatcher.requests().try_send(third_request).is_ok());
        let third_request = match node.requests().poll_receive(&mut context) {
            Poll::Ready(request) => request,
            Poll::Pending => panic!("queued third request did not poll ready"),
        };
        let third_reply = owner
            .authorize_tx(third_request, MonotonicMillis::new(1_101), &mut Allow)
            .unwrap_or_else(|failure| panic!("third request failed: {:?}", failure.reason()));

        {
            let mut receive = pin!(dispatcher.replies().receive());
            assert!(matches!(receive.as_mut().poll(&mut context), Poll::Pending));
            assert!(node.replies().try_send(first_reply).is_ok());
        }
        let third_reply = match node.replies().try_send(third_reply) {
            Err(full) => full.into_inner(),
            Ok(()) => panic!("depth-one reply channel accepted a second reply"),
        };
        let first_reply = dispatcher
            .replies()
            .try_receive()
            .expect("cancelled reply receive removed the first reply");
        assert!(node.replies().try_send(third_reply).is_ok());
        let third_reply = dispatcher
            .replies()
            .try_receive()
            .expect("third reply was lost");

        let (first_pending, third_reply) =
            match first_pending.resolve(third_reply, MonotonicMillis::new(1_200)) {
                Err(mismatch) => mismatch.into_parts(),
                Ok(_) => panic!("crossed third reply matched first owner"),
            };
        let (third_pending, first_reply) =
            match third_pending.resolve(first_reply, MonotonicMillis::new(1_200)) {
                Err(mismatch) => mismatch.into_parts(),
                Ok(_) => panic!("crossed first reply matched third owner"),
            };
        let first = match first_pending.resolve(first_reply, MonotonicMillis::new(1_200)) {
            Ok(OrdinaryPermitResolution::Authorized(authorized)) => authorized,
            _ => panic!("matching first reply did not authorize"),
        };
        let third = match third_pending.resolve(third_reply, MonotonicMillis::new(1_200)) {
            Ok(OrdinaryPermitResolution::Authorized(authorized)) => authorized,
            _ => panic!("matching third reply did not authorize"),
        };

        let completions = [
            first.complete(TxCompletionCode::new(21)),
            second_completion,
            third.complete(TxCompletionCode::new(22)),
        ];
        for completion in completions {
            let _ = reconcile(&mut owner, completion);
        }
        assert_eq!(owner.capacities().active, 0);
    }
}
