extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_device_api_credentials::{
    AuthorityRevision, AuthorizationPolicyVersion, CredentialAuthority, CredentialGeneration,
    CredentialId, CredentialSnapshotImage, NewPendingCredential, PairingLifecycleTransition,
    PairingOrigin, PendingCredentialRef, Permissions, PrincipalId, decode_e290_credential_snapshot,
};
use std::vec;
use std::vec::Vec;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Injected,
    Bounds,
    Alignment,
    IllegalProgramming,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            _ => NorFlashErrorKind::Other,
        }
    }
}

#[derive(Clone, Copy)]
enum FaultKind {
    Partial(usize),
    LostReply,
}

#[derive(Clone)]
struct FakeNor {
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
    erases: usize,
    fail_write: Option<(usize, FaultKind)>,
    fail_erase: Option<(usize, FaultKind)>,
    fail_read: Option<usize>,
    successful_noop_erase: bool,
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
            reads: 0,
            writes: 0,
            erases: 0,
            fail_write: None,
            fail_erase: None,
            fail_read: None,
            successful_noop_erase: false,
        }
    }

    fn reset_counts(&mut self) {
        self.reads = 0;
        self.writes = 0;
        self.erases = 0;
        self.fail_write = None;
        self.fail_erase = None;
        self.fail_read = None;
        self.successful_noop_erase = false;
    }

    fn fail_write_after(&mut self, call: usize, kind: FaultKind) {
        self.fail_write = Some((call, kind));
    }

    fn fail_erase_after(&mut self, call: usize, kind: FaultKind) {
        self.fail_erase = Some((call, kind));
    }

    fn fail_read_after(&mut self, call: usize) {
        self.fail_read = Some(call);
    }

    fn range(&self, offset: u32, len: usize) -> Result<core::ops::Range<usize>, FakeError> {
        let start = usize::try_from(offset).map_err(|_| FakeError::Bounds)?;
        let end = start.checked_add(len).ok_or(FakeError::Bounds)?;
        if end > self.bytes.len() {
            return Err(FakeError::Bounds);
        }
        Ok(start..end)
    }

    fn program(&mut self, offset: usize, bytes: &[u8]) -> Result<(), FakeError> {
        let target = &mut self.bytes[offset..offset + bytes.len()];
        if target
            .iter()
            .zip(bytes)
            .any(|(current, next)| current & next != *next)
        {
            return Err(FakeError::IllegalProgramming);
        }
        for (current, next) in target.iter_mut().zip(bytes) {
            *current &= *next;
        }
        Ok(())
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check)?;
        let range = self.range(offset, bytes.len())?;
        let call = self.reads;
        self.reads += 1;
        if self.fail_read == Some(call) {
            self.fail_read = None;
            return Err(FakeError::Injected);
        }
        bytes.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = SECTOR_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check)?;
        let range = self.range(offset, bytes.len())?;
        let call = self.writes;
        self.writes += 1;
        let fault = self
            .fail_write
            .filter(|(fault_call, _)| *fault_call == call)
            .map(|(_, kind)| kind);
        match fault {
            Some(FaultKind::Partial(cut)) => {
                let cut = cut.min(bytes.len());
                self.program(range.start, &bytes[..cut])?;
                self.fail_write = None;
                Err(FakeError::Injected)
            }
            Some(FaultKind::LostReply) => {
                self.program(range.start, bytes)?;
                self.fail_write = None;
                Err(FakeError::Injected)
            }
            None => self.program(range.start, bytes),
        }
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check)?;
        let len = usize::try_from(to - from).map_err(|_| FakeError::Bounds)?;
        let range = self.range(from, len)?;
        let call = self.erases;
        self.erases += 1;
        let fault = self
            .fail_erase
            .filter(|(fault_call, _)| *fault_call == call)
            .map(|(_, kind)| kind);
        if self.successful_noop_erase {
            return Ok(());
        }
        match fault {
            Some(FaultKind::Partial(cut)) => {
                let cut = cut.min(range.len());
                self.bytes[range.start..range.start + cut].fill(0xff);
                self.fail_erase = None;
                Err(FakeError::Injected)
            }
            Some(FaultKind::LostReply) => {
                self.bytes[range].fill(0xff);
                self.fail_erase = None;
                Err(FakeError::Injected)
            }
            None => {
                self.bytes[range].fill(0xff);
                Ok(())
            }
        }
    }
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check(kind: NorFlashErrorKind) -> FakeError {
    match kind {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

const DEVICE: CredentialStoreDeviceId = CredentialStoreDeviceId::new([0x51; 16]);
const ABSOLUTE_OFFSET: usize = 0x52_0000;

const fn binding() -> CredentialStoreBinding {
    CredentialStoreBinding::new(
        DEVICE,
        ABSOLUTE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn bound(flash: FakeNor) -> BoundCredentialStore<FakeNor> {
    BoundCredentialStore::new(flash, binding())
}

struct ReadOnlyBound<'flash> {
    backend: &'flash mut FakeNor,
    binding: CredentialStoreBinding,
}

impl ErrorType for ReadOnlyBound<'_> {
    type Error = FakeError;
}

impl ReadNorFlash for ReadOnlyBound<'_> {
    const READ_SIZE: usize = FakeNor::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.backend.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.backend.capacity()
    }
}

impl BoundCredentialStoreReadAccess for ReadOnlyBound<'_> {
    fn credential_store_binding(&self) -> CredentialStoreBinding {
        self.binding
    }
}

fn classify_read_only(
    access: &mut BoundCredentialStore<FakeNor>,
) -> Result<EmptyProvisionMediaClassification, CredentialStoreMountError<FakeError>> {
    let binding = access.binding();
    let mut read_only = ReadOnlyBound {
        backend: access.backend_mut(),
        binding,
    };
    classify_empty_provision_media(&mut read_only)
}

fn authority(revision: u64, created_revision: u64) -> CredentialAuthority<16> {
    authority_with_status(revision, created_revision, 2)
}

fn pending_authority(revision: u64, created_revision: u64) -> CredentialAuthority<16> {
    authority_with_status(revision, created_revision, 1)
}

fn new_pending(id_byte: u8, psk: [u8; 32]) -> NewPendingCredential {
    NewPendingCredential::new(
        CredentialId::new([id_byte; 16]),
        PrincipalId([0x61; 16]),
        Permissions::READ_SUBMISSION_STATUS,
        PairingOrigin::LocalPhysicalPresence,
        AuthorizationPolicyVersion::new(1),
        Zeroizing::new(psk),
    )
}

fn authority_with_status(
    revision: u64,
    created_revision: u64,
    status: u8,
) -> CredentialAuthority<16> {
    authority_with_status_and_psk(
        revision,
        created_revision,
        status,
        0x40_u8.wrapping_add(revision as u8),
    )
}

fn authority_with_status_and_psk(
    revision: u64,
    created_revision: u64,
    status: u8,
    psk_byte: u8,
) -> CredentialAuthority<16> {
    let mut image = CredentialSnapshotImage::zeroed();
    let slot = &mut image.as_mut_bytes()[..CREDENTIAL_SNAPSHOT_SLOT_SIZE];
    slot[..16].fill(0x21);
    slot[16..24].copy_from_slice(&revision.to_le_bytes());
    slot[24..40].fill(0x31);
    slot[40..44].copy_from_slice(&0_u32.to_le_bytes());
    slot[44] = status;
    slot[45] = 1;
    slot[47] = 1;
    slot[48..56].copy_from_slice(&created_revision.to_le_bytes());
    slot[56..64].copy_from_slice(&revision.to_le_bytes());
    slot[64..68].copy_from_slice(&1_u32.to_le_bytes());
    slot[72..104].fill(psk_byte);
    decode_e290_credential_snapshot(AuthorityRevision::new(revision), image)
        .unwrap_or_else(|fault| panic!("test authority was invalid: {fault:?}"))
}

