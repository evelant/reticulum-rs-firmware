use reticulum_storage_model::{
    AcceptOutcome, AcceptanceCandidate, Accepted, ApplyError, ApplyOutcome, AuditEntry, AuditEvent,
    BootRecoveryDecision, BootRecoveryMarker, ContentSha256, DestinationHash, EncodedPacketSha256,
    ExperimentalRnsDataIntent, FinalDisposition, IdempotencyKey, InternalFailure, InterruptedState,
    JournalEntry, LifecycleState, MAX_DURABLE_RECORDS_PER_SUBMISSION, MAX_ENCODED_PACKET_BYTES,
    MAX_EXPERIMENTAL_RNS_DATA_BYTES, MAX_JOURNAL_RECORD_BYTES,
    MAX_STATE_TRANSITIONS_PER_SUBMISSION, MAX_TRANSPORT_AUDITS_PER_SUBMISSION, PlanOutcome,
    PreparedPacketDetails, PrincipalId, RnsAttemptToken, StateTransition, SubmissionFailure,
    SubmissionId, SubmissionIndex, SubmissionReplay, TransitionError, TransportRecoveryReason,
    decode_journal_entry, encode_journal_entry, validate_transition,
};

fn principal(tag: u8) -> PrincipalId {
    PrincipalId::new([tag; 16])
}

fn key(tag: u8) -> IdempotencyKey {
    IdempotencyKey::new([tag; 16])
}

fn intent(destination: u8, payload: &[u8]) -> ExperimentalRnsDataIntent {
    ExperimentalRnsDataIntent::new(DestinationHash::new([destination; 16]), payload).unwrap()
}

fn details(tag: u8) -> PreparedPacketDetails {
    PreparedPacketDetails::new(
        97,
        EncodedPacketSha256::new([tag; 32]),
        RnsAttemptToken::new([tag.wrapping_add(1); 32]),
    )
    .unwrap()
}

fn live<const N: usize>(first_id: SubmissionId) -> SubmissionIndex<N> {
    SubmissionReplay::new(first_id).complete().unwrap()
}

fn accepted<const N: usize>(
    index: &mut SubmissionIndex<N>,
    principal_tag: u8,
    key_tag: u8,
    payload: &[u8],
) -> Accepted {
    let candidate = AcceptanceCandidate::new(
        principal(principal_tag),
        key(key_tag),
        intent(0x33, payload),
    );
    match index.plan_accept(candidate) {
        AcceptOutcome::Accepted(planned) => {
            let JournalEntry::Accepted(accepted) = planned.entry() else {
                panic!("acceptance planner returned a non-acceptance record")
            };
            assert_eq!(index.apply_planned(planned), Ok(ApplyOutcome::Applied));
            accepted
        }
        other => panic!("unexpected acceptance result: {other:?}"),
    }
}

fn transition(id: SubmissionId, revision: u64, state: LifecycleState) -> StateTransition {
    StateTransition::new(id, revision, state).unwrap()
}

fn append_planned_transition<const N: usize>(
    index: &mut SubmissionIndex<N>,
    transition: StateTransition,
) -> ApplyOutcome {
    let PlanOutcome::Append(planned) = index.plan_transition(transition).unwrap() else {
        panic!("transition unexpectedly already applied")
    };
    assert_eq!(planned.entry(), JournalEntry::StateTransition(transition));
    index.apply_planned(planned).unwrap()
}

fn append_planned_audit<const N: usize>(
    index: &mut SubmissionIndex<N>,
    audit: AuditEntry,
) -> ApplyOutcome {
    let PlanOutcome::Append(planned) = index.plan_audit(audit).unwrap() else {
        panic!("audit unexpectedly already applied")
    };
    assert_eq!(planned.entry(), JournalEntry::Audit(audit));
    index.apply_planned(planned).unwrap()
}

fn encoded(entry: JournalEntry) -> Vec<u8> {
    let mut bytes = [0_u8; MAX_JOURNAL_RECORD_BYTES];
    let len = encode_journal_entry(&entry, &mut bytes).unwrap();
    bytes[..len].to_vec()
}

#[test]
fn maximum_intent_is_owned_hashed_and_fits_one_record() {
    let payload = [0x5a; MAX_EXPERIMENTAL_RNS_DATA_BYTES];
    let intent = intent(0x44, &payload);
    assert_eq!(intent.payload(), payload);
    assert_eq!(intent.content_sha256(), intent.content_sha256());

    let entry = JournalEntry::Accepted(Accepted::new(
        SubmissionId::new(u64::MAX),
        principal(0xff),
        key(0xee),
        intent,
    ));
    let bytes = encoded(entry);
    assert_eq!(bytes.len(), 495);
    assert!(bytes.len() <= MAX_JOURNAL_RECORD_BYTES);
    assert_eq!(decode_journal_entry(&bytes), Ok(entry));

    let mut short = [0_u8; 494];
    assert_eq!(
        encode_journal_entry(&entry, &mut short),
        Err(reticulum_storage_model::EncodeError::OutputTooSmall)
    );
    let oversized_record = [0_u8; MAX_JOURNAL_RECORD_BYTES + 1];
    assert_eq!(
        decode_journal_entry(&oversized_record),
        Err(reticulum_storage_model::DecodeError::RecordTooLarge)
    );

    let oversized = [0_u8; MAX_EXPERIMENTAL_RNS_DATA_BYTES + 1];
    let error =
        ExperimentalRnsDataIntent::new(DestinationHash::new([0; 16]), &oversized).unwrap_err();
    assert_eq!(error.actual(), MAX_EXPERIMENTAL_RNS_DATA_BYTES + 1);
    assert_eq!(error.maximum(), MAX_EXPERIMENTAL_RNS_DATA_BYTES);
}

