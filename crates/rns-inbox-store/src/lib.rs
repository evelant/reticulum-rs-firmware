//! Power-loss-safe one-entry raw-NOR store for an inbound RNS DATA item.
//!
//! This deliberately narrow qualification format owns an exact 2 MiB range but
//! programs only one fixed record at partition-relative offset zero. A claim is
//! programmed first, the canonical body and SHA-256 digest second, and an
//! irregular commit marker last. Every byte after that record must remain
//! erased. There is intentionally no erase, acknowledgement, reclamation, or
//! multi-item policy in this crate.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "allocation-free outcomes retain exact fixed-capacity candidate and item ownership"
)]

use core::num::NonZeroU64;

use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// Exact raw-NOR partition length required by physical format version 1.
pub const PARTITION_SIZE: usize = 0x20_0000;
/// Current physical format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;
/// Maximum plaintext RNS DATA payload retained by this qualification store.
pub const MAX_PAYLOAD_SIZE: usize = 383;
/// Exact committed record length at partition-relative offset zero.
pub const RECORD_SIZE: usize = 576;

const CLAIM_SIZE: usize = 32;
const BODY_OFFSET: usize = CLAIM_SIZE;
const BODY_END: usize = 512;
const DIGEST_OFFSET: usize = BODY_END;
const DIGEST_SIZE: usize = 32;
const COMMIT_OFFSET: usize = DIGEST_OFFSET + DIGEST_SIZE;
const COMMIT_SIZE: usize = 32;
const BODY_AND_DIGEST_SIZE: usize = COMMIT_OFFSET - BODY_OFFSET;
const INSPECTION_CHUNK_SIZE: usize = 256;

const MAGIC_OFFSET: usize = BODY_OFFSET;
const MAGIC_END: usize = MAGIC_OFFSET + 8;
const FORMAT_VERSION_OFFSET: usize = MAGIC_END;
const FORMAT_VERSION_END: usize = FORMAT_VERSION_OFFSET + 2;
const FORMAT_RESERVED_OFFSET: usize = FORMAT_VERSION_END;
const FORMAT_RESERVED_END: usize = FORMAT_RESERVED_OFFSET + 2;
const DEVICE_ID_OFFSET: usize = FORMAT_RESERVED_END;
const DEVICE_ID_END: usize = DEVICE_ID_OFFSET + 16;
const ABSOLUTE_OFFSET_OFFSET: usize = DEVICE_ID_END;
const ABSOLUTE_OFFSET_END: usize = ABSOLUTE_OFFSET_OFFSET + 8;
const PARTITION_LENGTH_OFFSET: usize = ABSOLUTE_OFFSET_END;
const PARTITION_LENGTH_END: usize = PARTITION_LENGTH_OFFSET + 8;
const ITEM_ID_OFFSET: usize = PARTITION_LENGTH_END;
const ITEM_ID_END: usize = ITEM_ID_OFFSET + 8;
const DESTINATION_OFFSET: usize = ITEM_ID_END;
const DESTINATION_END: usize = DESTINATION_OFFSET + 16;
const PAYLOAD_LENGTH_OFFSET: usize = DESTINATION_END;
const PAYLOAD_LENGTH_END: usize = PAYLOAD_LENGTH_OFFSET + 2;
const BODY_RESERVED_OFFSET: usize = PAYLOAD_LENGTH_END;
const PAYLOAD_OFFSET: usize = 112;
const PAYLOAD_END: usize = PAYLOAD_OFFSET + MAX_PAYLOAD_SIZE;
const BODY_PADDING_OFFSET: usize = PAYLOAD_END;

const MAGIC: &[u8; 8] = b"RNSINBX1";
const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/rns-inbox-store/record/v1\0";
const CLAIM_MARKER: [u8; CLAIM_SIZE] = [
    0xb6, 0x2d, 0x81, 0x4a, 0xe3, 0x57, 0x09, 0xcc, 0x71, 0x18, 0xf4, 0x93, 0x2a, 0xd5, 0x60, 0x0e,
    0x8b, 0x34, 0xca, 0x76, 0x1f, 0xa9, 0x45, 0xd2, 0x68, 0x03, 0xbe, 0x59, 0xe7, 0x20, 0x9d, 0x41,
];
const COMMIT_MARKER: [u8; COMMIT_SIZE] = [
    0x43, 0xda, 0x16, 0x8f, 0x25, 0xb1, 0x6c, 0xe8, 0x09, 0x72, 0xcd, 0x34, 0x91, 0x5a, 0xf0, 0x2e,
    0xb8, 0x67, 0x0c, 0xd3, 0x4f, 0x95, 0x21, 0xea, 0x7d, 0x38, 0xa4, 0x5c, 0x12, 0xf9, 0x60, 0x87,
];

