use reticulum_device_api::{Permissions, PrincipalId};
use reticulum_device_api_credentials::{
    AuthorityRevision, AuthorizationPolicyVersion, CredentialAudit, CredentialAuthority,
    CredentialAuthorityBuilder, CredentialGeneration, CredentialId, CredentialRecord,
    CredentialStatus, CredentialSuccessorFaultKind, PairingOrigin, RevocationReason,
};

const PRINCIPAL_A: PrincipalId = PrincipalId([0x31; 16]);
const PRINCIPAL_B: PrincipalId = PrincipalId([0x42; 16]);
const PSK_A: [u8; 32] = [0xa1; 32];
const PSK_B: [u8; 32] = [0xb2; 32];
const PSK_C: [u8; 32] = [0xc3; 32];

fn id(byte: u8) -> CredentialId {
    CredentialId::new([byte; 16])
}

fn audit(created: u64, modified: u64, policy: u32) -> CredentialAudit {
    CredentialAudit::new(
        AuthorityRevision::new(created),
        AuthorityRevision::new(modified),
        PairingOrigin::LocalPhysicalPresence,
        AuthorizationPolicyVersion::new(policy),
    )
}

fn secret_record(
    id_byte: u8,
    generation: u64,
    principal: PrincipalId,
    permissions: Permissions,
    status: CredentialStatus,
    policy: u32,
    psk: [u8; 32],
) -> CredentialRecord {
    CredentialRecord::with_secret(
        id(id_byte),
        CredentialGeneration::new(generation),
        principal,
        permissions,
        status,
        audit(u64::from(id_byte), generation, policy),
        psk,
    )
}

fn active_a() -> CredentialRecord {
    secret_record(
        1,
        1,
        PRINCIPAL_A,
        Permissions::READ_SUBMISSION_STATUS,
        CredentialStatus::Active,
        1,
        PSK_A,
    )
}

fn pending_b() -> CredentialRecord {
    secret_record(
        2,
        2,
        PRINCIPAL_B,
        Permissions::SUBMIT_RNS_DATA,
        CredentialStatus::Pending,
        1,
        PSK_B,
    )
}

fn authority<const CAPACITY: usize>(
    revision: u64,
    records: impl IntoIterator<Item = CredentialRecord>,
) -> CredentialAuthority<CAPACITY> {
    let mut builder = match CredentialAuthorityBuilder::new(AuthorityRevision::new(revision)) {
        Ok(builder) => builder,
        Err(fault) => panic!("canonical revision rejected: {:?}", fault.kind()),
    };
    for record in records {
        builder = match builder.insert(record) {
            Ok(builder) => builder,
            Err(fault) => panic!("canonical record rejected: {:?}", fault.kind()),
        };
    }
    builder.finish()
}

fn publish_successor<const CAPACITY: usize>(
    current: &CredentialAuthority<CAPACITY>,
    candidate: CredentialAuthority<CAPACITY>,
) -> CredentialAuthority<CAPACITY> {
    let plan = match current.plan_successor(candidate) {
        Ok(plan) => plan,
        Err(fault) => panic!("valid successor rejected: {:?}", fault.kind()),
    };
    assert_eq!(
        plan.candidate().revision(),
        current
            .revision()
            .next()
            .unwrap_or_else(|_| panic!("test revision exhausted"))
    );
    plan.publish_after_commit()
}

fn assert_successor_fault<const CAPACITY: usize>(
    current: &CredentialAuthority<CAPACITY>,
    candidate: CredentialAuthority<CAPACITY>,
    expected: CredentialSuccessorFaultKind,
) -> CredentialAuthority<CAPACITY> {
    let fault = match current.plan_successor(candidate) {
        Ok(_) => panic!("invalid successor was accepted"),
        Err(fault) => fault,
    };
    assert_eq!(fault.kind(), expected);
    fault.into_candidate()
}

#[test]
fn one_new_record_is_a_valid_exact_next_successor() {
    let current = authority::<4>(2, [active_a(), pending_b()]);
    let candidate = authority::<4>(
        3,
        [
            pending_b(),
            active_a(),
            secret_record(
                3,
                3,
                PRINCIPAL_A,
                Permissions::NONE,
                CredentialStatus::Pending,
                1,
                PSK_C,
            ),
        ],
    );

    let published = publish_successor(&current, candidate);
    assert_eq!(published.revision(), AuthorityRevision::new(3));
    assert_eq!(published.record_count(), 3);
    assert_eq!(published.active_count(), 1);
    assert!(published.select_for_handshake(id(3)).is_err());
}

#[test]
fn canceled_commit_recovers_the_complete_unpublished_candidate() {
    let current = authority::<4>(1, [active_a()]);
    let candidate = authority::<4>(2, [active_a(), pending_b()]);
    let plan = current
        .plan_successor(candidate)
        .unwrap_or_else(|fault| panic!("valid successor rejected: {:?}", fault.kind()));

    let recovered = plan.into_unpublished_candidate();

    assert_eq!(current.revision(), AuthorityRevision::new(1));
    assert_eq!(recovered.revision(), AuthorityRevision::new(2));
    assert_eq!(recovered.record_count(), 2);
    assert!(recovered.select_for_handshake(id(1)).is_ok());
    assert!(recovered.select_for_handshake(id(2)).is_err());
}