#[test]
fn accepted_record_golden_and_every_truncated_prefix_are_strict() {
    let entry = JournalEntry::Accepted(Accepted::new(
        SubmissionId::new(7),
        principal(0x11),
        key(0x22),
        intent(0x33, b"abc"),
    ));
    let actual = encoded(entry);

    // Filled from this crate's canonical encoder and frozen against accidental
    // field-number, digest-domain, or integer-encoding drift.
    const GOLDEN: &[u8] = &[
        0xa3, 0x00, 0x01, 0x01, 0x00, 0x02, 0xa7, 0x00, 0x07, 0x01, 0x50, // Principal.
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        0x11, // Idempotency key.
        0x02, 0x50, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
        0x22, 0x22, 0x22, // Semantic content digest.
        0x03, 0x58, 0x20, 0x72, 0xc9, 0x6b, 0x00, 0x19, 0x4b, 0xe6, 0x46, 0x8d, 0xcd, 0x62, 0x2e,
        0xb8, 0x7b, 0xef, 0x0c, 0x8a, 0x4a, 0xec, 0x80, 0x09, 0xed, 0xe9, 0x3a, 0x1c, 0x24, 0xed,
        0x5c, 0xe9, 0x1a, 0x90, 0x61, // Intent kind, destination, and payload.
        0x04, 0x00, 0x05, 0x50, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
        0x33, 0x33, 0x33, 0x33, 0x33, 0x06, 0x43, 0x61, 0x62, 0x63,
    ];
    assert_eq!(actual, GOLDEN);
    assert_eq!(decode_journal_entry(GOLDEN), Ok(entry));
    for end in 0..GOLDEN.len() {
        assert!(
            decode_journal_entry(&GOLDEN[..end]).is_err(),
            "prefix {end}"
        );
    }
    let mut trailing = GOLDEN.to_vec();
    trailing.push(0);
    assert!(decode_journal_entry(&trailing).is_err());
}

#[test]
fn transition_and_audit_goldens_round_trip() {
    let state = transition(
        SubmissionId::new(7),
        3,
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
            InternalFailure::InterruptedByReset(reticulum_storage_model::BootRecoveryMarker::new(
                9,
                InterruptedState::AwaitingDelivery,
            )),
        ))),
    );
    let audit = AuditEntry::new(
        SubmissionId::new(7),
        2,
        AuditEvent::TransportRecovered {
            rns_attempt_token: RnsAttemptToken::new([0x55; 32]),
            may_have_transmitted: true,
            reason: TransportRecoveryReason::CompletionFault(0x1234),
        },
    )
    .unwrap();
    const TRANSITION_GOLDEN: &[u8] = &[
        0xa3, 0x00, 0x01, 0x01, 0x01, 0x02, 0xa8, 0x00, 0x07, 0x01, 0x03, 0x02, 0x04, 0x05, 0x03,
        0x06, 0x01, 0x07, 0x09, 0x08, 0x01, 0x09, 0x00,
    ];
    const AUDIT_GOLDEN: &[u8] = &[
        0xa3, 0x00, 0x01, 0x01, 0x02, 0x02, 0xa7, 0x00, 0x07, 0x01, 0x02, 0x02, 0x01, 0x03, 0x58,
        0x20, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
        0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
        0x55, 0x55, 0x55, 0x04, 0xf5, 0x05, 0x01, 0x06, 0x19, 0x12, 0x34,
    ];

    for (entry, golden) in [
        (JournalEntry::StateTransition(state), TRANSITION_GOLDEN),
        (JournalEntry::Audit(audit), AUDIT_GOLDEN),
    ] {
        assert_eq!(encoded(entry), golden);
        assert_eq!(decode_journal_entry(golden), Ok(entry));
    }
}

#[test]
fn transport_audit_codec_retains_every_reason_without_completion_code_collisions() {
    let token = RnsAttemptToken::new([0xa5; 32]);
    for reason in [
        TransportRecoveryReason::DeadlineExpired,
        TransportRecoveryReason::CompletionFault(0),
        TransportRecoveryReason::CompletionFault(0xfff1),
        TransportRecoveryReason::CompletionFault(0xfff2),
        TransportRecoveryReason::CompletionFault(0xfff3),
        TransportRecoveryReason::CompletionFault(0xffff),
        TransportRecoveryReason::ReceiptCancellationFailed,
        TransportRecoveryReason::HopIdentifierExhausted,
        TransportRecoveryReason::Invariant,
    ] {
        for event in [
            AuditEvent::TransportRecovered {
                rns_attempt_token: token,
                may_have_transmitted: true,
                reason,
            },
            AuditEvent::TransportQuarantined {
                rns_attempt_token: token,
                may_have_transmitted: false,
                reason,
            },
        ] {
            let entry =
                JournalEntry::Audit(AuditEntry::new(SubmissionId::new(3), 4, event).unwrap());
            let bytes = encoded(entry);
            assert_eq!(decode_journal_entry(&bytes), Ok(entry));
        }
    }
}

