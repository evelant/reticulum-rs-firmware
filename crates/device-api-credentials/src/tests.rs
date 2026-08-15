extern crate std;

use core::mem::size_of;

use reticulum_device_api::{Permissions, PrincipalId};
use zeroize::Zeroizing;

use crate::{
    AuthorityRevision, AuthorityRevisionExhausted, AuthorizationPolicyVersion, CredentialAudit,
    CredentialAuthority, CredentialAuthorityBuilder, CredentialGeneration, CredentialId,
    CredentialLifecycleFaultKind, CredentialLoadFaultKind, CredentialRecord, CredentialSecret,
    CredentialStatus, CredentialSuccessorFaultKind, E290_CREDENTIAL_AUTHORITY_RAM_CEILING,
    E290_CREDENTIAL_RECORD_CAPACITY, NewPendingCredential, PairingLifecycleStoreCandidate,
    PairingLifecycleStoreCandidateFaultKind, PairingLifecycleTransition, PairingOrigin,
    PendingCredentialRef, RevocationReason,
};

const PRINCIPAL: PrincipalId = PrincipalId([0x31; 16]);
const OTHER_PRINCIPAL: PrincipalId = PrincipalId([0x42; 16]);
const PSK: [u8; 32] = [0x5a; 32];

fn id(value: u8) -> CredentialId {
    CredentialId::new([value; 16])
}

fn audit(created: u64, modified: u64, policy: u32) -> CredentialAudit {
    CredentialAudit::new(
        AuthorityRevision::new(created),
        AuthorityRevision::new(modified),
        PairingOrigin::LocalPhysicalPresence,
        AuthorizationPolicyVersion::new(policy),
    )
}

fn with_secret(
    id_byte: u8,
    generation: u64,
    principal: PrincipalId,
    permissions: Permissions,
    status: CredentialStatus,
) -> CredentialRecord {
    let psk = [0x50_u8.wrapping_add(id_byte); 32];
    CredentialRecord::with_secret(
        id(id_byte),
        CredentialGeneration::new(generation),
        principal,
        permissions,
        status,
        audit(1, generation, 1),
        psk,
    )
}

fn authority<const N: usize>(
    revision: u64,
    records: impl IntoIterator<Item = CredentialRecord>,
) -> CredentialAuthority<N> {
    let mut builder = CredentialAuthorityBuilder::new(AuthorityRevision::new(revision))
        .unwrap_or_else(|fault| panic!("builder rejected revision: {:?}", fault.kind()));
    for record in records {
        builder = builder
            .insert(record)
            .unwrap_or_else(|fault| panic!("builder rejected record: {:?}", fault.kind()));
    }
    builder.finish()
}

fn new_builder<const N: usize>(revision: u64) -> CredentialAuthorityBuilder<N> {
    match CredentialAuthorityBuilder::new(AuthorityRevision::new(revision)) {
        Ok(builder) => builder,
        Err(fault) => panic!("builder rejected revision: {:?}", fault.kind()),
    }
}

fn insert_ok<const N: usize>(
    builder: CredentialAuthorityBuilder<N>,
    record: CredentialRecord,
) -> CredentialAuthorityBuilder<N> {
    builder
        .insert(record)
        .unwrap_or_else(|fault| panic!("builder rejected record: {:?}", fault.kind()))
}

#[test]
fn only_active_records_select_and_device_owned_facts_mint_the_lease() {
    let active_permissions = Permissions::READ_SUBMISSION_STATUS | Permissions::SUBMIT_RNS_DATA;
    let revoked = CredentialRecord::revoked(
        id(3),
        CredentialGeneration::new(3),
        OTHER_PRINCIPAL,
        audit(3, 3, 1),
        RevocationReason::Explicit,
    );
    let authority = authority::<4>(
        3,
        [
            with_secret(
                1,
                1,
                PRINCIPAL,
                active_permissions,
                CredentialStatus::Active,
            ),
            with_secret(
                2,
                2,
                OTHER_PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
            ),
            revoked,
        ],
    );

    assert_eq!(authority.record_count(), 3);
    assert_eq!(authority.active_count(), 1);
    assert!(authority.select_for_handshake(id(1)).is_ok());
    assert!(authority.select_for_handshake(id(2)).is_err());
    assert!(authority.select_for_handshake(id(3)).is_err());
    assert!(authority.select_for_handshake(id(99)).is_err());

    let lease = authority
        .revalidate(id(1), CredentialGeneration::new(1))
        .unwrap_or_else(|_| panic!("active exact generation was rejected"));
    lease.with_dispatch_context(|context| {
        assert_eq!(context.principal(), Some(PRINCIPAL));
        assert_eq!(context.permissions(), active_permissions);
        let provenance = context
            .provenance()
            .unwrap_or_else(|| panic!("authenticated lease omitted provenance"));
        assert_eq!(provenance.credential_id(), [1; 16]);
        assert_eq!(provenance.credential_generation(), 1);
        assert_eq!(provenance.authority_revision(), 3);
        assert_eq!(provenance.policy_version(), 1);
    });
    assert_eq!(lease.credential_id(), id(1));
    assert_eq!(lease.generation(), CredentialGeneration::new(1));
    assert_eq!(lease.authority_revision(), AuthorityRevision::new(3));
    assert_eq!(lease.policy_version(), AuthorizationPolicyVersion::new(1));
    assert_eq!(lease.pairing_origin(), PairingOrigin::LocalPhysicalPresence);
}

