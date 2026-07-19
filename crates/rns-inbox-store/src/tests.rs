use super::*;

extern crate std;

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    check_erase, check_read, check_write,
};
use std::{format, vec, vec::Vec};

const STORE_OFFSET: usize = 0x73_0000;
const DEVICE: InboxStoreDeviceId = InboxStoreDeviceId::new([0x5a; 16]);

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
enum WriteFault {
    Partial(usize),
    LostReply,
}

#[derive(Clone)]
struct FakeNor<const READ: usize = 4, const WRITE: usize = 4, const ERASE: usize = 4096> {
    bytes: Vec<u8>,
    reads: usize,
    writes: usize,
    erases: usize,
    read_fault: Option<usize>,
    write_fault: Option<(usize, WriteFault)>,
}

type TestNor = FakeNor<4, 4, 4096>;

impl<const READ: usize, const WRITE: usize, const ERASE: usize> FakeNor<READ, WRITE, ERASE> {
    fn erased() -> Self {
        Self::with_capacity(PARTITION_SIZE)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: vec![0xff; capacity],
            reads: 0,
            writes: 0,
            erases: 0,
            read_fault: None,
            write_fault: None,
        }
    }

    fn fail_read_after(&mut self, successful_reads: usize) {
        assert!(self.read_fault.is_none());
        self.read_fault = Some(successful_reads);
    }

    fn fail_write_after(&mut self, successful_writes: usize, fault: WriteFault) {
        assert!(self.write_fault.is_none());
        self.write_fault = Some((successful_writes, fault));
    }

    fn program(&mut self, offset: usize, bytes: &[u8]) {
        for (stored, supplied) in self.bytes[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            *stored &= *supplied;
        }
    }

    fn clear_one_programmed_bit(&mut self, range: core::ops::Range<usize>) {
        let byte = self.bytes[range]
            .iter_mut()
            .find(|byte| **byte != 0)
            .expect("fixture range contains a set bit");
        *byte &= !(1 << byte.trailing_zeros());
    }
}

impl<const READ: usize, const WRITE: usize, const ERASE: usize> ErrorType
    for FakeNor<READ, WRITE, ERASE>
{
    type Error = FakeError;
}

impl<const READ: usize, const WRITE: usize, const ERASE: usize> ReadNorFlash
    for FakeNor<READ, WRITE, ERASE>
{
    const READ_SIZE: usize = READ;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, bytes.len()).map_err(map_check_error)?;
        self.reads += 1;
        if self.read_fault == Some(0) {
            self.read_fault = None;
            return Err(FakeError::Injected);
        }
        if let Some(remaining) = &mut self.read_fault {
            *remaining -= 1;
        }
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl<const READ: usize, const WRITE: usize, const ERASE: usize> NorFlash
    for FakeNor<READ, WRITE, ERASE>
{
    const WRITE_SIZE: usize = WRITE;
    const ERASE_SIZE: usize = ERASE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, bytes.len()).map_err(map_check_error)?;
        self.writes += 1;
        let offset = offset as usize;
        let trigger = self
            .write_fault
            .is_some_and(|(remaining, _)| remaining == 0);
        if !trigger {
            if let Some((remaining, _)) = &mut self.write_fault {
                *remaining -= 1;
            }
            self.program(offset, bytes);
            return Ok(());
        }
        let (_, fault) = self.write_fault.take().expect("fault is armed");
        match fault {
            WriteFault::Partial(length) => {
                self.program(offset, &bytes[..length.min(bytes.len())]);
                Err(FakeError::Injected)
            }
            WriteFault::LostReply => {
                self.program(offset, bytes);
                Err(FakeError::Injected)
            }
        }
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to).map_err(map_check_error)?;
        self.erases += 1;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }
}

impl<const READ: usize, const WRITE: usize, const ERASE: usize> MultiwriteNorFlash
    for FakeNor<READ, WRITE, ERASE>
{
}

fn map_check_error(error: NorFlashErrorKind) -> FakeError {
    match error {
        NorFlashErrorKind::OutOfBounds => FakeError::Bounds,
        NorFlashErrorKind::NotAligned => FakeError::Alignment,
        _ => FakeError::Injected,
    }
}

fn binding() -> InboxStoreBinding {
    InboxStoreBinding::new(
        DEVICE,
        STORE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    )
}

fn bound<F>(flash: F) -> BoundInboxStore<F> {
    BoundInboxStore::new(flash, binding())
}

