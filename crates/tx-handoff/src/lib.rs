//! Bounded ownership-preserving channels between the Reticulum node owner and
//! its single packet-interface dispatcher.
//!
//! Packet jobs and returns use two channels whose capacity equals the external
//! packet-buffer pool. Permit requests and replies use separate depth-one
//! scalar channels sized for the future single dispatcher's required
//! one-at-a-time permit loop. The actor must enforce that rule; this crate
//! returns an over-capacity value unchanged for fault handling. Control-plane
//! pressure cannot consume an owning-channel slot. Sending is deliberately
//! synchronous: every full path returns the unchanged non-`Copy` value to its
//! caller. Receiving may await because cancelling an Embassy receive future
//! does not remove a queued item.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::task::{Context, Poll};

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};
use reticulum_node_core::{
    RoutedTxJob, TxCompletion, TxPacketBuffer, TxPermitReply, TxPermitRequest,
};

/// A bounded channel was full and returned its unchanged message.
#[must_use = "a full handoff retains the unchanged message owner"]
pub struct ChannelFull<T> {
    message: T,
}

impl<T> ChannelFull<T> {
    fn new(message: T) -> Self {
        Self { message }
    }

    /// Recover the unchanged message that was not enqueued.
    pub fn into_inner(self) -> T {
        self.message
    }
}

/// Unique ownership returning to the node actor.
///
/// `Available` seeds the registered fixed pool before actors start. Ordinary
/// dispatcher work returns a node-core `Completion`; only node-core may turn
/// that completion back into an available buffer or quarantine.
#[must_use = "a returned TX owner must be consumed by the node actor"]
pub enum TxOwnerReturn {
    /// A registered buffer available for a new node-core preparation.
    Available(&'static mut TxPacketBuffer),
    /// Completion of one exact routed hop.
    Completion(TxCompletion<'static>),
}

impl From<&'static mut TxPacketBuffer> for TxOwnerReturn {
    fn from(buffer: &'static mut TxPacketBuffer) -> Self {
        Self::Available(buffer)
    }
}

impl From<TxCompletion<'static>> for TxOwnerReturn {
    fn from(completion: TxCompletion<'static>) -> Self {
        Self::Completion(completion)
    }
}

fn try_enqueue<M, T, const N: usize>(
    channel: &Channel<M, T, N>,
    message: T,
) -> Result<(), ChannelFull<T>>
where
    M: RawMutex,
{
    channel
        .try_send(message)
        .map_err(|TrySendError::Full(message)| ChannelFull::new(message))
}

/// Sole node-side producer of routed packet jobs.
#[must_use = "dropping the job sender permanently abandons the node producer role"]
pub struct JobSender<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, RoutedTxJob<'static>, POOL_SIZE>,
}

impl<M, const POOL_SIZE: usize> JobSender<M, POOL_SIZE>
where
    M: RawMutex + 'static,
{
    /// Try to enqueue one unique job without awaiting.
    ///
    /// A full queue returns the unchanged job. A fresh job for which no route
    /// has ever been authorized can be passed to `NodeCore::rollback_queued`.
    /// A `TxCompletionDisposition::Next` job after prior authorization must
    /// instead be retained and retried; rollback deliberately rejects it.
    pub fn try_send(
        &mut self,
        job: RoutedTxJob<'static>,
    ) -> Result<(), ChannelFull<RoutedTxJob<'static>>> {
        try_enqueue(self.channel, job)
    }

    /// Configured packet-pool and channel capacity.
    pub const fn capacity(&self) -> usize {
        POOL_SIZE
    }

    /// Current number of queued jobs.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no jobs are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole dispatcher-side consumer of routed packet jobs.
#[must_use = "dropping the job receiver permanently abandons the dispatcher consumer role"]
pub struct JobReceiver<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, RoutedTxJob<'static>, POOL_SIZE>,
}

