use core::cmp::Ordering;

use reticulum_device_api::{Permissions, PrincipalId};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    AuthorityRevision, AuthorizationPolicyVersion, CredentialAudit, CredentialAuthority,
    CredentialAuthorityBuilder, CredentialGeneration, CredentialId, CredentialLoadFaultKind,
    CredentialRecord, CredentialStatus, E290_CREDENTIAL_RECORD_CAPACITY, PairingOrigin,
    RevocationReason,
};

/// Canonical byte size of one credential record in the E290 semantic snapshot.
pub const CREDENTIAL_SNAPSHOT_SLOT_SIZE: usize = 128;
/// Canonical byte size of the complete 16-record E290 semantic snapshot.
pub const CREDENTIAL_SNAPSHOT_IMAGE_SIZE: usize =
    E290_CREDENTIAL_RECORD_CAPACITY * CREDENTIAL_SNAPSHOT_SLOT_SIZE;

const ID_RANGE: core::ops::Range<usize> = 0..16;
const GENERATION_RANGE: core::ops::Range<usize> = 16..24;
const PRINCIPAL_RANGE: core::ops::Range<usize> = 24..40;
const PERMISSIONS_RANGE: core::ops::Range<usize> = 40..44;
const STATUS_OFFSET: usize = 44;
const ORIGIN_OFFSET: usize = 45;
const REVOCATION_OFFSET: usize = 46;
const SECRET_FLAG_OFFSET: usize = 47;
const CREATED_REVISION_RANGE: core::ops::Range<usize> = 48..56;
const MODIFIED_REVISION_RANGE: core::ops::Range<usize> = 56..64;
const POLICY_VERSION_RANGE: core::ops::Range<usize> = 64..68;
const RESERVED_WORD_RANGE: core::ops::Range<usize> = 68..72;
const PSK_RANGE: core::ops::Range<usize> = 72..104;
const RESERVED_TAIL_RANGE: core::ops::Range<usize> = 104..128;

/// Exact secret-bearing owner transferred between the codec and raw storage.
///
/// This owner deliberately implements neither `Clone`, `Copy`, nor `Debug`.
/// Its complete 2 KiB allocation is zeroized on drop, including unused slots.
/// Raw storage fills the exact mutable image before transferring it to the
/// decoder; accepting a slice or shorter buffer is deliberately unsupported.
///
/// ```compile_fail
/// use reticulum_device_api_credentials::CredentialSnapshotImage;
/// fn require_clone<T: Clone>() {}
/// require_clone::<CredentialSnapshotImage>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::CredentialSnapshotImage;
/// fn require_copy<T: Copy>() {}
/// require_copy::<CredentialSnapshotImage>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::CredentialSnapshotImage;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<CredentialSnapshotImage>();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::{
///     CREDENTIAL_SNAPSHOT_IMAGE_SIZE, CredentialSnapshotImage,
/// };
/// let truncated = [0_u8; CREDENTIAL_SNAPSHOT_IMAGE_SIZE - 1];
/// let image: CredentialSnapshotImage = truncated.into();
/// ```
///
/// ```compile_fail
/// use reticulum_device_api_credentials::{
///     AuthorityRevision, CredentialSnapshotImage, decode_e290_credential_snapshot,
/// };
/// let image = CredentialSnapshotImage::zeroed();
/// let _decoded = decode_e290_credential_snapshot(AuthorityRevision::new(1), image);
/// let _after_transfer = image.as_bytes();
/// ```
#[must_use = "a secret-bearing snapshot image must be stored, decoded, or explicitly dropped"]
pub struct CredentialSnapshotImage(Zeroizing<[u8; CREDENTIAL_SNAPSHOT_IMAGE_SIZE]>);

impl CredentialSnapshotImage {
    /// Allocate an exact all-zero image for encoding or raw-storage readback.
    pub fn zeroed() -> Self {
        Self(Zeroizing::new([0; CREDENTIAL_SNAPSHOT_IMAGE_SIZE]))
    }