#[test]
fn noncanonical_and_corrupt_content_digest_are_rejected() {
    let entry = JournalEntry::Accepted(Accepted::new(
        SubmissionId::new(1),
        principal(1),
        key(2),
        intent(3, b"x"),
    ));
    let canonical = encoded(entry);

    // Schema 1 encoded with an unnecessarily wide uint8 representation.
    let mut noncanonical = canonical.clone();
    assert_eq!(&noncanonical[..3], &[0xa3, 0x00, 0x01]);
    noncanonical.splice(2..3, [0x18, 0x01]);
    assert_eq!(
        decode_journal_entry(&noncanonical),
        Err(reticulum_storage_model::DecodeError::NonCanonical)
    );

    let mut corrupt = canonical;
    let digest_header = corrupt
        .windows(2)
        .position(|window| window == [0x58, 0x20])
        .unwrap();
    corrupt[digest_header + 2] ^= 1;
    assert_eq!(
        decode_journal_entry(&corrupt),
        Err(reticulum_storage_model::DecodeError::ContentDigestMismatch)
    );
}

#[test]
fn idempotency_is_principal_scoped_and_content_checked() {
    let mut index = live::<3>(SubmissionId::new(10));
    let first = accepted(&mut index, 1, 7, b"same");
    assert_eq!(first.id(), SubmissionId::new(10));
    assert_eq!(index.next_id(), Some(SubmissionId::new(11)));

    assert_eq!(
        index.plan_accept(AcceptanceCandidate::new(
            principal(1),
            key(7),
            intent(0x33, b"same")
        )),
        AcceptOutcome::Replay(first.id())
    );
    assert_eq!(
        index.plan_accept(AcceptanceCandidate::new(
            principal(1),
            key(7),
            intent(0x33, b"different")
        )),
        AcceptOutcome::IdempotencyConflict {
            existing: first.id()
        }
    );

    let second_candidate = AcceptanceCandidate::new(principal(2), key(7), intent(0x33, b"same"));
    let second = match index.plan_accept(second_candidate) {
        AcceptOutcome::Accepted(planned) => {
            let JournalEntry::Accepted(accepted) = planned.entry() else {
                panic!("unexpected planned record")
            };
            index.apply_planned(planned).unwrap();
            accepted
        }
        other => panic!("second principal was not independent: {other:?}"),
    };
    assert_eq!(second.id(), SubmissionId::new(11));
}

#[test]
fn lost_accept_reply_replays_the_committed_identifier_without_allocating_again() {
    let mut index = live::<2>(SubmissionId::new(70));
    let candidate = AcceptanceCandidate::new(principal(1), key(2), intent(3, b"durable"));

    let AcceptOutcome::Accepted(planned) = index.plan_accept(candidate) else {
        panic!("candidate should be plannable")
    };
    let JournalEntry::Accepted(accepted) = planned.entry() else {
        panic!("unexpected planned record")
    };
    assert_eq!(accepted.id(), SubmissionId::new(70));
    assert_eq!(index.next_id(), Some(SubmissionId::new(70)));

    // Model the storage actor appending bytes durably and then losing its apply
    // reply. The retained opaque plan remains safe to apply idempotently.
    let durable = encoded(planned.entry());
    let replayed = decode_journal_entry(&durable).unwrap();
    assert_eq!(replayed, planned.entry());
    assert_eq!(index.apply_planned(planned), Ok(ApplyOutcome::Applied));
    assert_eq!(index.next_id(), Some(SubmissionId::new(71)));

    assert_eq!(
        index.plan_accept(candidate),
        AcceptOutcome::Replay(accepted.id())
    );
    assert_eq!(
        index.apply_planned(planned),
        Ok(ApplyOutcome::AlreadyEquivalent)
    );
}

#[test]
fn submission_lookup_is_principal_owned() {
    let mut index = live::<1>(SubmissionId::new(1));
    let accepted = accepted(&mut index, 1, 1, b"private");

    assert_eq!(
        index
            .get_owned(principal(1), accepted.id())
            .unwrap()
            .accepted(),
        accepted
    );
    assert_eq!(index.get_owned(principal(2), accepted.id()), None);
}

#[test]
fn fixed_index_exhaustion_consumes_no_id_and_accepted_lifecycle_has_no_model_quota_dead_end() {
    let no_slots = live::<0>(SubmissionId::new(41));
    assert_eq!(
        no_slots.plan_accept(AcceptanceCandidate::new(
            principal(1),
            key(1),
            intent(1, b"x")
        )),
        AcceptOutcome::IndexExhausted
    );
    assert_eq!(no_slots.next_id(), Some(SubmissionId::new(41)));

    let mut index = live::<1>(SubmissionId::new(1));
    let accepted = accepted(&mut index, 1, 1, b"x");
    let packet = details(1);
    assert_eq!(
        append_planned_transition(
            &mut index,
            transition(accepted.id(), 1, LifecycleState::Preparing)
        ),
        ApplyOutcome::Applied
    );
    assert_eq!(
        append_planned_transition(
            &mut index,
            transition(accepted.id(), 2, LifecycleState::AwaitingDelivery(packet))
        ),
        ApplyOutcome::Applied
    );
    assert_eq!(
        append_planned_transition(
            &mut index,
            transition(
                accepted.id(),
                3,
                LifecycleState::Final(FinalDisposition::Delivered(packet))
            )
        ),
        ApplyOutcome::Applied
    );
    assert_eq!(
        index.plan_accept(AcceptanceCandidate::new(
            principal(2),
            key(2),
            intent(2, b"full")
        )),
        AcceptOutcome::IndexExhausted
    );
}