fn provisioned() -> (MountedCredentialStore, BoundCredentialStore<FakeNor>) {
    let mut access = bound(FakeNor::erased());
    let mounted = provision_empty(&mut access).unwrap_or_else(|_| panic!("provision failed"));
    (mounted, access)
}

fn commit_revision_two() -> (MountedCredentialStore, BoundCredentialStore<FakeNor>) {
    let (mounted, mut access) = provisioned();
    let mounted = commit_successor(mounted, &mut access, authority(2, 2))
        .unwrap_or_else(|_| panic!("successor failed"));
    (mounted, access)
}

fn commit_pending_revision_two() -> (
    MountedCredentialStore,
    BoundCredentialStore<FakeNor>,
    PendingCredentialRef,
) {
    let (mounted, mut access) = provisioned();
    let mounted = commit_successor(mounted, &mut access, pending_authority(2, 2))
        .unwrap_or_else(|_| panic!("pending successor failed"));
    let mounted = recover_once(mounted, &mut access)
        .unwrap_or_else(|_| panic!("pending predecessor cleanup failed"));
    (
        mounted,
        access,
        PendingCredentialRef::new(CredentialId::new([0x21; 16]), CredentialGeneration::new(2)),
    )
}

fn install_snapshot(
    flash: &mut FakeNor,
    sector: CredentialStoreSector,
    authority: &CredentialAuthority<16>,
    predecessor_revision: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
) -> [u8; DIGEST_SIZE] {
    let prefix = encode_snapshot_prefix(
        authority,
        binding(),
        sector,
        predecessor_revision,
        predecessor_digest,
    );
    let digest = snapshot_digest(&prefix);
    let base = sector.offset();
    flash.bytes[base..base + SNAPSHOT_PREFIX_SIZE].copy_from_slice(&prefix[..]);
    flash.bytes[base + COMMIT_DIGEST_OFFSET..base + COMMIT_MARKER_OFFSET].copy_from_slice(&digest);
    flash.bytes[base + COMMIT_MARKER_OFFSET..base + RETIREMENT_MARKER_OFFSET]
        .copy_from_slice(&COMMIT_MARKER);
    digest
}

#[test]
fn physical_layout_and_empty_first_provision_are_exact() {
    assert_eq!(PARTITION_SIZE, 8_192);
    assert_eq!(SECTOR_SIZE, 4_096);
    assert_eq!(SNAPSHOT_IMAGE_OFFSET, 128);
    assert_eq!(SNAPSHOT_DIGEST_OFFSET, 2_176);
    assert_eq!(COMMIT_MARKER_OFFSET, 2_208);
    assert_eq!(RETIREMENT_MARKER_OFFSET, 2_240);
    assert_eq!(ERASED_TAIL_OFFSET, 2_272);

    let (mounted, mut access) = provisioned();
    assert_eq!(mounted.revision(), AuthorityRevision::new(1));
    assert_eq!(
        mounted
            .publishable_authority()
            .expect("clean authority is publishable")
            .record_count(),
        0
    );
    assert_eq!(mounted.active_sector(), CredentialStoreSector::A);
    assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
    assert_eq!(access.backend().writes, 3);
    assert_eq!(access.backend().erases, 0);
    assert_eq!(&access.backend().bytes[..8], MAGIC);
    assert_eq!(
        &access.backend().bytes[COMMIT_MARKER_OFFSET..RETIREMENT_MARKER_OFFSET],
        COMMIT_MARKER
    );
    assert!(
        access.backend().bytes[RETIREMENT_MARKER_OFFSET..]
            .iter()
            .all(|b| *b == 0xff)
    );

    access.backend_mut().reset_counts();
    let remounted = mount(&mut access).unwrap_or_else(|_| panic!("clean mount failed"));
    assert_eq!(remounted.recovery(), CredentialStoreRecovery::Clean);
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);
}

#[test]
fn digest_formula_flushes_secret_buffer_and_matches_external_golden_vector() {
    assert_eq!(DIGEST_DOMAIN.len(), 62);
    assert_eq!(
        (DIGEST_DOMAIN.len() + SNAPSHOT_PREFIX_SIZE) % SHA256_BLOCK_SIZE,
        62
    );
    assert_eq!(DIGEST_FLUSH_TRAILER, [0xa5; SHA256_BLOCK_SIZE]);

    // Independently generated with OpenSSL SHA-256 over the normative domain,
    // canonical empty revision-1 test prefix, and public flush trailer.
    let expected = [
        0x98, 0x29, 0x60, 0xb9, 0x02, 0xf4, 0x15, 0x61, 0x36, 0xcb, 0xb6, 0xd4, 0xc4, 0x65, 0xd3,
        0x90, 0xd9, 0x96, 0x65, 0x65, 0x30, 0x1c, 0x18, 0x27, 0xff, 0x51, 0x7c, 0x36, 0x05, 0x78,
        0xe1, 0xae,
    ];
    let (_mounted, access) = provisioned();
    assert_eq!(
        &access.backend().bytes[COMMIT_DIGEST_OFFSET..COMMIT_MARKER_OFFSET],
        expected
    );
}

#[test]
fn empty_provision_media_classification_is_read_only_and_separates_committed_state() {
    let mut access = bound(FakeNor::erased());
    let before = access.backend().bytes.clone();
    assert_eq!(
        classify_read_only(&mut access).unwrap_or_else(|_| panic!("erased classification failed")),
        EmptyProvisionMediaClassification::ExactlyErased
    );
    assert_eq!(access.backend().bytes, before);
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);

    let _mounted =
        provision_empty(&mut access).unwrap_or_else(|_| panic!("canonical empty provision failed"));
    access.backend_mut().reset_counts();
    let before = access.backend().bytes.clone();
    assert_eq!(
        classify_read_only(&mut access)
            .unwrap_or_else(|_| panic!("committed classification failed")),
        EmptyProvisionMediaClassification::CommittedEmptyRevision1
    );
    assert_eq!(access.backend().bytes, before);
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);
    let binding = access.binding();
    let mounted = {
        let mut read_only = ReadOnlyBound {
            backend: access.backend_mut(),
            binding,
        };
        mount(&mut read_only).unwrap_or_else(|_| panic!("read-only mount failed"))
    };
    assert_eq!(mounted.revision(), AuthorityRevision::new(1));
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);

    let mut access = bound(FakeNor::erased());
    access.backend_mut().fail_read_after(0);
    assert!(matches!(
        classify_read_only(&mut access),
        Err(CredentialStoreMountError::Backend(FakeError::Injected))
    ));
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);

    let mut backend = FakeNor::erased();
    let mut read_only = ReadOnlyBound {
        backend: &mut backend,
        binding: CredentialStoreBinding::new(
            DEVICE,
            ABSOLUTE_OFFSET + 1,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
    };
    assert!(matches!(
        classify_empty_provision_media(&mut read_only),
        Err(CredentialStoreMountError::Binding(
            CredentialStoreBindingError::ReadAlignmentMismatch { .. }
        ))
    ));
    assert_eq!((backend.reads, backend.writes, backend.erases), (0, 0, 0));
}

#[test]
fn first_provision_requires_every_byte_erased_and_never_erases() {
    for offset in [
        0,
        SNAPSHOT_IMAGE_OFFSET,
        COMMIT_MARKER_OFFSET,
        PARTITION_SIZE - 1,
    ] {
        let mut flash = FakeNor::erased();
        flash.bytes[offset] = 0xfe;
        let mut access = bound(flash);
        assert!(matches!(
            provision_empty(&mut access),
            Err(CredentialStoreMountError::Fault(
                CredentialStoreFault::UnformattedNonErased
            ))
        ));
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }
}

