//! Power-loss-safe fixed-slot NOR journal for durable submission records.
//!
//! The physical format is intentionally narrow: one 1 MiB partition, two
//! alternating 4 KiB manifest sectors, and two equal 127-sector record banks.
//! Records use canonical [`reticulum_storage_model`] bytes, a SHA-256 integrity
//! chain, and a separate commit marker programmed last. The crate never erases
//! or formats implicitly.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use embedded_storage::nor_flash::{MultiwriteNorFlash, NorFlash, ReadNorFlash};
use reticulum_storage_model::{
    ApplyError, ApplyOutcome, JOURNAL_SCHEMA_VERSION, JournalEntry,
    MAX_DURABLE_RECORDS_PER_SUBMISSION, MAX_JOURNAL_RECORD_BYTES, SubmissionId, SubmissionIndex,
    SubmissionReplay, decode_journal_entry, encode_journal_entry,
};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// Exact partition size required by physical format version 1.
pub const PARTITION_SIZE: usize = 0x10_0000;
/// Erase-sector size required by physical format version 1.
pub const ERASE_SIZE: usize = 0x1000;
/// Size of one physical record slot.
pub const SLOT_SIZE: usize = 640;
/// Maximum canonical semantic bytes in one slot.
pub const BODY_SIZE: usize = MAX_JOURNAL_RECORD_BYTES;
/// Number of record slots in either bank.
pub const BANK_SLOT_COUNT: usize = 812;
/// Maximum accepted submissions whose complete schema-1 lifetime budget fits.
pub const MAX_ACCEPTED_SUBMISSIONS: usize = BANK_SLOT_COUNT / MAX_DURABLE_RECORDS_PER_SUBMISSION;
/// Current physical on-flash format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;

const MANIFEST_A_OFFSET: usize = 0x0000;
const MANIFEST_B_OFFSET: usize = 0x1000;
const BANK_A_OFFSET: usize = 0x2000;
const BANK_B_OFFSET: usize = 0x81000;
const BANK_SIZE: usize = 0x7f000;
const BANK_TAIL_SIZE: usize = BANK_SIZE - BANK_SLOT_COUNT * SLOT_SIZE;

const MANIFEST_DATA_SIZE: usize = 96;
const DIGEST_SIZE: usize = 32;
const COMMIT_SIZE: usize = 32;
const MANIFEST_PREFIX_SIZE: usize = MANIFEST_DATA_SIZE + DIGEST_SIZE;
const MANIFEST_SIZE: usize = MANIFEST_PREFIX_SIZE + COMMIT_SIZE;
const HANDOFF_DATA_SIZE: usize = 80;
const HANDOFF_PREFIX_SIZE: usize = HANDOFF_DATA_SIZE + DIGEST_SIZE;
const HANDOFF_SIZE: usize = HANDOFF_PREFIX_SIZE + COMMIT_SIZE;
const HANDOFF_OFFSET: usize = MANIFEST_SIZE;
const SLOT_HEADER_SIZE: usize = 64;
const SLOT_DIGEST_OFFSET: usize = SLOT_HEADER_SIZE + BODY_SIZE;
const SLOT_COMMIT_OFFSET: usize = SLOT_DIGEST_OFFSET + DIGEST_SIZE;

const MANIFEST_MAGIC: &[u8; 8] = b"RTJRMAN1";
const HANDOFF_MAGIC: &[u8; 8] = b"RTJRHOF1";
const SLOT_MAGIC: &[u8; 8] = b"RTJRSLT1";
const MANIFEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/storage-journal/manifest/v1\0";
const SLOT_DOMAIN: &[u8] = b"reticulum-rs-firmware/storage-journal/slot/v1\0";
const HANDOFF_DOMAIN: &[u8] = b"reticulum-rs-firmware/storage-journal/handoff/v1\0";
const MANIFEST_COMMIT: [u8; COMMIT_SIZE] = [
    0x52, 0x4a, 0x3c, 0xa5, 0x0f, 0x69, 0x96, 0xc3, 0x71, 0x1e, 0xd2, 0x4b, 0x87, 0x58, 0xb4, 0x2d,
    0xca, 0x35, 0x6a, 0x93, 0xe1, 0x0d, 0x7c, 0x46, 0x9b, 0x24, 0xd8, 0x63, 0x15, 0xae, 0x49, 0x72,
];
const SLOT_COMMIT: [u8; COMMIT_SIZE] = [
    0xa6, 0x31, 0x5c, 0xe2, 0x49, 0x8d, 0x17, 0x70, 0xcb, 0x24, 0x95, 0x4a, 0x3e, 0xd1, 0x68, 0x0b,
    0xf4, 0x52, 0x8a, 0x35, 0x6d, 0xc0, 0x19, 0xb7, 0x43, 0xec, 0x26, 0x91, 0x5a, 0x0e, 0xd4, 0x78,
];
const HANDOFF_COMMIT: [u8; COMMIT_SIZE] = [
    0x2d, 0x86, 0x41, 0xb9, 0x73, 0x1c, 0xe5, 0x58, 0x0a, 0xd4, 0x67, 0x92, 0x3b, 0xcf, 0x15, 0x6e,
    0xa1, 0x49, 0xf0, 0x27, 0x7d, 0x84, 0x32, 0xcb, 0x56, 0x19, 0xed, 0x60, 0x98, 0x43, 0xb5, 0x0c,
];

const ZERO_DIGEST: [u8; DIGEST_SIZE] = [0; DIGEST_SIZE];

const _: () = assert!(BODY_SIZE == 512);
const _: () = assert!(SLOT_COMMIT_OFFSET + COMMIT_SIZE == SLOT_SIZE);
const _: () = assert!(BANK_TAIL_SIZE == 512);
const _: () = assert!(MAX_DURABLE_RECORDS_PER_SUBMISSION == 5);
const _: () = assert!(MAX_ACCEPTED_SUBMISSIONS == 162);
const _: () = assert!(HANDOFF_OFFSET + HANDOFF_SIZE <= ERASE_SIZE);

/// One of the two physical record banks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bank {
    /// First bank, immediately after both manifests.
    A,
    /// Second bank, ending at the partition boundary.
    B,
}

impl Bank {
    const fn id(self) -> u8 {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    const fn offset(self) -> usize {
        match self {
            Self::A => BANK_A_OFFSET,
            Self::B => BANK_B_OFFSET,
        }
    }

    const fn manifest_offset(self) -> usize {
        match self {
            Self::A => MANIFEST_A_OFFSET,
            Self::B => MANIFEST_B_OFFSET,
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    const fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::A),
            1 => Some(Self::B),
            _ => None,
        }
    }
}

/// Classification of a committed slot that cannot be trusted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotCorruption {
    /// Commit bytes cannot result from monotonic programming toward the marker.
    CommitMarker,
    /// Header magic, reserved fields, metadata, or padding are invalid.
    Header,
    /// Record ordinal is not the next committed ordinal.
    Ordinal,
    /// SHA-256 integrity chain does not match.
    Digest,
    /// Canonical semantic record bytes could not be decoded or reproduced.
    CanonicalRecord,
    /// Header logical metadata disagrees with the decoded semantic record.
    LogicalMetadata,
}

