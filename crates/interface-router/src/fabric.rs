use super::*;

pub(crate) struct InterfaceChannels<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex,
{
    jobs: Channel<M, InterfaceTxJob, QUEUE_DEPTH>,
    completions: Channel<M, InterfaceTxCompletion, QUEUE_DEPTH>,
    available_ingress: Channel<M, AvailableIngressBuffer, QUEUE_DEPTH>,
    completed_ingress: Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    lifecycle_requests: Channel<M, InterfaceLifecycleRequest, 1>,
    lifecycle_acknowledgements: Channel<M, InterfaceLifecycleAcknowledgement, 1>,
}

impl<M, const QUEUE_DEPTH: usize> InterfaceChannels<M, QUEUE_DEPTH>
where
    M: RawMutex,
{
    pub(crate) const fn new() -> Self {
        Self {
            jobs: Channel::new(),
            completions: Channel::new(),
            available_ingress: Channel::new(),
            completed_ingress: Channel::new(),
            lifecycle_requests: Channel::new(),
            lifecycle_acknowledgements: Channel::new(),
        }
    }
}

pub(crate) struct RouterQueue<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    pub(crate) jobs: &'static Channel<M, InterfaceTxJob, QUEUE_DEPTH>,
    pub(crate) completions: &'static Channel<M, InterfaceTxCompletion, QUEUE_DEPTH>,
    pub(crate) available_ingress: &'static Channel<M, AvailableIngressBuffer, QUEUE_DEPTH>,
    pub(crate) completed_ingress: &'static Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    pub(crate) lifecycle_requests: &'static Channel<M, InterfaceLifecycleRequest, 1>,
    pub(crate) lifecycle_acknowledgements:
        &'static Channel<M, InterfaceLifecycleAcknowledgement, 1>,
    pub(crate) ingress_origin: &'static IngressFabricOrigin,
}

/// Fixed storage for a registry and one bounded job/completion pair per actor.
///
/// Allocate this in static storage and call [`Self::split`] once. The returned
/// actor handles are non-`Clone` capabilities permanently bound to distinct
/// queue slots.
pub struct InterfaceFabric<M, const SLOTS: usize, const QUEUE_DEPTH: usize>
where
    M: RawMutex,
{
    registry: InterfaceRegistry<SLOTS>,
    channels: [InterfaceChannels<M, QUEUE_DEPTH>; SLOTS],
    ingress_origin: IngressFabricOrigin,
    ingress_buffers: [[IngressBufferStorage; QUEUE_DEPTH]; SLOTS],
}

impl<M, const SLOTS: usize, const QUEUE_DEPTH: usize> InterfaceFabric<M, SLOTS, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Construct empty static interface-fabric storage.
    pub const fn new() -> Self {
        const {
            assert!(QUEUE_DEPTH > 0, "interface queues must be nonempty");
        }
        Self {
            registry: InterfaceRegistry::new(),
            channels: [const { InterfaceChannels::new() }; SLOTS],
            ingress_origin: IngressFabricOrigin::new(),
            ingress_buffers: [const { [const { IngressBufferStorage::new() }; QUEUE_DEPTH] };
                SLOTS],
        }
    }

    /// Split the fabric into its sole router and one actor capability per
    /// fixed queue slot.
    pub fn split(
        &'static mut self,
    ) -> (
        OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        [InterfaceActorHandoff<M, QUEUE_DEPTH>; SLOTS],
    ) {
        let registry = mem::take(&mut self.registry);
        let channels: &'static [InterfaceChannels<M, QUEUE_DEPTH>; SLOTS] = &self.channels;
        let ingress_origin: &'static IngressFabricOrigin = &self.ingress_origin;
        let ingress_buffers: &'static mut [[IngressBufferStorage; QUEUE_DEPTH]; SLOTS] =
            &mut self.ingress_buffers;
        for (queue_index, buffers) in ingress_buffers.iter_mut().enumerate() {
            let queue = InterfaceQueueId(queue_index as u16);
            for (buffer_index, buffer) in buffers.iter_mut().enumerate() {
                let available =
                    AvailableIngressBuffer::new(queue, buffer_index, ingress_origin, buffer);
                match channels[queue_index].available_ingress.try_send(available) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        unreachable!("fresh ingress pool must fit its empty channel")
                    }
                }
            }
        }
        let queues = array::from_fn(|index| RouterQueue {
            jobs: &channels[index].jobs,
            completions: &channels[index].completions,
            available_ingress: &channels[index].available_ingress,
            completed_ingress: &channels[index].completed_ingress,
            lifecycle_requests: &channels[index].lifecycle_requests,
            lifecycle_acknowledgements: &channels[index].lifecycle_acknowledgements,
            ingress_origin,
        });
        let actors = array::from_fn(|index| InterfaceActorHandoff {
            queue: InterfaceQueueId(index as u16),
            jobs: &channels[index].jobs,
            completions: &channels[index].completions,
            available_ingress: &channels[index].available_ingress,
            completed_ingress: &channels[index].completed_ingress,
            lifecycle_requests: &channels[index].lifecycle_requests,
            lifecycle_acknowledgements: &channels[index].lifecycle_acknowledgements,
            ingress_origin,
        });
        (
            OutboundRouter {
                registry,
                queues,
                completion_cursor: 0,
                ingress_cursor: 0,
                lifecycle_cursor: 0,
            },
            actors,
        )
    }
}