const _: () = assert!(BODY_RESERVED_OFFSET < PAYLOAD_OFFSET);
const _: () = assert!(PAYLOAD_END < BODY_END);
const _: () = assert!(COMMIT_OFFSET + COMMIT_SIZE == RECORD_SIZE);
const _: () = assert!(BODY_AND_DIGEST_SIZE == 512);
const _: () = assert!(PARTITION_SIZE > RECORD_SIZE);

/// Stable physical identity of the flash device containing the inbox store.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InboxStoreDeviceId([u8; 16]);

impl InboxStoreDeviceId {
    /// Construct a device identity from exact product-supplied bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the exact device identity bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Exact physical provenance and format represented by one backend view.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InboxStoreBinding {
    device: InboxStoreDeviceId,
    absolute_offset: usize,
    length: usize,
    format_version: u16,
}

impl InboxStoreBinding {
    /// Construct one operation-scoped inbox-store binding.
    pub const fn new(
        device: InboxStoreDeviceId,
        absolute_offset: usize,
        length: usize,
        format_version: u16,
    ) -> Self {
        Self {
            device,
            absolute_offset,
            length,
            format_version,
        }
    }

    /// Physical device containing this store.
    pub const fn device(self) -> InboxStoreDeviceId {
        self.device
    }

    /// Absolute byte offset in the containing device.
    pub const fn absolute_offset(self) -> usize {
        self.absolute_offset
    }

    /// Exact bound range length.
    pub const fn length(self) -> usize {
        self.length
    }

    /// Expected physical format version.
    pub const fn format_version(self) -> u16 {
        self.format_version
    }
}

/// Binding validation failure detected before any store I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxStoreBindingError {
    /// A later operation names a different physical device.
    DeviceMismatch {
        /// Device retained by mounted state.
        expected: InboxStoreDeviceId,
        /// Device supplied by the later view.
        actual: InboxStoreDeviceId,
    },
    /// A later operation names a different absolute range.
    RangeMismatch {
        /// Retained absolute start.
        expected_offset: usize,
        /// Retained range length.
        expected_length: usize,
        /// Supplied absolute start.
        actual_offset: usize,
        /// Supplied range length.
        actual_length: usize,
    },
    /// Absolute start plus length cannot be represented.
    RangeOverflow {
        /// Supplied absolute start.
        absolute_offset: usize,
        /// Supplied range length.
        length: usize,
    },
    /// The binding names a different physical format.
    FormatVersionMismatch {
        /// Required version.
        expected: u16,
        /// Supplied version.
        actual: u16,
    },
    /// The bound range is not exactly 2 MiB.
    LengthMismatch {
        /// Required length.
        expected: usize,
        /// Supplied length.
        actual: usize,
    },
    /// Backend capacity differs from the bound range.
    CapacityMismatch {
        /// Required capacity.
        expected: usize,
        /// Backend capacity.
        actual: usize,
    },
    /// Read operations or the absolute range violate backend granularity.
    ReadAlignmentMismatch {
        /// Backend read granularity.
        read_size: usize,
    },
    /// Program operations or the absolute range violate backend granularity.
    WriteAlignmentMismatch {
        /// Backend program granularity.
        write_size: usize,
    },
    /// The exact range is incompatible with backend erase geometry.
    EraseAlignmentMismatch {
        /// Backend erase granularity.
        erase_size: usize,
    },
}

/// A partition-relative backend carrying exact inbox-store provenance.
pub struct BoundInboxStore<F> {
    backend: F,
    binding: InboxStoreBinding,
}

impl<F> BoundInboxStore<F> {
    /// Attach provenance to one range-restricted backend.
    pub const fn new(backend: F, binding: InboxStoreBinding) -> Self {
        Self { backend, binding }
    }

    /// Binding carried by this view.
    pub const fn binding(&self) -> InboxStoreBinding {
        self.binding
    }

    /// Borrow the wrapped backend.
    pub const fn backend(&self) -> &F {
        &self.backend
    }

    /// Mutably borrow the wrapped backend.
    pub fn backend_mut(&mut self) -> &mut F {
        &mut self.backend
    }

    /// Consume this view and recover the backend.
    pub fn into_backend(self) -> F {
        self.backend
    }
}

impl<F: ErrorType> ErrorType for BoundInboxStore<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for BoundInboxStore<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.backend.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.backend.capacity()
    }
}

