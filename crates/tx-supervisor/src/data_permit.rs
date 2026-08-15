//! Persistent permit service for ticket-routed destination-DATA packets.

use core::{future::poll_fn, mem, task::Poll};

use embassy_sync::blocking_mutex::raw::RawMutex;
use reticulum_node_core::{
    MonotonicMillis, NodeCore, TxAuthorizationPolicy, TxPermitReply, TxPermitRequest,
};
use reticulum_tx_handoff::DataNodePermitHandoff;

use crate::{DataPermitAuthorizationError, DataPermitAuthorizationFailure, DataRouterCoordinator};

enum DataPermitServerState {
    Idle,
    Request(TxPermitRequest),
    Reply(TxPermitReply),
    Disabled(DataPermitAuthorizationFailure),
    Poisoned,
}

/// Persistent phase of one actor-specific DATA permit service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPermitServerPhase {
    /// No scalar request is retained.
    Idle,
    /// One exact request is stored for authorization with a fresh clock sample.
    Request,
    /// One exact reply is stored until the concrete actor accepts it.
    Reply,
    /// Request validation or a private invariant failed permanently.
    Disabled,
}

/// Result of one synchronous DATA permit-service transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "DATA permit-service progress and faults must be handled"]
pub enum DataPermitServerStep {
    /// One ownership-preserving transition completed.
    Advanced,
    /// No permit request is currently queued.
    NeedRequest,
    /// The full reply channel returned the exact reply to persistent state.
    ReplyBackpressured,
    /// Coordinator request validation failed and the exact request is retained.
    Disabled(DataPermitAuthorizationError),
    /// Private service state was internally inconsistent.
    InternalInvariant,
}

/// Result of the short cancellation-safe DATA permit-service wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "the caller must resume DATA permit-service stepping"]
pub enum DataPermitServerWait {
    /// One exact request was stored in persistent service state.
    RequestStored,
    /// The actor reply queue has advisory capacity for the retained reply.
    ReplyCapacityReady,
    /// Synchronous authorization is immediately possible; call [`DataPermitServer::step`].
    NotWaiting,
    /// The service was already disabled by request validation.
    Disabled(DataPermitAuthorizationError),
    /// Private service state was internally inconsistent.
    InternalInvariant,
}

/// Kind of exact non-`Copy` control value retained after service disablement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataPermitServerFaultResidueKind {
    /// The request rejected by the DATA coordinator remains retained unchanged.
    Request,
}

/// Node-side persistent server for one concrete actor's DATA permit exchange.
///
/// The actor keeps its [`reticulum_node_core::PermitPendingTx`] and router
/// completion ticket while only the opaque request crosses this service. The
/// server calls the DATA coordinator's authorization facade at most once per
/// request and retains a non-`Copy` reply unchanged across channel pressure.
/// Store this machine outside any short executor future; only
/// [`Self::wait_for_progress`] is cancellation-safe to abandon.
#[must_use = "dropping the DATA permit server abandons its ports and retained control value"]
pub struct DataPermitServer<M>
where
    M: RawMutex + 'static,
{
    handoff: DataNodePermitHandoff<M>,
    state: DataPermitServerState,
}