    /// Borrow the exact image for raw-storage programming or comparison.
    pub fn as_bytes(&self) -> &[u8; CREDENTIAL_SNAPSHOT_IMAGE_SIZE] {
        &self.0
    }

    /// Borrow the exact image mutably for a raw-storage read operation.
    pub fn as_mut_bytes(&mut self) -> &mut [u8; CREDENTIAL_SNAPSHOT_IMAGE_SIZE] {
        &mut self.0
    }
}

impl Zeroize for CredentialSnapshotImage {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

const _: () =
    assert!(core::mem::size_of::<CredentialSnapshotImage>() == CREDENTIAL_SNAPSHOT_IMAGE_SIZE);
const _: () = assert!(E290_CREDENTIAL_RECORD_CAPACITY == 16);
const _: () = assert!(CREDENTIAL_SNAPSHOT_IMAGE_SIZE == 2_048);

/// Non-secret reason a canonical E290 credential image could not be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialSnapshotDecodeFaultKind {
    /// Used slots were not strictly ordered by credential ID.
    CredentialIdOrder,
    /// A nonzero slot followed the first canonical all-zero unused slot.
    NonCanonicalUnusedSlot,
    /// The status discriminator is outside the canonical vocabulary.
    InvalidStatus,
    /// The pairing-origin discriminator is outside the canonical vocabulary.
    InvalidPairingOrigin,
    /// The revocation discriminator is outside the canonical vocabulary.
    InvalidRevocationReason,
    /// The secret-presence flag is neither zero nor one.
    InvalidSecretFlag,
    /// A reserved byte was not zero.
    NonZeroReserved,
    /// Permission bits outside the stable device-API vocabulary were present.
    UnknownPermissionBits,
    /// Status, reason, permissions, flag, and PSK presence were inconsistent.
    InvalidStatusShape,
    /// The decoded record or authority violated a semantic invariant.
    Authority(CredentialLoadFaultKind),
}

/// Encode one validated E290 authority as a canonical secret-bearing image.
///
/// Records are ordered strictly by credential ID, independent of builder
/// insertion order. Unused slots and every reserved byte remain zero.
pub fn encode_e290_credential_snapshot(
    authority: &CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>,
) -> CredentialSnapshotImage {
    let mut image = CredentialSnapshotImage::zeroed();
    let mut indices = [0_usize; E290_CREDENTIAL_RECORD_CAPACITY];
    let mut count = 0_usize;

    for (index, record) in authority.records.iter().enumerate() {
        if record.is_some() {
            indices[count] = index;
            count += 1;
        }
    }

    for position in 0..count {
        let mut least = position;
        for candidate in position + 1..count {
            let candidate_id = authority.records[indices[candidate]]
                .as_ref()
                .expect("an occupied credential index was collected")
                .id
                .as_bytes();
            let least_id = authority.records[indices[least]]
                .as_ref()
                .expect("an occupied credential index was collected")
                .id
                .as_bytes();
            if candidate_id.cmp(least_id) == Ordering::Less {
                least = candidate;
            }
        }
        indices.swap(position, least);
    }

    for (slot_index, record_index) in indices[..count].iter().copied().enumerate() {
        let record = authority.records[record_index]
            .as_ref()
            .expect("an occupied credential index was collected");
        let start = slot_index * CREDENTIAL_SNAPSHOT_SLOT_SIZE;
        encode_record(
            record,
            &mut image.as_mut_bytes()[start..start + CREDENTIAL_SNAPSHOT_SLOT_SIZE],
        );
    }

    image
}

