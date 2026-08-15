//! Pure USB pairing-edge policy for the permanent E290 node.
//!
//! This module translates raw active-low button samples and USB bus
//! observations into the stable facts consumed by the portable pairing
//! policy. It owns no HAL peripheral, executor, framing buffer, flash access,
//! credential state, or Reticulum interface.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use reticulum_device_api_pairing_policy::{ActiveLowButton, ConnectionId};

/// Raw button level must remain unchanged for this long before a transition
/// becomes debounced.
pub const BUTTON_DEBOUNCE_MILLIS: u64 = 20;

/// Longest raw-sample interval that can prove physical-button continuity.
///
/// This is deliberately separate from the electrical debounce interval. The
/// GPIO sampler shares an async bearer owner with bounded BLE or USB work, so
/// an ordinary scheduler pause can exceed 20 ms without meaning that physical
/// observation was lost. A longer gap still fails closed and requires a fresh
/// stable High before a later Low can count toward physical presence.
pub const BUTTON_MAX_SAMPLE_GAP_MILLIS: u64 = 250;

/// A USB connection is considered suspended after this many milliseconds
/// without an observed start-of-frame indication.
///
/// Suspension deliberately retains the connection epoch. Only a USB bus reset
/// can retire an epoch, because endpoint bytes already committed to hardware
/// cannot be retracted safely across an ordinary suspend/resume cycle.
pub const USB_MISSED_SOF_SUSPEND_MILLIS: u64 = 8;

/// One bounded unit of work selected at the USB/GPIO pairing edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbPairingWork {
    /// Publish the current debounced physical-presence level to the node.
    ObserveButton,
    /// Poll one bounded chunk of unauthenticated control input.
    PollControl {
        /// Publish the due button observation immediately if no complete
        /// control request was accepted by this poll.
        observe_button_if_empty: bool,
    },
    /// Neither lane can make progress in this poll.
    Wait,
}

/// Select bounded work without allowing either pairing lane to starve.
///
/// A due button observation normally outranks unauthenticated traffic. After
/// that observation is acknowledged, one ready control poll may run first;
/// this prevents a 20 ms button round trip from permanently excluding the
/// control lane. A suspended or TX-backpressured endpoint is not ready, so
/// physical observations continue without waiting for USB progress.
pub const fn select_usb_pairing_work(
    button_due: bool,
    control_ready: bool,
    control_turn_after_button: bool,
) -> UsbPairingWork {
    if button_due && control_ready && control_turn_after_button {
        return UsbPairingWork::PollControl {
            observe_button_if_empty: true,
        };
    }
    if button_due {
        return UsbPairingWork::ObserveButton;
    }
    if control_ready {
        return UsbPairingWork::PollControl {
            observe_button_if_empty: false,
        };
    }
    UsbPairingWork::Wait
}

/// Stable-time button debouncing failed closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonDebounceFault {
    /// A later observation supplied an earlier timestamp.
    ClockRegression,
}

/// Result of one raw active-low button observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DebouncedButtonObservation {
    level: ActiveLowButton,
    transitioned: bool,
    continuity_lost: bool,
}

impl DebouncedButtonObservation {
    /// Current debounced active-low level.
    pub const fn level(self) -> ActiveLowButton {
        self.level
    }

    /// Whether this observation committed a stable-level transition.
    pub const fn transitioned(self) -> bool {
        self.transitioned
    }

    /// Whether the interval since the prior raw sample was ambiguous.
    pub const fn continuity_lost(self) -> bool {
        self.continuity_lost
    }
}

/// Stable-time debouncer initialized from the actual electrical level.
///
/// Initializing directly from the pin prevents a held-low boot from inventing
/// a released-high observation that could arm physical-presence policy.
#[must_use = "the debouncer must remain resident across raw button samples"]
pub struct ActiveLowButtonDebouncer {
    current: ActiveLowButton,
    candidate: Option<(ActiveLowButton, u64)>,
    last_now: u64,
    fault: Option<ButtonDebounceFault>,
}

impl ActiveLowButtonDebouncer {
    /// Initialize from the actual raw pin level at the supplied monotonic time.
    pub const fn new(now_millis: u64, raw_level: ActiveLowButton) -> Self {
        Self {
            current: raw_level,
            candidate: None,
            last_now: now_millis,
            fault: None,
        }
    }

