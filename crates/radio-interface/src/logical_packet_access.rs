//! Deterministic channel access and airtime accounting for one logical packet.
//!
//! This module owns no clock, entropy source, executor, packet buffer, radio or
//! transmit capability. The sole radio dispatcher supplies every monotonic
//! timestamp, CAD result and random sample explicitly. One state machine covers
//! one complete RNode packet, so a split packet receives one CAD contest and
//! requests one aggregate airtime reservation rather than independent
//! per-frame grants. Every contender starts with caller-randomized backoff. A
//! clear CAD is timestamp-bound and expires under a configured freshness limit.
//! CAD and backoff remain definitely unpermitted: only an explicit post-permit
//! transition sampled at dispatcher start can emit a transmit action. Bounded
//! pre-RF setup, RF airtime, split-frame turnaround and a nonzero reconciliation
//! guard must all fit strictly before the packet-owner return deadline.

use reticulum_rns_conformance::{LengthError, validate_rns_packet_len, validate_sx1262_frame_len};

use crate::{
    LabRxProfile, RNODE_LORA_HEADER_LEN, lab_rx_profile::conservative_lora_frame_time_on_air_us,
    plan_frames,
};

/// Conservative whole-microsecond LoRa airtime for an RNode packet's frames.
///
/// The lengths include each physical frame's one-byte RNode header. The
/// aggregate is the indivisible transmit-airtime reservation consumed by a
/// logical packet; callers must not budget split frames independently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnodePacketAirtime {
    first_frame_len: usize,
    first_frame_time_on_air_us: u64,
    second_frame_len: Option<usize>,
    second_frame_time_on_air_us: Option<u64>,
    aggregate_time_on_air_us: u64,
}

impl RnodePacketAirtime {
    /// First physical-frame length, including its RNode header.
    pub const fn first_frame_len(self) -> usize {
        self.first_frame_len
    }

    /// First physical-frame LoRa airtime in microseconds.
    pub const fn first_frame_time_on_air_us(self) -> u64 {
        self.first_frame_time_on_air_us
    }

    /// Second physical-frame length for a split packet, including its header.
    pub const fn second_frame_len(self) -> Option<usize> {
        self.second_frame_len
    }

    /// Second physical-frame LoRa airtime for a split packet.
    pub const fn second_frame_time_on_air_us(self) -> Option<u64> {
        self.second_frame_time_on_air_us
    }

    /// Number of physical frames represented by this logical packet.
    pub const fn frame_count(self) -> u8 {
        if self.second_frame_len.is_some() {
            2
        } else {
            1
        }
    }

    /// Sum of all physical-frame airtimes in microseconds.
    ///
    /// This is the single budget amount that must be reserved after clear CAD
    /// and before the packet can receive a transmit action.
    pub const fn aggregate_time_on_air_us(self) -> u64 {
        self.aggregate_time_on_air_us
    }
}

/// Rejection while calculating physical-frame or logical-packet airtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RnodeAirtimeError {
    /// An SX1262 frame cannot carry an RNode packet without its one-byte header.
    MissingRnodeHeader,
    /// A physical frame exceeded the SX1262 payload limit.
    FrameTooLong(LengthError),
    /// A logical packet exceeded the project's base RNS MTU.
    PacketTooLong(LengthError),
    /// A checked length or aggregate-airtime calculation overflowed.
    ArithmeticOverflow,
}

impl LabRxProfile {
    /// Calculate a conservative whole-microsecond LoRa airtime ceiling.
    ///
    /// `frame_len` includes the one-byte RNode header and must be in
    /// `1..=255`. The calculation uses this validated profile's spreading
    /// factor, bandwidth, coding rate, explicit-header mode, CRC-bearing RNode
    /// packet mode and full `u16` preamble length. It deliberately returns
    /// `u64`; a legal long preamble can exceed the `u32` range used by the
    /// underlying modulation helper's optional-preamble path.
    pub const fn physical_frame_time_on_air_us(
        self,
        frame_len: usize,
    ) -> Result<u64, RnodeAirtimeError> {
        if frame_len < RNODE_LORA_HEADER_LEN {
            return Err(RnodeAirtimeError::MissingRnodeHeader);
        }
        if let Err(error) = validate_sx1262_frame_len(frame_len) {
            return Err(RnodeAirtimeError::FrameTooLong(error));
        }

        // The checked SX1262 maximum is exactly `u8::MAX`.
        match conservative_lora_frame_time_on_air_us(self, frame_len as u8) {
            Some(airtime_us) => Ok(airtime_us),
            None => Err(RnodeAirtimeError::ArithmeticOverflow),
        }
    }

    /// Calculate conservative one- or two-frame airtime for a base RNS packet.
    ///
    /// Packet lengths through 254 bytes use one physical frame. Lengths 255
    /// through 500 use two. Each physical length includes its own RNode header,
    /// and the returned aggregate is checked before construction.
    pub const fn rnode_packet_airtime(
        self,
        packet_len: usize,
    ) -> Result<RnodePacketAirtime, RnodeAirtimeError> {
        if let Err(error) = validate_rns_packet_len(packet_len) {
            return Err(RnodeAirtimeError::PacketTooLong(error));
        }
        let plan = match plan_frames(packet_len) {
            Ok(plan) => plan,
            Err(_) => return Err(RnodeAirtimeError::ArithmeticOverflow),
        };
        let first_frame_len = match plan.first_data_len.checked_add(RNODE_LORA_HEADER_LEN) {
            Some(frame_len) => frame_len,
            None => return Err(RnodeAirtimeError::ArithmeticOverflow),
        };
        let first_frame_time_on_air_us = match self.physical_frame_time_on_air_us(first_frame_len) {
            Ok(airtime_us) => airtime_us,
            Err(error) => return Err(error),
        };

        let (second_frame_len, second_frame_time_on_air_us) = match plan.second_data_len {
            Some(second_data_len) => {
                let frame_len = match second_data_len.checked_add(RNODE_LORA_HEADER_LEN) {
                    Some(frame_len) => frame_len,
                    None => return Err(RnodeAirtimeError::ArithmeticOverflow),
                };
                let airtime_us = match self.physical_frame_time_on_air_us(frame_len) {
                    Ok(airtime_us) => airtime_us,
                    Err(error) => return Err(error),
                };
                (Some(frame_len), Some(airtime_us))
            }
            None => (None, None),
        };
        let aggregate_time_on_air_us = match second_frame_time_on_air_us {
            Some(second_us) => match first_frame_time_on_air_us.checked_add(second_us) {
                Some(aggregate_us) => aggregate_us,
                None => return Err(RnodeAirtimeError::ArithmeticOverflow),
            },
            None => first_frame_time_on_air_us,
        };

        Ok(RnodePacketAirtime {
            first_frame_len,
            first_frame_time_on_air_us,
            second_frame_len,
            second_frame_time_on_air_us,
            aggregate_time_on_air_us,
        })
    }
}

/// Validated bounded-CAD and random-backoff policy for one logical packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogicalPacketAccessConfig {
    maximum_cad_attempts: u8,
    minimum_backoff_us: u64,
    maximum_backoff_us: u64,
    busy_retry_holdoff_us: u64,
    maximum_clear_to_first_rf_age_us: u64,
    maximum_pre_first_rf_setup_us: u64,
    maximum_inter_frame_turnaround_us: u64,
    post_tx_reconciliation_guard_us: u64,
}

impl LogicalPacketAccessConfig {
    /// Validate an explicit channel-access policy.
    ///
    /// `maximum_cad_attempts` counts the initial CAD. Each busy result before
    /// the final attempt scales a caller-supplied `u64` random sample across
    /// the inclusive backoff interval without wrapping either bound. The same
    /// interval also applies before the first CAD, so every contender backs off
    /// from a caller-supplied initial random sample.
    ///
    /// `maximum_clear_to_first_rf_age_us` is a required finite freshness bound
    /// from the clear-CAD observation to predicted first RF emission.
    /// `maximum_pre_first_rf_setup_us` bounds work from the immediate dispatcher
    /// start sample through first `SetTx`/on-air, while
    /// `maximum_inter_frame_turnaround_us` bounds the non-RF gap between split
    /// frames. `post_tx_reconciliation_guard_us` is time reserved after final
    /// predicted RF completion and strictly before owner reconciliation. Every
    /// non-RF bound must be nonzero.
    pub const fn try_new(
        maximum_cad_attempts: u8,
        minimum_backoff_us: u64,
        maximum_backoff_us: u64,
        maximum_clear_to_first_rf_age_us: u64,
        maximum_pre_first_rf_setup_us: u64,
        maximum_inter_frame_turnaround_us: u64,
        post_tx_reconciliation_guard_us: u64,
    ) -> Result<Self, LogicalPacketAccessConfigError> {
        Self::try_new_with_busy_retry_holdoff(
            maximum_cad_attempts,
            minimum_backoff_us,
            maximum_backoff_us,
            0,
            maximum_clear_to_first_rf_age_us,
            maximum_pre_first_rf_setup_us,
            maximum_inter_frame_turnaround_us,
            post_tx_reconciliation_guard_us,
        )
    }