/// Failure to format, mount, replay, or append the physical journal.
#[derive(Debug)]
pub enum JournalError<E> {
    /// Underlying NOR operation failed. Completion may be ambiguous.
    Backend(E),
    /// The backend does not have the exact required capacity or alignment.
    IncompatibleFlash,
    /// The complete partition is erased and may be explicitly formatted.
    UnformattedErased,
    /// No valid manifest exists and at least one partition byte is programmed.
    UnformattedNonErased,
    /// A manifest claims commitment but fails its protected format.
    ManifestCorrupt,
    /// Both manifests claim the same generation.
    ManifestConflict,
    /// A committed structure uses an unsupported physical format version.
    UnsupportedPhysicalVersion(u16),
    /// A committed structure uses an unsupported semantic schema version.
    UnsupportedSemanticVersion(u16),
    /// The selected manifest does not describe the compiled geometry.
    GeometryMismatch,
    /// One committed record slot is corrupt.
    SlotCorrupt {
        /// Physical slot index within the selected bank.
        slot: usize,
        /// Exact corruption class.
        reason: SlotCorruption,
    },
    /// Manifest baseline count or chain digest is not present in the bank.
    BaselineMismatch,
    /// Physical replay encountered a duplicate logical `(submission, revision)` key.
    DuplicateLogicalKey,
    /// Committed records violate the semantic submission state machine.
    SemanticReplay(ApplyError),
    /// The requested append contradicts a committed record with the same key.
    LogicalConflict,
    /// Torn slots consumed the physical tail; compaction is required before retry.
    NeedsCompaction,
    /// A durable or torn bank handoff must be resumed before another append.
    CompactionPending,
    /// The selected generation cannot be incremented without wrapping.
    GenerationExhausted,
    /// Handoff metadata, copied record count, or copied chain is inconsistent.
    CompactionInvariant,
    /// The schema-1 lifetime reservation cannot admit another submission.
    AcceptanceCapacityExhausted,
    /// Exact program readback did not match the bytes supplied to the backend.
    ReadbackMismatch,
    /// Committed acceptances exceed the schema-1 lifetime reservation invariant.
    ReservationInvariant,
}

impl<E: PartialEq> PartialEq for JournalError<E> {
    fn eq(&self, other: &Self) -> bool {
        use JournalError as J;
        match (self, other) {
            (J::Backend(a), J::Backend(b)) => a == b,
            (J::IncompatibleFlash, J::IncompatibleFlash)
            | (J::UnformattedErased, J::UnformattedErased)
            | (J::UnformattedNonErased, J::UnformattedNonErased)
            | (J::ManifestCorrupt, J::ManifestCorrupt)
            | (J::ManifestConflict, J::ManifestConflict)
            | (J::GeometryMismatch, J::GeometryMismatch)
            | (J::BaselineMismatch, J::BaselineMismatch)
            | (J::DuplicateLogicalKey, J::DuplicateLogicalKey)
            | (J::LogicalConflict, J::LogicalConflict)
            | (J::NeedsCompaction, J::NeedsCompaction)
            | (J::CompactionPending, J::CompactionPending)
            | (J::GenerationExhausted, J::GenerationExhausted)
            | (J::CompactionInvariant, J::CompactionInvariant)
            | (J::AcceptanceCapacityExhausted, J::AcceptanceCapacityExhausted)
            | (J::ReadbackMismatch, J::ReadbackMismatch)
            | (J::ReservationInvariant, J::ReservationInvariant) => true,
            (J::UnsupportedPhysicalVersion(a), J::UnsupportedPhysicalVersion(b))
            | (J::UnsupportedSemanticVersion(a), J::UnsupportedSemanticVersion(b)) => a == b,
            (
                J::SlotCorrupt {
                    slot: a_slot,
                    reason: a_reason,
                },
                J::SlotCorrupt {
                    slot: b_slot,
                    reason: b_reason,
                },
            ) => a_slot == b_slot && a_reason == b_reason,
            (J::SemanticReplay(a), J::SemanticReplay(b)) => a == b,
            _ => false,
        }
    }
}

impl<E: Eq> Eq for JournalError<E> {}

/// Fully scanned physical state of the selected bank.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalState {
    bank: Bank,
    generation: u64,
    committed_records: usize,
    consumed_slots: usize,
    accepted_submissions: usize,
    tail_digest: [u8; DIGEST_SIZE],
    compaction_pending: bool,
}

impl JournalState {
    /// Selected authoritative bank.
    pub const fn bank(self) -> Bank {
        self.bank
    }

    /// Selected manifest generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Number of integrity-validated committed records.
    pub const fn committed_records(self) -> usize {
        self.committed_records
    }

    /// Number of physical slots preceding the next safe append position.
    pub const fn consumed_slots(self) -> usize {
        self.consumed_slots
    }

    /// Number of immutable acceptance records.
    pub const fn accepted_submissions(self) -> usize {
        self.accepted_submissions
    }

    /// Tail SHA-256 digest of the committed chain.
    pub const fn tail_digest(self) -> [u8; DIGEST_SIZE] {
        self.tail_digest
    }

    /// Whether compaction or manifest retirement must be resumed before appending.
    pub const fn compaction_pending(self) -> bool {
        self.compaction_pending
    }
}

/// Successful complete physical and semantic replay.
pub struct MountedJournal<const SUBMISSIONS: usize> {
    state: JournalState,
    index: SubmissionIndex<SUBMISSIONS>,
}

impl<const SUBMISSIONS: usize> MountedJournal<SUBMISSIONS> {
    /// Complete physical state established before this value was exposed.
    pub const fn state(&self) -> JournalState {
        self.state
    }

    /// Borrow the live semantic index produced only after physical EOF.
    pub const fn index(&self) -> &SubmissionIndex<SUBMISSIONS> {
        &self.index
    }

    /// Consume the mount and return its live semantic index.
    pub fn into_index(self) -> SubmissionIndex<SUBMISSIONS> {
        self.index
    }
}

/// Result of an idempotent append request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendOutcome {
    /// A new slot was committed and exactly read back.
    Appended(JournalState),
    /// The exact logical record was already committed; no write was attempted.
    AlreadyEquivalent(JournalState),
}

/// Cross-store authority for establishing the first empty journal format.
///
/// The journal cannot prove that an unauthenticated torn first-prefix write
/// originated from its own formatter. A product must derive
/// [`AllowFirstProvision`](Self::AllowFirstProvision) from independent durable
/// state, such as a still-vacant device-identity store that is committed only
/// after journal provisioning completes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshJournalPolicy {
    /// Do not mutate erased or recognizably interrupted first-format media.
    Reject,
    /// Permit completion of the exact canonical empty generation-1 format.
    AllowFirstProvision,
}

/// Failure to establish or recognize the first empty journal format.
#[derive(Debug, Eq, PartialEq)]
pub enum FirstProvisionError<E> {
    /// Underlying NOR operation failed; completion may be ambiguous.
    Backend(E),
    /// Capacity or read/program/erase geometry is incompatible with format version 1.
    IncompatibleFlash,
    /// Media is erased or on the canonical first-format trajectory, but the
    /// caller supplied [`FreshJournalPolicy::Reject`].
    NotAuthorized,
    /// Media is neither canonical empty generation 1 nor a recognized
    /// interruption of that exact format. This includes an existing nonempty
    /// journal and programmed bytes outside the first manifest.
    MediaNotProvisionable,
    /// Exact program readback did not establish the canonical empty manifest.
    ReadbackMismatch,
}

#[derive(Clone, Copy)]
struct Manifest {
    bank: Bank,
    generation: u64,
    baseline_count: usize,
    baseline_tail: [u8; DIGEST_SIZE],
}

