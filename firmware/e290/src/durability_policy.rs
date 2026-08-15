//! Product policy for local durability-service degradation and retry.
//!
//! A permanent fault before an authorized DATA frame is active disables only
//! local durable submission service. Once an unresolved authorized frame
//! exists, however, the sole LoRa actor cannot release its completion or
//! router ticket without weakening persist-before-ack. That case therefore
//! fail-stops the LoRa interface for the remainder of the boot.

/// Authorized-frame work visible at the node/storage boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizedFrameDurability {
    /// No authorized frame is currently retained by this boundary.
    None,
    /// An exact authorized frame still requires durable projection.
    Unresolved,
    /// Projection is already durable and only its exact acknowledgement remains queued.
    DurableAcknowledgementPending,
}

/// Product state of the resident local durability service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurabilityServiceState {
    /// The mounted runtime may accept frame observations and run storage work.
    Ready,
    /// An ambiguous backend result or serialization pressure retains exact work.
    Retrying {
        /// Earliest monotonic millisecond at which physical work may be retried.
        retry_not_before_ms: u64,
    },
    /// Local durable service is unavailable, but no gated DATA owner exists.
    DisabledRouteOnly,
    /// An unresolved DATA owner is retained and the LoRa interface is fail-stopped.
    ActiveOwnerFailStopped,
}

impl DurabilityServiceState {
    /// Construct the initial state from the optional mounted runtime.
    pub const fn from_runtime_available(available: bool) -> Self {
        if available {
            Self::Ready
        } else {
            Self::DisabledRouteOnly
        }
    }

    /// Whether projection-only authorized-frame offers may enter the runtime.
    pub const fn can_offer_authorized_frame(self) -> bool {
        matches!(self, Self::Ready | Self::Retrying { .. })
    }

    /// Whether one physical runtime step is due at `now_ms`.
    pub const fn storage_step_due(self, now_ms: u64) -> bool {
        match self {
            Self::Ready => true,
            Self::Retrying {
                retry_not_before_ms,
            } => now_ms >= retry_not_before_ms,
            Self::DisabledRouteOnly | Self::ActiveOwnerFailStopped => false,
        }
    }

    /// Record useful or idle runtime progress after a successful storage call.
    pub const fn runtime_progress(self) -> Self {
        match self {
            Self::Ready | Self::Retrying { .. } => Self::Ready,
            Self::DisabledRouteOnly | Self::ActiveOwnerFailStopped => self,
        }
    }

    /// Retain exact work and defer its next physical retry.
    pub const fn retry_at(self, retry_not_before_ms: u64) -> Self {
        match self {
            Self::Ready | Self::Retrying { .. } => Self::Retrying {
                retry_not_before_ms,
            },
            Self::DisabledRouteOnly | Self::ActiveOwnerFailStopped => self,
        }
    }

    /// Apply a permanent runtime/storage failure without inventing durability.
    ///
    /// A durable acknowledgement already waiting for channel capacity remains
    /// releasable: its persistence proof preceded this failure. Only an
    /// unresolved frame requires the interface-local fail-stop.
    pub const fn permanent_failure(self, frame: AuthorizedFrameDurability) -> Self {
        if matches!(self, Self::ActiveOwnerFailStopped)
            || matches!(frame, AuthorizedFrameDurability::Unresolved)
        {
            Self::ActiveOwnerFailStopped
        } else {
            Self::DisabledRouteOnly
        }
    }

    /// Observe a newly received authorized-frame request.
    ///
    /// This closes the race where a permanent storage failure occurs after the
    /// node scans the request channel but before the dispatcher queues its
    /// frame. A request arriving after route-only degradation promotes the
    /// interface to the same active-owner fail-stop.
    pub const fn observe_authorized_frame_request(self) -> Self {
        match self {
            Self::DisabledRouteOnly | Self::ActiveOwnerFailStopped => Self::ActiveOwnerFailStopped,
            Self::Ready | Self::Retrying { .. } => self,
        }
    }

    /// Whether this boot must perform no further LoRa radio operations.
    pub const fn is_active_owner_fail_stopped(self) -> bool {
        matches!(self, Self::ActiveOwnerFailStopped)
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthorizedFrameDurability, DurabilityServiceState};

    #[test]
    fn optional_runtime_selects_ready_or_route_only() {
        assert_eq!(
            DurabilityServiceState::from_runtime_available(true),
            DurabilityServiceState::Ready
        );
        assert_eq!(
            DurabilityServiceState::from_runtime_available(false),
            DurabilityServiceState::DisabledRouteOnly
        );
    }

    #[test]
    fn retry_pressure_preserves_service_and_obeys_deadline() {
        let retrying = DurabilityServiceState::Ready.retry_at(1_500);
        assert_eq!(
            retrying,
            DurabilityServiceState::Retrying {
                retry_not_before_ms: 1_500
            }
        );
        assert!(retrying.can_offer_authorized_frame());
        assert!(!retrying.storage_step_due(1_499));
        assert!(retrying.storage_step_due(1_500));
        assert_eq!(retrying.runtime_progress(), DurabilityServiceState::Ready);
    }

    #[test]
    fn permanent_failure_without_unresolved_owner_degrades_route_only() {
        assert_eq!(
            DurabilityServiceState::Ready.permanent_failure(AuthorizedFrameDurability::None),
            DurabilityServiceState::DisabledRouteOnly
        );
        assert_eq!(
            DurabilityServiceState::Retrying {
                retry_not_before_ms: 20
            }
            .permanent_failure(AuthorizedFrameDurability::DurableAcknowledgementPending),
            DurabilityServiceState::DisabledRouteOnly,
            "already-durable acknowledgements remain releasable"
        );
    }

    #[test]
    fn unresolved_owner_selects_sticky_interface_fail_stop() {
        let stopped =
            DurabilityServiceState::Ready.permanent_failure(AuthorizedFrameDurability::Unresolved);
        assert_eq!(stopped, DurabilityServiceState::ActiveOwnerFailStopped);
        assert!(stopped.is_active_owner_fail_stopped());
        assert!(!stopped.can_offer_authorized_frame());
        assert!(!stopped.storage_step_due(u64::MAX));
        assert_eq!(stopped.runtime_progress(), stopped);
        assert_eq!(stopped.retry_at(1), stopped);
        assert_eq!(
            stopped.permanent_failure(AuthorizedFrameDurability::None),
            stopped
        );
        assert_eq!(stopped.observe_authorized_frame_request(), stopped);
    }

    #[test]
    fn frame_arriving_after_route_only_failure_promotes_to_fail_stop() {
        assert_eq!(
            DurabilityServiceState::DisabledRouteOnly.observe_authorized_frame_request(),
            DurabilityServiceState::ActiveOwnerFailStopped
        );
    }
}
