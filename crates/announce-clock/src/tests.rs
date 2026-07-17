use super::*;

extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use std::vec;
use std::vec::Vec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeError {
    Bounds,
    Alignment,
    Injected,
}

impl NorFlashError for FakeError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            Self::Alignment => NorFlashErrorKind::NotAligned,
            Self::Injected => NorFlashErrorKind::Other,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum WriteFaultKind {
    Partial(usize),
    LostReply,
    AcknowledgeWithoutWrite,
}

#[derive(Clone, Copy, Debug)]
enum EraseFaultKind {
    Partial(usize),
    LostReply,
}

#[derive(Clone, Copy, Debug)]
struct ArmedWriteFault {
    normal_writes_before_fault: usize,
    kind: WriteFaultKind,
}

#[derive(Clone, Copy, Debug)]
struct ArmedEraseFault {
    normal_erases_before_fault: usize,
    kind: EraseFaultKind,
}

#[derive(Clone)]
struct FakeNor {
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
    erases: usize,
    write_fault: Option<ArmedWriteFault>,
    erase_fault: Option<ArmedEraseFault>,
    require_other_valid_before_erase: bool,
}

impl FakeNor {
    fn erased() -> Self {
        Self::with_capacity(PARTITION_SIZE)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: vec![0xff; capacity],
            reads: 0,
            writes: 0,
            erases: 0,
            write_fault: None,
            erase_fault: None,
            require_other_valid_before_erase: false,
        }
    }

    fn fail_write_after(&mut self, normal_writes_before_fault: usize, kind: WriteFaultKind) {
        self.write_fault = Some(ArmedWriteFault {
            normal_writes_before_fault,
            kind,
        });
    }

    fn fail_erase_after(&mut self, normal_erases_before_fault: usize, kind: EraseFaultKind) {
        self.erase_fault = Some(ArmedEraseFault {
            normal_erases_before_fault,
            kind,
        });
    }

    fn program(&mut self, offset: usize, bytes: &[u8]) {
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
    }

    fn program_record(&mut self, sector: Sector, slot: usize, epoch: BootEpoch) {
        let offset = sector.offset() + slot * SLOT_SIZE;
        self.program(offset, &encode_record(epoch));
    }

    fn clear_one_set_bit(&mut self, range: core::ops::Range<usize>) {
        let byte = self.bytes[range]
            .iter_mut()
            .find(|byte| **byte != 0)
            .expect("fixture range must contain a programmed one bit");
        *byte &= !(1 << byte.trailing_zeros());
    }

    fn raw_max_epoch(&self, sector: Sector) -> Option<BootEpoch> {
        let mut max = None;
        for slot in 0..SLOTS_PER_SECTOR {
            let offset = sector.offset() + slot * SLOT_SIZE;
            let encoded: &[u8; SLOT_SIZE] = self.bytes[offset..offset + SLOT_SIZE]
                .try_into()
                .expect("slot geometry");
            if let SlotStatus::Valid(epoch) = classify_slot(encoded) {
                max = max_optional_epoch(max, Some(epoch));
            }
        }
        max
    }

    fn raw_global_max(&self) -> Option<BootEpoch> {
        max_optional_epoch(self.raw_max_epoch(Sector::A), self.raw_max_epoch(Sector::B))
    }

    fn assert_mirrored(&self, epoch: BootEpoch) {
        assert_eq!(self.raw_max_epoch(Sector::A), Some(epoch));
        assert_eq!(self.raw_max_epoch(Sector::B), Some(epoch));
    }
}

impl ErrorType for FakeNor {
    type Error = FakeError;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        self.reads += 1;
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = SECTOR_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.erases += 1;
        let from = from as usize;
        let to = to as usize;

        if self.require_other_valid_before_erase {
            let erased = if from == Sector::A.offset() {
                Sector::A
            } else {
                Sector::B
            };
            assert!(
                self.raw_max_epoch(erased.other()).is_some(),
                "rotation erased the last valid high-water copy"
            );
        }

        let trigger = self
            .erase_fault
            .is_some_and(|fault| fault.normal_erases_before_fault == 0);
        if !trigger {
            if let Some(fault) = &mut self.erase_fault {
                fault.normal_erases_before_fault -= 1;
            }
            self.bytes[from..to].fill(0xff);
            return Ok(());
        }