fn candidate(tag: u8, payload: &[u8]) -> InboxCandidate {
    InboxCandidate::new(InboxDestination::new([tag; 16]), payload).unwrap()
}

fn accept_one(access: &mut BoundInboxStore<TestNor>, value: InboxCandidate) -> MountedInboxStore {
    let mut mounted = mount(access).expect("erased store mounts");
    assert_eq!(
        mounted.accept(access, value),
        Ok(InboxAdmissionOutcome::Accepted(InboxItemId::FIRST))
    );
    mounted
}

#[test]
fn format_constants_and_owned_values_are_exact() {
    assert_eq!(PARTITION_SIZE, 2_097_152);
    assert_eq!(RECORD_SIZE, 576);
    assert_eq!(MAX_PAYLOAD_SIZE, 383);
    assert_eq!(InboxItemId::FIRST.get(), 1);
    assert_eq!(InboxItemId::new(0), None);

    let full = candidate(0x11, &[0xa5; MAX_PAYLOAD_SIZE]);
    assert_eq!(full.payload().len(), MAX_PAYLOAD_SIZE);
    assert_eq!(full.destination(), InboxDestination::new([0x11; 16]));
}

#[test]
fn plaintext_owners_and_failures_redact_payloads_from_debug() {
    let value = candidate(0x11, &[0xde, 0xad, 0xbe, 0xef]);
    let candidate_debug = format!("{value:?}");
    assert!(candidate_debug.contains("payload_len: 4"));
    assert!(!candidate_debug.contains("222, 173, 190, 239"));

    let item = value.clone().into_item();
    let item_debug = format!("{item:?}");
    assert!(item_debug.contains("payload_len: 4"));
    assert!(!item_debug.contains("222, 173, 190, 239"));

    let full_debug = format!("{:?}", InboxAdmissionOutcome::Full(value.clone()));
    assert!(full_debug.contains("payload_len: 4"));
    assert!(!full_debug.contains("222, 173, 190, 239"));

    let failure = InboxAdmissionFailure::new(
        value,
        InboxAdmissionError::<FakeError>::Fault(InboxStoreFault::InterruptedRecord),
    );
    let failure_debug = format!("{failure:?}");
    assert!(failure_debug.contains("InterruptedRecord"));
    assert!(failure_debug.contains("payload_len: 4"));
    assert!(!failure_debug.contains("222, 173, 190, 239"));
}

#[test]
fn canonical_record_matches_independent_digest_golden_vector() {
    let payload = [0x00, 0x01, 0x7f, 0x80, 0xfe, 0xff];
    let item = candidate(0xa6, &payload).into_item();
    let record = encode_record(binding(), &item);
    let expected_digest = [
        0x7c, 0xa0, 0x29, 0x16, 0xf2, 0xde, 0xbf, 0x99, 0xa2, 0xbb, 0x45, 0xe1, 0x69, 0xcb, 0x18,
        0xc4, 0x07, 0x6d, 0x6c, 0xb9, 0xab, 0x7f, 0xff, 0xe2, 0x48, 0x6e, 0xc3, 0xeb, 0xa5, 0x4e,
        0x9f, 0xd4,
    ];

    assert_eq!(&record[..CLAIM_SIZE], &CLAIM_MARKER);
    assert_eq!(&record[DIGEST_OFFSET..COMMIT_OFFSET], &expected_digest);
    assert_eq!(&record[COMMIT_OFFSET..], &COMMIT_MARKER);
    assert_eq!(
        &record[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload.len()],
        &payload
    );
}

#[test]
fn blank_mount_is_empty_and_never_mutates() {
    let mut access = bound(TestNor::erased());
    let mounted = mount(&mut access).expect("blank media mounts");
    assert_eq!(mounted.state(), InboxStoreState::Empty);
    assert!(mounted.item().is_none());
    assert!(access.backend().reads > 1);
    assert_eq!(access.backend().writes, 0);
    assert_eq!(access.backend().erases, 0);
}

#[test]
fn acceptance_commits_three_stages_retains_runtime_copy_and_remounts() {
    let expected = candidate(0x22, b"durable inbound qualification");
    let mut access = bound(TestNor::erased());
    let mounted = accept_one(&mut access, expected.clone());

    let item = mounted.item().expect("accepted item is retained");
    assert_eq!(item.id(), InboxItemId::FIRST);
    assert_eq!(item.destination(), expected.destination());
    assert_eq!(item.payload(), expected.payload());
    assert_eq!(access.backend().writes, 3);
    assert_eq!(access.backend().erases, 0);
    assert!(
        access.backend().bytes[RECORD_SIZE..]
            .iter()
            .all(|byte| *byte == 0xff)
    );

    let remounted = mount(&mut access).expect("committed media remounts");
    assert_eq!(remounted.item(), Some(item));
    assert_eq!(access.backend().writes, 3);
    assert_eq!(access.backend().erases, 0);
}

