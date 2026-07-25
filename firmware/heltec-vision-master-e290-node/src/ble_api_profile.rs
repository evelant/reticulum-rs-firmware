//! Fixed, host-checkable policy for the opt-in E290 BLE API proof.
//!
//! BLE carries the existing ordered RDA1 stream; it is neither a Reticulum
//! packet interface nor a second framing protocol. The first proof deliberately
//! retains exactly one indication until its ATT confirmation makes delivery
//! unambiguous.

use crate::usb_authenticated_session::UsbAuthenticatedSessionPhase;

/// Milliseconds allowed for the central to confirm one indication.
pub const INDICATION_CONFIRM_TIMEOUT_MS: u64 = 5_000;
/// Milliseconds allowed for a connected central to enable TX indications.
pub const CCCD_SUBSCRIBE_TIMEOUT_MS: u64 = 15_000;
/// Milliseconds allowed to reach the first authenticated `Established` phase.
///
/// This absolute, non-refreshing deadline starts after the indication CCCD and
/// connection lifecycle have been accepted. Partial framing, admission
/// pressure, and handshake progress do not extend it. Once authentication
/// succeeds, authenticated idle/session policy is a separate concern.
pub const PRE_AUTHENTICATION_TIMEOUT_MS: u64 = 30_000;
/// Milliseconds allowed for the one-time OS Bluetooth pairing ceremony.
///
/// Trouble 0.6 applies Bluetooth's fixed 30-second SMP inactivity timer. Keep
/// the product deadline explicit and aligned with it; the retained pre-SMP
/// link and the application-pairing window have separate, longer ownership.
pub const BLE_SECURITY_PAIRING_TIMEOUT_MS: u64 = 30_000;
/// Milliseconds between idle authenticated-session progress turns.
pub const API_POLL_INTERVAL_MS: u64 = 1;
/// Milliseconds between fail-closed BLE disconnect-drain observations.
pub const DISCONNECT_DRAIN_RECHECK_INTERVAL_MS: u64 = 25;
/// Milliseconds before, and between, warnings for a stalled disconnect drain.
///
/// This is a logging interval, not a teardown deadline. A warning never permits
/// the next advertiser to start. Releasing the last Trouble connection owner
/// before its exact `Disconnected` event can race controller teardown with the
/// next advertiser.
pub const DISCONNECT_DRAIN_PROLONGED_LOG_MS: u64 = 5_000;
/// Maximum time the bearer waits for one node-side control, live-pairing, or
/// bond-commit exchange.
///
/// A moved command remains owned by the bounded handoff or node actor. A bond
/// timeout additionally fail-stops BLE for the boot, so a late durable outcome
/// can never authorize Trouble's provisional bond in that incarnation.
pub const HANDOFF_EXCHANGE_TIMEOUT_MS: u64 = 10_000;
/// Milliseconds of application-pairing idle time admitted per connection.
pub const APPLICATION_PAIRING_IDLE_TIMEOUT_MS: u64 = 300_000;
/// Number of BLE links admitted by this proof.
pub const CONNECTIONS_MAX: usize = reticulum_device_api_ble::MAX_CONNECTIONS;
/// Controller activity slots reserved for one advertiser and one ACL link.
///
/// The pinned esp-radio 0.18 `Config::with_max_connections` API is a misnomer:
/// it writes Espressif's `ble_max_act`, whose unit is concurrent BLE
/// activities rather than established connections. Advertising consumes one
/// activity and the eventual sole connection consumes another.
pub const CONTROLLER_ACTIVITY_MAX: usize = 2;
/// L2CAP channels retained for signaling and ATT.
pub const L2CAP_CHANNELS_MAX: usize = 2;

/// Derive a stable static-random BLE address from the E290 eFuse EUI-48.
///
/// Trouble's `BdAddr` byte representation is little-endian relative to its
/// displayed address, so the input is reversed first. Bluetooth static-random
/// addresses require the two most-significant address bits to be `11`; those
/// bits therefore belong in output byte 5.
pub const fn static_random_address(eui48: [u8; 6]) -> [u8; 6] {
    let mut address = [eui48[5], eui48[4], eui48[3], eui48[2], eui48[1], eui48[0]];
    address[5] = (address[5] & 0x3f) | 0xc0;
    address
}

