//! Independent, fail-closed verification of the storage HIL's raw `retlog` dump.

#![forbid(unsafe_code)]

use core::fmt;

use embedded_storage::nor_flash::{ErrorType, NorFlashErrorKind, ReadNorFlash, check_read};
use reticulum_storage_journal::{
    BANK_SLOT_COUNT, Bank, ERASE_SIZE, JournalError, PARTITION_SIZE, SLOT_SIZE, mount,
};
use reticulum_storage_model::{
    Accepted, AuditEvent, DestinationHash, EncodedPacketSha256, ExperimentalRnsDataIntent,
    FinalDisposition, IdempotencyKey, LifecycleState, PreparedPacketDetails, PrincipalId,
    RnsAttemptToken, SubmissionId, TransportRecoveryReason,
};

/// Submission identifier assigned to the deterministic storage-HIL fixture.
pub const FIXTURE_SUBMISSION_ID: SubmissionId = SubmissionId::new(0x4849_4c00_0000_0001);
/// Final durable revision expected after the storage-HIL fixture has completed.
pub const FIXTURE_FINAL_REVISION: u64 = 4;

const FIXTURE_RECORD_COUNT: usize = 5;
const FIXTURE_ACCEPTED_SUBMISSIONS: usize = 1;
const FIXTURE_PAYLOAD: &[u8] = b"reticulum-storage-journal physical HIL fixture v1";
const RECORD_BANK_SIZE: usize = (PARTITION_SIZE - 2 * ERASE_SIZE) / 2;
const BANK_B_OFFSET: usize = 2 * ERASE_SIZE + RECORD_BANK_SIZE;
const BANK_B_EXPECTED_UNUSED_OFFSET: usize = BANK_B_OFFSET + FIXTURE_RECORD_COUNT * SLOT_SIZE;

const _: () = assert!(PARTITION_SIZE == 0x10_0000);
const _: () = assert!(ERASE_SIZE == 0x1000);
const _: () = assert!((PARTITION_SIZE - 2 * ERASE_SIZE).is_multiple_of(2));
const _: () = assert!(RECORD_BANK_SIZE == 0x7f000);
const _: () = assert!(BANK_B_OFFSET == 0x81000);
const _: () = assert!(BANK_B_OFFSET + RECORD_BANK_SIZE == PARTITION_SIZE);
const _: () = assert!(BANK_SLOT_COUNT * SLOT_SIZE <= RECORD_BANK_SIZE);
const _: () =
    assert!(FIXTURE_PAYLOAD.len() <= reticulum_storage_model::MAX_EXPERIMENTAL_RNS_DATA_BYTES);

/// Successful evidence extracted from one complete raw `retlog` partition dump.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerificationReport {
    /// Exact dump size established before parsing.
    pub dump_bytes: usize,
    /// Authoritative bank selected by the production journal mount path.
    pub bank: Bank,
    /// Authoritative manifest generation.
    pub generation: u64,
    /// Integrity-validated committed record count.
    pub committed_records: usize,
    /// Occupied physical slot count, including any torn slots.
    pub occupied_slots: usize,
    /// Semantically accepted submission count.
    pub accepted_submissions: usize,
    /// Deterministic fixture submission identifier.
    pub fixture_submission_id: SubmissionId,
    /// Latest replayed fixture revision.
    pub fixture_revision: u64,
    /// First byte after the five expected compacted records in bank B.
    pub bank_b_unused_offset: usize,
}

