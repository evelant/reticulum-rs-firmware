extern crate std;

use core::mem::size_of;

use reticulum_device_api_credentials::{CredentialGeneration, CredentialId};

use crate::{
    AbortOutcome, ActivationOutcome, ActiveLowButton, AttemptDecision, AttemptRefusal,
    BUTTON_HOLD_MILLIS, BeginFacts, BeginOutcome, ButtonEffect, CloseReason, ConnectionId,
    ConnectionRefusal, ErasedInitializationFacts, ExclusiveAcquireOutcome,
    MAX_BEGIN_PROOF_ATTEMPTS, MonotonicMillis, OrdinarySessionRefusal, PAIRING_POLICY_RAM_CEILING,
    PAIRING_WINDOW_MILLIS, PairingPolicy, PendingRef, PendingState, PolicyEvent, PolicyFault,
    RequestRefusal,
};

fn time(value: u64) -> MonotonicMillis {
    MonotonicMillis::new(value)
}

fn connection(value: u64) -> ConnectionId {
    ConnectionId::new(value).unwrap_or_else(|| panic!("test connection must be nonzero"))
}

fn pending(value: u8, generation: u64) -> PendingRef {
    PendingRef::new(
        CredentialId::new([value; 16]),
        CredentialGeneration::new(generation),
    )
    .unwrap_or_else(|_| panic!("test pending reference must be valid"))
}

fn begin_ready() -> BeginFacts {
    BeginFacts::new(true, true, true)
}

