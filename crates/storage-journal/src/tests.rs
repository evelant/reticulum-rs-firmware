use super::*;

extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use reticulum_storage_model::{
    AUTHORIZATION_KNOWN_PERMISSION_BITS, Accepted, AuthorizationSnapshot, DestinationHash,
    IdempotencyKey, LxmfMessageIntent, MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES,
    MAX_JOURNAL_RECORD_BYTES, MAX_RNS_DATA_BYTES, PrincipalId, RnsDataIntent, encode_journal_entry,
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
}

#[derive(Clone, Copy, Debug)]
enum EraseFaultKind {
    Partial(usize),
    PartialAt { offset: usize, length: usize },
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
}

impl FakeNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
            reads: 0,
            writes: 0,
            erases: 0,
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
            .expect("fixture range must contain a programmed one bit");
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
    const ERASE_SIZE: usize = ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.erases += 1;
        let trigger_fault = self
            .erase_fault
            .is_some_and(|fault| fault.normal_erases_before_fault == 0);
        if !trigger_fault {
            if let Some(fault) = &mut self.erase_fault {
                fault.normal_erases_before_fault -= 1;
            }
            self.bytes[from as usize..to as usize].fill(0xff);
            return Ok(());
        }

        let fault = self.erase_fault.take().expect("erase fault was armed");
        match fault.kind {
            EraseFaultKind::Partial(length) => {
                let end = (from as usize + length).min(to as usize);
                self.bytes[from as usize..end].fill(0xff);
                Err(FakeError::Injected)
            }
            EraseFaultKind::PartialAt { offset, length } => {
                let start = (from as usize + offset).min(to as usize);
                let end = start.saturating_add(length).min(to as usize);
                self.bytes[start..end].fill(0xff);
                Err(FakeError::Injected)
            }
            EraseFaultKind::LostReply => {
                self.bytes[from as usize..to as usize].fill(0xff);
                Err(FakeError::Injected)
            }
        }
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.writes += 1;

        let trigger_fault = self
            .write_fault
            .is_some_and(|fault| fault.normal_writes_before_fault == 0);
        if !trigger_fault {
            if let Some(fault) = &mut self.write_fault {
                fault.normal_writes_before_fault -= 1;
            }
            self.program(offset as usize, bytes);
            return Ok(());
        }

        let fault = self.write_fault.take().expect("fault was armed");
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
}

impl MultiwriteNorFlash for FakeNor {}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

fn accepted(id: u64, principal: u8, key: u8, payload: &[u8]) -> JournalEntry {
    let intent =
        RnsDataIntent::new(DestinationHash::new([0x31; 16]), payload).expect("test intent fits");
    JournalEntry::Accepted(Accepted::new(
        SubmissionId::new(id),
        PrincipalId::new([principal; 16]),
        IdempotencyKey::new([key; 16]),
        intent,
        AuthorizationSnapshot::new(
            [0xa5; 16],
            u64::MAX,
            u64::MAX,
            u32::MAX,
            AUTHORIZATION_KNOWN_PERMISSION_BITS,
        )
        .expect("test authorization is valid"),
    ))
}

fn lxmf_message_accepted(id: u64, principal: u8, key: u8, wire: &[u8]) -> JournalEntry {
    let intent = LxmfMessageIntent::new(wire).expect("test inline LXMF wire fits");
    JournalEntry::Accepted(Accepted::new(
        SubmissionId::new(id),
        PrincipalId::new([principal; 16]),
        IdempotencyKey::new([key; 16]),
        intent,
        AuthorizationSnapshot::new(
            [0xa5; 16],
            u64::MAX,
            u64::MAX,
            u32::MAX,
            AUTHORIZATION_KNOWN_PERMISSION_BITS,
        )
        .expect("test authorization is valid"),
    ))
}

fn formatted() -> FakeNor {
    let mut flash = FakeNor::erased();
    let state = format_erased(&mut flash).expect("erased fake NOR formats");
    assert_eq!(state.bank(), Bank::A);
    assert_eq!(state.generation(), 1);
    flash
}

fn accepted_value(entry: JournalEntry) -> Accepted {
    match entry {
        JournalEntry::Accepted(accepted) => accepted,
        _ => panic!("fixture must be an acceptance"),
    }
}

fn seeded_two() -> (FakeNor, [JournalEntry; 2]) {
    let mut flash = formatted();
    let entries = [
        accepted(1, 0x11, 0x21, b"first"),
        accepted(2, 0x12, 0x22, b"second"),
    ];
    for entry in entries {
        append::<4, _>(&mut flash, SubmissionId::new(1), entry).expect("seed append succeeds");
    }
    (flash, entries)
}

fn assert_complete_replay(flash: &mut FakeNor, entries: [JournalEntry; 2]) -> JournalState {
    let mounted = mount::<4, _>(flash, SubmissionId::new(1)).expect("complete journal mounts");
    assert_eq!(mounted.state().committed_records(), entries.len());
    assert_eq!(mounted.state().accepted_submissions(), entries.len());
    for entry in entries {
        let accepted = accepted_value(entry);
        assert_eq!(
            mounted
                .index()
                .get(accepted.id())
                .expect("submission is replayed")
                .accepted(),
            accepted
        );
    }
    mounted.state()
}

fn assert_complete_old_or_new(
    flash: &mut FakeNor,
    entries: [JournalEntry; 2],
    old_bank: Bank,
    old_generation: u64,
) -> JournalState {
    let state = assert_complete_replay(flash, entries);
    assert!(
        (state.bank() == old_bank && state.generation() == old_generation)
            || (state.bank() == old_bank.other() && state.generation() == old_generation + 1),
        "mount selected neither complete generation"
    );
    state
}

fn assert_new_append_blocked(flash: &mut FakeNor) {
    assert_eq!(
        append::<4, _>(
            flash,
            SubmissionId::new(1),
            accepted(3, 0x13, 0x23, b"must-wait"),
        ),
        Err(JournalError::CompactionPending)
    );
}

fn assert_backend_write_fault<T>(result: Result<T, JournalError<FakeError>>) {
    assert_eq!(
        result.err(),
        Some(JournalError::Backend(FakeError::Injected))
    );
}

fn canonical_empty_manifest() -> [u8; MANIFEST_SIZE] {
    encode_manifest(Manifest {
        bank: Bank::A,
        generation: 1,
        baseline_count: 0,
        baseline_tail: ZERO_DIGEST,
    })
}

fn encode_manifest_for_schema(manifest: Manifest, schema: u16) -> [u8; MANIFEST_SIZE] {
    let mut encoded = encode_manifest(manifest);
    put_u16(&mut encoded, 10, schema);
    let digest = manifest_digest(&encoded[..MANIFEST_DATA_SIZE]);
    encoded[MANIFEST_DATA_SIZE..MANIFEST_PREFIX_SIZE].copy_from_slice(&digest);
    encoded
}

fn encode_manifest_for_physical(manifest: Manifest, physical: u16) -> [u8; MANIFEST_SIZE] {
    let mut encoded = encode_manifest(manifest);
    put_u16(&mut encoded, 8, physical);
    let digest = manifest_digest(&encoded[..MANIFEST_DATA_SIZE]);
    encoded[MANIFEST_DATA_SIZE..MANIFEST_PREFIX_SIZE].copy_from_slice(&digest);
    encoded
}