    /// Current debounced active-low level.
    pub const fn current(&self) -> ActiveLowButton {
        self.current
    }

    /// Start a replacement connection from a conservative Low baseline.
    ///
    /// A prior epoch's debounced High must not arm a new epoch after the raw
    /// pin has already gone Low. Even when the pin is currently High, this
    /// reset requires a fresh full debounce interval before publishing that
    /// release to the replacement connection.
    pub fn reset_for_connection(
        &mut self,
        now_millis: u64,
        raw_level: ActiveLowButton,
    ) -> Result<(), ButtonDebounceFault> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if now_millis < self.last_now {
            self.candidate = None;
            self.fault = Some(ButtonDebounceFault::ClockRegression);
            return Err(ButtonDebounceFault::ClockRegression);
        }
        self.last_now = now_millis;
        self.current = ActiveLowButton::Low;
        self.candidate = match raw_level {
            ActiveLowButton::High => Some((ActiveLowButton::High, now_millis)),
            ActiveLowButton::Low => None,
        };
        Ok(())
    }

    /// Observe one raw level at a nondecreasing monotonic timestamp.
    ///
    /// A level different from the current output must remain continuously raw
    /// for exactly [`BUTTON_DEBOUNCE_MILLIS`] before it is published. A clock
    /// regression permanently faults this owner so elapsed time can never be
    /// manufactured by a later sample.
    pub fn observe(
        &mut self,
        now_millis: u64,
        raw_level: ActiveLowButton,
    ) -> Result<DebouncedButtonObservation, ButtonDebounceFault> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if now_millis < self.last_now {
            self.candidate = None;
            self.fault = Some(ButtonDebounceFault::ClockRegression);
            return Err(ButtonDebounceFault::ClockRegression);
        }
        let sample_gap = now_millis - self.last_now;
        self.last_now = now_millis;

        if sample_gap >= BUTTON_MAX_SAMPLE_GAP_MILLIS {
            if self.current == ActiveLowButton::High {
                // There is no active physical-presence hold to invalidate
                // while the last trusted level is released High. Discard any
                // pre-gap candidate and start a fresh Low candidate at this
                // observation instead of manufacturing a Low baseline that
                // would force the operator to release and press again.
                self.candidate = match raw_level {
                    ActiveLowButton::High => None,
                    ActiveLowButton::Low => Some((ActiveLowButton::Low, now_millis)),
                };
                return Ok(DebouncedButtonObservation {
                    level: ActiveLowButton::High,
                    transitioned: false,
                    continuity_lost: false,
                });
            }
            // Low is the fail-closed internal baseline. If the pin is really
            // High, it must remain there for a fresh debounce interval before
            // that release is trusted again.
            self.current = ActiveLowButton::Low;
            self.candidate = match raw_level {
                ActiveLowButton::High => Some((ActiveLowButton::High, now_millis)),
                ActiveLowButton::Low => None,
            };
            return Ok(DebouncedButtonObservation {
                level: self.current,
                transitioned: false,
                continuity_lost: true,
            });
        }

        if raw_level == self.current {
            self.candidate = None;
            return Ok(DebouncedButtonObservation {
                level: self.current,
                transitioned: false,
                continuity_lost: false,
            });
        }

        let candidate_started = match self.candidate {
            Some((candidate, started)) if candidate == raw_level => started,
            _ => {
                self.candidate = Some((raw_level, now_millis));
                return Ok(DebouncedButtonObservation {
                    level: self.current,
                    transitioned: false,
                    continuity_lost: false,
                });
            }
        };

        if now_millis - candidate_started < BUTTON_DEBOUNCE_MILLIS {
            return Ok(DebouncedButtonObservation {
                level: self.current,
                transitioned: false,
                continuity_lost: false,
            });
        }

        self.current = raw_level;
        self.candidate = None;
        Ok(DebouncedButtonObservation {
            level: self.current,
            transitioned: true,
            continuity_lost: false,
        })
    }
}

/// Latches release evidence until the node policy has observed it.
///
/// A debounced High followed by Low may occur while the depth-one handoff is
/// occupied. This guard publishes the High first instead of collapsing both
/// transitions to the latest level. After an ambiguous raw-sample gap it also
/// publishes a fail-closed High to break any prior hold, then suppresses Low
/// until a fresh debounced High has actually been observed.
#[must_use = "physical-presence publication state must survive handoff pressure"]
pub struct PhysicalPresencePublicationGuard {
    high_pending: bool,
    release_required: bool,
    release_observed: bool,
}

