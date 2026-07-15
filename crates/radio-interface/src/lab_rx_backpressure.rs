//! Build-time policy and one-shot state for the lab RX backpressure hook.
//!
//! This module owns no timer and cannot stall an executor. The target-specific
//! lab image must explicitly opt into the hook, await its platform timer and
//! report the resulting receive diagnostics.

/// Time reserved after the lab stall for the ingress owner to wake, expire the
/// pending fragment, emit evidence and feed its supervisor watchdog.
///
/// The current target uses a 30-second watchdog, so this five-second reserve
/// caps an instrumented stall at 25 seconds. A profile whose fragment timeout
/// does not leave this reserve cannot enable the hook; shortening its protocol
/// timeout to make the test fit would invalidate the qualification.
pub const LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US: u64 = 5_000_000;

/// Validated duration for the opt-in lab RX backpressure stall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LabRxBackpressureStall {
    duration_us: u64,
    maximum_duration_us: u64,
}

impl LabRxBackpressureStall {
    /// Validate a stall against the active fragment and watchdog policy.
    ///
    /// The stall must pass the fragment deadline so queued continuations are
    /// stale when ingress resumes. It must also leave the fixed service margin
    /// before the supervisor watchdog deadline.
    pub const fn validate(
        duration_us: u64,
        fragment_timeout_us: u64,
        watchdog_timeout_us: u64,
    ) -> Result<Self, LabRxBackpressureValidationError> {
        if watchdog_timeout_us <= LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US {
            return Err(
                LabRxBackpressureValidationError::WatchdogCannotReserveServiceMargin {
                    watchdog_timeout_us,
                    required_margin_us: LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US,
                },
            );
        }
        if duration_us <= fragment_timeout_us {
            return Err(
                LabRxBackpressureValidationError::StallDoesNotPassFragmentDeadline {
                    duration_us,
                    fragment_timeout_us,
                },
            );
        }

        let maximum_duration_us = watchdog_timeout_us - LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US;
        if duration_us > maximum_duration_us {
            return Err(
                LabRxBackpressureValidationError::StallExceedsWatchdogBudget {
                    duration_us,
                    maximum_duration_us,
                    watchdog_timeout_us,
                    reserved_service_margin_us: LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US,
                },
            );
        }

        Ok(Self {
            duration_us,
            maximum_duration_us,
        })
    }

    /// Exact duration requested by the instrumented build.
    pub const fn duration_us(self) -> u64 {
        self.duration_us
    }

    /// Longest stall allowed while retaining the watchdog service margin.
    pub const fn maximum_duration_us(self) -> u64 {
        self.maximum_duration_us
    }
}

/// Rejection from build-time lab backpressure policy validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabRxBackpressureValidationError {
    /// The watchdog period cannot retain the required post-stall service time.
    WatchdogCannotReserveServiceMargin {
        watchdog_timeout_us: u64,
        required_margin_us: u64,
    },
    /// The stall would wake before or exactly on the fragment deadline.
    StallDoesNotPassFragmentDeadline {
        duration_us: u64,
        fragment_timeout_us: u64,
    },
    /// The stall would consume the watchdog's post-stall service reserve.
    StallExceedsWatchdogBudget {
        duration_us: u64,
        maximum_duration_us: u64,
        watchdog_timeout_us: u64,
        reserved_service_margin_us: u64,
    },
}

/// Observable state of the compile-time-enabled one-shot hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabRxBackpressureHookState {
    /// Waiting for ingress to observe its first pending split packet.
    Armed,
    /// The target accepted the trigger and is awaiting its async timer.
    Stalling,
    /// The target returned from the timer and serviced protocol expiry.
    Completed,
}

/// Target-independent one-shot transition guard for the HIL hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LabRxBackpressureHook {
    state: LabRxBackpressureHookState,
}

impl LabRxBackpressureHook {
    /// Arm a new hook for the first `AwaitingContinuation` observation.
    pub const fn new() -> Self {
        Self {
            state: LabRxBackpressureHookState::Armed,
        }
    }

    /// Record an `AwaitingContinuation` observation.
    ///
    /// `true` authorizes exactly one target-side stall. Later observations are
    /// inert even if the caller accidentally offers them again.
    pub fn observe_awaiting_continuation(&mut self) -> bool {
        if self.state == LabRxBackpressureHookState::Armed {
            self.state = LabRxBackpressureHookState::Stalling;
            true
        } else {
            false
        }
    }

    /// Mark the timer and immediate expiry-service step complete.
    pub fn complete(&mut self) -> Result<(), LabRxBackpressureTransitionError> {
        if self.state != LabRxBackpressureHookState::Stalling {
            return Err(LabRxBackpressureTransitionError::NotStalling(self.state));
        }
        self.state = LabRxBackpressureHookState::Completed;
        Ok(())
    }