#[test]
fn geometry_exactly_fills_the_fixed_partition_contract() {
    assert_eq!(PARTITION_SIZE, 1024 * 1024);
    assert_eq!(MANIFEST_A_OFFSET, 0);
    assert_eq!(MANIFEST_B_OFFSET, ERASE_SIZE);
    assert_eq!(BANK_A_OFFSET, 2 * ERASE_SIZE);
    assert_eq!(BANK_B_OFFSET, 0x81000);
    assert_eq!(BANK_A_OFFSET + BANK_SIZE, BANK_B_OFFSET);
    assert_eq!(BANK_B_OFFSET + BANK_SIZE, PARTITION_SIZE);
    assert_eq!(BANK_SLOT_COUNT * SLOT_SIZE + BANK_TAIL_SIZE, BANK_SIZE);
    assert_eq!(BANK_SLOT_COUNT, 774);
    assert_eq!(
        SLOT_HEADER_SIZE + BODY_SIZE + DIGEST_SIZE + COMMIT_SIZE,
        SLOT_SIZE
    );
    assert_eq!(BANK_TAIL_SIZE, 64);
    assert_eq!(MAX_ACCEPTED_SUBMISSIONS, 154);
}

#[test]
fn largest_semantically_permitted_record_uses_538_body_bytes_and_round_trips() {
    assert_eq!(BODY_SIZE, MAX_JOURNAL_RECORD_BYTES);
    let mut wire = [0x5a; MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES];
    wire[..16].fill(0x31);
    let entry = lxmf_message_accepted(u64::MAX, 0xff, 0xee, &wire);
    let mut canonical = [0_u8; BODY_SIZE];
    let encoded_len = encode_journal_entry(&entry, &mut canonical)
        .expect("the semantic maximum must fit the physical body");

    // The current method-neutral LXMF variant retains all 431 signed wire bytes
    // and maximum-width identifiers and provenance in one canonical record.
    assert_eq!(encoded_len, 538);
    assert_eq!(BODY_SIZE - encoded_len, 6);

    // Raising durable inline-LXMF capacity by one byte is rejected by the
    // semantic constructor rather than consuming physical format headroom.
    let oversized_wire = [0_u8; MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES + 1];
    let semantic_error = LxmfMessageIntent::new(&oversized_wire)
        .expect_err("wire above the inline message ceiling must be rejected");
    assert_eq!(
        semantic_error.actual(),
        MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES + 1
    );
    assert_eq!(semantic_error.maximum(), MAX_INLINE_LXMF_MESSAGE_WIRE_BYTES);

    // Generic destination DATA remains independently constrained to 383 bytes.
    let oversized_payload = [0_u8; MAX_RNS_DATA_BYTES + 1];
    let semantic_error = RnsDataIntent::new(DestinationHash::new([0x31; 16]), &oversized_payload)
        .expect_err("generic DATA must retain its protocol MDU");
    assert_eq!(semantic_error.actual(), MAX_RNS_DATA_BYTES + 1);
    assert_eq!(semantic_error.maximum(), MAX_RNS_DATA_BYTES);

    let mut flash = formatted();
    let AppendOutcome::Appended(state) =
        append::<1, _>(&mut flash, SubmissionId::new(u64::MAX), entry)
            .expect("maximum semantic record appends")
    else {
        panic!("fresh maximum record must allocate a slot")
    };
    assert_eq!(state.committed_records(), 1);
    assert_eq!(
        read_u16(
            &flash.bytes[BANK_A_OFFSET..BANK_A_OFFSET + SLOT_HEADER_SIZE],
            14
        ) as usize,
        encoded_len
    );
    assert_eq!(
        flash.bytes
            [BANK_A_OFFSET + SLOT_HEADER_SIZE..BANK_A_OFFSET + SLOT_HEADER_SIZE + encoded_len],
        canonical[..encoded_len]
    );
    assert!(
        flash.bytes
            [BANK_A_OFFSET + SLOT_HEADER_SIZE + encoded_len..BANK_A_OFFSET + SLOT_DIGEST_OFFSET]
            .iter()
            .all(|byte| *byte == 0xff)
    );

    let mounted = mount::<1, _>(&mut flash, SubmissionId::new(u64::MAX))
        .expect("maximum semantic record mounts");
    assert_eq!(mounted.state(), state);
    assert_eq!(
        mounted
            .index()
            .get(SubmissionId::new(u64::MAX))
            .expect("maximum record replays")
            .accepted(),
        accepted_value(entry)
    );
}

#[test]
fn format_is_explicit_requires_every_byte_erased_and_never_erases() {
    let mut erased = FakeNor::erased();
    assert!(matches!(
        mount::<1, _>(&mut erased, SubmissionId::new(1)),
        Err(JournalError::UnformattedErased)
    ));
    let state = format_erased(&mut erased).expect("explicit format succeeds");
    assert_eq!(state.committed_records(), 0);
    assert_eq!(erased.erases, 0);
    assert_eq!(erased.writes, 2, "manifest prefix and commit only");

    let mut nonblank = FakeNor::erased();
    nonblank.bytes[BANK_A_OFFSET + 17] = 0xfe;
    assert_eq!(
        format_erased(&mut nonblank),
        Err(JournalError::UnformattedNonErased)
    );
    assert_eq!(nonblank.erases, 0);
    assert_eq!(nonblank.writes, 0);

    assert_eq!(
        format_erased(&mut erased),
        Err(JournalError::UnformattedNonErased)
    );
}

#[test]
fn non_current_manifest_is_typed_unsupported_and_all_mount_paths_are_read_only() {
    let non_current_schema = JOURNAL_SCHEMA_VERSION - 1;
    let non_current_manifest = encode_manifest_for_schema(
        Manifest {
            bank: Bank::A,
            generation: 1,
            baseline_count: 0,
            baseline_tail: ZERO_DIGEST,
        },
        non_current_schema,
    );
    let mut flash = FakeNor::erased();
    flash.bytes[..MANIFEST_SIZE].copy_from_slice(&non_current_manifest);
    let before = flash.bytes.clone();
    let writes = flash.writes;
    let erases = flash.erases;

    assert!(matches!(
        mount::<1, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::UnsupportedSemanticVersion(version)) if version == non_current_schema
    ));
    assert!(matches!(
        append::<1, _>(
            &mut flash,
            SubmissionId::new(1),
            accepted(1, 0x11, 0x22, b"must not append"),
        ),
        Err(JournalError::UnsupportedSemanticVersion(version)) if version == non_current_schema
    ));
    assert!(matches!(
        compact::<1, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::UnsupportedSemanticVersion(version)) if version == non_current_schema
    ));

    assert_eq!(flash.bytes, before);
    assert_eq!(flash.writes, writes);
    assert_eq!(flash.erases, erases);
}

#[test]
fn malformed_non_current_manifest_remains_manifest_corruption() {
    let mut non_current_manifest = encode_manifest_for_schema(
        Manifest {
            bank: Bank::A,
            generation: 1,
            baseline_count: 0,
            baseline_tail: ZERO_DIGEST,
        },
        JOURNAL_SCHEMA_VERSION - 1,
    );
    non_current_manifest[MANIFEST_DATA_SIZE] ^= 0x01;
    let mut flash = FakeNor::erased();
    flash.bytes[..MANIFEST_SIZE].copy_from_slice(&non_current_manifest);
    let before = flash.bytes.clone();

    assert!(matches!(
        mount::<1, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::ManifestCorrupt)
    ));
    assert_eq!(flash.bytes, before);
    assert_eq!(flash.writes, 0);
    assert_eq!(flash.erases, 0);
}