impl<M, const SLOTS: usize, const QUEUE_DEPTH: usize> Default
    for InterfaceFabric<M, SLOTS, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Immediate actor-side rejection while enqueueing one lifecycle request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceLifecycleTryRequestError {
    /// The supplied lease belongs to another fixed actor queue.
    ForeignQueue {
        /// Queue permanently owned by this actor capability.
        actor: InterfaceQueueId,
        /// Queue named by the supplied lease.
        supplied: InterfaceQueueId,
    },
    /// Another exact lifecycle exchange is awaiting acknowledgement.
    ExchangePending {
        /// Exact earlier request still awaiting acknowledgement.
        pending: InterfaceLifecycleRequest,
        /// Exact request that was not enqueued.
        unsent: InterfaceLifecycleRequest,
    },
    /// The request queue is occupied; the payload is the exact unsent request.
    RequestQueueFull(InterfaceLifecycleRequest),
}

/// Actor-side lifecycle exchange status or fail-closed correlation error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceLifecycleActorError {
    /// The supplied lease belongs to another fixed actor queue.
    ForeignQueue {
        /// Queue permanently owned by this actor capability.
        actor: InterfaceQueueId,
        /// Queue named by the supplied lease.
        supplied: InterfaceQueueId,
    },
    /// Another exact lifecycle request still awaits acknowledgement.
    ExchangePending(InterfaceLifecycleRequest),
    /// The bounded request queue was unexpectedly occupied while no exchange
    /// was locally pending.
    RequestQueueFull(InterfaceLifecycleRequest),
    /// No locally pending request exists to finish.
    NoPendingRequest,
    /// The supervisor rejected the exact request without changing registry
    /// eligibility.
    Rejected(InterfaceLifecycleRouteError),
    /// The acknowledgement did not name the request this call emitted.
    CrossedAcknowledgement {
        /// Exact request emitted by this call.
        expected: InterfaceLifecycleRequest,
        /// Different request named by the received acknowledgement.
        received: InterfaceLifecycleRequest,
    },
    /// An acknowledgement arrived while this actor had no pending exchange.
    UnexpectedAcknowledgement(InterfaceLifecycleRequest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InterfaceLifecycleActorPhase {
    Idle,
    Awaiting(InterfaceLifecycleRequest),
}

/// Split concrete-actor capability for generation-bound Ready/Offline
/// reporting.
#[must_use = "a permanent actor must retain its lifecycle capability"]
pub struct InterfaceLifecycleActorHandoff<M>
where
    M: RawMutex + 'static,
{
    pub(crate) queue: InterfaceQueueId,
    pub(crate) requests: &'static Channel<M, InterfaceLifecycleRequest, 1>,
    pub(crate) acknowledgements: &'static Channel<M, InterfaceLifecycleAcknowledgement, 1>,
    pub(crate) phase: InterfaceLifecycleActorPhase,
}

impl<M> InterfaceLifecycleActorHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Fixed queue identity owned by this actor capability.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.queue
    }

    /// Immediately enqueue one generation-bound lifecycle request.
    pub fn try_request_state(
        &mut self,
        lease: InterfaceLease,
        state: InterfaceLifecycleState,
    ) -> Result<InterfaceLifecycleRequest, InterfaceLifecycleTryRequestError> {
        if lease.queue != self.queue {
            return Err(InterfaceLifecycleTryRequestError::ForeignQueue {
                actor: self.queue,
                supplied: lease.queue,
            });
        }
        let request = InterfaceLifecycleRequest { lease, state };
        if let InterfaceLifecycleActorPhase::Awaiting(pending) = self.phase {
            return Err(InterfaceLifecycleTryRequestError::ExchangePending {
                pending,
                unsent: request,
            });
        }
        match self.requests.try_send(request) {
            Ok(()) => {
                self.phase = InterfaceLifecycleActorPhase::Awaiting(request);
                Ok(request)
            }
            Err(TrySendError::Full(request)) => {
                Err(InterfaceLifecycleTryRequestError::RequestQueueFull(request))
            }
        }
    }

    /// Finish the locally pending exchange immediately.
    ///
    /// `Ok(None)` means an exchange is pending but its acknowledgement is not
    /// yet available. Idle actors receive [`InterfaceLifecycleActorError::NoPendingRequest`]
    /// instead of an ambiguous empty result.
    pub fn try_finish_request(
        &mut self,
    ) -> Result<Option<InterfaceDescriptor>, InterfaceLifecycleActorError> {
        if matches!(self.phase, InterfaceLifecycleActorPhase::Idle) {
            return match self.acknowledgements.try_receive() {
                Ok(acknowledgement) => {
                    Err(InterfaceLifecycleActorError::UnexpectedAcknowledgement(
                        acknowledgement.request,
                    ))
                }
                Err(_) => Err(InterfaceLifecycleActorError::NoPendingRequest),
            };
        }
        let Ok(acknowledgement) = self.acknowledgements.try_receive() else {
            return Ok(None);
        };
        self.resolve_pending_acknowledgement(acknowledgement)
            .map(Some)
    }

    /// Request one state and wait for the exact supervisor acknowledgement.
    ///
    /// A permanent actor must drive this single-flight exchange to completion;
    /// cancelling after request delivery deliberately leaves the exact
    /// pending exchange represented by this capability, with its request or
    /// acknowledgement retained in the bounded channels rather than silently
    /// accepting a later crossed transition.
    pub async fn request_state(
        &mut self,
        lease: InterfaceLease,
        state: InterfaceLifecycleState,
    ) -> Result<InterfaceDescriptor, InterfaceLifecycleActorError> {
        self.try_request_state(lease, state)
            .map_err(|reason| match reason {
                InterfaceLifecycleTryRequestError::ForeignQueue { actor, supplied } => {
                    InterfaceLifecycleActorError::ForeignQueue { actor, supplied }
                }
                InterfaceLifecycleTryRequestError::ExchangePending { pending, .. } => {
                    InterfaceLifecycleActorError::ExchangePending(pending)
                }
                InterfaceLifecycleTryRequestError::RequestQueueFull(request) => {
                    InterfaceLifecycleActorError::RequestQueueFull(request)
                }
            })?;
        self.finish_pending_request().await
    }

    /// Resume and finish one request retained in the actor phase after a
    /// cancelled acknowledgement wait.
    pub async fn finish_pending_request(
        &mut self,
    ) -> Result<InterfaceDescriptor, InterfaceLifecycleActorError> {
        let InterfaceLifecycleActorPhase::Awaiting(expected) = self.phase else {
            return Err(InterfaceLifecycleActorError::NoPendingRequest);
        };
        let acknowledgement = self.acknowledgements.receive().await;
        debug_assert_eq!(self.phase, InterfaceLifecycleActorPhase::Awaiting(expected));
        self.resolve_pending_acknowledgement(acknowledgement)
    }

    fn resolve_pending_acknowledgement(
        &mut self,
        acknowledgement: InterfaceLifecycleAcknowledgement,
    ) -> Result<InterfaceDescriptor, InterfaceLifecycleActorError> {
        let InterfaceLifecycleActorPhase::Awaiting(expected) = self.phase else {
            return Err(InterfaceLifecycleActorError::UnexpectedAcknowledgement(
                acknowledgement.request,
            ));
        };
        if acknowledgement.request != expected {
            return Err(InterfaceLifecycleActorError::CrossedAcknowledgement {
                expected,
                received: acknowledgement.request,
            });
        }
        self.phase = InterfaceLifecycleActorPhase::Idle;
        acknowledgement
            .result
            .map_err(InterfaceLifecycleActorError::Rejected)
    }
}

