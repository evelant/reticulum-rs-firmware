extern crate std;

use core::mem::size_of;

use reticulum_device_api::{Permissions, PrincipalId};

use crate::{
    AuthorityRevision, AuthorityRevisionExhausted, AuthorizationPolicyVersion, CredentialAudit,
    CredentialAuthority, CredentialAuthorityBuilder, CredentialGeneration, CredentialId,
    CredentialLoadFaultKind, CredentialRecord, CredentialSecret, CredentialStatus,
    E290_CREDENTIAL_AUTHORITY_RAM_CEILING, E290_CREDENTIAL_RECORD_CAPACITY, PairingOrigin,
    RevocationReason,
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
        PairingOrigin::UsbPhysicalPresence,
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
    let active_permissions =
        Permissions::READ_SUBMISSION_STATUS | Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA;
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
                PairingOrigin::UsbPhysicalPresence,
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

    let all = Permissions::READ_SUBMISSION_STATUS | Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA;
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