impl<F: NorFlash> NorFlash for BoundInboxStore<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.backend.write(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.backend.erase(from, to)
    }
}

impl<F: MultiwriteNorFlash> MultiwriteNorFlash for BoundInboxStore<F> {}

/// Read-only NOR access proving the exact inbox-store device and range.
pub trait BoundInboxStoreReadAccess: ReadNorFlash {
    /// Binding represented by this operation-scoped view.
    fn inbox_store_binding(&self) -> InboxStoreBinding;
}

impl<F: ReadNorFlash> BoundInboxStoreReadAccess for BoundInboxStore<F> {
    fn inbox_store_binding(&self) -> InboxStoreBinding {
        self.binding
    }
}

/// Writable NOR access proving the exact inbox-store device and range.
pub trait BoundInboxStoreAccess: BoundInboxStoreReadAccess + MultiwriteNorFlash {}

impl<F: MultiwriteNorFlash> BoundInboxStoreAccess for BoundInboxStore<F> {}

/// Opaque, nonzero device-assigned inbox item identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InboxItemId(NonZeroU64);

impl InboxItemId {
    /// Identifier assigned to the only item supported by this format.
    pub const FIRST: Self = Self(NonZeroU64::MIN);

    /// Construct an opaque identifier from a nonzero scalar.
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Recover the stable nonzero scalar representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Complete 128-bit Reticulum destination hash for an inbox item.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InboxDestination([u8; 16]);

impl InboxDestination {
    /// Construct a destination from exact wire hash bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the exact destination bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Failure to construct an owned inbox candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxCandidateError {
    /// Payload exceeds the fixed qualification-store ceiling.
    PayloadTooLarge {
        /// Supplied byte length.
        actual: usize,
        /// Maximum supported byte length.
        maximum: usize,
    },
}

/// Owned candidate for the one durable inbox slot.
#[derive(Clone, Eq, PartialEq)]
pub struct InboxCandidate {
    destination: InboxDestination,
    payload: [u8; MAX_PAYLOAD_SIZE],
    payload_len: u16,
}

impl core::fmt::Debug for InboxCandidate {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InboxCandidate")
            .field("destination", &self.destination)
            .field("payload_len", &self.payload_len)
            .finish_non_exhaustive()
    }
}

impl InboxCandidate {
    /// Copy a bounded payload into an owned fixed-capacity candidate.
    pub fn new(destination: InboxDestination, payload: &[u8]) -> Result<Self, InboxCandidateError> {
        if payload.len() > MAX_PAYLOAD_SIZE {
            return Err(InboxCandidateError::PayloadTooLarge {
                actual: payload.len(),
                maximum: MAX_PAYLOAD_SIZE,
            });
        }
        let mut owned = [0_u8; MAX_PAYLOAD_SIZE];
        owned[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            destination,
            payload: owned,
            payload_len: payload.len() as u16,
        })
    }

    /// Destination addressed by this candidate.
    pub const fn destination(&self) -> InboxDestination {
        self.destination
    }

    /// Exact candidate payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.payload_len)]
    }

    fn into_item(self) -> InboxItem {
        InboxItem {
            id: InboxItemId::FIRST,
            destination: self.destination,
            payload: self.payload,
            payload_len: self.payload_len,
        }
    }
}

/// One complete committed item retained in runtime state.
#[derive(Clone, Eq, PartialEq)]
pub struct InboxItem {
    id: InboxItemId,
    destination: InboxDestination,
    payload: [u8; MAX_PAYLOAD_SIZE],
    payload_len: u16,
}

impl core::fmt::Debug for InboxItem {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InboxItem")
            .field("id", &self.id)
            .field("destination", &self.destination)
            .field("payload_len", &self.payload_len)
            .finish_non_exhaustive()
    }
}

impl InboxItem {
    /// Device-assigned item identifier.
    pub const fn id(&self) -> InboxItemId {
        self.id
    }

    /// Destination addressed by this item.
    pub const fn destination(&self) -> InboxDestination {
        self.destination
    }

    /// Exact committed payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.payload_len)]
    }
}

/// Canonical committed-record validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxRecordCorruption {
    /// Record magic is not format-v1 magic.
    Magic,
    /// Reserved bytes or payload padding are noncanonical.
    CanonicalPadding,
    /// The protected digest does not match the canonical prefix.
    Digest,
    /// The item identifier is zero or is not the first qualification ID.
    ItemId,
    /// Encoded payload length exceeds the fixed record capacity.
    PayloadLength,
    /// An encoded binding scalar cannot be represented on this target.
    BindingRepresentation,
}