#[test]
fn replacement_snapshots_invalidate_old_grants_before_dispatch() {
    let old = authority::<2>(
        1,
        [with_secret(
            1,
            1,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            CredentialStatus::Active,
        )],
    );
    assert!(old.revalidate(id(1), CredentialGeneration::new(1)).is_ok());

    let permission_changed = authority::<2>(
        2,
        [CredentialRecord::with_secret(
            id(1),
            CredentialGeneration::new(2),
            PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Active,
            audit(1, 2, 2),
            [0x6b; 32],
        )],
    );
    assert!(
        permission_changed
            .revalidate(id(1), CredentialGeneration::new(1))
            .is_err()
    );
    let new_lease = permission_changed
        .revalidate(id(1), CredentialGeneration::new(2))
        .unwrap_or_else(|_| panic!("new generation was rejected"));
    new_lease.with_dispatch_context(|context| {
        assert_eq!(context.principal(), Some(PRINCIPAL));
        assert_eq!(context.permissions(), Permissions::NONE);
    });

    let revoked = authority::<2>(
        3,
        [CredentialRecord::revoked(
            id(1),
            CredentialGeneration::new(3),
            PRINCIPAL,
            CredentialAudit::new(
                AuthorityRevision::new(1),
                AuthorityRevision::new(3),
                PairingOrigin::LocalPhysicalPresence,
                AuthorizationPolicyVersion::new(2),
            ),
            RevocationReason::Explicit,
        )],
    );
    assert!(
        revoked
            .revalidate(id(1), CredentialGeneration::new(2))
            .is_err()
    );
    assert!(revoked.select_for_handshake(id(1)).is_err());

    let replacement = authority::<2>(
        4,
        [CredentialRecord::with_secret(
            id(2),
            CredentialGeneration::new(4),
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            CredentialStatus::Active,
            audit(4, 4, 2),
            [0x7c; 32],
        )],
    );
    let lease = replacement
        .revalidate(id(2), CredentialGeneration::new(4))
        .unwrap_or_else(|_| panic!("replacement credential was rejected"));
    lease.with_dispatch_context(|context| assert_eq!(context.principal(), Some(PRINCIPAL)));
}

#[test]
fn builder_rejects_noncanonical_and_ambiguous_snapshots_with_exact_owner() {
    let fault = match CredentialAuthorityBuilder::<1>::new(AuthorityRevision::new(0)) {
        Ok(_) => panic!("zero authority revision was accepted"),
        Err(fault) => fault,
    };
    assert_eq!(fault.kind(), CredentialLoadFaultKind::ZeroAuthorityRevision);
    assert!(fault.into_record().is_none());

    for (record, expected) in [
        (
            CredentialRecord::with_secret(
                id(0),
                CredentialGeneration::new(1),
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Active,
                audit(1, 1, 1),
                PSK,
            ),
            CredentialLoadFaultKind::ZeroCredentialId,
        ),
        (
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(0),
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Active,
                audit(1, 1, 1),
                PSK,
            ),
            CredentialLoadFaultKind::ZeroGeneration,
        ),
        (
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(1),
                PrincipalId([0; 16]),
                Permissions::NONE,
                CredentialStatus::Active,
                audit(1, 1, 1),
                PSK,
            ),
            CredentialLoadFaultKind::ZeroPrincipal,
        ),
        (
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(1),
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Active,
                audit(1, 1, 1),
                [0; 32],
            ),
            CredentialLoadFaultKind::ZeroPsk,
        ),
        (
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(1),
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Active,
                audit(1, 1, 0),
                PSK,
            ),
            CredentialLoadFaultKind::ZeroPolicyVersion,
        ),
        (
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(2),
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Active,
                audit(1, 2, 1),
                PSK,
            ),
            CredentialLoadFaultKind::InvalidRevisionOrder,
        ),
        (
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(1),
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Revoked,
                audit(1, 1, 1),
                PSK,
            ),
            CredentialLoadFaultKind::RevokedStatusWithSecret,
        ),
    ] {
        let builder = new_builder::<1>(1);
        let fault = builder.insert(record).unwrap_err_or_else();
        assert_eq!(fault.kind(), expected);
        assert!(fault.into_record().is_some());
    }

    let duplicate_id = insert_ok(
        new_builder::<2>(2),
        with_secret(1, 1, PRINCIPAL, Permissions::NONE, CredentialStatus::Active),
    );
    let fault = duplicate_id
        .insert(CredentialRecord::with_secret(
            id(1),
            CredentialGeneration::new(2),
            OTHER_PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Active,
            audit(2, 2, 1),
            [0x22; 32],
        ))
        .unwrap_err_or_else();
    assert_eq!(fault.kind(), CredentialLoadFaultKind::DuplicateCredentialId);

    let duplicate_generation = insert_ok(
        new_builder::<2>(1),
        with_secret(1, 1, PRINCIPAL, Permissions::NONE, CredentialStatus::Active),
    );
    let fault = duplicate_generation
        .insert(CredentialRecord::with_secret(
            id(2),
            CredentialGeneration::new(1),
            OTHER_PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Active,
            audit(1, 1, 1),
            [0x22; 32],
        ))
        .unwrap_err_or_else();
    assert_eq!(fault.kind(), CredentialLoadFaultKind::DuplicateGeneration);

    let duplicate_psk = insert_ok(
        new_builder::<2>(2),
        with_secret(1, 1, PRINCIPAL, Permissions::NONE, CredentialStatus::Active),
    );
    let fault = duplicate_psk
        .insert(CredentialRecord::with_secret(
            id(2),
            CredentialGeneration::new(2),
            OTHER_PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Pending,
            audit(2, 2, 1),
            [0x51; 32],
        ))
        .unwrap_err_or_else();
    assert_eq!(fault.kind(), CredentialLoadFaultKind::DuplicatePsk);

    let full = insert_ok(
        new_builder::<1>(2),
        with_secret(1, 1, PRINCIPAL, Permissions::NONE, CredentialStatus::Active),
    );
    let fault = full
        .insert(CredentialRecord::with_secret(
            id(2),
            CredentialGeneration::new(2),
            OTHER_PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Active,
            audit(2, 2, 1),
            [0x22; 32],
        ))
        .unwrap_err_or_else();
    assert_eq!(fault.kind(), CredentialLoadFaultKind::CapacityExhausted);
}

