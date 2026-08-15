//! Bounded channels between Reticulum node and packet-interface actors.
//!
//! Destination-DATA and ordinary packet owners move through
//! `reticulum-interface-router`; this crate supplies independent depth-one
//! permit request/reply stores for each concrete actor. A separate depth-one
//! authorized-frame observation pair lets the interface retain an exact
//! observation until the node durably acknowledges it. Full sends return the
//! unchanged value for fault handling, and cancelling an Embassy receive
//! future does not remove a queued scalar message.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::task::{Context, Poll};

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};
use reticulum_node_core::{TxPermitReply, TxPermitRequest};

mod authorized_frame;
mod ordinary;

pub use authorized_frame::{
    AuthorizedFrameAcknowledgementReceiver, AuthorizedFrameAcknowledgementSender,
    AuthorizedFrameDispatcherHandoff, AuthorizedFrameHandoff, AuthorizedFrameNodeHandoff,
    AuthorizedFramePairedHandoff, AuthorizedFrameRequestReceiver, AuthorizedFrameRequestSender,
};
pub use ordinary::{
    OrdinaryDispatcherPermitHandoff, OrdinaryNodePermitHandoff, OrdinaryPairedPermitHandoff,
    OrdinaryPermitHandoff, OrdinaryPermitReplyReceiver, OrdinaryPermitReplySender,
    OrdinaryPermitRequestReceiver, OrdinaryPermitRequestSender,
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

    /// Poll until one request can be retried through [`Self::try_send`].
    ///
    /// Readiness is advisory. A persistent interface actor must retain the
    /// exact request and its packet owner until a later `try_send` succeeds.
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.channel.poll_ready_to_send(context)
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

    /// Poll for the oldest request without constructing a receive future.
    ///
    /// `Pending` leaves the request in the channel. `Ready` transfers the
    /// exact request to the caller, which must store it before returning from
    /// the surrounding poll.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<TxPermitRequest> {
        self.channel.poll_receive(context)
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

    /// Poll until one reply can be retried through [`Self::try_send`].
    ///
    /// Readiness is advisory. A persistent permit server must retain the
    /// exact non-`Copy` reply until a later `try_send` succeeds.
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.channel.poll_ready_to_send(context)
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

/// Dispatcher port group for the scalar permit request/reply exchange.
///
/// This group is created together with the matching node-side roles by a
/// [`DataPermitHandoff`].
#[must_use = "dropping dispatcher permit roles permanently abandons their channel capabilities"]
pub struct DispatcherPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    requests: PermitRequestSender<M>,
    replies: PermitReplyReceiver<M>,
}

impl<M> DispatcherPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Borrow the sole dispatcher-side permit-request producer.
    pub fn requests(&mut self) -> &mut PermitRequestSender<M> {
        &mut self.requests
    }

    /// Borrow the sole dispatcher-side permit-reply consumer.
    pub fn replies(&mut self) -> &mut PermitReplyReceiver<M> {
        &mut self.replies
    }
}

/// Node-side DATA permit ports for one concrete interface actor.
///
/// Routed DATA owners and their completion tickets move through
/// `reticulum-interface-router`; this group therefore contains only the
/// scalar request/reply exchange required while the actor retains an exact
/// [`reticulum_node_core::PermitPendingTx`] owner.
#[must_use = "dropping DATA node permit roles abandons their channel capabilities"]
pub struct DataNodePermitHandoff<M>
where
    M: RawMutex + 'static,
{
    requests: PermitRequestReceiver<M>,
    replies: PermitReplySender<M>,
}

impl<M> DataNodePermitHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Borrow the sole node-side DATA permit-request consumer.
    pub fn requests(&mut self) -> &mut PermitRequestReceiver<M> {
        &mut self.requests
    }

    /// Borrow the sole node-side DATA permit-reply producer.
    pub fn replies(&mut self) -> &mut PermitReplySender<M> {
        &mut self.replies
    }
}

/// One inseparable DATA permit role pair from a single static store.
///
/// A permanent node/interface aggregate consumes this proof so node and actor
/// permit endpoints cannot be accidentally assembled from different stores.
#[must_use = "dropping paired DATA permit roles abandons both channel capabilities"]
pub struct DataPairedPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    node: DataNodePermitHandoff<M>,
    dispatcher: DispatcherPermitHandoff<M>,
}

impl<M> DataPairedPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Consume common-origin proof into the node and actor permit roles.
    pub fn into_parts(self) -> (DataNodePermitHandoff<M>, DispatcherPermitHandoff<M>) {
        (self.node, self.dispatcher)
    }
}

/// Permit-only channel storage for ticket-routed destination-DATA packets.
///
/// Both channels have depth one because one concrete interface actor
/// serializes one retained packet owner through its permit exchange. Create
/// one independent store per actor; requests and replies never need a shared
/// correlation identifier.
pub struct DataPermitHandoff<M>
where
    M: RawMutex,
{
    requests: Channel<M, TxPermitRequest, 1>,
    replies: Channel<M, TxPermitReply, 1>,
}

impl<M> DataPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Construct an empty DATA permit handoff store.
    pub const fn new() -> Self {
        Self {
            requests: Channel::new(),
            replies: Channel::new(),
        }
    }

    /// Split the store into its only live node and actor permit roles.
    pub fn split(&'static mut self) -> (DataNodePermitHandoff<M>, DispatcherPermitHandoff<M>) {
        self.split_paired().into_parts()
    }

    /// Split into an unforgeable common-origin DATA permit role pair.
    pub fn split_paired(&'static mut self) -> DataPairedPermitHandoff<M> {
        DataPairedPermitHandoff {
            node: DataNodePermitHandoff {
                requests: PermitRequestReceiver {
                    channel: &self.requests,
                },
                replies: PermitReplySender {
                    channel: &self.replies,
                },
            },
            dispatcher: DispatcherPermitHandoff {
                requests: PermitRequestSender {
                    channel: &self.requests,
                },
                replies: PermitReplyReceiver {
                    channel: &self.replies,
                },
            },
        }
    }
}

impl<M> Default for DataPermitHandoff<M>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
