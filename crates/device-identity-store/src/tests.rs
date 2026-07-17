use super::*;

extern crate std;

use core::num::NonZeroU32;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use std::{vec, vec::Vec};

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
    last_erase: Option<(usize, usize)>,
    write_fault: Option<ArmedWriteFault>,
    erase_fault: Option<ArmedEraseFault>,
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
            reads: 0,
            writes: 0,
            erases: 0,
            last_erase: None,
            write_fault: None,
            erase_fault: None,
        }
    }

    fn fail_write_after(&mut self, normal_writes_before_fault: usize, kind: WriteFaultKind) {
        assert!(self.write_fault.is_none());
        self.write_fault = Some(ArmedWriteFault {
            normal_writes_before_fault,
            kind,
        });
    }

    fn fail_erase_after(&mut self, normal_erases_before_fault: usize, kind: EraseFaultKind) {
        assert!(self.erase_fault.is_none());
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

    fn clear_one_set_bit(&mut self, range: core::ops::Range<usize>) {
        let byte = self.bytes[range]
            .iter_mut()
            .find(|byte| **byte != 0)
            .expect("fixture range contains one programmed bit");
        *byte &= !(1 << byte.trailing_zeros());
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
    const ERASE_SIZE: usize = MIRROR_SECTOR_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.writes += 1;
        let trigger = self
            .write_fault
            .is_some_and(|fault| fault.normal_writes_before_fault == 0);
        if !trigger {
            if let Some(fault) = &mut self.write_fault {
                fault.normal_writes_before_fault -= 1;
            }
            self.program(offset as usize, bytes);
            return Ok(());
        }

        let fault = self.write_fault.take().expect("write fault is armed");
        match fault.kind {
            WriteFaultKind::Partial(length) => {
                let length = length.min(bytes.len());
                self.program(offset as usize, &bytes[..length]);
                Err(FakeError::Injected)
            }
            WriteFaultKind::LostReply => {
                self.program(offset as usize, bytes);
                Err(FakeError::Injected)
            }
        }
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.erases += 1;
        self.last_erase = Some((from as usize, to as usize));
        let trigger = self
            .erase_fault
            .is_some_and(|fault| fault.normal_erases_before_fault == 0);
        if !trigger {
            if let Some(fault) = &mut self.erase_fault {
                fault.normal_erases_before_fault -= 1;
            }
            self.bytes[from as usize..to as usize].fill(0xff);
            return Ok(());
        }

        let fault = self.erase_fault.take().expect("erase fault is armed");
        match fault.kind {
            EraseFaultKind::Partial(length) => {
                let end = (from as usize + length).min(to as usize);
                self.bytes[from as usize..end].fill(0xff);
                Err(FakeError::Injected)
            }
            EraseFaultKind::LostReply => {
                self.bytes[from as usize..to as usize].fill(0xff);
                Err(FakeError::Injected)
            }
        }
    }
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

struct PatternRng {
    seed: u8,
    calls: usize,
}

impl PatternRng {
    const fn new(seed: u8) -> Self {
        Self { seed, calls: 0 }
    }
}

impl RngCore for PatternRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0_u8; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0_u8; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, destination: &mut [u8]) {
        for (index, byte) in destination.iter_mut().enumerate() {
            *byte = self.seed.wrapping_add(index as u8);
        }
        self.calls += 1;
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for PatternRng {}

struct PanicRng;

impl RngCore for PanicRng {
    fn next_u32(&mut self) -> u32 {
        panic!("durable reload must not use entropy")
    }

    fn next_u64(&mut self) -> u64 {
        panic!("durable reload must not use entropy")
    }

    fn fill_bytes(&mut self, _destination: &mut [u8]) {
        panic!("durable reload must not use entropy")
    }

    fn try_fill_bytes(&mut self, _destination: &mut [u8]) -> Result<(), rand_core::Error> {
        panic!("durable reload must not use entropy")
    }
}

impl CryptoRng for PanicRng {}

struct FailingRng;

impl RngCore for FailingRng {
    fn next_u32(&mut self) -> u32 {
        0
    }

    fn next_u64(&mut self) -> u64 {
        0
    }

    fn fill_bytes(&mut self, _destination: &mut [u8]) {}