#[test]
fn revision_and_permission_vocabularies_never_wrap_or_feature_drift() {
    assert_eq!(
        AuthorityRevision::new(u64::MAX).next(),
        Err(AuthorityRevisionExhausted)
    );
    assert_eq!(
        AuthorityRevision::new(41).next(),
        Ok(AuthorityRevision::new(42))
    );

    let all = Permissions::READ_SUBMISSION_STATUS | Permissions::SUBMIT_RNS_DATA;
    assert_eq!(Permissions::from_bits(all.bits()), Ok(all));
    assert_eq!(
        Permissions::from_bits(1 << 31).unwrap_err().unknown(),
        1 << 31
    );
}

#[test]
fn e290_authority_has_a_bounded_internal_ram_ceiling() {
    assert!(
        size_of::<CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>>()
            <= E290_CREDENTIAL_AUTHORITY_RAM_CEILING
    );
    assert!(size_of::<crate::SelectedCredential>() <= 64);
}

trait ResultFaultExt<T> {
    fn unwrap_err_or_else(self) -> T;
}

impl<T, U> ResultFaultExt<U> for Result<T, U> {
    fn unwrap_err_or_else(self) -> U {
        match self {
            Ok(_) => panic!("noncanonical credential was accepted"),
            Err(error) => error,
        }
    }
}

#[test]
fn internal_invalid_secret_shapes_are_rejected() {
    let missing = CredentialRecord {
        id: id(1),
        generation: CredentialGeneration::new(1),
        principal: PRINCIPAL,
        permissions: Permissions::NONE,
        status: CredentialStatus::Pending,
        audit: audit(1, 1, 1),
        revocation_reason: None,
        secret: CredentialSecret::Absent,
    };
    let builder = new_builder::<1>(1);
    let fault = builder.insert(missing).unwrap_err_or_else();
    assert_eq!(fault.kind(), CredentialLoadFaultKind::MissingSecret);

    let revoked_without_reason = CredentialRecord {
        id: id(2),
        generation: CredentialGeneration::new(2),
        principal: PRINCIPAL,
        permissions: Permissions::NONE,
        status: CredentialStatus::Revoked,
        audit: audit(2, 2, 1),
        revocation_reason: None,
        secret: CredentialSecret::Absent,
    };
    let builder = new_builder::<1>(2);
    let fault = builder.insert(revoked_without_reason).unwrap_err_or_else();
    assert_eq!(fault.kind(), CredentialLoadFaultKind::InvalidRevokedRecord);
}

fn enrollment(
    id_byte: u8,
    principal: PrincipalId,
    permissions: Permissions,
    policy: u32,
    psk: [u8; 32],
) -> NewPendingCredential {
    NewPendingCredential::new(
        id(id_byte),
        principal,
        permissions,
        PairingOrigin::LocalPhysicalPresence,
        AuthorizationPolicyVersion::new(policy),
        Zeroizing::new(psk),
    )
}

fn cross_predecessor_pending(
    generation: u64,
    principal: PrincipalId,
    permissions: Permissions,
    policy: u32,
    psk: [u8; 32],
) -> CredentialAuthority<4> {
    authority::<4>(
        4,
        [CredentialRecord::with_secret(
            id(7),
            CredentialGeneration::new(generation),
            principal,
            permissions,
            CredentialStatus::Pending,
            audit(1, generation, policy),
            psk,
        )],
    )
}