impl<M, const POOL_SIZE: usize> JobReceiver<M, POOL_SIZE>
where
    M: RawMutex + 'static,
{
    /// Await the oldest routed job.
    pub async fn receive(&mut self) -> RoutedTxJob<'static> {
        self.channel.receive().await
    }

    /// Poll for the oldest routed job without constructing a receive future.
    ///
    /// `Pending` leaves ownership in the channel. `Ready` transfers the exact
    /// job to the caller, which must store or consume it before returning from
    /// the surrounding poll.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<RoutedTxJob<'static>> {
        self.channel.poll_receive(context)
    }

    /// Receive the oldest routed job immediately, if one is queued.
    pub fn try_receive(&mut self) -> Option<RoutedTxJob<'static>> {
        self.channel.try_receive().ok()
    }

    /// Configured packet-pool and channel capacity.
    pub const fn capacity(&self) -> usize {
        POOL_SIZE
    }

    /// Current number of queued jobs.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no jobs are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole dispatcher-side producer of owning returns.
#[must_use = "dropping the return sender permanently abandons the dispatcher producer role"]
pub struct OwnerReturnSender<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, TxOwnerReturn, POOL_SIZE>,
}

impl<M, const POOL_SIZE: usize> OwnerReturnSender<M, POOL_SIZE>
where
    M: RawMutex + 'static,
{
    /// Try to return one unique buffer owner without awaiting.
    pub fn try_send(&mut self, returned: TxOwnerReturn) -> Result<(), ChannelFull<TxOwnerReturn>> {
        try_enqueue(self.channel, returned)
    }

    /// Configured packet-pool and channel capacity.
    pub const fn capacity(&self) -> usize {
        POOL_SIZE
    }

    /// Current number of queued owner returns.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no owner returns are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side consumer of owning returns.
#[must_use = "dropping the return receiver permanently abandons the node consumer role"]
pub struct OwnerReturnReceiver<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, TxOwnerReturn, POOL_SIZE>,
}

impl<M, const POOL_SIZE: usize> OwnerReturnReceiver<M, POOL_SIZE>
where
    M: RawMutex + 'static,
{
    /// Await the oldest unique owner return.
    pub async fn receive(&mut self) -> TxOwnerReturn {
        self.channel.receive().await
    }

    /// Receive the oldest unique owner return immediately, if one is queued.
    pub fn try_receive(&mut self) -> Option<TxOwnerReturn> {
        self.channel.try_receive().ok()
    }

    /// Configured packet-pool and channel capacity.
    pub const fn capacity(&self) -> usize {
        POOL_SIZE
    }

    /// Current number of queued owner returns.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no owner returns are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole dispatcher-side producer of scalar permit requests.
#[must_use = "dropping the request sender permanently abandons the dispatcher producer role"]
pub struct PermitRequestSender<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, TxPermitRequest, 1>,
}

impl<M> PermitRequestSender<M>
where
    M: RawMutex + 'static,
{
    /// Try to enqueue one opaque request without awaiting.
    pub fn try_send(
        &mut self,
        request: TxPermitRequest,
    ) -> Result<(), ChannelFull<TxPermitRequest>> {
        try_enqueue(self.channel, request)
    }

    /// Fixed scalar-control channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued requests.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no permit requests are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side consumer of scalar permit requests.
#[must_use = "dropping the request receiver permanently abandons the node consumer role"]
pub struct PermitRequestReceiver<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, TxPermitRequest, 1>,
}

impl<M> PermitRequestReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Await the oldest opaque permit request.
    pub async fn receive(&mut self) -> TxPermitRequest {
        self.channel.receive().await
    }

    /// Receive the oldest request immediately, if one is queued.
    pub fn try_receive(&mut self) -> Option<TxPermitRequest> {
        self.channel.try_receive().ok()
    }

    /// Fixed scalar-control channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued requests.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no permit requests are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side producer of scalar permit replies.
#[must_use = "dropping the reply sender permanently abandons the node producer role"]
pub struct PermitReplySender<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, TxPermitReply, 1>,
}

impl<M> PermitReplySender<M>
where
    M: RawMutex + 'static,
{
    /// Try to enqueue one opaque reply without awaiting.
    pub fn try_send(&mut self, reply: TxPermitReply) -> Result<(), ChannelFull<TxPermitReply>> {
        try_enqueue(self.channel, reply)
    }

    /// Fixed scalar-control channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued replies.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no permit replies are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole dispatcher-side consumer of scalar permit replies.
#[must_use = "dropping the reply receiver permanently abandons the dispatcher consumer role"]
pub struct PermitReplyReceiver<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, TxPermitReply, 1>,
}