#[test]
fn transition_plans_are_nonmutating_complete_idempotent_and_conflict_detecting() {
    let mut index = live::<1>(SubmissionId::new(1));
    let accepted = accepted(&mut index, 1, 1, b"x");
    let preparing = transition(accepted.id(), 1, LifecycleState::Preparing);
    let PlanOutcome::Append(planned) = index.plan_transition(preparing).unwrap() else {
        panic!("new transition must require append")
    };
    assert_eq!(index.get(accepted.id()).unwrap().revision(), 0);
    assert_eq!(
        index.get(accepted.id()).unwrap().state(),
        LifecycleState::Queued
    );
    assert_eq!(index.apply_planned(planned), Ok(ApplyOutcome::Applied));
    assert_eq!(
        index.plan_transition(preparing),
        Ok(PlanOutcome::AlreadyEquivalent)
    );
    // A lost apply reply can safely consume the retained exact plan again.
    assert_eq!(
        index.apply_planned(planned),
        Ok(ApplyOutcome::AlreadyEquivalent)
    );

    let conflicting = transition(
        accepted.id(),
        1,
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Rejected)),
    );
    assert_eq!(
        index.plan_transition(conflicting),
        Err(ApplyError::RevisionConflict)
    );
    assert_eq!(
        index.get(accepted.id()).unwrap().state(),
        LifecycleState::Preparing
    );
    let gap = transition(
        accepted.id(),
        3,
        LifecycleState::AwaitingDelivery(details(1)),
    );
    assert_eq!(
        index.plan_transition(gap),
        Err(ApplyError::RevisionGap {
            expected: 2,
            actual: 3
        })
    );
    assert_eq!(index.get(accepted.id()).unwrap().revision(), 1);
    let delivered = transition(
        accepted.id(),
        2,
        LifecycleState::Final(FinalDisposition::Delivered(details(1))),
    );
    let PlanOutcome::Append(delivered_plan) = index.plan_transition(delivered).unwrap() else {
        panic!("proof-before-frame delivery must require append")
    };
    assert_eq!(index.get(accepted.id()).unwrap().revision(), 1);
    assert_eq!(
        index.apply_planned(delivered_plan),
        Ok(ApplyOutcome::Applied)
    );
    assert_eq!(index.get(accepted.id()).unwrap().state(), delivered.state());
    assert!(index.get(accepted.id()).unwrap().may_have_transmitted());
}