fn pending_transition_store_candidate(
    transition: PairingLifecycleTransition,
) -> PairingLifecycleStoreCandidate<4> {
    let source = cross_predecessor_pending(
        2,
        PRINCIPAL,
        Permissions::READ_SUBMISSION_STATUS,
        7,
        [0xa1; 32],
    );
    let pending = PendingCredentialRef::new(id(7), CredentialGeneration::new(2));
    match transition {
        PairingLifecycleTransition::ActivatePending => source
            .plan_activate_pending(pending)
            .unwrap_or_else(|fault| panic!("activation plan rejected: {:?}", fault.kind()))
            .into_store_candidate(),
        PairingLifecycleTransition::AbortPending => source
            .plan_abort_pending(pending)
            .unwrap_or_else(|fault| panic!("abort plan rejected: {:?}", fault.kind()))
            .into_store_candidate(),
        PairingLifecycleTransition::AddPending => {
            panic!("pending transition helper does not construct AddPending")
        }
    }
}

fn assert_enrollment_fault<const N: usize>(
    authority: &CredentialAuthority<N>,
    enrollment: NewPendingCredential,
    expected: CredentialLifecycleFaultKind,
    expected_psk: [u8; 32],
) {
    let fault = match authority.plan_add_pending(enrollment) {
        Ok(_) => panic!("invalid pending enrollment was accepted"),
        Err(fault) => fault,
    };
    assert_eq!(fault.kind(), expected);
    let (_, _, _, _, _, recovered_psk) = fault.into_enrollment().into_parts();
    assert_eq!(*recovered_psk, expected_psk);
}

#[test]
fn pending_lookup_is_exact_single_record_and_zeroizing() {
    let pending_psk = [0x92; 32];
    let current = authority::<4>(
        3,
        [
            with_secret(
                1,
                1,
                PRINCIPAL,
                Permissions::READ_SUBMISSION_STATUS,
                CredentialStatus::Active,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(3),
                OTHER_PRINCIPAL,
                Permissions::SUBMIT_RNS_DATA,
                CredentialStatus::Pending,
                audit(2, 3, 7),
                pending_psk,
            ),
        ],
    );
    let pending = current
        .pending_credential()
        .unwrap_or_else(|_| panic!("single pending record was rejected"))
        .unwrap_or_else(|| panic!("pending record was not found"));
    assert_eq!(pending.id(), id(2));
    assert_eq!(pending.generation(), CredentialGeneration::new(3));

    let selected = current
        .select_pending_for_proof_internal(pending)
        .unwrap_or_else(|_| panic!("exact pending record was not selected"));
    assert_eq!(selected.pending(), pending);
    let (selected_ref, selected_psk) = selected.into_parts();
    assert_eq!(selected_ref, pending);
    assert_eq!(*selected_psk, pending_psk);

    for wrong in [
        PendingCredentialRef::new(id(1), CredentialGeneration::new(1)),
        PendingCredentialRef::new(id(2), CredentialGeneration::new(2)),
        PendingCredentialRef::new(id(99), CredentialGeneration::new(3)),
    ] {
        assert!(current.select_pending_for_proof_internal(wrong).is_err());
    }

    let none = authority::<2>(
        1,
        [with_secret(
            1,
            1,
            PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Active,
        )],
    );
    assert_eq!(none.pending_credential(), Ok(None));
    assert!(none.select_pending_for_proof_internal(pending).is_err());

    let multiple = authority::<3>(
        2,
        [
            with_secret(
                1,
                1,
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(2),
                OTHER_PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
                audit(2, 2, 1),
                [0x62; 32],
            ),
        ],
    );
    assert!(multiple.pending_credential().is_err());
    assert!(
        multiple
            .select_pending_for_proof_internal(PendingCredentialRef::new(
                id(1),
                CredentialGeneration::new(1),
            ))
            .is_err()
    );
}

#[test]
fn retained_id_membership_includes_every_lifecycle_state() {
    let authority = authority::<4>(
        3,
        [
            with_secret(1, 1, PRINCIPAL, Permissions::NONE, CredentialStatus::Active),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(2),
                OTHER_PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
                audit(2, 2, 1),
                [0x62; 32],
            ),
            CredentialRecord::revoked(
                id(3),
                CredentialGeneration::new(3),
                PRINCIPAL,
                audit(3, 3, 1),
                RevocationReason::PairingAborted,
            ),
        ],
    );
    for retained in [id(1), id(2), id(3)] {
        assert!(authority.retains_id(retained));
    }
    assert!(!authority.retains_id(id(4)));
}

