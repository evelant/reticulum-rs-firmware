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

use crate::{
    LengthError, LoRaProfile, RNODE_LORA_HEADER_LEN,
    lora_profile::conservative_lora_frame_time_on_air_us, plan_frames, validate_rns_packet_len,
    validate_sx1262_frame_len,
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

impl LoRaProfile {
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
mod tests;