#[test]
fn every_first_provision_byte_cut_and_lost_reply_resumes_without_erase() {
    for (write_call, length) in [
        (0, SNAPSHOT_PREFIX_SIZE),
        (1, DIGEST_SIZE),
        (2, MARKER_SIZE),
    ] {
        for cut in 0..=length {
            let mut access = bound(FakeNor::erased());
            access
                .backend_mut()
                .fail_write_after(write_call, FaultKind::Partial(cut));
            assert!(provision_empty(&mut access).is_err());
            access.backend_mut().reset_counts();
            let before = access.backend().bytes.clone();
            let expected = if write_call == 0 && cut == 0 {
                EmptyProvisionMediaClassification::ExactlyErased
            } else if write_call == 2 && cut == MARKER_SIZE {
                EmptyProvisionMediaClassification::CommittedEmptyRevision1
            } else {
                EmptyProvisionMediaClassification::RecoverableInterrupted
            };
            assert_eq!(
                classify_read_only(&mut access).unwrap_or_else(|_| {
                    panic!("first provision write {write_call} cut {cut} did not classify")
                }),
                expected,
                "first provision write {write_call} cut {cut} misclassified"
            );
            assert_eq!(access.backend().bytes, before);
            assert_eq!(access.backend().writes, 0);
            assert_eq!(access.backend().erases, 0);
            let mounted = recover_empty_provision(&mut access).unwrap_or_else(|_| {
                panic!("first provision write {write_call} cut {cut} did not resume")
            });
            assert_eq!(mounted.revision(), AuthorityRevision::new(1));
            assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
            assert_eq!(access.backend().erases, 0);
        }

        let mut access = bound(FakeNor::erased());
        access
            .backend_mut()
            .fail_write_after(write_call, FaultKind::LostReply);
        assert!(provision_empty(&mut access).is_err());
        access.backend_mut().reset_counts();
        let before = access.backend().bytes.clone();
        let expected = if write_call == 2 {
            EmptyProvisionMediaClassification::CommittedEmptyRevision1
        } else {
            EmptyProvisionMediaClassification::RecoverableInterrupted
        };
        assert_eq!(
            classify_read_only(&mut access).unwrap_or_else(|_| {
                panic!("first provision write {write_call} lost reply did not classify")
            }),
            expected
        );
        assert_eq!(access.backend().bytes, before);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
        let mounted = recover_empty_provision(&mut access)
            .unwrap_or_else(|_| panic!("first provision write {write_call} lost reply failed"));
        assert_eq!(mounted.revision(), AuthorityRevision::new(1));
        assert_eq!(access.backend().erases, 0);
    }
}

#[test]
fn lost_final_empty_commit_reply_is_mounted_without_reprogramming() {
    let mut access = bound(FakeNor::erased());
    access
        .backend_mut()
        .fail_write_after(2, FaultKind::LostReply);
    assert!(provision_empty(&mut access).is_err());

    access.backend_mut().reset_counts();
    assert_eq!(
        classify_read_only(&mut access)
            .unwrap_or_else(|_| panic!("complete empty snapshot did not classify")),
        EmptyProvisionMediaClassification::CommittedEmptyRevision1
    );
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);

    let mounted = recover_empty_provision(&mut access)
        .unwrap_or_else(|_| panic!("complete empty snapshot did not mount"));
    assert_eq!(mounted.revision(), AuthorityRevision::new(1));
    assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);
}

#[test]
fn first_provision_recovery_rejects_off_trajectory_media_without_mutation() {
    for offset in [
        0,
        COMMIT_DIGEST_OFFSET,
        COMMIT_MARKER_OFFSET,
        RETIREMENT_MARKER_OFFSET,
        ERASED_TAIL_OFFSET,
        CredentialStoreSector::B.offset(),
        PARTITION_SIZE - 1,
    ] {
        let mut flash = FakeNor::erased();
        flash.bytes[offset] = 0;
        let mut access = bound(flash);
        let before = access.backend().bytes.clone();
        assert_eq!(
            classify_read_only(&mut access)
                .unwrap_or_else(|_| panic!("off-trajectory classification failed")),
            EmptyProvisionMediaClassification::NotRecoverable
        );
        assert_eq!(access.backend().bytes, before);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
        assert!(matches!(
            recover_empty_provision(&mut access),
            Err(CredentialStoreMountError::Fault(
                CredentialStoreFault::UnformattedNonErased
            ))
        ));
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }
}

#[test]
fn empty_provision_classifier_rejects_out_of_order_canonical_stages() {
    let (_mounted, canonical_access) = provisioned();
    let canonical = canonical_access.into_backend().bytes;

    let mut digest_before_prefix = FakeNor::erased();
    digest_before_prefix.bytes[COMMIT_DIGEST_OFFSET] = canonical[COMMIT_DIGEST_OFFSET];

    let mut commit_before_digest = FakeNor::erased();
    commit_before_digest.bytes[..SNAPSHOT_PREFIX_SIZE]
        .copy_from_slice(&canonical[..SNAPSHOT_PREFIX_SIZE]);
    commit_before_digest.bytes[COMMIT_MARKER_OFFSET] = canonical[COMMIT_MARKER_OFFSET];

    for flash in [digest_before_prefix, commit_before_digest] {
        let mut access = bound(flash);
        let before = access.backend().bytes.clone();
        assert_eq!(
            classify_read_only(&mut access)
                .unwrap_or_else(|_| panic!("out-of-order classification failed")),
            EmptyProvisionMediaClassification::NotRecoverable
        );
        assert_eq!(access.backend().bytes, before);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }
}