#[test]
fn add_pending_allocates_exact_next_facts_and_preserves_all_existing_secrets() {
    let active_psk = [0xa1; 32];
    let new_psk = [0xc3; 32];
    let current = authority::<4>(
        3,
        [
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(1),
                PRINCIPAL,
                Permissions::READ_SUBMISSION_STATUS,
                CredentialStatus::Active,
                audit(1, 1, 4),
                active_psk,
            ),
            CredentialRecord::revoked(
                id(2),
                CredentialGeneration::new(3),
                OTHER_PRINCIPAL,
                audit(2, 3, 5),
                RevocationReason::Explicit,
            ),
        ],
    );
    let plan = current
        .plan_add_pending(enrollment(
            3,
            OTHER_PRINCIPAL,
            Permissions::SUBMIT_RNS_DATA,
            9,
            new_psk,
        ))
        .unwrap_or_else(|fault| panic!("valid enrollment rejected: {:?}", fault.kind()));
    assert_eq!(plan.transition(), PairingLifecycleTransition::AddPending);
    assert_eq!(plan.candidate_revision(), AuthorityRevision::new(4));
    let store_candidate = plan.into_store_candidate();
    assert_eq!(
        store_candidate.transition(),
        PairingLifecycleTransition::AddPending
    );
    assert_eq!(store_candidate.revision(), AuthorityRevision::new(4));
    let candidate = store_candidate.into_unpublished_authority_internal();

    assert_eq!(current.revision(), AuthorityRevision::new(3));
    assert_eq!(candidate.revision(), AuthorityRevision::new(4));
    assert_eq!(candidate.record_count(), 3);
    assert!(
        current
            .find_by_id(id(1))
            .is_some_and(|record| { record.psk().is_some_and(|psk| *psk == active_psk) })
    );
    assert!(candidate.find_by_id(id(1)).is_some_and(|record| {
        record.psk().is_some_and(|psk| *psk == active_psk)
            && record.generation == CredentialGeneration::new(1)
    }));
    let pending = candidate
        .find_by_id(id(3))
        .unwrap_or_else(|| panic!("candidate omitted new pending record"));
    assert_eq!(pending.status, CredentialStatus::Pending);
    assert_eq!(pending.generation, CredentialGeneration::new(4));
    assert_eq!(pending.principal, OTHER_PRINCIPAL);
    assert_eq!(pending.permissions, Permissions::SUBMIT_RNS_DATA);
    assert_eq!(pending.audit.created_revision(), AuthorityRevision::new(4));
    assert_eq!(pending.audit.modified_revision(), AuthorityRevision::new(4));
    assert_eq!(
        pending.audit.policy_version(),
        AuthorizationPolicyVersion::new(9)
    );
    assert!(pending.psk().is_some_and(|psk| *psk == new_psk));
    assert!(
        current
            .pending_credential()
            .is_ok_and(|pending| pending.is_none())
    );
}

#[test]
fn pairing_store_candidate_preflight_retains_opaque_owner_on_both_outcomes() {
    let current = authority::<4>(
        3,
        [with_secret(
            1,
            1,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            CredentialStatus::Active,
        )],
    );
    let candidate = current
        .plan_add_pending(enrollment(
            2,
            OTHER_PRINCIPAL,
            Permissions::SUBMIT_RNS_DATA,
            9,
            [0xc3; 32],
        ))
        .unwrap_or_else(|fault| panic!("valid enrollment rejected: {:?}", fault.kind()))
        .into_store_candidate();

    let wrong_current = authority::<4>(1, []);
    let fault = candidate
        .validate_against(&wrong_current)
        .err()
        .unwrap_or_else(|| panic!("wrong predecessor accepted lifecycle candidate"));
    assert_eq!(
        fault.kind(),
        PairingLifecycleStoreCandidateFaultKind::Structural(
            CredentialSuccessorFaultKind::AuthorityRevisionNotNext
        )
    );

    let candidate = fault.into_candidate();
    assert_eq!(
        candidate.transition(),
        PairingLifecycleTransition::AddPending
    );
    assert_eq!(candidate.revision(), AuthorityRevision::new(4));

    let candidate = candidate
        .validate_against(&current)
        .unwrap_or_else(|fault| panic!("correct predecessor rejected: {:?}", fault.kind()));
    assert_eq!(
        candidate.transition(),
        PairingLifecycleTransition::AddPending
    );
    assert_eq!(candidate.revision(), AuthorityRevision::new(4));
    let authority = candidate.into_unpublished_authority_internal();
    assert_eq!(authority.revision(), AuthorityRevision::new(4));
    assert!(
        authority
            .pending_credential()
            .is_ok_and(|pending| pending.is_some_and(|pending| pending.id() == id(2)))
    );
}

#[test]
fn pairing_store_preflight_rejects_same_revision_add_collision() {
    let source = authority::<4>(
        3,
        [with_secret(
            1,
            1,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            CredentialStatus::Active,
        )],
    );
    let candidate = source
        .plan_add_pending(enrollment(
            2,
            OTHER_PRINCIPAL,
            Permissions::SUBMIT_RNS_DATA,
            9,
            [0xc4; 32],
        ))
        .unwrap_or_else(|fault| panic!("add plan rejected: {:?}", fault.kind()))
        .into_store_candidate();
    let wrong = authority::<4>(
        3,
        [
            with_secret(
                1,
                1,
                PRINCIPAL,
                Permissions::READ_SUBMISSION_STATUS,
                CredentialStatus::Active,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(2),
                OTHER_PRINCIPAL,
                Permissions::SUBMIT_RNS_DATA,
                CredentialStatus::Active,
                audit(2, 2, 9),
                [0xc5; 32],
            ),
        ],
    );

    let fault = candidate
        .validate_against(&wrong)
        .err()
        .unwrap_or_else(|| panic!("same-revision retained-ID collision was accepted"));
    assert_eq!(
        fault.kind(),
        PairingLifecycleStoreCandidateFaultKind::Structural(
            CredentialSuccessorFaultKind::ExistingCredentialAuditChanged
        )
    );
    let candidate = fault.into_candidate();
    assert_eq!(
        candidate.transition(),
        PairingLifecycleTransition::AddPending
    );
    assert_eq!(candidate.revision(), AuthorityRevision::new(4));
}