impl<M> PermitReplyReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Await the oldest opaque permit reply.
    pub async fn receive(&mut self) -> TxPermitReply {
        self.channel.receive().await
    }

    /// Poll for the oldest opaque permit reply without constructing a receive
    /// future.
    ///
    /// `Pending` leaves the reply in the channel. `Ready` transfers the exact
    /// reply to the caller, which must store or consume it before returning
    /// from the surrounding poll.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<TxPermitReply> {
        self.channel.poll_receive(context)
    }

    /// Receive the oldest reply immediately, if one is queued.
    pub fn try_receive(&mut self) -> Option<TxPermitReply> {
        self.channel.try_receive().ok()
    }

    /// Fixed scalar-control channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued replies.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no permit replies are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Node-actor roles for the four one-way channels.
#[must_use = "dropping node handoff roles permanently abandons their channel capabilities"]
pub struct NodeHandoff<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    /// Producer for unique routed jobs.
    pub jobs: JobSender<M, POOL_SIZE>,
    /// Consumer for available buffers and owning completions.
    pub returns: OwnerReturnReceiver<M, POOL_SIZE>,
    /// Consumer for scalar permit requests.
    pub permit_requests: PermitRequestReceiver<M>,
    /// Producer for scalar permit replies.
    pub permit_replies: PermitReplySender<M>,
}

/// Single-dispatcher roles for the four one-way channels.
#[must_use = "dropping dispatcher roles permanently abandons their channel capabilities"]
pub struct DispatcherHandoff<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    /// Consumer for unique routed jobs.
    pub jobs: JobReceiver<M, POOL_SIZE>,
    /// Producer for available buffers and owning completions.
    pub returns: OwnerReturnSender<M, POOL_SIZE>,
    /// Producer for scalar permit requests.
    pub permit_requests: PermitRequestSender<M>,
    /// Consumer for scalar permit replies.
    pub permit_replies: PermitReplyReceiver<M>,
}

/// Fixed-capacity channel storage for one external packet-buffer pool.
///
/// The two owner channels use `POOL_SIZE`; both control channels have depth
/// one. `split` consumes the unique `'static mut` reference normally obtained
/// from a `StaticCell`, so safe code cannot create a second set of roles.
pub struct TxHandoff<M, const POOL_SIZE: usize>
where
    M: RawMutex,
{
    jobs: Channel<M, RoutedTxJob<'static>, POOL_SIZE>,
    returns: Channel<M, TxOwnerReturn, POOL_SIZE>,
    permit_requests: Channel<M, TxPermitRequest, 1>,
    permit_replies: Channel<M, TxPermitReply, 1>,
}

impl<M, const POOL_SIZE: usize> TxHandoff<M, POOL_SIZE>
where
    M: RawMutex + 'static,
{
    /// Packet-buffer pool size and capacity of each owner channel.
    pub const POOL_SIZE: usize = POOL_SIZE;

    /// Construct empty handoff storage.
    ///
    /// The pool must be nonempty and fit node-core's 16-bit slot namespace.
    pub const fn new() -> Self {
        const {
            assert!(POOL_SIZE > 0, "TX handoff pool must be nonempty");
            assert!(
                POOL_SIZE <= (u16::MAX as usize) + 1,
                "TX handoff pool must fit node-core's packet-slot namespace"
            );
        }
        Self {
            jobs: Channel::new(),
            returns: Channel::new(),
            permit_requests: Channel::new(),
            permit_replies: Channel::new(),
        }
    }

    /// Split the storage into the only live node and dispatcher channel roles.
    pub fn split(
        &'static mut self,
    ) -> (NodeHandoff<M, POOL_SIZE>, DispatcherHandoff<M, POOL_SIZE>) {
        (
            NodeHandoff {
                jobs: JobSender {
                    channel: &self.jobs,
                },
                returns: OwnerReturnReceiver {
                    channel: &self.returns,
                },
                permit_requests: PermitRequestReceiver {
                    channel: &self.permit_requests,
                },
                permit_replies: PermitReplySender {
                    channel: &self.permit_replies,
                },
            },
            DispatcherHandoff {
                jobs: JobReceiver {
                    channel: &self.jobs,
                },
                returns: OwnerReturnSender {
                    channel: &self.returns,
                },
                permit_requests: PermitRequestSender {
                    channel: &self.permit_requests,
                },
                permit_replies: PermitReplyReceiver {
                    channel: &self.permit_replies,
                },
            },
        )
    }
}