#[test]
fn audit_plans_preflight_state_revision_token_and_transmission_uncertainty() {
    let mut index = live::<1>(SubmissionId::new(1));
    let submission = accepted(&mut index, 1, 1, b"x");
    let packet = details(8);
    let token = packet.rns_attempt_token();
    let queued_audit = AuditEntry::new(
        submission.id(),
        1,
        AuditEvent::TransportRecovered {
            rns_attempt_token: token,
            may_have_transmitted: false,
            reason: TransportRecoveryReason::CompletionFault(1),
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(queued_audit),
        Err(ApplyError::AuditStateMismatch)
    );
    assert_eq!(index.get(submission.id()).unwrap().revision(), 0);

    append_planned_transition(
        &mut index,
        transition(submission.id(), 1, LifecycleState::Preparing),
    );
    let gap = AuditEntry::new(
        submission.id(),
        3,
        AuditEvent::TransportRecovered {
            rns_attempt_token: token,
            may_have_transmitted: false,
            reason: TransportRecoveryReason::CompletionFault(2),
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(gap),
        Err(ApplyError::RevisionGap {
            expected: 2,
            actual: 3,
        })
    );

    let first_audit = AuditEntry::new(
        submission.id(),
        2,
        AuditEvent::TransportRecovered {
            rns_attempt_token: token,
            may_have_transmitted: false,
            reason: TransportRecoveryReason::CompletionFault(3),
        },
    )
    .unwrap();
    let PlanOutcome::Append(first_plan) = index.plan_audit(first_audit).unwrap() else {
        panic!("new audit must require append")
    };
    assert_eq!(index.get(submission.id()).unwrap().revision(), 1);
    assert_eq!(
        index.get(submission.id()).unwrap().rns_attempt_token(),
        None
    );
    assert_eq!(index.apply_planned(first_plan), Ok(ApplyOutcome::Applied));
    assert_eq!(
        index.plan_audit(first_audit),
        Ok(PlanOutcome::AlreadyEquivalent)
    );
    assert_eq!(
        index.apply_planned(first_plan),
        Ok(ApplyOutcome::AlreadyEquivalent)
    );
    assert_eq!(
        index.get(submission.id()).unwrap().rns_attempt_token(),
        Some(token)
    );

    let same_revision_reason_collision = AuditEntry::new(
        submission.id(),
        2,
        AuditEvent::TransportRecovered {
            rns_attempt_token: token,
            may_have_transmitted: false,
            reason: TransportRecoveryReason::HopIdentifierExhausted,
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(same_revision_reason_collision),
        Err(ApplyError::RevisionConflict)
    );

    let same_revision_conflict = AuditEntry::new(
        submission.id(),
        2,
        AuditEvent::TransportQuarantined {
            rns_attempt_token: token,
            may_have_transmitted: false,
            reason: TransportRecoveryReason::CompletionFault(4),
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(same_revision_conflict),
        Err(ApplyError::RevisionConflict)
    );

    let substituted_packet = details(9);
    assert_ne!(substituted_packet.rns_attempt_token(), token);
    assert_eq!(
        index.plan_transition(transition(
            submission.id(),
            3,
            LifecycleState::AwaitingDelivery(substituted_packet)
        )),
        Err(ApplyError::AttemptTokenMismatch)
    );
    append_planned_transition(
        &mut index,
        transition(submission.id(), 3, LifecycleState::AwaitingDelivery(packet)),
    );

    let wrong_token = AuditEntry::new(
        submission.id(),
        4,
        AuditEvent::TransportQuarantined {
            rns_attempt_token: RnsAttemptToken::new([0xaa; 32]),
            may_have_transmitted: true,
            reason: TransportRecoveryReason::CompletionFault(5),
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(wrong_token),
        Err(ApplyError::AuditCardinalityExceeded)
    );

    let uncertain = AuditEntry::new(
        submission.id(),
        4,
        AuditEvent::TransportQuarantined {
            rns_attempt_token: token,
            may_have_transmitted: true,
            reason: TransportRecoveryReason::CompletionFault(6),
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(uncertain),
        Err(ApplyError::AuditCardinalityExceeded)
    );
    assert!(index.get(submission.id()).unwrap().may_have_transmitted());

    let downgrade = AuditEntry::new(
        submission.id(),
        4,
        AuditEvent::TransportRecovered {
            rns_attempt_token: token,
            may_have_transmitted: false,
            reason: TransportRecoveryReason::CompletionFault(7),
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(downgrade),
        Err(ApplyError::AuditCardinalityExceeded)
    );
    assert_eq!(index.get(submission.id()).unwrap().revision(), 3);

    let mut never_prepared = live::<1>(SubmissionId::new(20));
    let direct = accepted(&mut never_prepared, 1, 1, b"direct");
    append_planned_transition(
        &mut never_prepared,
        transition(
            direct.id(),
            1,
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Rejected)),
        ),
    );
    let fabricated = AuditEntry::new(
        direct.id(),
        2,
        AuditEvent::TransportRecovered {
            rns_attempt_token: token,
            may_have_transmitted: true,
            reason: TransportRecoveryReason::CompletionFault(8),
        },
    )
    .unwrap();
    assert_eq!(
        never_prepared.plan_audit(fabricated),
        Err(ApplyError::AuditStateMismatch)
    );
}

#[test]
fn first_transport_audit_checks_token_and_preserves_accumulated_uncertainty() {
    let mut index = live::<1>(SubmissionId::new(1));
    let submission = accepted(&mut index, 1, 1, b"first-audit-validation");
    let packet = details(10);
    append_planned_transition(
        &mut index,
        transition(submission.id(), 1, LifecycleState::Preparing),
    );
    append_planned_transition(
        &mut index,
        transition(submission.id(), 2, LifecycleState::AwaitingDelivery(packet)),
    );

    let wrong_token = AuditEntry::new(
        submission.id(),
        3,
        AuditEvent::TransportQuarantined {
            rns_attempt_token: RnsAttemptToken::new([0xaa; 32]),
            may_have_transmitted: true,
            reason: TransportRecoveryReason::Invariant,
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(wrong_token),
        Err(ApplyError::AttemptTokenMismatch)
    );

    let exact_false_observation = AuditEntry::new(
        submission.id(),
        3,
        AuditEvent::TransportQuarantined {
            rns_attempt_token: packet.rns_attempt_token(),
            may_have_transmitted: false,
            reason: TransportRecoveryReason::Invariant,
        },
    )
    .unwrap();
    assert_eq!(
        append_planned_audit(&mut index, exact_false_observation),
        ApplyOutcome::Applied
    );
    assert_eq!(index.get(submission.id()).unwrap().revision(), 3);
    assert_eq!(
        index.get(submission.id()).unwrap().transport_audit(),
        Some(exact_false_observation.event())
    );
    assert!(index.get(submission.id()).unwrap().may_have_transmitted());
}

#[test]
fn schema_one_has_an_explicit_one_transport_audit_bound() {
    assert_eq!(MAX_STATE_TRANSITIONS_PER_SUBMISSION, 3);
    assert_eq!(MAX_TRANSPORT_AUDITS_PER_SUBMISSION, 1);
    assert_eq!(MAX_DURABLE_RECORDS_PER_SUBMISSION, 5);
}

#[test]
fn transport_audit_after_final_preserves_the_final_disposition() {
    let mut index = live::<1>(SubmissionId::new(1));
    let accepted = accepted(&mut index, 1, 1, b"x");
    let packet = details(4);
    append_planned_transition(
        &mut index,
        transition(accepted.id(), 1, LifecycleState::Preparing),
    );
    append_planned_transition(
        &mut index,
        transition(accepted.id(), 2, LifecycleState::AwaitingDelivery(packet)),
    );
    let final_state =
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout));
    append_planned_transition(&mut index, transition(accepted.id(), 3, final_state));

    let recovered = AuditEntry::new(
        accepted.id(),
        4,
        AuditEvent::TransportRecovered {
            rns_attempt_token: packet.rns_attempt_token(),
            may_have_transmitted: true,
            reason: TransportRecoveryReason::CompletionFault(0x1234),
        },
    )
    .unwrap();
    let PlanOutcome::Append(planned) = index.plan_audit(recovered).unwrap() else {
        panic!("new audit must require append")
    };
    assert_eq!(index.get(accepted.id()).unwrap().revision(), 3);
    assert!(index.get(accepted.id()).unwrap().may_have_transmitted());
    assert_eq!(index.apply_planned(planned), Ok(ApplyOutcome::Applied));
    assert_eq!(index.get(accepted.id()).unwrap().state(), final_state);
    assert_eq!(index.get(accepted.id()).unwrap().revision(), 4);
    assert_eq!(
        index.get(accepted.id()).unwrap().prepared_details(),
        Some(packet)
    );
    assert!(index.get(accepted.id()).unwrap().may_have_transmitted());
    assert_eq!(
        index.plan_audit(recovered),
        Ok(PlanOutcome::AlreadyEquivalent)
    );
    assert_eq!(
        index.apply_planned(planned),
        Ok(ApplyOutcome::AlreadyEquivalent)
    );

    let downgrade = AuditEntry::new(
        accepted.id(),
        5,
        AuditEvent::TransportQuarantined {
            rns_attempt_token: packet.rns_attempt_token(),
            may_have_transmitted: false,
            reason: TransportRecoveryReason::CompletionFault(7),
        },
    )
    .unwrap();
    assert_eq!(
        index.plan_audit(downgrade),
        Err(ApplyError::AuditCardinalityExceeded)
    );
    assert_eq!(index.get(accepted.id()).unwrap().revision(), 4);
}

#[test]
fn lifecycle_rejects_shortcuts_mutation_after_final_and_digest_substitution() {
    let packet = details(1);
    let other_digest = PreparedPacketDetails::new(
        packet.packet_len(),
        EncodedPacketSha256::new([9; 32]),
        packet.rns_attempt_token(),
    )
    .unwrap();
    let other_token = PreparedPacketDetails::new(
        packet.packet_len(),
        packet.encoded_packet_sha256(),
        RnsAttemptToken::new([9; 32]),
    )
    .unwrap();

    assert_eq!(
        validate_transition(
            LifecycleState::Queued,
            LifecycleState::AwaitingDelivery(packet)
        ),
        Err(TransitionError::IllegalLifecycle)
    );
    assert_eq!(
        validate_transition(
            LifecycleState::AwaitingDelivery(packet),
            LifecycleState::Final(FinalDisposition::Delivered(other_digest))
        ),
        Err(TransitionError::PreparedMetadataMismatch)
    );
    assert_eq!(
        validate_transition(
            LifecycleState::AwaitingDelivery(packet),
            LifecycleState::Final(FinalDisposition::Delivered(other_token))
        ),
        Err(TransitionError::PreparedMetadataMismatch)
    );
    assert_eq!(
        validate_transition(
            LifecycleState::Final(FinalDisposition::Cancelled),
            LifecycleState::Preparing
        ),
        Err(TransitionError::IllegalLifecycle)
    );
    let reset_from_preparing = FinalDisposition::Failed(SubmissionFailure::Internal(
        InternalFailure::InterruptedByReset(BootRecoveryMarker::new(
            7,
            InterruptedState::Preparing,
        )),
    ));
    let reset_from_awaiting = FinalDisposition::Failed(SubmissionFailure::Internal(
        InternalFailure::InterruptedByReset(BootRecoveryMarker::new(
            7,
            InterruptedState::AwaitingDelivery,
        )),
    ));
    for invalid in [
        (
            LifecycleState::Queued,
            LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::DeliveryTimeout)),
        ),
        (
            LifecycleState::Queued,
            LifecycleState::Final(reset_from_preparing),
        ),
        (
            LifecycleState::Preparing,
            LifecycleState::Final(reset_from_awaiting),
        ),
        (
            LifecycleState::AwaitingDelivery(packet),
            LifecycleState::Final(reset_from_preparing),
        ),
    ] {
        assert_eq!(
            validate_transition(invalid.0, invalid.1),
            Err(TransitionError::IllegalLifecycle)
        );
    }

    let _: EncodedPacketSha256 = packet.encoded_packet_sha256();
    let _: RnsAttemptToken = packet.rns_attempt_token();
}

#[test]
fn prepared_packet_length_is_checked_against_the_current_buffer_contract() {
    for invalid in [0, MAX_ENCODED_PACKET_BYTES as u16 + 1, u16::MAX] {
        let error = PreparedPacketDetails::new(
            invalid,
            EncodedPacketSha256::new([1; 32]),
            RnsAttemptToken::new([2; 32]),
        )
        .unwrap_err();
        assert_eq!(error.actual(), invalid);
        assert_eq!(error.maximum(), MAX_ENCODED_PACKET_BYTES as u16);
    }
    assert!(
        PreparedPacketDetails::new(
            MAX_ENCODED_PACKET_BYTES as u16,
            EncodedPacketSha256::new([1; 32]),
            RnsAttemptToken::new([2; 32]),
        )
        .is_ok()
    );
}

#[test]
fn boot_policy_replays_only_queued_and_marks_interrupted_states() {
    let mut queued = live::<1>(SubmissionId::new(1));
    let queued_id = accepted(&mut queued, 1, 1, b"queued").id();
    assert_eq!(
        queued.boot_recovery(queued_id, 7),
        Ok(BootRecoveryDecision::ReplayQueued)
    );

    let mut preparing = live::<1>(SubmissionId::new(2));
    let preparing_id = accepted(&mut preparing, 1, 1, b"preparing").id();
    append_planned_transition(
        &mut preparing,
        transition(preparing_id, 1, LifecycleState::Preparing),
    );
    let BootRecoveryDecision::FinalizeInterrupted(preparing_final) =
        preparing.boot_recovery(preparing_id, 76).unwrap()
    else {
        panic!("preparing state must not be replayed")
    };
    assert!(matches!(
        preparing_final.state(),
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
            InternalFailure::InterruptedByReset(marker)
        ))) if marker.interrupted_state() == InterruptedState::Preparing
    ));
    append_planned_transition(&mut preparing, preparing_final);
    assert!(preparing.get(preparing_id).unwrap().may_have_transmitted());

    let mut awaiting = live::<1>(SubmissionId::new(4));
    let id = accepted(&mut awaiting, 1, 1, b"awaiting").id();
    append_planned_transition(&mut awaiting, transition(id, 1, LifecycleState::Preparing));
    append_planned_transition(
        &mut awaiting,
        transition(id, 2, LifecycleState::AwaitingDelivery(details(3))),
    );
    let BootRecoveryDecision::FinalizeInterrupted(finalize) =
        awaiting.boot_recovery(id, 77).unwrap()
    else {
        panic!("awaiting state must not be replayed")
    };
    let LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
        InternalFailure::InterruptedByReset(marker),
    ))) = finalize.state()
    else {
        panic!("unexpected boot disposition")
    };
    assert_eq!(marker.boot_sequence(), 77);
    assert_eq!(
        marker.interrupted_state(),
        InterruptedState::AwaitingDelivery
    );
    append_planned_transition(&mut awaiting, finalize);
    assert_eq!(
        awaiting.boot_recovery(id, 78),
        Ok(BootRecoveryDecision::AlreadyFinal)
    );
}