/// Sole concrete-actor capability for one fixed interface queue.
#[must_use = "dropping an actor handoff abandons its interface queue capability"]
pub struct InterfaceActorHandoff<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    pub(crate) queue: InterfaceQueueId,
    pub(crate) jobs: &'static Channel<M, InterfaceTxJob, QUEUE_DEPTH>,
    pub(crate) completions: &'static Channel<M, InterfaceTxCompletion, QUEUE_DEPTH>,
    pub(crate) available_ingress: &'static Channel<M, AvailableIngressBuffer, QUEUE_DEPTH>,
    pub(crate) completed_ingress: &'static Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    pub(crate) lifecycle_requests: &'static Channel<M, InterfaceLifecycleRequest, 1>,
    pub(crate) lifecycle_acknowledgements:
        &'static Channel<M, InterfaceLifecycleAcknowledgement, 1>,
    pub(crate) ingress_origin: &'static IngressFabricOrigin,
}

impl<M, const QUEUE_DEPTH: usize> InterfaceActorHandoff<M, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Fixed queue identity owned by this actor capability.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.queue
    }

    /// Consume the combined handle into independent TX, RX, and lifecycle
    /// actor capabilities bound to the same fixed queue.
    ///
    /// A permanent concrete interface task owns the RX and lifecycle values
    /// while it may pass the TX capability to a dedicated dispatcher.
    pub fn into_parts(
        self,
    ) -> (
        InterfaceTxActorHandoff<M, QUEUE_DEPTH>,
        InterfaceIngressActorHandoff<M, QUEUE_DEPTH>,
        InterfaceLifecycleActorHandoff<M>,
    ) {
        (
            InterfaceTxActorHandoff {
                queue: self.queue,
                jobs: self.jobs,
                completions: self.completions,
            },
            InterfaceIngressActorHandoff {
                queue: self.queue,
                available: self.available_ingress,
                completed: self.completed_ingress,
                origin: self.ingress_origin,
            },
            InterfaceLifecycleActorHandoff {
                queue: self.queue,
                requests: self.lifecycle_requests,
                acknowledgements: self.lifecycle_acknowledgements,
                phase: InterfaceLifecycleActorPhase::Idle,
            },
        )
    }

    /// Bind a registry-issued descriptor into this fixed actor's RX
    /// provenance capability.
    ///
    /// The queue check prevents an actor from reporting another actor's
    /// interface identity. The outbound router still validates the generation
    /// for every completed ingress envelope.
    pub fn bind_ingress(
        &self,
        descriptor: InterfaceDescriptor,
    ) -> Result<InterfaceIngressAuthority, ActorIngressBindingError> {
        if descriptor.lease.queue != self.queue {
            return Err(ActorIngressBindingError::ForeignQueue {
                expected: self.queue,
                supplied: descriptor.lease.queue,
            });
        }
        Ok(InterfaceIngressAuthority { descriptor })
    }

    /// Receive one reusable mutable native-packet buffer immediately.
    pub fn try_receive_ingress_buffer(&mut self) -> Option<AvailableIngressBuffer> {
        self.available_ingress.try_receive().ok()
    }

    /// Poll for one reusable mutable native-packet buffer.
    ///
    /// A pending poll only registers the current waker. Cancelling the
    /// surrounding future leaves every exact buffer owner queued.
    pub fn poll_receive_ingress_buffer(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<AvailableIngressBuffer> {
        self.available_ingress.poll_receive(context)
    }

    /// Wait cancellation-safely for one reusable mutable packet buffer.
    pub async fn receive_ingress_buffer(&mut self) -> AvailableIngressBuffer {
        poll_fn(|context| self.poll_receive_ingress_buffer(context)).await
    }

    /// Submit one sealed native packet with actor-bound provenance.
    ///
    /// The actor's fixed queue is checked independently against both the
    /// registry authority and the reusable buffer's permanent origin. A
    /// crossed capability or bounded pressure returns the exact sealed owner.
    pub fn try_send_ingress(
        &mut self,
        authority: InterfaceIngressAuthority,
        packet: SealedIngressPacket,
    ) -> Result<(), ActorIngressSendFailure> {
        try_send_actor_ingress(
            self.queue,
            self.ingress_origin,
            self.completed_ingress,
            authority,
            packet,
        )
    }

    /// Poll for advisory completed-ingress queue capacity.
    ///
    /// Readiness reserves no slot and owns no packet. The actor must retain
    /// its sealed packet and retry [`Self::try_send_ingress`] after waking.
    pub fn poll_ingress_send_capacity(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.completed_ingress.poll_ready_to_send(context)
    }

    /// Wait cancellation-safely for advisory completed-ingress capacity.
    pub async fn wait_ingress_send_capacity(&mut self) {
        poll_fn(|context| self.poll_ingress_send_capacity(context)).await
    }

    /// Receive this actor's oldest exact owner immediately, if queued.
    pub fn try_receive_job(&mut self) -> Option<InterfaceTxJob> {
        self.jobs.try_receive().ok()
    }

    /// Poll for this actor's oldest exact owner.
    ///
    /// A pending poll only registers the current waker. Cancelling the
    /// surrounding future therefore does not reserve or remove a queued owner.
    pub fn poll_receive_job(&mut self, context: &mut Context<'_>) -> Poll<InterfaceTxJob> {
        self.jobs.poll_receive(context)
    }

    /// Wait for this actor's oldest exact owner.
    ///
    /// This operation is cancellation-safe: dropping it while pending leaves
    /// every queued owner available to the next receive operation.
    pub async fn receive_job(&mut self) -> InterfaceTxJob {
        poll_fn(|context| self.poll_receive_job(context)).await
    }

    /// Return one exact completion without awaiting.
    ///
    /// Crossed queue capabilities and pressure both return the unchanged
    /// completion envelope.
    // The exact completion owner must be returned inline under pressure.
    #[allow(clippy::result_large_err)]
    pub fn try_send_completion(
        &mut self,
        completion: InterfaceTxCompletion,
    ) -> Result<(), ActorCompletionSendFailure> {
        if completion.context().lease().queue() != self.queue {
            return Err(ActorCompletionSendFailure {
                reason: ActorCompletionSendError::ForeignQueue {
                    expected: self.queue,
                    supplied: completion.context().lease().queue(),
                },
                completion,
            });
        }
        self.completions
            .try_send(completion)
            .map_err(
                |TrySendError::Full(completion)| ActorCompletionSendFailure {
                    reason: ActorCompletionSendError::QueueFull(self.queue),
                    completion,
                },
            )
    }

    /// Poll for advisory capacity in this actor's completion queue.
    ///
    /// Readiness neither reserves capacity nor moves an exact completion.
    /// An actor must retain its completion in persistent state and retry
    /// [`Self::try_send_completion`] after this poll reports ready.
    pub fn poll_completion_capacity(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.completions.poll_ready_to_send(context)
    }

    /// Wait cancellation-safely for advisory completion-queue capacity.
    ///
    /// This future owns no completion. Dropping it while pending or after a
    /// wake therefore leaves the actor's separately retained completion
    /// unchanged and does not reserve queue capacity.
    pub async fn wait_completion_capacity(&mut self) {
        poll_fn(|context| self.poll_completion_capacity(context)).await
    }

    /// Configured bounded depth of this actor's job and completion queues.
    pub const fn capacity(&self) -> usize {
        QUEUE_DEPTH
    }

    /// Current number of exact jobs waiting for this actor.
    pub fn pending_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// Current number of reusable ingress buffers waiting for this actor.
    pub fn available_ingress_buffers(&self) -> usize {
        self.available_ingress.len()
    }

    /// Current number of completed ingress packets waiting for the router.
    pub fn pending_ingress_packets(&self) -> usize {
        self.completed_ingress.len()
    }
}