#[test]
fn non_current_physical_manifest_is_typed_unsupported_and_all_mount_paths_are_read_only() {
    let non_current_physical = PHYSICAL_FORMAT_VERSION - 1;
    let non_current_manifest = encode_manifest_for_physical(
        Manifest {
            bank: Bank::A,
            generation: 1,
            baseline_count: 0,
            baseline_tail: ZERO_DIGEST,
        },
        non_current_physical,
    );
    let mut flash = FakeNor::erased();
    flash.bytes[..MANIFEST_SIZE].copy_from_slice(&non_current_manifest);
    let before = flash.bytes.clone();
    let writes = flash.writes;
    let erases = flash.erases;

    assert!(matches!(
        mount::<1, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::UnsupportedPhysicalVersion(version)) if version == non_current_physical
    ));
    assert!(matches!(
        append::<1, _>(
            &mut flash,
            SubmissionId::new(1),
            accepted(1, 0x11, 0x22, b"must not append"),
        ),
        Err(JournalError::UnsupportedPhysicalVersion(version)) if version == non_current_physical
    ));
    assert!(matches!(
        compact::<1, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::UnsupportedPhysicalVersion(version)) if version == non_current_physical
    ));

    assert_eq!(flash.bytes, before);
    assert_eq!(flash.writes, writes);
    assert_eq!(flash.erases, erases);
}

#[test]
fn unsupported_schema_manifests_preserve_two_bank_trajectory_classification() {
    fn assert_read_only_error(mut flash: FakeNor, expected: JournalError<FakeError>) {
        let before = flash.bytes.clone();
        match mount::<1, _>(&mut flash, SubmissionId::new(1)) {
            Err(error) => assert_eq!(error, expected),
            Ok(_) => panic!("invalid manifest trajectory unexpectedly mounted"),
        }
        match append::<1, _>(
            &mut flash,
            SubmissionId::new(1),
            accepted(1, 0x11, 0x22, b"must not append"),
        ) {
            Err(error) => assert_eq!(error, expected),
            Ok(_) => panic!("invalid manifest trajectory unexpectedly appended"),
        }
        match compact::<1, _>(&mut flash, SubmissionId::new(1)) {
            Err(error) => assert_eq!(error, expected),
            Ok(_) => panic!("invalid manifest trajectory unexpectedly compacted"),
        }
        assert_eq!(flash.bytes, before);
        assert_eq!(flash.writes, 0);
        assert_eq!(flash.erases, 0);
    }

    let non_current_schema = JOURNAL_SCHEMA_VERSION - 1;
    let different_non_current_schema = JOURNAL_SCHEMA_VERSION - 2;
    let non_current_a1 = encode_manifest_for_schema(
        Manifest {
            bank: Bank::A,
            generation: 1,
            baseline_count: 0,
            baseline_tail: ZERO_DIGEST,
        },
        non_current_schema,
    );
    let mut interrupted_first_generation = FakeNor::erased();
    interrupted_first_generation.bytes[MANIFEST_A_OFFSET..MANIFEST_A_OFFSET + MANIFEST_SIZE]
        .copy_from_slice(&non_current_a1);
    interrupted_first_generation.bytes[MANIFEST_B_OFFSET] = 0xfe;
    assert_read_only_error(interrupted_first_generation, JournalError::ManifestCorrupt);

    let non_current_a2 = encode_manifest_for_schema(
        Manifest {
            bank: Bank::A,
            generation: 2,
            baseline_count: 0,
            baseline_tail: ZERO_DIGEST,
        },
        non_current_schema,
    );
    let mut interrupted_retirement = FakeNor::erased();
    interrupted_retirement.bytes[MANIFEST_A_OFFSET..MANIFEST_A_OFFSET + MANIFEST_SIZE]
        .copy_from_slice(&non_current_a2);
    interrupted_retirement.bytes[MANIFEST_B_OFFSET] = 0xfe;
    assert_read_only_error(
        interrupted_retirement,
        JournalError::UnsupportedSemanticVersion(non_current_schema),
    );

    for (a_generation, a_schema, b_generation, b_schema, expected) in [
        (
            1,
            non_current_schema,
            1,
            non_current_schema,
            JournalError::ManifestConflict,
        ),
        (
            1,
            non_current_schema,
            3,
            non_current_schema,
            JournalError::ManifestConflict,
        ),
        (1, non_current_schema, 2, 0, JournalError::ManifestConflict),
        (
            1,
            non_current_schema,
            2,
            different_non_current_schema,
            JournalError::ManifestConflict,
        ),
        (
            1,
            non_current_schema,
            2,
            non_current_schema,
            JournalError::UnsupportedSemanticVersion(non_current_schema),
        ),
    ] {
        let mut flash = FakeNor::erased();
        let a = encode_manifest_for_schema(
            Manifest {
                bank: Bank::A,
                generation: a_generation,
                baseline_count: 0,
                baseline_tail: ZERO_DIGEST,
            },
            a_schema,
        );
        let b = encode_manifest_for_schema(
            Manifest {
                bank: Bank::B,
                generation: b_generation,
                baseline_count: 0,
                baseline_tail: ZERO_DIGEST,
            },
            b_schema,
        );
        flash.bytes[MANIFEST_A_OFFSET..MANIFEST_A_OFFSET + MANIFEST_SIZE].copy_from_slice(&a);
        flash.bytes[MANIFEST_B_OFFSET..MANIFEST_B_OFFSET + MANIFEST_SIZE].copy_from_slice(&b);
        assert_read_only_error(flash, expected);
    }
}

#[test]
fn every_format_manifest_write_byte_cut_never_publishes_a_partial_manifest() {
    let writes = [MANIFEST_PREFIX_SIZE, COMMIT_SIZE];
    for (write_index, write_len) in writes.into_iter().enumerate() {
        for cut in 0..=write_len {
            let mut flash = FakeNor::erased();
            flash.fail_write_after(write_index, WriteFaultKind::Partial(cut));
            assert_backend_write_fault(format_erased(&mut flash));

            let mounted = mount::<1, _>(&mut flash, SubmissionId::new(1));
            let commit_is_complete = write_index == 1 && cut == COMMIT_SIZE;
            if commit_is_complete {
                let mounted = mounted.expect("a fully programmed commit marker publishes format");
                assert_eq!(mounted.state().bank(), Bank::A);
                assert_eq!(mounted.state().generation(), 1);
                assert_eq!(mounted.state().committed_records(), 0);
            } else if write_index == 0 && cut == 0 {
                assert!(matches!(mounted, Err(JournalError::UnformattedErased)));
            } else {
                assert!(matches!(mounted, Err(JournalError::UnformattedNonErased)));
            }
        }
    }
}

#[test]
fn first_provision_requires_explicit_authority_and_existing_empty_a1_is_read_only() {
    let mut flash = FakeNor::erased();
    let erased = flash.bytes.clone();
    assert_eq!(
        provision_first(&mut flash, FreshJournalPolicy::Reject),
        Err(FirstProvisionError::NotAuthorized)
    );
    assert_eq!(flash.bytes, erased);
    assert_eq!(flash.writes, 0);
    assert_eq!(flash.erases, 0);

    let state = provision_first(&mut flash, FreshJournalPolicy::AllowFirstProvision)
        .expect("independently authorized erased media provisions");
    assert_eq!(state, empty_generation_one_state());
    assert_eq!(flash.writes, 2);
    assert_eq!(flash.erases, 0);
    assert_eq!(
        mount::<1, _>(&mut flash, SubmissionId::new(1))
            .unwrap()
            .state(),
        state
    );

    let committed = flash.bytes.clone();
    let writes = flash.writes;
    assert_eq!(
        provision_first(&mut flash, FreshJournalPolicy::Reject),
        Ok(state)
    );
    assert_eq!(flash.bytes, committed);
    assert_eq!(flash.writes, writes);
    assert_eq!(flash.erases, 0);
}

