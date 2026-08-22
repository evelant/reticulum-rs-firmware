//! Bounded product policy for durable inbound LXMF carriers.
//!
//! PRNS owns Reticulum delivery and Python-compatible immediate proof timing.
//! This module classifies only failures that occur after the product has copied
//! an ordinary LXMF carrier and attempts to make it durable.

use reticulum_lxmf_durable_ingress::DurableCarrierRetentionReason;
use reticulum_lxmf_ingress::WireLimits;
use reticulum_lxmf_model::MessageId;
use reticulum_lxmf_store::LxmfCommitError;

use crate::product_config;

/// Consecutive ordinary packet-receipt timeouts before the product asks PRNS
/// to rediscover the selected path.
///
/// Python LXMF 1.0.1 begins stale-path recovery after two opportunistic
/// attempts. The E290 keeps the message intent durable beyond that volatile
/// recovery cycle.
pub const OUTBOUND_TIMEOUTS_BEFORE_PATH_REDISCOVERY: u8 = 2;

/// Volatile retry evidence for one currently pending durable outbound intent.
///
/// Reticulum route, receipt, and proof state remains wholly owned by PRNS. This
/// counter only tells the product when to invoke PRNS's public path-request
/// API, matching Python LXMF's stale-path recovery boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LxmfOutboundRetryState {
    consecutive_timeouts: u8,
}

impl LxmfOutboundRetryState {
    /// Record one ordinary packet-receipt timeout.
    ///
    /// Returns `true` exactly when the selected path should be rediscovered.
    /// The counter restarts after that recovery action so a durable intent can
    /// continue retrying across temporary network outages.
    pub fn note_timeout(&mut self) -> bool {
        self.consecutive_timeouts = self.consecutive_timeouts.saturating_add(1);
        if self.consecutive_timeouts >= OUTBOUND_TIMEOUTS_BEFORE_PATH_REDISCOVERY {
            self.consecutive_timeouts = 0;
            true
        } else {
            false
        }
    }

    /// Clear volatile failure evidence after delivery or another route state.
    pub fn reset(&mut self) {
        self.consecutive_timeouts = 0;
    }

    /// Number of timeouts accumulated in the current recovery cycle.
    pub const fn consecutive_timeouts(self) -> u8 {
        self.consecutive_timeouts
    }
}

/// Why an exact copied carrier must be retried after bounded backoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfRetryClass {
    /// Backend completion is ambiguous; this exact candidate owns reconciliation.
    StoreReconcile,
    /// Another candidate owns store reconciliation and must run first.
    StoreBusy,
}

/// Explicit terminal policy for a carrier that cannot become a durable message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfTerminalReject {
    /// Carrier normalization or MessagePack structure was invalid.
    InvalidMessage,
    /// Parsed evidence contradicted the exact retained carrier or durable model.
    CandidateContradiction,
    /// The append-only store has no remaining physical capacity.
    StoreFull,
    /// The caller-owned semantic index has no remaining entry.
    IndexFull,
    /// Stable logical handles are exhausted under the selected format.
    HandleExhausted,
    /// Binding, committed media, or readback state failed closed.
    StoreFault,
    /// One message ID names conflicting authenticated material.
    HashCollision,
}

/// Product action for one portable durable-ingress retention reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfRetentionAction {
    /// Retain the copied carrier and retry it after bounded backoff.
    Retry(LxmfRetryClass),
    /// Retain the carrier without spinning after a pending fail-closed fault.
    HoldPendingFault,
    /// Dispose of the copied carrier under an explicit terminal policy.
    Terminal(LxmfTerminalReject),
}

/// Select the E290 product's bounded LXMF wire-validation profile.
pub const fn wire_limits() -> WireLimits {
    WireLimits::new(
        product_config::LXMF_MAX_WIRE_BYTES,
        product_config::LXMF_MAX_VALUE_BYTES,
        product_config::LXMF_MAX_CONTAINER_ITEMS,
        product_config::LXMF_MAX_TOTAL_VALUES,
        product_config::LXMF_MAX_SCAN_STEPS,
        product_config::LXMF_MAX_NESTING_DEPTH,
    )
}