/// Stable fail-closed classification of programmed media.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxStoreFault {
    /// Claim programming began but did not complete.
    InterruptedClaim,
    /// The exact claim exists but the body or commit did not complete.
    InterruptedRecord,
    /// Programmed bytes are not on a writer-produced trajectory.
    UnknownProgrammedData,
    /// Bytes beyond the one fixed record are not entirely erased.
    NonCanonicalRemainder,
    /// A committed record names an unsupported physical format.
    UnsupportedPhysicalVersion(u16),
    /// A digest-valid committed record names a different device or range.
    RecordBindingMismatch {
        /// Binding supplied by the current operation-scoped view.
        expected: InboxStoreBinding,
        /// Binding encoded in the committed record.
        actual: InboxStoreBinding,
    },
    /// A committed record failed canonical validation.
    CommittedRecordCorrupt(InboxRecordCorruption),
    /// Exact program readback did not establish the intended bytes.
    ReadbackMismatch {
        /// Program stage whose readback differed.
        stage: InboxProgramStage,
    },
}

/// Physical program stage used by admission diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxProgramStage {
    /// Irregular claim marker.
    Claim,
    /// Canonical body and its protected digest.
    BodyAndDigest,
    /// Irregular terminal commit marker.
    Commit,
}

/// Mount failure separated into pre-I/O binding, backend, and media faults.
#[derive(Debug)]
pub enum InboxStoreMountError<E> {
    /// Binding rejected before physical I/O.
    Binding(InboxStoreBindingError),
    /// Raw NOR backend failure.
    Backend(E),
    /// Stable fail-closed media classification.
    Fault(InboxStoreFault),
}

/// Borrowed state of a successfully mounted one-entry store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxStoreState<'a> {
    /// The complete 2 MiB partition is erased.
    Empty,
    /// One exact item is committed and the remainder is erased.
    Occupied(&'a InboxItem),
}

/// Mounted runtime copy of the exact physical state.
#[must_use = "mounted inbox ownership must be retained or deliberately discarded"]
pub struct MountedInboxStore {
    binding: InboxStoreBinding,
    item: Option<InboxItem>,
}

impl MountedInboxStore {
    /// Exact binding established by mount.
    pub const fn binding(&self) -> InboxStoreBinding {
        self.binding
    }

    /// Borrow the complete current state without accessing flash.
    pub const fn state(&self) -> InboxStoreState<'_> {
        match self.item.as_ref() {
            Some(item) => InboxStoreState::Occupied(item),
            None => InboxStoreState::Empty,
        }
    }

    /// Borrow the committed item, when occupied.
    pub const fn item(&self) -> Option<&InboxItem> {
        self.item.as_ref()
    }

    /// Admit one candidate or return its ownership unchanged.
    ///
    /// Empty media is re-inspected before the claim. Each program stage is read
    /// back exactly. If a backend reports an error after completing a write, an
    /// equal readback reconciles it and admission continues. Runtime visibility
    /// changes only after the complete committed record has been decoded again.
    pub fn accept<A>(
        &mut self,
        access: &mut A,
        candidate: InboxCandidate,
    ) -> Result<InboxAdmissionOutcome, InboxAdmissionFailure<A::Error>>
    where
        A: BoundInboxStoreAccess,
    {
        if self.item.is_some() {
            return Ok(InboxAdmissionOutcome::Full(candidate));
        }

        if let Err(error) = validate_writable_access(access)
            .and_then(|actual| validate_same_binding(self.binding, actual))
        {
            return Err(InboxAdmissionFailure::new(
                candidate,
                InboxAdmissionError::Binding(error),
            ));
        }

        match inspect_media(access, self.binding) {
            Ok(MediaState::Empty) => {}
            Ok(MediaState::Occupied(existing)) => {
                if item_matches_candidate(&existing, &candidate) {
                    let id = existing.id();
                    self.item = Some(existing);
                    return Ok(InboxAdmissionOutcome::Accepted(id));
                }
                self.item = Some(existing);
                return Ok(InboxAdmissionOutcome::Full(candidate));
            }
            Err(InspectError::Backend(error)) => {
                return Err(InboxAdmissionFailure::new(
                    candidate,
                    InboxAdmissionError::Backend { stage: None, error },
                ));
            }
            Err(InspectError::Fault(fault)) => {
                return Err(InboxAdmissionFailure::new(
                    candidate,
                    InboxAdmissionError::Fault(fault),
                ));
            }
        }

        let intended = candidate.clone().into_item();
        let encoded = encode_record(self.binding, &intended);
        for (stage, offset, bytes) in [
            (InboxProgramStage::Claim, 0_usize, &encoded[..CLAIM_SIZE]),
            (
                InboxProgramStage::BodyAndDigest,
                BODY_OFFSET,
                &encoded[BODY_OFFSET..COMMIT_OFFSET],
            ),
            (
                InboxProgramStage::Commit,
                COMMIT_OFFSET,
                &encoded[COMMIT_OFFSET..],
            ),
        ] {
            if let Err(error) = program_exact(access, offset, bytes, stage) {
                return Err(InboxAdmissionFailure::new(candidate, error));
            }
        }

        let mut committed_record = [0_u8; RECORD_SIZE];
        let verified = access
            .read(0, &mut committed_record)
            .map_err(|error| {
                InboxAdmissionFailure::new(
                    candidate.clone(),
                    InboxAdmissionError::Backend {
                        stage: Some(InboxProgramStage::Commit),
                        error,
                    },
                )
            })
            .and_then(|()| {
                if committed_record != encoded {
                    return Err(InboxAdmissionFailure::new(
                        candidate.clone(),
                        InboxAdmissionError::Fault(InboxStoreFault::ReadbackMismatch {
                            stage: InboxProgramStage::Commit,
                        }),
                    ));
                }
                decode_committed_record(&committed_record, self.binding).map_err(|fault| {
                    InboxAdmissionFailure::new(candidate.clone(), InboxAdmissionError::Fault(fault))
                })
            });
        match verified {
            Ok(committed) if committed == intended => {
                let id = committed.id();
                self.item = Some(committed);
                Ok(InboxAdmissionOutcome::Accepted(id))
            }
            Ok(_) => Err(InboxAdmissionFailure::new(
                candidate,
                InboxAdmissionError::Fault(InboxStoreFault::ReadbackMismatch {
                    stage: InboxProgramStage::Commit,
                }),
            )),
            Err(failure) => Err(failure),
        }
    }
}