#[test]
fn binding_shape_is_rejected_before_any_io() {
    let cases = [
        CredentialStoreBinding::new(
            DEVICE,
            ABSOLUTE_OFFSET,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION + 1,
        ),
        CredentialStoreBinding::new(
            DEVICE,
            ABSOLUTE_OFFSET,
            PARTITION_SIZE - SECTOR_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
        CredentialStoreBinding::new(
            DEVICE,
            ABSOLUTE_OFFSET + 1,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
        CredentialStoreBinding::new(
            DEVICE,
            usize::MAX - PARTITION_SIZE + 1,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
    ];
    for invalid in cases {
        let mut access = BoundCredentialStore::new(FakeNor::erased(), invalid);
        assert!(matches!(
            mount(&mut access),
            Err(CredentialStoreMountError::Binding(_))
        ));
        assert_eq!(
            (
                access.backend().reads,
                access.backend().writes,
                access.backend().erases
            ),
            (0, 0, 0)
        );
    }

    let mut short = FakeNor::erased();
    short.bytes.truncate(PARTITION_SIZE - SECTOR_SIZE);
    let mut access = bound(short);
    assert!(matches!(
        mount(&mut access),
        Err(CredentialStoreMountError::Binding(
            CredentialStoreBindingError::CapacityMismatch { .. }
        ))
    ));
    assert_eq!(
        (
            access.backend().reads,
            access.backend().writes,
            access.backend().erases
        ),
        (0, 0, 0)
    );
}

#[test]
fn mounted_operations_reject_wrong_device_range_layout_and_capacity_without_io() {
    let (mounted, access) = provisioned();
    let candidates = [
        CredentialStoreBinding::new(
            CredentialStoreDeviceId::new([0x52; 16]),
            ABSOLUTE_OFFSET,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
        CredentialStoreBinding::new(
            DEVICE,
            ABSOLUTE_OFFSET + SECTOR_SIZE,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION,
        ),
        CredentialStoreBinding::new(
            DEVICE,
            ABSOLUTE_OFFSET,
            PARTITION_SIZE,
            PHYSICAL_FORMAT_VERSION + 1,
        ),
    ];
    let mut current = mounted;
    for invalid in candidates {
        let mut wrong = BoundCredentialStore::new(access.backend().clone(), invalid);
        let error = recover_once(current, &mut wrong).expect_err_owner();
        current = error;
        assert_eq!(
            (
                wrong.backend().reads,
                wrong.backend().writes,
                wrong.backend().erases
            ),
            (
                access.backend().reads,
                access.backend().writes,
                access.backend().erases
            )
        );
    }

    let mut short_backend = access.backend().clone();
    short_backend.bytes.truncate(PARTITION_SIZE - SECTOR_SIZE);
    short_backend.reset_counts();
    let mut short = bound(short_backend);
    current = recover_once(current, &mut short).expect_err_owner();
    assert_eq!(
        (
            short.backend().reads,
            short.backend().writes,
            short.backend().erases
        ),
        (0, 0, 0)
    );
    assert_eq!(current.revision(), AuthorityRevision::new(1));
}

trait RecoveryResultExt {
    fn expect_err_owner(self) -> MountedCredentialStore;
}

impl RecoveryResultExt for Result<MountedCredentialStore, CredentialStoreRecoveryError<FakeError>> {
    fn expect_err_owner(self) -> MountedCredentialStore {
        match self {
            Ok(_) => panic!("operation unexpectedly succeeded"),
            Err(error) => error.into_store(),
        }
    }
}

#[test]
fn header_device_binding_version_and_snapshot_corruption_fail_closed() {
    let (_mounted, access) = provisioned();

    let mut wrong_device = access.backend().clone();
    wrong_device.bytes[64] ^= 0x01;
    let digest = snapshot_digest(
        wrong_device.bytes[..SNAPSHOT_PREFIX_SIZE]
            .try_into()
            .expect("fixed prefix"),
    );
    wrong_device.bytes[COMMIT_DIGEST_OFFSET..COMMIT_MARKER_OFFSET].copy_from_slice(&digest);
    assert!(matches!(
        mount(&mut bound(wrong_device)),
        Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::DeviceBindingMismatch { .. }
        ))
    ));

    for (offset, version, expected) in [
        (
            8,
            PHYSICAL_FORMAT_VERSION + 1,
            CredentialStoreFault::UnsupportedPhysicalVersion(PHYSICAL_FORMAT_VERSION + 1),
        ),
        (
            10,
            SEMANTIC_FORMAT_VERSION - 1,
            CredentialStoreFault::UnsupportedSemanticVersion(SEMANTIC_FORMAT_VERSION - 1),
        ),
        (
            10,
            SEMANTIC_FORMAT_VERSION + 1,
            CredentialStoreFault::UnsupportedSemanticVersion(SEMANTIC_FORMAT_VERSION + 1),
        ),
    ] {
        let mut flash = access.backend().clone();
        flash.bytes[offset..offset + 2].copy_from_slice(&version.to_le_bytes());
        let digest = snapshot_digest(
            flash.bytes[..SNAPSHOT_PREFIX_SIZE]
                .try_into()
                .expect("fixed prefix"),
        );
        flash.bytes[COMMIT_DIGEST_OFFSET..COMMIT_MARKER_OFFSET].copy_from_slice(&digest);
        assert!(matches!(
            mount(&mut bound(flash)),
            Err(CredentialStoreMountError::Fault(actual)) if actual == expected
        ));
    }

    let mut corrupt = access.backend().clone();
    corrupt.bytes[SNAPSHOT_IMAGE_OFFSET] ^= 0x01;
    assert!(matches!(
        mount(&mut bound(corrupt)),
        Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::CommittedSnapshotCorrupt { .. }
        ))
    ));

    let mut high_image_word = access.backend().clone();
    high_image_word.bytes[88..96]
        .copy_from_slice(&(CREDENTIAL_SNAPSHOT_IMAGE_SIZE as u64 + (1_u64 << 32)).to_le_bytes());
    let digest = snapshot_digest(
        high_image_word.bytes[..SNAPSHOT_PREFIX_SIZE]
            .try_into()
            .expect("fixed prefix"),
    );
    high_image_word.bytes[COMMIT_DIGEST_OFFSET..COMMIT_MARKER_OFFSET].copy_from_slice(&digest);
    assert!(matches!(
        mount(&mut bound(high_image_word)),
        Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::CommittedSnapshotCorrupt { .. }
        ))
    ));
}

#[test]
fn successors_alternate_commit_retire_publish_and_cleanup() {
    let (mounted, mut access) = provisioned();
    let revision_two = commit_successor(mounted, &mut access, authority(2, 2))
        .unwrap_or_else(|_| panic!("revision two failed"));
    assert_eq!(revision_two.active_sector(), CredentialStoreSector::B);
    assert_eq!(revision_two.revision(), AuthorityRevision::new(2));
    assert_eq!(
        revision_two
            .publishable_authority()
            .expect("retired predecessor permits publication")
            .record_count(),
        1
    );
    assert_eq!(
        revision_two.recovery(),
        CredentialStoreRecovery::CleanupInactive {
            sector: CredentialStoreSector::A
        }
    );
    assert_eq!(
        &access.backend().bytes[RETIREMENT_MARKER_OFFSET..ERASED_TAIL_OFFSET],
        RETIREMENT_MARKER
    );
    let revision_two =
        recover_once(revision_two, &mut access).unwrap_or_else(|_| panic!("cleanup A failed"));
    assert_eq!(revision_two.recovery(), CredentialStoreRecovery::Clean);

    let revision_three = commit_successor(revision_two, &mut access, authority(3, 2))
        .unwrap_or_else(|_| panic!("revision three failed"));
    assert_eq!(revision_three.active_sector(), CredentialStoreSector::A);
    assert_eq!(revision_three.revision(), AuthorityRevision::new(3));
    let revision_three =
        recover_once(revision_three, &mut access).unwrap_or_else(|_| panic!("cleanup B failed"));
    assert_eq!(revision_three.recovery(), CredentialStoreRecovery::Clean);

    access.backend_mut().reset_counts();
    let remounted = mount(&mut access).unwrap_or_else(|_| panic!("revision three did not mount"));
    assert_eq!(remounted.revision(), AuthorityRevision::new(3));
    assert_eq!(remounted.active_sector(), CredentialStoreSector::A);
    assert_eq!(remounted.recovery(), CredentialStoreRecovery::Clean);
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);
}

#[test]
fn active_plus_torn_inactive_mounts_with_explicit_cleanup() {
    let (_mounted, mut access) = provisioned();
    let base = CredentialStoreSector::B.offset();
    access.backend_mut().bytes[base..base + 64].fill(0x00);
    let mounted = mount(&mut access).unwrap_or_else(|_| panic!("torn peer blocked authority"));
    assert_eq!(
        mounted.recovery(),
        CredentialStoreRecovery::CleanupInactive {
            sector: CredentialStoreSector::B
        }
    );
    let mounted =
        recover_once(mounted, &mut access).unwrap_or_else(|_| panic!("torn peer cleanup failed"));
    assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
}

#[test]
fn two_active_snapshots_require_exact_consecutive_predecessor_link() {
    let (_mounted, access) = provisioned();
    let mut consecutive = access.backend().clone();
    let mut predecessor_digest = [0_u8; DIGEST_SIZE];
    predecessor_digest
        .copy_from_slice(&consecutive.bytes[COMMIT_DIGEST_OFFSET..COMMIT_MARKER_OFFSET]);
    install_snapshot(
        &mut consecutive,
        CredentialStoreSector::B,
        &authority(2, 2),
        1,
        predecessor_digest,
    );
    let mounted =
        mount(&mut bound(consecutive)).unwrap_or_else(|_| panic!("consecutive snapshots rejected"));
    assert_eq!(mounted.revision(), AuthorityRevision::new(2));
    assert!(mounted.publishable_authority().is_none());
    assert_eq!(
        mounted.recovery(),
        CredentialStoreRecovery::RetirePredecessor {
            sector: CredentialStoreSector::A
        }
    );

    let mut gap = access.backend().clone();
    install_snapshot(
        &mut gap,
        CredentialStoreSector::B,
        &authority(3, 3),
        2,
        [0x72; DIGEST_SIZE],
    );
    assert!(matches!(
        mount(&mut bound(gap)),
        Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::SnapshotConflict
        ))
    ));

    let mut wrong_link = access.backend().clone();
    install_snapshot(
        &mut wrong_link,
        CredentialStoreSector::B,
        &authority(2, 2),
        1,
        [0x73; DIGEST_SIZE],
    );
    assert!(matches!(
        mount(&mut bound(wrong_link)),
        Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::SnapshotConflict
        ))
    ));
}

