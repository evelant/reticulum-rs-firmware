//! Durable request/acknowledgement handoff for authorized frame observations.
//!
//! The dispatcher keeps its local [`AuthorizedFrameObservation`] after sending
//! a request. Only an exactly matching acknowledgement permits that caller to
//! forget the observation. This module deliberately transports both scalars
//! unchanged and leaves correlation to the dispatcher owner, so a mismatched
//! acknowledgement remains visible instead of being silently consumed as
//! success.

use core::task::{Context, Poll};

use embassy_sync::{blocking_mutex::raw::RawMutex, channel::Channel};
use reticulum_node_core::AuthorizedFrameObservation;

use super::{ChannelFull, try_enqueue};

/// Sole dispatcher-side producer of authorized-frame observation requests.
#[must_use = "dropping this sender abandons the frame-observation request producer role"]
pub struct AuthorizedFrameRequestSender<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, AuthorizedFrameObservation, 1>,
}

impl<M> AuthorizedFrameRequestSender<M>
where
    M: RawMutex + 'static,
{
    /// Try to enqueue one exact authorized-frame observation.
    // Returning the complete scalar unchanged is the allocation-free recovery
    // contract; boxing or truncating it would defeat exact retry/correlation.
    #[allow(clippy::result_large_err)]
    pub fn try_send(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> Result<(), ChannelFull<AuthorizedFrameObservation>> {
        try_enqueue(self.channel, observation)
    }

    /// Poll until one request can be retried through [`Self::try_send`].
    ///
    /// Readiness is advisory. The dispatcher must retain the exact observation
    /// until a later send succeeds and an exactly matching acknowledgement is
    /// received.
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.channel.poll_ready_to_send(context)
    }

    /// Fixed observation-request channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued observation requests.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no observation requests are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side consumer of authorized-frame observation requests.
#[must_use = "dropping this receiver abandons the frame-observation request consumer role"]
pub struct AuthorizedFrameRequestReceiver<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, AuthorizedFrameObservation, 1>,
}

impl<M> AuthorizedFrameRequestReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Await the oldest authorized-frame observation request.
    pub async fn receive(&mut self) -> AuthorizedFrameObservation {
        self.channel.receive().await
    }

    /// Poll for the oldest observation without constructing a receive future.
    ///
    /// `Pending` leaves the observation in the channel. `Ready` transfers the
    /// exact scalar to the caller.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<AuthorizedFrameObservation> {
        self.channel.poll_receive(context)
    }

    /// Receive the oldest observation immediately, if one is queued.
    pub fn try_receive(&mut self) -> Option<AuthorizedFrameObservation> {
        self.channel.try_receive().ok()
    }

    /// Fixed observation-request channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued observation requests.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no observation requests are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole node-side producer of durable observation acknowledgements.
///
/// The payload is the exact request observation. Echoing the complete scalar
/// makes a crossed or stale acknowledgement observable to the dispatcher.
#[must_use = "dropping this sender abandons the frame-observation acknowledgement producer role"]
pub struct AuthorizedFrameAcknowledgementSender<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, AuthorizedFrameObservation, 1>,
}

impl<M> AuthorizedFrameAcknowledgementSender<M>
where
    M: RawMutex + 'static,
{
    /// Try to enqueue one exact durable acknowledgement.
    // Returning the complete scalar unchanged is the allocation-free recovery
    // contract; boxing or truncating it would defeat exact retry/correlation.
    #[allow(clippy::result_large_err)]
    pub fn try_send(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> Result<(), ChannelFull<AuthorizedFrameObservation>> {
        try_enqueue(self.channel, observation)
    }

    /// Poll until one acknowledgement can be retried through [`Self::try_send`].
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.channel.poll_ready_to_send(context)
    }

    /// Fixed acknowledgement channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued acknowledgements.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no acknowledgements are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Sole dispatcher-side consumer of durable observation acknowledgements.
#[must_use = "dropping this receiver abandons the frame-observation acknowledgement consumer role"]
pub struct AuthorizedFrameAcknowledgementReceiver<M>
where
    M: RawMutex + 'static,
{
    channel: &'static Channel<M, AuthorizedFrameObservation, 1>,
}

impl<M> AuthorizedFrameAcknowledgementReceiver<M>
where
    M: RawMutex + 'static,
{
    /// Await the oldest durable acknowledgement.
    pub async fn receive(&mut self) -> AuthorizedFrameObservation {
        self.channel.receive().await
    }

    /// Poll for the oldest acknowledgement without constructing a receive future.
    ///
    /// `Pending` leaves the acknowledgement in the channel. `Ready` transfers
    /// the exact scalar to the caller for explicit correlation.
    pub fn poll_receive(&mut self, context: &mut Context<'_>) -> Poll<AuthorizedFrameObservation> {
        self.channel.poll_receive(context)
    }

    /// Receive the oldest acknowledgement immediately, if one is queued.
    pub fn try_receive(&mut self) -> Option<AuthorizedFrameObservation> {
        self.channel.try_receive().ok()
    }

    /// Fixed acknowledgement channel capacity.
    pub const fn capacity(&self) -> usize {
        1
    }

    /// Current number of queued acknowledgements.
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// Whether no acknowledgements are queued.
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }
}