#[test]
fn every_first_provision_prefix_and_commit_byte_cut_resumes_only_when_authorized() {
    let writes = [MANIFEST_PREFIX_SIZE, COMMIT_SIZE];
    for (write_index, write_len) in writes.into_iter().enumerate() {
        for cut in 0..=write_len {
            let mut flash = FakeNor::erased();
            flash.fail_write_after(write_index, WriteFaultKind::Partial(cut));
            assert_eq!(
                provision_first(&mut flash, FreshJournalPolicy::AllowFirstProvision),
                Err(FirstProvisionError::Backend(FakeError::Injected)),
                "initial fault write={write_index} cut={cut}"
            );

            let before_reject = flash.bytes.clone();
            let writes_before_reject = flash.writes;
            let fully_committed = write_index == 1 && cut == COMMIT_SIZE;
            let rejected = provision_first(&mut flash, FreshJournalPolicy::Reject);
            if fully_committed {
                assert_eq!(rejected, Ok(empty_generation_one_state()));
            } else {
                assert_eq!(rejected, Err(FirstProvisionError::NotAuthorized));
            }
            assert_eq!(
                flash.bytes, before_reject,
                "Reject mutated write={write_index} cut={cut}"
            );
            assert_eq!(flash.writes, writes_before_reject);
            assert_eq!(flash.erases, 0);

            let recovered =
                provision_first(&mut flash, FreshJournalPolicy::AllowFirstProvision).unwrap();
            assert_eq!(recovered, empty_generation_one_state());
            assert_eq!(flash.erases, 0);
            assert_eq!(
                mount::<1, _>(&mut flash, SubmissionId::new(1))
                    .unwrap()
                    .state(),
                recovered,
                "recovery did not mount write={write_index} cut={cut}"
            );

            let writes_after_recovery = flash.writes;
            assert_eq!(
                provision_first(&mut flash, FreshJournalPolicy::AllowFirstProvision),
                Ok(recovered)
            );
            assert_eq!(flash.writes, writes_after_recovery);
        }
    }
}

#[test]
fn lost_first_provision_write_replies_reconcile_without_duplicate_formatting() {
    for write_index in 0..2 {
        let mut flash = FakeNor::erased();
        flash.fail_write_after(write_index, WriteFaultKind::LostReply);
        assert_eq!(
            provision_first(&mut flash, FreshJournalPolicy::AllowFirstProvision),
            Err(FirstProvisionError::Backend(FakeError::Injected))
        );

        let recovered =
            provision_first(&mut flash, FreshJournalPolicy::AllowFirstProvision).unwrap();
        assert_eq!(recovered, empty_generation_one_state());
        assert_eq!(flash.erases, 0);
        assert_eq!(
            mount::<1, _>(&mut flash, SubmissionId::new(1))
                .unwrap()
                .state(),
            recovered
        );

        let writes = flash.writes;
        assert_eq!(
            provision_first(&mut flash, FreshJournalPolicy::Reject),
            Ok(recovered)
        );
        assert_eq!(flash.writes, writes);
    }
}

#[test]
fn first_provision_accepts_noncontiguous_monotonic_compatible_programming() {
    let expected = canonical_empty_manifest();

    let mut prefix_torn = FakeNor::erased();
    for (index, target) in expected[..MANIFEST_PREFIX_SIZE]
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, _)| index % 2 == 0)
    {
        prefix_torn.bytes[index] &= target;
    }
    assert_ne!(
        prefix_torn.bytes[..MANIFEST_PREFIX_SIZE],
        expected[..MANIFEST_PREFIX_SIZE]
    );
    assert_eq!(
        provision_first(&mut prefix_torn, FreshJournalPolicy::AllowFirstProvision,),
        Ok(empty_generation_one_state())
    );

    let mut commit_torn = FakeNor::erased();
    commit_torn.bytes[..MANIFEST_PREFIX_SIZE].copy_from_slice(&expected[..MANIFEST_PREFIX_SIZE]);
    for (index, target) in expected[MANIFEST_PREFIX_SIZE..]
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, _)| index % 2 == 0)
    {
        commit_torn.bytes[MANIFEST_PREFIX_SIZE + index] &= target;
    }
    assert_ne!(commit_torn.bytes[..MANIFEST_SIZE], expected);
    assert_eq!(
        provision_first(&mut commit_torn, FreshJournalPolicy::AllowFirstProvision,),
        Ok(empty_generation_one_state())
    );

    for flash in [&mut prefix_torn, &mut commit_torn] {
        assert_eq!(flash.erases, 0);
        assert_eq!(
            mount::<1, _>(flash, SubmissionId::new(1)).unwrap().state(),
            empty_generation_one_state()
        );
    }
}

#[test]
fn authorized_first_provision_rejects_nonempty_and_off_trajectory_media_without_mutation() {
    let expected = canonical_empty_manifest();

    let mut outside_manifest = FakeNor::erased();
    outside_manifest.bytes[BANK_A_OFFSET + 17] = 0xfe;

    let mut incompatible_prefix = FakeNor::erased();
    let (incompatible_index, incompatible_byte) = expected[..MANIFEST_PREFIX_SIZE]
        .iter()
        .copied()
        .enumerate()
        .find(|(_, byte)| *byte != 0)
        .unwrap();
    let programmed_one_bit = 1_u8 << incompatible_byte.trailing_zeros();
    incompatible_prefix.bytes[incompatible_index] &= !programmed_one_bit;

    let mut commit_before_prefix = FakeNor::erased();
    commit_before_prefix.program(0, &expected[..4]);
    commit_before_prefix.program(
        MANIFEST_PREFIX_SIZE,
        &expected[MANIFEST_PREFIX_SIZE..MANIFEST_PREFIX_SIZE + 4],
    );

    let mut nonempty = formatted();
    let entry = accepted(1, 0x41, 0x42, b"must not become a fresh journal");
    assert!(matches!(
        append::<2, _>(&mut nonempty, SubmissionId::new(1), entry),
        Ok(AppendOutcome::Appended(_))
    ));

    for (name, media) in [
        ("outside-manifest", outside_manifest),
        ("incompatible-prefix", incompatible_prefix),
        ("commit-before-prefix", commit_before_prefix),
        ("valid-nonempty", nonempty),
    ] {
        for policy in [
            FreshJournalPolicy::Reject,
            FreshJournalPolicy::AllowFirstProvision,
        ] {
            let mut flash = media.clone();
            let before = flash.bytes.clone();
            let writes = flash.writes;
            let erases = flash.erases;
            assert_eq!(
                provision_first(&mut flash, policy),
                Err(FirstProvisionError::MediaNotProvisionable),
                "classification name={name} policy={policy:?}"
            );
            assert_eq!(
                flash.bytes, before,
                "mutation name={name} policy={policy:?}"
            );
            assert_eq!(flash.writes, writes);
            assert_eq!(flash.erases, erases);
        }
    }
}

#[test]
fn append_mount_and_exact_retry_are_idempotent_without_another_write() {
    let mut flash = formatted();
    let entry = accepted(1, 0x11, 0x22, b"durable");
    let AppendOutcome::Appended(appended) =
        append::<4, _>(&mut flash, SubmissionId::new(1), entry).expect("append succeeds")
    else {
        panic!("first append must allocate a slot")
    };
    assert_eq!(appended.committed_records(), 1);
    assert_eq!(appended.consumed_slots(), 1);

    let mounted = mount::<4, _>(&mut flash, SubmissionId::new(1)).expect("mount succeeds");
    assert_eq!(mounted.state(), appended);
    assert_eq!(
        mounted
            .index()
            .get(SubmissionId::new(1))
            .unwrap()
            .accepted(),
        match entry {
            JournalEntry::Accepted(value) => value,
            _ => unreachable!(),
        }
    );

    let writes_before_retry = flash.writes;
    assert_eq!(
        append::<4, _>(&mut flash, SubmissionId::new(1), entry),
        Ok(AppendOutcome::AlreadyEquivalent(appended))
    );
    assert_eq!(flash.writes, writes_before_retry);
}