#[derive(Clone, Copy)]
struct Handoff {
    source: Bank,
    target: Bank,
    source_generation: u64,
    target_generation: u64,
    source_count: usize,
    source_tail: [u8; DIGEST_SIZE],
}

#[derive(Clone, Copy)]
enum HandoffState {
    None,
    Torn,
    Valid(Handoff),
}

#[derive(Clone, Copy)]
struct ManifestRecord {
    manifest: Manifest,
    handoff: HandoffState,
    retire_manifest: Option<Bank>,
}

enum ManifestStatus {
    Absent,
    Uncommitted,
    Valid(ManifestRecord),
    CommittedInvalid,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct LogicalKey {
    submission: u64,
    revision: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum NeedleMatch {
    None,
    Equivalent,
    Conflict,
}

struct ScanResult<const SUBMISSIONS: usize> {
    state: JournalState,
    index: SubmissionIndex<SUBMISSIONS>,
    needle_match: NeedleMatch,
}

/// Explicitly format a completely erased compatible partition as generation 1.
///
/// This function never erases. Any programmed byte causes
/// [`JournalError::UnformattedNonErased`].
pub fn format_erased<F>(flash: &mut F) -> Result<JournalState, JournalError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    validate_writable_flash::<F>(flash)?;
    if !partition_is_erased(flash)? {
        return Err(JournalError::UnformattedNonErased);
    }

    let manifest = Manifest {
        bank: Bank::A,
        generation: 1,
        baseline_count: 0,
        baseline_tail: ZERO_DIGEST,
    };
    write_manifest(flash, manifest)?;
    Ok(empty_generation_one_state())
}

/// Establish or recognize the canonical empty generation-1 journal.
///
/// This is deliberately separate from [`mount`], whose validation and
/// fail-closed behavior are unchanged. A valid empty generation-1 journal is
/// returned without mutation under either policy. Erased media and a
/// monotonic-compatible interruption of the exact manifest prefix/commit
/// sequence are completed only under
/// [`FreshJournalPolicy::AllowFirstProvision`].
///
/// Provisioning never erases. Every byte outside manifest A's 160-byte
/// canonical image must remain erased. Before the prefix is exact, the commit
/// marker must also remain completely erased; after the prefix is exact, only
/// monotonic programming toward the canonical commit marker is accepted. Thus
/// a nonempty journal and all off-trajectory programmed media fail without a
/// write.
pub fn provision_first<F>(
    flash: &mut F,
    policy: FreshJournalPolicy,
) -> Result<JournalState, FirstProvisionError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    validate_writable_flash::<F>(flash).map_err(map_first_provision_journal_error)?;
    let manifest = Manifest {
        bank: Bank::A,
        generation: 1,
        baseline_count: 0,
        baseline_tail: ZERO_DIGEST,
    };
    let expected = encode_manifest(manifest);
    match classify_first_provision_media(flash, &expected)? {
        FirstProvisionMedia::ValidEmpty => return Ok(empty_generation_one_state()),
        FirstProvisionMedia::Erased | FirstProvisionMedia::Repairable => {}
    }
    if policy == FreshJournalPolicy::Reject {
        return Err(FirstProvisionError::NotAuthorized);
    }
    write_manifest(flash, manifest).map_err(map_first_provision_journal_error)?;
    Ok(empty_generation_one_state())
}

/// Scan every slot, validate the integrity chain, and complete semantic replay.
pub fn mount<const SUBMISSIONS: usize, F>(
    flash: &mut F,
    first_submission_id: SubmissionId,
) -> Result<MountedJournal<SUBMISSIONS>, JournalError<F::Error>>
where
    F: ReadNorFlash,
{
    validate_flash::<F>(flash)?;
    let manifest = select_manifest(flash)?;
    let scan = scan_bank::<SUBMISSIONS, F>(flash, manifest, first_submission_id, None)?;
    Ok(MountedJournal {
        state: scan.state,
        index: scan.index,
    })
}

/// Append one semantically valid record with exact idempotent retry behavior.
///
/// The current bank is fully scanned before any write. The supplied record is
/// replayed against that complete state, its logical key is checked for an
/// equivalent or conflicting committed record, and acceptance reservation is
/// enforced. The 608-byte protected prefix is programmed and read back before
/// the separate 32-byte commit marker is programmed last.
pub fn append<const SUBMISSIONS: usize, F>(
    flash: &mut F,
    first_submission_id: SubmissionId,
    entry: JournalEntry,
) -> Result<AppendOutcome, JournalError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    validate_writable_flash::<F>(flash)?;
    let manifest = select_manifest(flash)?;
    let scan = scan_bank::<SUBMISSIONS, F>(flash, manifest, first_submission_id, Some(entry))?;

    match scan.needle_match {
        NeedleMatch::Equivalent => return Ok(AppendOutcome::AlreadyEquivalent(scan.state)),
        NeedleMatch::Conflict => return Err(JournalError::LogicalConflict),
        NeedleMatch::None => {}
    }
    if scan.state.compaction_pending {
        return Err(JournalError::CompactionPending);
    }

    let is_acceptance = matches!(entry, JournalEntry::Accepted(_));
    if is_acceptance
        && scan
            .state
            .accepted_submissions
            .checked_add(1)
            .is_none_or(|count| count > MAX_ACCEPTED_SUBMISSIONS)
    {
        return Err(JournalError::AcceptanceCapacityExhausted);
    }
    if scan.state.consumed_slots >= BANK_SLOT_COUNT {
        return Err(JournalError::NeedsCompaction);
    }

    let slot_index = scan.state.consumed_slots;
    let mut slot = [0xff_u8; SLOT_SIZE];
    let digest = encode_slot(
        &mut slot,
        scan.state.bank,
        scan.state.generation,
        scan.state.committed_records,
        scan.state.tail_digest,
        entry,
    )?;
    let offset = scan.state.bank.offset() + slot_index * SLOT_SIZE;

    commit_encoded_slot(flash, offset, &slot)?;
    let decoded = decode_slot(
        &slot,
        scan.state.bank,
        scan.state.generation,
        scan.state.committed_records,
        scan.state.tail_digest,
        slot_index,
    )?;
    if decoded.0 != entry || decoded.1 != digest {
        return Err(JournalError::ReadbackMismatch);
    }

    Ok(AppendOutcome::Appended(JournalState {
        bank: scan.state.bank,
        generation: scan.state.generation,
        committed_records: scan.state.committed_records + 1,
        consumed_slots: slot_index + 1,
        accepted_submissions: scan.state.accepted_submissions + usize::from(is_acceptance),
        tail_digest: digest,
        compaction_pending: false,
    }))
}