#[test]
fn publication_is_gated_until_mount_retirement_recovery_completes() {
    let (_mounted, access) = provisioned();
    let mut two_active = access.backend().clone();
    let mut predecessor_digest = [0_u8; DIGEST_SIZE];
    predecessor_digest
        .copy_from_slice(&two_active.bytes[COMMIT_DIGEST_OFFSET..COMMIT_MARKER_OFFSET]);
    install_snapshot(
        &mut two_active,
        CredentialStoreSector::B,
        &authority(2, 2),
        1,
        predecessor_digest,
    );
    let mut access = bound(two_active);
    let mounted = mount(&mut access).unwrap_or_else(|_| panic!("two active did not mount"));
    let mounted = match mounted.into_authority() {
        Ok(_) => panic!("authority published before predecessor retirement"),
        Err(mounted) => mounted,
    };
    let mounted =
        recover_once(mounted, &mut access).unwrap_or_else(|_| panic!("retirement recovery failed"));
    assert!(mounted.publishable_authority().is_some());
    assert!(mounted.into_authority().is_ok());
}

#[test]
fn mounted_pending_secret_selection_requires_publishable_authority() {
    let (_mounted, access) = provisioned();
    let mut two_active = access.backend().clone();
    let mut predecessor_digest = [0_u8; DIGEST_SIZE];
    predecessor_digest
        .copy_from_slice(&two_active.bytes[COMMIT_DIGEST_OFFSET..COMMIT_MARKER_OFFSET]);
    install_snapshot(
        &mut two_active,
        CredentialStoreSector::B,
        &pending_authority(2, 2),
        1,
        predecessor_digest,
    );
    let pending =
        PendingCredentialRef::new(CredentialId::new([0x21; 16]), CredentialGeneration::new(2));
    let mut access = bound(two_active);
    let mounted = mount(&mut access).unwrap_or_else(|_| panic!("two active did not mount"));
    assert!(mounted.publishable_authority().is_none());
    assert!(mounted.select_pending_for_proof(pending).is_err());

    let mounted = recover_once(mounted, &mut access)
        .unwrap_or_else(|_| panic!("predecessor retirement failed"));
    assert!(mounted.publishable_authority().is_some());
    let selected = mounted
        .select_pending_for_proof(pending)
        .unwrap_or_else(|_| panic!("publishable pending secret unavailable"));
    let (selected_pending, psk) = selected.into_parts();
    assert_eq!(selected_pending, pending);
    assert_eq!(*psk, [0x42; 32]);
}

#[test]
fn pairing_add_mounted_proof_activate_chain_is_durable_and_typed() {
    let pending =
        PendingCredentialRef::new(CredentialId::new([0x51; 16]), CredentialGeneration::new(2));
    let pending_psk = [0x91; 32];
    let (mounted, mut access) = provisioned();
    let candidate = mounted
        .publishable_authority()
        .unwrap_or_else(|| panic!("freshly provisioned authority was not publishable"))
        .plan_add_pending(new_pending(0x51, pending_psk))
        .unwrap_or_else(|fault| panic!("valid pending enrollment rejected: {:?}", fault.kind()))
        .into_store_candidate();
    let committed = commit_pairing_lifecycle_successor(mounted, &mut access, candidate)
        .unwrap_or_else(|_| panic!("pending enrollment did not commit"));
    assert_eq!(
        committed.transition(),
        PairingLifecycleTransition::AddPending
    );
    assert_eq!(committed.store().revision(), AuthorityRevision::new(2));

    let selected = committed
        .store()
        .select_pending_for_proof(pending)
        .unwrap_or_else(|_| panic!("durably mounted pending proof secret was unavailable"));
    let (selected_pending, selected_psk) = selected.into_parts();
    assert_eq!(selected_pending, pending);
    assert_eq!(*selected_psk, pending_psk);

    let mounted = recover_once(committed.into_store(), &mut access)
        .unwrap_or_else(|_| panic!("post-enrollment cleanup failed"));
    let candidate = mounted
        .publishable_authority()
        .unwrap_or_else(|| panic!("clean pending authority was not publishable"))
        .plan_activate_pending(pending)
        .unwrap_or_else(|fault| panic!("proved pending activation rejected: {:?}", fault.kind()))
        .into_store_candidate();
    let committed = commit_pairing_lifecycle_successor(mounted, &mut access, candidate)
        .unwrap_or_else(|_| panic!("pending activation did not commit"));
    assert_eq!(
        committed.transition(),
        PairingLifecycleTransition::ActivatePending
    );
    assert_eq!(committed.store().revision(), AuthorityRevision::new(3));
    let authority = committed
        .store()
        .publishable_authority()
        .unwrap_or_else(|| panic!("activated authority was not publishable"));
    assert_eq!(authority.active_count(), 1);
    assert_eq!(authority.pending_credential(), Ok(None));
    assert!(committed.store().select_pending_for_proof(pending).is_err());
}

#[test]
fn pairing_add_abort_chain_removes_pending_proof_material() {
    let pending =
        PendingCredentialRef::new(CredentialId::new([0x52; 16]), CredentialGeneration::new(2));
    let (mounted, mut access) = provisioned();
    let candidate = mounted
        .publishable_authority()
        .unwrap_or_else(|| panic!("freshly provisioned authority was not publishable"))
        .plan_add_pending(new_pending(0x52, [0x92; 32]))
        .unwrap_or_else(|fault| panic!("valid pending enrollment rejected: {:?}", fault.kind()))
        .into_store_candidate();
    let committed = commit_pairing_lifecycle_successor(mounted, &mut access, candidate)
        .unwrap_or_else(|_| panic!("pending enrollment did not commit"));
    assert_eq!(
        committed.transition(),
        PairingLifecycleTransition::AddPending
    );
    assert!(committed.store().select_pending_for_proof(pending).is_ok());

    let mounted = recover_once(committed.into_store(), &mut access)
        .unwrap_or_else(|_| panic!("post-enrollment cleanup failed"));
    let candidate = mounted
        .publishable_authority()
        .unwrap_or_else(|| panic!("clean pending authority was not publishable"))
        .plan_abort_pending(pending)
        .unwrap_or_else(|fault| panic!("pending abort rejected: {:?}", fault.kind()))
        .into_store_candidate();
    let committed = commit_pairing_lifecycle_successor(mounted, &mut access, candidate)
        .unwrap_or_else(|_| panic!("pending abort did not commit"));
    assert_eq!(
        committed.transition(),
        PairingLifecycleTransition::AbortPending
    );
    assert_eq!(committed.store().revision(), AuthorityRevision::new(3));
    let authority = committed
        .store()
        .publishable_authority()
        .unwrap_or_else(|| panic!("aborted authority was not publishable"));
    assert_eq!(authority.record_count(), 1);
    assert_eq!(authority.active_count(), 0);
    assert_eq!(authority.pending_credential(), Ok(None));
    assert!(committed.store().select_pending_for_proof(pending).is_err());
}

