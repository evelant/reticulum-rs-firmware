//! Fixed, host-checkable policy for the E290 BLE API bearer.
//!
//! BLE carries the existing ordered RDA1 stream; it is neither a Reticulum
//! packet interface nor a second framing protocol. The bearer deliberately
//! retains exactly one indication until its ATT confirmation makes delivery
//! unambiguous.

use crate::authenticated_session::AuthenticatedSessionPhase;

/// Milliseconds allowed for the central to confirm one indication.
pub const INDICATION_CONFIRM_TIMEOUT_MS: u64 = 5_000;
/// Milliseconds allowed for a connected central to enable TX indications.
pub const CCCD_SUBSCRIBE_TIMEOUT_MS: u64 = 240_000;
/// Milliseconds allowed to reach the first authenticated `Established` phase.
///
/// This absolute, non-refreshing deadline starts when an authenticated,
/// authoritative Bluetooth peer enters the ordinary device-API lifecycle. On a
/// restored bond that can precede indication subscription, so this window
/// deliberately overlaps the independent CCCD deadline. Partial framing,
/// admission pressure, and handshake progress do not extend it. Once
/// authentication succeeds, authenticated idle/session policy is a separate
/// concern.
pub const PRE_AUTHENTICATION_TIMEOUT_MS: u64 = 300_000;
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
pub const HANDOFF_EXCHANGE_TIMEOUT_MS: u64 = 60_000;
/// Maximum time one nonblocking GPIO observation may remain node-owned.
///
/// Button sampling is human-facing and can overlap flash or radio work. Give
/// it a deliberately larger recovery envelope than atomic command exchanges so a
/// slow scheduler turn does not masquerade as a failed pairing ceremony.
pub const BUTTON_OBSERVATION_HANDOFF_TIMEOUT_MS: u64 = 120_000;
/// Milliseconds of application-pairing idle time admitted per connection.
pub const APPLICATION_PAIRING_IDLE_TIMEOUT_MS: u64 = 300_000;
/// Continuous boot-time GPIO21 hold required to authorize one BLE bond reset.
///
/// The caller must construct the gesture from its first boot observation of
/// the active-low user key. A released first observation, any later release,
/// or a clock regression permanently rejects recovery for that boot. This
/// policy emits authorization exactly once and does not itself mutate storage.
pub const BLE_BOND_BOOT_RECOVERY_HOLD_MS: u64 = 2_000;
/// Number of BLE links admitted by this bearer.
pub const CONNECTIONS_MAX: usize = reticulum_device_api_ble::MAX_CONNECTIONS;
/// Controller activity slots reserved for one advertiser and one ACL link.
///
/// The pinned esp-radio `Config::with_max_connections` API is a misnomer: it
/// writes Espressif's `ble_max_act`, whose unit is concurrent BLE activities
/// rather than established connections. Advertising consumes one activity and
/// the eventual sole connection consumes another.
pub const CONTROLLER_ACTIVITY_MAX: usize = 2;
/// L2CAP channels retained for signaling and ATT.
pub const L2CAP_CHANNELS_MAX: usize = 2;

/// Result of one boot-time BLE bond recovery gesture observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootBleBondRecoveryProgress {
    /// GPIO21 has remained asserted since boot but has not reached the hold
    /// duration.
    Pending,
    /// The continuous boot hold reached its duration and authorizes exactly one
    /// BLE bond reset.
    Authorized,
    /// The gesture was invalidated before authorization and cannot be retried
    /// without rebooting.
    Rejected,
    /// The sole authorization was already emitted.
    Consumed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BootBleBondRecoveryState {
    Pending,
    Rejected,
    Consumed,
}

/// Pure owner of the one-shot GPIO21-at-boot BLE bond recovery gesture.
///
/// `button_asserted` means the active-low GPIO21 input is electrically low.
/// Recovery is eligible only when the first boot sample is asserted and every
/// later sample remains asserted until [`BLE_BOND_BOOT_RECOVERY_HOLD_MS`].
/// Releasing and pressing again never repairs the gesture in the same boot.
#[must_use = "boot recovery authorization must be deliberately observed or rejected"]
pub struct BootBleBondRecoveryGesture {
    started_at_ms: u64,
    last_observed_at_ms: u64,
    state: BootBleBondRecoveryState,
}

