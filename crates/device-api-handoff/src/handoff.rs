//! Depth-one Embassy channels and their unique endpoint roles.

use core::{
    future::poll_fn,
    task::{Context, Poll},
};

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};

use crate::{LocalApiReply, LocalApiRequest};

/// A full depth-one channel returned its unchanged owner.
#[must_use = "channel pressure retains the exact owner for retry or explicit rejection"]
pub struct SendPressure<T> {
    owner: T,
}

impl<T> SendPressure<T> {
    fn new(owner: T) -> Self {
        Self { owner }
    }

    /// Recover the exact owner that was not enqueued.
    pub fn into_inner(self) -> T {
        self.owner
    }
}

fn try_enqueue<M, T>(channel: &Channel<M, T, 1>, owner: T) -> Result<(), SendPressure<T>>
where
    M: RawMutex,
{
    channel
        .try_send(owner)
        .map_err(|TrySendError::Full(owner)| SendPressure::new(owner))
}

/// Sole bearer-side producer of authenticated local API requests.
#[must_use = "dropping the request sender abandons the bearer producer role"]
pub struct RequestSender<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    channel: &'static Channel<M, LocalApiRequest<G>, 1>,
}

impl<M, G> RequestSender<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    /// Try to transfer one exact authenticated request to the node queue.
    ///
    /// Pressure returns the complete request, including its opaque grant and
    /// all 512 buffer bytes.
    #[allow(clippy::result_large_err)]
    pub fn try_send(
        &mut self,
        request: LocalApiRequest<G>,
    ) -> Result<(), SendPressure<LocalApiRequest<G>>> {
        try_enqueue(self.channel, request)
    }

    /// Await request capacity without moving a request into the future.
    ///
    /// Cancelling this wait retains the exact request in the caller. Readiness
    /// is advisory; the sole producer should immediately retry [`Self::try_send`].
    pub async fn wait_ready_to_send(&mut self) {
        poll_fn(|context| self.channel.poll_ready_to_send(context)).await;
    }

    /// Poll request capacity without moving a request.
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.channel.poll_ready_to_send(context)
    }

    /// Fixed request queue capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Number of queued request owners.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no request owner is queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side consumer of authenticated local API requests.
#[must_use = "dropping the request receiver abandons the node consumer role"]
pub struct RequestReceiver<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    channel: &'static Channel<M, LocalApiRequest<G>, 1>,
}

impl<M, G> RequestReceiver<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    /// Await the oldest exact request owner.
    ///
    /// Cancelling while pending leaves the request in this channel. Once this
    /// future returns, the node owns the request independently of the bearer
    /// connection's lifetime.
    pub async fn receive(&mut self) -> LocalApiRequest<G> {
        self.channel.receive().await
    }

    /// Poll for the oldest exact request owner.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<LocalApiRequest<G>> {
        self.channel.poll_receive(context)
    }

    /// Receive the oldest request immediately, if one is queued.
    pub fn try_receive(&mut self) -> Option<LocalApiRequest<G>> {
        self.channel.try_receive().ok()
    }

    /// Fixed request queue capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Number of queued request owners.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no request owner is queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side producer of encoded local API replies.
#[must_use = "dropping the reply sender abandons the node producer role"]
pub struct ReplySender<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, LocalApiReply, 1>,
}

impl<M> ReplySender<M>
where
    M: RawMutex + 'static,
{
    /// Try to transfer one exact reply to the bearer queue.
    ///
    /// Pressure returns the complete routing key and all 512 response bytes.
    #[allow(clippy::result_large_err)]
    pub fn try_send(&mut self, reply: LocalApiReply) -> Result<(), SendPressure<LocalApiReply>> {
        try_enqueue(self.channel, reply)
    }

    /// Await reply capacity without moving a reply into the future.
    ///
    /// Cancelling this wait retains the exact reply in the node owner.
    pub async fn wait_ready_to_send(&mut self) {
        poll_fn(|context| self.channel.poll_ready_to_send(context)).await;
    }

    /// Poll reply capacity without moving a reply.
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.channel.poll_ready_to_send(context)
    }

    /// Fixed reply queue capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Number of queued reply owners.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no reply owner is queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole bearer-side consumer of encoded local API replies.