/// TX-only concrete-actor capability for one fixed interface queue.
///
/// This is the outbound capability returned by
/// [`InterfaceActorHandoff::into_parts`] so a dedicated dispatcher need not
/// own any RX-buffer or lifecycle capability.
#[must_use = "dropping a TX actor handoff abandons its outbound queue capability"]
pub struct InterfaceTxActorHandoff<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    queue: InterfaceQueueId,
    jobs: &'static Channel<M, InterfaceTxJob, QUEUE_DEPTH>,
    completions: &'static Channel<M, InterfaceTxCompletion, QUEUE_DEPTH>,
}

impl<M, const QUEUE_DEPTH: usize> InterfaceTxActorHandoff<M, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Fixed queue identity owned by this TX capability.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.queue
    }

    /// Receive this actor's oldest exact outbound owner immediately.
    pub fn try_receive_job(&mut self) -> Option<InterfaceTxJob> {
        self.jobs.try_receive().ok()
    }

    /// Poll for this actor's oldest exact outbound owner.
    pub fn poll_receive_job(&mut self, context: &mut Context<'_>) -> Poll<InterfaceTxJob> {
        self.jobs.poll_receive(context)
    }

    /// Wait cancellation-safely for this actor's oldest outbound owner.
    pub async fn receive_job(&mut self) -> InterfaceTxJob {
        poll_fn(|context| self.poll_receive_job(context)).await
    }

    /// Return one exact completion without awaiting.
    #[allow(clippy::result_large_err)]
    pub fn try_send_completion(
        &mut self,
        completion: InterfaceTxCompletion,
    ) -> Result<(), ActorCompletionSendFailure> {
        if completion.context().lease().queue() != self.queue {
            return Err(ActorCompletionSendFailure {
                reason: ActorCompletionSendError::ForeignQueue {
                    expected: self.queue,
                    supplied: completion.context().lease().queue(),
                },
                completion,
            });
        }
        self.completions
            .try_send(completion)
            .map_err(
                |TrySendError::Full(completion)| ActorCompletionSendFailure {
                    reason: ActorCompletionSendError::QueueFull(self.queue),
                    completion,
                },
            )
    }

    /// Poll for advisory completion-queue capacity without moving an owner.
    pub fn poll_completion_capacity(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.completions.poll_ready_to_send(context)
    }

    /// Wait cancellation-safely for advisory completion-queue capacity.
    pub async fn wait_completion_capacity(&mut self) {
        poll_fn(|context| self.poll_completion_capacity(context)).await
    }

    /// Configured bounded outbound queue depth.
    pub const fn capacity(&self) -> usize {
        QUEUE_DEPTH
    }

    /// Current number of exact jobs waiting for this actor.
    pub fn pending_jobs(&self) -> usize {
        self.jobs.len()
    }
}