/// Resume or start an atomic streaming compaction into the inactive bank.
///
/// A source-side handoff is committed first. The inactive manifest and bank
/// are then erased, every committed source record is copied and exactly read
/// back in logical order, and the target manifest is sealed. The superseded
/// manifest is retired before appends are allowed on the target. Until the
/// target seal, mounting continues to select the complete source bank. The
/// selected record bank is never erased.
pub fn compact<const SUBMISSIONS: usize, F>(
    flash: &mut F,
    first_submission_id: SubmissionId,
) -> Result<JournalState, JournalError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    validate_writable_flash::<F>(flash)?;
    let record = select_manifest(flash)?;
    let scan = scan_bank::<SUBMISSIONS, F>(flash, record, first_submission_id, None)?;
    let source = scan.state;

    if let Some(retired) = record.retire_manifest {
        retire_manifest(flash, retired)?;
        if matches!(record.handoff, HandoffState::None) {
            let mounted = mount::<SUBMISSIONS, F>(flash, first_submission_id)?;
            let state = mounted.state();
            if state.bank != source.bank
                || state.generation != source.generation
                || state.committed_records != source.committed_records
                || state.accepted_submissions != source.accepted_submissions
                || state.tail_digest != source.tail_digest
                || state.compaction_pending
            {
                return Err(JournalError::CompactionInvariant);
            }
            return Ok(state);
        }
    }

    let target = source.bank.other();
    let target_generation = source
        .generation
        .checked_add(1)
        .ok_or(JournalError::GenerationExhausted)?;
    let expected_handoff = Handoff {
        source: source.bank,
        target,
        source_generation: source.generation,
        target_generation,
        source_count: source.committed_records,
        source_tail: source.tail_digest,
    };
    if let HandoffState::Valid(existing) = record.handoff
        && !same_handoff(existing, expected_handoff)
    {
        return Err(JournalError::CompactionInvariant);
    }
    write_handoff(flash, expected_handoff)?;

    flash
        .erase(
            target.manifest_offset() as u32,
            (target.manifest_offset() + ERASE_SIZE) as u32,
        )
        .map_err(JournalError::Backend)?;
    flash
        .erase(target.offset() as u32, (target.offset() + BANK_SIZE) as u32)
        .map_err(JournalError::Backend)?;
    if !region_is_erased(flash, target.manifest_offset(), ERASE_SIZE)?
        || !region_is_erased(flash, target.offset(), BANK_SIZE)?
    {
        return Err(JournalError::ReadbackMismatch);
    }

    let copied_tail = copy_bank(flash, source, target, target_generation)?;
    write_manifest(
        flash,
        Manifest {
            bank: target,
            generation: target_generation,
            baseline_count: source.committed_records,
            baseline_tail: copied_tail,
        },
    )?;
    retire_manifest(flash, source.bank)?;

    let mounted = mount::<SUBMISSIONS, F>(flash, first_submission_id)?;
    let state = mounted.state();
    if state.bank != target
        || state.generation != target_generation
        || state.committed_records != source.committed_records
        || state.accepted_submissions != source.accepted_submissions
        || state.compaction_pending
    {
        return Err(JournalError::CompactionInvariant);
    }
    Ok(state)
}

fn retire_manifest<F>(flash: &mut F, bank: Bank) -> Result<(), JournalError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    flash
        .erase(
            bank.manifest_offset() as u32,
            (bank.manifest_offset() + ERASE_SIZE) as u32,
        )
        .map_err(JournalError::Backend)?;
    if !region_is_erased(flash, bank.manifest_offset(), ERASE_SIZE)? {
        return Err(JournalError::ReadbackMismatch);
    }
    Ok(())
}

fn same_handoff(left: Handoff, right: Handoff) -> bool {
    left.source == right.source
        && left.target == right.target
        && left.source_generation == right.source_generation
        && left.target_generation == right.target_generation
        && left.source_count == right.source_count
        && left.source_tail == right.source_tail
}

fn region_is_erased<F>(
    flash: &mut F,
    offset: usize,
    len: usize,
) -> Result<bool, JournalError<F::Error>>
where
    F: ReadNorFlash,
{
    if !offset.is_multiple_of(ERASE_SIZE) || !len.is_multiple_of(ERASE_SIZE) {
        return Err(JournalError::GeometryMismatch);
    }
    let mut sector = [0_u8; ERASE_SIZE];
    let end = offset
        .checked_add(len)
        .ok_or(JournalError::GeometryMismatch)?;
    let mut cursor = offset;
    while cursor < end {
        flash
            .read(cursor as u32, &mut sector)
            .map_err(JournalError::Backend)?;
        if sector.iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        cursor += ERASE_SIZE;
    }
    Ok(true)
}

fn commit_encoded_slot<F>(
    flash: &mut F,
    offset: usize,
    slot: &[u8; SLOT_SIZE],
) -> Result<(), JournalError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    flash
        .write(offset as u32, &slot[..SLOT_COMMIT_OFFSET])
        .map_err(JournalError::Backend)?;
    let mut prefix = [0_u8; SLOT_COMMIT_OFFSET];
    flash
        .read(offset as u32, &mut prefix)
        .map_err(JournalError::Backend)?;
    if prefix != slot[..SLOT_COMMIT_OFFSET] {
        return Err(JournalError::ReadbackMismatch);
    }
    flash
        .write(
            (offset + SLOT_COMMIT_OFFSET) as u32,
            &slot[SLOT_COMMIT_OFFSET..],
        )
        .map_err(JournalError::Backend)?;
    let mut committed = [0_u8; SLOT_SIZE];
    flash
        .read(offset as u32, &mut committed)
        .map_err(JournalError::Backend)?;
    if committed != *slot {
        return Err(JournalError::ReadbackMismatch);
    }
    Ok(())
}

fn copy_bank<F>(
    flash: &mut F,
    source: JournalState,
    target: Bank,
    target_generation: u64,
) -> Result<[u8; DIGEST_SIZE], JournalError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    let mut source_count = 0_usize;
    let mut source_tail = ZERO_DIGEST;
    let mut target_tail = ZERO_DIGEST;
    for source_slot in 0..BANK_SLOT_COUNT {
        let source_offset = source.bank.offset() + source_slot * SLOT_SIZE;
        let mut encoded_source = [0_u8; SLOT_SIZE];
        flash
            .read(source_offset as u32, &mut encoded_source)
            .map_err(JournalError::Backend)?;
        if encoded_source.iter().all(|byte| *byte == 0xff) {
            continue;
        }
        let marker: &[u8; COMMIT_SIZE] = encoded_source[SLOT_COMMIT_OFFSET..]
            .try_into()
            .map_err(|_| JournalError::CompactionInvariant)?;
        if marker != &SLOT_COMMIT {
            if monotonic_partial(marker, &SLOT_COMMIT) {
                continue;
            }
            return Err(JournalError::SlotCorrupt {
                slot: source_slot,
                reason: SlotCorruption::CommitMarker,
            });
        }
        let (entry, digest) = decode_slot(
            &encoded_source,
            source.bank,
            source.generation,
            source_count,
            source_tail,
            source_slot,
        )?;
        source_tail = digest;

        let mut encoded_target = [0xff_u8; SLOT_SIZE];
        let target_digest = encode_slot(
            &mut encoded_target,
            target,
            target_generation,
            source_count,
            target_tail,
            entry,
        )?;
        let target_offset = target.offset() + source_count * SLOT_SIZE;
        commit_encoded_slot(flash, target_offset, &encoded_target)?;
        let (copied_entry, copied_digest) = decode_slot(
            &encoded_target,
            target,
            target_generation,
            source_count,
            target_tail,
            source_count,
        )?;
        if copied_entry != entry || copied_digest != target_digest {
            return Err(JournalError::CompactionInvariant);
        }
        target_tail = target_digest;
        source_count += 1;
    }
    if source_count != source.committed_records || source_tail != source.tail_digest {
        return Err(JournalError::CompactionInvariant);
    }
    Ok(target_tail)
}

fn validate_flash<F>(flash: &F) -> Result<(), JournalError<F::Error>>
where
    F: ReadNorFlash,
{
    if flash.capacity() != PARTITION_SIZE
        || F::READ_SIZE != 4
        || !PARTITION_SIZE.is_multiple_of(F::READ_SIZE)
    {
        return Err(JournalError::IncompatibleFlash);
    }
    Ok(())
}