fn open(policy: &mut PairingPolicy, connection_id: ConnectionId, start: u64) {
    assert_eq!(policy.connected(time(start), connection_id), Ok(None));
    assert!(matches!(
        policy.observe_button(time(start), ActiveLowButton::High),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(start + 10), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    let effect =
        match policy.observe_button(time(start + 10 + BUTTON_HOLD_MILLIS), ActiveLowButton::Low) {
            ButtonEffect::AcquirePairingExclusive(effect) => effect,
            _ => panic!("exact hold threshold did not request exclusivity"),
        };
    match policy.exclusive_acquired(time(start + 10 + BUTTON_HOLD_MILLIS), effect) {
        ExclusiveAcquireOutcome::Opened(opened) => {
            assert_eq!(opened.connection(), connection_id);
            assert_eq!(
                opened.deadline(),
                time(start + 10 + BUTTON_HOLD_MILLIS + PAIRING_WINDOW_MILLIS)
            );
        }
        _ => panic!("exclusive acquisition did not open the window"),
    }
}

#[test]
fn constants_ids_pending_validation_and_ram_are_bounded() {
    assert_eq!(BUTTON_HOLD_MILLIS, 2_000);
    assert_eq!(PAIRING_WINDOW_MILLIS, 60_000);
    assert_eq!(MAX_BEGIN_PROOF_ATTEMPTS, 3);
    assert!(ConnectionId::new(0).is_none());
    assert!(PendingRef::new(CredentialId::new([0; 16]), CredentialGeneration::new(1)).is_err());
    assert!(PendingRef::new(CredentialId::new([1; 16]), CredentialGeneration::new(0)).is_err());
    assert!(size_of::<PairingPolicy>() <= PAIRING_POLICY_RAM_CEILING);
}

#[test]
fn hold_requires_connection_release_and_one_continuous_exact_interval() {
    let mut policy = PairingPolicy::new(PendingState::None);
    assert!(matches!(
        policy.observe_button(time(0), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert_eq!(policy.connected(time(1), connection(1)), Ok(None));
    assert!(matches!(
        policy.observe_button(time(2), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(3), ActiveLowButton::High),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(10), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(2_009), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(2_009), ActiveLowButton::High),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(2_010), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(4_009), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(4_010), ActiveLowButton::Low),
        ButtonEffect::AcquirePairingExclusive(_)
    ));
}

#[test]
fn connection_epochs_are_strict_and_replacement_clears_hold_and_window() {
    let mut policy = PairingPolicy::new(PendingState::None);
    open(&mut policy, connection(7), 0);
    let closed = policy
        .connected(time(2_100), connection(8))
        .unwrap_or_else(|error| panic!("new epoch rejected: {error:?}"))
        .unwrap_or_else(|| panic!("replacement did not close old window"));
    assert_eq!(closed.reason(), CloseReason::Disconnect);
    assert_eq!(closed.connection(), connection(7));
    assert_eq!(
        policy.connected(time(2_101), connection(8)),
        Err(ConnectionRefusal::NotStrictlyIncreasing)
    );
    assert_eq!(
        policy.connected(time(2_102), connection(6)),
        Err(ConnectionRefusal::NotStrictlyIncreasing)
    );
    assert!(matches!(
        policy.observe_button(time(2_103), ActiveLowButton::Low),
        ButtonEffect::None
    ));
}

#[test]
fn exact_deadline_wins_and_held_low_does_not_reopen() {
    let mut policy = PairingPolicy::new(PendingState::None);
    open(&mut policy, connection(1), 0);
    assert!(
        policy
            .ordinary_session(time(62_009), connection(1))
            .is_err()
    );
    assert_eq!(
        policy.poll_timeout(time(62_010)),
        PolicyEvent::Closed(crate::WindowClosed {
            reason: CloseReason::Timeout,
            window: crate::WindowId(core::num::NonZeroU64::new(1).unwrap()),
            connection: connection(1),
        })
    );
    assert!(matches!(
        policy.observe_button(time(64_100), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert!(policy.ordinary_session(time(64_101), connection(1)).is_ok());
}

#[test]
fn ordinary_permit_is_invalidated_at_exclusive_threshold() {
    let mut policy = PairingPolicy::new(PendingState::None);
    assert_eq!(policy.connected(time(0), connection(1)), Ok(None));
    let permit = policy
        .ordinary_session(time(0), connection(1))
        .unwrap_or_else(|error| panic!("ordinary session rejected: {error:?}"));
    assert!(policy.ordinary_session_is_current(&permit));
    policy.observe_button(time(1), ActiveLowButton::High);
    policy.observe_button(time(2), ActiveLowButton::Low);
    assert!(matches!(
        policy.observe_button(time(2_002), ActiveLowButton::Low),
        ButtonEffect::AcquirePairingExclusive(_)
    ));
    assert!(!policy.ordinary_session_is_current(&permit));
    assert!(matches!(
        policy.ordinary_session(time(2_003), connection(1)),
        Err(OrdinarySessionRefusal::PairingExclusive)
    ));
}

#[test]
fn initialization_requires_both_trusted_facts_and_spends_no_attempt() {
    let mut policy = PairingPolicy::new(PendingState::None);
    open(&mut policy, connection(1), 0);
    for facts in [
        ErasedInitializationFacts::new(false, false),
        ErasedInitializationFacts::new(true, false),
        ErasedInitializationFacts::new(false, true),
    ] {
        let refused = policy
            .initialize_erased(time(2_100), connection(1), facts)
            .err()
            .unwrap_or_else(|| panic!("ineligible initialization admitted"));
        assert_eq!(refused.reason(), RequestRefusal::InitializationNotEligible);
    }
    let permit = policy
        .initialize_erased(
            time(2_100),
            connection(1),
            ErasedInitializationFacts::new(true, true),
        )
        .unwrap_or_else(|error| panic!("eligible initialization rejected: {error:?}"));
    policy
        .finish_initialization(permit)
        .unwrap_or_else(|error| panic!("initialization finish rejected: {error:?}"));
    match policy.begin(time(2_101), connection(1), begin_ready()) {
        AttemptDecision::Admitted { ordinal, .. } => assert_eq!(ordinal, 1),
        _ => panic!("initialization consumed Begin/Proof attempt budget"),
    }
}

#[test]
fn every_bound_begin_refusal_spends_the_shared_budget_and_third_closes() {
    let mut policy = PairingPolicy::new(PendingState::None);
    open(&mut policy, connection(1), 0);
    for (facts, expected, ordinal) in [
        (
            BeginFacts::new(false, true, true),
            AttemptRefusal::MutationBlocked,
            1,
        ),
        (
            BeginFacts::new(true, false, true),
            AttemptRefusal::CapacityExhausted,
            2,
        ),
        (
            BeginFacts::new(true, true, false),
            AttemptRefusal::RevisionExhausted,
            3,
        ),
    ] {
        match policy.begin(time(2_100 + u64::from(ordinal)), connection(1), facts) {
            AttemptDecision::Refused {
                reason,
                ordinal: actual,
                closed_to_new_attempts,
            } => {
                assert_eq!(reason, expected);
                assert_eq!(actual, Some(ordinal));
                assert_eq!(closed_to_new_attempts.is_some(), ordinal == 3);
            }
            _ => panic!("ineligible Begin was admitted"),
        }
    }
    match policy.begin(time(2_200), connection(1), begin_ready()) {
        AttemptDecision::Refused {
            reason: AttemptRefusal::WindowNotOpen,
            ordinal: None,
            ..
        } => {}
        _ => panic!("fourth attempt was not rejected without spending"),
    }
}

#[test]
fn admitted_third_operation_survives_disconnect_for_exact_reconciliation() {
    let existing = pending(4, 9);
    let mut policy = PairingPolicy::new(PendingState::One(existing));
    open(&mut policy, connection(1), 0);
    for ordinal in 1..=2 {
        match policy.begin(time(2_100 + ordinal), connection(1), begin_ready()) {
            AttemptDecision::Refused {
                reason: AttemptRefusal::PendingExists,
                ordinal: Some(actual),
                ..
            } => assert_eq!(u64::from(actual), ordinal),
            _ => panic!("expected counted pending refusal"),
        }
    }
    let permit = match policy.proof(time(2_103), connection(1), existing) {
        AttemptDecision::Admitted {
            permit,
            ordinal: 3,
            closed_to_new_attempts: Some(closed),
        } => {
            assert_eq!(closed.reason(), CloseReason::ThirdAttempt);
            permit
        }
        _ => panic!("third exact proof was not admitted while closing new work"),
    };
    assert!(policy.operation_outstanding());
    assert_eq!(
        policy.disconnected(time(2_104), connection(1)),
        PolicyEvent::None
    );
    let activation = policy
        .proof_verified(permit)
        .unwrap_or_else(|error| panic!("owned proof completion was lost: {error:?}"));
    policy
        .finish_activation(activation, ActivationOutcome::ActiveCommitted)
        .unwrap_or_else(|error| panic!("activation reconciliation failed: {error:?}"));
    assert!(!policy.operation_outstanding());
    assert_eq!(policy.pending(), None);
}

#[test]
fn begin_reserves_one_pending_and_exact_proof_activation_clears_it() {
    let enrolled = pending(7, 11);
    let mut policy = PairingPolicy::new(PendingState::None);
    open(&mut policy, connection(1), 0);
    let begin = match policy.begin(time(2_100), connection(1), begin_ready()) {
        AttemptDecision::Admitted { permit, .. } => permit,
        _ => panic!("eligible Begin rejected"),
    };
    assert!(policy.operation_outstanding());
    policy
        .finish_begin(begin, BeginOutcome::PendingCommitted(enrolled))
        .unwrap_or_else(|error| panic!("pending commit rejected: {error:?}"));
    assert_eq!(policy.pending(), Some(enrolled));
    match policy.proof(time(2_101), connection(1), pending(8, 11)) {
        AttemptDecision::Refused {
            reason: AttemptRefusal::PendingMismatch,
            ordinal: Some(2),
            ..
        } => {}
        _ => panic!("wrong pending proof was not counted and rejected"),
    }
    let proof = match policy.proof(time(2_102), connection(1), enrolled) {
        AttemptDecision::Admitted { permit, .. } => permit,
        _ => panic!("exact pending proof rejected"),
    };
    let activation = policy
        .proof_verified(proof)
        .unwrap_or_else(|error| panic!("proof verification rejected: {error:?}"));
    policy
        .finish_activation(activation, ActivationOutcome::ActiveCommitted)
        .unwrap_or_else(|error| panic!("activation rejected: {error:?}"));
    assert_eq!(policy.pending(), None);
}

#[test]
fn rejected_proof_and_failed_activation_retain_pending() {
    let existing = pending(2, 3);
    let mut policy = PairingPolicy::new(PendingState::One(existing));
    open(&mut policy, connection(1), 0);
    let proof = match policy.proof(time(2_100), connection(1), existing) {
        AttemptDecision::Admitted { permit, .. } => permit,
        _ => panic!("proof rejected"),
    };
    policy
        .proof_rejected(proof)
        .unwrap_or_else(|error| panic!("proof rejection completion failed: {error:?}"));
    assert_eq!(policy.pending(), Some(existing));
    let proof = match policy.proof(time(2_101), connection(1), existing) {
        AttemptDecision::Admitted { permit, .. } => permit,
        _ => panic!("second proof rejected"),
    };
    let activation = policy
        .proof_verified(proof)
        .unwrap_or_else(|error| panic!("proof verification rejected: {error:?}"));
    policy
        .finish_activation(activation, ActivationOutcome::NotCommitted)
        .unwrap_or_else(|error| panic!("definite noncommit rejected: {error:?}"));
    assert_eq!(policy.pending(), Some(existing));
}

#[test]
fn abort_is_exact_durable_and_does_not_spend_attempts() {
    let existing = pending(5, 6);
    let mut policy = PairingPolicy::new(PendingState::One(existing));
    open(&mut policy, connection(1), 0);
    let wrong = policy
        .abort_pending(time(2_100), connection(1), pending(4, 6))
        .err()
        .unwrap_or_else(|| panic!("wrong abort admitted"));
    assert_eq!(wrong.reason(), RequestRefusal::PendingMismatch);
    let abort = policy
        .abort_pending(time(2_101), connection(1), existing)
        .unwrap_or_else(|error| panic!("exact abort rejected: {error:?}"));
    policy
        .finish_abort(abort, AbortOutcome::TombstoneCommitted)
        .unwrap_or_else(|error| panic!("abort completion rejected: {error:?}"));
    assert_eq!(policy.pending(), None);
    match policy.begin(time(2_102), connection(1), begin_ready()) {
        AttemptDecision::Admitted { ordinal: 1, .. } => {}
        _ => panic!("abort spent the Begin/Proof budget"),
    }
}

#[test]
fn clock_regression_closes_and_requires_a_fresh_release_cycle() {
    let mut policy = PairingPolicy::new(PendingState::None);
    open(&mut policy, connection(1), 100);
    assert_eq!(
        policy.poll_timeout(time(50)),
        PolicyEvent::Fault(PolicyFault::ClockRegression)
    );
    assert!(matches!(
        policy.observe_button(time(2_200), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert!(policy.ordinary_session(time(2_201), connection(1)).is_ok());
}

#[test]
fn threshold_and_deadline_overflow_fail_without_opening() {
    let mut policy = PairingPolicy::new(PendingState::None);
    assert_eq!(
        policy.connected(time(u64::MAX - 1_000), connection(1)),
        Ok(None)
    );
    assert!(matches!(
        policy.observe_button(time(u64::MAX - 999), ActiveLowButton::High),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(u64::MAX - 998), ActiveLowButton::Low),
        ButtonEffect::None
    ));
    assert!(matches!(
        policy.observe_button(time(u64::MAX), ActiveLowButton::Low),
        ButtonEffect::Fault(PolicyFault::DeadlineOverflow)
    ));
}

#[test]
fn every_begin_proof_permutation_uses_one_shared_three_attempt_budget() {
    let existing = pending(9, 10);
    for proof_mask in 0_u8..16 {
        let mut policy = PairingPolicy::new(PendingState::One(existing));
        open(&mut policy, connection(1), 0);
        for event in 0_u8..4 {
            let expected_ordinal = event + 1;
            let is_proof = proof_mask & (1 << event) != 0;
            if event == 3 {
                let refused_without_spend = if is_proof {
                    matches!(
                        policy.proof(time(2_100 + u64::from(event)), connection(1), existing),
                        AttemptDecision::Refused {
                            reason: AttemptRefusal::WindowNotOpen,
                            ordinal: None,
                            ..
                        }
                    )
                } else {
                    matches!(
                        policy.begin(time(2_100 + u64::from(event)), connection(1), begin_ready(),),
                        AttemptDecision::Refused {
                            reason: AttemptRefusal::WindowNotOpen,
                            ordinal: None,
                            ..
                        }
                    )
                };
                assert!(refused_without_spend, "mask {proof_mask:04b}");
                continue;
            }
            if is_proof {
                let permit =
                    match policy.proof(time(2_100 + u64::from(event)), connection(1), existing) {
                        AttemptDecision::Admitted {
                            permit,
                            ordinal,
                            closed_to_new_attempts,
                        } => {
                            assert_eq!(ordinal, expected_ordinal, "mask {proof_mask:04b}");
                            assert_eq!(
                                closed_to_new_attempts.is_some(),
                                event == 2,
                                "mask {proof_mask:04b}"
                            );
                            permit
                        }
                        _ => panic!("exact proof was not admitted for mask {proof_mask:04b}"),
                    };
                policy
                    .proof_rejected(permit)
                    .unwrap_or_else(|error| panic!("proof finish failed: {error:?}"));
            } else {
                match policy.begin(time(2_100 + u64::from(event)), connection(1), begin_ready()) {
                    AttemptDecision::Refused {
                        reason: AttemptRefusal::PendingExists,
                        ordinal: Some(ordinal),
                        closed_to_new_attempts,
                    } => {
                        assert_eq!(ordinal, expected_ordinal, "mask {proof_mask:04b}");
                        assert_eq!(
                            closed_to_new_attempts.is_some(),
                            event == 2,
                            "mask {proof_mask:04b}"
                        );
                    }
                    _ => panic!("Begin refusal drifted for mask {proof_mask:04b}"),
                }
            }
        }
    }
}

#[test]
fn wrong_connection_requests_neither_spend_attempts_nor_close_the_window() {
    let existing = pending(3, 4);
    let mut policy = PairingPolicy::new(PendingState::One(existing));
    open(&mut policy, connection(10), 0);
    for wrong in [
        policy.begin(time(2_100), connection(9), begin_ready()),
        policy.begin(time(2_101), connection(11), begin_ready()),
    ] {
        assert!(matches!(
            wrong,
            AttemptDecision::Refused {
                reason: AttemptRefusal::WrongConnection,
                ordinal: None,
                closed_to_new_attempts: None,
            }
        ));
    }
    assert!(matches!(
        policy.proof(time(2_102), connection(11), existing),
        AttemptDecision::Refused {
            reason: AttemptRefusal::WrongConnection,
            ordinal: None,
            closed_to_new_attempts: None,
        }
    ));
    match policy.begin(time(2_103), connection(10), begin_ready()) {
        AttemptDecision::Refused {
            reason: AttemptRefusal::PendingExists,
            ordinal: Some(1),
            ..
        } => {}
        _ => panic!("wrong-connection traffic spent the bound window budget"),
    }
}

#[test]
fn timeout_closes_new_work_but_not_an_owned_begin_outcome() {
    let enrolled = pending(6, 7);
    let mut policy = PairingPolicy::new(PendingState::None);
    open(&mut policy, connection(1), 0);
    let permit = match policy.begin(time(2_100), connection(1), begin_ready()) {
        AttemptDecision::Admitted { permit, .. } => permit,
        _ => panic!("eligible Begin rejected"),
    };
    assert!(matches!(
        policy.poll_timeout(time(62_010)),
        PolicyEvent::Closed(closed) if closed.reason() == CloseReason::Timeout
    ));
    policy
        .finish_begin(permit, BeginOutcome::PendingCommitted(enrolled))
        .unwrap_or_else(|error| panic!("late durable outcome was lost: {error:?}"));
    assert_eq!(policy.pending(), Some(enrolled));
    assert!(policy.ordinary_session(time(62_011), connection(1)).is_ok());
}

#[test]
fn dropped_operation_owner_leaves_policy_fail_closed() {
    let mut policy = PairingPolicy::new(PendingState::None);
    open(&mut policy, connection(1), 0);
    let permit = match policy.begin(time(2_100), connection(1), begin_ready()) {
        AttemptDecision::Admitted { permit, .. } => permit,
        _ => panic!("eligible Begin rejected"),
    };
    drop(permit);
    assert!(policy.operation_outstanding());
    assert!(matches!(
        policy.begin(time(2_101), connection(1), begin_ready()),
        AttemptDecision::Refused {
            reason: AttemptRefusal::OperationInFlight,
            ordinal: Some(2),
            ..
        }
    ));
    assert!(matches!(
        policy.ordinary_session(time(2_102), connection(1)),
        Err(OrdinarySessionRefusal::PairingExclusive)
    ));
}

#[test]
fn stale_exclusive_effect_cannot_open_a_replacement_connection() {
    let mut policy = PairingPolicy::new(PendingState::None);
    assert_eq!(policy.connected(time(0), connection(1)), Ok(None));
    policy.observe_button(time(0), ActiveLowButton::High);
    policy.observe_button(time(1), ActiveLowButton::Low);
    let effect = match policy.observe_button(time(2_001), ActiveLowButton::Low) {
        ButtonEffect::AcquirePairingExclusive(effect) => effect,
        _ => panic!("threshold did not emit exclusivity"),
    };
    assert!(matches!(
        policy.disconnected(time(2_002), connection(1)),
        PolicyEvent::Closed(closed) if closed.reason() == CloseReason::Disconnect
    ));
    assert_eq!(policy.connected(time(2_003), connection(2)), Ok(None));
    assert!(matches!(
        policy.exclusive_acquired(time(2_004), effect),
        ExclusiveAcquireOutcome::Stale
    ));
}