/// RX-only concrete-actor capability for one fixed interface queue.
///
/// The available-buffer receiver and completed-packet sender share one fixed
/// queue origin and cannot be cloned or constructed independently.
#[must_use = "dropping an ingress actor handoff abandons its RX pool capability"]
pub struct InterfaceIngressActorHandoff<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    queue: InterfaceQueueId,
    available: &'static Channel<M, AvailableIngressBuffer, QUEUE_DEPTH>,
    completed: &'static Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    origin: &'static IngressFabricOrigin,
}

impl<M, const QUEUE_DEPTH: usize> InterfaceIngressActorHandoff<M, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Fixed queue identity owned by this RX capability.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.queue
    }

    /// Bind a registry descriptor to this fixed actor's RX provenance.
    pub fn bind_ingress(
        &self,
        descriptor: InterfaceDescriptor,
    ) -> Result<InterfaceIngressAuthority, ActorIngressBindingError> {
        if descriptor.lease.queue != self.queue {
            return Err(ActorIngressBindingError::ForeignQueue {
                expected: self.queue,
                supplied: descriptor.lease.queue,
            });
        }
        Ok(InterfaceIngressAuthority { descriptor })
    }

    /// Receive one reusable mutable native-packet buffer immediately.
    pub fn try_receive_buffer(&mut self) -> Option<AvailableIngressBuffer> {
        self.available.try_receive().ok()
    }

    /// Poll for one reusable mutable native-packet buffer.
    pub fn poll_receive_buffer(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<AvailableIngressBuffer> {
        self.available.poll_receive(context)
    }

    /// Wait cancellation-safely for one reusable mutable packet buffer.
    pub async fn receive_buffer(&mut self) -> AvailableIngressBuffer {
        poll_fn(|context| self.poll_receive_buffer(context)).await
    }

    /// Submit one exact sealed native packet with actor-bound provenance.
    pub fn try_send(
        &mut self,
        authority: InterfaceIngressAuthority,
        packet: SealedIngressPacket,
    ) -> Result<(), ActorIngressSendFailure> {
        try_send_actor_ingress(self.queue, self.origin, self.completed, authority, packet)
    }

    /// Poll for advisory completed-packet queue capacity.
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.completed.poll_ready_to_send(context)
    }

    /// Wait cancellation-safely for advisory completed-packet capacity.
    pub async fn wait_ready_to_send(&mut self) {
        poll_fn(|context| self.poll_ready_to_send(context)).await
    }

    /// Configured reusable-buffer pool and completed-packet queue depth.
    pub const fn capacity(&self) -> usize {
        QUEUE_DEPTH
    }

    /// Current number of reusable buffers waiting for this actor.
    pub fn available_buffers(&self) -> usize {
        self.available.len()
    }

    /// Current number of completed packets waiting for the router.
    pub fn pending_packets(&self) -> usize {
        self.completed.len()
    }
}