/// Failure to preserve the one-confirmation/one-fragment ownership rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndicationGateError {
    /// A second fragment was offered while the first still awaited confirmation.
    AlreadyPending,
    /// A confirmation arrived without one uniquely pending fragment.
    UnexpectedConfirmation,
    /// A zero-byte fragment cannot own an indication.
    EmptyFragment,
    /// A fragment exceeded the fixed initial ATT value bound.
    OversizedFragment,
}

/// Result of polling the connection's first-authentication deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreAuthenticationDeadlineStatus {
    /// The first authenticated session has not yet been established.
    Waiting,
    /// The connection reached `Established` before the absolute deadline.
    Authenticated,
    /// The absolute deadline elapsed before authentication completed.
    Expired,
}

/// Non-refreshing policy owner for a BLE connection's first authentication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreAuthenticationDeadline {
    status: PreAuthenticationDeadlineStatus,
}

impl PreAuthenticationDeadline {
    /// Start a fresh connection in the waiting state.
    pub const fn new() -> Self {
        Self {
            status: PreAuthenticationDeadlineStatus::Waiting,
        }
    }

    /// Observe session progress at `elapsed_millis` since lifecycle acceptance.
    ///
    /// The deadline is exclusive: first reaching `Established` at or after the
    /// timeout is too late. Both authenticated and expired outcomes are
    /// terminal, so later session phases cannot accidentally re-arm the policy.
    pub fn poll(
        &mut self,
        elapsed_millis: u64,
        phase: UsbAuthenticatedSessionPhase,
    ) -> PreAuthenticationDeadlineStatus {
        if self.status != PreAuthenticationDeadlineStatus::Waiting {
            return self.status;
        }
        if elapsed_millis >= PRE_AUTHENTICATION_TIMEOUT_MS {
            self.status = PreAuthenticationDeadlineStatus::Expired;
        } else if phase == UsbAuthenticatedSessionPhase::Established {
            self.status = PreAuthenticationDeadlineStatus::Authenticated;
        }
        self.status
    }
}

impl Default for PreAuthenticationDeadline {
    fn default() -> Self {
        Self::new()
    }
}

/// Deadline which counts application-pairing idle time but not response flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationPairingIdleDeadline {
    last_observed_millis: Option<u64>,
    idle_millis: u64,
}

impl ApplicationPairingIdleDeadline {
    /// Construct a stopped deadline.
    pub const fn new() -> Self {
        Self {
            last_observed_millis: None,
            idle_millis: 0,
        }
    }

    /// Start the deadline once, preserving an existing owner's elapsed time.
    pub fn ensure_started(&mut self, now_millis: u64) {
        if self.last_observed_millis.is_none() {
            self.last_observed_millis = Some(now_millis);
        }
    }

    /// Advance time and report whether the idle budget has expired.
    ///
    /// Time observed while `response_flight_owned` is true is deliberately not
    /// charged. The next idle interval begins at this observation rather than
    /// immediately expiring on time accumulated during indication backpressure.
    pub fn poll(&mut self, now_millis: u64, response_flight_owned: bool) -> bool {
        let Some(last) = self.last_observed_millis else {
            return false;
        };
        self.last_observed_millis = Some(now_millis);
        if !response_flight_owned {
            self.idle_millis = self
                .idle_millis
                .saturating_add(now_millis.saturating_sub(last));
        }
        self.idle_millis >= APPLICATION_PAIRING_IDLE_TIMEOUT_MS
    }

    /// Whether application pairing has started.
    pub const fn is_started(self) -> bool {
        self.last_observed_millis.is_some()
    }
}

impl Default for ApplicationPairingIdleDeadline {
    fn default() -> Self {
        Self::new()
    }
}

/// Exact one-fragment owner retained until an ATT confirmation arrives.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndicationGate {
    pending_bytes: Option<usize>,
}

impl IndicationGate {
    /// Construct an empty indication gate.
    pub const fn new() -> Self {
        Self {
            pending_bytes: None,
        }
    }

    /// Whether exactly one indication is awaiting confirmation.
    pub const fn is_pending(self) -> bool {
        self.pending_bytes.is_some()
    }

