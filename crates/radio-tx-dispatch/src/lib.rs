//! Persistent board-neutral dispatch for one RNode-compatible LoRa radio.
//!
//! [`SoleRadioTxDispatcher`] is the sole consumer of one transport-neutral
//! interface-actor queue and the DATA/ordinary permit exchanges. It retains
//! each router-issued completion ticket beside the active node-core typestate
//! while serializing both families over one [`SoleRnodeRadio`]. Every packet
//! performs randomized initial backoff,
//! bounded retry backoff and CAD before it requests an exact configuration- and
//! airtime-bound permit. Packet bytes are exposed only after a matching grant
//! and the final fresh channel-access check.
//! Once DATA bytes have been exposed, the dispatcher sends the exact
//! [`AuthorizedFrameObservation`] through its dedicated handoff and retains the
//! owning completion until the node echoes an identical durable
//! acknowledgement. There is deliberately no acknowledgement timeout. A
//! mismatch, or any acknowledgement queued before its request is accepted,
//! disables the dispatcher while retaining all correlation evidence and the
//! owner.
//!
//! Idle receive is serialized through the same owner. Starting RX is an
//! explicit scheduler choice whenever no TX is already active: a queued job is
//! left in the actor queue instead of being claimed merely because RX was
//! selected. This makes bounded RX/TX fairness expressible by the permanent
//! supervisor while retaining receive cancellation as an explicit fail-closed
//! recovery phase.
//!
//! Short radio-operation futures are cancellation-contained: before awaiting
//! RX, CAD, or TX, the receive phase or unique packet owner is moved into
//! persistent dispatcher state. If a supervisor drops such a future, it must call
//! [`SoleRadioTxDispatcher::recover_cancelled_radio_operation`] before making
//! further progress. A mismatched non-`Copy` permit reply cannot be returned by
//! the current one-way handoff API, so that case deliberately fails closed and
//! retains the exact reply rather than weakening ownership.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub(crate) use core::{
    future::poll_fn,
    mem,
    task::{Context, Poll},
};

pub(crate) use embassy_sync::blocking_mutex::raw::RawMutex;
pub(crate) use embassy_time::{Instant, Timer};
pub(crate) use rand_core::{CryptoRng, RngCore};
pub(crate) use reticulum_interface_router::{
    ActorCompletionSendError, DataCompletionMismatch, DataCompletionTicket, InterfaceConfigId,
    InterfaceTxActorHandoff, InterfaceTxCompletion, InterfaceTxJob, OrdinaryCompletionMismatch,
    OrdinaryCompletionTicket,
};
pub(crate) use reticulum_node_core::{
    AttemptHandle, AttemptToken, AuthorizedFrameObservation, AuthorizedTx, EncodedPacketSha256,
    ExpiredAuthorizedTx, MonotonicMillis, OrdinaryAuthorizedTx, OrdinaryExpiredAuthorizedTx,
    OrdinaryPermitPendingTx, OrdinaryPermitReplyMismatch, OrdinaryPermitResolution,
    OrdinaryTxCompletion, OrdinaryTxJob, OrdinaryTxPermitReply, OrdinaryTxPermitRequest,
    OrdinaryUnpermittedTx, PacketInterfaceId, PermitPendingTx, PermitReplyMismatch,
    PermitResolution, RoutedTxJob, TxAuthorizationCandidate, TxAuthorizationPolicy, TxCompletion,
    TxCompletionCode, TxFrameError, TxPermitReply, TxPermitRequest, TxPermitRequirements,
    TxPermitReservation, TxPermitResourceId, TxPolicyDecision, TxPolicyDenial, UnpermittedTx,
};
pub(crate) use reticulum_radio_interface::{
    BoundedRxObservation, BoundedRxOutcome, LoRaProfile, LogicalPacketAccessAction,
    LogicalPacketAccessConfig, LogicalPacketAccessPhase, LogicalPacketAccessRejection,
    LogicalPacketChannelAccess, PacketTxProgress, RadioConfigurationFingerprint, RnodeAirtimeError,
    RnodeTxFrameBuffer, SX1262_FRAME_MTU, SoleRadioFault, SoleRadioFaultClass, SoleRadioFaultPhase,
    SoleRnodeRadio, frame_rns_packet,
};
pub(crate) use reticulum_tx_handoff::{
    AuthorizedFrameDispatcherHandoff, DispatcherPermitHandoff, OrdinaryDispatcherPermitHandoff,
};

mod types;
pub use types::*;
mod state;
pub(crate) use state::*;
mod dispatcher;
pub use dispatcher::*;

