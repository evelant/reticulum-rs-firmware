//! Pure ordering policy for the node-owned pre-authentication frontier.
//!
//! The USB bearer records event time before transferring commands through two
//! owning handoffs. This selector restores that causal order at the sole node
//! owner without depending on executor scheduling or channel polling order.

/// Pairing command lane selected for the next semantic step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingCommandLane {
    /// Secret-bearing Begin, ProofStart, Activate, or AbortCurrent request.
    Live,
    /// Connection, button, exclusive-acquisition, or initialization control.
    Control,
}

/// Select the oldest captured pairing command.
///
/// Equal timestamps select [`PairingCommandLane::Live`]. The sole USB producer
/// can transfer a live request and then observe its disconnect in the same
/// millisecond; admitting the request first lets the resident policy apply the
/// later disconnect instead of manufacturing a pre-request closure.
pub const fn select_pairing_command_lane(
    live_at_millis: Option<u64>,
    control_at_millis: Option<u64>,
) -> Option<PairingCommandLane> {
    match (live_at_millis, control_at_millis) {
        (Some(live), Some(control)) if live <= control => Some(PairingCommandLane::Live),
        (Some(_), Some(_)) => Some(PairingCommandLane::Control),
        (Some(_), None) => Some(PairingCommandLane::Live),
        (None, Some(_)) => Some(PairingCommandLane::Control),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{PairingCommandLane, select_pairing_command_lane};

    #[test]
    fn absent_and_single_lane_inputs_are_exact() {
        assert_eq!(select_pairing_command_lane(None, None), None);
        assert_eq!(
            select_pairing_command_lane(Some(7), None),
            Some(PairingCommandLane::Live)
        );
        assert_eq!(
            select_pairing_command_lane(None, Some(7)),
            Some(PairingCommandLane::Control)
        );
    }

    #[test]
    fn captured_time_restores_cross_handoff_causal_order() {
        assert_eq!(
            select_pairing_command_lane(Some(6), Some(7)),
            Some(PairingCommandLane::Live)
        );
        assert_eq!(
            select_pairing_command_lane(Some(8), Some(7)),
            Some(PairingCommandLane::Control)
        );
    }

    #[test]
    fn equal_time_live_request_precedes_later_disconnect() {
        assert_eq!(
            select_pairing_command_lane(Some(7), Some(7)),
            Some(PairingCommandLane::Live)
        );
    }
}
