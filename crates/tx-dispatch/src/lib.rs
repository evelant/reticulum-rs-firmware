//! Persistent packet-interface dispatch orchestration with no RF capability.
//!
//! [`NoRfTxDispatcher`] owns the non-`Clone` dispatcher handoff ports and keeps
//! each packet owner or control value in a compact persistent state enum. Its
//! only frame consumer is an internal scalar inspector: this crate has no
//! TX-capable radio driver, HAL, executor, timer, device-API, or pluggable
//! byte-sink dependency and cannot transmit. Node-core's transitive portable
//! RX/framing dependency is not a transmit capability.
//!
//! [`TxPermitServer`] owns the node side of the scalar permit exchange. It
//! invokes the caller's synchronous authorization policy at most once per
//! request—only for a validated live candidate—and retains a reply unchanged
//! while its channel is full.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::{future::poll_fn, mem, task::Poll};

use embassy_sync::blocking_mutex::raw::RawMutex;
use reticulum_node_core::{
    AttemptToken, AuthorizedTx, ExpiredAuthorizedTx, MonotonicMillis, NodeCore, PacketInterfaceId,
    PermitPendingTx, PermitResolution, RoutedTxJob, TxAuthorizationErrorKind,
    TxAuthorizationFailure, TxAuthorizationPolicy, TxCompletionCode, TxFrameError, TxLeaseDeadline,
    TxPermitReply, TxPermitRequest, UnpermittedTx,
};
use reticulum_tx_handoff::{
    DispatcherHandoff, JobSender, NodeHandoff, OwnerReturnReceiver, PermitReplySender,
    PermitRequestReceiver, TxOwnerReturn,
};

/// Caller-owned completion-code namespace used by the dispatcher.
///
/// Node-core treats these as bounded diagnostics; conservative transmission
/// classification comes from the owning typestate, not from numeric values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxDispatcherCompletionCodes {
    /// Authorized completion after the fixed no-RF inspector borrowed bytes.
    pub no_rf_inspection: TxCompletionCode,
    /// Completion for a hop denied before authorization.
    pub unpermitted: TxCompletionCode,
    /// Authorized completion after grant or frame deadline expiry.
    pub expired_authorization: TxCompletionCode,
    /// Recovery completion for a broken or unresponsive permit exchange.
    pub control_plane_recovery: TxCompletionCode,
    /// Recovery completion for a one-shot frame invariant.
    pub frame_invariant_recovery: TxCompletionCode,
}

impl TxDispatcherCompletionCodes {
    /// Construct an explicit project-owned completion-code mapping.
    pub const fn new(
        no_rf_inspection: TxCompletionCode,
        unpermitted: TxCompletionCode,
        expired_authorization: TxCompletionCode,
        control_plane_recovery: TxCompletionCode,
        frame_invariant_recovery: TxCompletionCode,
    ) -> Self {
        Self {
            no_rf_inspection,
            unpermitted,
            expired_authorization,
            control_plane_recovery,
            frame_invariant_recovery,
        }
    }
}

/// Configuration for the RF-inert dispatcher machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoRfTxDispatcherConfig {
    /// Additional time after the packet-owner deadline to retain an outstanding
    /// permit exchange for an exact late reply.
    ///
    /// The caller must still drive node-core deadline maintenance at the owner
    /// deadline. On the first step sampling at or after the resulting grace
    /// threshold, any reply observable by that step wins; if none is
    /// observable, the dispatcher returns its exact pending owner as a recovery
    /// fault, deliberately forcing quarantine instead of guessing whether
    /// authorization occurred. Addition saturates at `u64::MAX`.
    pub permit_recovery_grace_ms: u64,
    /// Explicit caller-owned completion-code mapping.
    pub completion_codes: TxDispatcherCompletionCodes,
}

impl NoRfTxDispatcherConfig {
    /// Construct a configuration with explicit grace and completion codes.
    pub const fn new(
        permit_recovery_grace_ms: u64,
        completion_codes: TxDispatcherCompletionCodes,
    ) -> Self {
        Self {
            permit_recovery_grace_ms,
            completion_codes,
        }
    }
}

#[derive(Clone, Copy)]
struct DispatchMeta {
    owner_deadline: TxLeaseDeadline,
    grace_deadline: MonotonicMillis,
}

impl DispatchMeta {
    fn from_job(job: &RoutedTxJob<'_>, grace_ms: u64) -> Self {
        let owner_deadline = job.deadline();
        Self {
            owner_deadline,
            grace_deadline: MonotonicMillis::new(
                owner_deadline.instant().get().saturating_add(grace_ms),
            ),
        }
    }
}

enum RetainedControl {
    None,
    UnsentRequest(TxPermitRequest),
    MismatchedReply(TxPermitReply),
    OrphanReply(TxPermitReply),
}

impl RetainedControl {
    fn kind(&self) -> Option<TxDispatcherFaultResidueKind> {
        match self {
            Self::None => None,
            Self::UnsentRequest(request) => {
                let _ = request;
                Some(TxDispatcherFaultResidueKind::UnsentPermitRequest)
            }
            Self::MismatchedReply(reply) => {
                let _ = reply;
                Some(TxDispatcherFaultResidueKind::MismatchedPermitReply)
            }
            Self::OrphanReply(reply) => {
                let _ = reply;
                Some(TxDispatcherFaultResidueKind::OrphanPermitReply)
            }
        }
    }
}

enum AfterReturn {
    Resume,
    Disable {
        fault: TxDispatcherFault,
        retained: RetainedControl,
    },
}

enum DispatcherState {
    Idle,
    Job {
        job: RoutedTxJob<'static>,
        meta: DispatchMeta,
    },
    PermitSend {
        pending: PermitPendingTx<'static>,
        request: TxPermitRequest,
        meta: DispatchMeta,
    },
    PermitWait {
        pending: PermitPendingTx<'static>,
        meta: DispatchMeta,
    },
    PermitReply {
        pending: PermitPendingTx<'static>,
        reply: TxPermitReply,
        meta: DispatchMeta,
    },
    Authorized {
        owner: AuthorizedTx<'static>,
        meta: DispatchMeta,
    },
    Expired {
        owner: ExpiredAuthorizedTx<'static>,
        meta: DispatchMeta,
    },
    Unpermitted {
        owner: UnpermittedTx<'static>,
        meta: DispatchMeta,
    },
    Return {
        returned: TxOwnerReturn,
        after: AfterReturn,
        meta: DispatchMeta,
    },
    Disabled {
        fault: TxDispatcherFault,
        retained: RetainedControl,
    },
    Transitioning,
}

/// Persistent dispatcher phase, containing no packet bytes or owning values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoRfTxDispatcherPhase {
    /// No routed packet is currently owned by the dispatcher.
    Idle,
    /// A routed job is stored before permit negotiation.
    Job,
    /// A pending owner and request are stored until the request channel accepts
    /// them.
    PermitSend,
    /// A pending owner is stored while its request is owned by the node side.
    PermitWait,
    /// A pending owner and received reply await synchronous resolution.
    PermitReply,
    /// An authorized owner awaits one synchronous no-RF inspection.
    Authorized,
    /// An authorized owner whose grant arrived too late cannot expose bytes.
    Expired,
    /// A definitely-unpermitted owner awaits completion conversion.
    Unpermitted,
    /// An exact owning return is stored until the node-side channel accepts it.
    Return,
    /// A fail-closed invariant permanently disabled this machine.
    Disabled,
}

/// Channel whose synchronous send is currently backpressured.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxDispatcherChannel {
    /// Scalar permit-request channel.
    PermitRequest,
    /// Owning completion/available-buffer return channel.
    OwnerReturn,
}

/// Fail-closed reason retained by a disabled dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxDispatcherFault {
    /// An unsent permit request could not enter its channel before the recovery
    /// grace deadline.
    PermitRequestGraceExpired,
    /// No permit reply was observable when recovery was sampled at or after
    /// the grace threshold.
    PermitReplyGraceExpired,
    /// A reply belonged to a different pending owner.
    PermitReplyMismatch,
    /// A permit reply existed while no permit exchange was active.
    OrphanPermitReply,
    /// A supposedly fresh authorized owner reported that its frame had already
    /// been taken.
    FrameAlreadyTaken,
    /// Authorized owner, grant, and buffer metadata disagreed.
    FrameInvariant,
    /// Private phase and storage invariants disagreed.
    InternalInvariant,
}

/// Kind of non-`Copy` scalar retained forever by a disabled dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxDispatcherFaultResidueKind {
    /// A request proven not to have entered the permit channel.
    UnsentPermitRequest,
    /// A received reply that did not match the sole pending owner.
    MismatchedPermitReply,
    /// A reply received with no pending owner.
    OrphanPermitReply,
}

/// Scalar observation made by the fixed internal no-RF frame inspector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoRfFrameObservation {
    /// Complete proof-correlation hash bound to the borrowed frame.
    pub attempt: AttemptToken,
    /// Packet interface authorized for this exact hop.
    pub interface: PacketInterfaceId,
    /// Number of encoded Reticulum bytes inspected.
    pub packet_len: usize,
    /// Non-cryptographic wrapping byte sum used only for deterministic tests
    /// and diagnostics.
    pub wrapping_checksum: u8,
}