impl BootBleBondRecoveryGesture {
    /// Begin from the caller's first GPIO21 observation of this boot.
    pub const fn new(now_ms: u64, button_asserted: bool) -> Self {
        Self {
            started_at_ms: now_ms,
            last_observed_at_ms: now_ms,
            state: if button_asserted {
                BootBleBondRecoveryState::Pending
            } else {
                BootBleBondRecoveryState::Rejected
            },
        }
    }

    /// Observe one later raw level at a monotonic millisecond timestamp.
    ///
    /// [`BootBleBondRecoveryProgress::Authorized`] is returned exactly once.
    /// Any release or clock regression fails closed for the remainder of the
    /// boot, so a short press or bounce cannot become a recovery gesture by
    /// accumulating asserted intervals.
    pub fn observe(&mut self, now_ms: u64, button_asserted: bool) -> BootBleBondRecoveryProgress {
        match self.state {
            BootBleBondRecoveryState::Rejected => {
                return BootBleBondRecoveryProgress::Rejected;
            }
            BootBleBondRecoveryState::Consumed => {
                return BootBleBondRecoveryProgress::Consumed;
            }
            BootBleBondRecoveryState::Pending => {}
        }

        if now_ms < self.last_observed_at_ms || !button_asserted {
            self.state = BootBleBondRecoveryState::Rejected;
            return BootBleBondRecoveryProgress::Rejected;
        }
        self.last_observed_at_ms = now_ms;

        if now_ms - self.started_at_ms < BLE_BOND_BOOT_RECOVERY_HOLD_MS {
            return BootBleBondRecoveryProgress::Pending;
        }

        self.state = BootBleBondRecoveryState::Consumed;
        BootBleBondRecoveryProgress::Authorized
    }
}

/// Post-connection cleanup for one attempted fresh SMP ceremony.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshSecurityDisposition {
    /// No provisional security material exists.
    Clean,
    /// Scrub non-authoritative host bonds and resume advertising.
    ScrubAndRetry,
    /// Scrub host bonds but reboot before trusting the durable bond store.
    ScrubAndDisable,
}

impl FreshSecurityDisposition {
    /// Classify cleanup from the two monotonic ceremony facts.
    pub const fn classify(pending_durability: bool, bond_reboot_required: bool) -> Self {
        match (pending_durability, bond_reboot_required) {
            (false, _) => Self::Clean,
            (true, false) => Self::ScrubAndRetry,
            (true, true) => Self::ScrubAndDisable,
        }
    }

    /// Whether Trouble's non-authoritative in-memory bonds must be removed.
    pub const fn scrub_non_authoritative_bonds(self) -> bool {
        !matches!(self, Self::Clean)
    }

    /// Whether a possibly late flash-commit outcome requires reboot remount.
    pub const fn disable_until_reboot(self) -> bool {
        matches!(self, Self::ScrubAndDisable)
    }
}

/// Exact result of handing one freshly negotiated bond to the flash owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleBondExchangeResult {
    /// The flash owner confirmed the exact durable successor.
    Durable,
    /// The flash owner definitively rejected the command.
    ExplicitFailure,
    /// Queue pressure retained the command locally until the exchange deadline.
    TimedOutBeforeSend,
    /// The command crossed the handoff, but no exact reply arrived by the deadline.
    TimedOutAfterSend,
}

impl BleBondExchangeResult {
    /// Classify a deadline from whether the command crossed the owning handoff.
    pub const fn timed_out(command_sent: bool) -> Self {
        if command_sent {
            Self::TimedOutAfterSend
        } else {
            Self::TimedOutBeforeSend
        }
    }

    /// Whether durable authority must be remounted before BLE can be trusted.
    pub const fn bond_reboot_required(self) -> bool {
        matches!(self, Self::ExplicitFailure | Self::TimedOutAfterSend)
    }
}