fn try_send_actor_ingress<M, const QUEUE_DEPTH: usize>(
    expected: InterfaceQueueId,
    expected_origin: &'static IngressFabricOrigin,
    completed: &'static Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    authority: InterfaceIngressAuthority,
    packet: SealedIngressPacket,
) -> Result<(), ActorIngressSendFailure>
where
    M: RawMutex + 'static,
{
    let ingress = InterfaceIngress { authority, packet };
    let authority_queue = ingress.lease().queue();
    if authority_queue != expected {
        return Err(ActorIngressSendFailure {
            reason: ActorIngressSendError::ForeignAuthorityQueue {
                expected,
                supplied: authority_queue,
            },
            ingress,
        });
    }
    let buffer_queue = ingress.packet.id.queue;
    if buffer_queue != expected {
        return Err(ActorIngressSendFailure {
            reason: ActorIngressSendError::ForeignBufferQueue {
                expected,
                supplied: buffer_queue,
            },
            ingress,
        });
    }
    if !core::ptr::eq(ingress.packet.origin, expected_origin) {
        return Err(ActorIngressSendFailure {
            reason: ActorIngressSendError::ForeignFabricOrigin(expected),
            ingress,
        });
    }
    let maximum = ingress.authority.descriptor.logical_mtu();
    if ingress.packet.len() > usize::from(maximum.get()) {
        return Err(ActorIngressSendFailure {
            reason: ActorIngressSendError::PacketExceedsBoundMtu {
                actual: ingress.packet.len(),
                maximum,
            },
            ingress,
        });
    }
    completed
        .try_send(ingress)
        .map_err(|TrySendError::Full(ingress)| ActorIngressSendFailure {
            reason: ActorIngressSendError::QueueFull(expected),
            ingress,
        })
}