fn validate_writable_flash<F>(flash: &F) -> Result<(), JournalError<F::Error>>
where
    F: NorFlash,
{
    validate_flash::<F>(flash)?;
    if F::WRITE_SIZE != 4 || F::ERASE_SIZE != ERASE_SIZE {
        return Err(JournalError::IncompatibleFlash);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FirstProvisionMedia {
    Erased,
    Repairable,
    ValidEmpty,
}

fn classify_first_provision_media<F>(
    flash: &mut F,
    expected: &[u8; MANIFEST_SIZE],
) -> Result<FirstProvisionMedia, FirstProvisionError<F::Error>>
where
    F: ReadNorFlash,
{
    let mut first_manifest = [0xff_u8; MANIFEST_SIZE];
    let mut sector = [0_u8; ERASE_SIZE];
    let mut outside_manifest_erased = true;
    let mut offset = 0;
    while offset < PARTITION_SIZE {
        flash
            .read(offset as u32, &mut sector)
            .map_err(FirstProvisionError::Backend)?;
        if offset == MANIFEST_A_OFFSET {
            first_manifest.copy_from_slice(&sector[..MANIFEST_SIZE]);
            outside_manifest_erased &= sector[MANIFEST_SIZE..].iter().all(|byte| *byte == 0xff);
        } else {
            outside_manifest_erased &= sector.iter().all(|byte| *byte == 0xff);
        }
        offset += sector.len();
    }

    if !outside_manifest_erased {
        return Err(FirstProvisionError::MediaNotProvisionable);
    }
    if first_manifest == *expected {
        return Ok(FirstProvisionMedia::ValidEmpty);
    }
    if first_manifest.iter().all(|byte| *byte == 0xff) {
        return Ok(FirstProvisionMedia::Erased);
    }

    let prefix = &first_manifest[..MANIFEST_PREFIX_SIZE];
    let expected_prefix = &expected[..MANIFEST_PREFIX_SIZE];
    let commit = &first_manifest[MANIFEST_PREFIX_SIZE..];
    let expected_commit = &expected[MANIFEST_PREFIX_SIZE..];
    let prefix_exact = prefix == expected_prefix;
    let commit_erased = commit.iter().all(|byte| *byte == 0xff);
    let repairable = if prefix_exact {
        monotonic_programming_compatible(commit, expected_commit)
    } else {
        commit_erased && monotonic_programming_compatible(prefix, expected_prefix)
    };
    if repairable {
        Ok(FirstProvisionMedia::Repairable)
    } else {
        Err(FirstProvisionError::MediaNotProvisionable)
    }
}

fn map_first_provision_journal_error<E>(error: JournalError<E>) -> FirstProvisionError<E> {
    match error {
        JournalError::Backend(error) => FirstProvisionError::Backend(error),
        JournalError::IncompatibleFlash => FirstProvisionError::IncompatibleFlash,
        JournalError::ReadbackMismatch => FirstProvisionError::ReadbackMismatch,
        JournalError::UnformattedErased
        | JournalError::UnformattedNonErased
        | JournalError::ManifestCorrupt
        | JournalError::ManifestConflict
        | JournalError::UnsupportedPhysicalVersion(_)
        | JournalError::UnsupportedSemanticVersion(_)
        | JournalError::GeometryMismatch
        | JournalError::SlotCorrupt { .. }
        | JournalError::BaselineMismatch
        | JournalError::DuplicateLogicalKey
        | JournalError::SemanticReplay(_)
        | JournalError::LogicalConflict
        | JournalError::NeedsCompaction
        | JournalError::CompactionPending
        | JournalError::GenerationExhausted
        | JournalError::CompactionInvariant
        | JournalError::AcceptanceCapacityExhausted
        | JournalError::ReservationInvariant => FirstProvisionError::MediaNotProvisionable,
    }
}

const fn empty_generation_one_state() -> JournalState {
    JournalState {
        bank: Bank::A,
        generation: 1,
        committed_records: 0,
        consumed_slots: 0,
        accepted_submissions: 0,
        tail_digest: ZERO_DIGEST,
        compaction_pending: false,
    }
}

fn partition_is_erased<F>(flash: &mut F) -> Result<bool, JournalError<F::Error>>
where
    F: ReadNorFlash,
{
    let mut sector = [0_u8; ERASE_SIZE];
    let mut offset = 0;
    while offset < PARTITION_SIZE {
        flash
            .read(offset as u32, &mut sector)
            .map_err(JournalError::Backend)?;
        if sector.iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        offset += sector.len();
    }
    Ok(true)
}

fn select_manifest<F>(flash: &mut F) -> Result<ManifestRecord, JournalError<F::Error>>
where
    F: ReadNorFlash,
{
    let a = read_manifest(flash, Bank::A)?;
    let b = read_manifest(flash, Bank::B)?;
    match (a, b) {
        (ManifestStatus::Valid(a), ManifestStatus::Valid(b)) => {
            if a.manifest.generation == b.manifest.generation {
                Err(JournalError::ManifestConflict)
            } else {
                let (mut newer, older) = if a.manifest.generation > b.manifest.generation {
                    (a, b)
                } else {
                    (b, a)
                };
                let expected_generation = older
                    .manifest
                    .generation
                    .checked_add(1)
                    .ok_or(JournalError::ManifestConflict)?;
                if newer.manifest.generation != expected_generation {
                    return Err(JournalError::ManifestConflict);
                }
                newer.retire_manifest = Some(older.manifest.bank);
                Ok(newer)
            }
        }
        (ManifestStatus::Valid(mut record), other) | (other, ManifestStatus::Valid(mut record)) => {
            match other {
                ManifestStatus::Absent => Ok(record),
                ManifestStatus::Uncommitted | ManifestStatus::CommittedInvalid => {
                    if matches!(record.handoff, HandoffState::Torn | HandoffState::Valid(_)) {
                        Ok(record)
                    } else if record.manifest.generation > 1 {
                        record.retire_manifest = Some(record.manifest.bank.other());
                        Ok(record)
                    } else {
                        Err(JournalError::ManifestCorrupt)
                    }
                }
                ManifestStatus::Valid(_) => Err(JournalError::ManifestConflict),
            }
        }
        (ManifestStatus::CommittedInvalid, _) | (_, ManifestStatus::CommittedInvalid) => {
            Err(JournalError::ManifestCorrupt)
        }
        (
            ManifestStatus::Absent | ManifestStatus::Uncommitted,
            ManifestStatus::Absent | ManifestStatus::Uncommitted,
        ) => {
            if partition_is_erased(flash)? {
                Err(JournalError::UnformattedErased)
            } else {
                Err(JournalError::UnformattedNonErased)
            }
        }
    }
}

fn read_manifest<F>(
    flash: &mut F,
    expected_bank: Bank,
) -> Result<ManifestStatus, JournalError<F::Error>>
where
    F: ReadNorFlash,
{
    let mut sector = [0_u8; ERASE_SIZE];
    flash
        .read(expected_bank.manifest_offset() as u32, &mut sector)
        .map_err(JournalError::Backend)?;
    if sector.iter().all(|byte| *byte == 0xff) {
        return Ok(ManifestStatus::Absent);
    }

    let marker: &[u8; COMMIT_SIZE] = sector[MANIFEST_PREFIX_SIZE..MANIFEST_SIZE]
        .try_into()
        .map_err(|_| JournalError::ManifestCorrupt)?;
    if marker != &MANIFEST_COMMIT {
        return if monotonic_partial(marker, &MANIFEST_COMMIT) {
            Ok(ManifestStatus::Uncommitted)
        } else {
            Ok(ManifestStatus::CommittedInvalid)
        };
    }

    let data = &sector[..MANIFEST_DATA_SIZE];
    if &data[..8] != MANIFEST_MAGIC || data[13..16].iter().any(|byte| *byte != 0) {
        return Ok(ManifestStatus::CommittedInvalid);
    }
    let physical = read_u16(data, 8);
    if physical != PHYSICAL_FORMAT_VERSION {
        return Ok(ManifestStatus::CommittedInvalid);
    }
    let schema = read_u16(data, 10);
    if schema != JOURNAL_SCHEMA_VERSION {
        return Ok(ManifestStatus::CommittedInvalid);
    }
    let Some(bank) = Bank::from_id(data[12]) else {
        return Ok(ManifestStatus::CommittedInvalid);
    };
    if bank != expected_bank {
        return Ok(ManifestStatus::CommittedInvalid);
    }
    let generation = read_u64(data, 16);
    if generation == 0 {
        return Ok(ManifestStatus::CommittedInvalid);
    }
    let bank_offset = read_u32(data, 24) as usize;
    let bank_size = read_u32(data, 28) as usize;
    let slot_size = read_u32(data, 32) as usize;
    let slot_count = read_u32(data, 36) as usize;
    let baseline_count =
        usize::try_from(read_u64(data, 40)).map_err(|_| JournalError::GeometryMismatch)?;
    let mut baseline_tail = [0_u8; DIGEST_SIZE];
    baseline_tail.copy_from_slice(&data[48..80]);
    let max_records = usize::from(read_u16(data, 80));
    if data[82..].iter().any(|byte| *byte != 0) {
        return Ok(ManifestStatus::CommittedInvalid);
    }
    if bank_offset != bank.offset()
        || bank_size != BANK_SIZE
        || slot_size != SLOT_SIZE
        || slot_count != BANK_SLOT_COUNT
        || baseline_count > BANK_SLOT_COUNT
        || max_records != MAX_DURABLE_RECORDS_PER_SUBMISSION
        || (baseline_count == 0 && baseline_tail != ZERO_DIGEST)
    {
        return Ok(ManifestStatus::CommittedInvalid);
    }

    let expected_digest = manifest_digest(data);
    if sector[MANIFEST_DATA_SIZE..MANIFEST_PREFIX_SIZE] != expected_digest {
        return Ok(ManifestStatus::CommittedInvalid);
    }
    if sector[HANDOFF_OFFSET + HANDOFF_SIZE..]
        .iter()
        .any(|byte| *byte != 0xff)
    {
        return Ok(ManifestStatus::CommittedInvalid);
    }
    let handoff_bytes = &sector[HANDOFF_OFFSET..HANDOFF_OFFSET + HANDOFF_SIZE];
    let handoff = if handoff_bytes.iter().all(|byte| *byte == 0xff) {
        HandoffState::None
    } else {
        let marker: &[u8; COMMIT_SIZE] = handoff_bytes[HANDOFF_PREFIX_SIZE..]
            .try_into()
            .map_err(|_| JournalError::ManifestCorrupt)?;
        if marker == &HANDOFF_COMMIT {
            let Some(handoff) = decode_handoff(&handoff_bytes[..HANDOFF_DATA_SIZE]) else {
                return Ok(ManifestStatus::CommittedInvalid);
            };
            let Some(expected_target_generation) = generation.checked_add(1) else {
                return Ok(ManifestStatus::CommittedInvalid);
            };
            if handoff.source != bank
                || handoff.source_generation != generation
                || handoff.target == bank
                || handoff.target_generation != expected_target_generation
                || handoff.source_count > BANK_SLOT_COUNT
                || handoff_digest(&handoff_bytes[..HANDOFF_DATA_SIZE])
                    != handoff_bytes[HANDOFF_DATA_SIZE..HANDOFF_PREFIX_SIZE]
            {
                return Ok(ManifestStatus::CommittedInvalid);
            }
            HandoffState::Valid(handoff)
        } else if monotonic_partial(marker, &HANDOFF_COMMIT) {
            HandoffState::Torn
        } else {
            return Ok(ManifestStatus::CommittedInvalid);
        }
    };
    Ok(ManifestStatus::Valid(ManifestRecord {
        manifest: Manifest {
            bank,
            generation,
            baseline_count,
            baseline_tail,
        },
        handoff,
        retire_manifest: None,
    }))
}

fn write_manifest<F>(flash: &mut F, manifest: Manifest) -> Result<(), JournalError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    validate_writable_flash::<F>(flash)?;
    let encoded = encode_manifest(manifest);

    let offset = manifest.bank.manifest_offset();
    flash
        .write(offset as u32, &encoded[..MANIFEST_PREFIX_SIZE])
        .map_err(JournalError::Backend)?;
    let mut prefix = [0_u8; MANIFEST_PREFIX_SIZE];
    flash
        .read(offset as u32, &mut prefix)
        .map_err(JournalError::Backend)?;
    if prefix != encoded[..MANIFEST_PREFIX_SIZE] {
        return Err(JournalError::ReadbackMismatch);
    }
    flash
        .write(
            (offset + MANIFEST_PREFIX_SIZE) as u32,
            &encoded[MANIFEST_PREFIX_SIZE..],
        )
        .map_err(JournalError::Backend)?;
    let mut readback = [0_u8; MANIFEST_SIZE];
    flash
        .read(offset as u32, &mut readback)
        .map_err(JournalError::Backend)?;
    if readback != encoded {
        return Err(JournalError::ReadbackMismatch);
    }
    Ok(())
}

fn encode_manifest(manifest: Manifest) -> [u8; MANIFEST_SIZE] {
    let mut encoded = [0xff_u8; MANIFEST_SIZE];
    encoded[..8].copy_from_slice(MANIFEST_MAGIC);
    put_u16(&mut encoded, 8, PHYSICAL_FORMAT_VERSION);
    put_u16(&mut encoded, 10, JOURNAL_SCHEMA_VERSION);
    encoded[12] = manifest.bank.id();
    encoded[13..16].fill(0);
    put_u64(&mut encoded, 16, manifest.generation);
    put_u32(&mut encoded, 24, manifest.bank.offset() as u32);
    put_u32(&mut encoded, 28, BANK_SIZE as u32);
    put_u32(&mut encoded, 32, SLOT_SIZE as u32);
    put_u32(&mut encoded, 36, BANK_SLOT_COUNT as u32);
    put_u64(&mut encoded, 40, manifest.baseline_count as u64);
    encoded[48..80].copy_from_slice(&manifest.baseline_tail);
    put_u16(&mut encoded, 80, MAX_DURABLE_RECORDS_PER_SUBMISSION as u16);
    encoded[82..MANIFEST_DATA_SIZE].fill(0);
    let digest = manifest_digest(&encoded[..MANIFEST_DATA_SIZE]);
    encoded[MANIFEST_DATA_SIZE..MANIFEST_PREFIX_SIZE].copy_from_slice(&digest);
    encoded[MANIFEST_PREFIX_SIZE..].copy_from_slice(&MANIFEST_COMMIT);
    encoded
}

fn write_handoff<F>(flash: &mut F, handoff: Handoff) -> Result<(), JournalError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    let mut encoded = [0xff_u8; HANDOFF_SIZE];
    encoded[..8].copy_from_slice(HANDOFF_MAGIC);
    put_u16(&mut encoded, 8, PHYSICAL_FORMAT_VERSION);
    put_u16(&mut encoded, 10, JOURNAL_SCHEMA_VERSION);
    encoded[12] = handoff.source.id();
    encoded[13] = handoff.target.id();
    encoded[14..16].fill(0);
    put_u64(&mut encoded, 16, handoff.source_generation);
    put_u64(&mut encoded, 24, handoff.target_generation);
    put_u64(&mut encoded, 32, handoff.source_count as u64);
    encoded[40..72].copy_from_slice(&handoff.source_tail);
    encoded[72..HANDOFF_DATA_SIZE].fill(0);
    let digest = handoff_digest(&encoded[..HANDOFF_DATA_SIZE]);
    encoded[HANDOFF_DATA_SIZE..HANDOFF_PREFIX_SIZE].copy_from_slice(&digest);
    encoded[HANDOFF_PREFIX_SIZE..].copy_from_slice(&HANDOFF_COMMIT);

    let offset = handoff.source.manifest_offset() + HANDOFF_OFFSET;
    flash
        .write(offset as u32, &encoded[..HANDOFF_PREFIX_SIZE])
        .map_err(JournalError::Backend)?;
    let mut prefix = [0_u8; HANDOFF_PREFIX_SIZE];
    flash
        .read(offset as u32, &mut prefix)
        .map_err(JournalError::Backend)?;
    if prefix != encoded[..HANDOFF_PREFIX_SIZE] {
        return Err(JournalError::ReadbackMismatch);
    }
    flash
        .write(
            (offset + HANDOFF_PREFIX_SIZE) as u32,
            &encoded[HANDOFF_PREFIX_SIZE..],
        )
        .map_err(JournalError::Backend)?;
    let mut readback = [0_u8; HANDOFF_SIZE];
    flash
        .read(offset as u32, &mut readback)
        .map_err(JournalError::Backend)?;
    if readback != encoded {
        return Err(JournalError::ReadbackMismatch);
    }
    Ok(())
}