/// Node-side roles for one authorized-frame observation exchange.
#[must_use = "dropping node frame-observation roles abandons their channel capabilities"]
pub struct AuthorizedFrameNodeHandoff<M>
where
    M: RawMutex + 'static,
{
    requests: AuthorizedFrameRequestReceiver<M>,
    acknowledgements: AuthorizedFrameAcknowledgementSender<M>,
}

impl<M> AuthorizedFrameNodeHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Borrow the sole node-side observation-request consumer.
    pub fn requests(&mut self) -> &mut AuthorizedFrameRequestReceiver<M> {
        &mut self.requests
    }

    /// Borrow the sole node-side durable-acknowledgement producer.
    pub fn acknowledgements(&mut self) -> &mut AuthorizedFrameAcknowledgementSender<M> {
        &mut self.acknowledgements
    }
}

/// Dispatcher-side roles for one authorized-frame observation exchange.
#[must_use = "dropping dispatcher frame-observation roles abandons their channel capabilities"]
pub struct AuthorizedFrameDispatcherHandoff<M>
where
    M: RawMutex + 'static,
{
    requests: AuthorizedFrameRequestSender<M>,
    acknowledgements: AuthorizedFrameAcknowledgementReceiver<M>,
}

impl<M> AuthorizedFrameDispatcherHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Borrow the sole dispatcher-side observation-request producer.
    pub fn requests(&mut self) -> &mut AuthorizedFrameRequestSender<M> {
        &mut self.requests
    }

    /// Borrow the sole dispatcher-side durable-acknowledgement consumer.
    pub fn acknowledgements(&mut self) -> &mut AuthorizedFrameAcknowledgementReceiver<M> {
        &mut self.acknowledgements
    }
}

/// One inseparable frame-observation role pair from a single static store.
///
/// A permanent aggregate can consume this proof instead of constructing node
/// and dispatcher endpoints from unrelated stores.
#[must_use = "dropping paired frame-observation roles abandons both channel capabilities"]
pub struct AuthorizedFramePairedHandoff<M>
where
    M: RawMutex + 'static,
{
    node: AuthorizedFrameNodeHandoff<M>,
    dispatcher: AuthorizedFrameDispatcherHandoff<M>,
}

impl<M> AuthorizedFramePairedHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Consume common-origin proof into the node and dispatcher roles.
    pub fn into_parts(
        self,
    ) -> (
        AuthorizedFrameNodeHandoff<M>,
        AuthorizedFrameDispatcherHandoff<M>,
    ) {
        (self.node, self.dispatcher)
    }
}

/// Depth-one storage for retained authorized-frame observations and durable acknowledgements.
///
/// One static store belongs to one concrete dispatcher/node relationship. The
/// dispatcher may have at most one observation awaiting durable retirement,
/// matching the serialized request/re-offer protocol in the submission
/// runtime.
pub struct AuthorizedFrameHandoff<M>
where
    M: RawMutex,
{
    requests: Channel<M, AuthorizedFrameObservation, 1>,
    acknowledgements: Channel<M, AuthorizedFrameObservation, 1>,
}

impl<M> AuthorizedFrameHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Construct an empty authorized-frame observation handoff store.
    pub const fn new() -> Self {
        Self {
            requests: Channel::new(),
            acknowledgements: Channel::new(),
        }
    }

    /// Split the store into its only live node and dispatcher roles.
    pub fn split(
        &'static mut self,
    ) -> (
        AuthorizedFrameNodeHandoff<M>,
        AuthorizedFrameDispatcherHandoff<M>,
    ) {
        self.split_paired().into_parts()
    }

    /// Split into an unforgeable common-origin frame-observation role pair.
    pub fn split_paired(&'static mut self) -> AuthorizedFramePairedHandoff<M> {
        AuthorizedFramePairedHandoff {
            node: AuthorizedFrameNodeHandoff {
                requests: AuthorizedFrameRequestReceiver {
                    channel: &self.requests,
                },
                acknowledgements: AuthorizedFrameAcknowledgementSender {
                    channel: &self.acknowledgements,
                },
            },
            dispatcher: AuthorizedFrameDispatcherHandoff {
                requests: AuthorizedFrameRequestSender {
                    channel: &self.requests,
                },
                acknowledgements: AuthorizedFrameAcknowledgementReceiver {
                    channel: &self.acknowledgements,
                },
            },
        }
    }
}

impl<M> Default for AuthorizedFrameHandoff<M>
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

    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use static_cell::ConstStaticCell;

    use super::AuthorizedFrameHandoff;

    static PRODUCTION_HANDOFF: ConstStaticCell<AuthorizedFrameHandoff<CriticalSectionRawMutex>> =
        ConstStaticCell::new(AuthorizedFrameHandoff::new());

    #[test]
    fn production_mutex_static_store_splits_common_origin_roles() {
        let paired = PRODUCTION_HANDOFF.take().split_paired();
        let (mut node, mut dispatcher) = paired.into_parts();

        assert_eq!(node.requests().capacity(), 1);
        assert_eq!(node.acknowledgements().capacity(), 1);
        assert_eq!(dispatcher.requests().capacity(), 1);
        assert_eq!(dispatcher.acknowledgements().capacity(), 1);
    }
}