#[test]
fn replay_must_be_explicitly_completed_before_live_boot_and_planning() {
    let accepted = Accepted::new(
        SubmissionId::new(9),
        principal(1),
        key(1),
        intent(1, b"persisted"),
    );
    let preparing = transition(accepted.id(), 1, LifecycleState::Preparing);
    let final_state = transition(
        accepted.id(),
        2,
        LifecycleState::Final(FinalDisposition::Failed(SubmissionFailure::Internal(
            InternalFailure::Unspecified,
        ))),
    );

    let mut replay = SubmissionReplay::<2>::new(SubmissionId::new(1));
    for entry in [
        JournalEntry::Accepted(accepted),
        JournalEntry::StateTransition(preparing),
        JournalEntry::StateTransition(final_state),
    ] {
        assert_eq!(replay.apply_entry(entry), Ok(ApplyOutcome::Applied));
    }
    assert_eq!(
        replay.apply_entry(JournalEntry::StateTransition(final_state)),
        Ok(ApplyOutcome::AlreadyEquivalent)
    );
    let live = replay.complete().unwrap();
    assert_eq!(live.next_id(), Some(SubmissionId::new(10)));
    assert_eq!(
        live.boot_recovery(accepted.id(), 1),
        Ok(BootRecoveryDecision::AlreadyFinal)
    );

    // An empty backend still takes the explicit zero-record completion path.
    let empty = SubmissionReplay::<1>::new(SubmissionId::new(50))
        .complete()
        .unwrap();
    assert_eq!(empty.next_id(), Some(SubmissionId::new(50)));
}

