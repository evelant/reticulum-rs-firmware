use super::*;
use crate::{LoRaFrequencyRange, LoRaProfileConfig, RNS_MTU, SX1262_FRAME_MTU};

const RANGE: LoRaFrequencyRange = match LoRaFrequencyRange::try_new(863_000_000, 928_000_000) {
    Ok(range) => range,
    Err(_) => panic!("invalid test range"),
};

fn profile() -> LoRaProfile {
    LoRaProfile::validate(
        LoRaProfileConfig {
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

fn non_integral_symbol_profile() -> LoRaProfile {
    LoRaProfile::validate(
        LoRaProfileConfig {
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
        LogicalPacketChannelAccess::try_start(config(3), airtime, 10_000, 1_000_000, 0).unwrap();

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
    let mut access =
        LogicalPacketChannelAccess::try_start(narrow_config(2, 10, 1), airtime, 0, 1_000_000, 0)
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
        LogicalPacketAccessAction::Reject(LogicalPacketAccessRejection::ClearObservationTooOld {
            clear_observed_at_us: 10,
            dispatch_start_observed_at_us: 14,
            predicted_first_rf_start_us: 16,
            observed_age_us: 6,
            maximum_age_us: 5,
        })
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
    let mut access =
        LogicalPacketChannelAccess::try_start(narrow_config(2, 10, 1), airtime, 0, 2_000_000, 0)
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
        LogicalPacketChannelAccess::try_start(policy, airtime, 0, dispatch_bound + 1, 0).unwrap();
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