    /// Current state for artifact and heartbeat evidence.
    pub const fn state(self) -> LabRxBackpressureHookState {
        self.state
    }

    /// Whether the one-shot trigger has been accepted.
    pub const fn triggered(self) -> bool {
        !matches!(self.state, LabRxBackpressureHookState::Armed)
    }

    /// Whether the target completed its timer and expiry-service step.
    pub const fn completed(self) -> bool {
        matches!(self.state, LabRxBackpressureHookState::Completed)
    }
}

impl Default for LabRxBackpressureHook {
    fn default() -> Self {
        Self::new()
    }
}

/// Invalid one-shot lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabRxBackpressureTransitionError {
    /// Completion was requested before a trigger or after prior completion.
    NotStalling(LabRxBackpressureHookState),
}

#[cfg(test)]
mod tests {
    use super::*;

    const WATCHDOG_US: u64 = 30_000_000;
    const FRAGMENT_TIMEOUT_US: u64 = 6_000_000;

    #[test]
    fn stall_must_strictly_pass_fragment_deadline() {
        for duration_us in [0, FRAGMENT_TIMEOUT_US - 1, FRAGMENT_TIMEOUT_US] {
            assert_eq!(
                LabRxBackpressureStall::validate(duration_us, FRAGMENT_TIMEOUT_US, WATCHDOG_US),
                Err(
                    LabRxBackpressureValidationError::StallDoesNotPassFragmentDeadline {
                        duration_us,
                        fragment_timeout_us: FRAGMENT_TIMEOUT_US,
                    }
                )
            );
        }
        assert!(
            LabRxBackpressureStall::validate(
                FRAGMENT_TIMEOUT_US + 1,
                FRAGMENT_TIMEOUT_US,
                WATCHDOG_US
            )
            .is_ok()
        );
    }

    #[test]
    fn stall_retains_fixed_watchdog_service_margin() {
        let maximum_duration_us = WATCHDOG_US - LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US;
        let stall =
            LabRxBackpressureStall::validate(maximum_duration_us, FRAGMENT_TIMEOUT_US, WATCHDOG_US)
                .unwrap();
        assert_eq!(stall.duration_us(), maximum_duration_us);
        assert_eq!(stall.maximum_duration_us(), maximum_duration_us);

        assert_eq!(
            LabRxBackpressureStall::validate(
                maximum_duration_us + 1,
                FRAGMENT_TIMEOUT_US,
                WATCHDOG_US
            ),
            Err(
                LabRxBackpressureValidationError::StallExceedsWatchdogBudget {
                    duration_us: maximum_duration_us + 1,
                    maximum_duration_us,
                    watchdog_timeout_us: WATCHDOG_US,
                    reserved_service_margin_us: LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US,
                }
            )
        );
    }

    #[test]
    fn watchdog_must_be_longer_than_service_margin() {
        assert_eq!(
            LabRxBackpressureStall::validate(2, 1, LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US),
            Err(
                LabRxBackpressureValidationError::WatchdogCannotReserveServiceMargin {
                    watchdog_timeout_us: LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US,
                    required_margin_us: LAB_RX_BACKPRESSURE_WATCHDOG_MARGIN_US,
                }
            )
        );
    }

    #[test]
    fn first_observation_is_the_only_trigger() {
        let mut hook = LabRxBackpressureHook::new();
        assert_eq!(hook.state(), LabRxBackpressureHookState::Armed);
        assert!(!hook.triggered());
        assert!(!hook.completed());

        assert!(hook.observe_awaiting_continuation());
        assert_eq!(hook.state(), LabRxBackpressureHookState::Stalling);
        assert!(hook.triggered());
        assert!(!hook.completed());
        assert!(!hook.observe_awaiting_continuation());

        assert_eq!(hook.complete(), Ok(()));
        assert_eq!(hook.state(), LabRxBackpressureHookState::Completed);
        assert!(hook.triggered());
        assert!(hook.completed());
        assert!(!hook.observe_awaiting_continuation());
    }

    #[test]
    fn completion_requires_an_active_stall() {
        let mut hook = LabRxBackpressureHook::new();
        assert_eq!(
            hook.complete(),
            Err(LabRxBackpressureTransitionError::NotStalling(
                LabRxBackpressureHookState::Armed
            ))
        );
        assert!(hook.observe_awaiting_continuation());
        hook.complete().unwrap();
        assert_eq!(
            hook.complete(),
            Err(LabRxBackpressureTransitionError::NotStalling(
                LabRxBackpressureHookState::Completed
            ))
        );
    }
}
