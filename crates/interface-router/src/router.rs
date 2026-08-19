use super::*;

/// Why an exact outbound job was not accepted by an interface actor queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteError {
    /// Selected Reticulum interface has no current registry record.
    UnknownInterface(PacketInterfaceId),
    /// Selected interface currently rejects new outbound work.
    Offline(InterfaceLease),
    /// Complete native packet exceeds the selected interface's logical MTU.
    PacketTooLarge {
        /// Selected generation-safe interface authority.
        lease: InterfaceLease,
        /// Complete native packet length.
        packet_len: u16,
        /// Logical interface limit.
        logical_mtu: LogicalMtu,
    },
    /// Selected actor's bounded job queue is full.
    QueueFull(InterfaceLease),
}

/// DATA routing failure retaining the unchanged node-core job.
#[must_use = "a DATA route failure retains its exact unique owner"]
pub struct DataRouteFailure {
    reason: RouteError,
    job: RoutedTxJob<'static>,
}

impl fmt::Debug for DataRouteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataRouteFailure")
            .field("reason", &self.reason)
            .field("prepared", &self.job.prepared())
            .field("interface", &self.job.interface())
            .finish()
    }
}

impl DataRouteFailure {
    /// Typed reason this job was not accepted.
    pub const fn reason(&self) -> RouteError {
        self.reason
    }

    /// Recover the unchanged exact DATA owner.
    pub fn into_job(self) -> RoutedTxJob<'static> {
        self.job
    }
}

/// Ordinary routing failure retaining the unchanged node-core job.
#[must_use = "an ordinary route failure retains its exact unique owner"]
pub struct OrdinaryRouteFailure {
    reason: RouteError,
    job: OrdinaryTxJob<'static>,
}

impl fmt::Debug for OrdinaryRouteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrdinaryRouteFailure")
            .field("reason", &self.reason)
            .field("prepared", &self.job.prepared())
            .finish()
    }
}

impl OrdinaryRouteFailure {
    /// Typed reason this job was not accepted.
    pub const fn reason(&self) -> RouteError {
        self.reason
    }

    /// Recover the unchanged exact ordinary owner.
    pub fn into_job(self) -> OrdinaryTxJob<'static> {
        self.job
    }
}

/// Scalar proof that one exact owner entered its selected interface queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchReceipt {
    context: InterfaceDispatchContext,
}

impl DispatchReceipt {
    /// Registry/configuration snapshot stamped onto the accepted owner.
    pub const fn context(self) -> InterfaceDispatchContext {
        self.context
    }
}

/// Why one completed native ingress packet cannot reach node-core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressRouteError {
    /// Registry authority names a different queue than the queue observed by
    /// the router's fair scan.
    ForeignAuthorityQueue {
        /// Queue from which the packet was dequeued.
        observed: InterfaceQueueId,
        /// Queue stamped on the actor's registry authority.
        supplied: InterfaceQueueId,
    },
    /// Reusable buffer belongs to a different fixed actor queue.
    ForeignBufferQueue {
        /// Queue from which the packet was dequeued.
        observed: InterfaceQueueId,
        /// Permanent queue origin stamped on the reusable buffer.
        supplied: InterfaceQueueId,
    },
    /// Reusable buffer was seeded by a different static interface fabric.
    ForeignFabricOrigin(InterfaceQueueId),
    /// Actor authority is vacant, outside capacity, or superseded.
    StaleLease(InterfaceLeaseError),
    /// Sealed packet exceeds the authoritative current registry MTU.
    PacketExceedsCurrentMtu {
        /// Sealed native-packet length.
        actual: usize,
        /// Current registry logical MTU.
        maximum: LogicalMtu,
    },
}

/// Ingress-routing failure retaining the exact sealed packet and provenance.
#[must_use = "a rejected ingress packet remains exactly owned"]
pub struct IngressRouteFailure {
    reason: IngressRouteError,
    ingress: InterfaceIngress,
}

impl fmt::Debug for IngressRouteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngressRouteFailure")
            .field("reason", &self.reason)
            .field("lease", &self.ingress.lease())
            .field("buffer", &self.ingress.packet.id)
            .finish_non_exhaustive()
    }
}

impl IngressRouteFailure {
    /// Typed reason this exact ingress packet was rejected.
    pub const fn reason(&self) -> IngressRouteError {
        self.reason
    }