/// Derive a stable static-random BLE address from the durable node identity.
///
/// Trouble's `BdAddr` byte representation is little-endian relative to its
/// displayed address, so the first six canonical identity-hash bytes are
/// reversed. Bluetooth static-random addresses require the two
/// most-significant address bits to be `11`; those bits therefore belong in
/// output byte 5.
///
/// Ordinary boots reload the same durable Reticulum identity and therefore
/// preserve the local address. Erasing the identity partition causes first
/// provisioning to create a different identity and BLE address, preventing a
/// stale phone-side bond or peripheral cache from being attached to a freshly
/// initialized appliance.
pub const fn static_random_address(identity_hash: [u8; 16]) -> [u8; 6] {
    let mut address = [
        identity_hash[5],
        identity_hash[4],
        identity_hash[3],
        identity_hash[2],
        identity_hash[1],
        identity_hash[0],
    ];
    address[5] = (address[5] & 0x3f) | 0xc0;
    address
}

/// Complete public inputs for BLE local addressing and human discovery.
///
/// The static-random address follows the durable node identity, while the
/// unauthenticated local-name suffix remains bound to the physical board MAC.
/// Grouping the already-derived address with its separate name input prevents
/// the BLE task boundary from accidentally substituting one identity source
/// for the other.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleAdvertisingParameters {
    static_random_address: [u8; 6],
    local_name_mac: [u8; 6],
}

impl BleAdvertisingParameters {
    /// Derive one boot's complete BLE advertising inputs.
    pub const fn new(node_identity_hash: [u8; 16], local_name_mac: [u8; 6]) -> Self {
        Self {
            static_random_address: static_random_address(node_identity_hash),
            local_name_mac,
        }
    }

    /// Static-random local address derived from the durable node identity.
    pub const fn static_random_address(self) -> [u8; 6] {
        self.static_random_address
    }

    /// Physical MAC used only to construct the human-readable local name.
    pub const fn local_name_mac(self) -> [u8; 6] {
        self.local_name_mac
    }
}

/// Which public local name owns the next BLE advertisement.
///
/// Recovery deliberately remains distinct from whether an authenticated bond
/// has just become durable. A replacement central must first prove that it can
/// use that bond for the application session before stale normal-name clients
/// are allowed to contend for the sole controller slot again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleAdvertisingIdentityState {
    /// Advertise the stable name used by ordinary saved profiles.
    Normal,
    /// Advertise the recovery-only name ignored by ordinary saved profiles.
    Recovery,
}

impl BleAdvertisingIdentityState {
    /// Select the boot advertisement from the restored authoritative bond.
    pub const fn from_restored_bond(restored_bond_present: bool) -> Self {
        if restored_bond_present {
            Self::Normal
        } else {
            Self::Recovery
        }
    }

    /// Whether the next advertisement must use the recovery-only local name.
    pub const fn uses_recovery_name(self) -> bool {
        matches!(self, Self::Recovery)
    }

    /// Apply proof that recovery reached a usable application owner.
    ///
    /// A durable bond alone is insufficient: immediately revealing the normal
    /// name would let the stale saved profile that caused recovery reclaim the
    /// sole link before the replacement central authenticates. Fresh first-run
    /// onboarding may instead complete by durably activating its application
    /// credential on the retained pairing link.
    pub const fn after_connection(
        self,
        authoritative_bond_present: bool,
        ordinary_session_established: bool,
        application_credential_activated: bool,
    ) -> Self {
        if matches!(self, Self::Recovery)
            && authoritative_bond_present
            && (ordinary_session_established || application_credential_activated)
        {
            Self::Normal
        } else {
            self
        }
    }
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
    /// A fragment exceeded the profile's maximum ATT value bound.
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