    /// Validate a channel-access policy with a distinct post-busy holdoff.
    ///
    /// `busy_retry_holdoff_us` is added before the normal randomized backoff
    /// after each non-final busy CAD. It does not delay the initial CAD. A
    /// radio profile can therefore require every retry to wait out one maximum
    /// physical frame while retaining the short randomized RNode contention
    /// envelope for initial access and collision de-synchronization.
    #[allow(clippy::too_many_arguments)]
    pub const fn try_new_with_busy_retry_holdoff(
        maximum_cad_attempts: u8,
        minimum_backoff_us: u64,
        maximum_backoff_us: u64,
        busy_retry_holdoff_us: u64,
        maximum_clear_to_first_rf_age_us: u64,
        maximum_pre_first_rf_setup_us: u64,
        maximum_inter_frame_turnaround_us: u64,
        post_tx_reconciliation_guard_us: u64,
    ) -> Result<Self, LogicalPacketAccessConfigError> {
        if maximum_cad_attempts == 0 {
            return Err(LogicalPacketAccessConfigError::ZeroCadAttempts);
        }
        if minimum_backoff_us > maximum_backoff_us {
            return Err(LogicalPacketAccessConfigError::ReversedBackoffRange {
                minimum_backoff_us,
                maximum_backoff_us,
            });
        }
        if minimum_backoff_us == maximum_backoff_us {
            return Err(LogicalPacketAccessConfigError::DegenerateBackoffRange {
                backoff_us: minimum_backoff_us,
            });
        }
        if busy_retry_holdoff_us
            .checked_add(maximum_backoff_us)
            .is_none()
        {
            return Err(LogicalPacketAccessConfigError::BusyRetryBackoffOverflow {
                busy_retry_holdoff_us,
                maximum_backoff_us,
            });
        }
        if maximum_clear_to_first_rf_age_us == 0 {
            return Err(LogicalPacketAccessConfigError::ZeroClearToFirstRfAge);
        }
        if maximum_clear_to_first_rf_age_us == u64::MAX {
            return Err(LogicalPacketAccessConfigError::UnboundedClearToFirstRfAge);
        }
        if maximum_pre_first_rf_setup_us == 0 {
            return Err(LogicalPacketAccessConfigError::ZeroPreFirstRfSetup);
        }
        if maximum_inter_frame_turnaround_us == 0 {
            return Err(LogicalPacketAccessConfigError::ZeroInterFrameTurnaround);
        }
        if maximum_clear_to_first_rf_age_us < maximum_pre_first_rf_setup_us {
            return Err(
                LogicalPacketAccessConfigError::ClearFreshnessShorterThanPreFirstRfSetup {
                    maximum_clear_to_first_rf_age_us,
                    maximum_pre_first_rf_setup_us,
                },
            );
        }
        if post_tx_reconciliation_guard_us == 0 {
            return Err(LogicalPacketAccessConfigError::ZeroPostTxReconciliationGuard);
        }
        Ok(Self {
            maximum_cad_attempts,
            minimum_backoff_us,
            maximum_backoff_us,
            busy_retry_holdoff_us,
            maximum_clear_to_first_rf_age_us,
            maximum_pre_first_rf_setup_us,
            maximum_inter_frame_turnaround_us,
            post_tx_reconciliation_guard_us,
        })
    }

    /// Maximum number of CAD operations, including the initial attempt.
    pub const fn maximum_cad_attempts(self) -> u8 {
        self.maximum_cad_attempts
    }

    /// Smallest inclusive random backoff in microseconds.
    pub const fn minimum_backoff_us(self) -> u64 {
        self.minimum_backoff_us
    }

    /// Largest inclusive random backoff in microseconds.
    pub const fn maximum_backoff_us(self) -> u64 {
        self.maximum_backoff_us
    }

    /// Fixed holdoff added before randomized backoff after a busy CAD.
    pub const fn busy_retry_holdoff_us(self) -> u64 {
        self.busy_retry_holdoff_us
    }

    /// Earliest possible retry interval after a busy CAD.
    pub const fn minimum_busy_retry_interval_us(self) -> u64 {
        self.busy_retry_holdoff_us + self.minimum_backoff_us
    }

    /// Latest possible retry interval after a busy CAD.
    pub const fn maximum_busy_retry_interval_us(self) -> u64 {
        self.busy_retry_holdoff_us + self.maximum_backoff_us
    }

    /// Maximum inclusive age of clear CAD at predicted first RF emission.
    pub const fn maximum_clear_to_first_rf_age_us(self) -> u64 {
        self.maximum_clear_to_first_rf_age_us
    }

    /// Maximum dispatcher setup time before first `SetTx`/on-air.
    pub const fn maximum_pre_first_rf_setup_us(self) -> u64 {
        self.maximum_pre_first_rf_setup_us
    }

    /// Maximum non-RF turnaround between the two frames of a split packet.
    pub const fn maximum_inter_frame_turnaround_us(self) -> u64 {
        self.maximum_inter_frame_turnaround_us
    }

    /// Nonzero time reserved after RF completion for owner reconciliation.
    pub const fn post_tx_reconciliation_guard_us(self) -> u64 {
        self.post_tx_reconciliation_guard_us
    }
}

/// Invalid bounded-CAD/backoff configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalPacketAccessConfigError {
    /// At least one CAD is required before transmission can be authorized.
    ZeroCadAttempts,
    /// The inclusive random-backoff bounds were supplied in descending order.
    ReversedBackoffRange {
        /// Supplied lower bound.
        minimum_backoff_us: u64,
        /// Supplied upper bound.
        maximum_backoff_us: u64,
    },
    /// A fixed delay cannot randomize simultaneous contenders.
    DegenerateBackoffRange {
        /// Sole delay supplied as both inclusive bounds.
        backoff_us: u64,
    },
    /// The fixed busy holdoff plus the largest random backoff would overflow.
    BusyRetryBackoffOverflow {
        /// Fixed interval applied only after a busy CAD.
        busy_retry_holdoff_us: u64,
        /// Largest randomized contention backoff.
        maximum_backoff_us: u64,
    },
    /// A zero freshness bound would make every later permit ambiguous.
    ZeroClearToFirstRfAge,
    /// An all-epoch freshness bound would not meaningfully expire clear CAD.
    UnboundedClearToFirstRfAge,
    /// Dispatcher-to-first-RF setup must have an explicit nonzero bound.
    ZeroPreFirstRfSetup,
    /// Split-frame non-RF turnaround must have an explicit nonzero bound.
    ZeroInterFrameTurnaround,
    /// Clear freshness must permit at least the maximum pre-first-RF setup.
    ClearFreshnessShorterThanPreFirstRfSetup {
        /// Configured maximum clear-to-first-RF age.
        maximum_clear_to_first_rf_age_us: u64,
        /// Configured maximum dispatcher-to-first-RF setup.
        maximum_pre_first_rf_setup_us: u64,
    },
    /// RF completion must leave nonzero time for owner reconciliation.
    ZeroPostTxReconciliationGuard,
}

/// Public diagnostic phase of one logical-packet channel-access contest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalPacketAccessPhase {
    /// The caller must perform one CAD operation.
    AwaitingCad,
    /// The caller must wait until the initial or post-busy absolute timestamp.
    BackingOff,
    /// CAD was clear; the job remains unpermitted while awaiting a real grant.
    AwaitingPermit,
    /// A separate post-permit transition authorized this logical packet.
    TransmitAuthorized,
    /// The packet was rejected and can never be authorized by this machine.
    Rejected,
}