#[test]
fn pairing_semantic_rejection_retains_opaque_candidate_for_typed_retry() {
    let pending =
        PendingCredentialRef::new(CredentialId::new([0x53; 16]), CredentialGeneration::new(2));
    let retained_psk = [0x93; 32];
    let (correct, mut correct_access) = provisioned();
    let candidate = correct
        .publishable_authority()
        .unwrap_or_else(|| panic!("correct predecessor was not publishable"))
        .plan_add_pending(new_pending(0x53, retained_psk))
        .unwrap_or_else(|fault| panic!("valid pending enrollment rejected: {:?}", fault.kind()))
        .into_store_candidate();

    let (wrong, mut wrong_access) = commit_revision_two();
    let wrong = recover_once(wrong, &mut wrong_access)
        .unwrap_or_else(|_| panic!("wrong predecessor cleanup failed"));
    wrong_access.backend_mut().reset_counts();
    let error = match commit_pairing_lifecycle_successor(wrong, &mut wrong_access, candidate) {
        Err(CommitPairingLifecycleSuccessorError::Semantic(error)) => error,
        _ => panic!("wrong predecessor did not reject the typed candidate semantically"),
    };
    assert_eq!(error.transition(), PairingLifecycleTransition::AddPending);
    assert_eq!(
        error.kind(),
        PairingLifecycleStoreCandidateFaultKind::Structural(
            CredentialSuccessorFaultKind::AuthorityRevisionNotNext
        )
    );
    assert_eq!(error.current_revision(), AuthorityRevision::new(2));
    assert_eq!(error.candidate_revision(), AuthorityRevision::new(2));
    assert_eq!(
        (
            wrong_access.backend().reads,
            wrong_access.backend().writes,
            wrong_access.backend().erases
        ),
        (0, 0, 0)
    );
    let (_wrong, candidate) = error.into_parts();

    let committed = commit_pairing_lifecycle_successor(correct, &mut correct_access, candidate)
        .unwrap_or_else(|_| panic!("opaque candidate retry against its predecessor failed"));
    assert_eq!(
        committed.transition(),
        PairingLifecycleTransition::AddPending
    );
    let selected = committed
        .store()
        .select_pending_for_proof(pending)
        .unwrap_or_else(|_| panic!("retry lost the retained pending secret owner"));
    assert_eq!(*selected.into_parts().1, retained_psk);
}

#[test]
fn pairing_transition_mismatch_is_io_free_and_retries_with_source_secret_intact() {
    let source_psk = [0x42; 32];
    let (correct, mut correct_access, pending) = commit_pending_revision_two();
    let candidate = correct
        .publishable_authority()
        .unwrap_or_else(|| panic!("correct pending predecessor was not publishable"))
        .plan_activate_pending(pending)
        .unwrap_or_else(|fault| panic!("valid activation plan rejected: {:?}", fault.kind()))
        .into_store_candidate();

    let (wrong, mut wrong_access) = provisioned();
    let wrong = commit_successor(
        wrong,
        &mut wrong_access,
        authority_with_status_and_psk(2, 2, 1, 0x99),
    )
    .unwrap_or_else(|_| panic!("wrong pending predecessor did not commit"));
    let wrong = recover_once(wrong, &mut wrong_access)
        .unwrap_or_else(|_| panic!("wrong pending predecessor cleanup failed"));
    wrong_access.backend_mut().reset_counts();

    let error = match commit_pairing_lifecycle_successor(wrong, &mut wrong_access, candidate) {
        Err(CommitPairingLifecycleSuccessorError::Semantic(error)) => error,
        _ => panic!("cross-predecessor activation was not rejected semantically"),
    };
    assert_eq!(
        error.kind(),
        PairingLifecycleStoreCandidateFaultKind::TransitionMismatch
    );
    assert_eq!(
        error.transition(),
        PairingLifecycleTransition::ActivatePending
    );
    assert_eq!(error.current_revision(), AuthorityRevision::new(2));
    assert_eq!(error.candidate_revision(), AuthorityRevision::new(3));
    assert_eq!(
        (
            wrong_access.backend().reads,
            wrong_access.backend().writes,
            wrong_access.backend().erases
        ),
        (0, 0, 0)
    );

    let (wrong, candidate) = error.into_parts();
    let wrong_selected = wrong
        .select_pending_for_proof(pending)
        .unwrap_or_else(|_| panic!("semantic rejection lost the unchanged mounted current"));
    assert_eq!(*wrong_selected.into_parts().1, [0x99; 32]);

    let committed = commit_pairing_lifecycle_successor(correct, &mut correct_access, candidate)
        .unwrap_or_else(|_| panic!("candidate retry against original predecessor failed"));
    let selected = committed
        .store()
        .publishable_authority()
        .unwrap_or_else(|| panic!("retried activation was not publishable"))
        .select_for_handshake(pending.id())
        .unwrap_or_else(|_| panic!("retried activation lost its source secret"));
    let (selected_id, selected_generation, selected_psk) = selected.into_parts();
    assert_eq!(selected_id, pending.id());
    assert_eq!(selected_generation, CredentialGeneration::new(3));
    assert_eq!(*selected_psk, source_psk);
}

#[test]
fn pairing_physical_reconcile_preserves_transition_provenance() {
    let (mounted, mut access, pending_ref) = commit_pending_revision_two();
    let candidate = mounted
        .publishable_authority()
        .unwrap_or_else(|| panic!("pending predecessor was not publishable"))
        .plan_activate_pending(pending_ref)
        .unwrap_or_else(|fault| panic!("pending activation rejected: {:?}", fault.kind()))
        .into_store_candidate();
    access.backend_mut().reset_counts();
    access
        .backend_mut()
        .fail_write_after(0, FaultKind::LostReply);
    let pending = match commit_pairing_lifecycle_successor(mounted, &mut access, candidate) {
        Err(CommitPairingLifecycleSuccessorError::Physical(error)) => error.into_pending(),
        _ => panic!("ambiguous write did not retain typed physical ownership"),
    };
    assert_eq!(
        pending.transition(),
        PairingLifecycleTransition::ActivatePending
    );
    assert!(pending.media_ambiguous());
    let pending = match pending.cancel_before_io() {
        Ok(_) => panic!("media-ambiguous lifecycle mutation was cancelable"),
        Err(pending) => pending,
    };

    let committed = reconcile_pairing_lifecycle_successor(pending, &mut access)
        .unwrap_or_else(|_| panic!("typed lifecycle reconciliation failed"));
    assert_eq!(
        committed.transition(),
        PairingLifecycleTransition::ActivatePending
    );
    assert_eq!(committed.store().revision(), AuthorityRevision::new(3));
    assert_eq!(
        committed
            .store()
            .publishable_authority()
            .unwrap_or_else(|| panic!("reconciled authority was not publishable"))
            .pending_credential(),
        Ok(None)
    );
}

#[test]
fn pairing_io_free_recovery_requirement_can_return_unchanged_mounted_owner() {
    let pending_ref =
        PendingCredentialRef::new(CredentialId::new([0x54; 16]), CredentialGeneration::new(2));
    let (mounted, mut access) = provisioned();
    let candidate = mounted
        .publishable_authority()
        .unwrap_or_else(|| panic!("freshly provisioned authority was not publishable"))
        .plan_add_pending(new_pending(0x54, [0x94; 32]))
        .unwrap_or_else(|fault| panic!("valid pending enrollment rejected: {:?}", fault.kind()))
        .into_store_candidate();
    let mounted = commit_pairing_lifecycle_successor(mounted, &mut access, candidate)
        .unwrap_or_else(|_| panic!("pending enrollment did not commit"))
        .into_store();
    assert!(matches!(
        mounted.recovery(),
        CredentialStoreRecovery::CleanupInactive { .. }
    ));
    let candidate = mounted
        .publishable_authority()
        .unwrap_or_else(|| panic!("cleanup-only authority was not publishable"))
        .plan_activate_pending(pending_ref)
        .unwrap_or_else(|fault| panic!("pending activation rejected: {:?}", fault.kind()))
        .into_store_candidate();

    access.backend_mut().reset_counts();
    let pending = match commit_pairing_lifecycle_successor(mounted, &mut access, candidate) {
        Err(CommitPairingLifecycleSuccessorError::Physical(error)) => {
            assert!(matches!(
                error.error(),
                CredentialStoreMountError::Fault(CredentialStoreFault::RecoveryRequired)
            ));
            error.into_pending()
        }
        _ => panic!("cleanup-required store admitted a lifecycle mutation"),
    };
    assert_eq!(
        pending.transition(),
        PairingLifecycleTransition::ActivatePending
    );
    assert!(!pending.media_ambiguous());
    assert_eq!(
        (
            access.backend().reads,
            access.backend().writes,
            access.backend().erases
        ),
        (0, 0, 0)
    );
    let mounted = pending
        .cancel_before_io()
        .unwrap_or_else(|_| panic!("I/O-free lifecycle attempt was not cancelable"));
    assert_eq!(mounted.revision(), AuthorityRevision::new(2));
    assert!(matches!(
        mounted.recovery(),
        CredentialStoreRecovery::CleanupInactive { .. }
    ));
}