impl PhysicalPresencePublicationGuard {
    /// Construct an empty transition latch.
    pub const fn new() -> Self {
        Self {
            high_pending: false,
            release_required: false,
            release_observed: false,
        }
    }

    /// Drop transition evidence belonging to an older connection epoch.
    pub fn reset_for_connection(&mut self) {
        *self = Self::new();
    }

    /// Retain security-relevant state from one debounced raw sample.
    pub fn observe(&mut self, observation: DebouncedButtonObservation) {
        if observation.continuity_lost() {
            self.high_pending = true;
            self.release_required = true;
            self.release_observed = false;
            return;
        }
        if observation.transitioned() && observation.level() == ActiveLowButton::High {
            self.high_pending = true;
            if self.release_required {
                self.release_observed = true;
            }
        }
    }

    /// Whether a latched transition or ordinary periodic deadline is due.
    pub const fn publication_due(&self, periodic_due: bool) -> bool {
        self.high_pending || periodic_due
    }

    /// Conservative level that the node policy may consume next.
    pub const fn policy_level(&self, current: ActiveLowButton) -> ActiveLowButton {
        if self.high_pending || self.release_required {
            ActiveLowButton::High
        } else {
            current
        }
    }

    /// Commit the level selected by [`Self::policy_level`] to local handoff
    /// ownership.
    pub fn publication_queued(&mut self) {
        self.high_pending = false;
        if self.release_required && self.release_observed {
            self.release_required = false;
            self.release_observed = false;
        }
    }
}

impl Default for PhysicalPresencePublicationGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// One externally meaningful USB connection transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbConnectionEvent {
    /// No connection ownership changed.
    None,
    /// A fresh boot-lifetime connection epoch became active.
    Connected(ConnectionId),
    /// The named connection stopped receiving SOFs but retains its epoch.
    Suspended(ConnectionId),
    /// The suspended connection resumed with its existing epoch.
    Resumed(ConnectionId),
    /// The named connection was closed by USB bus reset.
    Disconnected(ConnectionId),
}

/// USB connection tracking failed closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbConnectionFault {
    /// A later observation supplied an earlier timestamp.
    ClockRegression,
    /// Every nonzero boot-lifetime connection epoch has been allocated.
    EpochExhausted,
}

/// Boot-lifetime USB activity and connection-epoch tracker.
///
/// The HAL edge supplies sticky SOF and bus-reset observations. A bus reset
/// always closes current ownership and deliberately ignores a simultaneous
/// SOF; only a later SOF can allocate the replacement connection.
#[must_use = "the sole connection tracker must survive every USB reconnect"]
pub struct UsbConnectionTracker {
    connection: Option<ConnectionId>,
    suspended: bool,
    last_sof_millis: Option<u64>,
    last_now: u64,
    last_epoch: u64,
    fault: Option<UsbConnectionFault>,
}

impl UsbConnectionTracker {
    /// Construct the boot-lifetime tracker with no active connection.
    pub const fn new(now_millis: u64) -> Self {
        Self {
            connection: None,
            suspended: false,
            last_sof_millis: None,
            last_now: now_millis,
            last_epoch: 0,
            fault: None,
        }
    }

    /// Current accepted USB connection while SOFs are arriving.
    pub const fn active(&self) -> Option<ConnectionId> {
        if self.suspended {
            None
        } else {
            self.connection
        }
    }

    /// Current allocated USB connection, including while it is suspended.
    pub const fn connection(&self) -> Option<ConnectionId> {
        self.connection
    }

    /// Process one bounded poll of the USB raw-status register.
    ///
    /// `sof_observed` means at least one SOF arrived since the previous poll.
    /// `bus_reset_observed` wins over SOF when both are true. Clock or epoch
    /// failure is sticky and no later call can reopen the tracker.
    pub fn observe(
        &mut self,
        now_millis: u64,
        sof_observed: bool,
        bus_reset_observed: bool,
    ) -> Result<UsbConnectionEvent, UsbConnectionFault> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if now_millis < self.last_now {
            self.fail(UsbConnectionFault::ClockRegression);
            return Err(UsbConnectionFault::ClockRegression);
        }
        self.last_now = now_millis;