/// Scalar action emitted by the logical-packet channel-access machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalPacketAccessAction {
    /// Perform one CAD operation and report its result with the sampled time.
    PerformCad {
        /// One-based bounded attempt number.
        attempt: u8,
    },
    /// Wait without touching the radio, then poll the machine again.
    WaitUntil {
        /// Absolute monotonic microsecond timestamp at which retry is allowed.
        retry_at_us: u64,
        /// One-based attempt number that follows the wait.
        next_attempt: u8,
    },
    /// CAD was clear; request one permit for the whole logical packet.
    ///
    /// This action is not transmit authorization. The caller must obtain a
    /// real node-core permit and atomically reserve the reported aggregate
    /// airtime before invoking the post-permit transition.
    ChannelClear {
        /// Number of CAD operations completed before the clear result.
        cad_attempts: u8,
        /// Number of physical RNode frames covered by the requested permit.
        frame_count: u8,
        /// RF-only airtime that the permit must reserve for every frame.
        required_aggregate_rf_time_on_air_us: u64,
        /// Monotonic timestamp sampled when CAD reported clear.
        clear_observed_at_us: u64,
        /// Inclusive maximum age of clear CAD at predicted first RF emission.
        maximum_clear_to_first_rf_age_us: u64,
        /// Maximum dispatcher work before predicted first RF emission.
        maximum_pre_first_rf_setup_us: u64,
        /// Latest dispatcher start leaving reconciliation before owner deadline.
        latest_dispatch_start_us: u64,
    },
    /// Transmit the entire logical packet without another inter-frame CAD.
    TransmitLogicalPacket {
        /// Number of CAD operations completed before the clear result.
        cad_attempts: u8,
        /// Number of physical RNode frames covered by this one authorization.
        frame_count: u8,
        /// Indivisible RF-only airtime reservation for all physical frames.
        aggregate_rf_time_on_air_us: u64,
        /// Fresh dispatcher-start timestamp sampled after the permit grant.
        dispatch_start_observed_at_us: u64,
        /// Conservative first `SetTx`/on-air boundary after dispatcher setup.
        predicted_first_rf_start_us: u64,
        /// Conservative final RF completion including split-frame turnaround.
        predicted_final_rf_complete_us: u64,
        /// Latest dispatcher start leaving reconciliation before owner deadline.
        latest_dispatch_start_us: u64,
    },
    /// Terminate without transmitting.
    Reject(LogicalPacketAccessRejection),
}

/// Terminal channel-access rejection for one logical packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalPacketAccessRejection {
    /// The one RF-airtime reservation does not cover every physical frame.
    AirtimeBudgetInsufficient {
        /// Total RF airtime required by the logical packet.
        required_us: u64,
        /// RF airtime reserved by the caller for this logical packet.
        reserved_us: u64,
    },
    /// Bounded dispatch plus reconciliation reaches or exceeds owner deadline.
    OwnerDeadlineInsufficient {
        /// Sampled or planned dispatcher start.
        dispatch_start_us: u64,
        /// First `SetTx`/on-air after maximum pre-RF setup.
        predicted_first_rf_start_us: u64,
        /// Final RF completion after airtime and any split turnaround.
        predicted_final_rf_complete_us: u64,
        /// Earliest owner reconciliation after the required guard.
        reconciliation_ready_us: u64,
        /// Absolute deadline for reconciling and returning the packet owner.
        owner_deadline_us: u64,
    },
    /// Monotonic timestamp arithmetic overflowed instead of wrapping.
    TimestampOverflow {
        /// Timestamp at which the interval begins.
        start_us: u64,
        /// Interval that could not be represented.
        interval_us: u64,
    },
    /// A sampled monotonic time moved backwards across a state transition.
    TimestampRegression {
        /// Earlier sample retained by the machine.
        previous_us: u64,
        /// New caller-supplied sample that moved backwards.
        observed_us: u64,
    },
    /// The clear-CAD observation was too old at predicted first RF emission.
    ClearObservationTooOld {
        /// Timestamp sampled when CAD reported clear.
        clear_observed_at_us: u64,
        /// Fresh timestamp sampled at the dispatcher-start boundary.
        dispatch_start_observed_at_us: u64,
        /// Predicted first RF boundary after maximum setup.
        predicted_first_rf_start_us: u64,
        /// Observed clear-to-first-RF age.
        observed_age_us: u64,
        /// Configured inclusive maximum age.
        maximum_age_us: u64,
    },
    /// Every permitted CAD attempt reported a busy channel.
    ChannelBusyExhausted {
        /// Exact number of busy CAD attempts observed.
        attempts: u8,
    },
}

/// Rejection of an observation that does not match the current phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogicalPacketAccessInputError {
    /// Phase required by the attempted operation.
    pub expected: LogicalPacketAccessPhase,
    /// Actual terminal or nonterminal phase.
    pub actual: LogicalPacketAccessPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AccessState {
    AwaitingCad {
        attempt: u8,
        not_before_us: u64,
    },
    BackingOff {
        retry_at_us: u64,
        next_attempt: u8,
    },
    AwaitingPermit {
        cad_attempts: u8,
        clear_observed_at_us: u64,
    },
    TransmitAuthorized,
    Rejected(LogicalPacketAccessRejection),
}

/// Executor-free bounded CAD/backoff state for one complete logical packet.
///
/// Construction schedules mandatory caller-randomized initial backoff and
/// checks the owner reconciliation deadline, but reserves no airtime and grants
/// no permit. The caller then performs only the scalar action returned by each
/// transition. Clear CAD enters [`LogicalPacketAccessPhase::AwaitingPermit`];
/// only [`Self::permit_granted`] can emit a transmit action after rechecking the
/// timestamp sampled at immediate dispatcher start, predicted first-RF clear
/// freshness, the owner deadline and the budget actually reserved by the
/// permit. A busy final CAD enters a
/// permanent rejected state with no fallback transmit action.
pub struct LogicalPacketChannelAccess {
    config: LogicalPacketAccessConfig,
    frame_count: u8,
    aggregate_time_on_air_us: u64,
    owner_reconciliation_deadline_us: u64,
    latest_dispatch_start_us: u64,
    initial_retry_at_us: u64,
    state: AccessState,
}

impl LogicalPacketChannelAccess {
    /// Start one definitely-unpermitted channel-access contest.
    ///
    /// `started_at_us` and `owner_reconciliation_deadline_us` share the caller's
    /// monotonic microsecond domain. `initial_random_sample` is mandatory and
    /// deterministically selects the first contention backoff. Successful
    /// construction therefore starts in [`LogicalPacketAccessPhase::BackingOff`]
    /// and never requests an immediate first CAD.
    ///
    /// The deadline is the absolute time by which the packet owner must be
    /// reconciled and returned, not merely an RF-completion deadline. Every
    /// projection includes maximum pre-first-RF setup, exact aggregate RF
    /// airtime, maximum turnaround for each split-frame gap and the configured
    /// nonzero post-TX reconciliation guard. The resulting reconciliation time
    /// must be strictly less than the owner deadline.
    pub const fn try_start(
        config: LogicalPacketAccessConfig,
        packet_airtime: RnodePacketAirtime,
        started_at_us: u64,
        owner_reconciliation_deadline_us: u64,
        initial_random_sample: u64,
    ) -> Result<Self, LogicalPacketAccessRejection> {
        let frame_count = packet_airtime.frame_count();
        let aggregate_time_on_air_us = packet_airtime.aggregate_time_on_air_us();
        let dispatch_bound_us =
            match dispatch_bound_us(config, frame_count, aggregate_time_on_air_us) {
                Ok(bound_us) => bound_us,
                Err(rejection) => return Err(rejection),
            };
        let initial_backoff_us = random_backoff_us(config, initial_random_sample);
        let initial_retry_at_us = match started_at_us.checked_add(initial_backoff_us) {
            Some(retry_at_us) => retry_at_us,
            None => {
                return Err(LogicalPacketAccessRejection::TimestampOverflow {
                    start_us: started_at_us,
                    interval_us: initial_backoff_us,
                });
            }
        };
        if let Err(rejection) = dispatch_projection(
            initial_retry_at_us,
            frame_count,
            aggregate_time_on_air_us,
            config,
            owner_reconciliation_deadline_us,
        ) {
            return Err(rejection);
        }
        // The strict deadline check above proves both subtractions safe.
        let latest_dispatch_start_us = owner_reconciliation_deadline_us - dispatch_bound_us - 1;

        Ok(Self {
            config,
            frame_count,
            aggregate_time_on_air_us,
            owner_reconciliation_deadline_us,
            latest_dispatch_start_us,
            initial_retry_at_us,
            state: AccessState::BackingOff {
                retry_at_us: initial_retry_at_us,
                next_attempt: 1,
            },
        })
    }

    /// Current diagnostic phase.
    pub const fn phase(&self) -> LogicalPacketAccessPhase {
        match self.state {
            AccessState::AwaitingCad { .. } => LogicalPacketAccessPhase::AwaitingCad,
            AccessState::BackingOff { .. } => LogicalPacketAccessPhase::BackingOff,
            AccessState::AwaitingPermit { .. } => LogicalPacketAccessPhase::AwaitingPermit,
            AccessState::TransmitAuthorized => LogicalPacketAccessPhase::TransmitAuthorized,
            AccessState::Rejected(_) => LogicalPacketAccessPhase::Rejected,
        }
    }

    /// Mandatory caller-randomized initial backoff action after construction.
    pub const fn initial_action(&self) -> LogicalPacketAccessAction {
        LogicalPacketAccessAction::WaitUntil {
            retry_at_us: self.initial_retry_at_us,
            next_attempt: 1,
        }
    }