fn decode_handoff(data: &[u8]) -> Option<Handoff> {
    if data.len() != HANDOFF_DATA_SIZE
        || &data[..8] != HANDOFF_MAGIC
        || read_u16(data, 8) != PHYSICAL_FORMAT_VERSION
        || read_u16(data, 10) != JOURNAL_SCHEMA_VERSION
        || data[14..16].iter().any(|byte| *byte != 0)
        || data[72..].iter().any(|byte| *byte != 0)
    {
        return None;
    }
    let source = Bank::from_id(data[12])?;
    let target = Bank::from_id(data[13])?;
    let source_count = usize::try_from(read_u64(data, 32)).ok()?;
    let mut source_tail = [0_u8; DIGEST_SIZE];
    source_tail.copy_from_slice(&data[40..72]);
    Some(Handoff {
        source,
        target,
        source_generation: read_u64(data, 16),
        target_generation: read_u64(data, 24),
        source_count,
        source_tail,
    })
}

fn scan_bank<const SUBMISSIONS: usize, F>(
    flash: &mut F,
    record: ManifestRecord,
    first_submission_id: SubmissionId,
    needle: Option<JournalEntry>,
) -> Result<ScanResult<SUBMISSIONS>, JournalError<F::Error>>
where
    F: ReadNorFlash,
{
    let manifest = record.manifest;
    let mut replay = SubmissionReplay::<SUBMISSIONS>::new(first_submission_id);
    let mut committed_records = 0_usize;
    let mut consumed_slots = 0_usize;
    let mut accepted_submissions = 0_usize;
    let mut tail_digest = ZERO_DIGEST;
    let mut needle_match = NeedleMatch::None;
    let needle_key = needle.map(logical_key);

    if manifest.baseline_count == 0 && manifest.baseline_tail != ZERO_DIGEST {
        return Err(JournalError::BaselineMismatch);
    }

    for slot_index in 0..BANK_SLOT_COUNT {
        let offset = manifest.bank.offset() + slot_index * SLOT_SIZE;
        let mut slot = [0_u8; SLOT_SIZE];
        flash
            .read(offset as u32, &mut slot)
            .map_err(JournalError::Backend)?;
        if slot.iter().all(|byte| *byte == 0xff) {
            continue;
        }
        consumed_slots = slot_index + 1;

        let marker: &[u8; COMMIT_SIZE] =
            slot[SLOT_COMMIT_OFFSET..]
                .try_into()
                .map_err(|_| JournalError::SlotCorrupt {
                    slot: slot_index,
                    reason: SlotCorruption::CommitMarker,
                })?;
        if marker != &SLOT_COMMIT {
            if monotonic_partial(marker, &SLOT_COMMIT) {
                continue;
            }
            return Err(JournalError::SlotCorrupt {
                slot: slot_index,
                reason: SlotCorruption::CommitMarker,
            });
        }

        let (entry, digest) = decode_slot(
            &slot,
            manifest.bank,
            manifest.generation,
            committed_records,
            tail_digest,
            slot_index,
        )?;
        if needle_key.is_some_and(|key| key == logical_key(entry)) {
            let observed = if needle == Some(entry) {
                NeedleMatch::Equivalent
            } else {
                NeedleMatch::Conflict
            };
            if needle_match != NeedleMatch::None {
                return Err(JournalError::DuplicateLogicalKey);
            }
            needle_match = observed;
        }
        match replay.apply_entry(entry) {
            Ok(ApplyOutcome::Applied) => {}
            Ok(ApplyOutcome::AlreadyEquivalent) => {
                return Err(JournalError::DuplicateLogicalKey);
            }
            Err(error) if replay_error_implies_duplicate_key(error) => {
                return Err(JournalError::DuplicateLogicalKey);
            }
            Err(error) => return Err(JournalError::SemanticReplay(error)),
        }
        if matches!(entry, JournalEntry::Accepted(_)) {
            accepted_submissions += 1;
        }
        committed_records += 1;
        tail_digest = digest;
        if committed_records == manifest.baseline_count && tail_digest != manifest.baseline_tail {
            return Err(JournalError::BaselineMismatch);
        }
    }

    if committed_records < manifest.baseline_count {
        return Err(JournalError::BaselineMismatch);
    }
    let mut bank_tail = [0_u8; BANK_TAIL_SIZE];
    flash
        .read(
            (manifest.bank.offset() + BANK_SLOT_COUNT * SLOT_SIZE) as u32,
            &mut bank_tail,
        )
        .map_err(JournalError::Backend)?;
    if bank_tail.iter().any(|byte| *byte != 0xff) {
        return Err(JournalError::GeometryMismatch);
    }
    if accepted_submissions
        .checked_mul(MAX_DURABLE_RECORDS_PER_SUBMISSION)
        .is_none_or(|reserved| reserved > BANK_SLOT_COUNT)
    {
        return Err(JournalError::ReservationInvariant);
    }
    if let HandoffState::Valid(handoff) = record.handoff
        && (handoff.source_count != committed_records || handoff.source_tail != tail_digest)
    {
        return Err(JournalError::CompactionInvariant);
    }

    if let Some(entry) = needle
        && needle_match == NeedleMatch::None
    {
        match replay.apply_entry(entry) {
            Ok(ApplyOutcome::Applied) => {}
            Ok(ApplyOutcome::AlreadyEquivalent) => {
                return Err(JournalError::DuplicateLogicalKey);
            }
            Err(error) => return Err(JournalError::SemanticReplay(error)),
        }
    }
    let index = replay.complete().map_err(JournalError::SemanticReplay)?;

    Ok(ScanResult {
        state: JournalState {
            bank: manifest.bank,
            generation: manifest.generation,
            committed_records,
            consumed_slots,
            accepted_submissions,
            tail_digest,
            compaction_pending: record.retire_manifest.is_some()
                || !matches!(record.handoff, HandoffState::None),
        },
        index,
        needle_match,
    })
}