    /// Recover the unchanged sealed packet for explicit recycling.
    pub fn into_packet(self) -> SealedIngressPacket {
        self.ingress.packet
    }
}

/// Why the sole router could not recycle one exact ingress buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressBufferReturnError {
    /// Buffer's fixed queue is outside this router's fabric capacity.
    QueueOutsideFabric(InterfaceQueueId),
    /// Buffer's queue-local slot is outside the configured pool depth.
    SlotOutsidePool(IngressBufferId),
    /// Buffer came from a different static interface fabric.
    ForeignFabricOrigin(InterfaceQueueId),
    /// Correctly paired pool accounting was violated because the actor's
    /// available-buffer queue was unexpectedly full.
    QueueFull(InterfaceQueueId),
}

/// Buffer-return failure retaining the exact sealed native packet owner.
#[must_use = "a failed ingress-buffer return retains the exact owner"]
pub struct IngressBufferReturnFailure {
    reason: IngressBufferReturnError,
    packet: SealedIngressPacket,
}

impl fmt::Debug for IngressBufferReturnFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngressBufferReturnFailure")
            .field("reason", &self.reason)
            .field("buffer", &self.packet.id)
            .finish_non_exhaustive()
    }
}

impl IngressBufferReturnFailure {
    /// Typed reason the exact reusable buffer was not returned.
    pub const fn reason(&self) -> IngressBufferReturnError {
        self.reason
    }

    /// Recover the unchanged sealed packet for explicit retry or quarantine.
    pub fn into_packet(self) -> SealedIngressPacket {
        self.packet
    }
}

/// Why a dequeued actor completion cannot be attributed to the current
/// authoritative registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionRouteError {
    /// Completion's ticket names a different fixed queue.
    ForeignQueue {
        /// Queue from which the completion was dequeued.
        observed: InterfaceQueueId,
        /// Queue stamped on the completion ticket.
        supplied: InterfaceQueueId,
    },
    /// Completion's lease is vacant, outside capacity, or superseded.
    StaleLease(InterfaceLeaseError),
}

/// Completion-routing failure retaining the exact owning completion.
#[must_use = "a stale or foreign completion remains exactly owned"]
pub struct CompletionRouteFailure {
    reason: CompletionRouteError,
    completion: InterfaceTxCompletion,
}

impl fmt::Debug for CompletionRouteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletionRouteFailure")
            .field("reason", &self.reason)
            .field("context", &self.completion.context())
            .finish_non_exhaustive()
    }
}

impl CompletionRouteFailure {
    /// Typed reason this completion was not attributed to the current lease.
    pub const fn reason(&self) -> CompletionRouteError {
        self.reason
    }

    /// Recover the unchanged exact interface completion envelope.
    pub fn into_completion(self) -> InterfaceTxCompletion {
        self.completion
    }

    /// Convert this rejected routing envelope into an explicit node-owner
    /// recovery value.
    ///
    /// A superseded interface generation must not strand a valid node-core
    /// owner. The permanent runtime must pass the returned completion to the
    /// matching DATA or ordinary owner even though the interface observation
    /// is stale. Node-core then performs its normal generation-safe
    /// reconciliation and may emit a serialized `Next` hop.
    pub fn into_node_recovery(self) -> CompletionRecovery {
        CompletionRecovery {
            reason: self.reason,
            completion: self.completion.into_outbound(),
        }
    }
}

/// Explicit node-reconciliation path for a stale or foreign interface
/// completion envelope.
#[must_use = "recovery completion must be reconciled by its node owner"]
pub struct CompletionRecovery {
    reason: CompletionRouteError,
    completion: OutboundCompletion,
}

impl CompletionRecovery {
    /// Interface-routing observation that required explicit reconciliation.
    pub const fn reason(&self) -> CompletionRouteError {
        self.reason
    }

    /// Recover the exact DATA or ordinary owning completion for node-core.
    pub fn into_outbound(self) -> OutboundCompletion {
        self.completion
    }
}