impl<M> DataPermitServer<M>
where
    M: RawMutex + 'static,
{
    /// Consume the sole node-side DATA permit roles into an empty server.
    pub const fn new(handoff: DataNodePermitHandoff<M>) -> Self {
        Self {
            handoff,
            state: DataPermitServerState::Idle,
        }
    }

    /// Current scalar phase.
    pub const fn phase(&self) -> DataPermitServerPhase {
        match &self.state {
            DataPermitServerState::Idle => DataPermitServerPhase::Idle,
            DataPermitServerState::Request(_) => DataPermitServerPhase::Request,
            DataPermitServerState::Reply(_) => DataPermitServerPhase::Reply,
            DataPermitServerState::Disabled(_) | DataPermitServerState::Poisoned => {
                DataPermitServerPhase::Disabled
            }
        }
    }

    /// Retained request-validation failure, if disabled for that reason.
    pub fn fault(&self) -> Option<DataPermitAuthorizationError> {
        match &self.state {
            DataPermitServerState::Disabled(failure) => Some(failure.reason()),
            DataPermitServerState::Idle
            | DataPermitServerState::Request(_)
            | DataPermitServerState::Reply(_)
            | DataPermitServerState::Poisoned => None,
        }
    }

    /// Kind of exact non-`Copy` residue retained after a validation fault.
    pub fn fault_residue_kind(&self) -> Option<DataPermitServerFaultResidueKind> {
        match &self.state {
            DataPermitServerState::Disabled(_) => Some(DataPermitServerFaultResidueKind::Request),
            DataPermitServerState::Idle
            | DataPermitServerState::Request(_)
            | DataPermitServerState::Reply(_)
            | DataPermitServerState::Poisoned => None,
        }
    }

    /// Run exactly one non-awaiting service transition with a fresh clock.
    ///
    /// Product policy is called only while moving `Request -> Reply`; retrying
    /// a full reply channel cannot authorize or consume a reservation twice.
    /// If the coordinator is already faulted, its facade validates a still-live
    /// request and forces an unpermitted drain reply without invoking `policy`.
    #[allow(clippy::too_many_arguments)]
    pub fn step<
        P,
        const PATHS: usize,
        const ANNOUNCES: usize,
        const DEDUPLICATION: usize,
        const LINKS: usize,
        const PACKET_BUFFERS: usize,
    >(
        &mut self,
        coordinator: &mut DataRouterCoordinator<PACKET_BUFFERS>,
        owner: &mut NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>,
        now: MonotonicMillis,
        policy: &mut P,
    ) -> DataPermitServerStep
    where
        P: TxAuthorizationPolicy,
    {
        let state = mem::replace(&mut self.state, DataPermitServerState::Poisoned);
        match state {
            DataPermitServerState::Idle => match self.handoff.requests().try_receive() {
                Some(request) => {
                    self.state = DataPermitServerState::Request(request);
                    DataPermitServerStep::Advanced
                }
                None => {
                    self.state = DataPermitServerState::Idle;
                    DataPermitServerStep::NeedRequest
                }
            },
            DataPermitServerState::Request(request) => {
                match coordinator.authorize_tx(owner, request, now, policy) {
                    Ok(reply) => {
                        self.state = DataPermitServerState::Reply(reply);
                        DataPermitServerStep::Advanced
                    }
                    Err(failure) => {
                        let reason = failure.reason();
                        self.state = DataPermitServerState::Disabled(failure);
                        DataPermitServerStep::Disabled(reason)
                    }
                }
            }
            DataPermitServerState::Reply(reply) => match self.handoff.replies().try_send(reply) {
                Ok(()) => {
                    self.state = DataPermitServerState::Idle;
                    DataPermitServerStep::Advanced
                }
                Err(full) => {
                    self.state = DataPermitServerState::Reply(full.into_inner());
                    DataPermitServerStep::ReplyBackpressured
                }
            },
            DataPermitServerState::Disabled(failure) => {
                let reason = failure.reason();
                self.state = DataPermitServerState::Disabled(failure);
                DataPermitServerStep::Disabled(reason)
            }
            DataPermitServerState::Poisoned => {
                self.state = DataPermitServerState::Poisoned;
                DataPermitServerStep::InternalInvariant
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
    ) -> Poll<DataPermitServerWait> {
        match self.phase() {
            DataPermitServerPhase::Idle => match self.handoff.requests().poll_receive(context) {
                Poll::Ready(request) => {
                    self.state = DataPermitServerState::Request(request);
                    Poll::Ready(DataPermitServerWait::RequestStored)
                }
                Poll::Pending => Poll::Pending,
            },
            DataPermitServerPhase::Request => Poll::Ready(DataPermitServerWait::NotWaiting),
            DataPermitServerPhase::Reply => self
                .handoff
                .replies()
                .poll_ready_to_send(context)
                .map(|()| DataPermitServerWait::ReplyCapacityReady),
            DataPermitServerPhase::Disabled => match &self.state {
                DataPermitServerState::Disabled(failure) => {
                    Poll::Ready(DataPermitServerWait::Disabled(failure.reason()))
                }
                DataPermitServerState::Poisoned => {
                    Poll::Ready(DataPermitServerWait::InternalInvariant)
                }
                DataPermitServerState::Idle
                | DataPermitServerState::Request(_)
                | DataPermitServerState::Reply(_) => {
                    Poll::Ready(DataPermitServerWait::InternalInvariant)
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
    pub async fn wait_for_progress(&mut self) -> DataPermitServerWait {
        poll_fn(|context| self.poll_progress(context)).await
    }
}

#[cfg(test)]
mod tests;
