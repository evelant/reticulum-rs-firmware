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
mod tests;