#[test]
fn occupied_store_returns_full_without_physical_io() {
    let mut access = bound(TestNor::erased());
    let mut mounted = accept_one(&mut access, candidate(0x31, b"first"));
    let io_before = (access.backend().reads, access.backend().writes);
    let second = candidate(0x32, b"second");
    assert_eq!(
        mounted.accept(&mut access, second.clone()),
        Ok(InboxAdmissionOutcome::Full(second))
    );
    assert_eq!((access.backend().reads, access.backend().writes), io_before);
}

#[test]
fn candidate_rejects_oversize_without_truncation() {
    let bytes = [0x77; MAX_PAYLOAD_SIZE + 1];
    assert_eq!(
        InboxCandidate::new(InboxDestination::new([0x44; 16]), &bytes),
        Err(InboxCandidateError::PayloadTooLarge {
            actual: MAX_PAYLOAD_SIZE + 1,
            maximum: MAX_PAYLOAD_SIZE,
        })
    );
}

#[test]
fn invalid_binding_and_read_geometry_fail_before_io() {
    let cases = [
        InboxStoreBinding::new(DEVICE, STORE_OFFSET, PARTITION_SIZE, 2),
        InboxStoreBinding::new(DEVICE, STORE_OFFSET, PARTITION_SIZE - 4, 1),
        InboxStoreBinding::new(DEVICE, usize::MAX - 3, PARTITION_SIZE, 1),
    ];
    for invalid in cases {
        let mut access = BoundInboxStore::new(TestNor::erased(), invalid);
        assert!(matches!(
            mount(&mut access),
            Err(InboxStoreMountError::Binding(_))
        ));
        assert_eq!(access.backend().reads, 0);
        assert_eq!(access.backend().writes, 0);
    }

    let short = FakeNor::<4, 4, 4096>::with_capacity(PARTITION_SIZE - 4);
    let mut access = bound(short);
    assert!(matches!(
        mount(&mut access),
        Err(InboxStoreMountError::Binding(
            InboxStoreBindingError::CapacityMismatch { .. }
        ))
    ));
    assert_eq!(access.backend().reads, 0);

    let mut access = bound(FakeNor::<3, 3, 4096>::erased());
    assert!(matches!(
        mount(&mut access),
        Err(InboxStoreMountError::Binding(
            InboxStoreBindingError::ReadAlignmentMismatch { .. }
        ))
    ));
    assert_eq!(access.backend().reads, 0);
}

#[test]
fn incompatible_write_or_erase_geometry_returns_candidate_before_io() {
    let value = candidate(0x51, b"geometry");

    let mut bad_write = bound(FakeNor::<4, 3, 4096>::erased());
    let mut mounted = mount(&mut bad_write).unwrap();
    let reads_before = bad_write.backend().reads;
    let failure = mounted
        .accept(&mut bad_write, value.clone())
        .expect_err("write geometry must fail");
    assert!(matches!(
        failure.error(),
        InboxAdmissionError::Binding(InboxStoreBindingError::WriteAlignmentMismatch { .. })
    ));
    assert_eq!(failure.into_parts().0, value);
    assert_eq!(bad_write.backend().reads, reads_before);
    assert_eq!(bad_write.backend().writes, 0);

    let mut bad_erase = bound(FakeNor::<4, 4, 3000>::erased());
    let mut mounted = mount(&mut bad_erase).unwrap();
    let reads_before = bad_erase.backend().reads;
    let failure = mounted
        .accept(&mut bad_erase, candidate(0x52, b"erase geometry"))
        .expect_err("erase geometry must fail");
    assert!(matches!(
        failure.error(),
        InboxAdmissionError::Binding(InboxStoreBindingError::EraseAlignmentMismatch { .. })
    ));
    assert_eq!(bad_erase.backend().reads, reads_before);
    assert_eq!(bad_erase.backend().writes, 0);
}