/// Result of one synchronous dispatcher-machine transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "dispatcher progress and backpressure must be handled"]
pub enum NoRfTxDispatcherStep {
    /// One transition completed; call [`NoRfTxDispatcher::step`] again with a
    /// fresh clock sample if cooperative scheduling permits.
    Advanced,
    /// No routed job is currently queued.
    NeedJob,
    /// The pending owner remains stored while waiting for a permit reply.
    NeedPermitReply {
        /// Recovery threshold for the outstanding control exchange.
        grace_deadline: MonotonicMillis,
    },
    /// A full synchronous channel returned its exact value, which remains
    /// stored in the machine.
    Backpressured(TxDispatcherChannel),
    /// One authorized frame was borrowed by the fixed no-RF inspector and an
    /// owning completion is now stored for return.
    Inspected(NoRfFrameObservation),
    /// The machine is permanently disabled and retains its fault state.
    Disabled(TxDispatcherFault),
}

/// Result of a short cancellation-safe dispatcher input wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "the caller must resume synchronous stepping after input is stored"]
pub enum NoRfTxDispatcherWait {
    /// A routed job was stored in persistent machine state.
    JobStored,
    /// A permit reply was stored alongside its persistent pending owner.
    PermitReplyStored,
    /// The current phase has no channel receive to await.
    NotWaiting,
    /// The machine was already disabled or found an orphan reply while idle.
    Disabled(TxDispatcherFault),
}

/// RF-inert persistent packet-interface dispatcher.
///
/// This type owns the sole dispatcher handoff capabilities. Its compact state
/// stores only one active typestate and contains no packet-sized array: jobs
/// and completions retain unique references to external static buffers. Store
/// the runtime itself outside an executor task future (normally in a
/// `StaticCell`) and let a permanent supervisor borrow it. Cancelling the whole
/// top-level task is not a supported hot-restart mechanism; only short waits
/// returned by [`Self::wait_for_input`] are cancellation-safe.
#[must_use = "dropping the dispatcher abandons its unique ports and any retained owner"]
pub struct NoRfTxDispatcher<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    handoff: DispatcherHandoff<M, POOL_SIZE>,
    config: NoRfTxDispatcherConfig,
    state: DispatcherState,
    reply_inbox: Option<TxPermitReply>,
}