#[test]
fn every_append_write_byte_cut_recovers_exactly_once_on_retry() {
    let base = formatted();
    let entry = accepted(1, 0x51, 0x61, b"exhaustive-append-cut");
    let writes = [SLOT_COMMIT_OFFSET, COMMIT_SIZE];

    for (write_index, write_len) in writes.into_iter().enumerate() {
        for cut in 0..=write_len {
            let mut flash = base.clone();
            flash.fail_write_after(write_index, WriteFaultKind::Partial(cut));
            assert_backend_write_fault(append::<2, _>(&mut flash, SubmissionId::new(1), entry));

            let writes_after_cut = flash.writes;
            let outcome = append::<2, _>(&mut flash, SubmissionId::new(1), entry)
                .expect("retry must recover the interrupted append");
            let commit_is_complete = write_index == 1 && cut == COMMIT_SIZE;
            let state = match outcome {
                AppendOutcome::AlreadyEquivalent(state) if commit_is_complete => {
                    assert_eq!(
                        flash.writes, writes_after_cut,
                        "a lost reply after the full marker must not rewrite"
                    );
                    state
                }
                AppendOutcome::Appended(state) if !commit_is_complete => state,
                unexpected => panic!(
                    "unexpected retry outcome at write {write_index}, byte cut {cut}: {unexpected:?}"
                ),
            };
            assert_eq!(state.committed_records(), 1);
            assert_eq!(state.accepted_submissions(), 1);
            assert_eq!(
                state.consumed_slots(),
                if (write_index == 0 && cut == 0) || commit_is_complete {
                    1
                } else {
                    2
                },
                "wrong physical slot consumption at write {write_index}, byte cut {cut}"
            );
        }
    }
}

#[test]
fn same_logical_key_with_different_content_is_a_conflict_without_writing() {
    let mut flash = formatted();
    let original = accepted(1, 1, 2, b"original");
    append::<2, _>(&mut flash, SubmissionId::new(1), original).unwrap();
    let writes_before_conflict = flash.writes;

    let conflict = accepted(1, 1, 2, b"different");
    assert_eq!(
        append::<2, _>(&mut flash, SubmissionId::new(1), conflict),
        Err(JournalError::LogicalConflict)
    );
    assert_eq!(flash.writes, writes_before_conflict);
}

#[test]
fn acceptance_reservation_admits_154_and_rejects_155_before_flash_mutation() {
    const INDEX_CAPACITY: usize = MAX_ACCEPTED_SUBMISSIONS + 1;
    let mut flash = formatted();

    for ordinal in 0..MAX_ACCEPTED_SUBMISSIONS {
        let id = (ordinal + 1) as u64;
        let entry = accepted(id, 0x70, ordinal as u8, &[ordinal as u8]);
        let AppendOutcome::Appended(state) =
            append::<INDEX_CAPACITY, _>(&mut flash, SubmissionId::new(1), entry)
                .unwrap_or_else(|error| panic!("acceptance {id} was rejected: {error:?}"))
        else {
            panic!("acceptance {id} unexpectedly deduplicated")
        };
        assert_eq!(state.accepted_submissions(), ordinal + 1);
        assert_eq!(state.committed_records(), ordinal + 1);
    }

    let full = mount::<INDEX_CAPACITY, _>(&mut flash, SubmissionId::new(1))
        .expect("all 154 reserved acceptances mount");
    assert_eq!(
        full.state().accepted_submissions(),
        MAX_ACCEPTED_SUBMISSIONS
    );
    assert_eq!(full.state().committed_records(), MAX_ACCEPTED_SUBMISSIONS);
    assert_eq!(full.state().consumed_slots(), MAX_ACCEPTED_SUBMISSIONS);
    assert_eq!(full.index().iter().count(), MAX_ACCEPTED_SUBMISSIONS);

    let bytes_before = flash.bytes.clone();
    let writes_before = flash.writes;
    let erases_before = flash.erases;
    let rejected = accepted(
        (MAX_ACCEPTED_SUBMISSIONS + 1) as u64,
        0x70,
        MAX_ACCEPTED_SUBMISSIONS as u8,
        b"reserved-terminal-capacity",
    );
    assert_eq!(
        append::<INDEX_CAPACITY, _>(&mut flash, SubmissionId::new(1), rejected),
        Err(JournalError::AcceptanceCapacityExhausted)
    );
    assert_eq!(flash.writes, writes_before);
    assert_eq!(flash.erases, erases_before);
    assert_eq!(flash.bytes, bytes_before);
}

#[test]
fn torn_prefix_is_a_hole_and_a_later_committed_slot_is_scanned() {
    let mut flash = formatted();
    let entry = accepted(1, 1, 1, b"after-hole");
    flash.fail_write_after(0, WriteFaultKind::Partial(SLOT_HEADER_SIZE));
    assert_eq!(
        append::<2, _>(&mut flash, SubmissionId::new(1), entry),
        Err(JournalError::Backend(FakeError::Injected))
    );
    assert!(
        flash.bytes[BANK_A_OFFSET..BANK_A_OFFSET + SLOT_HEADER_SIZE]
            .iter()
            .any(|byte| *byte != 0xff)
    );
    assert!(
        flash.bytes[BANK_A_OFFSET + SLOT_COMMIT_OFFSET..BANK_A_OFFSET + SLOT_SIZE]
            .iter()
            .all(|byte| *byte == 0xff)
    );

    let AppendOutcome::Appended(state) =
        append::<2, _>(&mut flash, SubmissionId::new(1), entry).expect("retry skips torn slot")
    else {
        panic!("torn record was never committed")
    };
    assert_eq!(state.committed_records(), 1);
    assert_eq!(state.consumed_slots(), 2);

    let mounted = mount::<2, _>(&mut flash, SubmissionId::new(1)).expect("full scan succeeds");
    assert_eq!(mounted.state(), state);
}

#[test]
fn lost_commit_reply_is_discovered_and_retry_performs_no_write() {
    let mut flash = formatted();
    let entry = accepted(1, 3, 4, b"ambiguous-commit");
    flash.fail_write_after(1, WriteFaultKind::LostReply);
    assert_eq!(
        append::<2, _>(&mut flash, SubmissionId::new(1), entry),
        Err(JournalError::Backend(FakeError::Injected))
    );
    let writes_after_lost_reply = flash.writes;

    let outcome = append::<2, _>(&mut flash, SubmissionId::new(1), entry)
        .expect("retry finds committed record");
    let AppendOutcome::AlreadyEquivalent(state) = outcome else {
        panic!("lost reply must not duplicate the committed record")
    };
    assert_eq!(state.committed_records(), 1);
    assert_eq!(state.consumed_slots(), 1);
    assert_eq!(flash.writes, writes_after_lost_reply);
}

#[test]
fn protected_byte_corruption_is_rejected_by_the_slot_digest() {
    let mut flash = formatted();
    append::<2, _>(
        &mut flash,
        SubmissionId::new(1),
        accepted(1, 1, 1, b"protected"),
    )
    .unwrap();
    flash.clear_one_set_bit(BANK_A_OFFSET + SLOT_HEADER_SIZE..BANK_A_OFFSET + SLOT_DIGEST_OFFSET);

    assert!(matches!(
        mount::<2, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::SlotCorrupt {
            slot: 0,
            reason: SlotCorruption::Digest,
        })
    ));
}

#[test]
fn non_monotonic_commit_marker_corruption_is_not_treated_as_a_torn_hole() {
    let mut flash = formatted();
    append::<2, _>(
        &mut flash,
        SubmissionId::new(1),
        accepted(1, 1, 1, b"commit-marker"),
    )
    .unwrap();
    flash.clear_one_set_bit(BANK_A_OFFSET + SLOT_COMMIT_OFFSET..BANK_A_OFFSET + SLOT_SIZE);

    assert!(matches!(
        mount::<2, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::SlotCorrupt {
            slot: 0,
            reason: SlotCorruption::CommitMarker,
        })
    ));
}