#[test]
fn later_access_with_wrong_device_is_rejected_before_io() {
    let mut initial = bound(TestNor::erased());
    let mut mounted = mount(&mut initial).unwrap();
    let wrong_binding = InboxStoreBinding::new(
        InboxStoreDeviceId::new([0x99; 16]),
        STORE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    );
    let mut wrong = BoundInboxStore::new(TestNor::erased(), wrong_binding);
    let failure = mounted
        .accept(&mut wrong, candidate(0x61, b"wrong device"))
        .expect_err("wrong device must fail");
    assert!(matches!(
        failure.error(),
        InboxAdmissionError::Binding(InboxStoreBindingError::DeviceMismatch { .. })
    ));
    assert_eq!(wrong.backend().reads, 0);
    assert_eq!(wrong.backend().writes, 0);
}

#[test]
fn digest_valid_record_mounted_through_wrong_binding_fails_closed() {
    let mut original = bound(TestNor::erased());
    let _ = accept_one(&mut original, candidate(0x62, b"bound record"));
    let backend = original.into_backend();
    let wrong_binding = InboxStoreBinding::new(
        InboxStoreDeviceId::new([0x63; 16]),
        STORE_OFFSET,
        PARTITION_SIZE,
        PHYSICAL_FORMAT_VERSION,
    );
    let mut wrong = BoundInboxStore::new(backend, wrong_binding);
    assert!(matches!(
        mount(&mut wrong),
        Err(InboxStoreMountError::Fault(
            InboxStoreFault::RecordBindingMismatch { .. }
        ))
    ));
    assert_eq!(wrong.backend().writes, 3);
    assert_eq!(wrong.backend().erases, 0);
}

#[test]
fn arbitrary_programming_committed_corruption_and_noncanonical_tail_fail_closed() {
    let mut unknown = bound(TestNor::erased());
    unknown.backend_mut().bytes[0] = 0;
    assert!(matches!(
        mount(&mut unknown),
        Err(InboxStoreMountError::Fault(
            InboxStoreFault::UnknownProgrammedData
        ))
    ));
    assert_eq!(unknown.backend().writes, 0);

    let mut corrupt = bound(TestNor::erased());
    let _ = accept_one(&mut corrupt, candidate(0x71, b"digest corruption"));
    corrupt
        .backend_mut()
        .clear_one_programmed_bit(DIGEST_OFFSET..COMMIT_OFFSET);
    assert!(matches!(
        mount(&mut corrupt),
        Err(InboxStoreMountError::Fault(
            InboxStoreFault::CommittedRecordCorrupt(InboxRecordCorruption::Digest)
        ))
    ));

    let mut tail = bound(TestNor::erased());
    let _ = accept_one(&mut tail, candidate(0x72, b"tail"));
    tail.backend_mut().bytes[RECORD_SIZE] = 0;
    assert!(matches!(
        mount(&mut tail),
        Err(InboxStoreMountError::Fault(
            InboxStoreFault::NonCanonicalRemainder
        ))
    ));
}

#[test]
fn partial_program_cuts_are_recognized_and_never_erased() {
    for (write_ordinal, partial, expected_fault) in [
        (0, 16, InboxStoreFault::InterruptedClaim),
        (1, 128, InboxStoreFault::InterruptedRecord),
        (2, 16, InboxStoreFault::InterruptedRecord),
    ] {
        let mut access = bound(TestNor::erased());
        let mut mounted = mount(&mut access).unwrap();
        access
            .backend_mut()
            .fail_write_after(write_ordinal, WriteFault::Partial(partial));
        let failure = mounted
            .accept(&mut access, candidate(0x81, b"cut trajectory"))
            .expect_err("partial program must remain ambiguous");
        assert!(matches!(
            failure.error(),
            InboxAdmissionError::Backend {
                error: FakeError::Injected,
                ..
            }
        ));
        assert_eq!(access.backend().erases, 0);
        assert!(matches!(
            mount(&mut access),
            Err(InboxStoreMountError::Fault(fault)) if fault == expected_fault
        ));
        assert_eq!(access.backend().erases, 0);
    }
}

#[test]
fn lost_program_replies_reconcile_including_commit() {
    for write_ordinal in 0..3 {
        let expected = candidate(0x91 + write_ordinal as u8, b"lost reply");
        let mut access = bound(TestNor::erased());
        let mut mounted = mount(&mut access).unwrap();
        access
            .backend_mut()
            .fail_write_after(write_ordinal, WriteFault::LostReply);
        assert_eq!(
            mounted.accept(&mut access, expected.clone()),
            Ok(InboxAdmissionOutcome::Accepted(InboxItemId::FIRST))
        );
        assert_eq!(mounted.item().unwrap().payload(), expected.payload());
        let remounted = mount(&mut access).expect("lost reply produced exact media");
        assert_eq!(remounted.item(), mounted.item());
        assert_eq!(access.backend().erases, 0);
    }
}