impl<M, const POOL_SIZE: usize> NoRfTxDispatcher<M, POOL_SIZE>
where
    M: RawMutex + 'static,
{
    /// Consume the sole dispatcher ports into an empty persistent machine.
    pub fn new(handoff: DispatcherHandoff<M, POOL_SIZE>, config: NoRfTxDispatcherConfig) -> Self {
        Self {
            handoff,
            config,
            state: DispatcherState::Idle,
            reply_inbox: None,
        }
    }

    /// Current scalar phase.
    pub fn phase(&self) -> NoRfTxDispatcherPhase {
        match &self.state {
            DispatcherState::Idle => NoRfTxDispatcherPhase::Idle,
            DispatcherState::Job { .. } => NoRfTxDispatcherPhase::Job,
            DispatcherState::PermitSend { .. } => NoRfTxDispatcherPhase::PermitSend,
            DispatcherState::PermitWait { .. } if self.reply_inbox.is_some() => {
                NoRfTxDispatcherPhase::PermitReply
            }
            DispatcherState::PermitWait { .. } => NoRfTxDispatcherPhase::PermitWait,
            DispatcherState::PermitReply { .. } => NoRfTxDispatcherPhase::PermitReply,
            DispatcherState::Authorized { .. } => NoRfTxDispatcherPhase::Authorized,
            DispatcherState::Expired { .. } => NoRfTxDispatcherPhase::Expired,
            DispatcherState::Unpermitted { .. } => NoRfTxDispatcherPhase::Unpermitted,
            DispatcherState::Return { .. } => NoRfTxDispatcherPhase::Return,
            DispatcherState::Disabled { .. } | DispatcherState::Transitioning => {
                NoRfTxDispatcherPhase::Disabled
            }
        }
    }

    /// Retained fail-closed reason, including one pending return that will
    /// disable the machine once accepted.
    pub const fn fault(&self) -> Option<TxDispatcherFault> {
        match &self.state {
            DispatcherState::Return {
                after: AfterReturn::Disable { fault, .. },
                ..
            }
            | DispatcherState::Disabled { fault, .. } => Some(*fault),
            DispatcherState::Idle
            | DispatcherState::Job { .. }
            | DispatcherState::PermitSend { .. }
            | DispatcherState::PermitWait { .. }
            | DispatcherState::PermitReply { .. }
            | DispatcherState::Authorized { .. }
            | DispatcherState::Expired { .. }
            | DispatcherState::Unpermitted { .. }
            | DispatcherState::Return { .. }
            | DispatcherState::Transitioning => None,
        }
    }

    /// Kind of unmatched non-`Copy` control value retained after a fault.
    pub fn fault_residue_kind(&self) -> Option<TxDispatcherFaultResidueKind> {
        match &self.state {
            DispatcherState::Return {
                after: AfterReturn::Disable { retained, .. },
                ..
            }
            | DispatcherState::Disabled { retained, .. } => retained.kind(),
            DispatcherState::Idle
            | DispatcherState::Job { .. }
            | DispatcherState::PermitSend { .. }
            | DispatcherState::PermitWait { .. }
            | DispatcherState::PermitReply { .. }
            | DispatcherState::Authorized { .. }
            | DispatcherState::Expired { .. }
            | DispatcherState::Unpermitted { .. }
            | DispatcherState::Return { .. }
            | DispatcherState::Transitioning => None,
        }
    }

    /// Owner deadline for the active dispatch, if one is retained.
    pub fn owner_deadline(&self) -> Option<TxLeaseDeadline> {
        self.meta().map(|meta| meta.owner_deadline)
    }

    /// Control-exchange recovery deadline for the active dispatch, if one is
    /// retained.
    pub fn grace_deadline(&self) -> Option<MonotonicMillis> {
        self.meta().map(|meta| meta.grace_deadline)
    }

    const fn meta(&self) -> Option<DispatchMeta> {
        match self.state {
            DispatcherState::Job { meta, .. }
            | DispatcherState::PermitSend { meta, .. }
            | DispatcherState::PermitWait { meta, .. }
            | DispatcherState::PermitReply { meta, .. }
            | DispatcherState::Authorized { meta, .. }
            | DispatcherState::Expired { meta, .. }
            | DispatcherState::Unpermitted { meta, .. }
            | DispatcherState::Return { meta, .. } => Some(meta),
            DispatcherState::Idle
            | DispatcherState::Disabled { .. }
            | DispatcherState::Transitioning => None,
        }
    }

    /// Run exactly one non-awaiting state transition with a fresh monotonic
    /// sample.
    ///
    /// Every consuming typestate transition and full-channel recovery finishes
    /// before this method returns. No owner is held in a future-local value.
    pub fn step(&mut self, now: MonotonicMillis) -> NoRfTxDispatcherStep {
        let state = mem::replace(&mut self.state, DispatcherState::Transitioning);
        match state {
            DispatcherState::Idle => self.step_idle(),
            DispatcherState::Job { job, meta } => self.step_job(job, meta, now),
            DispatcherState::PermitSend {
                pending,
                request,
                meta,
            } => self.step_permit_send(pending, request, meta, now),
            DispatcherState::PermitWait { pending, meta } => {
                self.step_permit_wait(pending, meta, now)
            }
            DispatcherState::PermitReply {
                pending,
                reply,
                meta,
            } => self.step_permit_reply(pending, reply, meta, now),
            DispatcherState::Authorized { owner, meta } => self.step_authorized(owner, meta, now),
            DispatcherState::Expired { owner, meta } => self.step_expired(owner, meta),
            DispatcherState::Unpermitted { owner, meta } => self.step_unpermitted(owner, meta),
            DispatcherState::Return {
                returned,
                after,
                meta,
            } => self.step_return(returned, after, meta),
            DispatcherState::Disabled { fault, retained } => {
                self.state = DispatcherState::Disabled { fault, retained };
                NoRfTxDispatcherStep::Disabled(fault)
            }
            DispatcherState::Transitioning => {
                let fault = TxDispatcherFault::InternalInvariant;
                self.state = DispatcherState::Disabled {
                    fault,
                    retained: RetainedControl::None,
                };
                NoRfTxDispatcherStep::Disabled(fault)
            }
        }
    }

    /// Await only the channel input required by the current phase and store it
    /// directly into persistent machine state.
    ///
    /// This future returns no owning value. If cancelled while pending, Embassy
    /// leaves each item queued. While idle it polls both jobs and unexpected
    /// replies, giving an already-observable orphan reply priority. Once a
    /// receive becomes ready, assignment to persistent state occurs in the same
    /// poll before this future reports readiness. Callers must race a
    /// permit-reply wait against their own recovery timer and then call
    /// [`Self::step`] with a newly sampled clock. Do not cancel the permanent
    /// supervisor that owns this runtime.
    pub async fn wait_for_input(&mut self) -> NoRfTxDispatcherWait {
        match self.phase() {
            NoRfTxDispatcherPhase::Idle => {
                if let Some(reply) = self.reply_inbox.take() {
                    let fault = TxDispatcherFault::OrphanPermitReply;
                    self.state = DispatcherState::Disabled {
                        fault,
                        retained: RetainedControl::OrphanReply(reply),
                    };
                    return NoRfTxDispatcherWait::Disabled(fault);
                }
                poll_fn(|context| {
                    if let Poll::Ready(reply) = self.handoff.permit_replies.poll_receive(context) {
                        let fault = TxDispatcherFault::OrphanPermitReply;
                        self.state = DispatcherState::Disabled {
                            fault,
                            retained: RetainedControl::OrphanReply(reply),
                        };
                        return Poll::Ready(NoRfTxDispatcherWait::Disabled(fault));
                    }
                    match self.handoff.jobs.poll_receive(context) {
                        Poll::Ready(job) => {
                            let meta =
                                DispatchMeta::from_job(&job, self.config.permit_recovery_grace_ms);
                            self.state = DispatcherState::Job { job, meta };
                            Poll::Ready(NoRfTxDispatcherWait::JobStored)
                        }
                        Poll::Pending => Poll::Pending,
                    }
                })
                .await
            }
            NoRfTxDispatcherPhase::PermitWait => {
                let reply = self.handoff.permit_replies.receive().await;
                self.reply_inbox = Some(reply);
                NoRfTxDispatcherWait::PermitReplyStored
            }
            NoRfTxDispatcherPhase::Disabled => NoRfTxDispatcherWait::Disabled(
                self.fault().unwrap_or(TxDispatcherFault::InternalInvariant),
            ),
            NoRfTxDispatcherPhase::Job
            | NoRfTxDispatcherPhase::PermitSend
            | NoRfTxDispatcherPhase::PermitReply
            | NoRfTxDispatcherPhase::Authorized
            | NoRfTxDispatcherPhase::Expired
            | NoRfTxDispatcherPhase::Unpermitted
            | NoRfTxDispatcherPhase::Return => NoRfTxDispatcherWait::NotWaiting,
        }
    }

    fn step_idle(&mut self) -> NoRfTxDispatcherStep {
        if let Some(reply) = self
            .reply_inbox
            .take()
            .or_else(|| self.handoff.permit_replies.try_receive())
        {
            let fault = TxDispatcherFault::OrphanPermitReply;
            self.state = DispatcherState::Disabled {
                fault,
                retained: RetainedControl::OrphanReply(reply),
            };
            return NoRfTxDispatcherStep::Disabled(fault);
        }
        match self.handoff.jobs.try_receive() {
            Some(job) => {
                let meta = DispatchMeta::from_job(&job, self.config.permit_recovery_grace_ms);
                self.state = DispatcherState::Job { job, meta };
                NoRfTxDispatcherStep::Advanced
            }
            None => {
                self.state = DispatcherState::Idle;
                NoRfTxDispatcherStep::NeedJob
            }
        }
    }

    fn step_job(
        &mut self,
        job: RoutedTxJob<'static>,
        meta: DispatchMeta,
        now: MonotonicMillis,
    ) -> NoRfTxDispatcherStep {
        if now >= meta.owner_deadline.instant() {
            self.state = DispatcherState::Unpermitted {
                owner: job.return_unpermitted(),
                meta,
            };
        } else {
            let (pending, request) = job.begin_permit();
            self.state = DispatcherState::PermitSend {
                pending,
                request,
                meta,
            };
        }
        NoRfTxDispatcherStep::Advanced
    }

    fn step_permit_send(
        &mut self,
        pending: PermitPendingTx<'static>,
        request: TxPermitRequest,
        meta: DispatchMeta,
        now: MonotonicMillis,
    ) -> NoRfTxDispatcherStep {
        if now >= meta.grace_deadline {
            self.state = DispatcherState::Return {
                returned: pending
                    .recovery_fault(self.config.completion_codes.control_plane_recovery)
                    .into(),
                after: AfterReturn::Disable {
                    fault: TxDispatcherFault::PermitRequestGraceExpired,
                    retained: RetainedControl::UnsentRequest(request),
                },
                meta,
            };
            return NoRfTxDispatcherStep::Advanced;
        }
        match self.handoff.permit_requests.try_send(request) {
            Ok(()) => {
                self.state = DispatcherState::PermitWait { pending, meta };
                NoRfTxDispatcherStep::Advanced
            }
            Err(full) => {
                self.state = DispatcherState::PermitSend {
                    pending,
                    request: full.into_inner(),
                    meta,
                };
                NoRfTxDispatcherStep::Backpressured(TxDispatcherChannel::PermitRequest)
            }
        }
    }

    fn step_permit_wait(
        &mut self,
        pending: PermitPendingTx<'static>,
        meta: DispatchMeta,
        now: MonotonicMillis,
    ) -> NoRfTxDispatcherStep {
        if let Some(reply) = self
            .reply_inbox
            .take()
            .or_else(|| self.handoff.permit_replies.try_receive())
        {
            self.state = DispatcherState::PermitReply {
                pending,
                reply,
                meta,
            };
            NoRfTxDispatcherStep::Advanced
        } else if now >= meta.grace_deadline {
            self.state = DispatcherState::Return {
                returned: pending
                    .recovery_fault(self.config.completion_codes.control_plane_recovery)
                    .into(),
                after: AfterReturn::Disable {
                    fault: TxDispatcherFault::PermitReplyGraceExpired,
                    retained: RetainedControl::None,
                },
                meta,
            };
            NoRfTxDispatcherStep::Advanced
        } else {
            self.state = DispatcherState::PermitWait { pending, meta };
            NoRfTxDispatcherStep::NeedPermitReply {
                grace_deadline: meta.grace_deadline,
            }
        }
    }

    fn step_permit_reply(
        &mut self,
        pending: PermitPendingTx<'static>,
        reply: TxPermitReply,
        meta: DispatchMeta,
        now: MonotonicMillis,
    ) -> NoRfTxDispatcherStep {
        self.state = match pending.resolve(reply, now) {
            Ok(PermitResolution::Authorized(owner)) => DispatcherState::Authorized { owner, meta },
            Ok(PermitResolution::Expired(owner)) => DispatcherState::Expired { owner, meta },
            Ok(PermitResolution::Unpermitted(owner)) => {
                DispatcherState::Unpermitted { owner, meta }
            }
            Err(mismatch) => {
                let (pending, reply) = mismatch.into_parts();
                DispatcherState::Return {
                    returned: pending
                        .recovery_fault(self.config.completion_codes.control_plane_recovery)
                        .into(),
                    after: AfterReturn::Disable {
                        fault: TxDispatcherFault::PermitReplyMismatch,
                        retained: RetainedControl::MismatchedReply(reply),
                    },
                    meta,
                }
            }
        };
        NoRfTxDispatcherStep::Advanced
    }

    fn step_authorized(
        &mut self,
        mut owner: AuthorizedTx<'static>,
        meta: DispatchMeta,
        now: MonotonicMillis,
    ) -> NoRfTxDispatcherStep {
        let inspected = match owner.frame(now) {
            Ok(frame) => Ok(NoRfFrameObservation {
                attempt: frame.attempt(),
                interface: frame.interface(),
                packet_len: frame.bytes().len(),
                wrapping_checksum: frame
                    .bytes()
                    .iter()
                    .fold(0, |checksum, byte| checksum.wrapping_add(*byte)),
            }),
            Err(error) => Err(error),
        };
        let (returned, after, result) = match inspected {
            Ok(observation) => (
                owner
                    .complete(self.config.completion_codes.no_rf_inspection)
                    .into(),
                AfterReturn::Resume,
                NoRfTxDispatcherStep::Inspected(observation),
            ),
            Err(TxFrameError::DeadlineExpired { .. }) => (
                owner
                    .complete(self.config.completion_codes.expired_authorization)
                    .into(),
                AfterReturn::Resume,
                NoRfTxDispatcherStep::Advanced,
            ),
            Err(TxFrameError::AlreadyTaken) => (
                owner
                    .recovery_fault(self.config.completion_codes.frame_invariant_recovery)
                    .into(),
                AfterReturn::Disable {
                    fault: TxDispatcherFault::FrameAlreadyTaken,
                    retained: RetainedControl::None,
                },
                NoRfTxDispatcherStep::Advanced,
            ),
            Err(TxFrameError::Invariant) => (
                owner
                    .recovery_fault(self.config.completion_codes.frame_invariant_recovery)
                    .into(),
                AfterReturn::Disable {
                    fault: TxDispatcherFault::FrameInvariant,
                    retained: RetainedControl::None,
                },
                NoRfTxDispatcherStep::Advanced,
            ),
        };
        self.state = DispatcherState::Return {
            returned,
            after,
            meta,
        };
        result
    }

    fn step_expired(
        &mut self,
        owner: ExpiredAuthorizedTx<'static>,
        meta: DispatchMeta,
    ) -> NoRfTxDispatcherStep {
        self.state = DispatcherState::Return {
            returned: owner
                .complete(self.config.completion_codes.expired_authorization)
                .into(),
            after: AfterReturn::Resume,
            meta,
        };
        NoRfTxDispatcherStep::Advanced
    }

    fn step_unpermitted(
        &mut self,
        owner: UnpermittedTx<'static>,
        meta: DispatchMeta,
    ) -> NoRfTxDispatcherStep {
        self.state = DispatcherState::Return {
            returned: owner
                .complete(self.config.completion_codes.unpermitted)
                .into(),
            after: AfterReturn::Resume,
            meta,
        };
        NoRfTxDispatcherStep::Advanced
    }

    fn step_return(
        &mut self,
        returned: TxOwnerReturn,
        after: AfterReturn,
        meta: DispatchMeta,
    ) -> NoRfTxDispatcherStep {
        match self.handoff.returns.try_send(returned) {
            Ok(()) => match after {
                AfterReturn::Resume => {
                    self.state = DispatcherState::Idle;
                    NoRfTxDispatcherStep::Advanced
                }
                AfterReturn::Disable { fault, retained } => {
                    self.state = DispatcherState::Disabled { fault, retained };
                    NoRfTxDispatcherStep::Disabled(fault)
                }
            },
            Err(full) => {
                self.state = DispatcherState::Return {
                    returned: full.into_inner(),
                    after,
                    meta,
                };
                NoRfTxDispatcherStep::Backpressured(TxDispatcherChannel::OwnerReturn)
            }
        }
    }
}