#[test]
fn one_revocation_is_a_valid_exact_next_successor() {
    let current = authority::<4>(2, [active_a(), pending_b()]);
    let candidate = authority::<4>(
        3,
        [
            pending_b(),
            CredentialRecord::revoked(
                id(1),
                CredentialGeneration::new(3),
                PRINCIPAL_A,
                audit(1, 3, 1),
                RevocationReason::Explicit,
            ),
        ],
    );

    let published = publish_successor(&current, candidate);
    assert_eq!(published.revision(), AuthorityRevision::new(3));
    assert_eq!(published.record_count(), 2);
    assert_eq!(published.active_count(), 0);
    assert!(published.select_for_handshake(id(1)).is_err());
}

#[test]
fn one_activation_with_a_fresh_generation_is_a_valid_exact_next_successor() {
    let current = authority::<4>(2, [active_a(), pending_b()]);
    let candidate = authority::<4>(
        3,
        [
            active_a(),
            secret_record(
                2,
                3,
                PRINCIPAL_B,
                Permissions::SUBMIT_RNS_DATA,
                CredentialStatus::Active,
                1,
                PSK_B,
            ),
        ],
    );

    let published = publish_successor(&current, candidate);
    assert_eq!(published.revision(), AuthorityRevision::new(3));
    assert_eq!(published.active_count(), 2);
    assert!(published.select_for_handshake(id(2)).is_ok());
    assert!(
        published
            .revalidate(id(2), CredentialGeneration::new(2))
            .is_err()
    );
    assert!(
        published
            .revalidate(id(2), CredentialGeneration::new(3))
            .is_ok()
    );
}

#[test]
fn authorization_relevant_changes_cannot_reuse_a_generation() {
    let current = authority::<4>(2, [active_a(), pending_b()]);

    for candidate in [
        authority::<4>(
            3,
            [
                secret_record(
                    1,
                    1,
                    PRINCIPAL_A,
                    Permissions::READ_SUBMISSION_STATUS | Permissions::SUBMIT_RNS_DATA,
                    CredentialStatus::Active,
                    1,
                    PSK_A,
                ),
                pending_b(),
            ],
        ),
        authority::<4>(
            3,
            [
                secret_record(
                    1,
                    1,
                    PRINCIPAL_B,
                    Permissions::READ_SUBMISSION_STATUS,
                    CredentialStatus::Active,
                    1,
                    PSK_A,
                ),
                pending_b(),
            ],
        ),
        authority::<4>(
            3,
            [
                secret_record(
                    1,
                    1,
                    PRINCIPAL_A,
                    Permissions::READ_SUBMISSION_STATUS,
                    CredentialStatus::Active,
                    1,
                    PSK_C,
                ),
                pending_b(),
            ],
        ),
        authority::<4>(
            3,
            [
                active_a(),
                secret_record(
                    2,
                    2,
                    PRINCIPAL_B,
                    Permissions::SUBMIT_RNS_DATA,
                    CredentialStatus::Active,
                    1,
                    PSK_B,
                ),
            ],
        ),
        authority::<4>(
            3,
            [
                secret_record(
                    1,
                    1,
                    PRINCIPAL_A,
                    Permissions::READ_SUBMISSION_STATUS,
                    CredentialStatus::Active,
                    2,
                    PSK_A,
                ),
                pending_b(),
            ],
        ),
    ] {
        let recovered = assert_successor_fault(
            &current,
            candidate,
            CredentialSuccessorFaultKind::ChangedWithoutGenerationAdvance,
        );
        assert_eq!(recovered.revision(), AuthorityRevision::new(3));
    }
}

#[test]
fn successor_revision_must_advance_exactly_once() {
    let current = authority::<4>(2, [active_a(), pending_b()]);

    for candidate in [
        authority::<4>(2, [active_a(), pending_b()]),
        authority::<4>(4, [active_a(), pending_b()]),
    ] {
        let recovered = assert_successor_fault(
            &current,
            candidate,
            CredentialSuccessorFaultKind::AuthorityRevisionNotNext,
        );
        assert!(matches!(recovered.revision().get(), 2 | 4));
    }
}

#[test]
fn active_and_pending_records_cannot_disappear_without_a_tombstone() {
    let current = authority::<4>(2, [active_a(), pending_b()]);

    for candidate in [
        authority::<4>(3, [pending_b()]),
        authority::<4>(3, [active_a()]),
    ] {
        let recovered = assert_successor_fault(
            &current,
            candidate,
            CredentialSuccessorFaultKind::CredentialRemovedWithoutTombstone,
        );
        assert_eq!(recovered.record_count(), 1);
    }
}

#[test]
fn one_transaction_cannot_mutate_more_than_one_record() {
    let current = authority::<4>(2, [active_a(), pending_b()]);
    let candidate = authority::<4>(
        3,
        [
            secret_record(
                1,
                3,
                PRINCIPAL_A,
                Permissions::NONE,
                CredentialStatus::Active,
                1,
                PSK_A,
            ),
            secret_record(
                2,
                2,
                PRINCIPAL_B,
                Permissions::SUBMIT_RNS_DATA,
                CredentialStatus::Active,
                1,
                PSK_B,
            ),
        ],
    );

    let recovered = assert_successor_fault(
        &current,
        candidate,
        CredentialSuccessorFaultKind::MultipleRecordMutations,
    );
    assert_eq!(recovered.record_count(), 2);
}