const fn replay_error_implies_duplicate_key(error: ApplyError) -> bool {
    matches!(
        error,
        ApplyError::SubmissionConflict | ApplyError::RevisionConflict | ApplyError::StaleRevision
    )
}

fn encode_slot<E>(
    slot: &mut [u8; SLOT_SIZE],
    bank: Bank,
    generation: u64,
    ordinal: usize,
    previous_digest: [u8; DIGEST_SIZE],
    entry: JournalEntry,
) -> Result<[u8; DIGEST_SIZE], JournalError<E>> {
    slot.fill(0xff);
    slot[..8].copy_from_slice(SLOT_MAGIC);
    put_u16(slot, 8, PHYSICAL_FORMAT_VERSION);
    put_u16(slot, 10, JOURNAL_SCHEMA_VERSION);
    slot[12] = bank.id();
    slot[13] = entry_kind(entry);
    let body = &mut slot[SLOT_HEADER_SIZE..SLOT_DIGEST_OFFSET];
    let len = encode_journal_entry(&entry, body).map_err(|_| JournalError::SlotCorrupt {
        slot: ordinal,
        reason: SlotCorruption::CanonicalRecord,
    })?;
    put_u16(slot, 14, len as u16);
    put_u64(slot, 16, generation);
    put_u64(slot, 24, ordinal as u64);
    let key = logical_key(entry);
    put_u64(slot, 32, key.submission);
    put_u64(slot, 40, key.revision);
    slot[48..SLOT_HEADER_SIZE].fill(0);
    let digest = slot_digest(
        &slot[..SLOT_HEADER_SIZE],
        &slot[SLOT_HEADER_SIZE..SLOT_DIGEST_OFFSET],
        &previous_digest,
    );
    slot[SLOT_DIGEST_OFFSET..SLOT_COMMIT_OFFSET].copy_from_slice(&digest);
    slot[SLOT_COMMIT_OFFSET..].copy_from_slice(&SLOT_COMMIT);
    Ok(digest)
}