        let fault = self.erase_fault.take().expect("erase fault was armed");
        match fault.kind {
            EraseFaultKind::Partial(length) => {
                let end = from.saturating_add(length).min(to);
                self.bytes[from..end].fill(0xff);
                Err(FakeError::Injected)
            }
            EraseFaultKind::LostReply => {
                self.bytes[from..to].fill(0xff);
                Err(FakeError::Injected)
            }
        }
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.writes += 1;
        let offset = offset as usize;
        let trigger = self
            .write_fault
            .is_some_and(|fault| fault.normal_writes_before_fault == 0);
        if !trigger {
            if let Some(fault) = &mut self.write_fault {
                fault.normal_writes_before_fault -= 1;
            }
            self.program(offset, bytes);
            return Ok(());
        }

        let fault = self.write_fault.take().expect("write fault was armed");
        match fault.kind {
            WriteFaultKind::Partial(length) => {
                let length = length.min(bytes.len());
                self.program(offset, &bytes[..length]);
                Err(FakeError::Injected)
            }
            WriteFaultKind::LostReply => {
                self.program(offset, bytes);
                Err(FakeError::Injected)
            }
            WriteFaultKind::AcknowledgeWithoutWrite => Ok(()),
        }
    }
}

impl MultiwriteNorFlash for FakeNor {}

struct BadReadGeometry(FakeNor);

impl ErrorType for BadReadGeometry {
    type Error = FakeError;
}

impl ReadNorFlash for BadReadGeometry {
    // A 128-byte slot is aligned, but the separately read 96-byte protected
    // prefix is not. Validation must reject this before issuing a read.
    const READ_SIZE: usize = 64;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        let offset = offset as usize;
        bytes.copy_from_slice(&self.0.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.0.bytes.len()
    }
}

impl NorFlash for BadReadGeometry {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = SECTOR_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.0.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.0.program(offset as usize, bytes);
        Ok(())
    }
}