    /// Retain the exact fragment length before sending its indication.
    pub fn arm(&mut self, bytes: usize) -> Result<(), IndicationGateError> {
        if bytes == 0 {
            return Err(IndicationGateError::EmptyFragment);
        }
        if bytes > reticulum_device_api_ble::INITIAL_ATT_VALUE_BYTES {
            return Err(IndicationGateError::OversizedFragment);
        }
        if self.pending_bytes.is_some() {
            return Err(IndicationGateError::AlreadyPending);
        }
        self.pending_bytes = Some(bytes);
        Ok(())
    }

    /// Consume the uniquely pending fragment after its ATT confirmation.
    pub fn confirm(&mut self) -> Result<usize, IndicationGateError> {
        self.pending_bytes
            .take()
            .ok_or(IndicationGateError::UnexpectedConfirmation)
    }

    /// Drop any connection-scoped indication owner without acknowledging it.
    pub fn reset(&mut self) {
        self.pending_bytes = None;
    }
}

const _: () = assert!(CONNECTIONS_MAX == 1);
const _: () = assert!(CONTROLLER_ACTIVITY_MAX == CONNECTIONS_MAX + 1);
const _: () = assert!(L2CAP_CHANNELS_MAX == 2);
const _: () = assert!(INDICATION_CONFIRM_TIMEOUT_MS > 0);
const _: () = assert!(CCCD_SUBSCRIBE_TIMEOUT_MS > INDICATION_CONFIRM_TIMEOUT_MS);
const _: () = assert!(PRE_AUTHENTICATION_TIMEOUT_MS > CCCD_SUBSCRIBE_TIMEOUT_MS);
const _: () = assert!(BLE_SECURITY_PAIRING_TIMEOUT_MS >= PRE_AUTHENTICATION_TIMEOUT_MS);
const _: () = assert!(DISCONNECT_DRAIN_RECHECK_INTERVAL_MS > 0);
const _: () = assert!(DISCONNECT_DRAIN_PROLONGED_LOG_MS > DISCONNECT_DRAIN_RECHECK_INTERVAL_MS);
const _: () = assert!(HANDOFF_EXCHANGE_TIMEOUT_MS > API_POLL_INTERVAL_MS);
const _: () = assert!(APPLICATION_PAIRING_IDLE_TIMEOUT_MS > HANDOFF_EXCHANGE_TIMEOUT_MS);

#[cfg(test)]
mod tests {
    use super::{
        APPLICATION_PAIRING_IDLE_TIMEOUT_MS, ApplicationPairingIdleDeadline,
        BLE_SECURITY_PAIRING_TIMEOUT_MS, CONNECTIONS_MAX, CONTROLLER_ACTIVITY_MAX, IndicationGate,
        IndicationGateError, PRE_AUTHENTICATION_TIMEOUT_MS, PreAuthenticationDeadline,
        PreAuthenticationDeadlineStatus, static_random_address,
    };
    use crate::usb_authenticated_session::UsbAuthenticatedSessionPhase;

    #[test]
    fn controller_activity_budget_keeps_one_advertiser_separate_from_one_link() {
        assert_eq!(CONNECTIONS_MAX, 1);
        assert_eq!(CONTROLLER_ACTIVITY_MAX, 2);
    }

    #[test]
    fn human_pairing_deadlines_are_forgiving_without_extending_ordinary_authentication() {
        assert_eq!(PRE_AUTHENTICATION_TIMEOUT_MS, 30_000);
        assert_eq!(BLE_SECURITY_PAIRING_TIMEOUT_MS, 30_000);
        assert_eq!(APPLICATION_PAIRING_IDLE_TIMEOUT_MS, 300_000);
    }

    #[test]
    fn static_random_address_is_stable_little_endian_and_sets_byte_five_bits() {
        let address = static_random_address([0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88]);
        assert_eq!(address, [0x88, 0x3e, 0xe1, 0x04, 0xa7, 0xec]);
        assert_eq!(address[5] & 0xc0, 0xc0);
        assert_eq!(address[0], 0x88);
        assert_ne!(
            address,
            static_random_address([0xac, 0xa7, 0x04, 0xe1, 0x3f, 0x88])
        );
    }