    /// Observe progress at `elapsed_millis` since ordinary lifecycle acceptance.
    ///
    /// The deadline is exclusive: first reaching `Established` at or after the
    /// timeout is too late. Both authenticated and expired outcomes are
    /// terminal, so later session phases cannot accidentally re-arm the policy.
    pub fn poll(
        &mut self,
        elapsed_millis: u64,
        phase: AuthenticatedSessionPhase,
    ) -> PreAuthenticationDeadlineStatus {
        if self.status != PreAuthenticationDeadlineStatus::Waiting {
            return self.status;
        }
        if elapsed_millis >= PRE_AUTHENTICATION_TIMEOUT_MS {
            self.status = PreAuthenticationDeadlineStatus::Expired;
        } else if phase == AuthenticatedSessionPhase::Established {
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
        if bytes > reticulum_device_api_ble::MAXIMUM_ATT_VALUE_BYTES {
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

/// Derive the usable characteristic-value payload from a connection's ATT MTU.
///
/// ATT reserves three bytes for the operation and attribute handle. Trouble
/// begins each connection at the mandatory 23-byte MTU and updates it after an
/// exchange. A malformed sub-minimum observation therefore falls back to the
/// universally safe 20-byte value instead of producing an empty fragment.
pub const fn negotiated_att_value_bytes(att_mtu: u16) -> usize {
    let payload = (att_mtu as usize).saturating_sub(3);
    if payload < reticulum_device_api_ble::MINIMUM_ATT_VALUE_BYTES {
        reticulum_device_api_ble::MINIMUM_ATT_VALUE_BYTES
    } else if payload > reticulum_device_api_ble::MAXIMUM_ATT_VALUE_BYTES {
        reticulum_device_api_ble::MAXIMUM_ATT_VALUE_BYTES
    } else {
        payload
    }
}

const _: () = assert!(CONNECTIONS_MAX == 1);
const _: () = assert!(CONTROLLER_ACTIVITY_MAX == CONNECTIONS_MAX + 1);
const _: () = assert!(L2CAP_CHANNELS_MAX == 2);
const _: () = assert!(INDICATION_CONFIRM_TIMEOUT_MS > 0);
const _: () = assert!(CCCD_SUBSCRIBE_TIMEOUT_MS > INDICATION_CONFIRM_TIMEOUT_MS);
const _: () = assert!(PRE_AUTHENTICATION_TIMEOUT_MS > CCCD_SUBSCRIBE_TIMEOUT_MS);
const _: () = assert!(BLE_SECURITY_PAIRING_TIMEOUT_MS > 0);
const _: () = assert!(DISCONNECT_DRAIN_RECHECK_INTERVAL_MS > 0);
const _: () = assert!(DISCONNECT_DRAIN_PROLONGED_LOG_MS > DISCONNECT_DRAIN_RECHECK_INTERVAL_MS);
const _: () = assert!(HANDOFF_EXCHANGE_TIMEOUT_MS > API_POLL_INTERVAL_MS);
const _: () = assert!(BUTTON_OBSERVATION_HANDOFF_TIMEOUT_MS > HANDOFF_EXCHANGE_TIMEOUT_MS);
const _: () = assert!(APPLICATION_PAIRING_IDLE_TIMEOUT_MS > HANDOFF_EXCHANGE_TIMEOUT_MS);
const _: () = assert!(BLE_BOND_BOOT_RECOVERY_HOLD_MS > 0);

#[cfg(test)]
mod tests {
    use super::{
        APPLICATION_PAIRING_IDLE_TIMEOUT_MS, ApplicationPairingIdleDeadline,
        BLE_BOND_BOOT_RECOVERY_HOLD_MS, BLE_SECURITY_PAIRING_TIMEOUT_MS,
        BUTTON_OBSERVATION_HANDOFF_TIMEOUT_MS, BleAdvertisingIdentityState,
        BleAdvertisingParameters, BleBondExchangeResult, BootBleBondRecoveryGesture,
        BootBleBondRecoveryProgress, CCCD_SUBSCRIBE_TIMEOUT_MS, CONNECTIONS_MAX,
        CONTROLLER_ACTIVITY_MAX, FreshSecurityDisposition, HANDOFF_EXCHANGE_TIMEOUT_MS,
        IndicationGate, IndicationGateError, PRE_AUTHENTICATION_TIMEOUT_MS,
        PreAuthenticationDeadline, PreAuthenticationDeadlineStatus, negotiated_att_value_bytes,
        static_random_address,
    };
    use crate::authenticated_session::AuthenticatedSessionPhase;

    #[test]
    fn controller_activity_budget_keeps_one_advertiser_separate_from_one_link() {
        assert_eq!(CONNECTIONS_MAX, 1);
        assert_eq!(CONTROLLER_ACTIVITY_MAX, 2);
    }

    #[test]
    fn human_pairing_envelopes_are_deliberately_forgiving() {
        assert_eq!(CCCD_SUBSCRIBE_TIMEOUT_MS, 240_000);
        assert_eq!(PRE_AUTHENTICATION_TIMEOUT_MS, 300_000);
        // Trouble owns Bluetooth's separate 30-second SMP transaction timer.
        assert_eq!(BLE_SECURITY_PAIRING_TIMEOUT_MS, 30_000);
        assert_eq!(HANDOFF_EXCHANGE_TIMEOUT_MS, 60_000);
        assert_eq!(BUTTON_OBSERVATION_HANDOFF_TIMEOUT_MS, 120_000);
        assert_eq!(APPLICATION_PAIRING_IDLE_TIMEOUT_MS, 300_000);
    }

    #[test]
    fn continuous_boot_hold_authorizes_exactly_once() {
        let mut gesture = BootBleBondRecoveryGesture::new(40, true);
        assert_eq!(
            gesture.observe(40, true),
            BootBleBondRecoveryProgress::Pending
        );
        assert_eq!(
            gesture.observe(40 + BLE_BOND_BOOT_RECOVERY_HOLD_MS - 1, true),
            BootBleBondRecoveryProgress::Pending
        );
        assert_eq!(
            gesture.observe(40 + BLE_BOND_BOOT_RECOVERY_HOLD_MS, true),
            BootBleBondRecoveryProgress::Authorized
        );
        assert_eq!(
            gesture.observe(40 + BLE_BOND_BOOT_RECOVERY_HOLD_MS + 1, true),
            BootBleBondRecoveryProgress::Consumed
        );
        assert_eq!(
            gesture.observe(40 + BLE_BOND_BOOT_RECOVERY_HOLD_MS + 2, false),
            BootBleBondRecoveryProgress::Consumed
        );
    }

    #[test]
    fn released_boot_sample_cannot_be_repaired_by_a_later_hold() {
        let mut gesture = BootBleBondRecoveryGesture::new(0, false);
        assert_eq!(
            gesture.observe(BLE_BOND_BOOT_RECOVERY_HOLD_MS + 1, true),
            BootBleBondRecoveryProgress::Rejected
        );
    }

    #[test]
    fn short_boot_hold_is_rejected_on_release() {
        let mut gesture = BootBleBondRecoveryGesture::new(0, true);
        assert_eq!(
            gesture.observe(BLE_BOND_BOOT_RECOVERY_HOLD_MS - 1, true),
            BootBleBondRecoveryProgress::Pending
        );
        assert_eq!(
            gesture.observe(BLE_BOND_BOOT_RECOVERY_HOLD_MS - 1, false),
            BootBleBondRecoveryProgress::Rejected
        );
        assert_eq!(
            gesture.observe(2 * BLE_BOND_BOOT_RECOVERY_HOLD_MS, true),
            BootBleBondRecoveryProgress::Rejected
        );
    }

    #[test]
    fn observed_bounce_permanently_rejects_boot_recovery() {
        let mut gesture = BootBleBondRecoveryGesture::new(100, true);
        assert_eq!(
            gesture.observe(600, true),
            BootBleBondRecoveryProgress::Pending
        );
        assert_eq!(
            gesture.observe(601, false),
            BootBleBondRecoveryProgress::Rejected
        );
        assert_eq!(
            gesture.observe(602, true),
            BootBleBondRecoveryProgress::Rejected
        );
        assert_eq!(
            gesture.observe(100 + BLE_BOND_BOOT_RECOVERY_HOLD_MS + 1, true),
            BootBleBondRecoveryProgress::Rejected
        );
    }

    #[test]
    fn clock_regression_fails_boot_recovery_closed() {
        let mut gesture = BootBleBondRecoveryGesture::new(500, true);
        assert_eq!(
            gesture.observe(700, true),
            BootBleBondRecoveryProgress::Pending
        );
        assert_eq!(
            gesture.observe(699, true),
            BootBleBondRecoveryProgress::Rejected
        );
        assert_eq!(
            gesture.observe(500 + BLE_BOND_BOOT_RECOVERY_HOLD_MS, true),
            BootBleBondRecoveryProgress::Rejected
        );
    }

    #[test]
    fn durable_identity_preserves_static_random_address_across_reboots() {
        let identity_hash = [
            0xfd, 0x9f, 0x12, 0x1e, 0x29, 0x3b, 0xf4, 0xa4, 0x15, 0xdd, 0x74, 0x36, 0x6f, 0xf7,
            0x5f, 0x69,
        ];
        let first_boot = static_random_address(identity_hash);
        let reboot = static_random_address(identity_hash);
        assert_eq!(first_boot, [0x3b, 0x29, 0x1e, 0x12, 0x9f, 0xfd]);
        assert_eq!(reboot, first_boot);
        assert_eq!(first_boot[5] & 0xc0, 0xc0);
    }

    #[test]
    fn reprovisioned_identity_rotates_static_random_address() {
        let before_erase = static_random_address([
            0xfd, 0x9f, 0x12, 0x1e, 0x29, 0x3b, 0xf4, 0xa4, 0x15, 0xdd, 0x74, 0x36, 0x6f, 0xf7,
            0x5f, 0x69,
        ]);
        let after_erase = static_random_address([
            0x83, 0xa0, 0x9e, 0xd8, 0x07, 0xa0, 0xa7, 0xc6, 0x31, 0x38, 0x6d, 0xea, 0xa0, 0x44,
            0x8f, 0xb9,
        ]);
        assert_ne!(after_erase, before_erase);
        assert_eq!(after_erase[5] & 0xc0, 0xc0);
    }

    #[test]
    fn advertising_parameters_keep_identity_address_separate_from_human_name() {
        let identity_hash = [
            0xfd, 0x9f, 0x12, 0x1e, 0x29, 0x3b, 0xf4, 0xa4, 0x15, 0xdd, 0x74, 0x36, 0x6f, 0xf7,
            0x5f, 0x69,
        ];
        let physical_mac = [0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88];
        let parameters = BleAdvertisingParameters::new(identity_hash, physical_mac);
        assert_eq!(
            parameters.static_random_address(),
            static_random_address(identity_hash)
        );
        assert_eq!(parameters.local_name_mac(), physical_mac);
    }

    #[test]
    fn bondless_boot_keeps_recovery_identity_until_application_access_is_proven() {
        let recovery = BleAdvertisingIdentityState::from_restored_bond(false);
        assert!(recovery.uses_recovery_name());
        assert_eq!(
            recovery.after_connection(true, false, false),
            BleAdvertisingIdentityState::Recovery,
            "a durable bond alone must not expose the stale profile's normal target"
        );
        assert_eq!(
            recovery.after_connection(true, true, false),
            BleAdvertisingIdentityState::Normal
        );
        assert_eq!(
            recovery.after_connection(true, false, true),
            BleAdvertisingIdentityState::Normal
        );
        assert_eq!(
            recovery.after_connection(false, true, true),
            BleAdvertisingIdentityState::Recovery,
            "no application proof can substitute for a durable authoritative bond"
        );
    }

    #[test]
    fn restored_bond_uses_and_retains_the_normal_identity() {
        let normal = BleAdvertisingIdentityState::from_restored_bond(true);
        assert!(!normal.uses_recovery_name());
        assert_eq!(
            normal.after_connection(false, false, false),
            BleAdvertisingIdentityState::Normal
        );
    }

    #[test]
    fn only_a_node_owned_bond_failure_keeps_failed_pairing_reboot_scoped() {
        let clean = FreshSecurityDisposition::classify(false, false);
        assert_eq!(clean, FreshSecurityDisposition::Clean);
        assert!(!clean.scrub_non_authoritative_bonds());
        assert!(!clean.disable_until_reboot());

        let precommit_failure = FreshSecurityDisposition::classify(true, false);
        assert_eq!(precommit_failure, FreshSecurityDisposition::ScrubAndRetry);
        assert!(precommit_failure.scrub_non_authoritative_bonds());
        assert!(!precommit_failure.disable_until_reboot());

        let ambiguous_commit = FreshSecurityDisposition::classify(true, true);
        assert_eq!(ambiguous_commit, FreshSecurityDisposition::ScrubAndDisable);
        assert!(ambiguous_commit.scrub_non_authoritative_bonds());
        assert!(ambiguous_commit.disable_until_reboot());

        assert_eq!(
            FreshSecurityDisposition::classify(false, true),
            FreshSecurityDisposition::Clean,
            "a confirmed durable commit clears the prior attempt fact"
        );
    }

    #[test]
    fn bond_exchange_timeout_distinguishes_local_pressure_from_moved_ownership() {
        let before_send = BleBondExchangeResult::timed_out(false);
        assert_eq!(before_send, BleBondExchangeResult::TimedOutBeforeSend);
        assert!(!before_send.bond_reboot_required());

        let after_send = BleBondExchangeResult::timed_out(true);
        assert_eq!(after_send, BleBondExchangeResult::TimedOutAfterSend);
        assert!(after_send.bond_reboot_required());

        assert!(!BleBondExchangeResult::Durable.bond_reboot_required());
        assert!(BleBondExchangeResult::ExplicitFailure.bond_reboot_required());
    }

    #[test]
    fn address_derivation_uses_identity_hash_prefix_not_efuse_mac_shape() {
        let address = static_random_address([
            0xac, 0xa7, 0x04, 0xe1, 0x3e, 0x88, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80,
            0x90, 0xa0,
        ]);
        assert_eq!(address, [0x88, 0x3e, 0xe1, 0x04, 0xa7, 0xec]);
        assert_eq!(address[5] & 0xc0, 0xc0);
        assert_eq!(address[0], 0x88);
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
            gate.arm(reticulum_device_api_ble::MAXIMUM_ATT_VALUE_BYTES + 1),
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
    fn negotiated_att_payload_uses_the_safe_fallback_and_profile_ceiling() {
        assert_eq!(negotiated_att_value_bytes(0), 20);
        assert_eq!(negotiated_att_value_bytes(23), 20);
        assert_eq!(negotiated_att_value_bytes(64), 61);
        assert_eq!(negotiated_att_value_bytes(251), 248);
        assert_eq!(negotiated_att_value_bytes(255), 248);
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
            deadline.poll(0, AuthenticatedSessionPhase::AwaitingClientHello),
            PreAuthenticationDeadlineStatus::Waiting
        );
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS - 1,
                AuthenticatedSessionPhase::AwaitingClientHello
            ),
            PreAuthenticationDeadlineStatus::Waiting
        );
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS,
                AuthenticatedSessionPhase::AwaitingClientHello
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
                AuthenticatedSessionPhase::AwaitingClientHello
            ),
            PreAuthenticationDeadlineStatus::Waiting
        );
        assert_eq!(
            deadline.poll(
                PRE_AUTHENTICATION_TIMEOUT_MS,
                AuthenticatedSessionPhase::AwaitingClientHello
            ),
            PreAuthenticationDeadlineStatus::Expired
        );
    }

    #[test]
    fn stalled_hello_and_proof_flights_share_one_non_refreshing_deadline() {
        let mut deadline = PreAuthenticationDeadline::new();
        for (elapsed, phase) in [
            (1, AuthenticatedSessionPhase::AdmissionCommandPending),
            (
                PRE_AUTHENTICATION_TIMEOUT_MS / 4,
                AuthenticatedSessionPhase::AwaitingAdmissionReply,
            ),
            (
                PRE_AUTHENTICATION_TIMEOUT_MS / 2,
                AuthenticatedSessionPhase::ServerHelloFlight,
            ),
            (
                PRE_AUTHENTICATION_TIMEOUT_MS - 1,
                AuthenticatedSessionPhase::PendingClientProof,
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
                AuthenticatedSessionPhase::PendingClientProof
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
                AuthenticatedSessionPhase::Established
            ),
            PreAuthenticationDeadlineStatus::Authenticated
        );
        assert_eq!(
            deadline.poll(u64::MAX, AuthenticatedSessionPhase::AwaitingReply),
            PreAuthenticationDeadlineStatus::Authenticated
        );
    }
}