/// Definitive result of one admission attempt.
#[derive(Eq, PartialEq)]
pub enum InboxAdmissionOutcome {
    /// The exact item is committed and retained in mounted runtime state.
    Accepted(InboxItemId),
    /// The single slot was already occupied; candidate ownership is returned.
    Full(InboxCandidate),
}

impl core::fmt::Debug for InboxAdmissionOutcome {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Accepted(id) => formatter.debug_tuple("Accepted").field(id).finish(),
            Self::Full(candidate) => formatter.debug_tuple("Full").field(candidate).finish(),
        }
    }
}

/// Admission failure reason, excluding the returned candidate owner.
#[derive(Debug, Eq, PartialEq)]
pub enum InboxAdmissionError<E> {
    /// Binding rejected before the relevant physical operation.
    Binding(InboxStoreBindingError),
    /// Raw NOR backend failure; completion may be ambiguous.
    Backend {
        /// Program stage, or `None` during inspection.
        stage: Option<InboxProgramStage>,
        /// Concrete backend error.
        error: E,
    },
    /// Stable fail-closed media or readback classification.
    Fault(InboxStoreFault),
}

/// Admission failure retaining the complete candidate for deliberate retry.
#[derive(Eq, PartialEq)]
pub struct InboxAdmissionFailure<E> {
    candidate: InboxCandidate,
    error: InboxAdmissionError<E>,
}

impl<E: core::fmt::Debug> core::fmt::Debug for InboxAdmissionFailure<E> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InboxAdmissionFailure")
            .field("candidate", &self.candidate)
            .field("error", &self.error)
            .finish()
    }
}

impl<E> InboxAdmissionFailure<E> {
    fn new(candidate: InboxCandidate, error: InboxAdmissionError<E>) -> Self {
        Self { candidate, error }
    }

    /// Borrow the stable or backend-specific failure reason.
    pub const fn error(&self) -> &InboxAdmissionError<E> {
        &self.error
    }

    /// Recover the candidate and failure reason.
    pub fn into_parts(self) -> (InboxCandidate, InboxAdmissionError<E>) {
        (self.candidate, self.error)
    }
}

/// Mount an exact read-only view without programming or erasing any byte.
pub fn mount<A>(access: &mut A) -> Result<MountedInboxStore, InboxStoreMountError<A::Error>>
where
    A: BoundInboxStoreReadAccess,
{
    let binding = validate_read_access(access).map_err(InboxStoreMountError::Binding)?;
    let item = match inspect_media(access, binding) {
        Ok(MediaState::Empty) => None,
        Ok(MediaState::Occupied(item)) => Some(item),
        Err(InspectError::Backend(error)) => return Err(InboxStoreMountError::Backend(error)),
        Err(InspectError::Fault(fault)) => return Err(InboxStoreMountError::Fault(fault)),
    };
    Ok(MountedInboxStore { binding, item })
}