#[test]
fn forged_second_committed_copy_of_a_logical_key_is_rejected() {
    let mut flash = formatted();
    let entry = accepted(1, 7, 8, b"duplicate");
    let AppendOutcome::Appended(first) =
        append::<2, _>(&mut flash, SubmissionId::new(1), entry).unwrap()
    else {
        unreachable!()
    };

    let mut forged = [0xff; SLOT_SIZE];
    encode_slot::<FakeError>(
        &mut forged,
        Bank::A,
        first.generation(),
        first.committed_records(),
        first.tail_digest(),
        entry,
    )
    .unwrap();
    NorFlash::write(&mut flash, (BANK_A_OFFSET + SLOT_SIZE) as u32, &forged).unwrap();

    assert!(matches!(
        mount::<2, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::DuplicateLogicalKey)
    ));
}

#[test]
fn compaction_round_trips_a_to_b_and_b_to_a_with_exact_semantic_replay() {
    let (mut flash, entries) = seeded_two();
    let initial = assert_complete_replay(&mut flash, entries);
    assert_eq!(initial.bank(), Bank::A);
    assert_eq!(initial.generation(), 1);

    let on_b = compact::<4, _>(&mut flash, SubmissionId::new(1)).expect("A to B compacts");
    assert_eq!(on_b.bank(), Bank::B);
    assert_eq!(on_b.generation(), 2);
    assert_eq!(on_b.committed_records(), entries.len());
    assert_eq!(on_b.consumed_slots(), entries.len());
    assert!(!on_b.compaction_pending());
    assert_eq!(assert_complete_replay(&mut flash, entries), on_b);

    let on_a = compact::<4, _>(&mut flash, SubmissionId::new(1)).expect("B to A compacts");
    assert_eq!(on_a.bank(), Bank::A);
    assert_eq!(on_a.generation(), 3);
    assert_eq!(on_a.committed_records(), entries.len());
    assert_eq!(on_a.consumed_slots(), entries.len());
    assert!(!on_a.compaction_pending());
    assert_eq!(assert_complete_replay(&mut flash, entries), on_a);
}

#[test]
fn retired_manifest_prevents_predecessor_resurrection_after_successor_corruption() {
    let mut base = formatted();
    let first = accepted(1, 0x11, 0x21, b"copied baseline");
    append::<4, _>(&mut base, SubmissionId::new(1), first).expect("baseline append succeeds");

    let on_b = compact::<4, _>(&mut base, SubmissionId::new(1)).expect("A1 compacts to B2");
    assert_eq!(on_b.bank(), Bank::B);
    assert_eq!(on_b.generation(), 2);
    assert!(
        base.bytes[MANIFEST_A_OFFSET..MANIFEST_A_OFFSET + ERASE_SIZE]
            .iter()
            .all(|byte| *byte == 0xff),
        "the superseded A1 manifest must be retired"
    );
    assert!(
        base.bytes[BANK_A_OFFSET..BANK_A_OFFSET + SLOT_SIZE]
            .iter()
            .any(|byte| *byte != 0xff),
        "retirement must preserve the old record bank"
    );

    let suffix = accepted(2, 0x12, 0x22, b"new-generation suffix");
    let AppendOutcome::Appended(with_suffix) =
        append::<4, _>(&mut base, SubmissionId::new(1), suffix).expect("B2 suffix append succeeds")
    else {
        panic!("suffix must allocate a slot")
    };
    assert_eq!(with_suffix.bank(), Bank::B);
    assert_eq!(with_suffix.generation(), 2);
    assert_eq!(with_suffix.committed_records(), 2);

    let corruptions = [
        (
            "protected manifest prefix",
            MANIFEST_B_OFFSET..MANIFEST_B_OFFSET + MANIFEST_PREFIX_SIZE,
        ),
        (
            "manifest commit marker",
            MANIFEST_B_OFFSET + MANIFEST_PREFIX_SIZE..MANIFEST_B_OFFSET + MANIFEST_SIZE,
        ),
    ];
    for (name, range) in corruptions {
        let mut flash = base.clone();
        flash.clear_one_set_bit(range);
        assert!(
            matches!(
                mount::<4, _>(&mut flash, SubmissionId::new(1)),
                Err(JournalError::ManifestCorrupt)
            ),
            "{name} corruption must fail closed instead of falling back to A1"
        );
    }
}

#[test]
fn interrupted_manifest_retirement_blocks_append_and_resumes_without_advancing_generation() {
    let (mut flash, entries) = seeded_two();
    flash.fail_erase_after(2, EraseFaultKind::Partial(0));
    assert_eq!(
        compact::<4, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::Backend(FakeError::Injected))
    );

    let selected = assert_complete_replay(&mut flash, entries);
    assert_eq!(selected.bank(), Bank::B);
    assert_eq!(selected.generation(), 2);
    assert!(selected.compaction_pending());
    let writes_before_append = flash.writes;
    assert_new_append_blocked(&mut flash);
    assert_eq!(flash.writes, writes_before_append);

    let erases_before_resume = flash.erases;
    let resumed = compact::<4, _>(&mut flash, SubmissionId::new(1))
        .expect("retry retires only the superseded manifest");
    assert_eq!(flash.erases, erases_before_resume + 1);
    assert_eq!(resumed.bank(), Bank::B);
    assert_eq!(resumed.generation(), 2);
    assert_eq!(resumed.committed_records(), entries.len());
    assert!(!resumed.compaction_pending());

    let AppendOutcome::Appended(appended) = append::<4, _>(
        &mut flash,
        SubmissionId::new(1),
        accepted(3, 0x13, 0x23, b"after retirement"),
    )
    .expect("append is allowed after retirement") else {
        panic!("new record must allocate a slot")
    };
    assert_eq!(appended.bank(), Bank::B);
    assert_eq!(appended.generation(), 2);
    assert_eq!(appended.committed_records(), entries.len() + 1);
}

#[test]
fn every_retirement_erase_byte_cut_keeps_the_new_generation_authoritative() {
    let (mut published, entries) = seeded_two();
    published.fail_erase_after(2, EraseFaultKind::Partial(0));
    assert_eq!(
        compact::<4, _>(&mut published, SubmissionId::new(1)),
        Err(JournalError::Backend(FakeError::Injected))
    );
    let awaiting_retirement = assert_complete_replay(&mut published, entries);
    assert_eq!(awaiting_retirement.bank(), Bank::B);
    assert_eq!(awaiting_retirement.generation(), 2);
    assert!(awaiting_retirement.compaction_pending());

    let programmed_envelope = HANDOFF_OFFSET + HANDOFF_SIZE;
    for cut in (0..=programmed_envelope).chain(core::iter::once(ERASE_SIZE)) {
        let mut flash = published.clone();
        flash.fail_erase_after(0, EraseFaultKind::Partial(cut));
        assert_eq!(
            compact::<4, _>(&mut flash, SubmissionId::new(1)),
            Err(JournalError::Backend(FakeError::Injected)),
            "retirement erase did not fail at byte cut {cut}"
        );

        let selected = assert_complete_replay(&mut flash, entries);
        assert_eq!(selected.bank(), Bank::B, "retirement byte cut {cut}");
        assert_eq!(selected.generation(), 2, "retirement byte cut {cut}");
        let retirement_completed = cut >= programmed_envelope;
        assert_eq!(
            selected.compaction_pending(),
            !retirement_completed,
            "wrong retirement state at byte cut {cut}"
        );

        if retirement_completed {
            let AppendOutcome::Appended(appended) = append::<4, _>(
                &mut flash,
                SubmissionId::new(1),
                accepted(3, 0x13, 0x23, b"completed despite lost reply"),
            )
            .expect("a physically completed retirement allows append") else {
                panic!("new record must allocate a slot")
            };
            assert_eq!(appended.generation(), 2);
            continue;
        }

        assert_new_append_blocked(&mut flash);
        let recovered = compact::<4, _>(&mut flash, SubmissionId::new(1))
            .unwrap_or_else(|error| panic!("retirement cut {cut} did not recover: {error:?}"));
        assert_eq!(recovered.bank(), Bank::B);
        assert_eq!(recovered.generation(), 2);
        assert_eq!(recovered.committed_records(), entries.len());
        assert_eq!(recovered.accepted_submissions(), entries.len());
        assert!(!recovered.compaction_pending());
    }

    let mut lost_reply = published;
    lost_reply.fail_erase_after(0, EraseFaultKind::LostReply);
    assert_eq!(
        compact::<4, _>(&mut lost_reply, SubmissionId::new(1)),
        Err(JournalError::Backend(FakeError::Injected))
    );
    let selected = assert_complete_replay(&mut lost_reply, entries);
    assert_eq!(selected.bank(), Bank::B);
    assert_eq!(selected.generation(), 2);
    assert!(!selected.compaction_pending());
}