fn decode_slot<E>(
    slot: &[u8; SLOT_SIZE],
    expected_bank: Bank,
    expected_generation: u64,
    expected_ordinal: usize,
    previous_digest: [u8; DIGEST_SIZE],
    slot_index: usize,
) -> Result<(JournalEntry, [u8; DIGEST_SIZE]), JournalError<E>> {
    let corrupt = |reason| JournalError::SlotCorrupt {
        slot: slot_index,
        reason,
    };
    if &slot[..8] != SLOT_MAGIC || slot[48..SLOT_HEADER_SIZE].iter().any(|byte| *byte != 0) {
        return Err(corrupt(SlotCorruption::Header));
    }
    let physical = read_u16(slot, 8);
    if physical != PHYSICAL_FORMAT_VERSION {
        return Err(JournalError::UnsupportedPhysicalVersion(physical));
    }
    let schema = read_u16(slot, 10);
    if schema != JOURNAL_SCHEMA_VERSION {
        return Err(JournalError::UnsupportedSemanticVersion(schema));
    }
    if Bank::from_id(slot[12]) != Some(expected_bank) || read_u64(slot, 16) != expected_generation {
        return Err(corrupt(SlotCorruption::Header));
    }
    let ordinal =
        usize::try_from(read_u64(slot, 24)).map_err(|_| corrupt(SlotCorruption::Ordinal))?;
    if ordinal != expected_ordinal {
        return Err(corrupt(SlotCorruption::Ordinal));
    }
    let len = usize::from(read_u16(slot, 14));
    if len == 0
        || len > BODY_SIZE
        || slot[SLOT_HEADER_SIZE + len..SLOT_DIGEST_OFFSET]
            .iter()
            .any(|byte| *byte != 0xff)
    {
        return Err(corrupt(SlotCorruption::Header));
    }
    let expected_digest = slot_digest(
        &slot[..SLOT_HEADER_SIZE],
        &slot[SLOT_HEADER_SIZE..SLOT_DIGEST_OFFSET],
        &previous_digest,
    );
    if slot[SLOT_DIGEST_OFFSET..SLOT_COMMIT_OFFSET] != expected_digest {
        return Err(corrupt(SlotCorruption::Digest));
    }
    let body = &slot[SLOT_HEADER_SIZE..SLOT_HEADER_SIZE + len];
    let entry = decode_journal_entry(body).map_err(|_| corrupt(SlotCorruption::CanonicalRecord))?;
    let mut canonical = [0xff_u8; BODY_SIZE];
    let canonical_len = encode_journal_entry(&entry, &mut canonical)
        .map_err(|_| corrupt(SlotCorruption::CanonicalRecord))?;
    if canonical_len != len || canonical[..len] != *body {
        return Err(corrupt(SlotCorruption::CanonicalRecord));
    }
    let key = logical_key(entry);
    if slot[13] != entry_kind(entry)
        || read_u64(slot, 32) != key.submission
        || read_u64(slot, 40) != key.revision
    {
        return Err(corrupt(SlotCorruption::LogicalMetadata));
    }
    Ok((entry, expected_digest))
}

fn manifest_digest(data: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut hasher = Sha256::new();
    hasher.update(MANIFEST_DOMAIN);
    hasher.update(data);
    hasher.finalize().into()
}

fn handoff_digest(data: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut hasher = Sha256::new();
    hasher.update(HANDOFF_DOMAIN);
    hasher.update(data);
    hasher.finalize().into()
}

fn slot_digest(
    header: &[u8],
    body: &[u8],
    previous_digest: &[u8; DIGEST_SIZE],
) -> [u8; DIGEST_SIZE] {
    let mut hasher = Sha256::new();
    hasher.update(SLOT_DOMAIN);
    hasher.update(header);
    hasher.update(body);
    hasher.update(previous_digest);
    hasher.finalize().into()
}

fn monotonic_partial(actual: &[u8; COMMIT_SIZE], target: &[u8; COMMIT_SIZE]) -> bool {
    monotonic_programming_compatible(actual, target)
}

fn monotonic_programming_compatible(actual: &[u8], target: &[u8]) -> bool {
    actual.len() == target.len()
        && actual
            .iter()
            .zip(target)
            .all(|(actual, target)| actual & target == *target)
}

const fn logical_key(entry: JournalEntry) -> LogicalKey {
    match entry {
        JournalEntry::Accepted(accepted) => LogicalKey {
            submission: accepted.id().get(),
            revision: 0,
        },
        JournalEntry::StateTransition(transition) => LogicalKey {
            submission: transition.id().get(),
            revision: transition.revision(),
        },
        JournalEntry::Audit(audit) => LogicalKey {
            submission: audit.id().get(),
            revision: audit.revision(),
        },
    }
}

const fn entry_kind(entry: JournalEntry) -> u8 {
    match entry {
        JournalEntry::Accepted(_) => 0,
        JournalEntry::StateTransition(_) => 1,
        JournalEntry::Audit(_) => 2,
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap_or([0; 2]))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap_or([0; 8]))
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