/// Classify a retained PRNS-native carrier without retaining Reticulum state.
///
/// A pending flash mutation retains the exact copied carrier even when its
/// surface error would otherwise be terminal. PRNS proof behavior has already
/// completed independently before this application-storage operation begins.
pub const fn carrier_retention_action<E>(
    reason: &DurableCarrierRetentionReason<E>,
    pending_after_call: Option<MessageId>,
) -> LxmfRetentionAction {
    if pending_after_call.is_some() {
        match reason {
            DurableCarrierRetentionReason::Store(LxmfCommitError::Backend { .. }) => {
                return LxmfRetentionAction::Retry(LxmfRetryClass::StoreReconcile);
            }
            DurableCarrierRetentionReason::Store(
                LxmfCommitError::Binding(_)
                | LxmfCommitError::Fault(_)
                | LxmfCommitError::Full { .. }
                | LxmfCommitError::IndexFull { .. }
                | LxmfCommitError::HashCollision { .. }
                | LxmfCommitError::HandleExhausted,
            ) => return LxmfRetentionAction::HoldPendingFault,
            _ => {}
        }
    }

    match reason {
        DurableCarrierRetentionReason::Rejected(_) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::InvalidMessage)
        }
        DurableCarrierRetentionReason::Candidate(_) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::CandidateContradiction)
        }
        DurableCarrierRetentionReason::Store(LxmfCommitError::Backend { .. }) => {
            LxmfRetentionAction::Retry(LxmfRetryClass::StoreReconcile)
        }
        DurableCarrierRetentionReason::Store(LxmfCommitError::AmbiguousMutationPending {
            ..
        }) => LxmfRetentionAction::Retry(LxmfRetryClass::StoreBusy),
        DurableCarrierRetentionReason::Store(LxmfCommitError::Full { .. }) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFull)
        }
        DurableCarrierRetentionReason::Store(LxmfCommitError::IndexFull { .. }) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::IndexFull)
        }
        DurableCarrierRetentionReason::Store(LxmfCommitError::HandleExhausted) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::HandleExhausted)
        }
        DurableCarrierRetentionReason::Store(
            LxmfCommitError::Binding(_) | LxmfCommitError::Fault(_),
        ) => LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFault),
        DurableCarrierRetentionReason::Store(LxmfCommitError::HashCollision { .. }) => {
            LxmfRetentionAction::Terminal(LxmfTerminalReject::HashCollision)
        }
    }
}

#[cfg(test)]
mod tests {
    use reticulum_lxmf_durable_ingress::DurableCarrierRetentionReason;
    use reticulum_lxmf_ingress::RejectedIngress;
    use reticulum_lxmf_model::MessageId;
    use reticulum_lxmf_store::{LxmfCommitError, LxmfProgramStage};
    use reticulum_lxmf_wire::WireError;

    use super::{
        LxmfOutboundRetryState, LxmfRetentionAction, LxmfRetryClass, LxmfTerminalReject,
        carrier_retention_action,
    };

    #[test]
    fn outbound_timeout_pair_requests_path_rediscovery_and_restarts_the_cycle() {
        let mut state = LxmfOutboundRetryState::default();

        assert!(!state.note_timeout());
        assert_eq!(state.consecutive_timeouts(), 1);
        assert!(state.note_timeout());
        assert_eq!(state.consecutive_timeouts(), 0);
        assert!(!state.note_timeout());
        state.reset();
        assert_eq!(state.consecutive_timeouts(), 0);
    }

    #[test]
    fn malformed_carrier_is_terminal_after_prns_delivery() {
        let reason = DurableCarrierRetentionReason::<()>::Rejected(RejectedIngress::Wire(
            WireError::TooShort {
                minimum: 1,
                actual: 0,
            },
        ));
        assert_eq!(
            carrier_retention_action(&reason, None),
            LxmfRetentionAction::Terminal(LxmfTerminalReject::InvalidMessage)
        );
    }

    #[test]
    fn ambiguous_backend_retains_exact_candidate_for_reconciliation() {
        let reason = DurableCarrierRetentionReason::Store(LxmfCommitError::Backend {
            stage: LxmfProgramStage::Commit,
            error: (),
        });
        assert_eq!(
            carrier_retention_action(&reason, None),
            LxmfRetentionAction::Retry(LxmfRetryClass::StoreReconcile)
        );
    }

    #[test]
    fn another_pending_candidate_defers_without_acquiring_its_authority() {
        let reason =
            DurableCarrierRetentionReason::<()>::Store(LxmfCommitError::AmbiguousMutationPending {
                message_id: MessageId::new([7; 32]),
            });
        assert_eq!(
            carrier_retention_action(&reason, None),
            LxmfRetentionAction::Retry(LxmfRetryClass::StoreBusy)
        );
    }

    #[test]
    fn terminal_capacity_error_is_held_when_a_mutation_remains_pending() {
        let reason = DurableCarrierRetentionReason::<()>::Store(LxmfCommitError::Full {
            required_extents: 2,
            remaining_extents: 1,
        });
        assert_eq!(
            carrier_retention_action(&reason, None),
            LxmfRetentionAction::Terminal(LxmfTerminalReject::StoreFull)
        );
        assert_eq!(
            carrier_retention_action(&reason, Some(MessageId::new([9; 32]))),
            LxmfRetentionAction::HoldPendingFault
        );
    }
}