#[test]
fn retired_snapshot_is_never_revived_without_its_successor() {
    let (_mounted, access) = commit_revision_two();
    let mut missing_newer = access.backend().clone();
    missing_newer.bytes[CredentialStoreSector::B.offset()..].fill(0xff);
    assert!(matches!(
        mount(&mut bound(missing_newer)),
        Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::RetiredWithoutSuccessor
        ))
    ));

    let mut corrupt_newer = access.backend().clone();
    corrupt_newer.bytes[CredentialStoreSector::B.offset() + SNAPSHOT_IMAGE_OFFSET] ^= 1;
    assert!(mount(&mut bound(corrupt_newer)).is_err());
}

#[test]
fn older_active_never_falls_back_across_a_commit_marked_corrupt_successor() {
    let (mounted, mut access) = commit_revision_two();
    let mounted = recover_once(mounted, &mut access)
        .unwrap_or_else(|_| panic!("revision-two predecessor cleanup failed"));
    assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
    let mut predecessor_digest = [0_u8; DIGEST_SIZE];
    predecessor_digest.copy_from_slice(
        &access.backend().bytes[CredentialStoreSector::B.offset() + COMMIT_DIGEST_OFFSET
            ..CredentialStoreSector::B.offset() + COMMIT_MARKER_OFFSET],
    );
    install_snapshot(
        access.backend_mut(),
        CredentialStoreSector::A,
        &authority(3, 2),
        2,
        predecessor_digest,
    );
    access.backend_mut().bytes[CredentialStoreSector::A.offset() + COMMIT_DIGEST_OFFSET
        ..CredentialStoreSector::A.offset() + COMMIT_MARKER_OFFSET]
        .fill(0xff);
    assert!(matches!(
        mount(&mut access),
        Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::CommittedSnapshotCorrupt {
                sector: CredentialStoreSector::A
            }
        ))
    ));
}

#[test]
fn lost_replies_at_each_program_phase_reconcile_the_exact_candidate() {
    for write_call in 0..4 {
        let (mounted, mut access) = provisioned();
        access.backend_mut().reset_counts();
        access
            .backend_mut()
            .fail_write_after(write_call, FaultKind::LostReply);
        let pending = match commit_successor(mounted, &mut access, authority(2, 2)) {
            Err(CommitSuccessorError::Physical(error)) => error.into_pending(),
            _ => panic!("lost write reply {write_call} did not retain pending ownership"),
        };
        assert!(pending.media_ambiguous());
        let pending = match pending.into_parts() {
            Err(pending) => pending,
            Ok(_) => panic!("ambiguous write {write_call} released the current authority"),
        };
        let mounted = reconcile_successor(pending, &mut access)
            .unwrap_or_else(|_| panic!("lost write reply {write_call} did not reconcile"));
        assert_eq!(mounted.revision(), AuthorityRevision::new(2));
        assert_eq!(
            mounted.recovery(),
            CredentialStoreRecovery::CleanupInactive {
                sector: CredentialStoreSector::A
            }
        );
    }
}

#[test]
fn every_program_byte_cut_reconciles_without_publishing_a_partial_candidate() {
    for (write_call, length) in [
        (0, SNAPSHOT_PREFIX_SIZE),
        (1, DIGEST_SIZE),
        (2, MARKER_SIZE),
        (3, MARKER_SIZE),
    ] {
        for cut in 0..=length {
            let (mounted, mut access) = provisioned();
            access.backend_mut().reset_counts();
            access
                .backend_mut()
                .fail_write_after(write_call, FaultKind::Partial(cut));
            let pending = match commit_successor(mounted, &mut access, authority(2, 2)) {
                Err(CommitSuccessorError::Physical(error)) => error.into_pending(),
                _ => panic!("write {write_call} cut {cut} did not retain pending ownership"),
            };

            // Exercise the independent cold-boot path from the same cut, not
            // only the retained in-RAM pending owner. Before the complete
            // commit marker the predecessor remains authoritative; at and
            // after that point the linked successor is authoritative but may
            // still require retirement and cleanup.
            let mut boot_access = bound(access.backend().clone());
            let mut remounted = mount(&mut boot_access).unwrap_or_else(|_| {
                panic!("write {write_call} cut {cut} did not remount after reboot")
            });
            let expected_revision = if write_call > 2 || (write_call == 2 && cut == length) {
                2
            } else {
                1
            };
            assert_eq!(
                remounted.revision(),
                AuthorityRevision::new(expected_revision),
                "write {write_call} cut {cut} selected the wrong reboot authority"
            );
            for _ in 0..2 {
                if remounted.recovery() == CredentialStoreRecovery::Clean {
                    break;
                }
                remounted = recover_once(remounted, &mut boot_access).unwrap_or_else(|_| {
                    panic!("write {write_call} cut {cut} reboot recovery failed")
                });
            }
            assert_eq!(
                remounted.recovery(),
                CredentialStoreRecovery::Clean,
                "write {write_call} cut {cut} reboot recovery did not converge"
            );
            assert_eq!(
                remounted
                    .publishable_authority()
                    .expect("clean reboot authority is publishable")
                    .record_count(),
                usize::from(expected_revision == 2)
            );

            let mounted = reconcile_successor(pending, &mut access)
                .unwrap_or_else(|_| panic!("write {write_call} cut {cut} did not reconcile"));
            assert_eq!(mounted.revision(), AuthorityRevision::new(2));
            assert_eq!(mounted.active_sector(), CredentialStoreSector::B);
        }
    }
}

#[test]
fn every_cleanup_erase_byte_cut_keeps_only_the_newer_authority_selectable() {
    let (_mounted, baseline) = commit_revision_two();
    for cut in 0..=SECTOR_SIZE {
        let mut access = bound(baseline.backend().clone());
        access.backend_mut().reset_counts();
        let mounted = mount(&mut access).unwrap_or_else(|_| panic!("baseline did not mount"));
        assert_eq!(mounted.revision(), AuthorityRevision::new(2));
        access.backend_mut().reset_counts();
        access
            .backend_mut()
            .fail_erase_after(0, FaultKind::Partial(cut));
        let _owner = recover_once(mounted, &mut access).expect_err_owner();

        let remounted = mount(&mut access)
            .unwrap_or_else(|_| panic!("erase cut {cut} lost the newer authority"));
        assert_eq!(remounted.revision(), AuthorityRevision::new(2));
        let remounted = match remounted.recovery() {
            CredentialStoreRecovery::Clean => remounted,
            CredentialStoreRecovery::CleanupInactive { .. } => recover_once(remounted, &mut access)
                .unwrap_or_else(|_| panic!("erase cut {cut} cleanup retry failed")),
            CredentialStoreRecovery::RetirePredecessor { .. } => {
                panic!("erase cut {cut} revived predecessor retirement")
            }
        };
        assert_eq!(remounted.recovery(), CredentialStoreRecovery::Clean);
    }
}

#[test]
fn lost_cleanup_erase_reply_is_idempotently_recoverable() {
    let (mounted, mut access) = commit_revision_two();
    access.backend_mut().reset_counts();
    access
        .backend_mut()
        .fail_erase_after(0, FaultKind::LostReply);
    let mounted = recover_once(mounted, &mut access).expect_err_owner();
    let mounted = recover_once(mounted, &mut access)
        .unwrap_or_else(|_| panic!("lost erase reply retry failed"));
    assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
    let remounted = mount(&mut access).unwrap_or_else(|_| panic!("clean state did not mount"));
    assert_eq!(remounted.revision(), AuthorityRevision::new(2));
}