enum MediaState {
    Empty,
    Occupied(InboxItem),
}

enum InspectError<E> {
    Backend(E),
    Fault(InboxStoreFault),
}

fn inspect_media<A>(
    access: &mut A,
    expected_binding: InboxStoreBinding,
) -> Result<MediaState, InspectError<A::Error>>
where
    A: BoundInboxStoreReadAccess,
{
    let mut record = [0_u8; RECORD_SIZE];
    access.read(0, &mut record).map_err(InspectError::Backend)?;
    if !remainder_is_erased(access)? {
        return Err(InspectError::Fault(InboxStoreFault::NonCanonicalRemainder));
    }

    if record.iter().all(|byte| *byte == 0xff) {
        return Ok(MediaState::Empty);
    }

    let claim = &record[..CLAIM_SIZE];
    if claim != CLAIM_MARKER {
        if monotonic_partial(claim, &CLAIM_MARKER)
            && record[CLAIM_SIZE..].iter().all(|byte| *byte == 0xff)
        {
            return Err(InspectError::Fault(InboxStoreFault::InterruptedClaim));
        }
        return Err(InspectError::Fault(InboxStoreFault::UnknownProgrammedData));
    }

    let commit = &record[COMMIT_OFFSET..];
    if commit != COMMIT_MARKER {
        if monotonic_partial(commit, &COMMIT_MARKER) {
            return Err(InspectError::Fault(InboxStoreFault::InterruptedRecord));
        }
        return Err(InspectError::Fault(InboxStoreFault::UnknownProgrammedData));
    }

    decode_committed_record(&record, expected_binding)
        .map(MediaState::Occupied)
        .map_err(InspectError::Fault)
}

fn remainder_is_erased<A>(access: &mut A) -> Result<bool, InspectError<A::Error>>
where
    A: BoundInboxStoreReadAccess,
{
    let mut bytes = [0_u8; INSPECTION_CHUNK_SIZE];
    let mut offset = RECORD_SIZE;
    while offset < PARTITION_SIZE {
        let length = (PARTITION_SIZE - offset).min(bytes.len());
        access
            .read(offset as u32, &mut bytes[..length])
            .map_err(InspectError::Backend)?;
        if bytes[..length].iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        offset += length;
    }
    Ok(true)
}

fn decode_committed_record(
    record: &[u8; RECORD_SIZE],
    expected_binding: InboxStoreBinding,
) -> Result<InboxItem, InboxStoreFault> {
    if &record[MAGIC_OFFSET..MAGIC_END] != MAGIC {
        return Err(InboxStoreFault::CommittedRecordCorrupt(
            InboxRecordCorruption::Magic,
        ));
    }
    let version = read_u16(&record[FORMAT_VERSION_OFFSET..FORMAT_VERSION_END]);
    if version != PHYSICAL_FORMAT_VERSION {
        return Err(InboxStoreFault::UnsupportedPhysicalVersion(version));
    }
    let expected_digest = record_digest(&record[..DIGEST_OFFSET]);
    if record[DIGEST_OFFSET..COMMIT_OFFSET] != expected_digest {
        return Err(InboxStoreFault::CommittedRecordCorrupt(
            InboxRecordCorruption::Digest,
        ));
    }
    if record[FORMAT_RESERVED_OFFSET..FORMAT_RESERVED_END]
        .iter()
        .chain(&record[BODY_RESERVED_OFFSET..PAYLOAD_OFFSET])
        .chain(&record[BODY_PADDING_OFFSET..BODY_END])
        .any(|byte| *byte != 0)
    {
        return Err(InboxStoreFault::CommittedRecordCorrupt(
            InboxRecordCorruption::CanonicalPadding,
        ));
    }

    let mut device = [0_u8; 16];
    device.copy_from_slice(&record[DEVICE_ID_OFFSET..DEVICE_ID_END]);
    let encoded_offset = usize::try_from(read_u64(
        &record[ABSOLUTE_OFFSET_OFFSET..ABSOLUTE_OFFSET_END],
    ))
    .map_err(|_| {
        InboxStoreFault::CommittedRecordCorrupt(InboxRecordCorruption::BindingRepresentation)
    })?;
    let encoded_length = usize::try_from(read_u64(
        &record[PARTITION_LENGTH_OFFSET..PARTITION_LENGTH_END],
    ))
    .map_err(|_| {
        InboxStoreFault::CommittedRecordCorrupt(InboxRecordCorruption::BindingRepresentation)
    })?;
    let encoded_binding = InboxStoreBinding::new(
        InboxStoreDeviceId::new(device),
        encoded_offset,
        encoded_length,
        version,
    );
    if encoded_binding != expected_binding {
        return Err(InboxStoreFault::RecordBindingMismatch {
            expected: expected_binding,
            actual: encoded_binding,
        });
    }

    let id = InboxItemId::new(read_u64(&record[ITEM_ID_OFFSET..ITEM_ID_END])).ok_or(
        InboxStoreFault::CommittedRecordCorrupt(InboxRecordCorruption::ItemId),
    )?;
    if id != InboxItemId::FIRST {
        return Err(InboxStoreFault::CommittedRecordCorrupt(
            InboxRecordCorruption::ItemId,
        ));
    }
    let payload_len = read_u16(&record[PAYLOAD_LENGTH_OFFSET..PAYLOAD_LENGTH_END]);
    if usize::from(payload_len) > MAX_PAYLOAD_SIZE {
        return Err(InboxStoreFault::CommittedRecordCorrupt(
            InboxRecordCorruption::PayloadLength,
        ));
    }
    let payload_end = PAYLOAD_OFFSET + usize::from(payload_len);
    if record[payload_end..PAYLOAD_END]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(InboxStoreFault::CommittedRecordCorrupt(
            InboxRecordCorruption::CanonicalPadding,
        ));
    }
    let mut destination = [0_u8; 16];
    destination.copy_from_slice(&record[DESTINATION_OFFSET..DESTINATION_END]);
    let mut payload = [0_u8; MAX_PAYLOAD_SIZE];
    payload.copy_from_slice(&record[PAYLOAD_OFFSET..PAYLOAD_END]);
    Ok(InboxItem {
        id,
        destination: InboxDestination::new(destination),
        payload,
        payload_len,
    })
}