#[test]
fn every_compaction_write_byte_cut_keeps_one_complete_generation_and_recovers() {
    // Physical operation order for a two-record source: source handoff,
    // record zero, record one, and target manifest; every object is written
    // as a protected prefix followed by its commit marker.
    let writes = [
        HANDOFF_PREFIX_SIZE,
        COMMIT_SIZE,
        SLOT_COMMIT_OFFSET,
        COMMIT_SIZE,
        SLOT_COMMIT_OFFSET,
        COMMIT_SIZE,
        MANIFEST_PREFIX_SIZE,
        COMMIT_SIZE,
    ];
    let (base, entries) = seeded_two();

    for (write_index, write_len) in writes.into_iter().enumerate() {
        for cut in 0..=write_len {
            let mut flash = base.clone();
            flash.fail_write_after(write_index, WriteFaultKind::Partial(cut));
            assert_backend_write_fault(compact::<4, _>(&mut flash, SubmissionId::new(1)));

            let selected = assert_complete_replay(&mut flash, entries);
            let target_was_published = write_index == writes.len() - 1 && cut == COMMIT_SIZE;
            if target_was_published {
                assert_eq!(selected.bank(), Bank::B);
                assert_eq!(selected.generation(), 2);
                assert!(selected.compaction_pending());
                assert_new_append_blocked(&mut flash);

                let recovered = compact::<4, _>(&mut flash, SubmissionId::new(1))
                    .expect("published target retires its old manifest on retry");
                assert_eq!(recovered.bank(), Bank::B);
                assert_eq!(recovered.generation(), 2);
                assert!(!recovered.compaction_pending());
                continue;
            }

            assert_eq!(
                selected.bank(),
                Bank::A,
                "partial target selected at compaction write {write_index}, byte cut {cut}"
            );
            assert_eq!(selected.generation(), 1);
            assert_eq!(selected.committed_records(), entries.len());
            assert_eq!(
                selected.compaction_pending(),
                !(write_index == 0 && cut == 0),
                "wrong handoff state at compaction write {write_index}, byte cut {cut}"
            );

            let recovered = compact::<4, _>(&mut flash, SubmissionId::new(1))
                .unwrap_or_else(|error| {
                    panic!(
                        "compaction did not recover at write {write_index}, byte cut {cut}: {error:?}"
                    )
                });
            assert_eq!(recovered.bank(), Bank::B);
            assert_eq!(recovered.generation(), 2);
            assert_eq!(recovered.committed_records(), entries.len());
            assert_eq!(recovered.accepted_submissions(), entries.len());
            assert!(!recovered.compaction_pending());
        }
    }
}

#[test]
fn every_relevant_compaction_erase_byte_cut_keeps_the_source_and_recovers() {
    let (mut base, entries) = seeded_two();
    let on_b = compact::<4, _>(&mut base, SubmissionId::new(1)).expect("seed B bank");
    assert_eq!(on_b.bank(), Bank::B);
    assert_eq!(on_b.generation(), 2);

    // The old A manifest sector has no programmed bytes after the handoff.
    // Cuts through that complete programmed envelope plus the full-sector
    // lost-reply boundary exhaust every distinct prefix-erase state here.
    for cut in (0..=HANDOFF_OFFSET + HANDOFF_SIZE).chain(core::iter::once(ERASE_SIZE)) {
        let mut flash = base.clone();
        flash.fail_erase_after(0, EraseFaultKind::Partial(cut));
        assert_eq!(
            compact::<4, _>(&mut flash, SubmissionId::new(1)),
            Err(JournalError::Backend(FakeError::Injected)),
            "manifest erase did not fail at byte cut {cut}"
        );

        let selected = assert_complete_replay(&mut flash, entries);
        assert_eq!(selected.bank(), Bank::B, "manifest erase byte cut {cut}");
        assert_eq!(selected.generation(), 2);
        assert!(selected.compaction_pending());

        let recovered = compact::<4, _>(&mut flash, SubmissionId::new(1))
            .unwrap_or_else(|error| panic!("manifest erase cut {cut} did not recover: {error:?}"));
        assert_eq!(recovered.bank(), Bank::A);
        assert_eq!(recovered.generation(), 3);
        assert_eq!(recovered.committed_records(), entries.len());
        assert_eq!(recovered.accepted_submissions(), entries.len());
        assert!(!recovered.compaction_pending());
    }

    // The old A bank has two physical slots and is erased afterwards. Cuts
    // through both slots plus the full-bank lost-reply boundary likewise
    // exhaust every distinct prefix-erase state in this fixture.
    for cut in (0..=entries.len() * SLOT_SIZE).chain(core::iter::once(BANK_SIZE)) {
        let mut flash = base.clone();
        flash.fail_erase_after(1, EraseFaultKind::Partial(cut));
        assert_eq!(
            compact::<4, _>(&mut flash, SubmissionId::new(1)),
            Err(JournalError::Backend(FakeError::Injected)),
            "bank erase did not fail at byte cut {cut}"
        );

        let selected = assert_complete_replay(&mut flash, entries);
        assert_eq!(selected.bank(), Bank::B, "bank erase byte cut {cut}");
        assert_eq!(selected.generation(), 2);
        assert!(selected.compaction_pending());

        let recovered = compact::<4, _>(&mut flash, SubmissionId::new(1))
            .unwrap_or_else(|error| panic!("bank erase cut {cut} did not recover: {error:?}"));
        assert_eq!(recovered.bank(), Bank::A);
        assert_eq!(recovered.generation(), 3);
        assert_eq!(recovered.committed_records(), entries.len());
        assert_eq!(recovered.accepted_submissions(), entries.len());
        assert!(!recovered.compaction_pending());
    }
}

#[test]
fn torn_or_ambiguously_completed_handoff_blocks_append_and_resumes() {
    let cases = [
        (0, WriteFaultKind::Partial(32)),
        (0, WriteFaultKind::LostReply),
        (1, WriteFaultKind::Partial(4)),
        (1, WriteFaultKind::LostReply),
    ];
    for (normal_writes, fault) in cases {
        let (mut flash, entries) = seeded_two();
        flash.fail_write_after(normal_writes, fault);
        assert_eq!(
            compact::<4, _>(&mut flash, SubmissionId::new(1)),
            Err(JournalError::Backend(FakeError::Injected))
        );

        let old = assert_complete_old_or_new(&mut flash, entries, Bank::A, 1);
        assert_eq!(old.bank(), Bank::A);
        assert_eq!(old.generation(), 1);
        assert!(old.compaction_pending());
        assert_new_append_blocked(&mut flash);

        let resumed = compact::<4, _>(&mut flash, SubmissionId::new(1)).expect("handoff resumes");
        assert_eq!(resumed.bank(), Bank::B);
        assert_eq!(resumed.generation(), 2);
        assert!(!resumed.compaction_pending());
        assert_eq!(assert_complete_replay(&mut flash, entries), resumed);
    }
}