/// Consume and decode one exact canonical E290 secret-bearing image.
///
/// `revision` comes from the validated outer-store header. The image is
/// zeroized on every return path. Decoded records are revalidated through the
/// authority builder before the authority can be returned.
pub fn decode_e290_credential_snapshot(
    revision: AuthorityRevision,
    image: CredentialSnapshotImage,
) -> Result<CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>, CredentialSnapshotDecodeFaultKind>
{
    let mut builder = CredentialAuthorityBuilder::new(revision)
        .map_err(|fault| CredentialSnapshotDecodeFaultKind::Authority(fault.kind()))?;
    let mut previous_id: Option<CredentialId> = None;
    let mut reached_unused = false;

    for slot in image.as_bytes().chunks_exact(CREDENTIAL_SNAPSHOT_SLOT_SIZE) {
        if is_all_zero(slot) {
            reached_unused = true;
            continue;
        }
        if reached_unused {
            return Err(CredentialSnapshotDecodeFaultKind::NonCanonicalUnusedSlot);
        }

        let record = decode_record(slot)?;
        if let Some(previous) = previous_id
            && previous.as_bytes().cmp(record.id.as_bytes()) != Ordering::Less
        {
            return Err(CredentialSnapshotDecodeFaultKind::CredentialIdOrder);
        }
        previous_id = Some(record.id);
        builder = match builder.insert(record) {
            Ok(builder) => builder,
            Err(fault) => {
                return Err(CredentialSnapshotDecodeFaultKind::Authority(fault.kind()));
            }
        };
    }

    Ok(builder.finish())
}

fn encode_record(record: &CredentialRecord, slot: &mut [u8]) {
    slot[ID_RANGE].copy_from_slice(record.id.as_bytes());
    put_u64(&mut slot[GENERATION_RANGE], record.generation.get());
    slot[PRINCIPAL_RANGE].copy_from_slice(&record.principal.0);
    put_u32(&mut slot[PERMISSIONS_RANGE], record.permissions.bits());
    slot[STATUS_OFFSET] = match record.status {
        CredentialStatus::Pending => 1,
        CredentialStatus::Active => 2,
        CredentialStatus::Revoked => 3,
    };
    slot[ORIGIN_OFFSET] = match record.audit.pairing_origin {
        PairingOrigin::LocalPhysicalPresence => 1,
    };
    slot[REVOCATION_OFFSET] = match record.revocation_reason {
        None => 0,
        Some(RevocationReason::Explicit) => 1,
        Some(RevocationReason::PairingAborted) => 2,
    };
    if let Some(psk) = record.psk() {
        slot[SECRET_FLAG_OFFSET] = 1;
        slot[PSK_RANGE].copy_from_slice(psk);
    }
    put_u64(
        &mut slot[CREATED_REVISION_RANGE],
        record.audit.created_revision.get(),
    );
    put_u64(
        &mut slot[MODIFIED_REVISION_RANGE],
        record.audit.modified_revision.get(),
    );
    put_u32(
        &mut slot[POLICY_VERSION_RANGE],
        record.audit.policy_version.get(),
    );
}