/// A fail-closed reason that a raw storage-HIL dump did not prove the expected result.
#[derive(Debug)]
pub enum VerificationError {
    /// The file is not the exact 1 MiB `retlog` partition.
    WrongLength {
        /// Required byte count.
        expected: usize,
        /// Observed byte count.
        actual: usize,
    },
    /// A region required to be erased contains a programmed byte.
    RegionNotErased {
        /// Stable evidence-region name.
        region: &'static str,
        /// Partition-relative byte offset of the first programmed byte.
        offset: usize,
        /// Programmed byte value.
        value: u8,
    },
    /// The production journal mount/replay path rejected the dump.
    Journal(JournalError<NorFlashErrorKind>),
    /// The selected bank is not the compacted target bank.
    UnexpectedBank(Bank),
    /// The selected manifest is not generation 2.
    UnexpectedGeneration(u64),
    /// The authenticated committed-record count is not exactly five.
    UnexpectedCommittedRecords(usize),
    /// The physical occupied-slot count is not exactly five.
    UnexpectedOccupiedSlots(usize),
    /// The semantic acceptance count is not exactly one.
    UnexpectedAcceptedSubmissions(usize),
    /// Mount found an unfinished compaction or manifest retirement.
    CompactionPending,
    /// The replay index does not contain exactly one submission.
    UnexpectedIndexCount(usize),
    /// The deterministic fixture submission is absent.
    FixtureSubmissionMissing,
    /// The immutable acceptance fields differ from the fixture contract.
    FixtureAcceptanceMismatch,
    /// The fixture did not replay through revision 4.
    FixtureRevisionMismatch(u64),
    /// The fixture is not terminally delivered with the exact packet metadata.
    FixtureLifecycleMismatch(LifecycleState),
    /// Replayed prepared-packet metadata differs from the fixture contract.
    FixturePreparedDetailsMismatch,
    /// Replayed Reticulum attempt correlation differs from the fixture contract.
    FixtureAttemptTokenMismatch,
    /// Replayed transport audit differs from the fixture contract.
    FixtureAuditMismatch,
    /// Replay failed to retain conservative possible-transmission accounting.
    FixtureTransmissionAccountingMismatch,
    /// The replay index's next submission ID is not the fixture successor.
    FixtureNextIdMismatch(Option<SubmissionId>),
    /// A fixed verifier-side fixture value became invalid for the compiled model.
    VerifierContract(&'static str),
}

impl fmt::Display for VerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { expected, actual } => {
                write!(
                    formatter,
                    "wrong dump length: expected {expected} bytes, observed {actual}"
                )
            }
            Self::RegionNotErased {
                region,
                offset,
                value,
            } => write!(
                formatter,
                "required erased region {region} has byte 0x{value:02x} at partition offset 0x{offset:x}"
            ),
            Self::Journal(error) => write!(formatter, "journal mount/replay failed: {error:?}"),
            Self::UnexpectedBank(bank) => {
                write!(formatter, "expected selected bank B, observed {bank:?}")
            }
            Self::UnexpectedGeneration(generation) => {
                write!(formatter, "expected generation 2, observed {generation}")
            }
            Self::UnexpectedCommittedRecords(records) => {
                write!(
                    formatter,
                    "expected 5 committed records, observed {records}"
                )
            }
            Self::UnexpectedOccupiedSlots(slots) => {
                write!(formatter, "expected 5 occupied slots, observed {slots}")
            }
            Self::UnexpectedAcceptedSubmissions(submissions) => write!(
                formatter,
                "expected one accepted submission, observed {submissions}"
            ),
            Self::CompactionPending => formatter.write_str("journal reports compaction pending"),
            Self::UnexpectedIndexCount(submissions) => write!(
                formatter,
                "expected exactly one replay-index submission, observed {submissions}"
            ),
            Self::FixtureSubmissionMissing => formatter.write_str("fixture submission is absent"),
            Self::FixtureAcceptanceMismatch => {
                formatter.write_str("fixture immutable acceptance fields differ")
            }
            Self::FixtureRevisionMismatch(revision) => {
                write!(
                    formatter,
                    "expected fixture revision 4, observed {revision}"
                )
            }
            Self::FixtureLifecycleMismatch(state) => write!(
                formatter,
                "expected exact fixture Delivered state, observed {state:?}"
            ),
            Self::FixturePreparedDetailsMismatch => {
                formatter.write_str("fixture prepared-packet details differ")
            }
            Self::FixtureAttemptTokenMismatch => {
                formatter.write_str("fixture Reticulum attempt token differs")
            }
            Self::FixtureAuditMismatch => formatter.write_str("fixture transport audit differs"),
            Self::FixtureTransmissionAccountingMismatch => formatter.write_str(
                "fixture replay did not retain conservative possible-transmission accounting",
            ),
            Self::FixtureNextIdMismatch(next_id) => write!(
                formatter,
                "expected fixture successor submission ID, observed {next_id:?}"
            ),
            Self::VerifierContract(message) => {
                write!(formatter, "compiled verifier fixture is invalid: {message}")
            }
        }
    }
}

impl std::error::Error for VerificationError {}