    #[test]
    fn indication_gate_never_acknowledges_ambiguous_or_empty_fragments() {
        let mut gate = IndicationGate::new();
        assert_eq!(
            gate.confirm(),
            Err(IndicationGateError::UnexpectedConfirmation)
        );
        assert_eq!(gate.arm(0), Err(IndicationGateError::EmptyFragment));
        assert_eq!(
            gate.arm(reticulum_device_api_ble::INITIAL_ATT_VALUE_BYTES + 1),
            Err(IndicationGateError::OversizedFragment)
        );
        gate.arm(20).unwrap();
        assert!(gate.is_pending());
        assert_eq!(gate.arm(1), Err(IndicationGateError::AlreadyPending));
        assert_eq!(gate.confirm(), Ok(20));
        assert!(!gate.is_pending());
        assert_eq!(
            gate.confirm(),
            Err(IndicationGateError::UnexpectedConfirmation)
        );
    }

    #[test]
    fn application_pairing_idle_deadline_suspends_complete_response_flight() {
        let mut deadline = ApplicationPairingIdleDeadline::new();
        assert!(!deadline.is_started());
        deadline.ensure_started(10);
        assert!(deadline.is_started());

        assert!(!deadline.poll(1_010, false));
        assert!(!deadline.poll(81_010, true));
        let just_before_expiry = 81_010 + APPLICATION_PAIRING_IDLE_TIMEOUT_MS - 1_001;
        assert!(!deadline.poll(just_before_expiry, false));
        assert!(deadline.poll(just_before_expiry + 1, false));
    }

    #[test]
    fn reset_drops_but_never_confirms_pending_bytes() {
        let mut gate = IndicationGate::new();
        gate.arm(7).unwrap();
        gate.reset();
        assert!(!gate.is_pending());
        assert_eq!(
            gate.confirm(),
            Err(IndicationGateError::UnexpectedConfirmation)
        );
    }

    #[test]
    fn no_client_hello_expires_at_the_absolute_deadline() {
        let mut deadline = PreAuthenticationDeadline::new();
        assert_eq!(
            deadline.poll(0, UsbAuthenticatedSessionPhase::AwaitingClientHello),
            PreAuthenticationDeadlineStatus::Waiting
        );
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS - 1,
                UsbAuthenticatedSessionPhase::AwaitingClientHello
            ),
            PreAuthenticationDeadlineStatus::Waiting
        );
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS,
                UsbAuthenticatedSessionPhase::AwaitingClientHello
            ),
            PreAuthenticationDeadlineStatus::Expired
        );
    }

    #[test]
    fn partial_client_hello_does_not_refresh_the_deadline() {
        let mut deadline = PreAuthenticationDeadline::new();
        // A partial stream has not yielded a record, so the session remains in
        // AwaitingClientHello regardless of how many fragments arrived.
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS / 2,
                UsbAuthenticatedSessionPhase::AwaitingClientHello
            ),
            PreAuthenticationDeadlineStatus::Waiting
        );
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS,
                UsbAuthenticatedSessionPhase::AwaitingClientHello
            ),
            PreAuthenticationDeadlineStatus::Expired
        );
    }

    #[test]
    fn stalled_hello_and_proof_flights_share_one_non_refreshing_deadline() {
        let mut deadline = PreAuthenticationDeadline::new();
        for (elapsed, phase) in [
            (1, UsbAuthenticatedSessionPhase::AdmissionCommandPending),
            (
                PRE_AUTHENTICATION_TIMEOUT_MS / 4,
                UsbAuthenticatedSessionPhase::AwaitingAdmissionReply,
            ),
            (
                PRE_AUTHENTICATION_TIMEOUT_MS / 2,
                UsbAuthenticatedSessionPhase::ServerHelloFlight,
            ),
            (
                PRE_AUTHENTICATION_TIMEOUT_MS - 1,
                UsbAuthenticatedSessionPhase::PendingClientProof,
            ),
        ] {
            assert_eq!(
                deadline.poll(elapsed, phase),
                PreAuthenticationDeadlineStatus::Waiting
            );
        }
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS,
                UsbAuthenticatedSessionPhase::PendingClientProof
            ),
            PreAuthenticationDeadlineStatus::Expired
        );
    }

    #[test]
    fn authentication_before_deadline_permanently_disarms_pre_auth_timeout() {
        let mut deadline = PreAuthenticationDeadline::new();
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS - 1,
                UsbAuthenticatedSessionPhase::Established
            ),
            PreAuthenticationDeadlineStatus::Authenticated
        );
        assert_eq!(
            deadline.poll(u64::MAX, UsbAuthenticatedSessionPhase::AwaitingReply),
            PreAuthenticationDeadlineStatus::Authenticated
        );
    }
}