fn decode_record(slot: &[u8]) -> Result<CredentialRecord, CredentialSnapshotDecodeFaultKind> {
    if !is_all_zero(&slot[RESERVED_WORD_RANGE]) || !is_all_zero(&slot[RESERVED_TAIL_RANGE]) {
        return Err(CredentialSnapshotDecodeFaultKind::NonZeroReserved);
    }

    let id = CredentialId::new(copy_array(&slot[ID_RANGE]));
    let generation = CredentialGeneration::new(read_u64(&slot[GENERATION_RANGE]));
    let principal = PrincipalId(copy_array(&slot[PRINCIPAL_RANGE]));
    let permissions = Permissions::from_bits(read_u32(&slot[PERMISSIONS_RANGE]))
        .map_err(|_| CredentialSnapshotDecodeFaultKind::UnknownPermissionBits)?;
    let status = match slot[STATUS_OFFSET] {
        1 => CredentialStatus::Pending,
        2 => CredentialStatus::Active,
        3 => CredentialStatus::Revoked,
        _ => return Err(CredentialSnapshotDecodeFaultKind::InvalidStatus),
    };
    let origin = match slot[ORIGIN_OFFSET] {
        1 => PairingOrigin::LocalPhysicalPresence,
        _ => return Err(CredentialSnapshotDecodeFaultKind::InvalidPairingOrigin),
    };
    let revocation_reason = match slot[REVOCATION_OFFSET] {
        0 => None,
        1 => Some(RevocationReason::Explicit),
        2 => Some(RevocationReason::PairingAborted),
        _ => return Err(CredentialSnapshotDecodeFaultKind::InvalidRevocationReason),
    };
    let secret_present = match slot[SECRET_FLAG_OFFSET] {
        0 => false,
        1 => true,
        _ => return Err(CredentialSnapshotDecodeFaultKind::InvalidSecretFlag),
    };
    let audit = CredentialAudit::new(
        AuthorityRevision::new(read_u64(&slot[CREATED_REVISION_RANGE])),
        AuthorityRevision::new(read_u64(&slot[MODIFIED_REVISION_RANGE])),
        origin,
        AuthorizationPolicyVersion::new(read_u32(&slot[POLICY_VERSION_RANGE])),
    );

    match status {
        CredentialStatus::Pending | CredentialStatus::Active => {
            if revocation_reason.is_some() || !secret_present {
                return Err(CredentialSnapshotDecodeFaultKind::InvalidStatusShape);
            }
            let psk = Zeroizing::new(copy_array(&slot[PSK_RANGE]));
            Ok(CredentialRecord::with_zeroizing_secret(
                id,
                generation,
                principal,
                permissions,
                status,
                audit,
                psk,
            ))
        }
        CredentialStatus::Revoked => {
            let Some(reason) = revocation_reason else {
                return Err(CredentialSnapshotDecodeFaultKind::InvalidStatusShape);
            };
            if secret_present || !is_all_zero(&slot[PSK_RANGE]) || permissions != Permissions::NONE
            {
                return Err(CredentialSnapshotDecodeFaultKind::InvalidStatusShape);
            }
            Ok(CredentialRecord::revoked(
                id, generation, principal, audit, reason,
            ))
        }
    }
}

fn put_u32(bytes: &mut [u8], value: u32) {
    bytes.copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], value: u64) {
    bytes.copy_from_slice(&value.to_le_bytes());
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(copy_array(bytes))
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(copy_array(bytes))
}

fn copy_array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    bytes
        .try_into()
        .expect("a fixed credential slot range has its declared length")
}