/// Failure to bind registry-issued ingress provenance to one concrete actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorIngressBindingError {
    /// Descriptor belongs to a different fixed actor queue.
    ForeignQueue {
        /// Queue permanently owned by this actor capability.
        expected: InterfaceQueueId,
        /// Queue named by the supplied registry descriptor.
        supplied: InterfaceQueueId,
    },
}

/// Why an actor could not submit one sealed native ingress packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorIngressSendError {
    /// Registry authority belongs to a different fixed actor queue.
    ForeignAuthorityQueue {
        /// Queue owned by the actor capability used for submission.
        expected: InterfaceQueueId,
        /// Queue stamped on the supplied registry authority.
        supplied: InterfaceQueueId,
    },
    /// Reusable packet buffer originated from a different fixed actor pool.
    ForeignBufferQueue {
        /// Queue owned by the actor capability used for submission.
        expected: InterfaceQueueId,
        /// Permanent origin stamped on the supplied buffer.
        supplied: InterfaceQueueId,
    },
    /// Packet buffer was seeded by a different static interface fabric.
    ForeignFabricOrigin(InterfaceQueueId),
    /// Sealed packet exceeds the descriptor MTU bound into this actor's
    /// ingress authority.
    PacketExceedsBoundMtu {
        /// Sealed native-packet length.
        actual: usize,
        /// Actor-bound logical MTU.
        maximum: LogicalMtu,
    },
    /// The bounded completed-packet queue is full.
    QueueFull(InterfaceQueueId),
}