fn encode_record(binding: InboxStoreBinding, item: &InboxItem) -> [u8; RECORD_SIZE] {
    let mut record = [0_u8; RECORD_SIZE];
    record[..CLAIM_SIZE].copy_from_slice(&CLAIM_MARKER);
    record[MAGIC_OFFSET..MAGIC_END].copy_from_slice(MAGIC);
    record[FORMAT_VERSION_OFFSET..FORMAT_VERSION_END]
        .copy_from_slice(&PHYSICAL_FORMAT_VERSION.to_le_bytes());
    record[DEVICE_ID_OFFSET..DEVICE_ID_END].copy_from_slice(binding.device().as_bytes());
    record[ABSOLUTE_OFFSET_OFFSET..ABSOLUTE_OFFSET_END]
        .copy_from_slice(&(binding.absolute_offset() as u64).to_le_bytes());
    record[PARTITION_LENGTH_OFFSET..PARTITION_LENGTH_END]
        .copy_from_slice(&(binding.length() as u64).to_le_bytes());
    record[ITEM_ID_OFFSET..ITEM_ID_END].copy_from_slice(&item.id().get().to_le_bytes());
    record[DESTINATION_OFFSET..DESTINATION_END].copy_from_slice(item.destination().as_bytes());
    record[PAYLOAD_LENGTH_OFFSET..PAYLOAD_LENGTH_END]
        .copy_from_slice(&item.payload_len.to_le_bytes());
    record[PAYLOAD_OFFSET..PAYLOAD_OFFSET + item.payload().len()].copy_from_slice(item.payload());
    let digest = record_digest(&record[..DIGEST_OFFSET]);
    record[DIGEST_OFFSET..COMMIT_OFFSET].copy_from_slice(&digest);
    record[COMMIT_OFFSET..].copy_from_slice(&COMMIT_MARKER);
    record
}

fn record_digest(prefix: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    digest.update(prefix);
    digest.finalize().into()
}

fn program_exact<A>(
    access: &mut A,
    offset: usize,
    expected: &[u8],
    stage: InboxProgramStage,
) -> Result<(), InboxAdmissionError<A::Error>>
where
    A: BoundInboxStoreAccess,
{
    let write_result = access.write(offset as u32, expected);
    let mut readback = [0_u8; BODY_AND_DIGEST_SIZE];
    let read_result = access.read(offset as u32, &mut readback[..expected.len()]);
    match (write_result, read_result) {
        (_, Ok(())) if readback[..expected.len()] == *expected => Ok(()),
        (Err(error), _) => Err(InboxAdmissionError::Backend {
            stage: Some(stage),
            error,
        }),
        (Ok(()), Err(error)) => Err(InboxAdmissionError::Backend {
            stage: Some(stage),
            error,
        }),
        (Ok(()), Ok(())) => Err(InboxAdmissionError::Fault(
            InboxStoreFault::ReadbackMismatch { stage },
        )),
    }
}