fn is_all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::mem::{needs_drop, size_of};

    use super::*;

    fn active_permissions() -> Permissions {
        Permissions::READ_SUBMISSION_STATUS | Permissions::SUBMIT_RNS_DATA
    }

    fn audit(created: u64, modified: u64, origin: PairingOrigin, policy: u32) -> CredentialAudit {
        CredentialAudit::new(
            AuthorityRevision::new(created),
            AuthorityRevision::new(modified),
            origin,
            AuthorizationPolicyVersion::new(policy),
        )
    }

    fn record_with_secret(
        id_byte: u8,
        principal_byte: u8,
        permissions: Permissions,
        status: CredentialStatus,
        origin: PairingOrigin,
        policy: u32,
        psk_byte: u8,
    ) -> CredentialRecord {
        let generation = u64::from(id_byte);
        CredentialRecord::with_secret(
            CredentialId::new([id_byte; 16]),
            CredentialGeneration::new(generation),
            PrincipalId([principal_byte; 16]),
            permissions,
            status,
            audit(generation, generation, origin, policy),
            [psk_byte; 32],
        )
    }

    fn representative_authority() -> CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY> {
        let builder = CredentialAuthorityBuilder::new(AuthorityRevision::new(3))
            .unwrap_or_else(|fault| panic!("valid revision rejected: {:?}", fault.kind()));
        let builder = builder
            .insert(CredentialRecord::revoked(
                CredentialId::new([3; 16]),
                CredentialGeneration::new(3),
                PrincipalId([0x33; 16]),
                audit(3, 3, PairingOrigin::LocalPhysicalPresence, 7),
                RevocationReason::Explicit,
            ))
            .unwrap_or_else(|fault| panic!("valid tombstone rejected: {:?}", fault.kind()));
        let builder = builder
            .insert(record_with_secret(
                1,
                0x11,
                active_permissions(),
                CredentialStatus::Active,
                PairingOrigin::LocalPhysicalPresence,
                9,
                0xa1,
            ))
            .unwrap_or_else(|fault| panic!("valid active record rejected: {:?}", fault.kind()));
        builder
            .insert(record_with_secret(
                2,
                0x22,
                Permissions::READ_SUBMISSION_STATUS,
                CredentialStatus::Pending,
                PairingOrigin::LocalPhysicalPresence,
                8,
                0xb2,
            ))
            .unwrap_or_else(|fault| panic!("valid pending record rejected: {:?}", fault.kind()))
            .finish()
    }

    fn active_authority(
        record_count: usize,
    ) -> CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY> {
        let revision = u64::try_from(record_count).expect("test count fits in u64");
        let mut builder = CredentialAuthorityBuilder::new(AuthorityRevision::new(revision))
            .unwrap_or_else(|fault| panic!("valid revision rejected: {:?}", fault.kind()));
        for index in 0..record_count {
            let value = u8::try_from(index + 1).expect("test index fits in u8");
            builder = builder
                .insert(record_with_secret(
                    value,
                    0x40_u8.wrapping_add(value),
                    Permissions::READ_SUBMISSION_STATUS,
                    CredentialStatus::Active,
                    PairingOrigin::LocalPhysicalPresence,
                    1,
                    0x60_u8.wrapping_add(value),
                ))
                .unwrap_or_else(|fault| panic!("valid active record rejected: {:?}", fault.kind()));
        }
        builder.finish()
    }

    fn fault_for(
        revision: u64,
        image: CredentialSnapshotImage,
    ) -> CredentialSnapshotDecodeFaultKind {
        match decode_e290_credential_snapshot(AuthorityRevision::new(revision), image) {
            Ok(_) => panic!("noncanonical snapshot was accepted"),
            Err(fault) => fault,
        }
    }

    #[test]
    fn golden_image_is_strictly_id_sorted_with_exact_slot_layout() {
        let image = encode_e290_credential_snapshot(&representative_authority());
        let mut expected = [0_u8; CREDENTIAL_SNAPSHOT_IMAGE_SIZE];

        expected[0..16].fill(1);
        expected[16..24].copy_from_slice(&1_u64.to_le_bytes());
        expected[24..40].fill(0x11);
        expected[40..44].copy_from_slice(&active_permissions().bits().to_le_bytes());
        expected[44] = 2;
        expected[45] = 1;
        expected[46] = 0;
        expected[47] = 1;
        expected[48..56].copy_from_slice(&1_u64.to_le_bytes());
        expected[56..64].copy_from_slice(&1_u64.to_le_bytes());
        expected[64..68].copy_from_slice(&9_u32.to_le_bytes());
        expected[72..104].fill(0xa1);

        let pending = CREDENTIAL_SNAPSHOT_SLOT_SIZE;
        expected[pending..pending + 16].fill(2);
        expected[pending + 16..pending + 24].copy_from_slice(&2_u64.to_le_bytes());
        expected[pending + 24..pending + 40].fill(0x22);
        expected[pending + 40..pending + 44]
            .copy_from_slice(&Permissions::READ_SUBMISSION_STATUS.bits().to_le_bytes());
        expected[pending + 44] = 1;
        expected[pending + 45] = 1;
        expected[pending + 46] = 0;
        expected[pending + 47] = 1;
        expected[pending + 48..pending + 56].copy_from_slice(&2_u64.to_le_bytes());
        expected[pending + 56..pending + 64].copy_from_slice(&2_u64.to_le_bytes());
        expected[pending + 64..pending + 68].copy_from_slice(&8_u32.to_le_bytes());
        expected[pending + 72..pending + 104].fill(0xb2);

        let revoked = 2 * CREDENTIAL_SNAPSHOT_SLOT_SIZE;
        expected[revoked..revoked + 16].fill(3);
        expected[revoked + 16..revoked + 24].copy_from_slice(&3_u64.to_le_bytes());
        expected[revoked + 24..revoked + 40].fill(0x33);
        expected[revoked + 44] = 3;
        expected[revoked + 45] = 1;
        expected[revoked + 46] = 1;
        expected[revoked + 47] = 0;
        expected[revoked + 48..revoked + 56].copy_from_slice(&3_u64.to_le_bytes());
        expected[revoked + 56..revoked + 64].copy_from_slice(&3_u64.to_le_bytes());
        expected[revoked + 64..revoked + 68].copy_from_slice(&7_u32.to_le_bytes());

        assert_eq!(image.as_bytes(), &expected);
    }

    #[test]
    fn complete_authority_round_trips_to_the_same_canonical_image() {
        let original = representative_authority();
        let expected = encode_e290_credential_snapshot(&original);
        let decoded = decode_e290_credential_snapshot(
            AuthorityRevision::new(3),
            encode_e290_credential_snapshot(&original),
        )
        .unwrap_or_else(|fault| panic!("canonical image rejected: {fault:?}"));

        assert_eq!(decoded.revision(), AuthorityRevision::new(3));
        assert_eq!(decoded.record_count(), 3);
        assert_eq!(decoded.active_count(), 1);
        assert!(
            decoded
                .select_for_handshake(CredentialId::new([1; 16]))
                .is_ok()
        );
        assert!(
            decoded
                .select_for_handshake(CredentialId::new([2; 16]))
                .is_err()
        );
        assert!(
            decoded
                .select_for_handshake(CredentialId::new([3; 16]))
                .is_err()
        );
        assert_eq!(
            encode_e290_credential_snapshot(&decoded).as_bytes(),
            expected.as_bytes()
        );
    }

    #[test]
    fn decoder_rejects_noncanonical_discriminators_reserved_bytes_and_shapes() {
        macro_rules! rejects_active_mutation {
            ($offset:expr, $value:expr, $expected:expr) => {{
                let authority = active_authority(1);
                let mut image = encode_e290_credential_snapshot(&authority);
                image.as_mut_bytes()[$offset] = $value;
                assert_eq!(fault_for(1, image), $expected);
            }};
        }

        rejects_active_mutation!(44, 0, CredentialSnapshotDecodeFaultKind::InvalidStatus);
        rejects_active_mutation!(
            45,
            0,
            CredentialSnapshotDecodeFaultKind::InvalidPairingOrigin
        );
        rejects_active_mutation!(
            46,
            5,
            CredentialSnapshotDecodeFaultKind::InvalidRevocationReason
        );
        rejects_active_mutation!(47, 2, CredentialSnapshotDecodeFaultKind::InvalidSecretFlag);
        rejects_active_mutation!(68, 1, CredentialSnapshotDecodeFaultKind::NonZeroReserved);
        rejects_active_mutation!(104, 1, CredentialSnapshotDecodeFaultKind::NonZeroReserved);

        let authority = active_authority(1);
        let mut image = encode_e290_credential_snapshot(&authority);
        image.as_mut_bytes()[40..44].copy_from_slice(&(1_u32 << 31).to_le_bytes());
        assert_eq!(
            fault_for(1, image),
            CredentialSnapshotDecodeFaultKind::UnknownPermissionBits
        );

        rejects_active_mutation!(46, 1, CredentialSnapshotDecodeFaultKind::InvalidStatusShape);
        rejects_active_mutation!(47, 0, CredentialSnapshotDecodeFaultKind::InvalidStatusShape);

        let authority = representative_authority();
        let mut revoked_with_permissions = encode_e290_credential_snapshot(&authority);
        let permissions = 2 * CREDENTIAL_SNAPSHOT_SLOT_SIZE + 40;
        revoked_with_permissions.as_mut_bytes()[permissions..permissions + 4]
            .copy_from_slice(&Permissions::READ_SUBMISSION_STATUS.bits().to_le_bytes());
        assert_eq!(
            fault_for(3, revoked_with_permissions),
            CredentialSnapshotDecodeFaultKind::InvalidStatusShape
        );

        let mut revoked_with_secret = encode_e290_credential_snapshot(&authority);
        let flag = 2 * CREDENTIAL_SNAPSHOT_SLOT_SIZE + SECRET_FLAG_OFFSET;
        revoked_with_secret.as_mut_bytes()[flag] = 1;
        assert_eq!(
            fault_for(3, revoked_with_secret),
            CredentialSnapshotDecodeFaultKind::InvalidStatusShape
        );
    }

    #[test]
    fn decoder_rejects_noncanonical_order_and_nonzero_slots_after_erased_state() {
        let authority = active_authority(2);
        let mut reversed = encode_e290_credential_snapshot(&authority);
        for offset in 0..CREDENTIAL_SNAPSHOT_SLOT_SIZE {
            reversed
                .as_mut_bytes()
                .swap(offset, CREDENTIAL_SNAPSHOT_SLOT_SIZE + offset);
        }
        assert_eq!(
            fault_for(2, reversed),
            CredentialSnapshotDecodeFaultKind::CredentialIdOrder
        );

        let authority = active_authority(1);
        let mut gap = encode_e290_credential_snapshot(&authority);
        gap.as_mut_bytes()[2 * CREDENTIAL_SNAPSHOT_SLOT_SIZE] = 1;
        assert_eq!(
            fault_for(1, gap),
            CredentialSnapshotDecodeFaultKind::NonCanonicalUnusedSlot
        );
    }

    #[test]
    fn decoder_revalidates_semantics_and_exposes_only_the_fault_kind() {
        let authority = active_authority(1);
        let mut zero_generation = encode_e290_credential_snapshot(&authority);
        zero_generation.as_mut_bytes()[GENERATION_RANGE].fill(0);
        assert_eq!(
            fault_for(1, zero_generation),
            CredentialSnapshotDecodeFaultKind::Authority(CredentialLoadFaultKind::ZeroGeneration)
        );

        let mut zero_psk = encode_e290_credential_snapshot(&authority);
        zero_psk.as_mut_bytes()[PSK_RANGE].fill(0);
        assert_eq!(
            fault_for(1, zero_psk),
            CredentialSnapshotDecodeFaultKind::Authority(CredentialLoadFaultKind::ZeroPsk)
        );

        let mut future_generation = encode_e290_credential_snapshot(&authority);
        future_generation.as_mut_bytes()[GENERATION_RANGE].copy_from_slice(&2_u64.to_le_bytes());
        future_generation.as_mut_bytes()[MODIFIED_REVISION_RANGE]
            .copy_from_slice(&2_u64.to_le_bytes());
        assert_eq!(
            fault_for(1, future_generation),
            CredentialSnapshotDecodeFaultKind::Authority(
                CredentialLoadFaultKind::InvalidRevisionOrder
            )
        );
    }

    #[test]
    fn image_is_exact_owned_mutable_and_explicitly_zeroizable() {
        assert_eq!(size_of::<CredentialSnapshotImage>(), 2_048);
        assert!(needs_drop::<CredentialSnapshotImage>());

        let mut image = CredentialSnapshotImage::zeroed();
        image.as_mut_bytes().fill(0xa5);
        image.zeroize();
        assert!(is_all_zero(image.as_bytes()));
    }
}