/// Actor ingress-send failure retaining the exact sealed packet and authority.
#[must_use = "actor ingress failure retains an exact owning packet"]
pub struct ActorIngressSendFailure {
    reason: ActorIngressSendError,
    ingress: InterfaceIngress,
}

impl fmt::Debug for ActorIngressSendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActorIngressSendFailure")
            .field("reason", &self.reason)
            .field("buffer", &self.ingress.packet.id)
            .finish_non_exhaustive()
    }
}

impl ActorIngressSendFailure {
    /// Typed reason the exact packet was not enqueued.
    pub const fn reason(&self) -> ActorIngressSendError {
        self.reason
    }

    /// Recover the unchanged actor authority and exact sealed packet owner.
    pub fn into_parts(self) -> (InterfaceIngressAuthority, SealedIngressPacket) {
        (self.ingress.authority, self.ingress.packet)
    }
}

/// Why an actor could not return an exact completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorCompletionSendError {
    /// Completion ticket was issued for another fixed actor queue.
    ForeignQueue {
        /// Queue owned by the actor capability used for return.
        expected: InterfaceQueueId,
        /// Queue stamped on the completion ticket.
        supplied: InterfaceQueueId,
    },
    /// The bounded completion queue is full.
    QueueFull(InterfaceQueueId),
}

/// Actor completion-send failure retaining the unchanged exact owner.
#[must_use = "actor send failure retains an exact owning completion"]
pub struct ActorCompletionSendFailure {
    reason: ActorCompletionSendError,
    completion: InterfaceTxCompletion,
}

impl fmt::Debug for ActorCompletionSendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActorCompletionSendFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl ActorCompletionSendFailure {
    /// Typed reason the exact completion was not enqueued.
    pub const fn reason(&self) -> ActorCompletionSendError {
        self.reason
    }

    /// Recover the unchanged exact completion envelope.
    pub fn into_completion(self) -> InterfaceTxCompletion {
        self.completion
    }
}
