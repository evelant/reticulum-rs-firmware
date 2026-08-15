use std::vec::Vec;

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use zeroize::Zeroize;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
    Injected,
    IllegalRewrite,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::Injected | Self::IllegalRewrite => NorFlashErrorKind::Other,
        }
    }
}

#[derive(Clone)]
struct FakeNor {
    bytes: [u8; PARTITION_SIZE],
    capacity: usize,
    mutation_count: usize,
    fail_after_mutation: Option<usize>,
    corrupt_write: Option<usize>,
    write_count: usize,
    corrupt_erase: bool,
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: [0xff; PARTITION_SIZE],
            capacity: PARTITION_SIZE,
            mutation_count: 0,
            fail_after_mutation: None,
            corrupt_write: None,
            write_count: 0,
            corrupt_erase: false,
        }
    }

    fn after_full_mutation(mut self, mutation: usize) -> Self {
        self.mutation_count = 0;
        self.fail_after_mutation = Some(mutation);
        self
    }

    fn finish_mutation(&mut self) -> Result<(), FakeError> {
        let mutation = self.mutation_count;
        self.mutation_count += 1;
        if self.fail_after_mutation == Some(mutation) {
            Err(FakeError::Injected)
        } else {
            Ok(())
        }
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let offset = offset as usize;
        let end = offset.checked_add(bytes.len()).ok_or(FakeError::Bounds)?;
        let source = self.bytes.get(offset..end).ok_or(FakeError::Bounds)?;
        bytes.copy_from_slice(source);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.capacity
    }
}

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = SECTOR_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let from = from as usize;
        let to = to as usize;
        if from > to || to > self.capacity {
            return Err(FakeError::Bounds);
        }
        if !from.is_multiple_of(Self::ERASE_SIZE) || !to.is_multiple_of(Self::ERASE_SIZE) {
            return Err(FakeError::Alignment);
        }
        self.bytes[from..to].fill(0xff);
        if self.corrupt_erase {
            self.corrupt_erase = false;
            self.bytes[from] = 0xfe;
        }
        self.finish_mutation()
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let offset = offset as usize;
        let end = offset.checked_add(bytes.len()).ok_or(FakeError::Bounds)?;
        if end > self.capacity {
            return Err(FakeError::Bounds);
        }
        for (stored, desired) in self.bytes[offset..end].iter_mut().zip(bytes) {
            if *stored != 0xff {
                return Err(FakeError::IllegalRewrite);
            }
            *stored &= *desired;
        }
        let write = self.write_count;
        self.write_count += 1;
        if self.corrupt_write == Some(write) {
            self.bytes[offset] ^= 1;
        }
        self.finish_mutation()
    }
}

struct ReadOnlyNor(FakeNor);

impl ErrorType for ReadOnlyNor {
    type Error = FakeError;
}