#[test]
fn target_manifest_and_bank_erase_faults_leave_the_complete_source_mountable() {
    let cases = [
        (0, EraseFaultKind::Partial(512)),
        (0, EraseFaultKind::LostReply),
        (1, EraseFaultKind::Partial(ERASE_SIZE)),
        (1, EraseFaultKind::LostReply),
    ];
    for (normal_erases, fault) in cases {
        let (mut flash, entries) = seeded_two();
        let on_b = compact::<4, _>(&mut flash, SubmissionId::new(1)).expect("seed B bank");
        assert_eq!(on_b.bank(), Bank::B);
        assert_eq!(on_b.generation(), 2);

        flash.fail_erase_after(normal_erases, fault);
        assert_eq!(
            compact::<4, _>(&mut flash, SubmissionId::new(1)),
            Err(JournalError::Backend(FakeError::Injected))
        );
        let still_b = assert_complete_old_or_new(&mut flash, entries, Bank::B, 2);
        assert_eq!(still_b.bank(), Bank::B);
        assert_eq!(still_b.generation(), 2);
        assert!(still_b.compaction_pending());
        assert_new_append_blocked(&mut flash);

        let resumed =
            compact::<4, _>(&mut flash, SubmissionId::new(1)).expect("target erase fault resumes");
        assert_eq!(resumed.bank(), Bank::A);
        assert_eq!(resumed.generation(), 3);
        assert_eq!(assert_complete_replay(&mut flash, entries), resumed);
    }
}

#[test]
fn target_copy_faults_never_publish_a_copied_prefix_and_are_resumable() {
    let cases = [
        (2, WriteFaultKind::Partial(SLOT_HEADER_SIZE)),
        (3, WriteFaultKind::LostReply),
    ];
    for (normal_writes, fault) in cases {
        let (mut flash, entries) = seeded_two();
        flash.fail_write_after(normal_writes, fault);
        assert_eq!(
            compact::<4, _>(&mut flash, SubmissionId::new(1)),
            Err(JournalError::Backend(FakeError::Injected))
        );

        let source = assert_complete_old_or_new(&mut flash, entries, Bank::A, 1);
        assert_eq!(source.bank(), Bank::A);
        assert_eq!(source.committed_records(), entries.len());
        assert!(source.compaction_pending());
        assert_new_append_blocked(&mut flash);

        let resumed =
            compact::<4, _>(&mut flash, SubmissionId::new(1)).expect("copy fault resumes");
        assert_eq!(resumed.bank(), Bank::B);
        assert_eq!(resumed.generation(), 2);
        assert_eq!(assert_complete_replay(&mut flash, entries), resumed);
    }
}

#[test]
fn target_manifest_fault_selects_only_a_complete_old_or_complete_new_bank() {
    let cases = [
        (6, WriteFaultKind::Partial(32), Bank::A, 1),
        (6, WriteFaultKind::LostReply, Bank::A, 1),
        (7, WriteFaultKind::Partial(4), Bank::A, 1),
        (7, WriteFaultKind::LostReply, Bank::B, 2),
    ];
    for (normal_writes, fault, expected_bank, expected_generation) in cases {
        let (mut flash, entries) = seeded_two();
        flash.fail_write_after(normal_writes, fault);
        assert_eq!(
            compact::<4, _>(&mut flash, SubmissionId::new(1)),
            Err(JournalError::Backend(FakeError::Injected))
        );

        let selected = assert_complete_old_or_new(&mut flash, entries, Bank::A, 1);
        assert_eq!(selected.bank(), expected_bank);
        assert_eq!(selected.generation(), expected_generation);
        assert_eq!(selected.committed_records(), entries.len());

        if expected_bank == Bank::A {
            assert!(selected.compaction_pending());
            assert_new_append_blocked(&mut flash);
            let resumed = compact::<4, _>(&mut flash, SubmissionId::new(1))
                .expect("unsealed target manifest resumes");
            assert_eq!(resumed.bank(), Bank::B);
            assert_eq!(resumed.generation(), 2);
            assert_eq!(assert_complete_replay(&mut flash, entries), resumed);
        } else {
            assert!(selected.compaction_pending());
            assert_new_append_blocked(&mut flash);
            let resumed = compact::<4, _>(&mut flash, SubmissionId::new(1))
                .expect("sealed target retires the old manifest");
            assert_eq!(resumed.bank(), Bank::B);
            assert_eq!(resumed.generation(), 2);
            assert!(!resumed.compaction_pending());
        }
    }
}

#[test]
fn active_bank_tail_programming_is_a_geometry_fault() {
    let mut flash = formatted();
    let tail = BANK_A_OFFSET + BANK_SLOT_COUNT * SLOT_SIZE;
    flash.program(tail, &[0xfe]);
    assert!(matches!(
        mount::<1, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::GeometryMismatch)
    ));
}

#[test]
fn selected_new_generation_requires_its_complete_manifest_baseline() {
    let (mut flash, _entries) = seeded_two();
    let on_b = compact::<4, _>(&mut flash, SubmissionId::new(1)).expect("A to B compacts");
    assert_eq!(on_b.bank(), Bank::B);
    assert_eq!(on_b.generation(), 2);

    NorFlash::erase(
        &mut flash,
        BANK_B_OFFSET as u32,
        (BANK_B_OFFSET + ERASE_SIZE) as u32,
    )
    .expect("fault injection erases copied records");
    assert!(matches!(
        mount::<4, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::BaselineMismatch)
    ));
}

#[test]
fn alternating_compaction_survives_partial_erase_of_only_the_old_handoff() {
    let (mut flash, entries) = seeded_two();
    flash.fail_erase_after(2, EraseFaultKind::Partial(0));
    assert_eq!(
        compact::<4, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::Backend(FakeError::Injected))
    );
    let published = assert_complete_replay(&mut flash, entries);
    assert_eq!(published.bank(), Bank::B);
    assert_eq!(published.generation(), 2);
    assert!(published.compaction_pending());

    flash.fail_erase_after(
        0,
        EraseFaultKind::PartialAt {
            offset: HANDOFF_OFFSET,
            length: HANDOFF_SIZE,
        },
    );
    assert_eq!(
        compact::<4, _>(&mut flash, SubmissionId::new(1)),
        Err(JournalError::Backend(FakeError::Injected))
    );

    // A1's outer manifest remains valid while its completed A1 -> B2 handoff
    // is erased. The consecutive, sealed B2 manifest remains authoritative,
    // but append stays blocked until A1's outer manifest is retired.
    let selected = assert_complete_replay(&mut flash, entries);
    assert_eq!(selected.bank(), Bank::B);
    assert_eq!(selected.generation(), 2);
    assert!(selected.compaction_pending());
    assert_new_append_blocked(&mut flash);

    let retired = compact::<4, _>(&mut flash, SubmissionId::new(1))
        .expect("B2 retirement resumes after the handoff-only erase");
    assert_eq!(retired.bank(), Bank::B);
    assert_eq!(retired.generation(), 2);
    assert!(!retired.compaction_pending());

    let on_a = compact::<4, _>(&mut flash, SubmissionId::new(1))
        .expect("B2 to A3 compacts after retirement completes");
    assert_eq!(on_a.bank(), Bank::A);
    assert_eq!(on_a.generation(), 3);
    assert_eq!(assert_complete_replay(&mut flash, entries), on_a);
}