    /// Aggregate RF-only airtime a later permit must reserve.
    ///
    /// This deliberately excludes bounded dispatcher setup, split-frame
    /// turnaround and reconciliation time; those affect owner deadlines but are
    /// not regulatory RF-airtime reservations.
    pub const fn aggregate_time_on_air_us(&self) -> u64 {
        self.aggregate_time_on_air_us
    }

    /// Terminal rejection retained by this machine, if any.
    pub const fn rejection(&self) -> Option<LogicalPacketAccessRejection> {
        match self.state {
            AccessState::Rejected(rejection) => Some(rejection),
            _ => None,
        }
    }

    /// Consume one CAD result sampled at an explicit monotonic timestamp.
    ///
    /// `channel_activity_detected == false` binds `observed_at_us` into the
    /// still-unpermitted awaiting-permit phase. It does not emit a transmit
    /// action. `random_sample` is used only when a busy result has a remaining
    /// retry; the same inputs always produce the same absolute backoff deadline.
    pub fn observe_cad(
        &mut self,
        observed_at_us: u64,
        channel_activity_detected: bool,
        random_sample: u64,
    ) -> Result<LogicalPacketAccessAction, LogicalPacketAccessInputError> {
        let (attempt, not_before_us) = match self.state {
            AccessState::AwaitingCad {
                attempt,
                not_before_us,
            } => (attempt, not_before_us),
            _ => {
                return Err(LogicalPacketAccessInputError {
                    expected: LogicalPacketAccessPhase::AwaitingCad,
                    actual: self.phase(),
                });
            }
        };
        if observed_at_us < not_before_us {
            let rejection = LogicalPacketAccessRejection::TimestampRegression {
                previous_us: not_before_us,
                observed_us: observed_at_us,
            };
            self.state = AccessState::Rejected(rejection);
            return Ok(LogicalPacketAccessAction::Reject(rejection));
        }

        if let Some(rejection) = self.deadline_rejection(observed_at_us) {
            self.state = AccessState::Rejected(rejection);
            return Ok(LogicalPacketAccessAction::Reject(rejection));
        }

        if !channel_activity_detected {
            self.state = AccessState::AwaitingPermit {
                cad_attempts: attempt,
                clear_observed_at_us: observed_at_us,
            };
            return Ok(LogicalPacketAccessAction::ChannelClear {
                cad_attempts: attempt,
                frame_count: self.frame_count,
                required_aggregate_rf_time_on_air_us: self.aggregate_time_on_air_us,
                clear_observed_at_us: observed_at_us,
                maximum_clear_to_first_rf_age_us: self.config.maximum_clear_to_first_rf_age_us,
                maximum_pre_first_rf_setup_us: self.config.maximum_pre_first_rf_setup_us,
                latest_dispatch_start_us: self.latest_dispatch_start_us,
            });
        }

        if attempt == self.config.maximum_cad_attempts {
            let rejection =
                LogicalPacketAccessRejection::ChannelBusyExhausted { attempts: attempt };
            self.state = AccessState::Rejected(rejection);
            return Ok(LogicalPacketAccessAction::Reject(rejection));
        }

        let backoff_us = busy_retry_backoff_us(self.config, random_sample);
        let retry_at_us = match observed_at_us.checked_add(backoff_us) {
            Some(retry_at_us) => retry_at_us,
            None => {
                let rejection = LogicalPacketAccessRejection::TimestampOverflow {
                    start_us: observed_at_us,
                    interval_us: backoff_us,
                };
                self.state = AccessState::Rejected(rejection);
                return Ok(LogicalPacketAccessAction::Reject(rejection));
            }
        };
        if let Some(rejection) = self.deadline_rejection(retry_at_us) {
            self.state = AccessState::Rejected(rejection);
            return Ok(LogicalPacketAccessAction::Reject(rejection));
        }

        let next_attempt = attempt + 1;
        self.state = AccessState::BackingOff {
            retry_at_us,
            next_attempt,
        };
        Ok(LogicalPacketAccessAction::WaitUntil {
            retry_at_us,
            next_attempt,
        })
    }

    /// Poll an absolute monotonic timestamp while backing off.
    ///
    /// Early polls repeat the same scalar wait action. At or after the exact
    /// retry time the next bounded CAD becomes eligible, subject to a fresh
    /// aggregate-airtime deadline check.
    pub fn poll_backoff(
        &mut self,
        observed_at_us: u64,
    ) -> Result<LogicalPacketAccessAction, LogicalPacketAccessInputError> {
        let (retry_at_us, next_attempt) = match self.state {
            AccessState::BackingOff {
                retry_at_us,
                next_attempt,
            } => (retry_at_us, next_attempt),
            _ => {
                return Err(LogicalPacketAccessInputError {
                    expected: LogicalPacketAccessPhase::BackingOff,
                    actual: self.phase(),
                });
            }
        };

        if observed_at_us < retry_at_us {
            return Ok(LogicalPacketAccessAction::WaitUntil {
                retry_at_us,
                next_attempt,
            });
        }
        if let Some(rejection) = self.deadline_rejection(observed_at_us) {
            self.state = AccessState::Rejected(rejection);
            return Ok(LogicalPacketAccessAction::Reject(rejection));
        }

        self.state = AccessState::AwaitingCad {
            attempt: next_attempt,
            not_before_us: observed_at_us,
        };
        Ok(LogicalPacketAccessAction::PerformCad {
            attempt: next_attempt,
        })
    }

    /// Apply a real permit and its atomically reserved aggregate airtime.
    ///
    /// The caller may invoke this only after the node's permit authority has
    /// granted this exact logical packet. `dispatch_start_observed_at_us` must
    /// be a fresh monotonic sample taken after that grant at the boundary just
    /// before bounded packet/radio setup begins, and
    /// `reserved_airtime_budget_us` must be the airtime actually reserved by
    /// it. The timestamp is a dispatcher-start sample, not a claim that RF is
    /// already on air. This is the sole transition capable of emitting
    /// [`LogicalPacketAccessAction::TransmitLogicalPacket`]. It rechecks both
    /// aggregate budget and deadline because either can differ from the earlier
    /// clear-CAD observation. If this returns [`LogicalPacketAccessAction::Reject`],
    /// the caller still owns the external permit and reservation and must
    /// reconcile them through their defining authority; this scalar machine
    /// cannot consume or release that capability.
    pub fn permit_granted(
        &mut self,
        dispatch_start_observed_at_us: u64,
        reserved_airtime_budget_us: u64,
    ) -> Result<LogicalPacketAccessAction, LogicalPacketAccessInputError> {
        let (cad_attempts, clear_observed_at_us) = match self.state {
            AccessState::AwaitingPermit {
                cad_attempts,
                clear_observed_at_us,
            } => (cad_attempts, clear_observed_at_us),
            _ => {
                return Err(LogicalPacketAccessInputError {
                    expected: LogicalPacketAccessPhase::AwaitingPermit,
                    actual: self.phase(),
                });
            }
        };

        if dispatch_start_observed_at_us < clear_observed_at_us {
            let rejection = LogicalPacketAccessRejection::TimestampRegression {
                previous_us: clear_observed_at_us,
                observed_us: dispatch_start_observed_at_us,
            };
            self.state = AccessState::Rejected(rejection);
            return Ok(LogicalPacketAccessAction::Reject(rejection));
        }
        let predicted_first_rf_start_us = match dispatch_start_observed_at_us
            .checked_add(self.config.maximum_pre_first_rf_setup_us)
        {
            Some(first_rf_start_us) => first_rf_start_us,
            None => {
                let rejection = LogicalPacketAccessRejection::TimestampOverflow {
                    start_us: dispatch_start_observed_at_us,
                    interval_us: self.config.maximum_pre_first_rf_setup_us,
                };
                self.state = AccessState::Rejected(rejection);
                return Ok(LogicalPacketAccessAction::Reject(rejection));
            }
        };
        let clear_age_us = predicted_first_rf_start_us - clear_observed_at_us;
        if clear_age_us > self.config.maximum_clear_to_first_rf_age_us {
            let rejection = LogicalPacketAccessRejection::ClearObservationTooOld {
                clear_observed_at_us,
                dispatch_start_observed_at_us,
                predicted_first_rf_start_us,
                observed_age_us: clear_age_us,
                maximum_age_us: self.config.maximum_clear_to_first_rf_age_us,
            };
            self.state = AccessState::Rejected(rejection);
            return Ok(LogicalPacketAccessAction::Reject(rejection));
        }

        if reserved_airtime_budget_us < self.aggregate_time_on_air_us {
            let rejection = LogicalPacketAccessRejection::AirtimeBudgetInsufficient {
                required_us: self.aggregate_time_on_air_us,
                reserved_us: reserved_airtime_budget_us,
            };
            self.state = AccessState::Rejected(rejection);
            return Ok(LogicalPacketAccessAction::Reject(rejection));
        }
        let projection = match self.dispatch_projection(dispatch_start_observed_at_us) {
            Ok(projection) => projection,
            Err(rejection) => {
                self.state = AccessState::Rejected(rejection);
                return Ok(LogicalPacketAccessAction::Reject(rejection));
            }
        };

        self.state = AccessState::TransmitAuthorized;
        Ok(LogicalPacketAccessAction::TransmitLogicalPacket {
            cad_attempts,
            frame_count: self.frame_count,
            aggregate_rf_time_on_air_us: self.aggregate_time_on_air_us,
            dispatch_start_observed_at_us,
            predicted_first_rf_start_us: projection.predicted_first_rf_start_us,
            predicted_final_rf_complete_us: projection.predicted_final_rf_complete_us,
            latest_dispatch_start_us: self.latest_dispatch_start_us,
        })
    }