impl ReadNorFlash for ReadOnlyNor {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

struct BadReadGeometry(FakeNor);

impl ErrorType for BadReadGeometry {
    type Error = FakeError;
}

impl ReadNorFlash for BadReadGeometry {
    const READ_SIZE: usize = 3;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

fn bond(tag: u8, with_irk: bool) -> BleBond {
    BleBond::new(
        if tag.is_multiple_of(2) {
            BleAddressKind::Public
        } else {
            BleAddressKind::Random
        },
        [tag, tag + 1, tag + 2, tag + 3, tag + 4, tag + 5],
        with_irk.then_some([tag.wrapping_add(0x40); 16]),
        [tag.wrapping_add(0x80); 16],
    )
}

fn assert_bond(stored: &BleBond, tag: u8, with_irk: bool) {
    assert_eq!(
        stored.address_kind(),
        if tag.is_multiple_of(2) {
            BleAddressKind::Public
        } else {
            BleAddressKind::Random
        }
    );
    assert_eq!(
        stored.address(),
        &[tag, tag + 1, tag + 2, tag + 3, tag + 4, tag + 5]
    );
    assert_eq!(
        stored.irk().copied(),
        with_irk.then_some([tag.wrapping_add(0x40); 16])
    );
    assert_eq!(stored.ltk(), &[tag.wrapping_add(0x80); 16]);
    assert!(stored.is_bonded());
    assert!(stored.is_authenticated());
}

fn assert_fault<T>(result: Result<T, BondStoreError<FakeError>>, expected: BondStoreFault) {
    assert_eq!(result.err(), Some(BondStoreError::Fault(expected)));
}

fn program_partial(flash: &mut FakeNor, offset: usize, desired: &[u8], count: usize) {
    for (stored, desired) in flash.bytes[offset..offset + count]
        .iter_mut()
        .zip(&desired[..count])
    {
        *stored &= *desired;
    }
}

fn install_record(
    flash: &mut FakeNor,
    sector: BondStoreSector,
    generation: u64,
    value: Option<&BleBond>,
) {
    let encoded = encode_record(
        sector,
        NonZeroU64::new(generation).expect("test generation is nonzero"),
        value,
    );
    let offset = sector.offset();
    flash.bytes[offset..offset + RECORD_SIZE].copy_from_slice(&encoded[..]);
}

#[test]
fn geometry_and_marker_are_exact_and_irregular() {
    assert_eq!(PARTITION_SIZE, 8 * 1024);
    assert_eq!(SECTOR_SIZE, 4 * 1024);
    assert_eq!(PARTITION_SIZE, 2 * SECTOR_SIZE);
    assert_eq!(BOND_CAPACITY, 1);
    assert_eq!(COMMIT_OFFSET + COMMIT_SIZE, RECORD_SIZE);
    assert!(COMMIT_MARKER.iter().all(|byte| *byte != 0 && *byte != 0xff));
    assert!(COMMIT_MARKER.windows(2).any(|pair| pair[0] != pair[1]));
}

#[test]
fn non_current_semantic_version_is_not_restored() {
    let source = bond(7, true);
    let mut encoded = encode_record(
        BondStoreSector::A,
        NonZeroU64::new(1).unwrap(),
        Some(&source),
    );
    put_u16(&mut encoded[..], 10, SEMANTIC_FORMAT_VERSION - 1);
    let digest = record_digest(&encoded[..PROTECTED_SIZE]);
    encoded[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE].copy_from_slice(&digest);

    let mut flash = FakeNor::erased();
    flash.bytes[..RECORD_SIZE].copy_from_slice(&encoded[..]);
    assert_fault(
        mount(&mut flash),
        BondStoreFault::UnsupportedSemanticVersion(SEMANTIC_FORMAT_VERSION - 1),
    );
}

#[test]
fn non_current_physical_version_is_not_restored() {
    let source = bond(7, true);
    let mut encoded = encode_record(
        BondStoreSector::A,
        NonZeroU64::new(1).unwrap(),
        Some(&source),
    );
    put_u16(&mut encoded[..], 8, PHYSICAL_FORMAT_VERSION + 1);
    let digest = record_digest(&encoded[..PROTECTED_SIZE]);
    encoded[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE].copy_from_slice(&digest);

    let mut flash = FakeNor::erased();
    flash.bytes[..RECORD_SIZE].copy_from_slice(&encoded[..]);
    assert_fault(
        mount(&mut flash),
        BondStoreFault::UnsupportedPhysicalVersion(PHYSICAL_FORMAT_VERSION + 1),
    );
}

#[test]
fn non_current_committed_record_does_not_fall_back_to_a_current_record() {
    let current = bond(2, true);
    let stale = bond(3, true);
    let mut flash = FakeNor::erased();
    install_record(&mut flash, BondStoreSector::A, 1, Some(&current));

    let mut encoded = encode_record(
        BondStoreSector::B,
        NonZeroU64::new(2).unwrap(),
        Some(&stale),
    );
    put_u16(&mut encoded[..], 10, SEMANTIC_FORMAT_VERSION - 1);
    let digest = record_digest(&encoded[..PROTECTED_SIZE]);
    encoded[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE].copy_from_slice(&digest);
    let offset = BondStoreSector::B.offset();
    flash.bytes[offset..offset + RECORD_SIZE].copy_from_slice(&encoded[..]);

    assert_fault(
        mount(&mut flash),
        BondStoreFault::UnsupportedSemanticVersion(SEMANTIC_FORMAT_VERSION - 1),
    );
}

#[test]
fn fresh_erased_media_mounts_through_read_only_api() {
    let mut flash = ReadOnlyNor(FakeNor::erased());
    let state = mount(&mut flash).expect("fresh media mounts");
    assert!(state.is_fresh_erased());
    assert_eq!(state.generation(), None);
    assert_eq!(state.active_sector(), None);
    assert!(state.bond().is_none());
    assert_eq!(state.cleanup(), BondStoreCleanup::Clean);
}

#[test]
fn first_bond_and_remount_round_trip_all_material() {
    let mut flash = FakeNor::erased();
    let state = commit_bond(&mut flash, bond(1, true)).expect("first bond commits");
    assert_eq!(state.generation().map(NonZeroU64::get), Some(1));
    assert_eq!(state.active_sector(), Some(BondStoreSector::A));
    assert_eq!(state.cleanup(), BondStoreCleanup::Clean);
    assert_bond(state.bond().expect("bond is present"), 1, true);
    drop(state);

    let remounted = mount(&mut flash).expect("committed bond remounts");
    assert_bond(remounted.bond().expect("bond is present"), 1, true);
}

#[test]
fn mounted_bond_moves_out_without_duplication() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(6, true)).expect("bond commits"));

    let mounted = mount(&mut flash).expect("committed bond remounts");
    let owner = mounted.into_bond().expect("mounted bond moves out");
    assert_bond(&owner, 6, true);

    let empty = mount(&mut FakeNor::erased()).expect("fresh media mounts");
    assert!(empty.into_bond().is_none());
}