#[test]
fn pairing_store_preflight_rejects_activate_cross_predecessor_facts_and_generation() {
    let wrong_predecessors = [
        cross_predecessor_pending(
            2,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            7,
            [0xa2; 32],
        ),
        cross_predecessor_pending(
            2,
            OTHER_PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            7,
            [0xa1; 32],
        ),
        cross_predecessor_pending(2, PRINCIPAL, Permissions::SUBMIT_RNS_DATA, 7, [0xa1; 32]),
        cross_predecessor_pending(
            2,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            8,
            [0xa1; 32],
        ),
        cross_predecessor_pending(
            3,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            7,
            [0xa1; 32],
        ),
    ];

    for (case, wrong) in wrong_predecessors.iter().enumerate() {
        let fault = pending_transition_store_candidate(PairingLifecycleTransition::ActivatePending)
            .validate_against(wrong)
            .err()
            .unwrap_or_else(|| panic!("activate cross-predecessor case {case} was accepted"));
        assert_eq!(
            fault.kind(),
            PairingLifecycleStoreCandidateFaultKind::TransitionMismatch,
            "activate cross-predecessor case {case} returned the wrong category"
        );
        let candidate = fault.into_candidate();
        assert_eq!(
            candidate.transition(),
            PairingLifecycleTransition::ActivatePending
        );
        assert_eq!(candidate.revision(), AuthorityRevision::new(5));
    }
}

#[test]
fn pairing_store_preflight_rejects_abort_cross_predecessor_facts_and_generation() {
    let wrong_predecessors = [
        cross_predecessor_pending(
            2,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            7,
            [0xa2; 32],
        ),
        cross_predecessor_pending(
            2,
            OTHER_PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            7,
            [0xa1; 32],
        ),
        cross_predecessor_pending(2, PRINCIPAL, Permissions::SUBMIT_RNS_DATA, 7, [0xa1; 32]),
        cross_predecessor_pending(
            2,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            8,
            [0xa1; 32],
        ),
        cross_predecessor_pending(
            3,
            PRINCIPAL,
            Permissions::READ_SUBMISSION_STATUS,
            7,
            [0xa1; 32],
        ),
    ];

    for (case, wrong) in wrong_predecessors.iter().enumerate() {
        let fault = pending_transition_store_candidate(PairingLifecycleTransition::AbortPending)
            .validate_against(wrong)
            .err()
            .unwrap_or_else(|| panic!("abort cross-predecessor case {case} was accepted"));
        assert_eq!(
            fault.kind(),
            PairingLifecycleStoreCandidateFaultKind::TransitionMismatch,
            "abort cross-predecessor case {case} returned the wrong category"
        );
        let candidate = fault.into_candidate();
        assert_eq!(
            candidate.transition(),
            PairingLifecycleTransition::AbortPending
        );
        assert_eq!(candidate.revision(), AuthorityRevision::new(5));
    }
}