impl<M, const POOL_SIZE: usize> Default for TxHandoff<M, POOL_SIZE>
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
        ptr,
        task::{Context, Poll, Waker},
    };

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
    use rand_core::{CryptoRng, RngCore};
    use reticulum_node_core::{
        DestinationHash, InterfaceSet, MonotonicMillis, MonotonicSeconds, NodeConfig, NodeCore,
        NodeIdentity, NodeInstanceId, PermitResolution, PrepareDataRequest, RoutedTxJob,
        TxAuthorizationCandidate, TxAuthorizationPolicy, TxCompletionCode, TxCompletionDisposition,
        TxLeaseDeadline, TxPacketBuffer, TxPermitReply, TxPolicyDecision,
    };
    use static_cell::ConstStaticCell;

    use super::{TxHandoff, TxOwnerReturn};

    type TestNode<const BUFFERS: usize> = NodeCore<4, 2, 8, 2, BUFFERS>;

    static PRODUCTION_MUTEX_HANDOFF: ConstStaticCell<TxHandoff<CriticalSectionRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn production_mutex_supports_static_storage_and_exclusive_split() {
        let (node, dispatcher) = PRODUCTION_MUTEX_HANDOFF.take().split();
        assert_eq!(node.jobs.capacity(), 1);
        assert_eq!(node.returns.capacity(), 1);
        assert_eq!(node.permit_requests.capacity(), 1);
        assert_eq!(node.permit_replies.capacity(), 1);
        assert_eq!(dispatcher.jobs.capacity(), 1);
        assert_eq!(dispatcher.returns.capacity(), 1);
        assert_eq!(dispatcher.permit_requests.capacity(), 1);
        assert_eq!(dispatcher.permit_replies.capacity(), 1);
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
        fn authorize(&mut self, _candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            TxPolicyDecision::Authorize
        }
    }

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

    fn prepare<'a, const BUFFERS: usize>(
        sender: &mut TestNode<BUFFERS>,
        buffer: &'a mut TxPacketBuffer,
        destination: DestinationHash,
        plaintext: &[u8],
        rns_now: u64,
        rng: &mut CounterRng,
    ) -> RoutedTxJob<'a> {
        match sender.prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination,
                plaintext,
                rns_now: MonotonicSeconds::new(rns_now),
                owner_now: MonotonicMillis::new(1_000),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(100_000)),
                enabled_interfaces: InterfaceSet::from_bits(1 << 1),
            },
            rng,
        ) {
            Ok(job) => job,
            Err(failure) => panic!("test preparation failed: {:?}", failure.reason()),
        }
    }

    fn authorize<const BUFFERS: usize>(
        owner: &mut TestNode<BUFFERS>,
        request: reticulum_node_core::TxPermitRequest,
    ) -> TxPermitReply {
        match owner.authorize_tx(request, MonotonicMillis::new(2_000), &mut Allow) {
            Ok(reply) => reply,
            Err(_) => panic!("test authorization failed"),
        }
    }

    fn available<const BUFFERS: usize>(
        owner: &mut TestNode<BUFFERS>,
        returned: TxOwnerReturn,
    ) -> &'static mut TxPacketBuffer {
        let completion = match returned {
            TxOwnerReturn::Completion(completion) => completion,
            TxOwnerReturn::Available(_) => panic!("expected an owning completion"),
        };
        match owner.complete_tx(completion, MonotonicMillis::new(3_000)) {
            Ok(TxCompletionDisposition::Available(buffer)) => buffer,
            Ok(TxCompletionDisposition::Recovered { buffer, .. }) => buffer,
            Ok(TxCompletionDisposition::Next(_)) => panic!("single-interface route advanced"),
            Ok(TxCompletionDisposition::Quarantined(_)) => panic!("valid completion quarantined"),
            Err(_) => panic!("valid completion was rejected"),
        }
    }

    static AVAILABLE_A: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static AVAILABLE_B: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static AVAILABLE_C: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static AVAILABLE_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 2>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn static_available_owners_are_fifo_and_full_returns_the_exact_reference() {
        let mut owner = node::<3>(1, "available-owner");
        let first = AVAILABLE_A.take();
        let second = AVAILABLE_B.take();
        let third = AVAILABLE_C.take();
        owner
            .register_packet_buffer(first)
            .expect("first buffer must register");
        owner
            .register_packet_buffer(second)
            .expect("second buffer must register");
        owner
            .register_packet_buffer(third)
            .expect("third buffer must register");
        let first_pointer = ptr::from_ref(&*first);
        let second_pointer = ptr::from_ref(&*second);
        let third_pointer = ptr::from_ref(&*third);

        let storage = AVAILABLE_HANDOFF.take();
        let (mut node, mut dispatcher) = storage.split();
        assert_eq!(TxHandoff::<NoopRawMutex, 2>::POOL_SIZE, 2);
        assert_eq!(node.jobs.capacity(), 2);
        assert_eq!(node.returns.capacity(), 2);
        assert_eq!(node.permit_requests.capacity(), 1);
        assert_eq!(node.permit_replies.capacity(), 1);
        assert_eq!(dispatcher.jobs.capacity(), 2);
        assert_eq!(dispatcher.returns.capacity(), 2);
        assert_eq!(dispatcher.permit_requests.capacity(), 1);
        assert_eq!(dispatcher.permit_replies.capacity(), 1);

        {
            let mut receive = pin!(node.returns.receive());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(receive.as_mut().poll(&mut context), Poll::Pending));
            assert!(
                dispatcher
                    .returns
                    .try_send(TxOwnerReturn::Available(first))
                    .is_ok()
            );
        }
        let first = match node.returns.try_receive() {
            Some(TxOwnerReturn::Available(buffer)) => buffer,
            Some(TxOwnerReturn::Completion(_)) => panic!("available owner changed variant"),
            None => panic!("owner was lost when a woken receive future was cancelled"),
        };
        assert_eq!(ptr::from_ref(&*first), first_pointer);

        assert!(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Available(first))
                .is_ok()
        );
        assert!(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Available(second))
                .is_ok()
        );
        let third = match dispatcher.returns.try_send(TxOwnerReturn::Available(third)) {
            Err(full) => match full.into_inner() {
                TxOwnerReturn::Available(buffer) => buffer,
                TxOwnerReturn::Completion(_) => panic!("available owner changed variant"),
            },
            Ok(()) => panic!("over-capacity return was accepted"),
        };
        assert_eq!(ptr::from_ref(&*third), third_pointer);

        let first = match node.returns.try_receive() {
            Some(TxOwnerReturn::Available(buffer)) => buffer,
            Some(TxOwnerReturn::Completion(_)) => panic!("available owner changed variant"),
            None => panic!("first owner was lost"),
        };
        let second = match node.returns.try_receive() {
            Some(TxOwnerReturn::Available(buffer)) => buffer,
            Some(TxOwnerReturn::Completion(_)) => panic!("available owner changed variant"),
            None => panic!("second owner was lost"),
        };
        assert_eq!(ptr::from_ref(&*first), first_pointer);
        assert_eq!(ptr::from_ref(&*second), second_pointer);
        assert_ne!(ptr::from_ref(&*first), ptr::from_ref(&*second));
        assert!(node.returns.try_receive().is_none());

        assert!(
            dispatcher
                .returns
                .try_send(TxOwnerReturn::Available(third))
                .is_ok()
        );
        let third = match block_on(node.returns.receive()) {
            TxOwnerReturn::Available(buffer) => buffer,
            TxOwnerReturn::Completion(_) => panic!("available owner changed variant"),
        };
        assert_eq!(ptr::from_ref(&*third), third_pointer);
    }

    static JOB_A: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
    static JOB_B: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
    static JOB_C: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
    static JOB_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 2>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn routed_jobs_are_fifo_and_pressure_returns_the_exact_job() {
        let mut owner = node::<3>(10, "job-owner");
        let receiver = node::<0>(11, "job-receiver");
        register_peer(&mut owner, 11, "job-receiver");
        let first_buffer = JOB_A.take();
        let second_buffer = JOB_B.take();
        let third_buffer = JOB_C.take();
        owner.register_packet_buffer(first_buffer).unwrap();
        owner.register_packet_buffer(second_buffer).unwrap();
        owner.register_packet_buffer(third_buffer).unwrap();
        let first_pointer = ptr::from_ref(&*first_buffer);
        let second_pointer = ptr::from_ref(&*second_buffer);
        let third_pointer = ptr::from_ref(&*third_buffer);
        let mut rng = CounterRng::default();
        let first = prepare(
            &mut owner,
            first_buffer,
            receiver.destination_hash(),
            b"first",
            1,
            &mut rng,
        );
        let second = prepare(
            &mut owner,
            second_buffer,
            receiver.destination_hash(),
            b"second",
            2,
            &mut rng,
        );
        let third = prepare(
            &mut owner,
            third_buffer,
            receiver.destination_hash(),
            b"third",
            3,
            &mut rng,
        );
        let first_token = first.attempt();
        let second_token = second.attempt();
        let third_token = third.attempt();

        // Three node-core owners intentionally overdrive a two-buffer handoff
        // to exercise its fail-closed pressure path.
        let storage = JOB_HANDOFF.take();
        let (mut node, mut dispatcher) = storage.split();
        assert!(node.jobs.try_send(first).is_ok());
        assert!(node.jobs.try_send(second).is_ok());
        let third = match node.jobs.try_send(third) {
            Err(full) => full.into_inner(),
            Ok(()) => panic!("over-capacity job was accepted"),
        };
        assert_eq!(third.attempt(), third_token);
        assert_eq!(node.jobs.len(), 2);

        let first = dispatcher.jobs.try_receive().expect("first job was lost");
        assert_eq!(first.attempt(), first_token);
        assert!(node.jobs.try_send(third).is_ok());
        let second = dispatcher.jobs.try_receive().expect("second job was lost");
        let third = block_on(dispatcher.jobs.receive());
        assert_eq!(second.attempt(), second_token);
        assert_eq!(third.attempt(), third_token);
        assert!(dispatcher.jobs.try_receive().is_none());

        let first = match owner.rollback_queued(first, MonotonicMillis::new(4_000)) {
            Ok(disposition) => disposition,
            Err(_) => panic!("first rollback failed"),
        };
        let second = match owner.rollback_queued(second, MonotonicMillis::new(4_000)) {
            Ok(disposition) => disposition,
            Err(_) => panic!("second rollback failed"),
        };
        let third = match owner.rollback_queued(third, MonotonicMillis::new(4_000)) {
            Ok(disposition) => disposition,
            Err(_) => panic!("third rollback failed"),
        };
        let first = match first {
            TxCompletionDisposition::Available(buffer) => buffer,
            _ => panic!("first rollback did not return its buffer"),
        };
        let second = match second {
            TxCompletionDisposition::Available(buffer) => buffer,
            _ => panic!("second rollback did not return its buffer"),
        };
        let third = match third {
            TxCompletionDisposition::Available(buffer) => buffer,
            _ => panic!("third rollback did not return its buffer"),
        };
        assert_eq!(ptr::from_ref(&*first), first_pointer);
        assert_eq!(ptr::from_ref(&*second), second_pointer);
        assert_eq!(ptr::from_ref(&*third), third_pointer);
    }

    static PERMIT_A: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
    static PERMIT_B: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
    static PERMIT_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 2>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn permit_control_is_separate_and_mismatch_retains_both_sides() {
        let mut owner = node::<2>(20, "permit-owner");
        let receiver = node::<0>(21, "permit-receiver");
        register_peer(&mut owner, 21, "permit-receiver");
        let first_buffer = PERMIT_A.take();
        let second_buffer = PERMIT_B.take();
        owner.register_packet_buffer(first_buffer).unwrap();
        owner.register_packet_buffer(second_buffer).unwrap();
        let first_pointer = ptr::from_ref(&*first_buffer);
        let second_pointer = ptr::from_ref(&*second_buffer);
        let mut rng = CounterRng::default();
        let first = prepare(
            &mut owner,
            first_buffer,
            receiver.destination_hash(),
            b"permit-first",
            1,
            &mut rng,
        );
        let second = prepare(
            &mut owner,
            second_buffer,
            receiver.destination_hash(),
            b"permit-second",
            2,
            &mut rng,
        );

        let storage = PERMIT_HANDOFF.take();
        let (mut node, mut dispatcher) = storage.split();
        assert!(node.jobs.try_send(first).is_ok());
        assert!(node.jobs.try_send(second).is_ok());
        let first = dispatcher.jobs.try_receive().expect("first job was lost");
        let (first_pending, first_request) = first.begin_permit();
        assert!(dispatcher.permit_requests.try_send(first_request).is_ok());
        assert_eq!(dispatcher.jobs.len(), 1);
        assert_eq!(dispatcher.permit_requests.len(), 1);
        let first_reply = authorize(
            &mut owner,
            node.permit_requests
                .try_receive()
                .expect("first permit request was lost"),
        );
        assert_eq!(dispatcher.permit_requests.len(), 0);
        let second = dispatcher.jobs.try_receive().expect("second job was lost");
        let (second_pending, second_request) = second.begin_permit();
        assert!(dispatcher.permit_requests.try_send(second_request).is_ok());
        assert_eq!(dispatcher.jobs.len(), 0);
        assert_eq!(dispatcher.permit_requests.len(), 1);
        assert_eq!(dispatcher.returns.len(), 0);
        let second_reply = authorize(&mut owner, block_on(node.permit_requests.receive()));

        assert!(node.permit_replies.try_send(first_reply).is_ok());
        let first_reply = dispatcher
            .permit_replies
            .try_receive()
            .expect("first permit reply was lost");
        assert!(node.permit_replies.try_send(second_reply).is_ok());
        let second_reply = block_on(dispatcher.permit_replies.receive());

        // Product code permits one job at a time. Retaining both pending
        // owners here deliberately fault-injects crossed replies and proves
        // node-core's mismatch return loses neither non-Copy side.
        let (first_pending, second_reply) =
            match first_pending.resolve(second_reply, MonotonicMillis::new(2_500)) {
                Err(mismatch) => mismatch.into_parts(),
                Ok(_) => panic!("crossed second reply unexpectedly matched first owner"),
            };
        let (second_pending, first_reply) =
            match second_pending.resolve(first_reply, MonotonicMillis::new(2_500)) {
                Err(mismatch) => mismatch.into_parts(),
                Ok(_) => panic!("crossed first reply unexpectedly matched second owner"),
            };
        let first = match first_pending.resolve(first_reply, MonotonicMillis::new(2_500)) {
            Ok(PermitResolution::Authorized(owner)) => owner,
            Ok(_) => panic!("matching first grant did not authorize"),
            Err(_) => panic!("matching first grant was rejected"),
        };
        let second = match second_pending.resolve(second_reply, MonotonicMillis::new(2_500)) {
            Ok(PermitResolution::Authorized(owner)) => owner,
            Ok(_) => panic!("matching second grant did not authorize"),
            Err(_) => panic!("matching second grant was rejected"),
        };

        assert!(
            dispatcher
                .returns
                .try_send(first.complete(TxCompletionCode::new(1)).into())
                .is_ok()
        );
        assert!(
            dispatcher
                .returns
                .try_send(second.complete(TxCompletionCode::new(2)).into())
                .is_ok()
        );
        let first = available(
            &mut owner,
            node.returns.try_receive().expect("first return was lost"),
        );
        let second = available(&mut owner, block_on(node.returns.receive()));
        assert_eq!(ptr::from_ref(&*first), first_pointer);
        assert_eq!(ptr::from_ref(&*second), second_pointer);
    }

    static PRESSURE_A: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static PRESSURE_B: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static PRESSURE_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn control_and_completion_pressure_return_exact_noncopy_values() {
        let mut owner = node::<2>(30, "pressure-owner");
        let receiver = node::<0>(31, "pressure-receiver");
        register_peer(&mut owner, 31, "pressure-receiver");
        let first_buffer = PRESSURE_A.take();
        let second_buffer = PRESSURE_B.take();
        owner.register_packet_buffer(first_buffer).unwrap();
        owner.register_packet_buffer(second_buffer).unwrap();
        let first_pointer = ptr::from_ref(&*first_buffer);
        let second_pointer = ptr::from_ref(&*second_buffer);
        let mut rng = CounterRng::default();
        let first = prepare(
            &mut owner,
            first_buffer,
            receiver.destination_hash(),
            b"pressure-first",
            1,
            &mut rng,
        );
        let second = prepare(
            &mut owner,
            second_buffer,
            receiver.destination_hash(),
            b"pressure-second",
            2,
            &mut rng,
        );
        let (first_pending, first_request) = first.begin_permit();
        let (second_pending, second_request) = second.begin_permit();

        // Deliberately overdrive capacity one with two owners so every scalar
        // and owning Full path is exercised and then reconciled end to end.
        let storage = PRESSURE_HANDOFF.take();
        let (mut node, mut dispatcher) = storage.split();
        assert!(dispatcher.permit_requests.try_send(first_request).is_ok());
        let second_request = match dispatcher.permit_requests.try_send(second_request) {
            Err(full) => full.into_inner(),
            Ok(()) => panic!("over-capacity request was accepted"),
        };
        let first_reply = authorize(
            &mut owner,
            node.permit_requests
                .try_receive()
                .expect("first request was lost"),
        );
        assert!(dispatcher.permit_requests.try_send(second_request).is_ok());
        let second_reply = authorize(
            &mut owner,
            node.permit_requests
                .try_receive()
                .expect("second request was lost"),
        );

        assert!(node.permit_replies.try_send(first_reply).is_ok());
        let second_reply = match node.permit_replies.try_send(second_reply) {
            Err(full) => full.into_inner(),
            Ok(()) => panic!("over-capacity reply was accepted"),
        };
        let first_reply = dispatcher
            .permit_replies
            .try_receive()
            .expect("first reply was lost");
        assert!(node.permit_replies.try_send(second_reply).is_ok());
        let second_reply = dispatcher
            .permit_replies
            .try_receive()
            .expect("second reply was lost");

        let first = match first_pending.resolve(first_reply, MonotonicMillis::new(2_500)) {
            Ok(PermitResolution::Authorized(owner)) => owner,
            Ok(_) => panic!("first pressure reply did not authorize"),
            Err(_) => panic!("first pressure reply mismatched"),
        };
        let second = match second_pending.resolve(second_reply, MonotonicMillis::new(2_500)) {
            Ok(PermitResolution::Authorized(owner)) => owner,
            Ok(_) => panic!("second pressure reply did not authorize"),
            Err(_) => panic!("second pressure reply mismatched"),
        };
        let first = TxOwnerReturn::Completion(first.complete(TxCompletionCode::new(1)));
        let second = TxOwnerReturn::Completion(second.complete(TxCompletionCode::new(2)));
        assert!(dispatcher.returns.try_send(first).is_ok());
        let second = match dispatcher.returns.try_send(second) {
            Err(full) => full.into_inner(),
            Ok(()) => panic!("over-capacity completion was accepted"),
        };
        let first = available(
            &mut owner,
            node.returns.try_receive().expect("first return was lost"),
        );
        assert!(dispatcher.returns.try_send(second).is_ok());
        let second = available(
            &mut owner,
            node.returns.try_receive().expect("second return was lost"),
        );
        assert_eq!(ptr::from_ref(&*first), first_pointer);
        assert_eq!(ptr::from_ref(&*second), second_pointer);
    }
}