#[test]
fn optional_irk_absence_is_canonical_and_round_trips() {
    let mut flash = FakeNor::erased();
    let state = commit_bond(&mut flash, bond(2, false)).expect("bond commits");
    assert_bond(state.bond().expect("bond is present"), 2, false);
    assert!(flash.bytes[32..48].iter().all(|byte| *byte == 0));
}

#[test]
fn replacement_selects_newest_and_cleanup_erases_predecessor() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(1, true)).unwrap());
    let state = commit_bond(&mut flash, bond(2, false)).expect("replacement commits");
    assert_eq!(state.generation().map(NonZeroU64::get), Some(2));
    assert_eq!(state.active_sector(), Some(BondStoreSector::B));
    assert_eq!(
        state.cleanup(),
        BondStoreCleanup::EraseInactive {
            sector: BondStoreSector::A
        }
    );
    assert_bond(state.bond().unwrap(), 2, false);

    let clean = cleanup(&mut flash).expect("cleanup succeeds");
    assert_eq!(clean.cleanup(), BondStoreCleanup::Clean);
    assert!(flash.bytes[..SECTOR_SIZE].iter().all(|byte| *byte == 0xff));
    assert_bond(clean.bond().unwrap(), 2, false);
}

#[test]
fn another_replacement_can_erase_stale_sector_without_prior_cleanup() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(1, true)).unwrap());
    drop(commit_bond(&mut flash, bond(2, true)).unwrap());
    let state = commit_bond(&mut flash, bond(3, true)).expect("third generation commits");
    assert_eq!(state.generation().map(NonZeroU64::get), Some(3));
    assert_eq!(state.active_sector(), Some(BondStoreSector::A));
    assert_bond(state.bond().unwrap(), 3, true);
}

#[test]
fn clear_is_a_successor_and_cleanup_scrubs_the_old_secret_sector() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(7, true)).unwrap());
    let cleared = clear(&mut flash).expect("clear tombstone commits");
    assert_eq!(cleared.generation().map(NonZeroU64::get), Some(2));
    assert!(cleared.bond().is_none());
    assert_eq!(
        cleared.cleanup(),
        BondStoreCleanup::EraseInactive {
            sector: BondStoreSector::A
        }
    );
    drop(cleared);

    let clean = cleanup(&mut flash).expect("old secret sector erases");
    assert!(clean.bond().is_none());
    assert!(flash.bytes[..SECTOR_SIZE].iter().all(|byte| *byte == 0xff));
}

#[test]
fn backend_errors_during_clear_remount_as_predecessor_or_tombstone() {
    let mut baseline = FakeNor::erased();
    drop(commit_bond(&mut baseline, bond(7, true)).unwrap());

    for mutation in 0..3 {
        let mut flash = baseline.clone().after_full_mutation(mutation);
        assert_eq!(
            clear(&mut flash).err(),
            Some(BondStoreError::Backend(FakeError::Injected))
        );
        flash.fail_after_mutation = None;
        let mounted = mount(&mut flash).expect("one complete authority remains");
        if mutation == 2 {
            assert_eq!(mounted.generation().map(NonZeroU64::get), Some(2));
            assert!(mounted.bond().is_none(), "the committed tombstone wins");
        } else {
            assert_eq!(mounted.generation().map(NonZeroU64::get), Some(1));
            assert_bond(
                mounted
                    .bond()
                    .expect("the predecessor remains authoritative"),
                7,
                true,
            );
        }
    }
}