#[test]
fn read_failures_retain_owners_and_are_retryable() {
    let mut mount_access = bound(FakeNor::erased());
    mount_access.backend_mut().fail_read_after(0);
    assert!(matches!(
        mount(&mut mount_access),
        Err(CredentialStoreMountError::Backend(FakeError::Injected))
    ));

    let mut provision_access = bound(FakeNor::erased());
    provision_access.backend_mut().fail_read_after(2);
    assert!(matches!(
        provision_empty(&mut provision_access),
        Err(CredentialStoreMountError::Backend(FakeError::Injected))
    ));
    assert_eq!(
        recover_empty_provision(&mut provision_access)
            .unwrap_or_else(|_| panic!("provision readback retry failed"))
            .revision(),
        AuthorityRevision::new(1)
    );

    let (mounted, mut access) = provisioned();
    access.backend_mut().reset_counts();
    access.backend_mut().fail_read_after(1);
    let pending = match commit_successor(mounted, &mut access, authority(2, 2)) {
        Err(CommitSuccessorError::Physical(error)) => error.into_pending(),
        _ => panic!("successor readback failure did not retain pending owner"),
    };
    let mounted = reconcile_successor(pending, &mut access)
        .unwrap_or_else(|_| panic!("successor readback retry failed"));
    assert_eq!(mounted.revision(), AuthorityRevision::new(2));

    access.backend_mut().reset_counts();
    access.backend_mut().fail_read_after(0);
    let mounted = recover_once(mounted, &mut access).expect_err_owner();
    let mounted = recover_once(mounted, &mut access)
        .unwrap_or_else(|_| panic!("cleanup readback retry failed"));
    assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
}

#[test]
fn target_cleanup_erase_cuts_lost_reply_and_noop_success_are_bounded() {
    for cut in 0..=SECTOR_SIZE {
        let (mounted, mut access) = provisioned();
        access.backend_mut().reset_counts();
        access
            .backend_mut()
            .fail_write_after(0, FaultKind::Partial(1));
        let pending = match commit_successor(mounted, &mut access, authority(2, 2)) {
            Err(CommitSuccessorError::Physical(error)) => error.into_pending(),
            _ => panic!("prefix cut did not retain pending owner"),
        };
        access.backend_mut().reset_counts();
        access
            .backend_mut()
            .fail_erase_after(0, FaultKind::Partial(cut));
        let pending = match reconcile_successor(pending, &mut access) {
            Err(error) => error.into_pending(),
            Ok(_) => panic!("target erase cut {cut} unexpectedly completed"),
        };
        let mounted = reconcile_successor(pending, &mut access)
            .unwrap_or_else(|_| panic!("target erase cut {cut} did not retry"));
        assert_eq!(mounted.revision(), AuthorityRevision::new(2));
    }

    let (mounted, mut access) = provisioned();
    access.backend_mut().reset_counts();
    access
        .backend_mut()
        .fail_write_after(0, FaultKind::Partial(1));
    let pending = match commit_successor(mounted, &mut access, authority(2, 2)) {
        Err(CommitSuccessorError::Physical(error)) => error.into_pending(),
        _ => panic!("prefix cut did not retain pending owner"),
    };
    access.backend_mut().reset_counts();
    access
        .backend_mut()
        .fail_erase_after(0, FaultKind::LostReply);
    let pending = match reconcile_successor(pending, &mut access) {
        Err(error) => error.into_pending(),
        Ok(_) => panic!("lost target erase reply unexpectedly completed"),
    };
    assert_eq!(
        reconcile_successor(pending, &mut access)
            .unwrap_or_else(|_| panic!("lost target erase reply retry failed"))
            .revision(),
        AuthorityRevision::new(2)
    );

    let (mounted, mut access) = provisioned();
    access.backend_mut().reset_counts();
    access
        .backend_mut()
        .fail_write_after(0, FaultKind::Partial(1));
    let pending = match commit_successor(mounted, &mut access, authority(2, 2)) {
        Err(CommitSuccessorError::Physical(error)) => error.into_pending(),
        _ => panic!("prefix cut did not retain pending owner"),
    };
    access.backend_mut().reset_counts();
    access.backend_mut().successful_noop_erase = true;
    let error = match reconcile_successor(pending, &mut access) {
        Err(error) => error,
        Ok(_) => panic!("successful no-op erase bypassed readback"),
    };
    assert!(matches!(
        error.error(),
        CredentialStoreMountError::Fault(CredentialStoreFault::ReadbackMismatch { .. })
    ));
    let pending = error.into_pending();
    access.backend_mut().successful_noop_erase = false;
    assert_eq!(
        reconcile_successor(pending, &mut access)
            .unwrap_or_else(|_| panic!("no-op erase retry failed"))
            .revision(),
        AuthorityRevision::new(2)
    );
}

#[test]
fn semantic_rejection_and_recovery_required_are_io_free_and_retain_owners() {
    let (mounted, mut access) = provisioned();
    access.backend_mut().reset_counts();
    let error = match commit_successor(mounted, &mut access, authority(3, 3)) {
        Err(CommitSuccessorError::Semantic(error)) => error,
        _ => panic!("non-next candidate was not rejected semantically"),
    };
    assert_eq!(
        error.kind(),
        reticulum_device_api_credentials::CredentialSuccessorFaultKind::AuthorityRevisionNotNext
    );
    assert_eq!(error.current.revision(), AuthorityRevision::new(1));
    assert_eq!(error.candidate.revision(), AuthorityRevision::new(3));
    assert_eq!(
        (
            access.backend().reads,
            access.backend().writes,
            access.backend().erases
        ),
        (0, 0, 0)
    );

    let mounted = error.current;
    let mounted = commit_successor(mounted, &mut access, authority(2, 2))
        .unwrap_or_else(|_| panic!("valid successor failed"));
    access.backend_mut().reset_counts();
    let pending = match commit_successor(mounted, &mut access, authority(3, 2)) {
        Err(CommitSuccessorError::Physical(error)) => {
            assert!(matches!(
                error.error(),
                CredentialStoreMountError::Fault(CredentialStoreFault::RecoveryRequired)
            ));
            error.into_pending()
        }
        _ => panic!("dirty store admitted another mutation"),
    };
    assert_eq!(
        (
            access.backend().reads,
            access.backend().writes,
            access.backend().erases
        ),
        (0, 0, 0)
    );
    assert!(!pending.media_ambiguous());
    let (mounted, candidate) = match pending.into_parts() {
        Ok(parts) => parts,
        Err(_) => panic!("I/O-free recovery requirement became media-ambiguous"),
    };
    assert_eq!(mounted.revision(), AuthorityRevision::new(2));
    assert_eq!(candidate.revision(), AuthorityRevision::new(3));
    let mounted =
        recover_once(mounted, &mut access).unwrap_or_else(|_| panic!("required cleanup failed"));
    assert_eq!(
        commit_successor(mounted, &mut access, candidate)
            .unwrap_or_else(|_| panic!("retained candidate failed after cleanup"))
            .revision(),
        AuthorityRevision::new(3)
    );
}

#[test]
fn exhausted_revision_rejects_successor_without_io() {
    let mut flash = FakeNor::erased();
    install_snapshot(
        &mut flash,
        CredentialStoreSector::A,
        &authority(u64::MAX, u64::MAX),
        u64::MAX - 1,
        [0x9a; DIGEST_SIZE],
    );
    let mut access = bound(flash);
    let mounted = mount(&mut access).unwrap_or_else(|_| panic!("max revision did not mount"));
    access.backend_mut().reset_counts();
    let error = match commit_successor(mounted, &mut access, authority(1, 1)) {
        Err(CommitSuccessorError::Semantic(error)) => error,
        _ => panic!("revision exhaustion did not reject successor"),
    };
    assert_eq!(
        error.kind(),
        reticulum_device_api_credentials::CredentialSuccessorFaultKind::AuthorityRevisionNotNext
    );
    assert_eq!(
        (
            access.backend().reads,
            access.backend().writes,
            access.backend().erases
        ),
        (0, 0, 0)
    );
}
