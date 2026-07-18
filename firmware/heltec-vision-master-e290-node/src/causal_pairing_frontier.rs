//! Pure ordering policy for the node-owned pre-authentication frontier.
//!
//! The USB bearer records event time before transferring commands through two
//! owning handoffs. This selector restores that causal order at the sole node
//! owner without depending on executor scheduling or channel polling order.

/// Pairing command lane selected for the next semantic step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingCommandLane {
    /// Authenticated-session credential selection.
    SessionAdmission,
    /// Secret-bearing Begin, ProofStart, Activate, or AbortCurrent request.
    Live,
    /// Connection, button, exclusive-acquisition, or initialization control.
    Control,
}

/// Select the oldest captured pairing command.
///
/// Wire requests win equal timestamps over scalar control observations. The
/// sole USB producer can transfer a live or session-admission request and then
/// observe its disconnect in the same millisecond; admitting the request first
/// lets the resident policy apply the later disconnect instead of manufacturing
/// a pre-request closure. Session admission wins the otherwise-unreachable tie
/// between the two mutually exclusive wire-request families.
pub const fn select_pairing_command_lane(
    session_admission_at_millis: Option<u64>,
    live_at_millis: Option<u64>,
    control_at_millis: Option<u64>,
) -> Option<PairingCommandLane> {
    let mut selected = match control_at_millis {
        Some(at) => Some((at, PairingCommandLane::Control)),
        None => None,
    };
    if let Some(at) = live_at_millis {
        match selected {
            Some((selected_at, _)) if selected_at < at => {}
            _ => selected = Some((at, PairingCommandLane::Live)),
        }
    }
    if let Some(at) = session_admission_at_millis {
        match selected {
            Some((selected_at, _)) if selected_at < at => {}
            _ => selected = Some((at, PairingCommandLane::SessionAdmission)),
        }
    }
    match selected {
        Some((_, lane)) => Some(lane),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{PairingCommandLane, select_pairing_command_lane};

    #[test]
    fn absent_and_single_lane_inputs_are_exact() {
        assert_eq!(select_pairing_command_lane(None, None, None), None);
        assert_eq!(
            select_pairing_command_lane(None, Some(7), None),
            Some(PairingCommandLane::Live)
        );
        assert_eq!(
            select_pairing_command_lane(None, None, Some(7)),
            Some(PairingCommandLane::Control)
        );
        assert_eq!(
            select_pairing_command_lane(Some(7), None, None),
            Some(PairingCommandLane::SessionAdmission)
        );
    }

    #[test]
    fn captured_time_restores_cross_handoff_causal_order() {
        assert_eq!(
            select_pairing_command_lane(None, Some(6), Some(7)),
            Some(PairingCommandLane::Live)
        );
        assert_eq!(
            select_pairing_command_lane(None, Some(8), Some(7)),
            Some(PairingCommandLane::Control)
        );
        assert_eq!(
            select_pairing_command_lane(Some(5), Some(6), Some(7)),
            Some(PairingCommandLane::SessionAdmission)
        );
    }

    #[test]
    fn equal_time_wire_request_precedes_later_disconnect() {
        assert_eq!(
            select_pairing_command_lane(None, Some(7), Some(7)),
            Some(PairingCommandLane::Live)
        );
        assert_eq!(
            select_pairing_command_lane(Some(7), None, Some(7)),
            Some(PairingCommandLane::SessionAdmission)
        );
        assert_eq!(
            select_pairing_command_lane(Some(7), Some(7), Some(7)),
            Some(PairingCommandLane::SessionAdmission)
        );
    }
}