#[test]
fn first_replay_error_poison_prevents_sealing_or_skipping_the_record() {
    let accepted = Accepted::new(
        SubmissionId::new(1),
        principal(1),
        key(1),
        intent(1, b"poison"),
    );
    let mut replay = SubmissionReplay::<1>::new(SubmissionId::new(1));
    assert_eq!(
        replay.apply_entry(JournalEntry::Accepted(accepted)),
        Ok(ApplyOutcome::Applied)
    );
    let gap = ApplyError::RevisionGap {
        expected: 1,
        actual: 2,
    };
    assert_eq!(
        replay.apply_entry(JournalEntry::StateTransition(transition(
            accepted.id(),
            2,
            LifecycleState::Preparing,
        ))),
        Err(gap)
    );
    // Even a record that would have been valid for the surviving prefix cannot
    // be applied after the rejected durable record.
    assert_eq!(
        replay.apply_entry(JournalEntry::StateTransition(transition(
            accepted.id(),
            1,
            LifecycleState::Preparing,
        ))),
        Err(gap)
    );
    assert!(matches!(replay.complete(), Err(error) if error == gap));
}

#[test]
fn replay_rejects_attempt_substitution_and_preserves_accumulated_uncertainty() {
    let accepted = Accepted::new(
        SubmissionId::new(1),
        principal(1),
        key(1),
        intent(1, b"replay"),
    );
    let packet = details(1);
    let substituted = details(2);
    let token = packet.rns_attempt_token();
    let mut replay = SubmissionReplay::<1>::new(SubmissionId::new(1));
    assert_eq!(
        replay.apply_entry(JournalEntry::Accepted(accepted)),
        Ok(ApplyOutcome::Applied)
    );
    assert_eq!(
        replay.apply_entry(JournalEntry::StateTransition(transition(
            accepted.id(),
            1,
            LifecycleState::Preparing,
        ))),
        Ok(ApplyOutcome::Applied)
    );
    assert_eq!(
        replay.apply_entry(JournalEntry::Audit(
            AuditEntry::new(
                accepted.id(),
                2,
                AuditEvent::TransportRecovered {
                    rns_attempt_token: token,
                    may_have_transmitted: true,
                    reason: TransportRecoveryReason::CompletionFault(1),
                },
            )
            .unwrap(),
        )),
        Ok(ApplyOutcome::Applied)
    );
    assert_eq!(
        replay.apply_entry(JournalEntry::StateTransition(transition(
            accepted.id(),
            3,
            LifecycleState::AwaitingDelivery(substituted),
        ))),
        Err(ApplyError::AttemptTokenMismatch)
    );
    // The rejected durable record poisons replay; a later matching revision
    // cannot make the prefix live.
    assert_eq!(
        replay.apply_entry(JournalEntry::StateTransition(transition(
            accepted.id(),
            3,
            LifecycleState::AwaitingDelivery(packet),
        ))),
        Err(ApplyError::AttemptTokenMismatch)
    );
    assert!(matches!(
        replay.complete(),
        Err(ApplyError::AttemptTokenMismatch)
    ));

    let mut uncertainty = SubmissionReplay::<1>::new(SubmissionId::new(1));
    assert_eq!(
        uncertainty.apply_entry(JournalEntry::Accepted(accepted)),
        Ok(ApplyOutcome::Applied)
    );
    assert_eq!(
        uncertainty.apply_entry(JournalEntry::StateTransition(transition(
            accepted.id(),
            1,
            LifecycleState::Preparing,
        ))),
        Ok(ApplyOutcome::Applied)
    );
    assert_eq!(
        uncertainty.apply_entry(JournalEntry::StateTransition(transition(
            accepted.id(),
            2,
            LifecycleState::AwaitingDelivery(packet),
        ))),
        Ok(ApplyOutcome::Applied)
    );
    assert_eq!(
        uncertainty.apply_entry(JournalEntry::Audit(
            AuditEntry::new(
                accepted.id(),
                3,
                AuditEvent::TransportQuarantined {
                    rns_attempt_token: token,
                    may_have_transmitted: false,
                    reason: TransportRecoveryReason::CompletionFault(2),
                },
            )
            .unwrap(),
        )),
        Ok(ApplyOutcome::Applied)
    );
    let uncertainty = uncertainty.complete().unwrap();
    assert!(
        uncertainty
            .get(accepted.id())
            .unwrap()
            .may_have_transmitted()
    );
}