    const fn deadline_rejection(
        &self,
        dispatch_start_us: u64,
    ) -> Option<LogicalPacketAccessRejection> {
        match self.dispatch_projection(dispatch_start_us) {
            Ok(_) => None,
            Err(rejection) => Some(rejection),
        }
    }

    const fn dispatch_projection(
        &self,
        dispatch_start_us: u64,
    ) -> Result<DispatchProjection, LogicalPacketAccessRejection> {
        dispatch_projection(
            dispatch_start_us,
            self.frame_count,
            self.aggregate_time_on_air_us,
            self.config,
            self.owner_reconciliation_deadline_us,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DispatchProjection {
    predicted_first_rf_start_us: u64,
    predicted_final_rf_complete_us: u64,
}

const fn dispatch_bound_us(
    config: LogicalPacketAccessConfig,
    frame_count: u8,
    aggregate_time_on_air_us: u64,
) -> Result<u64, LogicalPacketAccessRejection> {
    let through_rf_us = match config
        .maximum_pre_first_rf_setup_us
        .checked_add(aggregate_time_on_air_us)
    {
        Some(duration_us) => duration_us,
        None => {
            return Err(LogicalPacketAccessRejection::TimestampOverflow {
                start_us: config.maximum_pre_first_rf_setup_us,
                interval_us: aggregate_time_on_air_us,
            });
        }
    };
    // `RnodePacketAirtime` is opaque and can represent only one or two frames.
    let turnaround_us = if frame_count > 1 {
        config.maximum_inter_frame_turnaround_us
    } else {
        0
    };
    let through_turnaround_us = match through_rf_us.checked_add(turnaround_us) {
        Some(duration_us) => duration_us,
        None => {
            return Err(LogicalPacketAccessRejection::TimestampOverflow {
                start_us: through_rf_us,
                interval_us: turnaround_us,
            });
        }
    };
    match through_turnaround_us.checked_add(config.post_tx_reconciliation_guard_us) {
        Some(duration_us) => Ok(duration_us),
        None => Err(LogicalPacketAccessRejection::TimestampOverflow {
            start_us: through_turnaround_us,
            interval_us: config.post_tx_reconciliation_guard_us,
        }),
    }
}

const fn dispatch_projection(
    dispatch_start_us: u64,
    frame_count: u8,
    aggregate_time_on_air_us: u64,
    config: LogicalPacketAccessConfig,
    owner_reconciliation_deadline_us: u64,
) -> Result<DispatchProjection, LogicalPacketAccessRejection> {
    let predicted_first_rf_start_us =
        match dispatch_start_us.checked_add(config.maximum_pre_first_rf_setup_us) {
            Some(first_rf_start_us) => first_rf_start_us,
            None => {
                return Err(LogicalPacketAccessRejection::TimestampOverflow {
                    start_us: dispatch_start_us,
                    interval_us: config.maximum_pre_first_rf_setup_us,
                });
            }
        };
    let after_rf_airtime_us =
        match predicted_first_rf_start_us.checked_add(aggregate_time_on_air_us) {
            Some(complete_us) => complete_us,
            None => {
                return Err(LogicalPacketAccessRejection::TimestampOverflow {
                    start_us: predicted_first_rf_start_us,
                    interval_us: aggregate_time_on_air_us,
                });
            }
        };
    let turnaround_us = if frame_count > 1 {
        config.maximum_inter_frame_turnaround_us
    } else {
        0
    };
    let predicted_final_rf_complete_us = match after_rf_airtime_us.checked_add(turnaround_us) {
        Some(complete_us) => complete_us,
        None => {
            return Err(LogicalPacketAccessRejection::TimestampOverflow {
                start_us: after_rf_airtime_us,
                interval_us: turnaround_us,
            });
        }
    };
    let reconciliation_ready_us =
        match predicted_final_rf_complete_us.checked_add(config.post_tx_reconciliation_guard_us) {
            Some(ready_us) => ready_us,
            None => {
                return Err(LogicalPacketAccessRejection::TimestampOverflow {
                    start_us: predicted_final_rf_complete_us,
                    interval_us: config.post_tx_reconciliation_guard_us,
                });
            }
        };
    if reconciliation_ready_us >= owner_reconciliation_deadline_us {
        Err(LogicalPacketAccessRejection::OwnerDeadlineInsufficient {
            dispatch_start_us,
            predicted_first_rf_start_us,
            predicted_final_rf_complete_us,
            reconciliation_ready_us,
            owner_deadline_us: owner_reconciliation_deadline_us,
        })
    } else {
        Ok(DispatchProjection {
            predicted_first_rf_start_us,
            predicted_final_rf_complete_us,
        })
    }
}

const fn random_backoff_us(config: LogicalPacketAccessConfig, random_sample: u64) -> u64 {
    let span = (config.maximum_backoff_us - config.minimum_backoff_us) as u128 + 1;
    let offset = ((random_sample as u128 * span) >> 64) as u64;
    config.minimum_backoff_us + offset
}

const fn busy_retry_backoff_us(config: LogicalPacketAccessConfig, random_sample: u64) -> u64 {
    // Construction proves this addition cannot overflow.
    config.busy_retry_holdoff_us + random_backoff_us(config, random_sample)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LabRxProfileConfig, RNS_MTU, ReceiveFrequencyRange, SX1262_FRAME_MTU};

    const RANGE: ReceiveFrequencyRange =
        match ReceiveFrequencyRange::try_new(863_000_000, 928_000_000) {
            Ok(range) => range,
            Err(_) => panic!("invalid test range"),
        };

    fn profile() -> LabRxProfile {
        LabRxProfile::validate(
            LabRxProfileConfig {
                frequency_hz: Some(915_000_000),
                spreading_factor: 7,
                bandwidth_hz: 125_000,
                coding_rate_denominator: 5,
                preamble_symbols: 18,
                explicit_header: true,
                crc: true,
                iq_inverted: false,
            },
            RANGE,
        )
        .unwrap()
    }

    fn non_integral_symbol_profile() -> LabRxProfile {
        LabRxProfile::validate(
            LabRxProfileConfig {
                frequency_hz: Some(915_000_000),
                spreading_factor: 7,
                bandwidth_hz: 10_420,
                coding_rate_denominator: 5,
                preamble_symbols: 24,
                explicit_header: true,
                crc: true,
                iq_inverted: false,
            },
            RANGE,
        )
        .unwrap()
    }

    fn config(maximum_cad_attempts: u8) -> LogicalPacketAccessConfig {
        LogicalPacketAccessConfig::try_new(maximum_cad_attempts, 100, 200, 50, 5, 7, 10).unwrap()
    }

    fn narrow_config(
        maximum_cad_attempts: u8,
        maximum_clear_to_first_rf_age_us: u64,
        post_tx_reconciliation_guard_us: u64,
    ) -> LogicalPacketAccessConfig {
        LogicalPacketAccessConfig::try_new(
            maximum_cad_attempts,
            0,
            1,
            maximum_clear_to_first_rf_age_us,
            2,
            3,
            post_tx_reconciliation_guard_us,
        )
        .unwrap()
    }

    #[test]
    fn bw125_airtime_and_frame_lengths_cover_logical_boundaries() {
        let profile = profile();

        let at_254 = profile.rnode_packet_airtime(254).unwrap();
        assert_eq!(at_254.first_frame_len(), 255);
        assert_eq!(at_254.second_frame_len(), None);
        assert_eq!(at_254.first_frame_time_on_air_us(), 409_856);
        assert_eq!(at_254.aggregate_time_on_air_us(), 409_856);

        let at_255 = profile.rnode_packet_airtime(255).unwrap();
        assert_eq!(at_255.first_frame_len(), 255);
        assert_eq!(at_255.second_frame_len(), Some(2));
        assert_eq!(at_255.first_frame_time_on_air_us(), 409_856);
        assert_eq!(at_255.second_frame_time_on_air_us(), Some(41_216));
        assert_eq!(at_255.aggregate_time_on_air_us(), 451_072);

        let at_500 = profile.rnode_packet_airtime(500).unwrap();
        assert_eq!(at_500.first_frame_len(), 255);
        assert_eq!(at_500.second_frame_len(), Some(247));
        assert_eq!(at_500.first_frame_time_on_air_us(), 409_856);
        assert_eq!(at_500.second_frame_time_on_air_us(), Some(399_616));
        assert_eq!(at_500.aggregate_time_on_air_us(), 809_472);

        assert_eq!(
            profile.rnode_packet_airtime(RNS_MTU + 1),
            Err(RnodeAirtimeError::PacketTooLong(LengthError {
                actual: RNS_MTU + 1,
                maximum: RNS_MTU,
            }))
        );
        assert_eq!(
            profile.physical_frame_time_on_air_us(0),
            Err(RnodeAirtimeError::MissingRnodeHeader)
        );
        assert!(matches!(
            profile.physical_frame_time_on_air_us(SX1262_FRAME_MTU + 1),
            Err(RnodeAirtimeError::FrameTooLong(_))
        ));
    }

    #[test]
    fn non_integral_symbol_duration_is_never_rounded_below_true_airtime() {
        let profile = non_integral_symbol_profile();
        let airtime_us = profile.physical_frame_time_on_air_us(255).unwrap();

        // SX126x BW code 0x08 is exactly 32 MHz / 3,072, not the
        // rounded 10,420 Hz public label. At SF7, 1,625 quarter-symbols
        // therefore occupy exactly 4,992,000 microseconds.
        const NUMERATOR_US: u64 = 638_976_000_000_000;
        const DENOMINATOR: u64 = 128_000_000;
        assert_eq!(airtime_us, 4_992_000);
        assert!(airtime_us * DENOMINATOR >= NUMERATOR_US);
        assert!((airtime_us - 1) * DENOMINATOR < NUMERATOR_US);

        // Both the receive deadline and logical-packet reservation use the
        // same conservative calculation. The original floor-biased result was
        // 4,990,375 us and the first decimal-label ceiling was still short at
        // 4,990,404 us for this profile.
        assert_eq!(profile.maximum_frame_time_on_air_us(), airtime_us);
        assert_eq!(
            profile
                .rnode_packet_airtime(254)
                .unwrap()
                .aggregate_time_on_air_us(),
            airtime_us
        );
        assert!(airtime_us > 4_990_404);
    }

    #[test]
    fn every_contender_backs_off_from_its_initial_random_sample() {
        let airtime = profile().rnode_packet_airtime(1).unwrap();
        let early =
            LogicalPacketChannelAccess::try_start(config(1), airtime, 1_000, 1_000_000, 0).unwrap();
        let late =
            LogicalPacketChannelAccess::try_start(config(1), airtime, 1_000, 1_000_000, u64::MAX)
                .unwrap();

        assert_eq!(early.phase(), LogicalPacketAccessPhase::BackingOff);
        assert_eq!(
            early.initial_action(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 1_100,
                next_attempt: 1,
            }
        );
        assert_eq!(
            late.initial_action(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 1_200,
                next_attempt: 1,
            }
        );
    }

    #[test]
    fn clear_first_cad_awaits_fresh_permit_before_authorizing_packet() {
        let airtime = profile().rnode_packet_airtime(500).unwrap();
        let mut access =
            LogicalPacketChannelAccess::try_start(config(3), airtime, 1_000, 1_000_000, 0).unwrap();

        assert_eq!(
            access.initial_action(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 1_100,
                next_attempt: 1,
            }
        );
        assert_eq!(
            access.poll_backoff(1_099).unwrap(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 1_100,
                next_attempt: 1,
            }
        );
        assert_eq!(
            access.poll_backoff(1_100).unwrap(),
            LogicalPacketAccessAction::PerformCad { attempt: 1 }
        );
        assert_eq!(
            access.observe_cad(1_110, false, u64::MAX).unwrap(),
            LogicalPacketAccessAction::ChannelClear {
                cad_attempts: 1,
                frame_count: 2,
                required_aggregate_rf_time_on_air_us: 809_472,
                clear_observed_at_us: 1_110,
                maximum_clear_to_first_rf_age_us: 50,
                maximum_pre_first_rf_setup_us: 5,
                latest_dispatch_start_us: 190_505,
            }
        );
        assert_eq!(access.phase(), LogicalPacketAccessPhase::AwaitingPermit);
        assert_eq!(
            access
                .permit_granted(1_155, airtime.aggregate_time_on_air_us())
                .unwrap(),
            LogicalPacketAccessAction::TransmitLogicalPacket {
                cad_attempts: 1,
                frame_count: 2,
                aggregate_rf_time_on_air_us: 809_472,
                dispatch_start_observed_at_us: 1_155,
                predicted_first_rf_start_us: 1_160,
                predicted_final_rf_complete_us: 810_639,
                latest_dispatch_start_us: 190_505,
            }
        );
        assert_eq!(access.phase(), LogicalPacketAccessPhase::TransmitAuthorized);
    }

    #[test]
    fn no_transmit_action_exists_before_explicit_post_permit_transition() {
        let airtime = profile().rnode_packet_airtime(1).unwrap();
        let aggregate = airtime.aggregate_time_on_air_us();
        let mut access =
            LogicalPacketChannelAccess::try_start(config(2), airtime, 0, 1_000_000, 0).unwrap();

        assert!(!matches!(
            access.initial_action(),
            LogicalPacketAccessAction::TransmitLogicalPacket { .. }
        ));
        assert_eq!(
            access.permit_granted(0, aggregate),
            Err(LogicalPacketAccessInputError {
                expected: LogicalPacketAccessPhase::AwaitingPermit,
                actual: LogicalPacketAccessPhase::BackingOff,
            })
        );
        assert_eq!(
            access.observe_cad(0, false, 0),
            Err(LogicalPacketAccessInputError {
                expected: LogicalPacketAccessPhase::AwaitingCad,
                actual: LogicalPacketAccessPhase::BackingOff,
            })
        );
        assert_eq!(
            access.poll_backoff(100).unwrap(),
            LogicalPacketAccessAction::PerformCad { attempt: 1 }
        );
        assert_eq!(
            access.permit_granted(100, aggregate),
            Err(LogicalPacketAccessInputError {
                expected: LogicalPacketAccessPhase::AwaitingPermit,
                actual: LogicalPacketAccessPhase::AwaitingCad,
            })
        );
        let clear = access.observe_cad(101, false, 0).unwrap();
        assert!(matches!(
            clear,
            LogicalPacketAccessAction::ChannelClear { .. }
        ));
        assert!(!matches!(
            clear,
            LogicalPacketAccessAction::TransmitLogicalPacket { .. }
        ));
        assert_eq!(access.phase(), LogicalPacketAccessPhase::AwaitingPermit);
        assert_eq!(
            access.poll_backoff(101),
            Err(LogicalPacketAccessInputError {
                expected: LogicalPacketAccessPhase::BackingOff,
                actual: LogicalPacketAccessPhase::AwaitingPermit,
            })
        );

        assert!(matches!(
            access.permit_granted(102, aggregate).unwrap(),
            LogicalPacketAccessAction::TransmitLogicalPacket { .. }
        ));
        assert_eq!(
            access.permit_granted(102, aggregate),
            Err(LogicalPacketAccessInputError {
                expected: LogicalPacketAccessPhase::AwaitingPermit,
                actual: LogicalPacketAccessPhase::TransmitAuthorized,
            })
        );
    }

    #[test]
    fn busy_cad_backs_off_deterministically_and_retries() {
        let airtime = profile().rnode_packet_airtime(254).unwrap();
        let mut access =
            LogicalPacketChannelAccess::try_start(config(3), airtime, 10_000, 1_000_000, 0)
                .unwrap();

        assert_eq!(
            access.initial_action(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 10_100,
                next_attempt: 1,
            }
        );
        assert_eq!(
            access.poll_backoff(10_100).unwrap(),
            LogicalPacketAccessAction::PerformCad { attempt: 1 }
        );
        assert_eq!(
            access.observe_cad(10_110, true, 0).unwrap(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 10_210,
                next_attempt: 2,
            }
        );
        assert_eq!(
            access.poll_backoff(10_209).unwrap(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 10_210,
                next_attempt: 2,
            }
        );
        assert_eq!(
            access.poll_backoff(10_210).unwrap(),
            LogicalPacketAccessAction::PerformCad { attempt: 2 }
        );
        assert!(matches!(
            access.observe_cad(10_220, false, 42).unwrap(),
            LogicalPacketAccessAction::ChannelClear {
                cad_attempts: 2,
                frame_count: 1,
                ..
            }
        ));
        assert_eq!(access.phase(), LogicalPacketAccessPhase::AwaitingPermit);
        assert!(matches!(
            access
                .permit_granted(10_221, airtime.aggregate_time_on_air_us())
                .unwrap(),
            LogicalPacketAccessAction::TransmitLogicalPacket {
                cad_attempts: 2,
                frame_count: 1,
                ..
            }
        ));
    }

    #[test]
    fn busy_retry_holdoff_does_not_delay_initial_access_or_repeat_inside_a_frame() {
        let maximum_frame_us = profile().maximum_frame_time_on_air_us();
        let policy = LogicalPacketAccessConfig::try_new_with_busy_retry_holdoff(
            3,
            100,
            200,
            maximum_frame_us,
            50,
            5,
            7,
            10,
        )
        .unwrap();
        let airtime = profile().rnode_packet_airtime(1).unwrap();
        let mut access =
            LogicalPacketChannelAccess::try_start(policy, airtime, 10_000, 2_000_000, 0).unwrap();

        assert_eq!(policy.busy_retry_holdoff_us(), maximum_frame_us);
        assert_eq!(
            policy.minimum_busy_retry_interval_us(),
            maximum_frame_us + 100
        );
        assert_eq!(
            access.initial_action(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 10_100,
                next_attempt: 1,
            }
        );
        assert_eq!(
            access.poll_backoff(10_100).unwrap(),
            LogicalPacketAccessAction::PerformCad { attempt: 1 }
        );

        let busy_observed_at_us = 10_110;
        let retry_at_us = busy_observed_at_us + maximum_frame_us + 100;
        assert_eq!(
            access.observe_cad(busy_observed_at_us, true, 0).unwrap(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us,
                next_attempt: 2,
            }
        );
        assert!(retry_at_us > busy_observed_at_us + maximum_frame_us);
        assert_eq!(
            access.poll_backoff(retry_at_us).unwrap(),
            LogicalPacketAccessAction::PerformCad { attempt: 2 }
        );
        assert!(matches!(
            access.observe_cad(retry_at_us, false, 0).unwrap(),
            LogicalPacketAccessAction::ChannelClear {
                cad_attempts: 2,
                ..
            }
        ));
    }

    #[test]
    fn busy_exhaustion_is_terminal_and_never_forces_transmit() {
        let airtime = profile().rnode_packet_airtime(1).unwrap();
        let mut access = LogicalPacketChannelAccess::try_start(
            narrow_config(2, 10, 1),
            airtime,
            0,
            1_000_000,
            0,
        )
        .unwrap();

        assert_eq!(
            access.initial_action(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 0,
                next_attempt: 1,
            }
        );
        assert_eq!(
            access.poll_backoff(0).unwrap(),
            LogicalPacketAccessAction::PerformCad { attempt: 1 }
        );
        assert_eq!(
            access.observe_cad(1, true, 0).unwrap(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 1,
                next_attempt: 2,
            }
        );
        assert_eq!(
            access.poll_backoff(1).unwrap(),
            LogicalPacketAccessAction::PerformCad { attempt: 2 }
        );
        let exhausted = LogicalPacketAccessRejection::ChannelBusyExhausted { attempts: 2 };
        assert_eq!(
            access.observe_cad(2, true, u64::MAX).unwrap(),
            LogicalPacketAccessAction::Reject(exhausted)
        );
        assert_eq!(access.phase(), LogicalPacketAccessPhase::Rejected);
        assert_eq!(
            access.observe_cad(3, false, 0),
            Err(LogicalPacketAccessInputError {
                expected: LogicalPacketAccessPhase::AwaitingCad,
                actual: LogicalPacketAccessPhase::Rejected,
            })
        );
        assert_eq!(
            access.permit_granted(3, airtime.aggregate_time_on_air_us()),
            Err(LogicalPacketAccessInputError {
                expected: LogicalPacketAccessPhase::AwaitingPermit,
                actual: LogicalPacketAccessPhase::Rejected,
            })
        );
    }

    #[test]
    fn clear_freshness_is_inclusive_bounded_and_monotonic() {
        let airtime = profile().rnode_packet_airtime(1).unwrap();
        let aggregate = airtime.aggregate_time_on_air_us();
        let policy = narrow_config(1, 5, 1);

        let mut exact =
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, 1_000_000, 0).unwrap();
        exact.poll_backoff(0).unwrap();
        assert!(matches!(
            exact.observe_cad(10, false, 0).unwrap(),
            LogicalPacketAccessAction::ChannelClear {
                clear_observed_at_us: 10,
                maximum_clear_to_first_rf_age_us: 5,
                maximum_pre_first_rf_setup_us: 2,
                ..
            }
        ));
        assert!(matches!(
            exact.permit_granted(13, aggregate).unwrap(),
            LogicalPacketAccessAction::TransmitLogicalPacket { .. }
        ));

        let mut stale =
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, 1_000_000, 0).unwrap();
        stale.poll_backoff(0).unwrap();
        stale.observe_cad(10, false, 0).unwrap();
        assert_eq!(
            stale.permit_granted(14, aggregate).unwrap(),
            LogicalPacketAccessAction::Reject(
                LogicalPacketAccessRejection::ClearObservationTooOld {
                    clear_observed_at_us: 10,
                    dispatch_start_observed_at_us: 14,
                    predicted_first_rf_start_us: 16,
                    observed_age_us: 6,
                    maximum_age_us: 5,
                }
            )
        );

        let mut regressed =
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, 1_000_000, 0).unwrap();
        regressed.poll_backoff(0).unwrap();
        regressed.observe_cad(10, false, 0).unwrap();
        assert_eq!(
            regressed.permit_granted(9, aggregate).unwrap(),
            LogicalPacketAccessAction::Reject(LogicalPacketAccessRejection::TimestampRegression {
                previous_us: 10,
                observed_us: 9,
            })
        );
    }

    #[test]
    fn cad_result_timestamp_cannot_precede_cad_start_sample() {
        let airtime = profile().rnode_packet_airtime(1).unwrap();
        let mut access =
            LogicalPacketChannelAccess::try_start(config(1), airtime, 0, 1_000_000, 0).unwrap();
        access.poll_backoff(150).unwrap();
        assert_eq!(
            access.observe_cad(149, false, 0).unwrap(),
            LogicalPacketAccessAction::Reject(LogicalPacketAccessRejection::TimestampRegression {
                previous_us: 150,
                observed_us: 149,
            })
        );
    }

    #[test]
    fn post_permit_rechecks_aggregate_budget() {
        let airtime = profile().rnode_packet_airtime(500).unwrap();
        let aggregate = airtime.aggregate_time_on_air_us();
        let mut access = LogicalPacketChannelAccess::try_start(
            narrow_config(2, 10, 1),
            airtime,
            0,
            2_000_000,
            0,
        )
        .unwrap();
        access.poll_backoff(0).unwrap();
        assert!(matches!(
            access.observe_cad(1, false, 0).unwrap(),
            LogicalPacketAccessAction::ChannelClear {
                required_aggregate_rf_time_on_air_us: 809_472,
                ..
            }
        ));
        assert_eq!(
            access.permit_granted(2, aggregate - 1).unwrap(),
            LogicalPacketAccessAction::Reject(
                LogicalPacketAccessRejection::AirtimeBudgetInsufficient {
                    required_us: 809_472,
                    reserved_us: 809_471,
                }
            )
        );
        assert_eq!(access.phase(), LogicalPacketAccessPhase::Rejected);
    }

    #[test]
    fn reconciliation_guard_and_exact_owner_deadline_ties_reject() {
        let airtime = profile().rnode_packet_airtime(1).unwrap();
        let aggregate = airtime.aggregate_time_on_air_us();
        let pre_first_rf_setup = 2;
        let guard = 10;
        let policy = narrow_config(1, 100, guard);
        let exact_deadline = aggregate + pre_first_rf_setup + guard;

        assert!(matches!(
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, exact_deadline, 0,),
            Err(LogicalPacketAccessRejection::OwnerDeadlineInsufficient {
                dispatch_start_us: 0,
                predicted_first_rf_start_us: 2,
                predicted_final_rf_complete_us,
                reconciliation_ready_us,
                owner_deadline_us,
            }) if predicted_final_rf_complete_us == aggregate + pre_first_rf_setup
                && reconciliation_ready_us == exact_deadline
                && owner_deadline_us == exact_deadline
        ));

        let deadline = exact_deadline + 11;
        let mut exact_tie =
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, deadline, 0).unwrap();
        exact_tie.poll_backoff(0).unwrap();
        assert_eq!(
            exact_tie.observe_cad(0, false, 0).unwrap(),
            LogicalPacketAccessAction::ChannelClear {
                cad_attempts: 1,
                frame_count: 1,
                required_aggregate_rf_time_on_air_us: aggregate,
                clear_observed_at_us: 0,
                maximum_clear_to_first_rf_age_us: 100,
                maximum_pre_first_rf_setup_us: 2,
                latest_dispatch_start_us: 10,
            }
        );
        assert_eq!(
            exact_tie.permit_granted(11, aggregate).unwrap(),
            LogicalPacketAccessAction::Reject(
                LogicalPacketAccessRejection::OwnerDeadlineInsufficient {
                    dispatch_start_us: 11,
                    predicted_first_rf_start_us: 13,
                    predicted_final_rf_complete_us: 13 + aggregate,
                    reconciliation_ready_us: 13 + aggregate + guard,
                    owner_deadline_us: deadline,
                }
            )
        );

        let mut latest_allowed =
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, deadline, 0).unwrap();
        latest_allowed.poll_backoff(0).unwrap();
        latest_allowed.observe_cad(0, false, 0).unwrap();
        assert!(matches!(
            latest_allowed.permit_granted(10, aggregate).unwrap(),
            LogicalPacketAccessAction::TransmitLogicalPacket {
                latest_dispatch_start_us: 10,
                ..
            }
        ));
    }

    #[test]
    fn split_packet_consumes_one_aggregate_budget_and_one_cad_contest() {
        let airtime = profile().rnode_packet_airtime(255).unwrap();
        let aggregate = airtime.aggregate_time_on_air_us();
        assert_eq!(
            aggregate,
            airtime.first_frame_time_on_air_us() + airtime.second_frame_time_on_air_us().unwrap()
        );
        let policy = narrow_config(4, 10, 1);
        let dispatch_bound = 2 + aggregate + 3 + 1;

        assert!(matches!(
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, aggregate + 2, 0),
            Err(LogicalPacketAccessRejection::OwnerDeadlineInsufficient { .. })
        ));
        assert!(matches!(
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, dispatch_bound, 0),
            Err(LogicalPacketAccessRejection::OwnerDeadlineInsufficient {
                dispatch_start_us: 0,
                predicted_first_rf_start_us: 2,
                predicted_final_rf_complete_us,
                reconciliation_ready_us,
                owner_deadline_us,
            }) if predicted_final_rf_complete_us == 2 + aggregate + 3
                && reconciliation_ready_us == dispatch_bound
                && owner_deadline_us == dispatch_bound
        ));

        let mut access =
            LogicalPacketChannelAccess::try_start(policy, airtime, 0, dispatch_bound + 1, 0)
                .unwrap();
        assert_eq!(
            access.initial_action(),
            LogicalPacketAccessAction::WaitUntil {
                retry_at_us: 0,
                next_attempt: 1,
            }
        );
        access.poll_backoff(0).unwrap();
        assert_eq!(access.aggregate_time_on_air_us(), aggregate);
        assert_eq!(
            access.observe_cad(0, false, 0).unwrap(),
            LogicalPacketAccessAction::ChannelClear {
                cad_attempts: 1,
                frame_count: 2,
                required_aggregate_rf_time_on_air_us: 451_072,
                clear_observed_at_us: 0,
                maximum_clear_to_first_rf_age_us: 10,
                maximum_pre_first_rf_setup_us: 2,
                latest_dispatch_start_us: 0,
            }
        );
        assert_eq!(access.phase(), LogicalPacketAccessPhase::AwaitingPermit);
        assert!(matches!(
            access.permit_granted(0, aggregate).unwrap(),
            LogicalPacketAccessAction::TransmitLogicalPacket {
                aggregate_rf_time_on_air_us: 451_072,
                predicted_first_rf_start_us: 2,
                predicted_final_rf_complete_us,
                ..
            } if predicted_final_rf_complete_us == 2 + aggregate + 3
        ));
    }

    #[test]
    fn configuration_and_timestamp_overflow_fail_closed() {
        assert_eq!(
            LogicalPacketAccessConfig::try_new(0, 0, 1, 1, 1, 1, 1),
            Err(LogicalPacketAccessConfigError::ZeroCadAttempts)
        );
        assert!(matches!(
            LogicalPacketAccessConfig::try_new(1, 2, 1, 1, 1, 1, 1),
            Err(LogicalPacketAccessConfigError::ReversedBackoffRange { .. })
        ));
        assert_eq!(
            LogicalPacketAccessConfig::try_new(1, 7, 7, 1, 1, 1, 1),
            Err(LogicalPacketAccessConfigError::DegenerateBackoffRange { backoff_us: 7 })
        );
        assert_eq!(
            LogicalPacketAccessConfig::try_new_with_busy_retry_holdoff(
                1,
                0,
                2,
                u64::MAX - 1,
                1,
                1,
                1,
                1,
            ),
            Err(LogicalPacketAccessConfigError::BusyRetryBackoffOverflow {
                busy_retry_holdoff_us: u64::MAX - 1,
                maximum_backoff_us: 2,
            })
        );
        assert_eq!(
            LogicalPacketAccessConfig::try_new(1, 0, 1, 0, 1, 1, 1),
            Err(LogicalPacketAccessConfigError::ZeroClearToFirstRfAge)
        );
        assert_eq!(
            LogicalPacketAccessConfig::try_new(1, 0, 1, u64::MAX, 1, 1, 1),
            Err(LogicalPacketAccessConfigError::UnboundedClearToFirstRfAge)
        );
        assert_eq!(
            LogicalPacketAccessConfig::try_new(1, 0, 1, 1, 0, 1, 1),
            Err(LogicalPacketAccessConfigError::ZeroPreFirstRfSetup)
        );
        assert_eq!(
            LogicalPacketAccessConfig::try_new(1, 0, 1, 1, 1, 0, 1),
            Err(LogicalPacketAccessConfigError::ZeroInterFrameTurnaround)
        );
        assert_eq!(
            LogicalPacketAccessConfig::try_new(1, 0, 1, 1, 2, 1, 1),
            Err(
                LogicalPacketAccessConfigError::ClearFreshnessShorterThanPreFirstRfSetup {
                    maximum_clear_to_first_rf_age_us: 1,
                    maximum_pre_first_rf_setup_us: 2,
                }
            )
        );
        assert_eq!(
            LogicalPacketAccessConfig::try_new(1, 0, 1, 1, 1, 1, 0),
            Err(LogicalPacketAccessConfigError::ZeroPostTxReconciliationGuard)
        );
        let minimum_random_range = LogicalPacketAccessConfig::try_new(1, 7, 8, 1, 1, 1, 1).unwrap();
        assert_eq!(minimum_random_range.minimum_backoff_us(), 7);
        assert_eq!(minimum_random_range.maximum_backoff_us(), 8);
        assert_eq!(minimum_random_range.busy_retry_holdoff_us(), 0);
        assert_eq!(minimum_random_range.minimum_busy_retry_interval_us(), 7);
        assert_eq!(minimum_random_range.maximum_busy_retry_interval_us(), 8);
        assert_eq!(minimum_random_range.maximum_clear_to_first_rf_age_us(), 1);
        assert_eq!(minimum_random_range.maximum_pre_first_rf_setup_us(), 1);
        assert_eq!(minimum_random_range.maximum_inter_frame_turnaround_us(), 1);
        assert_eq!(minimum_random_range.post_tx_reconciliation_guard_us(), 1);

        let airtime = profile().rnode_packet_airtime(1).unwrap();
        assert!(matches!(
            LogicalPacketChannelAccess::try_start(
                LogicalPacketAccessConfig::try_new(1, 1, 2, 1, 1, 1, 1).unwrap(),
                airtime,
                u64::MAX,
                u64::MAX,
                0,
            ),
            Err(LogicalPacketAccessRejection::TimestampOverflow { .. })
        ));
    }

    #[test]
    fn caller_random_samples_cover_inclusive_backoff_range() {
        let policy = LogicalPacketAccessConfig::try_new(2, 100, 200, 1, 1, 1, 1).unwrap();
        assert_eq!(random_backoff_us(policy, 0), 100);
        assert_eq!(random_backoff_us(policy, u64::MAX), 200);

        let full_range = LogicalPacketAccessConfig::try_new(2, 0, u64::MAX, 1, 1, 1, 1).unwrap();
        assert_eq!(random_backoff_us(full_range, 0), 0);
        assert_eq!(random_backoff_us(full_range, u64::MAX), u64::MAX);

        let two_delays = LogicalPacketAccessConfig::try_new(2, 7, 8, 1, 1, 1, 1).unwrap();
        assert_eq!(random_backoff_us(two_delays, 0), 7);
        assert_eq!(random_backoff_us(two_delays, u64::MAX), 8);
    }
}