/// Node-actor ports left after the permit service takes the scalar control
/// channels.
#[must_use = "dropping node data ports abandons unique jobs or owner returns"]
pub struct NodeTxDataHandoff<M, const POOL_SIZE: usize>
where
    M: RawMutex + 'static,
{
    /// Sole node-side producer of routed jobs.
    pub jobs: JobSender<M, POOL_SIZE>,
    /// Sole node-side consumer of owning dispatcher returns.
    pub returns: OwnerReturnReceiver<M, POOL_SIZE>,
}

enum PermitServerState {
    Idle,
    Request(TxPermitRequest),
    Reply(TxPermitReply),
    Disabled(TxAuthorizationFailure),
    Poisoned,
}

/// Persistent permit-service phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxPermitServerPhase {
    /// No scalar request is retained.
    Idle,
    /// One request is stored for a fresh authorization sample.
    Request,
    /// One reply is stored until the dispatcher accepts it.
    Reply,
    /// Request validation or a private invariant failed.
    Disabled,
}

/// Result of one synchronous permit-service transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "permit-service progress and faults must be handled"]
pub enum TxPermitServerStep {
    /// One transition completed.
    Advanced,
    /// No permit request is queued.
    NeedRequest,
    /// The reply channel returned the exact reply and it remains stored.
    ReplyBackpressured,
    /// Request validation failed and the exact request remains retained.
    Disabled(TxAuthorizationErrorKind),
    /// Private service state was internally inconsistent.
    InternalInvariant,
}

/// Result of the short cancellation-safe permit-request wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "the caller must resume permit-service stepping"]
pub enum TxPermitServerWait {
    /// One request was stored in persistent service state.
    RequestStored,
    /// The current phase has no request receive to await.
    NotWaiting,
    /// The service was already disabled by request validation.
    Disabled(TxAuthorizationErrorKind),
    /// Private service state was internally inconsistent.
    InternalInvariant,
}

/// Node-side persistent server for scalar permit requests and replies.
#[must_use = "dropping the permit server abandons its ports and retained control value"]
pub struct TxPermitServer<M>
where
    M: RawMutex + 'static,
{
    requests: PermitRequestReceiver<M>,
    replies: PermitReplySender<M>,
    state: PermitServerState,
}