#[test]
fn identifier_allocation_is_checked_at_u64_max() {
    let mut exhausted = live::<2>(SubmissionId::new(u64::MAX));
    assert!(matches!(
        exhausted.plan_accept(AcceptanceCandidate::new(
            principal(1),
            key(1),
            intent(1, b"last")
        )),
        AcceptOutcome::Accepted(_)
    ));
    let AcceptOutcome::Accepted(last) = exhausted.plan_accept(AcceptanceCandidate::new(
        principal(1),
        key(1),
        intent(1, b"last"),
    )) else {
        panic!("last ID must remain plannable until committed")
    };
    exhausted.apply_planned(last).unwrap();
    assert_eq!(exhausted.next_id(), None);
    assert_eq!(
        exhausted.plan_accept(AcceptanceCandidate::new(
            principal(2),
            key(2),
            intent(2, b"none")
        )),
        AcceptOutcome::IdentifierExhausted
    );
}

#[test]
fn content_digest_is_semantic_and_not_principal_or_key_dependent() {
    let semantic_intent = intent(4, b"semantic");
    let first = Accepted::new(SubmissionId::new(1), principal(1), key(1), semantic_intent);
    let second = Accepted::new(SubmissionId::new(2), principal(2), key(2), semantic_intent);
    let _: ContentSha256 = first.content_sha256();
    assert_eq!(first.content_sha256(), second.content_sha256());
    assert_ne!(
        first.content_sha256(),
        intent(5, b"semantic").content_sha256()
    );
    assert_ne!(
        first.content_sha256(),
        intent(4, b"semantic!").content_sha256()
    );
}