/// Sole transport-neutral outbound owner router and completion demultiplexer.
pub struct OutboundRouter<M, const SLOTS: usize, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    pub(crate) registry: InterfaceRegistry<SLOTS>,
    pub(crate) queues: [RouterQueue<M, QUEUE_DEPTH>; SLOTS],
    pub(crate) completion_cursor: usize,
    pub(crate) ingress_cursor: usize,
    pub(crate) lifecycle_cursor: usize,
}

impl<M, const SLOTS: usize, const QUEUE_DEPTH: usize> OutboundRouter<M, SLOTS, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Immutable authoritative interface registry.
    pub const fn registry(&self) -> &InterfaceRegistry<SLOTS> {
        &self.registry
    }

    /// Derive the authoritative online interface snapshot for node-core route
    /// resolution.
    pub fn eligible_interfaces(&self) -> Result<InterfaceSet, EligibleInterfaceSetError> {
        self.registry.eligible_interfaces()
    }

    /// Derive the exact currently-online egress set for an announce learned on
    /// one registered ingress interface.
    pub fn announce_egress_interfaces(
        &self,
        source: PacketInterfaceId,
    ) -> Result<InterfaceSet, AnnounceEgressSetError> {
        self.registry.announce_egress_interfaces(source)
    }

    /// Derive recursive unknown-path search egress for one registered ingress.
    pub fn recursive_path_search_egress_interfaces(
        &self,
        source: PacketInterfaceId,
    ) -> Result<Option<InterfaceSet>, RecursivePathSearchEgressSetError> {
        self.registry
            .recursive_path_search_egress_interfaces(source)
    }

    /// Consume, validate, apply or reject, and acknowledge at most one actor
    /// lifecycle request in bounded round-robin order.
    ///
    /// The acknowledgement queue is checked before removing the request, so a
    /// cancelled or wedged actor cannot cause a state transition whose exact
    /// result the fabric cannot retain. The router is the sole acknowledgement
    /// producer, making the subsequent send infallible while capacity remains
    /// reserved by this synchronous method.
    pub fn try_process_lifecycle(&mut self) -> Option<InterfaceLifecycleTransition> {
        for offset in 0..SLOTS {
            let index = (self.lifecycle_cursor + offset) % SLOTS;
            let queue = &self.queues[index];
            if queue.lifecycle_acknowledgements.is_full() {
                continue;
            }
            let Ok(request) = queue.lifecycle_requests.try_receive() else {
                continue;
            };
            self.lifecycle_cursor = (index + 1) % SLOTS;
            let observed = InterfaceQueueId(index as u16);
            let supplied = request.lease.queue;
            let result = if supplied != observed {
                Err(InterfaceLifecycleRouteError::ForeignQueue { observed, supplied })
            } else {
                self.registry
                    .set_online(request.lease, request.state.online())
                    .map_err(InterfaceLifecycleRouteError::InvalidLease)
            };
            let acknowledgement = InterfaceLifecycleAcknowledgement { request, result };
            match queue.lifecycle_acknowledgements.try_send(acknowledgement) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    unreachable!("sole lifecycle acknowledgement producer reserved capacity")
                }
            }
            return Some(InterfaceLifecycleTransition {
                queue: observed,
                acknowledgement,
            });
        }
        None
    }

    /// Dequeue and validate one exact completed native packet in bounded
    /// round-robin order.
    ///
    /// The observed queue, reusable-buffer origin, static fabric origin, and
    /// current registry lease must all agree. An online-to-offline transition
    /// remains valid because the actor already completed RX under this lease.
    /// Every rejection retains the exact sealed packet for recycling.
    #[allow(clippy::result_large_err)]
    pub fn try_receive_ingress(&mut self) -> Result<Option<ValidatedIngress>, IngressRouteFailure> {
        for offset in 0..SLOTS {
            let index = (self.ingress_cursor + offset) % SLOTS;
            let Ok(ingress) = self.queues[index].completed_ingress.try_receive() else {
                continue;
            };
            self.ingress_cursor = (index + 1) % SLOTS;
            return self.route_ingress(index, ingress).map(Some);
        }
        Ok(None)
    }

    /// Poll for one exact completed native packet in bounded round-robin
    /// order.
    ///
    /// A pending poll registers the current waker on every actor queue without
    /// reserving or removing any packet owner.
    #[allow(clippy::result_large_err)]
    pub fn poll_receive_ingress(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<ValidatedIngress, IngressRouteFailure>> {
        for offset in 0..SLOTS {
            let index = (self.ingress_cursor + offset) % SLOTS;
            let Poll::Ready(ingress) = self.queues[index].completed_ingress.poll_receive(context)
            else {
                continue;
            };
            self.ingress_cursor = (index + 1) % SLOTS;
            return Poll::Ready(self.route_ingress(index, ingress));
        }
        Poll::Pending
    }

    /// Wait cancellation-safely for the next exact completed native packet.
    #[allow(clippy::result_large_err)]
    pub async fn receive_ingress(&mut self) -> Result<ValidatedIngress, IngressRouteFailure> {
        poll_fn(|context| self.poll_receive_ingress(context)).await
    }

    /// Recycle one exact sealed packet buffer to its original fixed actor
    /// queue without awaiting.
    ///
    /// Correctly paired pool accounting guarantees capacity: one actor receive
    /// creates the only available-queue slot needed for its eventual return.
    /// The method remains typed and fail-closed so crossed fabrics or broken
    /// accounting never lose the non-`Copy` owner.
    #[allow(clippy::result_large_err)]
    pub fn try_return_ingress_buffer(
        &mut self,
        packet: SealedIngressPacket,
    ) -> Result<(), IngressBufferReturnFailure> {
        let id = packet.id;
        let Some(queue) = self.queues.get(id.queue.index()) else {
            return Err(IngressBufferReturnFailure {
                reason: IngressBufferReturnError::QueueOutsideFabric(id.queue),
                packet,
            });
        };
        if id.slot >= QUEUE_DEPTH {
            return Err(IngressBufferReturnFailure {
                reason: IngressBufferReturnError::SlotOutsidePool(id),
                packet,
            });
        }
        if !core::ptr::eq(packet.origin, queue.ingress_origin) {
            return Err(IngressBufferReturnFailure {
                reason: IngressBufferReturnError::ForeignFabricOrigin(id.queue),
                packet,
            });
        }
        let packet_len = packet.packet_len;
        let signal = packet.signal;
        match queue.available_ingress.try_send(packet.recycle()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(buffer)) => Err(IngressBufferReturnFailure {
                reason: IngressBufferReturnError::QueueFull(id.queue),
                packet: SealedIngressPacket {
                    id: buffer.id,
                    origin: buffer.origin,
                    storage: buffer.storage,
                    packet_len,
                    signal,
                },
            }),
        }
    }

    /// Register one stable interface identity in a vacant actor queue.
    pub fn register(
        &mut self,
        queue: InterfaceQueueId,
        interface: PacketInterfaceId,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceRegistrationError> {
        self.registry.register(queue, interface, properties, online)
    }

    /// Change whether a current interface accepts newly routed owners.
    pub fn set_online(
        &mut self,
        lease: InterfaceLease,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.registry.set_online(lease, online)
    }

    /// Replace MTU/configuration under a new queue-local generation.
    pub fn reconfigure(
        &mut self,
        lease: InterfaceLease,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.registry.reconfigure(lease, properties, online)
    }

    /// Remove one current registry lease.
    pub fn unregister(
        &mut self,
        lease: InterfaceLease,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.registry.unregister(lease)
    }

    /// Route one exact destination-DATA owner to its selected interface.
    pub fn try_route_data(
        &mut self,
        job: RoutedTxJob<'static>,
    ) -> Result<DispatchReceipt, DataRouteFailure> {
        let interface = job.interface();
        let packet_len = job.packet_len();
        let descriptor = match self.validate_route(interface, packet_len) {
            Ok(descriptor) => descriptor,
            Err(reason) => return Err(DataRouteFailure { reason, job }),
        };
        let context = InterfaceDispatchContext { descriptor };
        let message = InterfaceTxJob::Data(InterfaceDataJob {
            context,
            binding: DataOwnerBinding::from_job(&job),
            job,
        });
        let queue = &self.queues[descriptor.lease.queue.index()];
        match queue.jobs.try_send(message) {
            Ok(()) => Ok(DispatchReceipt { context }),
            Err(TrySendError::Full(InterfaceTxJob::Data(job))) => Err(DataRouteFailure {
                reason: RouteError::QueueFull(descriptor.lease),
                job: job.job,
            }),
            Err(TrySendError::Full(InterfaceTxJob::Ordinary(_))) => {
                unreachable!("DATA enqueue returned an ordinary owner")
            }
        }
    }

    /// Route one exact ordinary protocol-action owner to its selected
    /// interface.
    #[allow(
        clippy::result_large_err,
        reason = "route failure must return the exact ordinary packet owner inline"
    )]
    pub fn try_route_ordinary(
        &mut self,
        job: OrdinaryTxJob<'static>,
    ) -> Result<DispatchReceipt, OrdinaryRouteFailure> {
        let interface = job.interface();
        let packet_len = job.packet_len();
        let descriptor = match self.validate_route(interface, packet_len) {
            Ok(descriptor) => descriptor,
            Err(reason) => return Err(OrdinaryRouteFailure { reason, job }),
        };
        let context = InterfaceDispatchContext { descriptor };
        let binding = job.prepared();
        let message = InterfaceTxJob::Ordinary(InterfaceOrdinaryJob {
            context,
            binding,
            job,
        });
        let queue = &self.queues[descriptor.lease.queue.index()];
        match queue.jobs.try_send(message) {
            Ok(()) => Ok(DispatchReceipt { context }),
            Err(TrySendError::Full(InterfaceTxJob::Ordinary(job))) => Err(OrdinaryRouteFailure {
                reason: RouteError::QueueFull(descriptor.lease),
                job: job.job,
            }),
            Err(TrySendError::Full(InterfaceTxJob::Data(_))) => {
                unreachable!("ordinary enqueue returned a DATA owner")
            }
        }
    }

    /// Poll until the selected interface queue can accept one owner, or until
    /// current registry state proves that routing would fail.
    ///
    /// Readiness is advisory. The caller must still use `try_route_data` or
    /// `try_route_ordinary`, which revalidates the registry generation, online
    /// state, MTU, and queue capacity while moving the exact owner.
    pub fn poll_route_capacity(
        &mut self,
        interface: PacketInterfaceId,
        packet_len: u16,
        context: &mut Context<'_>,
    ) -> Poll<Result<InterfaceDescriptor, RouteError>> {
        let descriptor = match self.validate_route(interface, packet_len) {
            Ok(descriptor) => descriptor,
            Err(reason) => return Poll::Ready(Err(reason)),
        };
        let queue = &self.queues[descriptor.lease.queue.index()];
        queue
            .jobs
            .poll_ready_to_send(context)
            .map(|()| Ok(descriptor))
    }

    /// Wait cancellation-safely for advisory capacity on one selected
    /// interface queue.
    ///
    /// Dropping this future while pending only removes a waker registration;
    /// it neither reserves queue capacity nor moves a packet owner.
    pub async fn wait_route_capacity(
        &mut self,
        interface: PacketInterfaceId,
        packet_len: u16,
    ) -> Result<InterfaceDescriptor, RouteError> {
        poll_fn(|context| self.poll_route_capacity(interface, packet_len, context)).await
    }

    /// Dequeue one exact actor completion in bounded round-robin order.
    ///
    /// Offline state does not invalidate previously accepted jobs. A changed
    /// generation does, and returns a retained stale-completion failure.
    // A stale result must retain the exact non-Copy completion without heap
    // allocation so the node recovery path can reconcile it.
    #[allow(clippy::result_large_err)]
    pub fn try_receive_completion(
        &mut self,
    ) -> Result<Option<OutboundCompletion>, CompletionRouteFailure> {
        for offset in 0..SLOTS {
            let index = (self.completion_cursor + offset) % SLOTS;
            let Ok(completion) = self.queues[index].completions.try_receive() else {
                continue;
            };
            self.completion_cursor = (index + 1) % SLOTS;
            let observed = InterfaceQueueId(index as u16);
            let supplied = completion.context().lease().queue();
            if supplied != observed {
                return Err(CompletionRouteFailure {
                    reason: CompletionRouteError::ForeignQueue { observed, supplied },
                    completion,
                });
            }
            if let Err(reason) = self.registry.validate(completion.context().lease()) {
                return Err(CompletionRouteFailure {
                    reason: CompletionRouteError::StaleLease(reason),
                    completion,
                });
            }
            return Ok(Some(completion.into_outbound()));
        }
        Ok(None)
    }

    /// Poll for one exact actor completion in bounded round-robin order.
    ///
    /// A pending poll registers the current waker on every actor completion
    /// queue without reserving or removing an owner. A ready poll moves exactly
    /// one completion and performs the same lease validation as
    /// [`Self::try_receive_completion`].
    #[allow(clippy::result_large_err)]
    pub fn poll_receive_completion(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<OutboundCompletion, CompletionRouteFailure>> {
        for offset in 0..SLOTS {
            let index = (self.completion_cursor + offset) % SLOTS;
            let Poll::Ready(completion) = self.queues[index].completions.poll_receive(context)
            else {
                continue;
            };
            self.completion_cursor = (index + 1) % SLOTS;
            return Poll::Ready(self.route_completion(index, completion));
        }
        Poll::Pending
    }

    /// Wait cancellation-safely for the next exact actor completion.
    ///
    /// Dropping this future while pending leaves every completion queued. A
    /// stale completion remains an owning error and must take the explicit
    /// node-recovery path.
    #[allow(clippy::result_large_err)]
    pub async fn receive_completion(
        &mut self,
    ) -> Result<OutboundCompletion, CompletionRouteFailure> {
        poll_fn(|context| self.poll_receive_completion(context)).await
    }

    #[allow(clippy::result_large_err)]
    fn route_ingress(
        &self,
        index: usize,
        ingress: InterfaceIngress,
    ) -> Result<ValidatedIngress, IngressRouteFailure> {
        let observed = InterfaceQueueId(index as u16);
        let authority_queue = ingress.lease().queue();
        if authority_queue != observed {
            return Err(IngressRouteFailure {
                reason: IngressRouteError::ForeignAuthorityQueue {
                    observed,
                    supplied: authority_queue,
                },
                ingress,
            });
        }
        let buffer_queue = ingress.packet.id.queue;
        if buffer_queue != observed {
            return Err(IngressRouteFailure {
                reason: IngressRouteError::ForeignBufferQueue {
                    observed,
                    supplied: buffer_queue,
                },
                ingress,
            });
        }
        if !core::ptr::eq(ingress.packet.origin, self.queues[index].ingress_origin) {
            return Err(IngressRouteFailure {
                reason: IngressRouteError::ForeignFabricOrigin(observed),
                ingress,
            });
        }
        match self.registry.validate(ingress.authority.lease()) {
            Ok(descriptor) => {
                let maximum = descriptor.logical_mtu();
                if ingress.packet.len() > usize::from(maximum.get()) {
                    return Err(IngressRouteFailure {
                        reason: IngressRouteError::PacketExceedsCurrentMtu {
                            actual: ingress.packet.len(),
                            maximum,
                        },
                        ingress,
                    });
                }
                Ok(ValidatedIngress {
                    descriptor,
                    packet: ingress.packet,
                })
            }
            Err(reason) => Err(IngressRouteFailure {
                reason: IngressRouteError::StaleLease(reason),
                ingress,
            }),
        }
    }

    #[allow(clippy::result_large_err)]
    fn route_completion(
        &self,
        index: usize,
        completion: InterfaceTxCompletion,
    ) -> Result<OutboundCompletion, CompletionRouteFailure> {
        let observed = InterfaceQueueId(index as u16);
        let supplied = completion.context().lease().queue();
        if supplied != observed {
            return Err(CompletionRouteFailure {
                reason: CompletionRouteError::ForeignQueue { observed, supplied },
                completion,
            });
        }
        if let Err(reason) = self.registry.validate(completion.context().lease()) {
            return Err(CompletionRouteFailure {
                reason: CompletionRouteError::StaleLease(reason),
                completion,
            });
        }
        Ok(completion.into_outbound())
    }

    fn validate_route(
        &self,
        interface: PacketInterfaceId,
        packet_len: u16,
    ) -> Result<InterfaceDescriptor, RouteError> {
        let Some(descriptor) = self.registry.descriptor(interface) else {
            return Err(RouteError::UnknownInterface(interface));
        };
        if !descriptor.online {
            return Err(RouteError::Offline(descriptor.lease));
        }
        if packet_len > descriptor.logical_mtu().get() {
            return Err(RouteError::PacketTooLarge {
                lease: descriptor.lease,
                packet_len,
                logical_mtu: descriptor.logical_mtu(),
            });
        }
        Ok(descriptor)
    }
}