        if bus_reset_observed {
            self.last_sof_millis = None;
            self.suspended = false;
            return Ok(self
                .connection
                .take()
                .map_or(UsbConnectionEvent::None, UsbConnectionEvent::Disconnected));
        }

        if sof_observed {
            self.last_sof_millis = Some(now_millis);
            if let Some(connection) = self.connection {
                if self.suspended {
                    self.suspended = false;
                    return Ok(UsbConnectionEvent::Resumed(connection));
                }
                return Ok(UsbConnectionEvent::None);
            }
            let Some(epoch) = self.last_epoch.checked_add(1) else {
                self.fail(UsbConnectionFault::EpochExhausted);
                return Err(UsbConnectionFault::EpochExhausted);
            };
            let Some(connection) = ConnectionId::new(epoch) else {
                self.fail(UsbConnectionFault::EpochExhausted);
                return Err(UsbConnectionFault::EpochExhausted);
            };
            self.last_epoch = epoch;
            self.connection = Some(connection);
            self.suspended = false;
            return Ok(UsbConnectionEvent::Connected(connection));
        }

        let Some(connection) = self.connection else {
            return Ok(UsbConnectionEvent::None);
        };
        if self.suspended {
            return Ok(UsbConnectionEvent::None);
        }
        let Some(last_sof_millis) = self.last_sof_millis else {
            self.suspended = true;
            return Ok(UsbConnectionEvent::Suspended(connection));
        };
        if now_millis - last_sof_millis < USB_MISSED_SOF_SUSPEND_MILLIS {
            return Ok(UsbConnectionEvent::None);
        }

        self.suspended = true;
        Ok(UsbConnectionEvent::Suspended(connection))
    }

    fn fail(&mut self, fault: UsbConnectionFault) {
        self.connection = None;
        self.suspended = false;
        self.last_sof_millis = None;
        self.fault = Some(fault);
    }
}

/// Why a pre-authentication record sequence was not accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceRefusal {
    /// The record belongs to a stale or otherwise different connection epoch.
    WrongConnection {
        /// Connection bound to this sequence gate.
        expected: ConnectionId,
        /// Connection supplied with the record.
        observed: ConnectionId,
    },
    /// The record reused an already consumed sequence.
    Duplicate {
        /// Exact sequence required next.
        expected: u64,
        /// Earlier sequence supplied by the record.
        observed: u64,
    },
    /// The record skipped one or more required sequences.
    Gap {
        /// Exact sequence required next.
        expected: u64,
        /// Later sequence supplied by the record.
        observed: u64,
    },
    /// Advancing the exact-next sequence would wrap to zero.
    Exhausted,
}

/// Exact-next sequence gate bound to one USB connection epoch.
///
/// Every new connection constructs a new gate starting at sequence zero.
/// Duplicate and gap refusals do not advance state. Sequence `u64::MAX` is
/// refused and permanently exhausts this gate instead of wrapping.
#[must_use = "the connection-local sequence gate must survive accepted records"]
pub struct ExactNextSequenceGate {
    connection: ConnectionId,
    next: Option<u64>,
}

impl ExactNextSequenceGate {
    /// Bind a fresh exact-next sequence space to one connection.
    pub const fn new(connection: ConnectionId) -> Self {
        Self {
            connection,
            next: Some(0),
        }
    }

    /// Connection epoch to which this gate belongs.
    pub const fn connection(&self) -> ConnectionId {
        self.connection
    }

    /// Exact sequence required next, or `None` after exhaustion.
    pub const fn next_expected(&self) -> Option<u64> {
        self.next
    }