impl MultiwriteNorFlash for BadReadGeometry {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

fn reserve(flash: &mut FakeNor) -> BootEpochReservation {
    reserve_next_boot_epoch(flash, FreshClockPolicy::AllowFirstProvision)
        .expect("reservation succeeds")
}

fn filled_log() -> FakeNor {
    let mut flash = FakeNor::erased();
    for expected in 1..=SLOTS_PER_SECTOR as u32 {
        assert_eq!(reserve(&mut flash).epoch().get(), expected);
    }
    assert_eq!(flash.raw_global_max().map(BootEpoch::get), Some(32));
    flash
}

#[test]
fn fresh_partition_commits_both_copies_and_maps_exactly_20_plus_20_bits() {
    assert_eq!(PARTITION_SIZE, 8 * 1024);
    assert_eq!(SECTOR_SIZE, 4 * 1024);
    assert_eq!(SLOT_SIZE, 128);
    assert_eq!(SLOTS_PER_SECTOR, 32);
    assert_eq!(BOOT_EPOCH_BITS, 20);
    assert_eq!(ANNOUNCE_ORDINAL_BITS, 20);
    assert_eq!(MAX_BOOT_EPOCH, 0x000f_ffff);
    assert_eq!(MAX_ANNOUNCE_ORDINAL, 0x000f_ffff);
    assert!(BootEpoch::new(0).is_none());
    assert_eq!(BootEpoch::new(1), Some(BootEpoch(1)));
    assert_eq!(
        BootEpoch::new(MAX_BOOT_EPOCH),
        Some(BootEpoch(MAX_BOOT_EPOCH))
    );
    assert!(BootEpoch::new(MAX_BOOT_EPOCH + 1).is_none());

    let mut flash = FakeNor::erased();
    let first = reserve(&mut flash);
    assert_eq!(first.epoch(), BootEpoch(1));
    assert_eq!(first.report().previous_high_water(), None);
    assert_eq!(
        first.report().sector_a(),
        SectorUpdate::Appended { slot: 0 }
    );
    assert_eq!(
        first.report().sector_b(),
        SectorUpdate::Appended { slot: 0 }
    );
    flash.assert_mirrored(BootEpoch(1));

    let encoded = &flash.bytes[..SLOT_SIZE];
    assert_eq!(&encoded[..16], DOMAIN_CLAIM);
    assert_eq!(&encoded[16..24], HEADER_MAGIC);
    assert_eq!(read_u16(encoded, 24), PHYSICAL_FORMAT_VERSION);
    assert_eq!(read_u16(encoded, 26), SLOT_SIZE as u16);
    assert_eq!(read_u32(encoded, 28), 1);
    assert!(encoded[32..64].iter().all(|byte| *byte == 0));
    assert_eq!(encoded[64..96], record_digest(&encoded[..64]));
    assert_eq!(encoded[96..], COMMIT_MARKER);

    let max_ordinal = AnnounceOrdinal::new(MAX_ANNOUNCE_ORDINAL).expect("maximum fits");
    assert!(AnnounceOrdinal::new(MAX_ANNOUNCE_ORDINAL + 1).is_none());
    assert_eq!(
        BootEpoch(MAX_BOOT_EPOCH).timestamp(max_ordinal).get(),
        MAX_ANNOUNCE_EMISSION_TIMESTAMP
    );
    assert_eq!(
        first.epoch().timestamp(AnnounceOrdinal::ZERO).get(),
        1_u64 << 20
    );

    let second = reserve(&mut flash);
    assert_eq!(second.epoch(), BootEpoch(2));
    assert_eq!(second.report().previous_high_water(), Some(BootEpoch(1)));
    assert_eq!(
        second.report().sector_a(),
        SectorUpdate::Appended { slot: 1 }
    );
    assert_eq!(
        second.report().sector_b(),
        SectorUpdate::Appended { slot: 1 }
    );
    flash.assert_mirrored(BootEpoch(2));
}

#[test]
fn claim_compatible_torn_prefix_is_recoverable_without_a_high_water() {
    let mut flash = FakeNor::erased();
    let encoded = encode_record(BootEpoch(1));
    flash.program(0, &encoded[..47]);
    assert_eq!(
        classify_slot(flash.bytes[..SLOT_SIZE].try_into().expect("slot")),
        SlotStatus::Torn
    );

    let reservation = reserve(&mut flash);
    assert_eq!(reservation.epoch(), BootEpoch(1));
    assert_eq!(
        reservation.report().sector_a(),
        SectorUpdate::Appended { slot: 1 }
    );
    assert_eq!(
        reservation.report().sector_b(),
        SectorUpdate::Appended { slot: 0 }
    );
    flash.assert_mirrored(BootEpoch(1));
}

#[test]
fn persistent_identity_policy_rejects_blank_and_torn_clock_media_without_mutation() {
    let mut blank = FakeNor::erased();
    let blank_before = blank.bytes.clone();
    assert_eq!(
        reserve_next_boot_epoch(&mut blank, FreshClockPolicy::Reject),
        Err(ReserveError::MissingHighWater)
    );
    assert_eq!(blank.bytes, blank_before);
    assert_eq!(blank.writes, 0);
    assert_eq!(blank.erases, 0);

    let mut torn = FakeNor::erased();
    let encoded = encode_record(BootEpoch(1));
    torn.program(0, &encoded[..47]);
    let torn_before = torn.bytes.clone();
    assert_eq!(
        reserve_next_boot_epoch(&mut torn, FreshClockPolicy::Reject),
        Err(ReserveError::MissingHighWater)
    );
    assert_eq!(torn.bytes, torn_before);
    assert_eq!(torn.writes, 0);
    assert_eq!(torn.erases, 0);

    let allowed = reserve(&mut torn);
    assert_eq!(allowed.epoch(), BootEpoch(1));
    torn.assert_mirrored(BootEpoch(1));
    let existing = reserve_next_boot_epoch(&mut torn, FreshClockPolicy::Reject)
        .expect("existing high-water does not need fresh-clock permission");
    assert_eq!(existing.epoch(), BootEpoch(2));
    torn.assert_mirrored(BootEpoch(2));
}

#[test]
fn no_high_water_fails_closed_on_unknown_or_committed_corrupt_data() {
    let mut unknown = FakeNor::erased();
    unknown.bytes[0] = 0;
    assert_eq!(
        reserve_next_boot_epoch(&mut unknown, FreshClockPolicy::AllowFirstProvision),
        Err(ReserveError::UnknownProgrammedData {
            sector: Sector::A,
            slot: 0,
        })
    );
    assert_eq!(unknown.writes, 0);
    assert_eq!(unknown.erases, 0);

    let mut corrupt = FakeNor::erased();
    corrupt.program_record(Sector::A, 0, BootEpoch(1));
    corrupt.clear_one_set_bit(DIGEST_OFFSET..COMMIT_OFFSET);
    assert_eq!(
        reserve_next_boot_epoch(&mut corrupt, FreshClockPolicy::AllowFirstProvision),
        Err(ReserveError::CommittedCorrupt {
            sector: Sector::A,
            slot: 0,
            reason: CorruptionReason::Digest,
        })
    );
    assert_eq!(corrupt.writes, 0);
    assert_eq!(corrupt.erases, 0);
}

#[test]
fn a_valid_copy_allows_safe_repair_of_unknown_and_committed_corrupt_sectors() {
    let mut flash = FakeNor::erased();
    assert_eq!(reserve(&mut flash).epoch(), BootEpoch(1));
    flash.bytes[Sector::B.offset() + SLOT_SIZE] = 0;

    let repaired = reserve(&mut flash);
    assert_eq!(repaired.epoch(), BootEpoch(2));
    assert_eq!(
        repaired.report().sector_a(),
        SectorUpdate::Appended { slot: 1 }
    );
    assert_eq!(
        repaired.report().sector_b(),
        SectorUpdate::Rotated {
            reason: RotationReason::Repair,
        }
    );
    flash.assert_mirrored(BootEpoch(2));

    let mut both_damaged = FakeNor::erased();
    assert_eq!(reserve(&mut both_damaged).epoch(), BootEpoch(1));
    both_damaged.program_record(Sector::A, 1, BootEpoch(2));
    both_damaged.clear_one_set_bit(SLOT_SIZE + DIGEST_OFFSET..SLOT_SIZE + COMMIT_OFFSET);
    both_damaged.bytes[Sector::B.offset() + SLOT_SIZE] = 0;
    both_damaged.require_other_valid_before_erase = true;

    let repaired = reserve(&mut both_damaged);
    assert_eq!(repaired.epoch(), BootEpoch(2));
    assert_eq!(
        repaired.report().sector_a(),
        SectorUpdate::Rotated {
            reason: RotationReason::Repair,
        }
    );
    assert_eq!(
        repaired.report().sector_b(),
        SectorUpdate::Rotated {
            reason: RotationReason::Repair,
        }
    );
    both_damaged.assert_mirrored(BootEpoch(2));
}

#[test]
fn full_logs_rotate_without_ever_erasing_the_last_valid_copy() {
    let mut flash = filled_log();
    flash.require_other_valid_before_erase = true;
    let erases_before = flash.erases;

    let reservation = reserve(&mut flash);
    assert_eq!(reservation.epoch(), BootEpoch(33));
    assert_eq!(
        reservation.report().sector_a(),
        SectorUpdate::Rotated {
            reason: RotationReason::Full,
        }
    );
    assert_eq!(
        reservation.report().sector_b(),
        SectorUpdate::Rotated {
            reason: RotationReason::Full,
        }
    );
    assert_eq!(flash.erases - erases_before, 2);
    flash.assert_mirrored(BootEpoch(33));
}

#[test]
fn every_prefix_and_commit_byte_cut_is_recovered_without_reusing_a_valid_epoch() {
    for write_index in 0..4 {
        let write_len = if write_index % 2 == 0 {
            COMMIT_OFFSET
        } else {
            SLOT_SIZE - COMMIT_OFFSET
        };
        for cut in 0..=write_len {
            let mut flash = FakeNor::erased();
            flash.fail_write_after(write_index, WriteFaultKind::Partial(cut));
            assert_eq!(
                reserve_next_boot_epoch(&mut flash, FreshClockPolicy::AllowFirstProvision),
                Err(ReserveError::Backend(FakeError::Injected)),
                "write {write_index}, cut {cut}"
            );
            let observed = flash.raw_global_max();
            let expected = observed.map_or(1, |epoch| epoch.get() + 1);
            let retry = reserve(&mut flash);
            assert_eq!(
                retry.epoch().get(),
                expected,
                "write {write_index}, cut {cut}"
            );
            flash.assert_mirrored(retry.epoch());
        }
    }
}

#[test]
fn lost_write_replies_are_resolved_only_by_a_rescan_and_new_epoch() {
    for write_index in 0..4 {
        let mut flash = FakeNor::erased();
        flash.fail_write_after(write_index, WriteFaultKind::LostReply);
        assert_eq!(
            reserve_next_boot_epoch(&mut flash, FreshClockPolicy::AllowFirstProvision),
            Err(ReserveError::Backend(FakeError::Injected))
        );
        let observed = flash.raw_global_max();
        let expected = observed.map_or(1, |epoch| epoch.get() + 1);
        let retry = reserve(&mut flash);
        assert_eq!(
            retry.epoch().get(),
            expected,
            "lost write reply {write_index}"
        );
        flash.assert_mirrored(retry.epoch());
    }

    let mut flash = FakeNor::erased();
    assert_eq!(reserve(&mut flash).epoch(), BootEpoch(1));
    flash.fail_write_after(3, WriteFaultKind::LostReply);
    assert_eq!(
        reserve_next_boot_epoch(&mut flash, FreshClockPolicy::AllowFirstProvision),
        Err(ReserveError::Backend(FakeError::Injected))
    );
    flash.assert_mirrored(BootEpoch(2));
    let retry = reserve(&mut flash);
    assert_eq!(retry.epoch(), BootEpoch(3));
    flash.assert_mirrored(BootEpoch(3));
}

#[test]
fn acknowledged_but_missing_program_is_caught_by_exact_readback() {
    let mut flash = FakeNor::erased();
    flash.fail_write_after(0, WriteFaultKind::AcknowledgeWithoutWrite);
    assert_eq!(
        reserve_next_boot_epoch(&mut flash, FreshClockPolicy::AllowFirstProvision),
        Err(ReserveError::ReadbackMismatch {
            sector: Sector::A,
            slot: Some(0),
        })
    );
    assert_eq!(reserve(&mut flash).epoch(), BootEpoch(1));
}

#[test]
fn every_rotation_erase_byte_cut_preserves_the_other_high_water_and_recovers() {
    let base = filled_log();
    for cut in 0..=SECTOR_SIZE {
        let mut flash = base.clone();
        flash.require_other_valid_before_erase = true;
        flash.fail_erase_after(0, EraseFaultKind::Partial(cut));
        assert_eq!(
            reserve_next_boot_epoch(&mut flash, FreshClockPolicy::AllowFirstProvision),
            Err(ReserveError::Backend(FakeError::Injected)),
            "erase cut {cut}"
        );
        assert_eq!(flash.raw_max_epoch(Sector::B), Some(BootEpoch(32)));
        let retry = reserve(&mut flash);
        assert_eq!(retry.epoch(), BootEpoch(33), "erase cut {cut}");
        flash.assert_mirrored(BootEpoch(33));
    }
}

#[test]
fn lost_rotation_reply_can_skip_but_never_reuse_an_epoch() {
    let base = filled_log();

    let mut first_erase = base.clone();
    first_erase.require_other_valid_before_erase = true;
    first_erase.fail_erase_after(0, EraseFaultKind::LostReply);
    assert_eq!(
        reserve_next_boot_epoch(&mut first_erase, FreshClockPolicy::AllowFirstProvision,),
        Err(ReserveError::Backend(FakeError::Injected))
    );
    assert_eq!(reserve(&mut first_erase).epoch(), BootEpoch(33));
    first_erase.assert_mirrored(BootEpoch(33));

    let mut second_erase = base;
    second_erase.require_other_valid_before_erase = true;
    second_erase.fail_erase_after(1, EraseFaultKind::LostReply);
    assert_eq!(
        reserve_next_boot_epoch(&mut second_erase, FreshClockPolicy::AllowFirstProvision,),
        Err(ReserveError::Backend(FakeError::Injected))
    );
    assert_eq!(second_erase.raw_global_max(), Some(BootEpoch(33)));
    let retry = reserve(&mut second_erase);
    assert_eq!(retry.epoch(), BootEpoch(34));
    second_erase.assert_mirrored(BootEpoch(34));
}

#[test]
fn maximum_epoch_fails_without_writing_or_erasing() {
    let mut flash = FakeNor::erased();
    flash.program_record(Sector::A, 0, BootEpoch(MAX_BOOT_EPOCH));
    flash.program_record(Sector::B, 0, BootEpoch(MAX_BOOT_EPOCH));
    let writes = flash.writes;
    let erases = flash.erases;

    assert_eq!(
        reserve_next_boot_epoch(&mut flash, FreshClockPolicy::AllowFirstProvision),
        Err(ReserveError::EpochExhausted)
    );
    assert_eq!(flash.writes, writes);
    assert_eq!(flash.erases, erases);
}

#[test]
fn exact_partition_geometry_is_mandatory() {
    let mut short = FakeNor::with_capacity(PARTITION_SIZE - SECTOR_SIZE);
    assert_eq!(
        reserve_next_boot_epoch(&mut short, FreshClockPolicy::Reject),
        Err(ReserveError::IncompatibleFlash)
    );

    let mut bad_read = BadReadGeometry(FakeNor::erased());
    assert_eq!(
        reserve_next_boot_epoch(&mut bad_read, FreshClockPolicy::Reject),
        Err(ReserveError::IncompatibleFlash)
    );
}