#[test]
fn rejected_add_pending_returns_the_exact_secret_owner_for_every_policy_gate() {
    let proposal_psk = [0xd4; 32];
    let empty = authority::<2>(1, []);
    assert_enrollment_fault(
        &empty,
        enrollment(0, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::ZeroCredentialId,
        proposal_psk,
    );
    assert_enrollment_fault(
        &empty,
        enrollment(1, PrincipalId([0; 16]), Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::ZeroPrincipal,
        proposal_psk,
    );
    assert_enrollment_fault(
        &empty,
        enrollment(1, PRINCIPAL, Permissions::NONE, 0, proposal_psk),
        CredentialLifecycleFaultKind::ZeroPolicyVersion,
        proposal_psk,
    );
    assert_enrollment_fault(
        &empty,
        enrollment(1, PRINCIPAL, Permissions::NONE, 1, [0; 32]),
        CredentialLifecycleFaultKind::ZeroPsk,
        [0; 32],
    );

    let active = authority::<3>(
        1,
        [CredentialRecord::with_secret(
            id(1),
            CredentialGeneration::new(1),
            PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Active,
            audit(1, 1, 1),
            proposal_psk,
        )],
    );
    assert_enrollment_fault(
        &active,
        enrollment(1, PRINCIPAL, Permissions::NONE, 1, [0xe5; 32]),
        CredentialLifecycleFaultKind::CredentialIdAlreadyRetained,
        [0xe5; 32],
    );
    assert_enrollment_fault(
        &active,
        enrollment(2, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::DuplicatePsk,
        proposal_psk,
    );

    let tombstoned = authority::<2>(
        1,
        [CredentialRecord::revoked(
            id(1),
            CredentialGeneration::new(1),
            PRINCIPAL,
            audit(1, 1, 1),
            RevocationReason::PairingAborted,
        )],
    );
    assert_enrollment_fault(
        &tombstoned,
        enrollment(1, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::CredentialIdAlreadyRetained,
        proposal_psk,
    );

    let with_pending = authority::<3>(
        1,
        [with_secret(
            1,
            1,
            PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Pending,
        )],
    );
    assert_enrollment_fault(
        &with_pending,
        enrollment(2, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::PendingAlreadyExists,
        proposal_psk,
    );

    let multiple = authority::<3>(
        2,
        [
            with_secret(
                1,
                1,
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(2),
                OTHER_PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
                audit(2, 2, 1),
                [0x62; 32],
            ),
        ],
    );
    assert_enrollment_fault(
        &multiple,
        enrollment(3, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::MultiplePendingCredentials,
        proposal_psk,
    );

    let full_multiple = authority::<2>(
        2,
        [
            with_secret(
                1,
                1,
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(2),
                OTHER_PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
                audit(2, 2, 1),
                [0x62; 32],
            ),
        ],
    );
    assert_enrollment_fault(
        &full_multiple,
        enrollment(3, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::MultiplePendingCredentials,
        proposal_psk,
    );

    let exhausted_multiple = authority::<2>(
        u64::MAX,
        [
            with_secret(
                1,
                1,
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(u64::MAX),
                OTHER_PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
                audit(2, u64::MAX, 1),
                [0x62; 32],
            ),
        ],
    );
    assert_enrollment_fault(
        &exhausted_multiple,
        enrollment(3, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::MultiplePendingCredentials,
        proposal_psk,
    );

    let exhausted_pending = authority::<2>(
        u64::MAX,
        [CredentialRecord::with_secret(
            id(1),
            CredentialGeneration::new(u64::MAX),
            PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Pending,
            audit(1, u64::MAX, 1),
            [0x51; 32],
        )],
    );
    assert_enrollment_fault(
        &exhausted_pending,
        enrollment(2, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::PendingAlreadyExists,
        proposal_psk,
    );

    let full = authority::<1>(
        1,
        [with_secret(
            1,
            1,
            PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Active,
        )],
    );
    assert_enrollment_fault(
        &full,
        enrollment(2, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::CapacityExhausted,
        proposal_psk,
    );

    let exhausted = authority::<2>(u64::MAX, []);
    assert_enrollment_fault(
        &exhausted,
        enrollment(1, PRINCIPAL, Permissions::NONE, 1, proposal_psk),
        CredentialLifecycleFaultKind::AuthorityRevisionExhausted,
        proposal_psk,
    );
}

#[test]
fn activation_preserves_psks_and_policy_while_advancing_only_pending_generation() {
    let active_psk = [0xa1; 32];
    let pending_psk = [0xb2; 32];
    let current = authority::<4>(
        3,
        [
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(1),
                PRINCIPAL,
                Permissions::READ_SUBMISSION_STATUS,
                CredentialStatus::Active,
                audit(1, 1, 3),
                active_psk,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(3),
                OTHER_PRINCIPAL,
                Permissions::SUBMIT_RNS_DATA,
                CredentialStatus::Pending,
                audit(2, 3, 7),
                pending_psk,
            ),
        ],
    );
    let pending = current
        .pending_credential()
        .unwrap_or_else(|_| panic!("single pending rejected"))
        .unwrap_or_else(|| panic!("pending omitted"));
    let plan = current
        .plan_activate_pending(pending)
        .unwrap_or_else(|fault| panic!("activation rejected: {:?}", fault.kind()));
    assert_eq!(
        plan.transition(),
        PairingLifecycleTransition::ActivatePending
    );
    let store_candidate = plan.into_store_candidate();
    assert_eq!(
        store_candidate.transition(),
        PairingLifecycleTransition::ActivatePending
    );
    assert_eq!(store_candidate.revision(), AuthorityRevision::new(4));
    let candidate = store_candidate.into_unpublished_authority_internal();
    assert_eq!(candidate.revision(), AuthorityRevision::new(4));
    assert_eq!(candidate.active_count(), 2);
    assert_eq!(candidate.pending_credential(), Ok(None));

    let unchanged = candidate
        .find_by_id(id(1))
        .unwrap_or_else(|| panic!("active record disappeared"));
    assert_eq!(unchanged.generation, CredentialGeneration::new(1));
    assert!(unchanged.psk().is_some_and(|psk| *psk == active_psk));
    let activated = candidate
        .find_by_id(id(2))
        .unwrap_or_else(|| panic!("activated record disappeared"));
    assert_eq!(activated.status, CredentialStatus::Active);
    assert_eq!(activated.generation, CredentialGeneration::new(4));
    assert_eq!(activated.principal, OTHER_PRINCIPAL);
    assert_eq!(activated.permissions, Permissions::SUBMIT_RNS_DATA);
    assert_eq!(
        activated.audit.created_revision(),
        AuthorityRevision::new(2)
    );
    assert_eq!(
        activated.audit.modified_revision(),
        AuthorityRevision::new(4)
    );
    assert_eq!(
        activated.audit.policy_version(),
        AuthorizationPolicyVersion::new(7)
    );
    assert!(activated.psk().is_some_and(|psk| *psk == pending_psk));

    assert_eq!(current.pending_credential(), Ok(Some(pending)));
    assert!(current.select_pending_for_proof_internal(pending).is_ok());
}

#[test]
fn abort_drops_only_pending_psk_and_retains_an_exact_pairing_tombstone() {
    let active_psk = [0xa1; 32];
    let current = authority::<4>(
        3,
        [
            CredentialRecord::with_secret(
                id(1),
                CredentialGeneration::new(1),
                PRINCIPAL,
                Permissions::READ_SUBMISSION_STATUS,
                CredentialStatus::Active,
                audit(1, 1, 3),
                active_psk,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(3),
                OTHER_PRINCIPAL,
                Permissions::SUBMIT_RNS_DATA,
                CredentialStatus::Pending,
                audit(2, 3, 7),
                [0xb2; 32],
            ),
        ],
    );
    let pending = current
        .pending_credential()
        .unwrap_or_else(|_| panic!("single pending rejected"))
        .unwrap_or_else(|| panic!("pending omitted"));
    let plan = current
        .plan_abort_pending(pending)
        .unwrap_or_else(|fault| panic!("abort rejected: {:?}", fault.kind()));
    assert_eq!(plan.transition(), PairingLifecycleTransition::AbortPending);
    let store_candidate = plan.into_store_candidate();
    assert_eq!(
        store_candidate.transition(),
        PairingLifecycleTransition::AbortPending
    );
    assert_eq!(store_candidate.revision(), AuthorityRevision::new(4));
    let candidate = store_candidate.into_unpublished_authority_internal();

    let unchanged = candidate
        .find_by_id(id(1))
        .unwrap_or_else(|| panic!("active record disappeared"));
    assert!(unchanged.psk().is_some_and(|psk| *psk == active_psk));
    let tombstone = candidate
        .find_by_id(id(2))
        .unwrap_or_else(|| panic!("aborted tombstone disappeared"));
    assert_eq!(tombstone.status, CredentialStatus::Revoked);
    assert_eq!(tombstone.generation, CredentialGeneration::new(4));
    assert_eq!(tombstone.principal, OTHER_PRINCIPAL);
    assert_eq!(tombstone.permissions, Permissions::NONE);
    assert_eq!(
        tombstone.audit.created_revision(),
        AuthorityRevision::new(2)
    );
    assert_eq!(
        tombstone.audit.modified_revision(),
        AuthorityRevision::new(4)
    );
    assert_eq!(
        tombstone.revocation_reason,
        Some(RevocationReason::PairingAborted)
    );
    assert!(tombstone.psk().is_none());
    assert!(candidate.retains_id(id(2)));
    assert_eq!(candidate.pending_credential(), Ok(None));
}

#[test]
fn pending_transitions_reject_stale_ambiguous_and_exhausted_references() {
    let no_pending = authority::<2>(
        1,
        [with_secret(
            1,
            1,
            PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Active,
        )],
    );
    let reference = PendingCredentialRef::new(id(1), CredentialGeneration::new(1));
    assert_eq!(
        no_pending
            .plan_activate_pending(reference)
            .err()
            .unwrap_or_else(|| panic!("missing pending was activated"))
            .kind(),
        CredentialLifecycleFaultKind::PendingCredentialUnavailable
    );
    assert_eq!(
        no_pending
            .plan_abort_pending(reference)
            .err()
            .unwrap_or_else(|| panic!("missing pending was aborted"))
            .kind(),
        CredentialLifecycleFaultKind::PendingCredentialUnavailable
    );

    let pending = authority::<3>(
        2,
        [CredentialRecord::with_secret(
            id(2),
            CredentialGeneration::new(2),
            OTHER_PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Pending,
            audit(2, 2, 1),
            [0x62; 32],
        )],
    );
    for stale in [
        PendingCredentialRef::new(id(2), CredentialGeneration::new(1)),
        PendingCredentialRef::new(id(3), CredentialGeneration::new(2)),
    ] {
        assert_eq!(
            pending
                .plan_activate_pending(stale)
                .err()
                .unwrap_or_else(|| panic!("stale pending was activated"))
                .kind(),
            CredentialLifecycleFaultKind::PendingCredentialUnavailable
        );
    }

    let multiple = authority::<3>(
        2,
        [
            with_secret(
                1,
                1,
                PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
            ),
            CredentialRecord::with_secret(
                id(2),
                CredentialGeneration::new(2),
                OTHER_PRINCIPAL,
                Permissions::NONE,
                CredentialStatus::Pending,
                audit(2, 2, 1),
                [0x62; 32],
            ),
        ],
    );
    assert_eq!(
        multiple
            .plan_abort_pending(PendingCredentialRef::new(
                id(1),
                CredentialGeneration::new(1),
            ))
            .err()
            .unwrap_or_else(|| panic!("ambiguous pending was aborted"))
            .kind(),
        CredentialLifecycleFaultKind::MultiplePendingCredentials
    );

    let exhausted = authority::<2>(
        u64::MAX,
        [CredentialRecord::with_secret(
            id(2),
            CredentialGeneration::new(u64::MAX),
            OTHER_PRINCIPAL,
            Permissions::NONE,
            CredentialStatus::Pending,
            audit(2, u64::MAX, 1),
            [0x62; 32],
        )],
    );
    let exhausted_ref = exhausted
        .pending_credential()
        .unwrap_or_else(|_| panic!("single exhausted pending rejected"))
        .unwrap_or_else(|| panic!("exhausted pending omitted"));
    assert_eq!(
        exhausted
            .plan_activate_pending(exhausted_ref)
            .err()
            .unwrap_or_else(|| panic!("exhausted pending was activated"))
            .kind(),
        CredentialLifecycleFaultKind::AuthorityRevisionExhausted
    );
}