#[test]
fn prewrite_inspection_read_failures_return_the_exact_owner_without_mutation() {
    for successful_reads_before_fault in [0, 1] {
        let expected = candidate(0xa1, b"prewrite inspection read fault");
        let mut access = bound(TestNor::erased());
        let mut mounted = mount(&mut access).unwrap();
        access
            .backend_mut()
            .fail_read_after(successful_reads_before_fault);

        let failure = mounted
            .accept(&mut access, expected.clone())
            .expect_err("prewrite inspection read failure must not admit");
        let (returned, error) = failure.into_parts();
        assert_eq!(returned, expected);
        assert!(matches!(
            error,
            InboxAdmissionError::Backend {
                stage: None,
                error: FakeError::Injected,
            }
        ));
        assert_eq!(mounted.state(), InboxStoreState::Empty);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);

        let remounted = mount(&mut access).expect("failed read left media erased");
        assert_eq!(remounted.state(), InboxStoreState::Empty);
    }
}

#[test]
fn every_program_stage_readback_failure_returns_owner_and_never_publishes() {
    const EMPTY_INSPECTION_READS: usize =
        1 + (PARTITION_SIZE - RECORD_SIZE).div_ceil(INSPECTION_CHUNK_SIZE);

    for (successful_stage_readbacks, expected_stage, remounts_occupied) in [
        (0, InboxProgramStage::Claim, false),
        (1, InboxProgramStage::BodyAndDigest, false),
        (2, InboxProgramStage::Commit, true),
    ] {
        let expected = candidate(
            0xb1 + successful_stage_readbacks as u8,
            b"program readback fault",
        );
        let mut access = bound(TestNor::erased());
        let mut mounted = mount(&mut access).unwrap();
        access
            .backend_mut()
            .fail_read_after(EMPTY_INSPECTION_READS + successful_stage_readbacks);

        let failure = mounted
            .accept(&mut access, expected.clone())
            .expect_err("stage readback failure must remain ambiguous");
        let (returned, error) = failure.into_parts();
        assert_eq!(returned, expected);
        assert!(matches!(
            error,
            InboxAdmissionError::Backend {
                stage: Some(stage),
                error: FakeError::Injected,
            } if stage == expected_stage
        ));
        assert_eq!(mounted.state(), InboxStoreState::Empty);
        assert_eq!(access.backend().writes, successful_stage_readbacks + 1);
        assert_eq!(access.backend().erases, 0);

        if remounts_occupied {
            let remounted = mount(&mut access).expect("exact commit remounts occupied");
            let item = remounted.item().expect("commit write reached media");
            assert_eq!(item.destination(), expected.destination());
            assert_eq!(item.payload(), expected.payload());
        } else {
            assert!(matches!(
                mount(&mut access),
                Err(InboxStoreMountError::Fault(
                    InboxStoreFault::InterruptedRecord
                ))
            ));
        }
        assert_eq!(access.backend().erases, 0);
    }
}

#[test]
fn final_record_verification_read_failure_returns_owner_and_remounts_exact_commit() {
    const READS_BEFORE_FINAL_VERIFICATION: usize =
        1 + (PARTITION_SIZE - RECORD_SIZE).div_ceil(INSPECTION_CHUNK_SIZE) + 3;

    let expected = candidate(0xc1, b"final verification read fault");
    let mut access = bound(TestNor::erased());
    let mut mounted = mount(&mut access).unwrap();
    access
        .backend_mut()
        .fail_read_after(READS_BEFORE_FINAL_VERIFICATION);

    let failure = mounted
        .accept(&mut access, expected.clone())
        .expect_err("final verification read failure must not publish");
    let (returned, error) = failure.into_parts();
    assert_eq!(returned, expected);
    assert!(matches!(
        error,
        InboxAdmissionError::Backend {
            stage: Some(InboxProgramStage::Commit),
            error: FakeError::Injected,
        }
    ));
    assert_eq!(mounted.state(), InboxStoreState::Empty);
    assert_eq!(access.backend().writes, 3);
    assert_eq!(access.backend().erases, 0);

    let remounted = mount(&mut access).expect("all committed bytes reached media");
    let item = remounted
        .item()
        .expect("exact commit is recovered on remount");
    assert_eq!(item.destination(), expected.destination());
    assert_eq!(item.payload(), expected.payload());
    assert_eq!(access.backend().erases, 0);
}