    fn try_fill_bytes(&mut self, _destination: &mut [u8]) -> Result<(), rand_core::Error> {
        Err(rand_core::Error::from(
            NonZeroU32::new(rand_core::Error::CUSTOM_START).unwrap(),
        ))
    }
}

impl CryptoRng for FailingRng {}

fn expected_generated(seed: u8) -> [u8; PRIVATE_KEY_SIZE] {
    let mut key = [0_u8; PRIVATE_KEY_SIZE];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    key[0] &= 248;
    key[31] &= 127;
    key[31] |= 64;
    key
}

fn arbitrary_key(tag: u8) -> [u8; PRIVATE_KEY_SIZE] {
    let mut key = [tag; PRIVATE_KEY_SIZE];
    key[0] = 0xff;
    key[31] = tag.wrapping_add(0x80);
    key
}

fn seed_mirror(flash: &mut FakeNor, mirror: IdentityMirror, key: &[u8; PRIVATE_KEY_SIZE]) {
    write_record(flash, mirror, key).expect("test mirror commits");
}

#[test]
fn geometry_and_record_encoding_are_exact() {
    assert_eq!(PARTITION_SIZE, 8192);
    assert_eq!(MIRROR_SECTOR_SIZE, 4096);
    assert_eq!(RECORD_SIZE, 256);

    let key = arbitrary_key(0x31);
    let encoded = encode_record(&key);
    assert_eq!(&encoded[CLAIM_OFFSET..HEADER_OFFSET], &CLAIM_MARKER);
    assert_eq!(&encoded[HEADER_OFFSET..HEADER_OFFSET + 8], HEADER_MAGIC);
    assert_eq!(
        read_u16(&encoded[..], HEADER_OFFSET + 8),
        Some(PHYSICAL_FORMAT_VERSION)
    );
    assert_eq!(
        &encoded[PRIVATE_KEY_OFFSET..RESERVED_OFFSET],
        key.as_slice()
    );
    assert!(
        encoded[RESERVED_OFFSET..DIGEST_OFFSET]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert_eq!(
        &encoded[DIGEST_OFFSET..COMMIT_OFFSET],
        &record_digest(&encoded[..])
    );
    assert_eq!(&encoded[COMMIT_OFFSET..RECORD_SIZE], &COMMIT_MARKER);
    assert_eq!(decode_record(&encoded[..]).unwrap().as_bytes(), &key);
}

#[test]
fn identity_preflight_classifies_blank_and_recognizable_torn_as_vacant_without_mutation() {
    let mut blank = FakeNor::erased();
    let blank_bytes = blank.bytes.clone();
    assert_eq!(
        inspect_identity(&mut blank).unwrap(),
        IdentityPreflight::Vacant
    );
    assert_eq!(blank.bytes, blank_bytes);
    assert_eq!(blank.writes, 0);
    assert_eq!(blank.erases, 0);

    let mut torn = FakeNor::erased();
    torn.bytes[..11].copy_from_slice(&CLAIM_MARKER[..11]);
    let torn_bytes = torn.bytes.clone();
    assert_eq!(
        inspect_identity(&mut torn).unwrap(),
        IdentityPreflight::Vacant
    );
    assert_eq!(torn.bytes, torn_bytes);
    assert_eq!(torn.writes, 0);
    assert_eq!(torn.erases, 0);
}

#[test]
fn identity_preflight_reports_single_and_redundant_coverage_without_retaining_key_bytes() {
    let key = arbitrary_key(0x2d);
    let mut flash = FakeNor::erased();
    seed_mirror(&mut flash, IdentityMirror::A, &key);
    let single_bytes = flash.bytes.clone();
    let single_writes = flash.writes;
    let single_erases = flash.erases;
    let single = inspect_identity(&mut flash).unwrap();
    assert_eq!(
        single,
        IdentityPreflight::Committed {
            coverage: IdentityMirrorCoverage::Single,
        }
    );
    assert_eq!(std::format!("{single:?}"), "Committed { coverage: Single }");
    assert_eq!(flash.bytes, single_bytes);
    assert_eq!(flash.writes, single_writes);
    assert_eq!(flash.erases, single_erases);

    seed_mirror(&mut flash, IdentityMirror::B, &key);
    let redundant_bytes = flash.bytes.clone();
    let redundant_writes = flash.writes;
    let redundant_erases = flash.erases;
    let redundant = inspect_identity(&mut flash).unwrap();
    assert_eq!(
        redundant,
        IdentityPreflight::Committed {
            coverage: IdentityMirrorCoverage::Redundant,
        }
    );
    assert_eq!(
        std::format!("{redundant:?}"),
        "Committed { coverage: Redundant }"
    );
    assert_eq!(flash.bytes, redundant_bytes);
    assert_eq!(flash.writes, redundant_writes);
    assert_eq!(flash.erases, redundant_erases);
}

#[test]
fn identity_preflight_fails_closed_without_mutating_unknown_corrupt_or_conflicting_flash() {
    let mut unknown = FakeNor::erased();
    unknown.bytes[PRIVATE_KEY_OFFSET] = 0xfe;
    let unknown_bytes = unknown.bytes.clone();
    assert!(matches!(
        inspect_identity(&mut unknown),
        Err(IdentityStoreError::UnknownProgrammedData {
            mirror: IdentityMirror::A
        })
    ));
    assert_eq!(unknown.bytes, unknown_bytes);
    assert_eq!(unknown.writes, 0);
    assert_eq!(unknown.erases, 0);

    let mut corrupt = FakeNor::erased();
    seed_mirror(&mut corrupt, IdentityMirror::A, &arbitrary_key(0x38));
    corrupt.clear_one_set_bit(DIGEST_OFFSET..COMMIT_OFFSET);
    let corrupt_bytes = corrupt.bytes.clone();
    let corrupt_writes = corrupt.writes;
    let corrupt_erases = corrupt.erases;
    assert!(matches!(
        inspect_identity(&mut corrupt),
        Err(IdentityStoreError::CommittedCorrupt {
            mirror: IdentityMirror::A
        })
    ));
    assert_eq!(corrupt.bytes, corrupt_bytes);
    assert_eq!(corrupt.writes, corrupt_writes);
    assert_eq!(corrupt.erases, corrupt_erases);

    let mut conflict = FakeNor::erased();
    seed_mirror(&mut conflict, IdentityMirror::A, &arbitrary_key(0x41));
    seed_mirror(&mut conflict, IdentityMirror::B, &arbitrary_key(0x52));
    let conflict_bytes = conflict.bytes.clone();
    let conflict_writes = conflict.writes;
    let conflict_erases = conflict.erases;
    assert!(matches!(
        inspect_identity(&mut conflict),
        Err(IdentityStoreError::MirrorConflict)
    ));
    assert_eq!(conflict.bytes, conflict_bytes);
    assert_eq!(conflict.writes, conflict_writes);
    assert_eq!(conflict.erases, conflict_erases);
}

#[test]
fn fresh_provision_clamps_only_generated_x25519_and_reload_is_mutation_free() {
    let mut flash = FakeNor::erased();
    let mut rng = PatternRng::new(0xff);
    let (material, report) = boot_load_or_provision(&mut flash, &mut rng).unwrap();
    let expected = expected_generated(0xff);
    assert_eq!(material.as_bytes(), &expected);
    assert_eq!(rng.calls, 1);
    assert_eq!(report.source(), IdentityBootSource::Provisioned);
    assert_eq!(report.coverage(), IdentityMirrorCoverage::Redundant);
    assert!(!report.repair_deferred());
    assert_eq!(flash.writes, 6);
    assert_eq!(flash.erases, 0);
    assert!(
        flash.bytes[RECORD_SIZE..MIRROR_SECTOR_SIZE]
            .iter()
            .all(|byte| *byte == 0xff)
    );
    assert!(
        flash.bytes[MIRROR_SECTOR_SIZE + RECORD_SIZE..PARTITION_SIZE]
            .iter()
            .all(|byte| *byte == 0xff)
    );

    drop(material);
    let writes = flash.writes;
    let erases = flash.erases;
    let (reloaded, reloaded_report) = boot_load_or_provision(&mut flash, &mut PanicRng).unwrap();
    assert_eq!(reloaded.as_bytes(), &expected);
    assert_eq!(reloaded_report.source(), IdentityBootSource::Loaded);
    assert_eq!(
        reloaded_report.coverage(),
        IdentityMirrorCoverage::Redundant
    );
    assert_eq!(flash.writes, writes);
    assert_eq!(flash.erases, erases);
}

#[test]
fn valid_imported_or_reloaded_bytes_are_never_reclamped() {
    let mut flash = FakeNor::erased();
    let key = arbitrary_key(0x7f);
    assert_eq!(key[0], 0xff);
    assert_ne!(key[0] & 7, 0);
    seed_mirror(&mut flash, IdentityMirror::A, &key);
    seed_mirror(&mut flash, IdentityMirror::B, &key);
    let writes = flash.writes;

    let (loaded, report) = boot_load_or_provision(&mut flash, &mut PanicRng).unwrap();
    assert_eq!(loaded.as_bytes(), &key);
    assert_eq!(report.source(), IdentityBootSource::Loaded);
    assert_eq!(flash.writes, writes);
}

#[test]
fn entropy_failure_is_typed_and_never_mutates_flash() {
    let mut flash = FakeNor::erased();
    let result = boot_load_or_provision(&mut flash, &mut FailingRng);
    assert!(matches!(result, Err(IdentityStoreError::Entropy(_))));
    assert_eq!(flash.writes, 0);
    assert_eq!(flash.erases, 0);
    assert!(flash.bytes.iter().all(|byte| *byte == 0xff));
}

#[test]
fn unknown_nonblank_or_sole_committed_corruption_fails_without_mutation() {
    let mut unknown = FakeNor::erased();
    unknown.bytes[PRIVATE_KEY_OFFSET] = 0xfe;
    assert!(matches!(
        boot_load_or_provision(&mut unknown, &mut PanicRng),
        Err(IdentityStoreError::UnknownProgrammedData {
            mirror: IdentityMirror::A
        })
    ));
    assert_eq!(unknown.writes, 0);
    assert_eq!(unknown.erases, 0);

    let mut corrupt = FakeNor::erased();
    let key = arbitrary_key(0x44);
    seed_mirror(&mut corrupt, IdentityMirror::A, &key);
    corrupt.clear_one_set_bit(DIGEST_OFFSET..COMMIT_OFFSET);
    let writes = corrupt.writes;
    assert!(matches!(
        boot_load_or_provision(&mut corrupt, &mut PanicRng),
        Err(IdentityStoreError::CommittedCorrupt {
            mirror: IdentityMirror::A
        })
    ));
    assert_eq!(corrupt.writes, writes);
    assert_eq!(corrupt.erases, 0);
}

#[test]
fn different_valid_mirrors_conflict_without_mutation() {
    let mut flash = FakeNor::erased();
    seed_mirror(&mut flash, IdentityMirror::A, &arbitrary_key(0x11));
    seed_mirror(&mut flash, IdentityMirror::B, &arbitrary_key(0x22));
    let writes = flash.writes;
    let erases = flash.erases;
    assert!(matches!(
        boot_load_or_provision(&mut flash, &mut PanicRng),
        Err(IdentityStoreError::MirrorConflict)
    ));
    assert_eq!(flash.writes, writes);
    assert_eq!(flash.erases, erases);
}

#[test]
fn one_valid_mirror_repairs_only_its_peer_and_preserves_exact_key() {
    let mut flash = FakeNor::erased();
    let key = arbitrary_key(0x53);
    seed_mirror(&mut flash, IdentityMirror::A, &key);
    flash.bytes[MIRROR_SECTOR_SIZE + 7] = 0x7f;
    let writes = flash.writes;

    let (loaded, report) = boot_load_or_provision(&mut flash, &mut PanicRng).unwrap();
    assert_eq!(loaded.as_bytes(), &key);
    assert_eq!(report.source(), IdentityBootSource::Repaired);
    assert_eq!(report.coverage(), IdentityMirrorCoverage::Redundant);
    assert_eq!(flash.erases, 1);
    assert_eq!(flash.last_erase, Some((MIRROR_SECTOR_SIZE, PARTITION_SIZE)));
    assert_eq!(flash.writes, writes + 3);
    assert_eq!(
        decode_record(&flash.bytes[..RECORD_SIZE])
            .unwrap()
            .as_bytes(),
        &key
    );
    assert_eq!(
        decode_record(&flash.bytes[MIRROR_SECTOR_SIZE..MIRROR_SECTOR_SIZE + RECORD_SIZE])
            .unwrap()
            .as_bytes(),
        &key
    );
}

#[test]
fn recognizable_torn_claim_is_owned_and_can_be_first_provisioned() {
    let mut flash = FakeNor::erased();
    flash.bytes[..8].copy_from_slice(&CLAIM_MARKER[..8]);
    assert!(matches!(
        scan_mirror(&mut flash, IdentityMirror::A).unwrap(),
        SectorState::Torn
    ));
    let expected = expected_generated(0x35);
    let (loaded, report) = boot_load_or_provision(&mut flash, &mut PatternRng::new(0x35)).unwrap();
    assert_eq!(loaded.as_bytes(), &expected);
    assert_eq!(report.source(), IdentityBootSource::Provisioned);
    assert_eq!(report.coverage(), IdentityMirrorCoverage::Redundant);
    assert_eq!(flash.erases, 1);
    assert_eq!(flash.last_erase, Some((0, MIRROR_SECTOR_SIZE)));
}

#[test]
fn exhaustive_write_cuts_and_lost_replies_never_replace_a_committed_key() {
    let write_lengths = [CLAIM_SIZE, BODY_WRITE_SIZE, COMMIT_SIZE];

    // Cuts during primary A. Before its full commit there is no externally
    // usable identity, so a later boot may safely generate a new one. A fully
    // programmed commit, even with a lost reply, always preserves the first key.
    for (write_index, write_len) in write_lengths.into_iter().enumerate() {
        for cut in 0..=write_len {
            let mut flash = FakeNor::erased();
            flash.fail_write_after(write_index, WriteFaultKind::Partial(cut));
            let first = boot_load_or_provision(&mut flash, &mut PatternRng::new(0x10));
            let first_committed = write_index == 2 && cut == COMMIT_SIZE;
            if first_committed {
                let (material, _) = first.expect("full primary commit reconciles");
                assert_eq!(material.as_bytes(), &expected_generated(0x10));
            } else {
                assert!(matches!(first, Err(IdentityStoreError::Backend(_))));
            }

            flash.write_fault = None;
            let (recovered, report) =
                boot_load_or_provision(&mut flash, &mut PatternRng::new(0x80)).unwrap();
            let first_expected = expected_generated(0x10);
            let recovery_expected = expected_generated(0x80);
            assert_eq!(
                recovered.as_bytes(),
                if first_committed {
                    &first_expected
                } else {
                    &recovery_expected
                },
                "wrong recovered identity after A write {write_index} cut {cut}"
            );
            assert_eq!(report.coverage(), IdentityMirrorCoverage::Redundant);
        }
    }

    // Cuts during peer B happen only after A is committed. Every subsequent
    // boot must therefore retain A's exact identity, regardless of B's state.
    for (local_index, write_len) in write_lengths.into_iter().enumerate() {
        let write_index = 3 + local_index;
        for cut in 0..=write_len {
            let mut flash = FakeNor::erased();
            flash.fail_write_after(write_index, WriteFaultKind::Partial(cut));
            let (first, first_report) =
                boot_load_or_provision(&mut flash, &mut PatternRng::new(0x24)).unwrap();
            assert_eq!(first.as_bytes(), &expected_generated(0x24));
            assert_eq!(
                first_report.coverage(),
                if local_index == 2 && cut == COMMIT_SIZE {
                    IdentityMirrorCoverage::Redundant
                } else {
                    IdentityMirrorCoverage::Single
                }
            );

            flash.write_fault = None;
            let (recovered, recovered_report) =
                boot_load_or_provision(&mut flash, &mut PanicRng).unwrap();
            assert_eq!(recovered.as_bytes(), &expected_generated(0x24));
            assert_eq!(
                recovered_report.coverage(),
                IdentityMirrorCoverage::Redundant
            );
        }
    }

    for write_index in 0..6 {
        let mut flash = FakeNor::erased();
        flash.fail_write_after(write_index, WriteFaultKind::LostReply);
        let first = boot_load_or_provision(&mut flash, &mut PatternRng::new(0x42));
        if write_index == 2 || write_index >= 3 {
            let (material, _) = first.expect("a valid A mirror survives later lost replies");
            assert_eq!(material.as_bytes(), &expected_generated(0x42));
        } else {
            assert!(matches!(first, Err(IdentityStoreError::Backend(_))));
        }
        flash.write_fault = None;
        let (recovered, _) =
            boot_load_or_provision(&mut flash, &mut PatternRng::new(0xa0)).unwrap();
        let first_expected = expected_generated(0x42);
        let recovery_expected = expected_generated(0xa0);
        assert_eq!(
            recovered.as_bytes(),
            if write_index >= 2 {
                &first_expected
            } else {
                &recovery_expected
            }
        );
    }
}

#[test]
fn peer_erase_cuts_leave_the_sole_valid_mirror_authoritative() {
    for cut in 0..=MIRROR_SECTOR_SIZE {
        let mut flash = FakeNor::erased();
        let key = arbitrary_key(0x61);
        seed_mirror(&mut flash, IdentityMirror::A, &key);
        flash.bytes[MIRROR_SECTOR_SIZE..MIRROR_SECTOR_SIZE + 8].copy_from_slice(&CLAIM_MARKER[..8]);
        flash.fail_erase_after(0, EraseFaultKind::Partial(cut));

        let (loaded, report) = boot_load_or_provision(&mut flash, &mut PanicRng).unwrap();
        assert_eq!(loaded.as_bytes(), &key);
        assert_eq!(report.coverage(), IdentityMirrorCoverage::Single);
        assert!(report.repair_deferred());
        assert_eq!(flash.last_erase, Some((MIRROR_SECTOR_SIZE, PARTITION_SIZE)));

        flash.erase_fault = None;
        let (recovered, recovered_report) =
            boot_load_or_provision(&mut flash, &mut PanicRng).unwrap();
        assert_eq!(recovered.as_bytes(), &key);
        assert_eq!(
            recovered_report.coverage(),
            IdentityMirrorCoverage::Redundant
        );
    }

    let mut lost_reply = FakeNor::erased();
    let key = arbitrary_key(0x62);
    seed_mirror(&mut lost_reply, IdentityMirror::A, &key);
    lost_reply.bytes[MIRROR_SECTOR_SIZE..MIRROR_SECTOR_SIZE + 8]
        .copy_from_slice(&CLAIM_MARKER[..8]);
    lost_reply.fail_erase_after(0, EraseFaultKind::LostReply);
    let (loaded, report) = boot_load_or_provision(&mut lost_reply, &mut PanicRng).unwrap();
    assert_eq!(loaded.as_bytes(), &key);
    assert_eq!(report.coverage(), IdentityMirrorCoverage::Single);

    lost_reply.erase_fault = None;
    let (recovered, recovered_report) =
        boot_load_or_provision(&mut lost_reply, &mut PanicRng).unwrap();
    assert_eq!(recovered.as_bytes(), &key);
    assert_eq!(
        recovered_report.coverage(),
        IdentityMirrorCoverage::Redundant
    );
}

#[test]
fn product_can_require_redundant_coverage_and_retry_a_deferred_repair() {
    fn product_accepts(
        result: Result<
            (DurableIdentityMaterial, IdentityBootReport),
            IdentityStoreError<FakeError>,
        >,
    ) -> Option<DurableIdentityMaterial> {
        let (material, report) = result.ok()?;
        (report.coverage() == IdentityMirrorCoverage::Redundant).then_some(material)
    }

    let mut flash = FakeNor::erased();
    flash.fail_write_after(3, WriteFaultKind::Partial(0));
    let first = boot_load_or_provision(&mut flash, &mut PatternRng::new(0x6a));
    assert!(product_accepts(first).is_none());

    flash.write_fault = None;
    let accepted = product_accepts(boot_load_or_provision(&mut flash, &mut PanicRng))
        .expect("a product requiring redundancy accepts the repaired identity");
    assert_eq!(accepted.as_bytes(), &expected_generated(0x6a));
}

#[test]
fn corruption_of_one_mirror_is_repaired_but_nonmonotonic_unknown_without_peer_fails() {
    let mut flash = FakeNor::erased();
    let key = arbitrary_key(0x71);
    seed_mirror(&mut flash, IdentityMirror::A, &key);
    seed_mirror(&mut flash, IdentityMirror::B, &key);
    flash.clear_one_set_bit(DIGEST_OFFSET..COMMIT_OFFSET);
    let (loaded, report) = boot_load_or_provision(&mut flash, &mut PanicRng).unwrap();
    assert_eq!(loaded.as_bytes(), &key);
    assert_eq!(report.source(), IdentityBootSource::Repaired);
    assert_eq!(report.coverage(), IdentityMirrorCoverage::Redundant);
    assert_eq!(flash.last_erase, Some((0, MIRROR_SECTOR_SIZE)));

    let mut unknown = FakeNor::erased();
    unknown.bytes[..CLAIM_SIZE].copy_from_slice(&CLAIM_MARKER);
    unknown.clear_one_set_bit(COMMIT_OFFSET..RECORD_SIZE);
    assert!(matches!(
        boot_load_or_provision(&mut unknown, &mut PanicRng),
        Err(IdentityStoreError::UnknownProgrammedData {
            mirror: IdentityMirror::A
        })
    ));
}