impl<M> TxPermitServer<M>
where
    M: RawMutex + 'static,
{
    /// Split complete node handoff roles into DATA ownership ports and the
    /// scalar permit service.
    pub fn from_node_handoff<const POOL_SIZE: usize>(
        handoff: NodeHandoff<M, POOL_SIZE>,
    ) -> (NodeTxDataHandoff<M, POOL_SIZE>, Self) {
        let NodeHandoff {
            jobs,
            returns,
            permit_requests,
            permit_replies,
        } = handoff;
        (
            NodeTxDataHandoff { jobs, returns },
            Self {
                requests: permit_requests,
                replies: permit_replies,
                state: PermitServerState::Idle,
            },
        )
    }

    /// Current scalar phase.
    pub const fn phase(&self) -> TxPermitServerPhase {
        match self.state {
            PermitServerState::Idle => TxPermitServerPhase::Idle,
            PermitServerState::Request(_) => TxPermitServerPhase::Request,
            PermitServerState::Reply(_) => TxPermitServerPhase::Reply,
            PermitServerState::Disabled(_) | PermitServerState::Poisoned => {
                TxPermitServerPhase::Disabled
            }
        }
    }

    /// Retained request-validation failure, if disabled for that reason.
    pub fn fault(&self) -> Option<TxAuthorizationErrorKind> {
        match &self.state {
            PermitServerState::Disabled(failure) => Some(failure.reason()),
            PermitServerState::Idle
            | PermitServerState::Request(_)
            | PermitServerState::Reply(_)
            | PermitServerState::Poisoned => None,
        }
    }

    /// Run exactly one non-awaiting service transition with a fresh
    /// authorization clock sample.
    ///
    /// Policy is called only while moving `Request -> Reply`; a full reply
    /// channel never invokes it again.
    pub fn step<
        P,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const PACKET_BUFFERS: usize,
    >(
        &mut self,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        now: MonotonicMillis,
        policy: &mut P,
    ) -> TxPermitServerStep
    where
        P: TxAuthorizationPolicy,
    {
        let state = mem::replace(&mut self.state, PermitServerState::Poisoned);
        match state {
            PermitServerState::Idle => match self.requests.try_receive() {
                Some(request) => {
                    self.state = PermitServerState::Request(request);
                    TxPermitServerStep::Advanced
                }
                None => {
                    self.state = PermitServerState::Idle;
                    TxPermitServerStep::NeedRequest
                }
            },
            PermitServerState::Request(request) => match owner.authorize_tx(request, now, policy) {
                Ok(reply) => {
                    self.state = PermitServerState::Reply(reply);
                    TxPermitServerStep::Advanced
                }
                Err(failure) => {
                    let reason = failure.reason();
                    self.state = PermitServerState::Disabled(failure);
                    TxPermitServerStep::Disabled(reason)
                }
            },
            PermitServerState::Reply(reply) => match self.replies.try_send(reply) {
                Ok(()) => {
                    self.state = PermitServerState::Idle;
                    TxPermitServerStep::Advanced
                }
                Err(full) => {
                    self.state = PermitServerState::Reply(full.into_inner());
                    TxPermitServerStep::ReplyBackpressured
                }
            },
            PermitServerState::Disabled(failure) => {
                let reason = failure.reason();
                self.state = PermitServerState::Disabled(failure);
                TxPermitServerStep::Disabled(reason)
            }
            PermitServerState::Poisoned => {
                self.state = PermitServerState::Poisoned;
                TxPermitServerStep::InternalInvariant
            }
        }
    }

    /// Await one permit request only while idle and store it before returning.
    ///
    /// Cancellation while pending leaves the request queued; once ready, the
    /// request is assigned to persistent state in the same poll. Do not cancel
    /// the permanent supervisor that owns this service.
    pub async fn wait_for_request(&mut self) -> TxPermitServerWait {
        match self.phase() {
            TxPermitServerPhase::Idle => {
                let request = self.requests.receive().await;
                self.state = PermitServerState::Request(request);
                TxPermitServerWait::RequestStored
            }
            TxPermitServerPhase::Disabled => match &self.state {
                PermitServerState::Disabled(failure) => {
                    TxPermitServerWait::Disabled(failure.reason())
                }
                PermitServerState::Poisoned => TxPermitServerWait::InternalInvariant,
                PermitServerState::Idle
                | PermitServerState::Request(_)
                | PermitServerState::Reply(_) => TxPermitServerWait::InternalInvariant,
            },
            TxPermitServerPhase::Request | TxPermitServerPhase::Reply => {
                TxPermitServerWait::NotWaiting
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        future::Future,
        mem::size_of,
        pin::pin,
        ptr,
        task::{Context, Poll, Waker},
    };

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
    use rand_core::{CryptoRng, RngCore};
    use reticulum_node_core::{
        AttemptOutcome, DestinationHash, InterfaceSet, MonotonicSeconds, NodeConfig, NodeIdentity,
        NodeInstanceId, PrepareDataRequest, RoutedTxJob, TxAuthorizationCandidate,
        TxCompletionDisposition, TxPacketBuffer, TxPermitDenialReason, TxPolicyDecision,
        TxRecoveryPriorPhase, TxRecoveryReason,
    };
    use reticulum_tx_handoff::{ChannelFull, TxHandoff};
    use static_cell::{ConstStaticCell, StaticCell};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Wake,
    };

    use super::*;

    type TestNode<const BUFFERS: usize> = NodeCore<4, 2, 8, 2, BUFFERS>;
    type TestDispatcher<const POOL_SIZE: usize> = NoRfTxDispatcher<NoopRawMutex, POOL_SIZE>;
    type TestPermitServer = TxPermitServer<NoopRawMutex>;

    struct CountingWake(AtomicUsize);

    impl CountingWake {
        fn new() -> Self {
            Self(AtomicUsize::new(0))
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl Wake for CountingWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
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

    struct RecordingPolicy {
        decision: TxPolicyDecision,
        candidates: [Option<TxAuthorizationCandidate>; 4],
        calls: usize,
    }

    impl RecordingPolicy {
        fn allowing() -> Self {
            Self {
                decision: TxPolicyDecision::Authorize,
                candidates: [None; 4],
                calls: 0,
            }
        }
    }

    impl TxAuthorizationPolicy for RecordingPolicy {
        fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            if let Some(slot) = self.candidates.get_mut(self.calls) {
                *slot = Some(candidate);
            }
            self.calls += 1;
            self.decision
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

    fn interfaces(ids: &[u8]) -> InterfaceSet {
        ids.iter().fold(InterfaceSet::empty(), |set, id| {
            set.with(PacketInterfaceId::new(*id))
                .expect("test interface must fit the compact profile")
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare<const BUFFERS: usize>(
        sender: &mut TestNode<BUFFERS>,
        buffer: &'static mut TxPacketBuffer,
        destination: DestinationHash,
        plaintext: &[u8],
        rns_now: u64,
        owner_now: u64,
        deadline: u64,
        enabled_interfaces: InterfaceSet,
        rng: &mut CounterRng,
    ) -> RoutedTxJob<'static> {
        match sender.prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination,
                plaintext,
                rns_now: MonotonicSeconds::new(rns_now),
                owner_now: MonotonicMillis::new(owner_now),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(deadline)),
                enabled_interfaces,
            },
            rng,
        ) {
            Ok(job) => job,
            Err(failure) => panic!("test preparation failed: {:?}", failure.reason()),
        }
    }

    fn config(grace_ms: u64) -> NoRfTxDispatcherConfig {
        NoRfTxDispatcherConfig::new(
            grace_ms,
            TxDispatcherCompletionCodes::new(
                TxCompletionCode::new(0x101),
                TxCompletionCode::new(0x102),
                TxCompletionCode::new(0x103),
                TxCompletionCode::new(0x104),
                TxCompletionCode::new(0x105),
            ),
        )
    }

    fn must_fit<T>(result: Result<(), ChannelFull<T>>, message: &str) {
        if result.is_err() {
            panic!("{message}");
        }
    }

    fn completion_from(returned: TxOwnerReturn) -> reticulum_node_core::TxCompletion<'static> {
        match returned {
            TxOwnerReturn::Completion(completion) => completion,
            TxOwnerReturn::Available(_) => panic!("expected an owning completion"),
        }
    }

    fn drive_authorized_hop(
        dispatcher: &mut TestDispatcher<1>,
        permit: &mut TestPermitServer,
        owner: &mut TestNode<1>,
        policy: &mut RecordingPolicy,
        start: u64,
    ) -> NoRfFrameObservation {
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Job);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start + 1)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitSend);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start + 2)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitWait);
        assert_eq!(
            permit.step(owner, MonotonicMillis::new(start + 3), policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(permit.phase(), TxPermitServerPhase::Request);
        assert_eq!(
            permit.step(owner, MonotonicMillis::new(start + 4), policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(permit.phase(), TxPermitServerPhase::Reply);
        assert_eq!(
            permit.step(owner, MonotonicMillis::new(start + 5), policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(permit.phase(), TxPermitServerPhase::Idle);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start + 6)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitReply);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start + 7)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Authorized);
        let observation = match dispatcher.step(MonotonicMillis::new(start + 8)) {
            NoRfTxDispatcherStep::Inspected(observation) => observation,
            other => panic!("authorized hop was not inspected: {other:?}"),
        };
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Return);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start + 9)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Idle);
        observation
    }

    fn drive_job_to_permit_wait(dispatcher: &mut TestDispatcher<1>, start: u64) {
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Job);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start + 1)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitSend);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(start + 2)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitWait);
    }

    static FANOUT_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static FANOUT_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn persistent_machine_inspects_serialized_fanout_and_returns_same_owner() {
        let mut owner = node::<1>(1, "fanout-owner");
        let receiver = node::<0>(2, "fanout-receiver");
        register_peer(&mut owner, 2, "fanout-receiver");
        let buffer = FANOUT_BUFFER.take();
        let pointer = ptr::from_ref(&*buffer);
        let slot = owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"persistent fanout",
            1,
            1_000,
            2_000,
            interfaces(&[4, 1]),
            &mut rng,
        );
        let attempt = job.attempt();
        let packet_len = usize::from(job.packet_len());
        let (node_ports, dispatcher_ports) = FANOUT_HANDOFF.take().split();
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(500));
        must_fit(data.jobs.try_send(job), "initial job must fit");
        let mut policy = RecordingPolicy::allowing();

        let first =
            drive_authorized_hop(&mut dispatcher, &mut permit, &mut owner, &mut policy, 1_100);
        let completion = completion_from(data.returns.try_receive().expect("first return"));
        let next = match owner
            .complete_tx(completion, MonotonicMillis::new(1_200))
            .unwrap_or_else(|failure| panic!("first completion: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Next(job) => job,
            TxCompletionDisposition::Available(_) => panic!("fanout stopped after first hop"),
            TxCompletionDisposition::Recovered { .. } => panic!("fresh fanout recovered"),
            TxCompletionDisposition::Quarantined(_) => panic!("valid fanout quarantined"),
        };
        assert_eq!(next.slot_id(), slot);
        assert_eq!(next.attempt(), attempt);
        must_fit(data.jobs.try_send(next), "next job must fit");

        let second =
            drive_authorized_hop(&mut dispatcher, &mut permit, &mut owner, &mut policy, 1_300);
        let completion = completion_from(data.returns.try_receive().expect("second return"));
        let returned = match owner
            .complete_tx(completion, MonotonicMillis::new(1_400))
            .unwrap_or_else(|failure| panic!("second completion: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Available(buffer) => buffer,
            TxCompletionDisposition::Next(_) => panic!("fanout continued past final hop"),
            TxCompletionDisposition::Recovered { .. } => panic!("fresh fanout recovered"),
            TxCompletionDisposition::Quarantined(_) => panic!("valid fanout quarantined"),
        };

        assert_eq!(ptr::from_ref(&*returned), pointer);
        assert_eq!(first.interface, PacketInterfaceId::new(1));
        assert_eq!(second.interface, PacketInterfaceId::new(4));
        assert_eq!(first.attempt, attempt);
        assert_eq!(second.attempt, attempt);
        assert_eq!(first.packet_len, packet_len);
        assert_eq!(second.packet_len, packet_len);
        assert_eq!(first.wrapping_checksum, second.wrapping_checksum);
        assert_eq!(policy.calls, 2);
        assert!(!policy.candidates[0].unwrap().may_have_transmitted);
        assert!(policy.candidates[1].unwrap().may_have_transmitted);
        assert_eq!(owner.capacities().dispatches_used, 0);
        assert_eq!(owner.capacities().receipts_used, 1);
    }

    static EXPIRED_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static EXPIRED_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn exact_deadline_late_grant_never_reaches_no_rf_inspector() {
        let mut owner = node::<1>(3, "expired-owner");
        let receiver = node::<0>(4, "expired-receiver");
        register_peer(&mut owner, 4, "expired-receiver");
        let buffer = EXPIRED_BUFFER.take();
        let pointer = ptr::from_ref(&*buffer);
        owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"late grant",
            1,
            1_000,
            1_500,
            interfaces(&[2]),
            &mut rng,
        );
        let (node_ports, dispatcher_ports) = EXPIRED_HANDOFF.take().split();
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(500));
        must_fit(data.jobs.try_send(job), "job must fit");
        let mut policy = RecordingPolicy::allowing();

        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_100)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_101)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_102)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_200), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_499), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_499), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_500)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitReply);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_500)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Expired);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_500)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Return);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_500)),
            NoRfTxDispatcherStep::Advanced
        );
        let completion = completion_from(data.returns.try_receive().expect("expired return"));
        let (returned, record) = match owner
            .complete_tx(completion, MonotonicMillis::new(1_500))
            .unwrap_or_else(|failure| panic!("expired completion: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Recovered { buffer, record } => (buffer, record),
            TxCompletionDisposition::Available(_) => panic!("deadline recovery was hidden"),
            TxCompletionDisposition::Next(_) => panic!("expired route fanned out"),
            TxCompletionDisposition::Quarantined(_) => panic!("matching owner quarantined"),
        };
        assert_eq!(ptr::from_ref(&*returned), pointer);
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Authorized);
        assert!(record.may_have_transmitted());
        assert_eq!(record.reason(), TxRecoveryReason::DeadlineExpired);
        assert_eq!(policy.calls, 1);
    }

    static CANCEL_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static CANCEL_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn cancelled_short_waits_leave_job_request_and_reply_recoverable() {
        let mut owner = node::<1>(5, "cancel-owner");
        let receiver = node::<0>(6, "cancel-receiver");
        register_peer(&mut owner, 6, "cancel-receiver");
        let buffer = CANCEL_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"cancel waits",
            1,
            1_000,
            2_000,
            interfaces(&[1]),
            &mut rng,
        );
        let attempt = job.attempt();
        let packet_len = usize::from(job.packet_len());
        let (node_ports, dispatcher_ports) = CANCEL_HANDOFF.take().split();
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(500));

        {
            let mut wait = pin!(dispatcher.wait_for_input());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
            must_fit(data.jobs.try_send(job), "job must fit");
        }
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Idle);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_100)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Job);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_101)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitSend);

        {
            let mut wait = pin!(permit.wait_for_request());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
            assert_eq!(
                dispatcher.step(MonotonicMillis::new(1_102)),
                NoRfTxDispatcherStep::Advanced
            );
        }
        assert_eq!(permit.phase(), TxPermitServerPhase::Idle);
        let mut policy = RecordingPolicy::allowing();
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_103), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_104), &mut policy),
            TxPermitServerStep::Advanced
        );

        {
            let mut wait = pin!(dispatcher.wait_for_input());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
            assert_eq!(
                permit.step(&mut owner, MonotonicMillis::new(1_105), &mut policy),
                TxPermitServerStep::Advanced
            );
        }
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitWait);
        assert_eq!(
            block_on(dispatcher.wait_for_input()),
            NoRfTxDispatcherWait::PermitReplyStored
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitReply);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_106)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_107)),
            NoRfTxDispatcherStep::Advanced
        );
        let observation = match dispatcher.step(MonotonicMillis::new(1_108)) {
            NoRfTxDispatcherStep::Inspected(observation) => observation,
            other => panic!("ready reply did not reach inspection: {other:?}"),
        };
        assert_eq!(observation.attempt, attempt);
        assert_eq!(observation.interface, PacketInterfaceId::new(1));
        assert_eq!(observation.packet_len, packet_len);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_109)),
            NoRfTxDispatcherStep::Advanced
        );
        let completion = completion_from(data.returns.try_receive().expect("cancel return"));
        assert!(matches!(
            owner
                .complete_tx(completion, MonotonicMillis::new(1_110))
                .unwrap_or_else(|failure| panic!("cancel completion: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
    }

    static TERMINAL_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static TERMINAL_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn terminal_before_permit_bypasses_policy_and_returns_available_owner() {
        let mut owner = node::<1>(7, "terminal-owner");
        let receiver = node::<0>(8, "terminal-receiver");
        register_peer(&mut owner, 8, "terminal-receiver");
        let buffer = TERMINAL_BUFFER.take();
        let pointer = ptr::from_ref(&*buffer);
        owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"terminal before permit",
            100,
            100_000,
            200_000,
            interfaces(&[1, 4]),
            &mut rng,
        );
        let handle = job.attempt_handle();
        let (node_ports, dispatcher_ports) = TERMINAL_HANDOFF.take().split();
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(5_000));
        must_fit(data.jobs.try_send(job), "job must fit");
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(100_100)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(100_101)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(100_102)),
            NoRfTxDispatcherStep::Advanced
        );
        let report = owner.tick(MonotonicSeconds::new(132), &mut rng);
        assert_eq!(report.timed_out_attempts, 1);
        let mut policy = RecordingPolicy::allowing();
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(100_103), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(100_104), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(policy.calls, 0);
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(100_105), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(100_106)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(100_107)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Unpermitted);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(100_108)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(100_109)),
            NoRfTxDispatcherStep::Advanced
        );
        let completion = completion_from(data.returns.try_receive().expect("terminal return"));
        let returned = match owner
            .complete_tx(completion, MonotonicMillis::new(100_110))
            .unwrap_or_else(|failure| panic!("terminal completion: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Available(buffer) => buffer,
            TxCompletionDisposition::Next(_) => panic!("terminal attempt continued fanout"),
            TxCompletionDisposition::Recovered { .. } => panic!("fresh terminal recovered"),
            TxCompletionDisposition::Quarantined(_) => panic!("valid terminal quarantined"),
        };
        assert_eq!(ptr::from_ref(&*returned), pointer);
        assert_eq!(
            owner.acknowledge_terminal(handle).unwrap().outcome(),
            AttemptOutcome::DeliveryTimeout
        );
    }

    static GRACE_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static GRACE_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn missing_reply_past_grace_returns_exact_owner_then_disables() {
        let mut owner = node::<1>(9, "grace-owner");
        let receiver = node::<0>(10, "grace-receiver");
        register_peer(&mut owner, 10, "grace-receiver");
        let buffer = GRACE_BUFFER.take();
        let slot = owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"missing reply",
            1,
            1_000,
            1_500,
            interfaces(&[1]),
            &mut rng,
        );
        let (node_ports, dispatcher_ports) = GRACE_HANDOFF.take().split();
        let (mut data, _permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(100));
        must_fit(data.jobs.try_send(job), "job must fit");
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_100)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_101)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_102)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_599)),
            NoRfTxDispatcherStep::NeedPermitReply {
                grace_deadline: MonotonicMillis::new(1_600)
            }
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Return);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Disabled(TxDispatcherFault::PermitReplyGraceExpired)
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Disabled);
        let completion = completion_from(data.returns.try_receive().expect("fault return"));
        let quarantine = match owner
            .complete_tx(completion, MonotonicMillis::new(1_600))
            .unwrap_or_else(|failure| panic!("fault completion: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            TxCompletionDisposition::Available(_) => panic!("fault released buffer"),
            TxCompletionDisposition::Next(_) => panic!("fault continued fanout"),
            TxCompletionDisposition::Recovered { .. } => panic!("fault hid quarantine"),
        };
        assert_eq!(quarantine.slot_id(), slot);
        assert_eq!(
            quarantine.record().reason(),
            TxRecoveryReason::CompletionFault(TxCompletionCode::new(0x104))
        );
    }

    static REQUEST_CUTOFF_A: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static REQUEST_CUTOFF_B: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static REQUEST_CUTOFF_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn exact_grace_cutoff_retains_unsent_request_even_after_pressure_clears() {
        let mut owner = node::<2>(15, "request-cutoff-owner");
        let receiver = node::<0>(16, "request-cutoff-receiver");
        register_peer(&mut owner, 16, "request-cutoff-receiver");
        let first_buffer = REQUEST_CUTOFF_A.take();
        let second_buffer = REQUEST_CUTOFF_B.take();
        owner.register_packet_buffer(first_buffer).unwrap();
        owner.register_packet_buffer(second_buffer).unwrap();
        let mut rng = CounterRng::default();
        let first = prepare(
            &mut owner,
            first_buffer,
            receiver.destination_hash(),
            b"cutoff owner",
            1,
            1_000,
            1_500,
            interfaces(&[1]),
            &mut rng,
        );
        let second = prepare(
            &mut owner,
            second_buffer,
            receiver.destination_hash(),
            b"channel filler",
            2,
            1_000,
            2_000,
            interfaces(&[1]),
            &mut rng,
        );
        let (_second_pending, second_request) = second.begin_permit();
        let (node_ports, mut dispatcher_ports) = REQUEST_CUTOFF_HANDOFF.take().split();
        must_fit(
            dispatcher_ports.permit_requests.try_send(second_request),
            "filler request must occupy the control channel",
        );
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(100));
        must_fit(data.jobs.try_send(first), "cutoff job must fit");

        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_100)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_101)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_102)),
            NoRfTxDispatcherStep::Backpressured(TxDispatcherChannel::PermitRequest)
        );
        let mut policy = RecordingPolicy::allowing();
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_599), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(permit.phase(), TxPermitServerPhase::Request);
        assert_eq!(policy.calls, 0);

        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Return);
        assert_eq!(
            dispatcher.fault(),
            Some(TxDispatcherFault::PermitRequestGraceExpired)
        );
        assert_eq!(
            dispatcher.fault_residue_kind(),
            Some(TxDispatcherFaultResidueKind::UnsentPermitRequest)
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Disabled(TxDispatcherFault::PermitRequestGraceExpired)
        );
        let completion = completion_from(data.returns.try_receive().expect("cutoff return"));
        assert!(matches!(
            owner
                .complete_tx(completion, MonotonicMillis::new(1_600))
                .unwrap_or_else(|failure| panic!("cutoff completion: {:?}", failure.reason())),
            TxCompletionDisposition::Quarantined(_)
        ));
        assert_eq!(policy.calls, 0);
    }

    static AUTHORIZED_GRACE_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static AUTHORIZED_GRACE_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn withheld_authorized_reply_past_grace_quarantines_as_possibly_transmitted() {
        let mut owner = node::<1>(17, "authorized-grace-owner");
        let receiver = node::<0>(18, "authorized-grace-receiver");
        register_peer(&mut owner, 18, "authorized-grace-receiver");
        let buffer = AUTHORIZED_GRACE_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"authorized reply withheld",
            1,
            1_000,
            1_500,
            interfaces(&[1]),
            &mut rng,
        );
        let (node_ports, dispatcher_ports) = AUTHORIZED_GRACE_HANDOFF.take().split();
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(100));
        must_fit(data.jobs.try_send(job), "authorized-grace job must fit");
        drive_job_to_permit_wait(&mut dispatcher, 1_100);
        let mut policy = RecordingPolicy::allowing();
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_200), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_300), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(permit.phase(), TxPermitServerPhase::Reply);
        assert_eq!(policy.calls, 1);

        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Return);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Disabled(TxDispatcherFault::PermitReplyGraceExpired)
        );
        let completion = completion_from(data.returns.try_receive().expect("authorized recovery"));
        let quarantine = match owner
            .complete_tx(completion, MonotonicMillis::new(1_600))
            .unwrap_or_else(|failure| panic!("authorized recovery: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            TxCompletionDisposition::Available(_) => panic!("authorized fault released owner"),
            TxCompletionDisposition::Next(_) => panic!("authorized fault continued fanout"),
            TxCompletionDisposition::Recovered { .. } => {
                panic!("authorized control fault hid quarantine")
            }
        };
        assert_eq!(
            quarantine.record().prior_phase(),
            TxRecoveryPriorPhase::Authorized
        );
        assert!(quarantine.record().may_have_transmitted());
        assert_eq!(
            quarantine.record().reason(),
            TxRecoveryReason::CompletionFault(TxCompletionCode::new(0x104))
        );
    }

    static LATE_REQUEST_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static LATE_REQUEST_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn request_serviced_after_dispatcher_recovery_cannot_call_policy_or_expose_bytes() {
        let mut owner = node::<1>(19, "late-request-owner");
        let receiver = node::<0>(20, "late-request-receiver");
        register_peer(&mut owner, 20, "late-request-receiver");
        let buffer = LATE_REQUEST_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"request after recovery",
            1,
            1_000,
            1_500,
            interfaces(&[1]),
            &mut rng,
        );
        let (node_ports, dispatcher_ports) = LATE_REQUEST_HANDOFF.take().split();
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(100));
        must_fit(data.jobs.try_send(job), "late-request job must fit");
        drive_job_to_permit_wait(&mut dispatcher, 1_100);

        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Disabled(TxDispatcherFault::PermitReplyGraceExpired)
        );
        let completion = completion_from(data.returns.try_receive().expect("late-request return"));
        assert!(matches!(
            owner
                .complete_tx(completion, MonotonicMillis::new(1_600))
                .unwrap_or_else(|failure| panic!(
                    "late-request completion: {:?}",
                    failure.reason()
                )),
            TxCompletionDisposition::Quarantined(_)
        ));

        let mut policy = RecordingPolicy::allowing();
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_601), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(permit.phase(), TxPermitServerPhase::Request);
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_602), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(permit.phase(), TxPermitServerPhase::Reply);
        assert_eq!(policy.calls, 0);
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_603), &mut policy),
            TxPermitServerStep::Advanced
        );
        let late_reply = dispatcher
            .handoff
            .permit_replies
            .try_receive()
            .expect("late request must receive a bounded denial");
        match late_reply {
            TxPermitReply::Denied(denial) => {
                assert_eq!(denial.reason(), TxPermitDenialReason::RecoveryRequired)
            }
            TxPermitReply::Granted(_) => panic!("recovered request was authorized"),
        }
        assert_eq!(permit.phase(), TxPermitServerPhase::Idle);
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Disabled);
    }

    static REPLY_AT_GRACE_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static REPLY_AT_GRACE_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn already_queued_reply_wins_at_exact_grace_but_remains_expired() {
        let mut owner = node::<1>(21, "reply-at-grace-owner");
        let receiver = node::<0>(22, "reply-at-grace-receiver");
        register_peer(&mut owner, 22, "reply-at-grace-receiver");
        let buffer = REPLY_AT_GRACE_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"reply observable at grace",
            1,
            1_000,
            1_500,
            interfaces(&[1]),
            &mut rng,
        );
        let (node_ports, dispatcher_ports) = REPLY_AT_GRACE_HANDOFF.take().split();
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(100));
        must_fit(data.jobs.try_send(job), "reply-at-grace job must fit");
        drive_job_to_permit_wait(&mut dispatcher, 1_100);
        let mut policy = RecordingPolicy::allowing();
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_200), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_300), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_400), &mut policy),
            TxPermitServerStep::Advanced
        );

        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitReply);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Expired);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_600)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Idle);
        let completion = completion_from(data.returns.try_receive().expect("grace reply return"));
        let (_, record) = match owner
            .complete_tx(completion, MonotonicMillis::new(1_600))
            .unwrap_or_else(|failure| panic!("grace reply completion: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Recovered { buffer, record } => (buffer, record),
            TxCompletionDisposition::Available(_) => panic!("late grant appeared successful"),
            TxCompletionDisposition::Next(_) => panic!("late grant continued fanout"),
            TxCompletionDisposition::Quarantined(_) => panic!("matching late grant quarantined"),
        };
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Authorized);
        assert!(record.may_have_transmitted());
        assert_eq!(record.reason(), TxRecoveryReason::DeadlineExpired);
        assert_eq!(policy.calls, 1);
    }

    static OBSERVED_AFTER_GRACE_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static OBSERVED_AFTER_GRACE_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn reply_enqueued_after_threshold_but_before_recovery_observation_wins() {
        let mut owner = node::<1>(25, "observed-after-grace-owner");
        let receiver = node::<0>(26, "observed-after-grace-receiver");
        register_peer(&mut owner, 26, "observed-after-grace-receiver");
        let buffer = OBSERVED_AFTER_GRACE_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"reply observed after grace",
            1,
            1_000,
            1_500,
            interfaces(&[1]),
            &mut rng,
        );
        let (node_ports, dispatcher_ports) = OBSERVED_AFTER_GRACE_HANDOFF.take().split();
        let (mut data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(100));
        must_fit(data.jobs.try_send(job), "observed-after-grace job must fit");
        drive_job_to_permit_wait(&mut dispatcher, 1_100);
        let mut policy = RecordingPolicy::allowing();
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_200), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_300), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(permit.phase(), TxPermitServerPhase::Reply);

        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_601), &mut policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_700)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::PermitReply);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_700)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Expired);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_700)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_700)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Idle);
        let completion = completion_from(
            data.returns
                .try_receive()
                .expect("post-threshold reply return"),
        );
        let record = match owner
            .complete_tx(completion, MonotonicMillis::new(1_700))
            .unwrap_or_else(|failure| panic!("post-threshold completion: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Recovered { record, .. } => record,
            TxCompletionDisposition::Available(_) => {
                panic!("post-threshold grant appeared successful")
            }
            TxCompletionDisposition::Next(_) => {
                panic!("post-threshold grant continued fanout")
            }
            TxCompletionDisposition::Quarantined(_) => {
                panic!("matching post-threshold grant quarantined")
            }
        };
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Authorized);
        assert!(record.may_have_transmitted());
        assert_eq!(record.reason(), TxRecoveryReason::DeadlineExpired);
        assert_eq!(policy.calls, 1);
    }

    static ORPHAN_WAIT_BUFFER: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static ORPHAN_WAIT_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn idle_wait_wakes_on_orphan_reply_and_disables_without_waiting_for_a_job() {
        let mut owner = node::<1>(23, "orphan-wait-owner");
        let receiver = node::<0>(24, "orphan-wait-receiver");
        register_peer(&mut owner, 24, "orphan-wait-receiver");
        let buffer = ORPHAN_WAIT_BUFFER.take();
        owner.register_packet_buffer(buffer).unwrap();
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            buffer,
            receiver.destination_hash(),
            b"orphan wait",
            1,
            1_000,
            2_000,
            interfaces(&[1]),
            &mut rng,
        );
        let (_pending, request) = job.begin_permit();
        let mut policy = RecordingPolicy::allowing();
        let reply = owner
            .authorize_tx(request, MonotonicMillis::new(1_100), &mut policy)
            .unwrap_or_else(|failure| panic!("orphan authorization: {:?}", failure.reason()));
        let (mut node_ports, dispatcher_ports) = ORPHAN_WAIT_HANDOFF.take().split();
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(100));

        let wake_count = Arc::new(CountingWake::new());
        let waker = Waker::from(Arc::clone(&wake_count));
        let result = {
            let mut wait = pin!(dispatcher.wait_for_input());
            let mut context = Context::from_waker(&waker);
            assert!(matches!(wait.as_mut().poll(&mut context), Poll::Pending));
            must_fit(
                node_ports.permit_replies.try_send(reply),
                "orphan reply must fit",
            );
            assert!(
                wake_count.count() > 0,
                "orphan reply did not wake idle wait"
            );
            match wait.as_mut().poll(&mut context) {
                Poll::Ready(result) => result,
                Poll::Pending => panic!("woken orphan reply remained pending"),
            }
        };
        assert_eq!(
            result,
            NoRfTxDispatcherWait::Disabled(TxDispatcherFault::OrphanPermitReply)
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Disabled);
        assert_eq!(
            dispatcher.fault_residue_kind(),
            Some(TxDispatcherFaultResidueKind::OrphanPermitReply)
        );
        assert_eq!(policy.calls, 1);
    }

    static MISMATCH_A: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static MISMATCH_B: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static MISMATCH_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn mismatched_reply_returns_pending_owner_and_retains_scalar_before_disable() {
        let mut owner = node::<2>(11, "mismatch-owner");
        let receiver = node::<0>(12, "mismatch-receiver");
        register_peer(&mut owner, 12, "mismatch-receiver");
        let first_buffer = MISMATCH_A.take();
        let second_buffer = MISMATCH_B.take();
        owner.register_packet_buffer(first_buffer).unwrap();
        owner.register_packet_buffer(second_buffer).unwrap();
        let mut rng = CounterRng::default();
        let first = prepare(
            &mut owner,
            first_buffer,
            receiver.destination_hash(),
            b"mismatch first",
            1,
            1_000,
            2_000,
            interfaces(&[1]),
            &mut rng,
        );
        let first_slot = first.slot_id();
        let meta = DispatchMeta::from_job(&first, 100);
        let second = prepare(
            &mut owner,
            second_buffer,
            receiver.destination_hash(),
            b"mismatch second",
            2,
            1_000,
            2_000,
            interfaces(&[1]),
            &mut rng,
        );
        let (first_pending, _first_request) = first.begin_permit();
        let (_second_pending, second_request) = second.begin_permit();
        let second_reply = owner
            .authorize_tx(
                second_request,
                MonotonicMillis::new(1_100),
                &mut RecordingPolicy::allowing(),
            )
            .unwrap_or_else(|failure| panic!("second authorization: {:?}", failure.reason()));
        let (mut node_ports, dispatcher_ports) = MISMATCH_HANDOFF.take().split();
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(100));
        dispatcher.state = DispatcherState::PermitReply {
            pending: first_pending,
            reply: second_reply,
            meta,
        };

        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_101)),
            NoRfTxDispatcherStep::Advanced
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Return);
        assert_eq!(
            dispatcher.fault(),
            Some(TxDispatcherFault::PermitReplyMismatch)
        );
        assert_eq!(
            dispatcher.fault_residue_kind(),
            Some(TxDispatcherFaultResidueKind::MismatchedPermitReply)
        );
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1_102)),
            NoRfTxDispatcherStep::Disabled(TxDispatcherFault::PermitReplyMismatch)
        );
        let completion = completion_from(node_ports.returns.try_receive().expect("fault return"));
        let quarantine = match owner
            .complete_tx(completion, MonotonicMillis::new(1_103))
            .unwrap_or_else(|failure| panic!("mismatch completion: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            TxCompletionDisposition::Available(_) => panic!("mismatch released owner"),
            TxCompletionDisposition::Next(_) => panic!("mismatch continued fanout"),
            TxCompletionDisposition::Recovered { .. } => panic!("mismatch hid quarantine"),
        };
        assert_eq!(quarantine.slot_id(), first_slot);
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Disabled);
        assert_eq!(
            dispatcher.fault_residue_kind(),
            Some(TxDispatcherFaultResidueKind::MismatchedPermitReply)
        );
    }

    static PERMIT_PRESSURE_A: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static PERMIT_PRESSURE_B: ConstStaticCell<TxPacketBuffer> =
        ConstStaticCell::new(TxPacketBuffer::new());
    static PERMIT_PRESSURE_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn permit_server_authorizes_once_and_retains_reply_under_pressure() {
        let mut owner = node::<2>(13, "permit-pressure-owner");
        let receiver = node::<0>(14, "permit-pressure-receiver");
        register_peer(&mut owner, 14, "permit-pressure-receiver");
        let first_buffer = PERMIT_PRESSURE_A.take();
        let second_buffer = PERMIT_PRESSURE_B.take();
        owner.register_packet_buffer(first_buffer).unwrap();
        owner.register_packet_buffer(second_buffer).unwrap();
        let mut rng = CounterRng::default();
        let first = prepare(
            &mut owner,
            first_buffer,
            receiver.destination_hash(),
            b"pressure first",
            1,
            1_000,
            3_000,
            interfaces(&[1]),
            &mut rng,
        );
        let second = prepare(
            &mut owner,
            second_buffer,
            receiver.destination_hash(),
            b"pressure second",
            2,
            1_000,
            3_000,
            interfaces(&[1]),
            &mut rng,
        );
        let (first_pending, first_request) = first.begin_permit();
        let (second_pending, second_request) = second.begin_permit();
        let (mut node_ports, mut dispatcher_ports) = PERMIT_PRESSURE_HANDOFF.take().split();
        let mut first_policy = RecordingPolicy::allowing();
        let first_reply = owner
            .authorize_tx(
                first_request,
                MonotonicMillis::new(1_100),
                &mut first_policy,
            )
            .unwrap_or_else(|failure| panic!("first authorization: {:?}", failure.reason()));
        must_fit(
            node_ports.permit_replies.try_send(first_reply),
            "first reply must fill channel",
        );
        must_fit(
            dispatcher_ports.permit_requests.try_send(second_request),
            "second request must fit",
        );
        let (_data, mut permit) = TxPermitServer::from_node_handoff(node_ports);
        let mut second_policy = RecordingPolicy::allowing();

        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_101), &mut second_policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_102), &mut second_policy),
            TxPermitServerStep::Advanced
        );
        assert_eq!(second_policy.calls, 1);
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_103), &mut second_policy),
            TxPermitServerStep::ReplyBackpressured
        );
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_104), &mut second_policy),
            TxPermitServerStep::ReplyBackpressured
        );
        assert_eq!(second_policy.calls, 1);

        let first_reply = dispatcher_ports
            .permit_replies
            .try_receive()
            .expect("first reply must remain queued");
        assert_eq!(
            permit.step(&mut owner, MonotonicMillis::new(1_105), &mut second_policy),
            TxPermitServerStep::Advanced
        );
        let second_reply = dispatcher_ports
            .permit_replies
            .try_receive()
            .expect("second reply must be sent unchanged");
        let first_authorized = match first_pending.resolve(first_reply, MonotonicMillis::new(1_106))
        {
            Ok(PermitResolution::Authorized(owner)) => owner,
            Ok(_) => panic!("first grant changed classification"),
            Err(_) => panic!("first grant mismatched"),
        };
        let second_authorized =
            match second_pending.resolve(second_reply, MonotonicMillis::new(1_106)) {
                Ok(PermitResolution::Authorized(owner)) => owner,
                Ok(_) => panic!("second grant changed classification"),
                Err(_) => panic!("second grant mismatched"),
            };
        assert!(matches!(
            owner
                .complete_tx(
                    first_authorized.complete(TxCompletionCode::new(1)),
                    MonotonicMillis::new(1_107)
                )
                .unwrap_or_else(|failure| panic!("first completion: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
        assert!(matches!(
            owner
                .complete_tx(
                    second_authorized.complete(TxCompletionCode::new(2)),
                    MonotonicMillis::new(1_107)
                )
                .unwrap_or_else(|failure| panic!("second completion: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
        assert_eq!(first_policy.calls, 1);
        assert_eq!(second_policy.calls, 1);
    }

    static RETURN_A: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
    static RETURN_B: ConstStaticCell<TxPacketBuffer> = ConstStaticCell::new(TxPacketBuffer::new());
    static RETURN_HANDOFF: ConstStaticCell<TxHandoff<NoopRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());

    #[test]
    fn owner_return_pressure_restores_exact_noncopy_value_in_machine() {
        let first = RETURN_A.take();
        let second = RETURN_B.take();
        let first_pointer = ptr::from_ref(&*first);
        let second_pointer = ptr::from_ref(&*second);
        let (mut node_ports, mut dispatcher_ports) = RETURN_HANDOFF.take().split();
        must_fit(
            dispatcher_ports.returns.try_send(first.into()),
            "first return must fit",
        );
        let meta = DispatchMeta {
            owner_deadline: TxLeaseDeadline::new(MonotonicMillis::new(10)),
            grace_deadline: MonotonicMillis::new(20),
        };
        let mut dispatcher = NoRfTxDispatcher::new(dispatcher_ports, config(10));
        dispatcher.state = DispatcherState::Return {
            returned: second.into(),
            after: AfterReturn::Resume,
            meta,
        };
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(1)),
            NoRfTxDispatcherStep::Backpressured(TxDispatcherChannel::OwnerReturn)
        );
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Return);
        let first = match node_ports.returns.try_receive().expect("first return") {
            TxOwnerReturn::Available(buffer) => buffer,
            TxOwnerReturn::Completion(_) => panic!("first changed variant"),
        };
        assert_eq!(ptr::from_ref(&*first), first_pointer);
        assert_eq!(
            dispatcher.step(MonotonicMillis::new(2)),
            NoRfTxDispatcherStep::Advanced
        );
        let second = match node_ports.returns.try_receive().expect("second return") {
            TxOwnerReturn::Available(buffer) => buffer,
            TxOwnerReturn::Completion(_) => panic!("second changed variant"),
        };
        assert_eq!(ptr::from_ref(&*second), second_pointer);
    }

    static PRODUCTION_HANDOFF: ConstStaticCell<TxHandoff<CriticalSectionRawMutex, 1>> =
        ConstStaticCell::new(TxHandoff::new());
    static PRODUCTION_DISPATCHER: StaticCell<NoRfTxDispatcher<CriticalSectionRawMutex, 1>> =
        StaticCell::new();

    #[test]
    fn production_mutex_runtime_fits_static_storage_without_packet_array() {
        let (_node, dispatcher_ports) = PRODUCTION_HANDOFF.take().split();
        let dispatcher =
            PRODUCTION_DISPATCHER.init(NoRfTxDispatcher::new(dispatcher_ports, config(500)));
        assert_eq!(dispatcher.phase(), NoRfTxDispatcherPhase::Idle);
        assert!(
            size_of::<NoRfTxDispatcher<NoopRawMutex, 1>>() < size_of::<TxPacketBuffer>(),
            "compact dispatcher state must not embed one packet-sized array"
        );
    }
}