fn item_matches_candidate(item: &InboxItem, candidate: &InboxCandidate) -> bool {
    item.id() == InboxItemId::FIRST
        && item.destination() == candidate.destination()
        && item.payload() == candidate.payload()
}

fn validate_read_access<A>(access: &A) -> Result<InboxStoreBinding, InboxStoreBindingError>
where
    A: BoundInboxStoreReadAccess,
{
    let binding = access.inbox_store_binding();
    validate_binding_shape(binding, access.capacity())?;
    let read_size = A::READ_SIZE;
    if read_size == 0
        || !binding.absolute_offset().is_multiple_of(read_size)
        || !binding.length().is_multiple_of(read_size)
        || !RECORD_SIZE.is_multiple_of(read_size)
        || !INSPECTION_CHUNK_SIZE.is_multiple_of(read_size)
    {
        return Err(InboxStoreBindingError::ReadAlignmentMismatch { read_size });
    }
    Ok(binding)
}

fn validate_writable_access<A>(access: &A) -> Result<InboxStoreBinding, InboxStoreBindingError>
where
    A: BoundInboxStoreAccess,
{
    let binding = validate_read_access(access)?;
    let write_size = A::WRITE_SIZE;
    if write_size == 0
        || !binding.absolute_offset().is_multiple_of(write_size)
        || !CLAIM_SIZE.is_multiple_of(write_size)
        || !BODY_OFFSET.is_multiple_of(write_size)
        || !BODY_AND_DIGEST_SIZE.is_multiple_of(write_size)
        || !COMMIT_OFFSET.is_multiple_of(write_size)
        || !COMMIT_SIZE.is_multiple_of(write_size)
    {
        return Err(InboxStoreBindingError::WriteAlignmentMismatch { write_size });
    }
    let erase_size = A::ERASE_SIZE;
    if erase_size == 0
        || !binding.absolute_offset().is_multiple_of(erase_size)
        || !binding.length().is_multiple_of(erase_size)
    {
        return Err(InboxStoreBindingError::EraseAlignmentMismatch { erase_size });
    }
    Ok(binding)
}

fn validate_binding_shape(
    binding: InboxStoreBinding,
    capacity: usize,
) -> Result<(), InboxStoreBindingError> {
    if binding
        .absolute_offset()
        .checked_add(binding.length())
        .is_none()
    {
        return Err(InboxStoreBindingError::RangeOverflow {
            absolute_offset: binding.absolute_offset(),
            length: binding.length(),
        });
    }
    if binding.format_version() != PHYSICAL_FORMAT_VERSION {
        return Err(InboxStoreBindingError::FormatVersionMismatch {
            expected: PHYSICAL_FORMAT_VERSION,
            actual: binding.format_version(),
        });
    }
    if binding.length() != PARTITION_SIZE {
        return Err(InboxStoreBindingError::LengthMismatch {
            expected: PARTITION_SIZE,
            actual: binding.length(),
        });
    }
    if capacity != binding.length() {
        return Err(InboxStoreBindingError::CapacityMismatch {
            expected: binding.length(),
            actual: capacity,
        });
    }
    Ok(())
}

fn validate_same_binding(
    expected: InboxStoreBinding,
    actual: InboxStoreBinding,
) -> Result<InboxStoreBinding, InboxStoreBindingError> {
    if expected.device() != actual.device() {
        return Err(InboxStoreBindingError::DeviceMismatch {
            expected: expected.device(),
            actual: actual.device(),
        });
    }
    if expected.absolute_offset() != actual.absolute_offset()
        || expected.length() != actual.length()
    {
        return Err(InboxStoreBindingError::RangeMismatch {
            expected_offset: expected.absolute_offset(),
            expected_length: expected.length(),
            actual_offset: actual.absolute_offset(),
            actual_length: actual.length(),
        });
    }
    if expected.format_version() != actual.format_version() {
        return Err(InboxStoreBindingError::FormatVersionMismatch {
            expected: expected.format_version(),
            actual: actual.format_version(),
        });
    }
    Ok(actual)
}

fn monotonic_partial(observed: &[u8], expected: &[u8]) -> bool {
    observed.len() == expected.len()
        && observed
            .iter()
            .zip(expected)
            .all(|(observed, expected)| observed & expected == *expected)
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}