#[test]
fn ble_bond_explicit_zeroize_clears_only_secret_fields() {
    let mut owner = bond(9, true);
    let address_kind = owner.address_kind();
    let address = *owner.address();
    owner.zeroize();
    assert_eq!(owner.address_kind(), address_kind);
    assert_eq!(owner.address(), &address);
    assert_eq!(owner.irk(), None);
    assert_eq!(owner.ltk(), &[0; 16]);
}

#[test]
fn every_partial_first_prefix_and_marker_cut_is_recoverable() {
    let source = bond(3, true);
    let encoded = encode_record(
        BondStoreSector::A,
        NonZeroU64::new(1).unwrap(),
        Some(&source),
    );

    for cut in 0..=PREFIX_SIZE {
        let mut flash = FakeNor::erased();
        program_partial(&mut flash, 0, &encoded[..PREFIX_SIZE], cut);
        if cut == 0 {
            assert!(mount(&mut flash).unwrap().is_fresh_erased());
        } else {
            assert_fault(mount(&mut flash), BondStoreFault::RecoveryRequired);
            assert!(recover_empty(&mut flash).unwrap().is_fresh_erased());
        }
    }

    for cut in 0..=COMMIT_SIZE {
        let mut flash = FakeNor::erased();
        program_partial(&mut flash, 0, &encoded[..PREFIX_SIZE], PREFIX_SIZE);
        program_partial(&mut flash, COMMIT_OFFSET, &encoded[COMMIT_OFFSET..], cut);
        if cut == COMMIT_SIZE {
            assert_bond(mount(&mut flash).unwrap().bond().unwrap(), 3, true);
        } else {
            assert_fault(mount(&mut flash), BondStoreFault::RecoveryRequired);
        }
    }
}

#[test]
fn every_partial_successor_write_cut_preserves_old_until_commit_is_complete() {
    let mut baseline = FakeNor::erased();
    drop(commit_bond(&mut baseline, bond(1, true)).unwrap());
    let source = bond(2, false);
    let encoded = encode_record(
        BondStoreSector::B,
        NonZeroU64::new(2).unwrap(),
        Some(&source),
    );

    for cut in 0..=PREFIX_SIZE {
        let mut flash = baseline.clone();
        program_partial(
            &mut flash,
            BondStoreSector::B.offset(),
            &encoded[..PREFIX_SIZE],
            cut,
        );
        let state = mount(&mut flash).expect("predecessor remains valid");
        assert_eq!(state.generation().map(NonZeroU64::get), Some(1));
        assert_bond(state.bond().unwrap(), 1, true);
    }

    for cut in 0..=COMMIT_SIZE {
        let mut flash = baseline.clone();
        let base = BondStoreSector::B.offset();
        program_partial(&mut flash, base, &encoded[..PREFIX_SIZE], PREFIX_SIZE);
        program_partial(
            &mut flash,
            base + COMMIT_OFFSET,
            &encoded[COMMIT_OFFSET..],
            cut,
        );
        let state = mount(&mut flash).expect("one complete generation remains");
        if cut == COMMIT_SIZE {
            assert_eq!(state.generation().map(NonZeroU64::get), Some(2));
            assert_bond(state.bond().unwrap(), 2, false);
        } else {
            assert_eq!(state.generation().map(NonZeroU64::get), Some(1));
            assert_bond(state.bond().unwrap(), 1, true);
        }
    }
}

#[test]
fn every_partial_inactive_erase_cut_preserves_newest_generation() {
    let mut baseline = FakeNor::erased();
    drop(commit_bond(&mut baseline, bond(1, true)).unwrap());
    drop(commit_bond(&mut baseline, bond(2, true)).unwrap());

    for cut in 0..=SECTOR_SIZE {
        let mut flash = baseline.clone();
        flash.bytes[..cut].fill(0xff);
        let state = mount(&mut flash).expect("newer B always survives A cleanup cut");
        assert_eq!(state.generation().map(NonZeroU64::get), Some(2));
        assert_eq!(state.active_sector(), Some(BondStoreSector::B));
        assert_bond(state.bond().unwrap(), 2, true);
    }
}

