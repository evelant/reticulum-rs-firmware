use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashErrorKind, ReadNorFlash, check_erase,
    check_read, check_write,
};
use reticulum_storage_journal::{Bank, JournalError, append, compact, format_erased};
use reticulum_storage_model::{
    Accepted, AuditEntry, AuditEvent, FinalDisposition, JournalEntry, LifecycleState, PrincipalId,
    StateTransition, TransportRecoveryReason,
};

use super::*;

#[allow(dead_code)]
#[path = "../../../firmware/heltec-tracker-v2-storage-hil/src/fixture.rs"]
mod firmware_fixture;

struct TestNor {
    bytes: Vec<u8>,
}

impl TestNor {
    fn erased() -> Self {
        Self {
            bytes: vec![0xff; PARTITION_SIZE],
        }
    }
}

impl ErrorType for TestNor {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for TestNor {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
        check_read(self, offset, output.len())?;
        let offset = offset as usize;
        output.copy_from_slice(&self.bytes[offset..offset + output.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for TestNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        check_erase(self, from, to)?;
        self.bytes[from as usize..to as usize].fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, input: &[u8]) -> Result<(), Self::Error> {
        check_write(self, offset, input.len())?;
        let offset = offset as usize;
        for (stored, supplied) in self.bytes[offset..offset + input.len()]
            .iter_mut()
            .zip(input)
        {
            *stored &= *supplied;
        }
        Ok(())
    }
}

impl MultiwriteNorFlash for TestNor {}

fn compacted_dump(records: [JournalEntry; FIXTURE_RECORD_COUNT]) -> Vec<u8> {
    let mut flash = TestNor::erased();
    let formatted = format_erased(&mut flash).expect("erased test NOR formats");
    assert_eq!(formatted.bank(), Bank::A);
    assert_eq!(formatted.generation(), 1);
    for record in records {
        append::<FIXTURE_ACCEPTED_SUBMISSIONS, _>(&mut flash, FIXTURE_SUBMISSION_ID, record)
            .expect("valid fixture record appends");
    }
    let compacted = compact::<FIXTURE_ACCEPTED_SUBMISSIONS, _>(&mut flash, FIXTURE_SUBMISSION_ID)
        .expect("fixture compacts");
    assert_eq!(compacted.bank(), Bank::B);
    assert_eq!(compacted.generation(), 2);
    flash.bytes
}

fn alternate_fixture() -> [JournalEntry; FIXTURE_RECORD_COUNT] {
    let expected_accepted = expected_accepted().expect("compiled expected acceptance is valid");
    let expected_details = expected_details().expect("compiled expected packet details are valid");
    let token = expected_token();
    let intent = expected_accepted.intent();
    let accepted = Accepted::new(
        FIXTURE_SUBMISSION_ID,
        PrincipalId::new([0x52; 16]),
        expected_accepted.idempotency_key(),
        intent,
        expected_accepted.authorization(),
    );
    let preparing = StateTransition::new(FIXTURE_SUBMISSION_ID, 1, LifecycleState::Preparing)
        .expect("preparing transition is valid");
    let awaiting = StateTransition::new(
        FIXTURE_SUBMISSION_ID,
        2,
        LifecycleState::AwaitingDelivery(expected_details),
    )
    .expect("awaiting transition is valid");
    let audit = AuditEntry::new(
        FIXTURE_SUBMISSION_ID,
        3,
        AuditEvent::TransportRecovered {
            rns_attempt_token: token,
            may_have_transmitted: true,
            reason: TransportRecoveryReason::CompletionFault(0x4849),
        },
    )
    .expect("audit revision is nonzero");
    let delivered = StateTransition::new(
        FIXTURE_SUBMISSION_ID,
        4,
        LifecycleState::Final(FinalDisposition::Delivered(expected_details)),
    )
    .expect("delivered transition is valid");
    [
        JournalEntry::Accepted(accepted),
        JournalEntry::StateTransition(preparing),
        JournalEntry::StateTransition(awaiting),
        JournalEntry::Audit(audit),
        JournalEntry::StateTransition(delivered),
    ]
}

#[test]
fn accepts_exact_firmware_fixture_after_compaction() {
    let dump = compacted_dump(firmware_fixture::records());
    let report = verify_dump(&dump).expect("exact firmware fixture must verify");
    assert_eq!(report.bank, Bank::B);
    assert_eq!(report.generation, 2);
    assert_eq!(report.committed_records, 5);
    assert_eq!(report.occupied_slots, 5);
    assert_eq!(report.accepted_submissions, 1);
    assert_eq!(
        report.fixture_submission_id,
        firmware_fixture::submission_id()
    );
    assert_eq!(report.fixture_revision, 4);
}

#[test]
fn rejects_wrong_partition_length() {
    let error = verify_dump(&vec![0xff; PARTITION_SIZE - 1]).unwrap_err();
    assert!(matches!(
        error,
        VerificationError::WrongLength {
            expected: PARTITION_SIZE,
            actual
        } if actual == PARTITION_SIZE - 1
    ));
}

#[test]
fn rejects_unformatted_erased_partition() {
    let error = verify_dump(&vec![0xff; PARTITION_SIZE]).unwrap_err();
    assert!(matches!(
        error,
        VerificationError::Journal(JournalError::UnformattedErased)
    ));
}

#[test]
fn rejects_programmed_retired_manifest_sector() {
    let mut dump = compacted_dump(firmware_fixture::records());
    dump[ERASE_SIZE - 1] = 0xfe;
    let error = verify_dump(&dump).unwrap_err();
    assert!(matches!(
        error,
        VerificationError::RegionNotErased {
            region: "bank-a-manifest",
            offset,
            value: 0xfe,
        } if offset == ERASE_SIZE - 1
    ));
}

#[test]
fn rejects_any_programmed_byte_after_expected_bank_b_records() {
    let mut dump = compacted_dump(firmware_fixture::records());
    dump[PARTITION_SIZE - 1] = 0xfe;
    let error = verify_dump(&dump).unwrap_err();
    assert!(matches!(
        error,
        VerificationError::RegionNotErased {
            region: "bank-b-unused-tail",
            offset,
            value: 0xfe,
        } if offset == PARTITION_SIZE - 1
    ));
}

#[test]
fn rejects_integrity_failure_in_committed_record() {
    let mut dump = compacted_dump(firmware_fixture::records());
    dump[BANK_B_OFFSET] ^= 0x01;
    let error = verify_dump(&dump).unwrap_err();
    assert!(matches!(error, VerificationError::Journal(_)));
}

#[test]
fn rejects_semantically_valid_but_different_fixture() {
    let dump = compacted_dump(alternate_fixture());
    let error = verify_dump(&dump).unwrap_err();
    assert!(matches!(
        error,
        VerificationError::FixtureAcceptanceMismatch
    ));
}