    /// Accept only the exact next sequence for the bound connection.
    pub fn accept(
        &mut self,
        connection: ConnectionId,
        observed: u64,
    ) -> Result<(), SequenceRefusal> {
        if connection != self.connection {
            return Err(SequenceRefusal::WrongConnection {
                expected: self.connection,
                observed: connection,
            });
        }
        let Some(expected) = self.next else {
            return Err(SequenceRefusal::Exhausted);
        };
        if observed < expected {
            return Err(SequenceRefusal::Duplicate { expected, observed });
        }
        if observed > expected {
            return Err(SequenceRefusal::Gap { expected, observed });
        }
        let Some(next) = expected.checked_add(1) else {
            self.next = None;
            return Err(SequenceRefusal::Exhausted);
        };
        self.next = Some(next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use reticulum_device_api_pairing_policy::{ActiveLowButton, ConnectionId};

    use super::{
        ActiveLowButtonDebouncer, BUTTON_DEBOUNCE_MILLIS, BUTTON_MAX_SAMPLE_GAP_MILLIS,
        ButtonDebounceFault, ExactNextSequenceGate, PhysicalPresencePublicationGuard,
        SequenceRefusal, USB_MISSED_SOF_SUSPEND_MILLIS, UsbConnectionEvent, UsbConnectionFault,
        UsbConnectionTracker, UsbPairingWork, select_usb_pairing_work,
    };

    fn connection(value: u64) -> ConnectionId {
        ConnectionId::new(value).expect("test connection must be nonzero")
    }

    #[test]
    fn pairing_lanes_bound_button_and_control_starvation() {
        assert_eq!(
            select_usb_pairing_work(true, true, false),
            UsbPairingWork::ObserveButton,
            "unauthenticated traffic cannot postpone the first due observation"
        );
        assert_eq!(
            select_usb_pairing_work(true, true, true),
            UsbPairingWork::PollControl {
                observe_button_if_empty: true,
            },
            "an acknowledged observation grants exactly one bounded control turn"
        );
        assert_eq!(
            select_usb_pairing_work(true, false, true),
            UsbPairingWork::ObserveButton,
            "suspend or TX pressure cannot pause physical observations"
        );
        assert_eq!(
            select_usb_pairing_work(false, true, false),
            UsbPairingWork::PollControl {
                observe_button_if_empty: false,
            }
        );
        assert_eq!(
            select_usb_pairing_work(false, false, true),
            UsbPairingWork::Wait
        );
    }

    #[test]
    fn high_transition_is_published_before_a_later_low_after_pressure() {
        let mut button = ActiveLowButtonDebouncer::new(0, ActiveLowButton::Low);
        let mut publication = PhysicalPresencePublicationGuard::new();

        for now in [1, 10, 21] {
            publication.observe(button.observe(now, ActiveLowButton::High).unwrap());
        }
        assert_eq!(button.current(), ActiveLowButton::High);

        // The handoff remains occupied while the pin returns to a stable Low.
        for now in [22, 32, 42] {
            publication.observe(button.observe(now, ActiveLowButton::Low).unwrap());
        }
        assert_eq!(button.current(), ActiveLowButton::Low);
        assert!(publication.publication_due(false));
        assert_eq!(
            publication.policy_level(button.current()),
            ActiveLowButton::High,
            "the stable release must not collapse into the latest Low"
        );
        publication.publication_queued();
        assert_eq!(
            publication.policy_level(button.current()),
            ActiveLowButton::Low
        );
    }

    #[test]
    fn ambiguous_sample_gap_suppresses_low_until_fresh_debounced_release() {
        let mut button = ActiveLowButtonDebouncer::new(0, ActiveLowButton::Low);
        let mut publication = PhysicalPresencePublicationGuard::new();

        let lost = button
            .observe(BUTTON_MAX_SAMPLE_GAP_MILLIS, ActiveLowButton::Low)
            .unwrap();
        assert!(lost.continuity_lost());
        publication.observe(lost);
        assert_eq!(
            publication.policy_level(button.current()),
            ActiveLowButton::High,
            "continuity loss must first cancel any policy-owned hold"
        );
        publication.publication_queued();
        assert_eq!(
            publication.policy_level(button.current()),
            ActiveLowButton::High,
            "synthetic cancellation cannot itself re-arm a later Low"
        );

        for now in [
            BUTTON_MAX_SAMPLE_GAP_MILLIS + 1,
            BUTTON_MAX_SAMPLE_GAP_MILLIS + 11,
            BUTTON_MAX_SAMPLE_GAP_MILLIS + 21,
        ] {
            publication.observe(button.observe(now, ActiveLowButton::High).unwrap());
        }
        assert_eq!(button.current(), ActiveLowButton::High);
        publication.publication_queued();
        assert_eq!(
            publication.policy_level(ActiveLowButton::Low),
            ActiveLowButton::Low,
            "a fresh stable released-high observation restores trust"
        );
    }

    #[test]
    fn routine_async_scheduler_gaps_preserve_a_deliberate_press() {
        let mut button = ActiveLowButtonDebouncer::new(0, ActiveLowButton::High);

        let candidate = button.observe(50, ActiveLowButton::Low).unwrap();
        assert!(!candidate.transitioned());
        assert!(!candidate.continuity_lost());

        let pressed = button.observe(100, ActiveLowButton::Low).unwrap();
        assert!(pressed.transitioned());
        assert!(!pressed.continuity_lost());
        assert_eq!(pressed.level(), ActiveLowButton::Low);

        let retained = button.observe(200, ActiveLowButton::Low).unwrap();
        assert!(!retained.transitioned());
        assert!(!retained.continuity_lost());
        assert_eq!(retained.level(), ActiveLowButton::Low);
    }

    #[test]
    fn long_gap_from_released_high_starts_a_fresh_press_candidate() {
        let mut button = ActiveLowButtonDebouncer::new(0, ActiveLowButton::High);

        let candidate = button
            .observe(BUTTON_MAX_SAMPLE_GAP_MILLIS, ActiveLowButton::Low)
            .unwrap();
        assert!(!candidate.transitioned());
        assert!(!candidate.continuity_lost());
        assert_eq!(candidate.level(), ActiveLowButton::High);

        let pressed = button
            .observe(
                BUTTON_MAX_SAMPLE_GAP_MILLIS + BUTTON_DEBOUNCE_MILLIS,
                ActiveLowButton::Low,
            )
            .unwrap();
        assert!(pressed.transitioned());
        assert!(!pressed.continuity_lost());
        assert_eq!(pressed.level(), ActiveLowButton::Low);
    }

    #[test]
    fn new_connection_drops_an_old_epoch_latched_release() {
        let mut button = ActiveLowButtonDebouncer::new(0, ActiveLowButton::Low);
        let mut publication = PhysicalPresencePublicationGuard::new();

        for now in [1, 10, 21] {
            publication.observe(button.observe(now, ActiveLowButton::High).unwrap());
        }
        for now in [22, 32, 42] {
            publication.observe(button.observe(now, ActiveLowButton::Low).unwrap());
        }
        assert_eq!(
            publication.policy_level(button.current()),
            ActiveLowButton::High,
            "the old epoch still owns its unforwarded stable release"
        );

        publication.reset_for_connection();
        assert_eq!(
            publication.policy_level(button.current()),
            ActiveLowButton::Low,
            "a new epoch must derive its first publication from current state"
        );
    }

    #[test]
    fn new_connection_rejects_old_high_when_raw_pin_is_already_low() {
        let mut button = ActiveLowButtonDebouncer::new(0, ActiveLowButton::High);
        assert!(
            !button
                .observe(1, ActiveLowButton::Low)
                .unwrap()
                .transitioned()
        );
        assert_eq!(button.current(), ActiveLowButton::High);

        button
            .reset_for_connection(2, ActiveLowButton::Low)
            .unwrap();
        assert_eq!(
            button.current(),
            ActiveLowButton::Low,
            "the replacement epoch cannot inherit the old debounced High"
        );

        button
            .reset_for_connection(3, ActiveLowButton::High)
            .unwrap();
        assert_eq!(button.current(), ActiveLowButton::Low);
        assert!(
            !button
                .observe(13, ActiveLowButton::High)
                .unwrap()
                .transitioned()
        );
        assert!(
            button
                .observe(3 + BUTTON_DEBOUNCE_MILLIS, ActiveLowButton::High)
                .unwrap()
                .transitioned(),
            "a current High becomes trusted only after a fresh full interval"
        );
    }

    #[test]
    fn button_bounce_never_shortens_the_stable_interval() {
        let mut button = ActiveLowButtonDebouncer::new(0, ActiveLowButton::High);
        assert_eq!(button.current(), ActiveLowButton::High);

        assert!(
            !button
                .observe(5, ActiveLowButton::Low)
                .unwrap()
                .transitioned()
        );
        assert!(
            !button
                .observe(15, ActiveLowButton::High)
                .unwrap()
                .transitioned()
        );
        assert!(
            !button
                .observe(18, ActiveLowButton::Low)
                .unwrap()
                .transitioned()
        );
        assert!(
            !button
                .observe(18 + BUTTON_DEBOUNCE_MILLIS - 1, ActiveLowButton::Low)
                .unwrap()
                .transitioned()
        );
        let stable = button
            .observe(18 + BUTTON_DEBOUNCE_MILLIS, ActiveLowButton::Low)
            .unwrap();
        assert!(stable.transitioned());
        assert_eq!(stable.level(), ActiveLowButton::Low);
        assert_eq!(button.current(), ActiveLowButton::Low);
    }

    #[test]
    fn held_low_startup_never_invents_a_released_high() {
        let mut button = ActiveLowButtonDebouncer::new(100, ActiveLowButton::Low);
        assert_eq!(button.current(), ActiveLowButton::Low);
        for now in [100, 110, 120, 2_120] {
            let observed = button.observe(now, ActiveLowButton::Low).unwrap();
            assert_eq!(observed.level(), ActiveLowButton::Low);
            assert!(!observed.transitioned());
        }

        assert!(
            !button
                .observe(2_121, ActiveLowButton::High)
                .unwrap()
                .transitioned()
        );
        assert!(
            !button
                .observe(2_131, ActiveLowButton::High)
                .unwrap()
                .transitioned()
        );
        let released = button
            .observe(2_121 + BUTTON_DEBOUNCE_MILLIS, ActiveLowButton::High)
            .unwrap();
        assert!(released.transitioned());
        assert_eq!(released.level(), ActiveLowButton::High);
    }

    #[test]
    fn button_clock_regression_is_sticky_and_cannot_publish_a_candidate() {
        let mut button = ActiveLowButtonDebouncer::new(10, ActiveLowButton::High);
        button.observe(20, ActiveLowButton::Low).unwrap();
        assert_eq!(
            button.observe(19, ActiveLowButton::Low),
            Err(ButtonDebounceFault::ClockRegression)
        );
        assert_eq!(button.current(), ActiveLowButton::High);
        assert_eq!(
            button.observe(100, ActiveLowButton::Low),
            Err(ButtonDebounceFault::ClockRegression)
        );
    }

    #[test]
    fn sof_suspend_and_resume_retain_epoch_until_bus_reset() {
        let mut tracker = UsbConnectionTracker::new(0);
        let first = connection(1);
        let second = connection(2);
        assert_eq!(
            tracker.observe(1, true, false),
            Ok(UsbConnectionEvent::Connected(first))
        );
        assert_eq!(tracker.active(), Some(first));
        assert_eq!(tracker.connection(), Some(first));
        assert_eq!(
            tracker.observe(7, false, false),
            Ok(UsbConnectionEvent::None)
        );
        assert_eq!(
            tracker.observe(1 + USB_MISSED_SOF_SUSPEND_MILLIS, false, false),
            Ok(UsbConnectionEvent::Suspended(first))
        );
        assert_eq!(tracker.active(), None);
        assert_eq!(tracker.connection(), Some(first));
        assert_eq!(
            tracker.observe(20, true, false),
            Ok(UsbConnectionEvent::Resumed(first))
        );
        assert_eq!(tracker.active(), Some(first));
        assert_eq!(
            tracker.observe(21, false, true),
            Ok(UsbConnectionEvent::Disconnected(first))
        );
        assert_eq!(tracker.connection(), None);
        assert_eq!(
            tracker.observe(22, true, false),
            Ok(UsbConnectionEvent::Connected(second))
        );
        assert_eq!(tracker.active(), Some(second));
    }

    #[test]
    fn periodic_sof_refresh_prevents_false_detach() {
        let mut tracker = UsbConnectionTracker::new(0);
        let first = connection(1);
        assert_eq!(
            tracker.observe(0, true, false),
            Ok(UsbConnectionEvent::Connected(first))
        );
        for now in [7, 14, 21, 28] {
            assert_eq!(
                tracker.observe(now, true, false),
                Ok(UsbConnectionEvent::None)
            );
        }
        assert_eq!(
            tracker.observe(35, false, false),
            Ok(UsbConnectionEvent::None)
        );
        assert_eq!(tracker.active(), Some(first));
    }

    #[test]
    fn bus_reset_closes_current_epoch_and_later_sof_reconnects() {
        let mut tracker = UsbConnectionTracker::new(0);
        let first = connection(1);
        let second = connection(2);
        tracker.observe(1, true, false).unwrap();
        assert_eq!(
            tracker.observe(2, false, true),
            Ok(UsbConnectionEvent::Disconnected(first))
        );
        assert_eq!(tracker.active(), None);
        assert_eq!(
            tracker.observe(3, true, false),
            Ok(UsbConnectionEvent::Connected(second))
        );
    }

    #[test]
    fn bus_reset_wins_over_a_simultaneous_sof() {
        let mut tracker = UsbConnectionTracker::new(0);
        let first = connection(1);
        let second = connection(2);
        tracker.observe(1, true, false).unwrap();
        assert_eq!(
            tracker.observe(2, true, true),
            Ok(UsbConnectionEvent::Disconnected(first))
        );
        assert_eq!(tracker.active(), None);
        assert_eq!(
            tracker.observe(3, false, false),
            Ok(UsbConnectionEvent::None),
            "simultaneous SOF must not survive reset"
        );
        assert_eq!(
            tracker.observe(4, true, false),
            Ok(UsbConnectionEvent::Connected(second))
        );
    }

    #[test]
    fn final_connection_epoch_is_used_once_then_exhaustion_is_sticky() {
        let mut tracker = UsbConnectionTracker {
            connection: None,
            suspended: false,
            last_sof_millis: None,
            last_now: 0,
            last_epoch: u64::MAX - 1,
            fault: None,
        };
        let final_connection = connection(u64::MAX);
        assert_eq!(
            tracker.observe(1, true, false),
            Ok(UsbConnectionEvent::Connected(final_connection))
        );
        assert_eq!(
            tracker.observe(2, false, true),
            Ok(UsbConnectionEvent::Disconnected(final_connection))
        );
        assert_eq!(
            tracker.observe(3, true, false),
            Err(UsbConnectionFault::EpochExhausted)
        );
        assert_eq!(tracker.active(), None);
        assert_eq!(tracker.connection(), None);
        assert_eq!(
            tracker.observe(4, true, false),
            Err(UsbConnectionFault::EpochExhausted)
        );
    }

    #[test]
    fn connection_clock_regression_is_sticky_and_closes_ownership() {
        let mut tracker = UsbConnectionTracker::new(10);
        tracker.observe(11, true, false).unwrap();
        assert_eq!(
            tracker.observe(9, true, false),
            Err(UsbConnectionFault::ClockRegression)
        );
        assert_eq!(tracker.active(), None);
        assert_eq!(
            tracker.observe(12, true, false),
            Err(UsbConnectionFault::ClockRegression)
        );
    }

    #[test]
    fn sequence_gate_starts_at_zero_and_rejects_duplicates_and_gaps() {
        let first = connection(1);
        let mut sequences = ExactNextSequenceGate::new(first);
        assert_eq!(sequences.next_expected(), Some(0));
        assert_eq!(sequences.accept(first, 0), Ok(()));
        assert_eq!(sequences.next_expected(), Some(1));
        assert_eq!(
            sequences.accept(first, 0),
            Err(SequenceRefusal::Duplicate {
                expected: 1,
                observed: 0,
            })
        );
        assert_eq!(
            sequences.accept(first, 2),
            Err(SequenceRefusal::Gap {
                expected: 1,
                observed: 2,
            })
        );
        assert_eq!(sequences.next_expected(), Some(1));
        assert_eq!(sequences.accept(first, 1), Ok(()));
    }

    #[test]
    fn sequence_gate_rejects_stale_connections_without_advancing() {
        let first = connection(1);
        let second = connection(2);
        let mut sequences = ExactNextSequenceGate::new(second);
        assert_eq!(
            sequences.accept(first, 0),
            Err(SequenceRefusal::WrongConnection {
                expected: second,
                observed: first,
            })
        );
        assert_eq!(sequences.next_expected(), Some(0));
    }

    #[test]
    fn sequence_exhaustion_refuses_maximum_and_never_wraps() {
        let first = connection(1);
        let mut sequences = ExactNextSequenceGate {
            connection: first,
            next: Some(u64::MAX),
        };
        assert_eq!(
            sequences.accept(first, u64::MAX),
            Err(SequenceRefusal::Exhausted)
        );
        assert_eq!(sequences.next_expected(), None);
        assert_eq!(sequences.accept(first, 0), Err(SequenceRefusal::Exhausted));
    }
}