#[test]
fn backend_errors_after_each_mutation_remount_as_old_or_new_not_neither() {
    let mut baseline = FakeNor::erased();
    drop(commit_bond(&mut baseline, bond(1, true)).unwrap());

    for mutation in 0..3 {
        let mut flash = baseline.clone().after_full_mutation(mutation);
        assert_eq!(
            commit_bond(&mut flash, bond(2, true)).err(),
            Some(BondStoreError::Backend(FakeError::Injected))
        );
        flash.fail_after_mutation = None;
        let state = mount(&mut flash).expect("one whole commit remains");
        let expected = if mutation == 2 { 2 } else { 1 };
        assert_eq!(state.generation().map(NonZeroU64::get), Some(expected));
        assert_bond(state.bond().unwrap(), expected as u8, true);
    }
}

#[test]
fn corrupt_and_torn_newer_sector_falls_back_to_valid_predecessor() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(1, true)).unwrap());
    drop(commit_bond(&mut flash, bond(2, true)).unwrap());

    flash.bytes[BondStoreSector::B.offset() + 48] ^= 1;
    let state = mount(&mut flash).expect("digest-corrupt B cannot override A");
    assert_eq!(state.generation().map(NonZeroU64::get), Some(1));
    assert_bond(state.bond().unwrap(), 1, true);
    assert_eq!(
        state.cleanup(),
        BondStoreCleanup::EraseInactive {
            sector: BondStoreSector::B
        }
    );

    let mut torn = FakeNor::erased();
    drop(commit_bond(&mut torn, bond(4, true)).unwrap());
    let source = bond(5, true);
    let encoded = encode_record(
        BondStoreSector::B,
        NonZeroU64::new(2).unwrap(),
        Some(&source),
    );
    let base = BondStoreSector::B.offset();
    program_partial(&mut torn, base, &encoded[..PREFIX_SIZE], PREFIX_SIZE);
    program_partial(
        &mut torn,
        base + COMMIT_OFFSET,
        &encoded[COMMIT_OFFSET..],
        COMMIT_SIZE / 2,
    );
    assert_bond(mount(&mut torn).unwrap().bond().unwrap(), 4, true);
}

#[test]
fn committed_tail_corruption_invalidates_only_that_sector() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(1, true)).unwrap());
    drop(commit_bond(&mut flash, bond(2, true)).unwrap());
    flash.bytes[BondStoreSector::B.offset() + RECORD_SIZE + 17] = 0xfe;
    let state = mount(&mut flash).expect("valid predecessor remains");
    assert_eq!(state.generation().map(NonZeroU64::get), Some(1));
}

#[test]
fn no_valid_commit_with_programmed_bytes_requires_explicit_recovery() {
    let mut flash = FakeNor::erased();
    flash.bytes[17] = 0xfe;
    flash.bytes[SECTOR_SIZE + 29] = 0xfc;
    assert_fault(mount(&mut flash), BondStoreFault::RecoveryRequired);
    let recovered = recover_empty(&mut flash).expect("explicit recovery erases debris");
    assert!(recovered.is_fresh_erased());
    assert!(flash.bytes.iter().all(|byte| *byte == 0xff));
}

#[test]
fn interrupted_recovery_is_safe_to_repeat_after_either_erase() {
    for failed_mutation in 0..2 {
        let mut flash = FakeNor::erased();
        flash.bytes[1] = 0xfe;
        flash.bytes[SECTOR_SIZE + 1] = 0xfe;
        flash.fail_after_mutation = Some(failed_mutation);
        assert_eq!(
            recover_empty(&mut flash).err(),
            Some(BondStoreError::Backend(FakeError::Injected))
        );
        flash.fail_after_mutation = None;
        let recovered = recover_empty(&mut flash).expect("recovery resumes");
        assert!(recovered.is_fresh_erased());
    }
}

#[test]
fn recovery_refuses_to_erase_any_valid_commit() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(1, true)).unwrap());
    flash.bytes[SECTOR_SIZE + 3] = 0xfe;
    assert_fault(
        recover_empty(&mut flash),
        BondStoreFault::CommittedStatePresent,
    );
    assert_bond(mount(&mut flash).unwrap().bond().unwrap(), 1, true);
}

#[test]
fn equal_valid_generations_are_rejected_as_conflict() {
    let mut flash = FakeNor::erased();
    let a = bond(1, true);
    let b = bond(2, true);
    install_record(&mut flash, BondStoreSector::A, 7, Some(&a));
    install_record(&mut flash, BondStoreSector::B, 7, Some(&b));
    assert_fault(mount(&mut flash), BondStoreFault::GenerationConflict);
}