fn data_phase(state: &DataState) -> RadioTxDispatcherPhase {
    match state {
        DataState::Idle => RadioTxDispatcherPhase::Idle,
        DataState::Job(_) => RadioTxDispatcherPhase::JobReady(DispatchFamily::Data),
        DataState::Access { access, .. } => match access.phase() {
            LogicalPacketAccessPhase::BackingOff => {
                RadioTxDispatcherPhase::BackingOff(DispatchFamily::Data)
            }
            LogicalPacketAccessPhase::AwaitingCad => {
                RadioTxDispatcherPhase::CadReady(DispatchFamily::Data)
            }
            _ => RadioTxDispatcherPhase::Disabled,
        },
        DataState::CadInFlight { .. } => RadioTxDispatcherPhase::CadInFlight(DispatchFamily::Data),
        DataState::PermitSend { .. } => RadioTxDispatcherPhase::PermitSend(DispatchFamily::Data),
        DataState::PermitWait { .. } | DataState::PermitReply { .. } => {
            RadioTxDispatcherPhase::PermitWait(DispatchFamily::Data)
        }
        DataState::TransmitReady { .. } => {
            RadioTxDispatcherPhase::TransmitReady(DispatchFamily::Data)
        }
        DataState::TxInFlight { .. } => {
            RadioTxDispatcherPhase::TransmitInFlight(DispatchFamily::Data)
        }
        DataState::Expired { .. } | DataState::Unpermitted { .. } => {
            RadioTxDispatcherPhase::PermitWait(DispatchFamily::Data)
        }
        DataState::AuthorizedFrameRequest { .. } => RadioTxDispatcherPhase::AuthorizedFrameRequest,
        DataState::AuthorizedFrameAcknowledgementWait { .. } => {
            RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
        }
        DataState::Return { .. } | DataState::InterfaceReturn { .. } => {
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Data)
        }
        DataState::AuthorizedFrameAcknowledgementFault { .. }
        | DataState::Disabled { .. }
        | DataState::Transitioning => RadioTxDispatcherPhase::Disabled,
    }
}

const fn authorized_frame_progress_step(
    progress: AuthorizedFrameAcknowledgementProgress,
) -> RadioTxDispatcherStep {
    match progress {
        AuthorizedFrameAcknowledgementProgress::Matched
        | AuthorizedFrameAcknowledgementProgress::NotRetained(_) => RadioTxDispatcherStep::Advanced,
        AuthorizedFrameAcknowledgementProgress::Disabled(fault) => {
            RadioTxDispatcherStep::Disabled(fault)
        }
    }
}

fn ordinary_phase(state: &OrdinaryState) -> RadioTxDispatcherPhase {
    match state {
        OrdinaryState::Idle => RadioTxDispatcherPhase::Idle,
        OrdinaryState::Job(_) => RadioTxDispatcherPhase::JobReady(DispatchFamily::Ordinary),
        OrdinaryState::Access { access, .. } => match access.phase() {
            LogicalPacketAccessPhase::BackingOff => {
                RadioTxDispatcherPhase::BackingOff(DispatchFamily::Ordinary)
            }
            LogicalPacketAccessPhase::AwaitingCad => {
                RadioTxDispatcherPhase::CadReady(DispatchFamily::Ordinary)
            }
            _ => RadioTxDispatcherPhase::Disabled,
        },
        OrdinaryState::CadInFlight { .. } => {
            RadioTxDispatcherPhase::CadInFlight(DispatchFamily::Ordinary)
        }
        OrdinaryState::PermitSend { .. } => {
            RadioTxDispatcherPhase::PermitSend(DispatchFamily::Ordinary)
        }
        OrdinaryState::PermitWait { .. } | OrdinaryState::PermitReply { .. } => {
            RadioTxDispatcherPhase::PermitWait(DispatchFamily::Ordinary)
        }
        OrdinaryState::TransmitReady { .. } => {
            RadioTxDispatcherPhase::TransmitReady(DispatchFamily::Ordinary)
        }
        OrdinaryState::TxInFlight { .. } => {
            RadioTxDispatcherPhase::TransmitInFlight(DispatchFamily::Ordinary)
        }
        OrdinaryState::Expired { .. } | OrdinaryState::Unpermitted { .. } => {
            RadioTxDispatcherPhase::PermitWait(DispatchFamily::Ordinary)
        }
        OrdinaryState::Return { .. } | OrdinaryState::InterfaceReturn { .. } => {
            RadioTxDispatcherPhase::CompletionReturn(DispatchFamily::Ordinary)
        }
        OrdinaryState::Disabled { .. } | OrdinaryState::Transitioning => {
            RadioTxDispatcherPhase::Disabled
        }
    }
}

const fn monotonic_millis_ceiling(now_us: u64) -> MonotonicMillis {
    let whole_ms = now_us / 1_000;
    let partial_ms = if now_us.is_multiple_of(1_000) { 0 } else { 1 };
    MonotonicMillis::new(whole_ms + partial_ms)
}

fn physical_completion_millis(
    progress: PacketTxProgress,
    frame_count: u8,
) -> Option<MonotonicMillis> {
    let final_frame = usize::from(frame_count.saturating_sub(1));
    // A missing timestamp does not erase completed-frame evidence. Returning
    // `None` deliberately selects the conservative authorized-completion path,
    // whose receipt boundary is the node owner's later reconciliation sample.
    progress
        .frame_completed_at_us(final_frame)
        .map(monotonic_millis_ceiling)
}

/// Current Embassy monotonic time in whole microseconds.
pub fn embassy_now_us() -> u64 {
    Instant::now().as_micros()
}

/// Wait until an absolute Embassy monotonic microsecond timestamp.
pub async fn embassy_wait_until_us(deadline_us: u64) {
    Timer::at(Instant::from_micros(deadline_us)).await;
}

#[cfg(test)]
mod tests;