#[must_use = "dropping the reply receiver abandons the bearer consumer role"]
pub struct ReplyReceiver<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, LocalApiReply, 1>,
}

impl<M> ReplyReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Await the oldest exact reply owner.
    ///
    /// Cancelling while pending leaves the reply in this channel. After
    /// receipt, the bearer must validate both session epoch and correlation.
    pub async fn receive(&mut self) -> LocalApiReply {
        self.channel.receive().await
    }

    /// Poll for the oldest exact reply owner.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<LocalApiReply> {
        self.channel.poll_receive(context)
    }

    /// Receive the oldest reply immediately, if one is queued.
    pub fn try_receive(&mut self) -> Option<LocalApiReply> {
        self.channel.try_receive().ok()
    }

    /// Fixed reply queue capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Number of queued reply owners.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no reply owner is queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Boot-lifetime bearer-manager request producer and reply consumer.
///
/// This owner outlives individual USB/BLE/Wi-Fi connections and authenticated
/// session epochs. Dropping it permanently abandons both channel capabilities.
#[must_use = "dropping bearer roles permanently abandons both local API channel capabilities"]
pub struct BearerHandoff<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    requests: RequestSender<M, G>,
    replies: ReplyReceiver<M>,
}

impl<M, G> BearerHandoff<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    /// Borrow the sole bearer request producer.
    pub fn requests(&mut self) -> &mut RequestSender<M, G> {
        &mut self.requests
    }

    /// Borrow the sole bearer reply consumer.
    pub fn replies(&mut self) -> &mut ReplyReceiver<M> {
        &mut self.replies
    }
}

/// Node-side request consumer and reply producer from one static store.
#[must_use = "dropping node roles abandons both local API channel capabilities"]
pub struct NodeHandoff<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    requests: RequestReceiver<M, G>,
    replies: ReplySender<M>,
}

impl<M, G> NodeHandoff<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    /// Borrow the sole node request consumer.
    pub fn requests(&mut self) -> &mut RequestReceiver<M, G> {
        &mut self.requests
    }

    /// Borrow the sole node reply producer.
    pub fn replies(&mut self) -> &mut ReplySender<M> {
        &mut self.replies
    }
}

/// Inseparable common-origin proof for bearer and node roles.
#[must_use = "dropping paired roles abandons all local API channel capabilities"]
pub struct PairedHandoff<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    bearer: BearerHandoff<M, G>,
    node: NodeHandoff<M, G>,
}

impl<M, G> PairedHandoff<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    /// Consume common-origin proof into the sole bearer and node roles.
    pub fn into_parts(self) -> (BearerHandoff<M, G>, NodeHandoff<M, G>) {
        (self.bearer, self.node)
    }
}

/// Static depth-one request/reply storage for one local API bearer owner.
///
/// The request and reply channels are independent so a node mutation may
/// finish and retain its response while the bearer is disconnected or under
/// backpressure.
pub struct DeviceApiHandoff<M, G>
where
    M: RawMutex,
{
    requests: Channel<M, LocalApiRequest<G>, 1>,
    replies: Channel<M, LocalApiReply, 1>,
}

impl<M, G> DeviceApiHandoff<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    /// Construct empty request and reply stores.
    pub const fn new() -> Self {
        Self {
            requests: Channel::new(),
            replies: Channel::new(),
        }
    }

    /// Split the store into its only boot-lifetime bearer and node roles.
    pub fn split(&'static mut self) -> (BearerHandoff<M, G>, NodeHandoff<M, G>) {
        self.split_paired().into_parts()
    }

    /// Split the store into unforgeable common-origin roles.
    pub fn split_paired(&'static mut self) -> PairedHandoff<M, G> {
        PairedHandoff {
            bearer: BearerHandoff {
                requests: RequestSender {
                    channel: &self.requests,
                },
                replies: ReplyReceiver {
                    channel: &self.replies,
                },
            },
            node: NodeHandoff {
                requests: RequestReceiver {
                    channel: &self.requests,
                },
                replies: ReplySender {
                    channel: &self.replies,
                },
            },
        }
    }
}

impl<M, G> Default for DeviceApiHandoff<M, G>
where
    M: RawMutex + 'static,
    G: 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