#[test]
fn generation_max_cannot_wrap_and_flash_is_unchanged() {
    let mut flash = FakeNor::erased();
    let source = bond(1, true);
    install_record(&mut flash, BondStoreSector::A, u64::MAX, Some(&source));
    let before = flash.bytes;
    assert_fault(
        commit_bond(&mut flash, bond(2, true)),
        BondStoreFault::GenerationExhausted,
    );
    assert_eq!(flash.bytes, before);
}

#[test]
fn write_and_erase_readback_mismatches_are_detected() {
    let mut corrupt_prefix = FakeNor::erased();
    corrupt_prefix.corrupt_write = Some(0);
    assert_fault(
        commit_bond(&mut corrupt_prefix, bond(1, true)),
        BondStoreFault::ReadbackMismatch {
            sector: BondStoreSector::A,
        },
    );

    let mut corrupt_marker = FakeNor::erased();
    corrupt_marker.corrupt_write = Some(1);
    assert_fault(
        commit_bond(&mut corrupt_marker, bond(1, true)),
        BondStoreFault::ReadbackMismatch {
            sector: BondStoreSector::A,
        },
    );

    let mut corrupt_erase = FakeNor::erased();
    corrupt_erase.corrupt_erase = true;
    assert_fault(
        commit_bond(&mut corrupt_erase, bond(1, true)),
        BondStoreFault::ReadbackMismatch {
            sector: BondStoreSector::A,
        },
    );
}

#[test]
fn cleanup_backend_failure_never_loses_the_authoritative_successor() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(1, true)).unwrap());
    drop(commit_bond(&mut flash, bond(2, true)).unwrap());
    flash.mutation_count = 0;
    flash.fail_after_mutation = Some(0);
    assert_eq!(
        cleanup(&mut flash).err(),
        Some(BondStoreError::Backend(FakeError::Injected))
    );
    flash.fail_after_mutation = None;
    let state = mount(&mut flash).expect("successor survives cleanup fault");
    assert_eq!(state.generation().map(NonZeroU64::get), Some(2));
    assert_bond(state.bond().unwrap(), 2, true);
}

#[test]
fn cleanup_backend_failure_after_clear_never_restores_the_predecessor() {
    let mut flash = FakeNor::erased();
    drop(commit_bond(&mut flash, bond(1, true)).unwrap());
    let cleared = clear(&mut flash).expect("clear tombstone commits");
    assert!(cleared.bond().is_none());
    drop(cleared);

    flash.mutation_count = 0;
    flash.fail_after_mutation = Some(0);
    assert_eq!(
        cleanup(&mut flash).err(),
        Some(BondStoreError::Backend(FakeError::Injected))
    );
    flash.fail_after_mutation = None;
    let state = mount(&mut flash).expect("clear tombstone survives cleanup fault");
    assert_eq!(state.generation().map(NonZeroU64::get), Some(2));
    assert!(state.bond().is_none());
}

#[test]
fn exact_capacity_and_granularity_are_validated_before_io() {
    let mut wrong_capacity = FakeNor::erased();
    wrong_capacity.capacity -= 1;
    assert_fault(
        mount(&mut wrong_capacity),
        BondStoreFault::IncompatibleFlash,
    );

    let mut wrong_read = BadReadGeometry(FakeNor::erased());
    assert_eq!(
        mount(&mut wrong_read).err(),
        Some(BondStoreError::Fault(BondStoreFault::IncompatibleFlash))
    );
}

#[test]
fn all_supported_state_transitions_remount_identically() {
    let mut flash = FakeNor::erased();
    let mut observations = Vec::new();
    for tag in 1..=4 {
        let committed = commit_bond(&mut flash, bond(tag, tag % 2 == 0)).unwrap();
        observations.push((
            committed.generation().unwrap().get(),
            committed.active_sector().unwrap(),
            *committed.bond().unwrap().address(),
        ));
        drop(committed);
        let remounted = mount(&mut flash).unwrap();
        assert_eq!(
            observations.last().copied(),
            Some((
                remounted.generation().unwrap().get(),
                remounted.active_sector().unwrap(),
                *remounted.bond().unwrap().address(),
            ))
        );
    }
}