/// Verify one exact raw `retlog` partition image without mutating it.
///
/// This uses the production journal's complete mount and semantic replay path,
/// then applies an independent fixture oracle and raw-erasure checks. Any
/// omitted, extra, torn, corrupt, unfinished, or semantically different state
/// is an error.
pub fn verify_dump(bytes: &[u8]) -> Result<VerificationReport, VerificationError> {
    if bytes.len() != PARTITION_SIZE {
        return Err(VerificationError::WrongLength {
            expected: PARTITION_SIZE,
            actual: bytes.len(),
        });
    }

    require_erased(bytes, 0..ERASE_SIZE, "bank-a-manifest")?;
    require_erased(
        bytes,
        BANK_B_EXPECTED_UNUSED_OFFSET..PARTITION_SIZE,
        "bank-b-unused-tail",
    )?;

    let mut flash = DumpFlash { bytes };
    let mounted = mount::<FIXTURE_ACCEPTED_SUBMISSIONS, _>(&mut flash, FIXTURE_SUBMISSION_ID)
        .map_err(VerificationError::Journal)?;
    let state = mounted.state();

    if state.bank() != Bank::B {
        return Err(VerificationError::UnexpectedBank(state.bank()));
    }
    if state.generation() != 2 {
        return Err(VerificationError::UnexpectedGeneration(state.generation()));
    }
    if state.committed_records() != FIXTURE_RECORD_COUNT {
        return Err(VerificationError::UnexpectedCommittedRecords(
            state.committed_records(),
        ));
    }
    if state.consumed_slots() != FIXTURE_RECORD_COUNT {
        return Err(VerificationError::UnexpectedOccupiedSlots(
            state.consumed_slots(),
        ));
    }
    if state.accepted_submissions() != FIXTURE_ACCEPTED_SUBMISSIONS {
        return Err(VerificationError::UnexpectedAcceptedSubmissions(
            state.accepted_submissions(),
        ));
    }
    if state.compaction_pending() {
        return Err(VerificationError::CompactionPending);
    }

    let index = mounted.index();
    let index_count = index.iter().count();
    if index_count != FIXTURE_ACCEPTED_SUBMISSIONS {
        return Err(VerificationError::UnexpectedIndexCount(index_count));
    }
    let submission = index
        .get(FIXTURE_SUBMISSION_ID)
        .ok_or(VerificationError::FixtureSubmissionMissing)?;
    let expected_accepted = expected_accepted()?;
    let expected_details = expected_details()?;
    let expected_token = expected_token();
    let expected_audit = AuditEvent::TransportRecovered {
        rns_attempt_token: expected_token,
        may_have_transmitted: true,
        reason: TransportRecoveryReason::CompletionFault(0x4849),
    };

    if submission.accepted() != expected_accepted {
        return Err(VerificationError::FixtureAcceptanceMismatch);
    }
    if submission.revision() != FIXTURE_FINAL_REVISION {
        return Err(VerificationError::FixtureRevisionMismatch(
            submission.revision(),
        ));
    }
    let expected_state = LifecycleState::Final(FinalDisposition::Delivered(expected_details));
    if submission.state() != expected_state {
        return Err(VerificationError::FixtureLifecycleMismatch(
            submission.state(),
        ));
    }
    if submission.prepared_details() != Some(expected_details) {
        return Err(VerificationError::FixturePreparedDetailsMismatch);
    }
    if submission.rns_attempt_token() != Some(expected_token) {
        return Err(VerificationError::FixtureAttemptTokenMismatch);
    }
    if submission.transport_audit() != Some(expected_audit) {
        return Err(VerificationError::FixtureAuditMismatch);
    }
    if !submission.may_have_transmitted() {
        return Err(VerificationError::FixtureTransmissionAccountingMismatch);
    }
    let expected_next_id = FIXTURE_SUBMISSION_ID
        .get()
        .checked_add(1)
        .map(SubmissionId::new);
    if index.next_id() != expected_next_id {
        return Err(VerificationError::FixtureNextIdMismatch(index.next_id()));
    }

    Ok(VerificationReport {
        dump_bytes: bytes.len(),
        bank: state.bank(),
        generation: state.generation(),
        committed_records: state.committed_records(),
        occupied_slots: state.consumed_slots(),
        accepted_submissions: state.accepted_submissions(),
        fixture_submission_id: FIXTURE_SUBMISSION_ID,
        fixture_revision: submission.revision(),
        bank_b_unused_offset: BANK_B_EXPECTED_UNUSED_OFFSET,
    })
}

fn expected_accepted() -> Result<Accepted, VerificationError> {
    let intent = ExperimentalRnsDataIntent::new(DestinationHash::new([0xd3; 16]), FIXTURE_PAYLOAD)
        .map_err(|_| {
            VerificationError::VerifierContract("fixture payload exceeds model capacity")
        })?;
    Ok(Accepted::new(
        FIXTURE_SUBMISSION_ID,
        PrincipalId::new([0x51; 16]),
        IdempotencyKey::new([0x71; 16]),
        intent,
    ))
}

fn expected_token() -> RnsAttemptToken {
    RnsAttemptToken::new([0xa5; 32])
}

fn expected_details() -> Result<PreparedPacketDetails, VerificationError> {
    PreparedPacketDetails::new(97, EncodedPacketSha256::new([0x6e; 32]), expected_token())
        .map_err(|_| VerificationError::VerifierContract("fixture packet length is invalid"))
}

fn require_erased(
    bytes: &[u8],
    range: core::ops::Range<usize>,
    region: &'static str,
) -> Result<(), VerificationError> {
    if let Some(relative) = bytes[range.clone()].iter().position(|byte| *byte != 0xff) {
        let offset = range.start + relative;
        return Err(VerificationError::RegionNotErased {
            region,
            offset,
            value: bytes[offset],
        });
    }
    Ok(())
}

struct DumpFlash<'a> {
    bytes: &'a [u8],
}

impl ErrorType for DumpFlash<'_> {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for DumpFlash<'_> {
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

#[cfg(test)]
mod tests;
