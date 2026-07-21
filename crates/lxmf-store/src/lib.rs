//! Append-only, power-loss-safe NOR ownership for inbound LXMF messages.
//!
//! The store uses variable-length runs of 4 KiB extents. Every extent carries
//! a self-describing header, normalized full-wire bytes are streamed directly
//! from the semantic candidate, and a terminal marker in the final extent is
//! programmed last. No operation allocates or buffers a complete message.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::large_enum_variant,
    clippy::result_large_err,
    reason = "failures retain the borrowed semantic candidate for an explicit retry"
)]

use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use reticulum_lxmf_model::{
    AuthenticatedMaterialFingerprint, CarrierProvenance, DestinationHash, DurableMessageReceipt,
    ExactWireDigest, InboundMessageCandidate, InboundMessageLengths, InboundMessageMetadata,
    MessageHandle, MessageId, ReplayFingerprint, ReplayRelation, RequiredStampCost, SourceHash,
    StampAdmissionProvenance,
};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// Physical erase extent used by format version 1.
pub const EXTENT_SIZE: usize = 4096;
/// Current physical format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;
/// Bytes occupied by the repeated header in every extent.
pub const EXTENT_HEADER_SIZE: usize = 512;
/// Bytes occupied by the footer in the final extent.
pub const RECORD_FOOTER_SIZE: usize = 256;

const IO_CHUNK_SIZE: usize = 256;
const EXTENT_PAYLOAD_SIZE: usize = EXTENT_SIZE - EXTENT_HEADER_SIZE;
const FINAL_EXTENT_PAYLOAD_SIZE: usize = EXTENT_PAYLOAD_SIZE - RECORD_FOOTER_SIZE;
const COMMIT_SIZE: usize = 32;
const FOOTER_COMMIT_OFFSET: usize = RECORD_FOOTER_SIZE - COMMIT_SIZE;
const HEADER_DIGEST_OFFSET: usize = EXTENT_HEADER_SIZE - 32;
const INSPECTION_CHUNK_SIZE: usize = 256;

const CLAIM_MARKER: [u8; 32] = [
    0x6d, 0x13, 0xa8, 0x42, 0xf0, 0x2c, 0x79, 0xb5, 0x31, 0xe6, 0x0a, 0x97, 0x54, 0xcb, 0x25, 0x8f,
    0xd2, 0x48, 0x7a, 0x1c, 0xb9, 0x63, 0x05, 0xee, 0x36, 0x91, 0x5b, 0xc4, 0x0f, 0xa7, 0x68, 0x23,
];
const COMMIT_MARKER: [u8; COMMIT_SIZE] = [
    0x47, 0xbc, 0x12, 0xe9, 0x65, 0x08, 0xd3, 0x2a, 0xf1, 0x84, 0x3e, 0x59, 0xa6, 0x0d, 0xc8, 0x71,
    0x25, 0xfa, 0x43, 0x9c, 0x16, 0xb0, 0x6e, 0xd7, 0x38, 0x82, 0x0b, 0xed, 0x51, 0xa4, 0x7f, 0x19,
];
const HEADER_MAGIC: &[u8; 8] = b"LXMFEXT1";
const FOOTER_MAGIC: &[u8; 8] = b"LXMFEND1";
const HEADER_DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/lxmf-store/header/v1\0";
const RECORD_DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/lxmf-store/record/v1\0";

const HEADER_CLAIM_OFFSET: usize = 0;
const HEADER_MAGIC_OFFSET: usize = 32;
const HEADER_VERSION_OFFSET: usize = 40;
const HEADER_SIZE_OFFSET: usize = 42;
const HEADER_EXTENT_INDEX_OFFSET: usize = 44;
const HEADER_EXTENT_COUNT_OFFSET: usize = 46;
const HEADER_HANDLE_OFFSET: usize = 48;
const HEADER_DEVICE_OFFSET: usize = 56;
const HEADER_ABSOLUTE_OFFSET: usize = 72;
const HEADER_PARTITION_LENGTH_OFFSET: usize = 80;
const HEADER_MESSAGE_ID_OFFSET: usize = 88;
const HEADER_AUTHENTICATED_MATERIAL_OFFSET: usize = 120;
const HEADER_DESTINATION_OFFSET: usize = 152;
const HEADER_SOURCE_OFFSET: usize = 168;
const HEADER_TIMESTAMP_OFFSET: usize = 184;
const HEADER_CARRIER_OFFSET: usize = 192;
const HEADER_STAMP_KIND_OFFSET: usize = 193;
const HEADER_STAMP_ARGUMENT_OFFSET: usize = 194;
const HEADER_STAMP_OBSERVED_OFFSET: usize = 196;
const HEADER_LENGTHS_OFFSET: usize = 200;
const HEADER_EXACT_WIRE_DIGEST_OFFSET: usize = 220;
const HEADER_PADDING_OFFSET: usize = 252;

const FOOTER_MAGIC_OFFSET: usize = 0;
const FOOTER_VERSION_OFFSET: usize = 8;
const FOOTER_HANDLE_OFFSET: usize = 12;
const FOOTER_EXTENT_COUNT_OFFSET: usize = 20;
const FOOTER_WIRE_LENGTH_OFFSET: usize = 24;
const FOOTER_WIRE_DIGEST_OFFSET: usize = 28;
const FOOTER_RECORD_DIGEST_OFFSET: usize = 60;
const FOOTER_PADDING_OFFSET: usize = 92;

const _: () = assert!(EXTENT_HEADER_SIZE.is_multiple_of(IO_CHUNK_SIZE));
const _: () = assert!(RECORD_FOOTER_SIZE == IO_CHUNK_SIZE);
const _: () = assert!(EXTENT_PAYLOAD_SIZE == 3584);
const _: () = assert!(FINAL_EXTENT_PAYLOAD_SIZE == 3328);

/// Stable physical identity of the NOR device containing the store.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LxmfStoreDeviceId([u8; 16]);

impl LxmfStoreDeviceId {
    /// Construct an identity from exact product-supplied bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the exact identity bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Exact physical device, range, and format represented by an operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LxmfStoreBinding {
    device: LxmfStoreDeviceId,
    absolute_offset: usize,
    length: usize,
    format_version: u16,
}

impl LxmfStoreBinding {
    /// Construct an operation-scoped binding.
    pub const fn new(
        device: LxmfStoreDeviceId,
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

    /// Physical device containing the store.
    pub const fn device(self) -> LxmfStoreDeviceId {
        self.device
    }

    /// Absolute offset of the bound range in the containing device.
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

/// Binding validation failure detected before store I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfStoreBindingError {
    /// A later operation names a different physical device.
    DeviceMismatch {
        /// Mounted device identity.
        expected: LxmfStoreDeviceId,
        /// Supplied device identity.
        actual: LxmfStoreDeviceId,
    },
    /// A later operation names a different physical range.
    RangeMismatch {
        /// Mounted absolute offset.
        expected_offset: usize,
        /// Mounted range length.
        expected_length: usize,
        /// Supplied absolute offset.
        actual_offset: usize,
        /// Supplied range length.
        actual_length: usize,
    },
    /// Absolute offset plus length cannot be represented.
    RangeOverflow {
        /// Supplied absolute offset.
        absolute_offset: usize,
        /// Supplied range length.
        length: usize,
    },
    /// A binding names a different physical format.
    FormatVersionMismatch {
        /// Required version.
        expected: u16,
        /// Supplied version.
        actual: u16,
    },
    /// Range length is zero or is not a multiple of 4 KiB.
    LengthNotExtentMultiple {
        /// Supplied range length.
        actual: usize,
    },
    /// Backend capacity differs from the exact bound range.
    CapacityMismatch {
        /// Bound range length.
        expected: usize,
        /// Backend capacity.
        actual: usize,
    },
    /// Read geometry cannot support the physical format.
    ReadAlignmentMismatch {
        /// Backend read granularity.
        read_size: usize,
    },
    /// Program geometry cannot support the physical format.
    WriteAlignmentMismatch {
        /// Backend program granularity.
        write_size: usize,
    },
    /// Erase geometry cannot support independent extents.
    EraseAlignmentMismatch {
        /// Backend erase granularity.
        erase_size: usize,
    },
}

/// A partition-relative backend carrying exact store provenance.
pub struct BoundLxmfStore<F> {
    backend: F,
    binding: LxmfStoreBinding,
}

impl<F> BoundLxmfStore<F> {
    /// Attach physical provenance to a range-restricted backend.
    pub const fn new(backend: F, binding: LxmfStoreBinding) -> Self {
        Self { backend, binding }
    }

    /// Binding carried by this view.
    pub const fn binding(&self) -> LxmfStoreBinding {
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

impl<F: ErrorType> ErrorType for BoundLxmfStore<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for BoundLxmfStore<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.backend.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.backend.capacity()
    }
}

impl<F: NorFlash> NorFlash for BoundLxmfStore<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.backend.write(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.backend.erase(from, to)
    }
}

impl<F: MultiwriteNorFlash> MultiwriteNorFlash for BoundLxmfStore<F> {}

/// Read-only access proving an exact physical store binding.
pub trait BoundLxmfStoreReadAccess: ReadNorFlash {
    /// Binding represented by this operation-scoped view.
    fn lxmf_store_binding(&self) -> LxmfStoreBinding;
}

impl<F: ReadNorFlash> BoundLxmfStoreReadAccess for BoundLxmfStore<F> {
    fn lxmf_store_binding(&self) -> LxmfStoreBinding {
        self.binding
    }
}

/// Writable access proving an exact physical store binding.
pub trait BoundLxmfStoreAccess: BoundLxmfStoreReadAccess + MultiwriteNorFlash {}

impl<F: MultiwriteNorFlash> BoundLxmfStoreAccess for BoundLxmfStore<F> {}

/// Maximum normalized wire bytes representable by one exact partition length.
///
/// Version 1 repeats a 512-byte header in every extent. That repetition lets a
/// mount resynchronize at every 4 KiB boundary even when power fails while a
/// multi-extent claim is being written. The final extent also reserves one
/// 256-byte footer, so a 2 MiB range holds 1,834,752 message bytes rather than
/// the complete nominal partition size. Version 1 stores its extent count as a
/// `u16`, so partitions larger than 65,535 extents do not increase the maximum
/// size of one record. A prospective 1 MiB normalized record is representable;
/// no Resource product profile is selected by this crate.
pub const fn max_normalized_wire_bytes(binding_length: usize) -> Option<usize> {
    if binding_length == 0 || !binding_length.is_multiple_of(EXTENT_SIZE) {
        return None;
    }
    let available_extents = binding_length / EXTENT_SIZE;
    let extents = if available_extents > u16::MAX as usize {
        u16::MAX as usize
    } else {
        available_extents
    };
    match extents.checked_mul(EXTENT_PAYLOAD_SIZE) {
        Some(bytes) => bytes.checked_sub(RECORD_FOOTER_SIZE),
        None => None,
    }
}

/// Stable class of a committed-media validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfStoreFault {
    /// Programmed bytes cannot be attributed to a valid in-range writer
    /// trajectory, including unclaimed media and orphaned continuations.
    UnknownProgrammedExtent {
        /// Partition-relative extent containing unknown bytes.
        extent: usize,
    },
    /// An exact commit marker exists without a valid record start.
    UnknownCommittedExtent {
        /// Partition-relative extent containing the marker.
        extent: usize,
    },
    /// A committed record has an invalid first or continuation header.
    CommittedHeaderCorrupt {
        /// First extent of the record.
        extent: usize,
    },
    /// A committed record has an invalid canonical footer.
    CommittedFooterCorrupt {
        /// First extent of the record.
        extent: usize,
    },
    /// A committed record names a different bound device or range.
    CommittedBindingMismatch {
        /// Binding supplied by the current access.
        expected: LxmfStoreBinding,
        /// Binding encoded by committed media.
        actual: LxmfStoreBinding,
    },
    /// A committed record uses an unsupported physical version.
    UnsupportedPhysicalVersion {
        /// First extent of the record.
        extent: usize,
        /// Encoded version.
        actual: u16,
    },
    /// Exact normalized wire bytes disagree with the protected digest.
    CommittedWireDigestMismatch {
        /// First extent of the record.
        extent: usize,
    },
    /// Bytes outside the exact normalized wire payload are not erased.
    CommittedWirePaddingProgrammed {
        /// First extent of the record.
        extent: usize,
    },
    /// Header, wire, and footer disagree with the protected record digest.
    CommittedRecordDigestMismatch {
        /// First extent of the record.
        extent: usize,
    },
    /// A committed record's portable semantic metadata is invalid.
    CommittedMetadataInvalid {
        /// First extent of the record.
        extent: usize,
    },
    /// More than one committed record uses one authenticated message ID.
    DuplicateCommittedMessageId {
        /// Repeated identifier.
        message_id: MessageId,
    },
    /// More than one committed record uses one stable logical handle.
    DuplicateCommittedHandle {
        /// Repeated stable handle.
        handle: MessageHandle,
    },
    /// Program readback did not establish the intended monotonic bytes.
    ReadbackMismatch {
        /// Stage being programmed.
        stage: LxmfProgramStage,
        /// First extent of the candidate record.
        extent: usize,
    },
    /// Runtime media no longer agrees with the mounted append position.
    MediaChanged {
        /// First extent of the candidate span whose erased preflight failed.
        extent: usize,
    },
}

/// Physical programming or verification stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfProgramStage {
    /// High-entropy record-ownership claim programmed before header fields.
    Claim,
    /// Repeated extent claims and headers.
    Header,
    /// Streamed normalized wire bytes.
    Wire,
    /// Canonical footer excluding its commit marker.
    Footer,
    /// Terminal commit marker.
    Commit,
    /// Complete committed-record verification.
    Verification,
}

/// Failure to mount an exact read-only store view.
#[derive(Debug, Eq, PartialEq)]
pub enum LxmfStoreMountError<E> {
    /// Binding rejected before physical I/O.
    Binding(LxmfStoreBindingError),
    /// Raw NOR backend failure.
    Backend(E),
    /// Stable fail-closed committed-media fault.
    Fault(LxmfStoreFault),
    /// Committed messages exceed the supplied fixed RAM index capacity.
    IndexCapacityExceeded {
        /// Entries required by media.
        required: usize,
        /// Entries supplied by the caller's profile.
        capacity: usize,
    },
    /// The next stable logical handle cannot be incremented.
    HandleExhausted,
}

/// Definitive result returned only after durable verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfCommitOutcome {
    /// This call completed a new durable record.
    Committed(DurableMessageReceipt),
    /// Equivalent authenticated content was already durable.
    AlreadyDurable(DurableMessageReceipt),
}

/// Commit failure reason excluding the returned borrowed candidate.
#[derive(Debug, Eq, PartialEq)]
pub enum LxmfCommitError<E> {
    /// Binding rejected before physical I/O.
    Binding(LxmfStoreBindingError),
    /// Raw NOR backend failure; completion may be ambiguous.
    Backend {
        /// Physical stage active at the failure.
        stage: LxmfProgramStage,
        /// Concrete backend error.
        error: E,
    },
    /// Stable fail-closed media or readback fault.
    Fault(LxmfStoreFault),
    /// The append-only physical tail cannot hold this candidate.
    Full {
        /// Extents required by this candidate.
        required_extents: usize,
        /// Extents remaining at the append tail.
        remaining_extents: usize,
    },
    /// The fixed RAM index has no slot for another committed message.
    IndexFull {
        /// Caller-supplied fixed index capacity.
        capacity: usize,
    },
    /// One message ID names conflicting authenticated material or metadata.
    HashCollision {
        /// Colliding authenticated identifier.
        message_id: MessageId,
    },
    /// A prior ambiguous mutation must be reconciled with the same candidate.
    AmbiguousMutationPending {
        /// Message ID whose exact physical mutation remains pending.
        message_id: MessageId,
    },
    /// No further stable nonzero logical handle can be allocated.
    HandleExhausted,
}

/// Commit failure retaining the exact borrowed candidate for deliberate retry.
#[derive(Eq, PartialEq)]
pub struct LxmfCommitFailure<'a, E> {
    candidate: InboundMessageCandidate<'a>,
    error: LxmfCommitError<E>,
}

impl<E: core::fmt::Debug> core::fmt::Debug for LxmfCommitFailure<'_, E> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LxmfCommitFailure")
            .field("candidate", &self.candidate)
            .field("error", &self.error)
            .finish()
    }
}

impl<'a, E> LxmfCommitFailure<'a, E> {
    fn new(candidate: InboundMessageCandidate<'a>, error: LxmfCommitError<E>) -> Self {
        Self { candidate, error }
    }

    /// Borrow the stable or backend-specific failure reason.
    pub const fn error(&self) -> &LxmfCommitError<E> {
        &self.error
    }

    /// Recover the exact candidate and owned reason, candidate first.
    pub fn into_parts(self) -> (InboundMessageCandidate<'a>, LxmfCommitError<E>) {
        (self.candidate, self.error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IndexEntry {
    receipt: DurableMessageReceipt,
    metadata: InboundMessageMetadata,
    first_extent: u32,
    extent_count: u16,
    wire_len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingMutation {
    replay: ReplayFingerprint,
    metadata: InboundMessageMetadata,
    first_extent: u32,
    extent_count: u16,
    handle: MessageHandle,
}

/// Caller-owned storage for one committed-message index entry.
///
/// The entry remains private so only a mounted store can interpret or mutate
/// it. Construct the product-selected number of slots outside the mounted
/// owner, then pass the complete mutable slice to [`mount`].
#[must_use = "an LXMF index slot must outlive the mounted store that borrows it"]
pub struct LxmfStoreIndexSlot {
    entry: Option<IndexEntry>,
}

impl LxmfStoreIndexSlot {
    /// Construct one empty reusable index slot.
    pub const fn new() -> Self {
        Self { entry: None }
    }
}

impl Default for LxmfStoreIndexSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// Mounted runtime state borrowing a caller-owned committed-message index.
#[must_use = "mounted durable ownership must be retained or deliberately discarded"]
pub struct MountedLxmfStore<'index> {
    binding: LxmfStoreBinding,
    entries: &'index mut [LxmfStoreIndexSlot],
    entry_count: usize,
    append_extent: usize,
    next_handle: u64,
    pending: Option<PendingMutation>,
}

impl MountedLxmfStore<'_> {
    /// Exact physical binding established by mount.
    pub const fn binding(&self) -> LxmfStoreBinding {
        self.binding
    }

    /// Number of committed logical messages in the fixed RAM index.
    pub const fn message_count(&self) -> usize {
        self.entry_count
    }

    /// Whether an exact physical mutation has an ambiguous result awaiting
    /// reconciliation with the same message.
    pub const fn has_pending_mutation(&self) -> bool {
        self.pending.is_some()
    }

    /// Message identity whose exact physical mutation awaits reconciliation.
    pub const fn pending_message_id(&self) -> Option<MessageId> {
        match self.pending {
            Some(pending) => Some(pending.replay.authenticated().message_id()),
            None => None,
        }
    }

    /// Extents committed, retired, or conservatively reserved by this mount.
    ///
    /// A read failure during erased-span preflight retains the pending
    /// reservation even when no program operation was attempted. Retrying the
    /// same candidate reconciles that reservation; unrelated appends fail
    /// closed until it is resolved.
    pub const fn consumed_extents(&self) -> usize {
        match self.pending {
            Some(pending) => {
                let pending_end = pending.first_extent as usize + pending.extent_count as usize;
                if pending_end > self.append_extent {
                    pending_end
                } else {
                    self.append_extent
                }
            }
            None => self.append_extent,
        }
    }

    /// Retrieve a durable receipt by stable logical handle without I/O.
    pub fn receipt(&self, handle: MessageHandle) -> Option<DurableMessageReceipt> {
        self.entries[..self.entry_count]
            .iter()
            .filter_map(|slot| slot.entry.as_ref())
            .find(|entry| entry.receipt.handle() == handle)
            .map(|entry| entry.receipt)
    }

    /// Retrieve immutable first-arrival metadata by stable handle without I/O.
    pub fn metadata(&self, handle: MessageHandle) -> Option<InboundMessageMetadata> {
        self.entries[..self.entry_count]
            .iter()
            .filter_map(|slot| slot.entry.as_ref())
            .find(|entry| entry.receipt.handle() == handle)
            .map(|entry| entry.metadata)
    }

    /// Iterate committed receipts in physical commit order without I/O.
    pub fn receipts(&self) -> impl Iterator<Item = DurableMessageReceipt> + '_ {
        self.entries[..self.entry_count]
            .iter()
            .filter_map(|slot| slot.entry.as_ref())
            .map(|entry| entry.receipt)
    }

    /// Commit one admitted candidate without allocating or copying a complete message.
    pub fn commit<'a, A>(
        &mut self,
        access: &mut A,
        candidate: InboundMessageCandidate<'a>,
    ) -> Result<LxmfCommitOutcome, LxmfCommitFailure<'a, A::Error>>
    where
        A: BoundLxmfStoreAccess,
    {
        let actual = validate_write_access(access)
            .and_then(|binding| validate_same_binding(self.binding, binding))
            .map_err(|error| LxmfCommitFailure::new(candidate, LxmfCommitError::Binding(error)))?;
        debug_assert_eq!(actual, self.binding);

        let exact_wire_digest = digest_candidate_wire(candidate);
        let replay = ReplayFingerprint::new(
            candidate.metadata().authenticated_fingerprint(),
            exact_wire_digest,
        );
        if let Some(entry) = self.entry_for_message(candidate.metadata().message_id()) {
            return match replay.classify_against(entry.receipt.fingerprint()) {
                ReplayRelation::ExactReplay | ReplayRelation::EquivalentReplay => {
                    Ok(LxmfCommitOutcome::AlreadyDurable(entry.receipt))
                }
                ReplayRelation::AuthenticatedMetadataConflict => Err(LxmfCommitFailure::new(
                    candidate,
                    LxmfCommitError::HashCollision {
                        message_id: candidate.metadata().message_id(),
                    },
                )),
                ReplayRelation::DistinctMessage => unreachable!("message lookup matched the ID"),
            };
        }

        let existing_pending = self.pending;
        if let Some(pending) = existing_pending
            && (pending.replay != replay || pending.metadata != candidate.metadata())
        {
            return Err(LxmfCommitFailure::new(
                candidate,
                LxmfCommitError::AmbiguousMutationPending {
                    message_id: pending.replay.authenticated().message_id(),
                },
            ));
        }
        if self.entry_count == self.entries.len() {
            return Err(LxmfCommitFailure::new(
                candidate,
                LxmfCommitError::IndexFull {
                    capacity: self.entries.len(),
                },
            ));
        }

        let wire_len = candidate.segments().total_len();
        let required = required_extents(wire_len).ok_or_else(|| {
            LxmfCommitFailure::new(
                candidate,
                LxmfCommitError::Full {
                    required_extents: usize::MAX,
                    remaining_extents: 0,
                },
            )
        })?;
        let total_extents = self.binding.length() / EXTENT_SIZE;
        let remaining = total_extents.saturating_sub(self.append_extent);
        if required > remaining || required > usize::from(u16::MAX) {
            return Err(LxmfCommitFailure::new(
                candidate,
                LxmfCommitError::Full {
                    required_extents: required,
                    remaining_extents: remaining,
                },
            ));
        }

        let (first_extent, extent_count, handle) = match existing_pending {
            Some(pending) => (
                pending.first_extent as usize,
                pending.extent_count,
                pending.handle,
            ),
            None => {
                let handle = MessageHandle::new(self.next_handle).map_err(|_| {
                    LxmfCommitFailure::new(candidate, LxmfCommitError::HandleExhausted)
                })?;
                (self.append_extent, required as u16, handle)
            }
        };
        if usize::from(extent_count) != required {
            return Err(LxmfCommitFailure::new(
                candidate,
                LxmfCommitError::AmbiguousMutationPending {
                    message_id: replay.authenticated().message_id(),
                },
            ));
        }
        self.pending = Some(PendingMutation {
            replay,
            metadata: candidate.metadata(),
            first_extent: first_extent as u32,
            extent_count,
            handle,
        });

        if existing_pending.is_some() {
            match committed_record_at(access, self.binding, first_extent) {
                Ok(Some(entry)) => {
                    if replay.classify_against(entry.receipt.fingerprint())
                        == ReplayRelation::AuthenticatedMetadataConflict
                    {
                        return Err(LxmfCommitFailure::new(
                            candidate,
                            LxmfCommitError::HashCollision {
                                message_id: replay.authenticated().message_id(),
                            },
                        ));
                    }
                    if entry.receipt.fingerprint() != replay
                        || entry.metadata != candidate.metadata()
                    {
                        return Err(LxmfCommitFailure::new(
                            candidate,
                            LxmfCommitError::Fault(LxmfStoreFault::CommittedMetadataInvalid {
                                extent: first_extent,
                            }),
                        ));
                    }
                    self.install_entry(entry);
                    self.finish_commit(first_extent, extent_count, handle);
                    return Ok(LxmfCommitOutcome::AlreadyDurable(entry.receipt));
                }
                Ok(None) => {}
                Err(InspectError::Backend(error)) => {
                    return Err(LxmfCommitFailure::new(
                        candidate,
                        LxmfCommitError::Backend {
                            stage: LxmfProgramStage::Verification,
                            error,
                        },
                    ));
                }
                Err(InspectError::Fault(fault)) => {
                    return Err(LxmfCommitFailure::new(
                        candidate,
                        LxmfCommitError::Fault(fault),
                    ));
                }
            }
        } else {
            match span_is_erased(access, first_extent, required) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(LxmfCommitFailure::new(
                        candidate,
                        LxmfCommitError::Fault(LxmfStoreFault::MediaChanged {
                            extent: first_extent,
                        }),
                    ));
                }
                Err(error) => {
                    return Err(LxmfCommitFailure::new(
                        candidate,
                        LxmfCommitError::Backend {
                            stage: LxmfProgramStage::Verification,
                            error,
                        },
                    ));
                }
            }
        }

        let first_header = encode_header(
            self.binding,
            handle,
            0,
            extent_count,
            candidate.metadata(),
            exact_wire_digest,
        );
        for extent_index in 0..extent_count {
            let header = if extent_index == 0 {
                first_header
            } else {
                encode_header(
                    self.binding,
                    handle,
                    extent_index,
                    extent_count,
                    candidate.metadata(),
                    exact_wire_digest,
                )
            };
            let extent = first_extent + usize::from(extent_index);
            let mut claim_overlay = [0xff_u8; IO_CHUNK_SIZE];
            claim_overlay[..CLAIM_MARKER.len()].copy_from_slice(&CLAIM_MARKER);
            if let Err(error) = program_claim(
                access,
                extent_offset(extent),
                &claim_overlay,
                (&header[..IO_CHUNK_SIZE])
                    .try_into()
                    .expect("one header page"),
                first_extent,
            ) {
                return Err(LxmfCommitFailure::new(candidate, error));
            }
            for page in 0..(EXTENT_HEADER_SIZE / IO_CHUNK_SIZE) {
                let offset = extent_offset(extent) + page * IO_CHUNK_SIZE;
                let expected = &header[page * IO_CHUNK_SIZE..(page + 1) * IO_CHUNK_SIZE];
                if let Err(error) = program_exact(
                    access,
                    offset,
                    expected,
                    expected,
                    LxmfProgramStage::Header,
                    first_extent,
                ) {
                    return Err(LxmfCommitFailure::new(candidate, error));
                }
            }
        }

        if let Err(error) = program_candidate_wire(access, first_extent, extent_count, candidate) {
            return Err(LxmfCommitFailure::new(candidate, error));
        }
        let record_digest = digest_candidate_record(&first_header, candidate);
        let footer_before_commit = encode_footer(
            handle,
            extent_count,
            wire_len as u32,
            exact_wire_digest,
            record_digest,
            false,
        );
        let final_extent = first_extent + usize::from(extent_count) - 1;
        let footer_offset = extent_offset(final_extent) + EXTENT_SIZE - RECORD_FOOTER_SIZE;
        if let Err(error) =
            program_footer_base(access, footer_offset, &footer_before_commit, first_extent)
        {
            return Err(LxmfCommitFailure::new(candidate, error));
        }
        let footer_committed = encode_footer(
            handle,
            extent_count,
            wire_len as u32,
            exact_wire_digest,
            record_digest,
            true,
        );
        let mut commit_overlay = [0xff_u8; RECORD_FOOTER_SIZE];
        commit_overlay[FOOTER_COMMIT_OFFSET..].copy_from_slice(&COMMIT_MARKER);
        if let Err(error) = program_exact(
            access,
            footer_offset,
            &commit_overlay,
            &footer_committed,
            LxmfProgramStage::Commit,
            first_extent,
        ) {
            return Err(LxmfCommitFailure::new(candidate, error));
        }

        let entry = match committed_record_at(access, self.binding, first_extent) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                return Err(LxmfCommitFailure::new(
                    candidate,
                    LxmfCommitError::Fault(LxmfStoreFault::ReadbackMismatch {
                        stage: LxmfProgramStage::Verification,
                        extent: first_extent,
                    }),
                ));
            }
            Err(InspectError::Backend(error)) => {
                return Err(LxmfCommitFailure::new(
                    candidate,
                    LxmfCommitError::Backend {
                        stage: LxmfProgramStage::Verification,
                        error,
                    },
                ));
            }
            Err(InspectError::Fault(fault)) => {
                return Err(LxmfCommitFailure::new(
                    candidate,
                    LxmfCommitError::Fault(fault),
                ));
            }
        };
        if entry.receipt.fingerprint() != replay || entry.metadata != candidate.metadata() {
            return Err(LxmfCommitFailure::new(
                candidate,
                LxmfCommitError::Fault(LxmfStoreFault::CommittedMetadataInvalid {
                    extent: first_extent,
                }),
            ));
        }
        self.install_entry(entry);
        self.finish_commit(first_extent, extent_count, handle);
        Ok(LxmfCommitOutcome::Committed(entry.receipt))
    }

    fn entry_for_message(&self, id: MessageId) -> Option<IndexEntry> {
        self.entries[..self.entry_count]
            .iter()
            .filter_map(|slot| slot.entry)
            .find(|entry| entry.receipt.message_id() == id)
    }

    fn install_entry(&mut self, entry: IndexEntry) {
        if self.entry_for_message(entry.receipt.message_id()).is_none() {
            self.entries[self.entry_count].entry = Some(entry);
            self.entry_count += 1;
        }
    }

    fn finish_commit(&mut self, first_extent: usize, extent_count: u16, handle: MessageHandle) {
        self.append_extent = self
            .append_extent
            .max(first_extent + usize::from(extent_count));
        self.next_handle = handle.get().checked_add(1).unwrap_or(0);
        self.pending = None;
    }
}

/// Mount a complete exact-range read-only view into caller-owned index slots
/// without programming or erasing.
pub fn mount<'index, A>(
    access: &mut A,
    index: &'index mut [LxmfStoreIndexSlot],
) -> Result<MountedLxmfStore<'index>, LxmfStoreMountError<A::Error>>
where
    A: BoundLxmfStoreReadAccess,
{
    let binding = validate_read_access(access).map_err(LxmfStoreMountError::Binding)?;
    for slot in index.iter_mut() {
        slot.entry = None;
    }
    let total_extents = binding.length() / EXTENT_SIZE;
    let mut mounted = MountedLxmfStore {
        binding,
        entries: index,
        entry_count: 0,
        append_extent: 0,
        next_handle: 1,
        pending: None,
    };
    let mut extent = 0;
    let mut max_handle = 0_u64;
    while extent < total_extents {
        let snapshot =
            read_extent_snapshot(access, extent).map_err(LxmfStoreMountError::Backend)?;
        if snapshot.erased {
            extent += 1;
            continue;
        }
        mounted.append_extent = mounted.append_extent.max(extent + 1);
        let header = match decode_header(&snapshot.header, binding) {
            Ok(header) => header,
            Err(error) => {
                match snapshot.commit_marker() {
                    CommitMarkerState::Committed => {
                        return Err(LxmfStoreMountError::Fault(
                            error.into_fault(extent, binding),
                        ));
                    }
                    CommitMarkerState::Unknown => {
                        return Err(LxmfStoreMountError::Fault(
                            LxmfStoreFault::CommittedFooterCorrupt { extent },
                        ));
                    }
                    CommitMarkerState::Incomplete if recognized_incomplete_extent(&snapshot) => {}
                    CommitMarkerState::Incomplete => {
                        return Err(LxmfStoreMountError::Fault(
                            LxmfStoreFault::UnknownProgrammedExtent { extent },
                        ));
                    }
                }
                extent += 1;
                continue;
            }
        };
        max_handle = max_handle.max(header.handle.get());
        if header.extent_index != 0 {
            return Err(LxmfStoreMountError::Fault(unexpected_continuation_fault(
                &snapshot, extent,
            )));
        }
        let count = usize::from(header.extent_count);
        let end = match extent.checked_add(count) {
            Some(end) if end <= total_extents => end,
            _ => {
                return Err(LxmfStoreMountError::Fault(
                    LxmfStoreFault::UnknownProgrammedExtent { extent },
                ));
            }
        };
        mounted.append_extent = mounted.append_extent.max(end);
        let first_header = snapshot.header;
        let mut headers_complete = true;
        let mut first_programmed_body = (!snapshot.outside_header_erased).then_some(extent);
        let mut final_snapshot = if count == 1 { Some(snapshot) } else { None };
        for index in 1..count {
            let continuation = read_extent_snapshot(access, extent + index)
                .map_err(LxmfStoreMountError::Backend)?;
            max_handle = decode_header(&continuation.header, binding)
                .map(|value| max_handle.max(value.handle.get()))
                .unwrap_or(max_handle);
            let expected = encode_header(
                binding,
                header.handle,
                index as u16,
                header.extent_count,
                header.metadata,
                header.exact_wire_digest,
            );
            if !headers_complete {
                if !continuation.erased {
                    return Err(LxmfStoreMountError::Fault(unexpected_continuation_fault(
                        &continuation,
                        extent + index,
                    )));
                }
            } else if continuation.header == expected {
                if first_programmed_body.is_none() && !continuation.outside_header_erased {
                    first_programmed_body = Some(extent + index);
                }
            } else {
                if continuation.commit_marker() != CommitMarkerState::Incomplete {
                    return Err(LxmfStoreMountError::Fault(unexpected_continuation_fault(
                        &continuation,
                        extent + index,
                    )));
                }
                if first_programmed_body.is_some()
                    || !recognized_incomplete_continuation(&continuation, &expected)
                {
                    return Err(LxmfStoreMountError::Fault(
                        LxmfStoreFault::UnknownProgrammedExtent {
                            extent: extent + index,
                        },
                    ));
                }
                headers_complete = false;
            }
            if index + 1 == count {
                final_snapshot = Some(continuation);
            }
        }
        let final_snapshot = final_snapshot.expect("nonzero extent count has a final extent");
        if !headers_complete {
            extent = end;
            continue;
        }
        match final_snapshot.commit_marker() {
            CommitMarkerState::Incomplete => {
                extent = end;
                continue;
            }
            CommitMarkerState::Unknown => {
                return Err(LxmfStoreMountError::Fault(
                    LxmfStoreFault::CommittedFooterCorrupt { extent },
                ));
            }
            CommitMarkerState::Committed => {}
        }

        let entry = decode_committed_record(
            access,
            binding,
            extent,
            &first_header,
            &final_snapshot.footer,
            header,
        )
        .map_err(|error| match error {
            InspectError::Backend(error) => LxmfStoreMountError::Backend(error),
            InspectError::Fault(fault) => LxmfStoreMountError::Fault(fault),
        })?;
        if mounted
            .entry_for_message(entry.receipt.message_id())
            .is_some()
        {
            return Err(LxmfStoreMountError::Fault(
                LxmfStoreFault::DuplicateCommittedMessageId {
                    message_id: entry.receipt.message_id(),
                },
            ));
        }
        if mounted.entries[..mounted.entry_count]
            .iter()
            .filter_map(|slot| slot.entry.as_ref())
            .any(|existing| existing.receipt.handle() == entry.receipt.handle())
        {
            return Err(LxmfStoreMountError::Fault(
                LxmfStoreFault::DuplicateCommittedHandle {
                    handle: entry.receipt.handle(),
                },
            ));
        }
        if mounted.entry_count == mounted.entries.len() {
            return Err(LxmfStoreMountError::IndexCapacityExceeded {
                required: mounted.entry_count + 1,
                capacity: mounted.entries.len(),
            });
        }
        mounted.install_entry(entry);
        extent = end;
    }
    mounted.next_handle = max_handle
        .checked_add(1)
        .ok_or(LxmfStoreMountError::HandleExhausted)?;
    Ok(mounted)
}

#[derive(Clone, Copy)]
struct ExtentSnapshot {
    header: [u8; EXTENT_HEADER_SIZE],
    footer: [u8; RECORD_FOOTER_SIZE],
    erased: bool,
    outside_claim_erased: bool,
    outside_header_erased: bool,
}

impl ExtentSnapshot {
    fn commit_marker(&self) -> CommitMarkerState {
        classify_commit_marker(&self.footer[FOOTER_COMMIT_OFFSET..])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitMarkerState {
    Incomplete,
    Committed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HeaderFields {
    handle: MessageHandle,
    extent_index: u16,
    extent_count: u16,
    metadata: InboundMessageMetadata,
    exact_wire_digest: ExactWireDigest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeaderDecodeError {
    Invalid,
    UnsupportedVersion(u16),
    BindingMismatch(LxmfStoreBinding),
}

impl HeaderDecodeError {
    fn into_fault(self, extent: usize, expected: LxmfStoreBinding) -> LxmfStoreFault {
        match self {
            Self::Invalid => LxmfStoreFault::CommittedHeaderCorrupt { extent },
            Self::UnsupportedVersion(actual) => {
                LxmfStoreFault::UnsupportedPhysicalVersion { extent, actual }
            }
            Self::BindingMismatch(actual) => {
                LxmfStoreFault::CommittedBindingMismatch { expected, actual }
            }
        }
    }
}

enum InspectError<E> {
    Backend(E),
    Fault(LxmfStoreFault),
}

fn required_extents(wire_len: usize) -> Option<usize> {
    wire_len
        .checked_add(RECORD_FOOTER_SIZE)?
        .checked_add(EXTENT_PAYLOAD_SIZE - 1)
        .map(|adjusted| adjusted / EXTENT_PAYLOAD_SIZE)
}

fn extent_offset(extent: usize) -> usize {
    extent * EXTENT_SIZE
}

fn digest_candidate_wire(candidate: InboundMessageCandidate<'_>) -> ExactWireDigest {
    let segments = candidate.segments();
    let mut digest = Sha256::new();
    digest.update(segments.first());
    digest.update(segments.second());
    ExactWireDigest::new(digest.finalize().into())
}

fn digest_candidate_record(
    first_header: &[u8; EXTENT_HEADER_SIZE],
    candidate: InboundMessageCandidate<'_>,
) -> [u8; 32] {
    let segments = candidate.segments();
    let mut digest = Sha256::new();
    digest.update(RECORD_DIGEST_DOMAIN);
    digest.update(first_header);
    digest.update(segments.first());
    digest.update(segments.second());
    digest.finalize().into()
}

fn encode_header(
    binding: LxmfStoreBinding,
    handle: MessageHandle,
    extent_index: u16,
    extent_count: u16,
    metadata: InboundMessageMetadata,
    exact_wire_digest: ExactWireDigest,
) -> [u8; EXTENT_HEADER_SIZE] {
    let mut header = [0_u8; EXTENT_HEADER_SIZE];
    header[HEADER_CLAIM_OFFSET..HEADER_MAGIC_OFFSET].copy_from_slice(&CLAIM_MARKER);
    header[HEADER_MAGIC_OFFSET..HEADER_VERSION_OFFSET].copy_from_slice(HEADER_MAGIC);
    write_u16(&mut header, HEADER_VERSION_OFFSET, PHYSICAL_FORMAT_VERSION);
    write_u16(&mut header, HEADER_SIZE_OFFSET, EXTENT_HEADER_SIZE as u16);
    write_u16(&mut header, HEADER_EXTENT_INDEX_OFFSET, extent_index);
    write_u16(&mut header, HEADER_EXTENT_COUNT_OFFSET, extent_count);
    write_u64(&mut header, HEADER_HANDLE_OFFSET, handle.get());
    header[HEADER_DEVICE_OFFSET..HEADER_ABSOLUTE_OFFSET]
        .copy_from_slice(binding.device().as_bytes());
    write_u64(
        &mut header,
        HEADER_ABSOLUTE_OFFSET,
        binding.absolute_offset() as u64,
    );
    write_u64(
        &mut header,
        HEADER_PARTITION_LENGTH_OFFSET,
        binding.length() as u64,
    );
    header[HEADER_MESSAGE_ID_OFFSET..HEADER_AUTHENTICATED_MATERIAL_OFFSET]
        .copy_from_slice(metadata.message_id().as_bytes());
    header[HEADER_AUTHENTICATED_MATERIAL_OFFSET..HEADER_DESTINATION_OFFSET]
        .copy_from_slice(metadata.authenticated_material().as_bytes());
    header[HEADER_DESTINATION_OFFSET..HEADER_SOURCE_OFFSET]
        .copy_from_slice(metadata.destination().as_bytes());
    header[HEADER_SOURCE_OFFSET..HEADER_TIMESTAMP_OFFSET]
        .copy_from_slice(metadata.source().as_bytes());
    write_u64(
        &mut header,
        HEADER_TIMESTAMP_OFFSET,
        metadata.timestamp_bits(),
    );
    header[HEADER_CARRIER_OFFSET] = encode_carrier(metadata.carrier());
    let (stamp_kind, stamp_argument, stamp_observed) = encode_stamp(metadata.stamp_admission());
    header[HEADER_STAMP_KIND_OFFSET] = stamp_kind;
    write_u16(&mut header, HEADER_STAMP_ARGUMENT_OFFSET, stamp_argument);
    write_u16(&mut header, HEADER_STAMP_OBSERVED_OFFSET, stamp_observed);
    let lengths = metadata.lengths();
    for (index, value) in [
        lengths.normalized_wire(),
        lengths.carrier_payload(),
        lengths.title(),
        lengths.content(),
        lengths.fields_encoded(),
    ]
    .into_iter()
    .enumerate()
    {
        write_u32(&mut header, HEADER_LENGTHS_OFFSET + index * 4, value);
    }
    header[HEADER_EXACT_WIRE_DIGEST_OFFSET..HEADER_PADDING_OFFSET]
        .copy_from_slice(exact_wire_digest.as_bytes());
    let mut digest = Sha256::new();
    digest.update(HEADER_DIGEST_DOMAIN);
    digest.update(&header[..HEADER_DIGEST_OFFSET]);
    header[HEADER_DIGEST_OFFSET..].copy_from_slice(&digest.finalize());
    header
}

fn decode_header(
    header: &[u8; EXTENT_HEADER_SIZE],
    expected_binding: LxmfStoreBinding,
) -> Result<HeaderFields, HeaderDecodeError> {
    if header[HEADER_CLAIM_OFFSET..HEADER_MAGIC_OFFSET] != CLAIM_MARKER
        || &header[HEADER_MAGIC_OFFSET..HEADER_VERSION_OFFSET] != HEADER_MAGIC
    {
        return Err(HeaderDecodeError::Invalid);
    }
    let version = read_u16(header, HEADER_VERSION_OFFSET);
    if version != PHYSICAL_FORMAT_VERSION {
        return Err(HeaderDecodeError::UnsupportedVersion(version));
    }
    if read_u16(header, HEADER_SIZE_OFFSET) != EXTENT_HEADER_SIZE as u16 {
        return Err(HeaderDecodeError::Invalid);
    }
    let mut device = [0_u8; 16];
    device.copy_from_slice(&header[HEADER_DEVICE_OFFSET..HEADER_ABSOLUTE_OFFSET]);
    let absolute_offset = usize::try_from(read_u64(header, HEADER_ABSOLUTE_OFFSET))
        .map_err(|_| HeaderDecodeError::Invalid)?;
    let length = usize::try_from(read_u64(header, HEADER_PARTITION_LENGTH_OFFSET))
        .map_err(|_| HeaderDecodeError::Invalid)?;
    let actual_binding = LxmfStoreBinding::new(
        LxmfStoreDeviceId::new(device),
        absolute_offset,
        length,
        version,
    );
    if actual_binding != expected_binding {
        return Err(HeaderDecodeError::BindingMismatch(actual_binding));
    }
    let extent_index = read_u16(header, HEADER_EXTENT_INDEX_OFFSET);
    let extent_count = read_u16(header, HEADER_EXTENT_COUNT_OFFSET);
    if extent_count == 0 || extent_index >= extent_count {
        return Err(HeaderDecodeError::Invalid);
    }
    let handle = MessageHandle::new(read_u64(header, HEADER_HANDLE_OFFSET))
        .map_err(|_| HeaderDecodeError::Invalid)?;
    let mut message_id = [0_u8; 32];
    message_id
        .copy_from_slice(&header[HEADER_MESSAGE_ID_OFFSET..HEADER_AUTHENTICATED_MATERIAL_OFFSET]);
    let mut authenticated_material = [0_u8; 32];
    authenticated_material
        .copy_from_slice(&header[HEADER_AUTHENTICATED_MATERIAL_OFFSET..HEADER_DESTINATION_OFFSET]);
    let mut destination = [0_u8; 16];
    destination.copy_from_slice(&header[HEADER_DESTINATION_OFFSET..HEADER_SOURCE_OFFSET]);
    let mut source = [0_u8; 16];
    source.copy_from_slice(&header[HEADER_SOURCE_OFFSET..HEADER_TIMESTAMP_OFFSET]);
    let carrier =
        decode_carrier(header[HEADER_CARRIER_OFFSET]).ok_or(HeaderDecodeError::Invalid)?;
    let stamp = decode_stamp(
        header[HEADER_STAMP_KIND_OFFSET],
        read_u16(header, HEADER_STAMP_ARGUMENT_OFFSET),
        read_u16(header, HEADER_STAMP_OBSERVED_OFFSET),
    )
    .ok_or(HeaderDecodeError::Invalid)?;
    let lengths = InboundMessageLengths::new(
        read_u32(header, HEADER_LENGTHS_OFFSET) as usize,
        read_u32(header, HEADER_LENGTHS_OFFSET + 4) as usize,
        read_u32(header, HEADER_LENGTHS_OFFSET + 8) as usize,
        read_u32(header, HEADER_LENGTHS_OFFSET + 12) as usize,
        read_u32(header, HEADER_LENGTHS_OFFSET + 16) as usize,
    )
    .map_err(|_| HeaderDecodeError::Invalid)?;
    let metadata = InboundMessageMetadata::new(
        MessageId::new(message_id),
        AuthenticatedMaterialFingerprint::new(authenticated_material),
        DestinationHash::new(destination),
        SourceHash::new(source),
        read_u64(header, HEADER_TIMESTAMP_OFFSET),
        carrier,
        stamp,
        lengths,
    )
    .map_err(|_| HeaderDecodeError::Invalid)?;
    let mut exact_digest = [0_u8; 32];
    exact_digest.copy_from_slice(&header[HEADER_EXACT_WIRE_DIGEST_OFFSET..HEADER_PADDING_OFFSET]);
    if required_extents(metadata.lengths().normalized_wire() as usize)
        != Some(usize::from(extent_count))
    {
        return Err(HeaderDecodeError::Invalid);
    }
    let fields = HeaderFields {
        handle,
        extent_index,
        extent_count,
        metadata,
        exact_wire_digest: ExactWireDigest::new(exact_digest),
    };
    if *header
        != encode_header(
            expected_binding,
            handle,
            extent_index,
            extent_count,
            metadata,
            fields.exact_wire_digest,
        )
    {
        return Err(HeaderDecodeError::Invalid);
    }
    Ok(fields)
}

fn encode_footer(
    handle: MessageHandle,
    extent_count: u16,
    wire_len: u32,
    exact_wire_digest: ExactWireDigest,
    record_digest: [u8; 32],
    committed: bool,
) -> [u8; RECORD_FOOTER_SIZE] {
    let mut footer = [0_u8; RECORD_FOOTER_SIZE];
    footer[FOOTER_MAGIC_OFFSET..FOOTER_VERSION_OFFSET].copy_from_slice(FOOTER_MAGIC);
    write_u16(&mut footer, FOOTER_VERSION_OFFSET, PHYSICAL_FORMAT_VERSION);
    write_u64(&mut footer, FOOTER_HANDLE_OFFSET, handle.get());
    write_u16(&mut footer, FOOTER_EXTENT_COUNT_OFFSET, extent_count);
    write_u32(&mut footer, FOOTER_WIRE_LENGTH_OFFSET, wire_len);
    footer[FOOTER_WIRE_DIGEST_OFFSET..FOOTER_RECORD_DIGEST_OFFSET]
        .copy_from_slice(exact_wire_digest.as_bytes());
    footer[FOOTER_RECORD_DIGEST_OFFSET..FOOTER_PADDING_OFFSET].copy_from_slice(&record_digest);
    if committed {
        footer[FOOTER_COMMIT_OFFSET..].copy_from_slice(&COMMIT_MARKER);
    } else {
        footer[FOOTER_COMMIT_OFFSET..].fill(0xff);
    }
    footer
}

fn encode_carrier(carrier: CarrierProvenance) -> u8 {
    match carrier {
        CarrierProvenance::Complete => 0,
        CarrierProvenance::Opportunistic => 1,
        CarrierProvenance::LinkDataContextNone => 2,
        CarrierProvenance::ResourceComplete => 3,
    }
}

fn decode_carrier(value: u8) -> Option<CarrierProvenance> {
    match value {
        0 => Some(CarrierProvenance::Complete),
        1 => Some(CarrierProvenance::Opportunistic),
        2 => Some(CarrierProvenance::LinkDataContextNone),
        3 => Some(CarrierProvenance::ResourceComplete),
        _ => None,
    }
}

fn encode_stamp(stamp: StampAdmissionProvenance) -> (u8, u16, u16) {
    match stamp {
        StampAdmissionProvenance::NotRequired {
            stamp_present: false,
        } => (0, 0, 0),
        StampAdmissionProvenance::NotRequired {
            stamp_present: true,
        } => (1, 0, 0),
        StampAdmissionProvenance::TrustedPriorTicket => (2, 0, 0),
        StampAdmissionProvenance::ProofOfWork {
            target_cost,
            observed_value,
        } => (3, target_cost.get(), observed_value),
    }
}

fn decode_stamp(kind: u8, argument: u16, observed: u16) -> Option<StampAdmissionProvenance> {
    match (kind, argument, observed) {
        (0, 0, 0) => Some(StampAdmissionProvenance::NotRequired {
            stamp_present: false,
        }),
        (1, 0, 0) => Some(StampAdmissionProvenance::NotRequired {
            stamp_present: true,
        }),
        (2, 0, 0) => Some(StampAdmissionProvenance::TrustedPriorTicket),
        (3, argument, observed) => Some(StampAdmissionProvenance::ProofOfWork {
            target_cost: RequiredStampCost::new(argument).ok()?,
            observed_value: observed,
        }),
        _ => None,
    }
}

fn classify_commit_marker(bytes: &[u8]) -> CommitMarkerState {
    if bytes == COMMIT_MARKER {
        CommitMarkerState::Committed
    } else if monotonic_partial(bytes, &COMMIT_MARKER) {
        CommitMarkerState::Incomplete
    } else {
        CommitMarkerState::Unknown
    }
}

fn recognized_incomplete_extent(snapshot: &ExtentSnapshot) -> bool {
    let claim = &snapshot.header[HEADER_CLAIM_OFFSET..HEADER_MAGIC_OFFSET];
    if claim == CLAIM_MARKER {
        return true;
    }
    monotonic_partial(claim, &CLAIM_MARKER) && snapshot.outside_claim_erased
}

fn recognized_incomplete_continuation(
    snapshot: &ExtentSnapshot,
    expected_header: &[u8; EXTENT_HEADER_SIZE],
) -> bool {
    recognized_incomplete_extent(snapshot)
        && snapshot.outside_header_erased
        && monotonic_partial(&snapshot.header, expected_header)
}

fn unexpected_continuation_fault(snapshot: &ExtentSnapshot, extent: usize) -> LxmfStoreFault {
    if snapshot.commit_marker() == CommitMarkerState::Committed {
        LxmfStoreFault::UnknownCommittedExtent { extent }
    } else {
        LxmfStoreFault::UnknownProgrammedExtent { extent }
    }
}

fn monotonic_partial(actual: &[u8], intended: &[u8]) -> bool {
    actual
        .iter()
        .zip(intended)
        .all(|(actual, intended)| actual & intended == *intended)
}

fn read_extent_snapshot<A>(access: &mut A, extent: usize) -> Result<ExtentSnapshot, A::Error>
where
    A: BoundLxmfStoreReadAccess,
{
    let mut snapshot = ExtentSnapshot {
        header: [0_u8; EXTENT_HEADER_SIZE],
        footer: [0_u8; RECORD_FOOTER_SIZE],
        erased: true,
        outside_claim_erased: true,
        outside_header_erased: true,
    };
    let base = extent_offset(extent);
    let mut chunk = [0_u8; INSPECTION_CHUNK_SIZE];
    for page in 0..(EXTENT_SIZE / INSPECTION_CHUNK_SIZE) {
        let within = page * INSPECTION_CHUNK_SIZE;
        access.read((base + within) as u32, &mut chunk)?;
        snapshot.erased &= chunk.iter().all(|byte| *byte == 0xff);
        snapshot.outside_claim_erased &= if within == 0 {
            chunk[CLAIM_MARKER.len()..].iter().all(|byte| *byte == 0xff)
        } else {
            chunk.iter().all(|byte| *byte == 0xff)
        };
        snapshot.outside_header_erased &=
            within < EXTENT_HEADER_SIZE || chunk.iter().all(|byte| *byte == 0xff);
        if within < EXTENT_HEADER_SIZE {
            snapshot.header[within..within + INSPECTION_CHUNK_SIZE].copy_from_slice(&chunk);
        }
        if within >= EXTENT_SIZE - RECORD_FOOTER_SIZE {
            let footer_offset = within - (EXTENT_SIZE - RECORD_FOOTER_SIZE);
            snapshot.footer[footer_offset..footer_offset + INSPECTION_CHUNK_SIZE]
                .copy_from_slice(&chunk);
        }
    }
    Ok(snapshot)
}

fn span_is_erased<A>(
    access: &mut A,
    first_extent: usize,
    extent_count: usize,
) -> Result<bool, A::Error>
where
    A: BoundLxmfStoreReadAccess,
{
    for extent in first_extent..first_extent + extent_count {
        if !read_extent_snapshot(access, extent)?.erased {
            return Ok(false);
        }
    }
    Ok(true)
}

fn committed_record_at<A>(
    access: &mut A,
    binding: LxmfStoreBinding,
    first_extent: usize,
) -> Result<Option<IndexEntry>, InspectError<A::Error>>
where
    A: BoundLxmfStoreReadAccess,
{
    let first = read_extent_snapshot(access, first_extent).map_err(InspectError::Backend)?;
    if first.erased {
        return Ok(None);
    }
    let header = match decode_header(&first.header, binding) {
        Ok(header) if header.extent_index == 0 => header,
        Ok(_) | Err(_) => {
            return match first.commit_marker() {
                CommitMarkerState::Committed => Err(InspectError::Fault(
                    LxmfStoreFault::UnknownCommittedExtent {
                        extent: first_extent,
                    },
                )),
                CommitMarkerState::Unknown => Err(InspectError::Fault(
                    LxmfStoreFault::CommittedFooterCorrupt {
                        extent: first_extent,
                    },
                )),
                CommitMarkerState::Incomplete if recognized_incomplete_extent(&first) => Ok(None),
                CommitMarkerState::Incomplete => Err(InspectError::Fault(
                    LxmfStoreFault::UnknownProgrammedExtent {
                        extent: first_extent,
                    },
                )),
            };
        }
    };
    let count = usize::from(header.extent_count);
    if first_extent
        .checked_add(count)
        .is_none_or(|end| end > binding.length() / EXTENT_SIZE)
    {
        return Err(InspectError::Fault(
            LxmfStoreFault::UnknownProgrammedExtent {
                extent: first_extent,
            },
        ));
    }
    let mut headers_complete = true;
    let mut programmed_body_seen = !first.outside_header_erased;
    let mut final_snapshot = first;
    for index in 1..count {
        let continuation =
            read_extent_snapshot(access, first_extent + index).map_err(InspectError::Backend)?;
        let expected = encode_header(
            binding,
            header.handle,
            index as u16,
            header.extent_count,
            header.metadata,
            header.exact_wire_digest,
        );
        if !headers_complete {
            if !continuation.erased {
                return Err(InspectError::Fault(unexpected_continuation_fault(
                    &continuation,
                    first_extent + index,
                )));
            }
        } else if continuation.header == expected {
            programmed_body_seen |= !continuation.outside_header_erased;
        } else {
            if continuation.commit_marker() != CommitMarkerState::Incomplete {
                return Err(InspectError::Fault(unexpected_continuation_fault(
                    &continuation,
                    first_extent + index,
                )));
            }
            if programmed_body_seen || !recognized_incomplete_continuation(&continuation, &expected)
            {
                return Err(InspectError::Fault(
                    LxmfStoreFault::UnknownProgrammedExtent {
                        extent: first_extent + index,
                    },
                ));
            }
            headers_complete = false;
        }
        final_snapshot = continuation;
    }
    if !headers_complete {
        return Ok(None);
    }
    match final_snapshot.commit_marker() {
        CommitMarkerState::Incomplete => Ok(None),
        CommitMarkerState::Unknown => Err(InspectError::Fault(
            LxmfStoreFault::CommittedFooterCorrupt {
                extent: first_extent,
            },
        )),
        CommitMarkerState::Committed => decode_committed_record(
            access,
            binding,
            first_extent,
            &first.header,
            &final_snapshot.footer,
            header,
        )
        .map(Some),
    }
}

fn decode_committed_record<A>(
    access: &mut A,
    _binding: LxmfStoreBinding,
    first_extent: usize,
    first_header: &[u8; EXTENT_HEADER_SIZE],
    footer: &[u8; RECORD_FOOTER_SIZE],
    header: HeaderFields,
) -> Result<IndexEntry, InspectError<A::Error>>
where
    A: BoundLxmfStoreReadAccess,
{
    if &footer[FOOTER_MAGIC_OFFSET..FOOTER_VERSION_OFFSET] != FOOTER_MAGIC
        || read_u16(footer, FOOTER_VERSION_OFFSET) != PHYSICAL_FORMAT_VERSION
        || read_u64(footer, FOOTER_HANDLE_OFFSET) != header.handle.get()
        || read_u16(footer, FOOTER_EXTENT_COUNT_OFFSET) != header.extent_count
        || read_u32(footer, FOOTER_WIRE_LENGTH_OFFSET)
            != header.metadata.lengths().normalized_wire()
        || footer[FOOTER_WIRE_DIGEST_OFFSET..FOOTER_RECORD_DIGEST_OFFSET]
            != *header.exact_wire_digest.as_bytes()
        || footer[10..12].iter().any(|byte| *byte != 0)
        || footer[22..24].iter().any(|byte| *byte != 0)
        || footer[FOOTER_PADDING_OFFSET..FOOTER_COMMIT_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        || footer[FOOTER_COMMIT_OFFSET..] != COMMIT_MARKER
    {
        return Err(InspectError::Fault(
            LxmfStoreFault::CommittedFooterCorrupt {
                extent: first_extent,
            },
        ));
    }
    let (wire_digest, record_digest, padding_erased) = digest_flash_wire(
        access,
        first_extent,
        header.extent_count,
        header.metadata.lengths().normalized_wire() as usize,
        first_header,
    )
    .map_err(InspectError::Backend)?;
    if wire_digest != header.exact_wire_digest {
        return Err(InspectError::Fault(
            LxmfStoreFault::CommittedWireDigestMismatch {
                extent: first_extent,
            },
        ));
    }
    if !padding_erased {
        return Err(InspectError::Fault(
            LxmfStoreFault::CommittedWirePaddingProgrammed {
                extent: first_extent,
            },
        ));
    }
    if footer[FOOTER_RECORD_DIGEST_OFFSET..FOOTER_PADDING_OFFSET] != record_digest {
        return Err(InspectError::Fault(
            LxmfStoreFault::CommittedRecordDigestMismatch {
                extent: first_extent,
            },
        ));
    }
    let replay = ReplayFingerprint::new(header.metadata.authenticated_fingerprint(), wire_digest);
    Ok(IndexEntry {
        receipt: DurableMessageReceipt::new(header.handle, replay),
        metadata: header.metadata,
        first_extent: first_extent as u32,
        extent_count: header.extent_count,
        wire_len: header.metadata.lengths().normalized_wire(),
    })
}

fn digest_flash_wire<A>(
    access: &mut A,
    first_extent: usize,
    extent_count: u16,
    wire_len: usize,
    first_header: &[u8; EXTENT_HEADER_SIZE],
) -> Result<(ExactWireDigest, [u8; 32], bool), A::Error>
where
    A: BoundLxmfStoreReadAccess,
{
    let mut wire_digest = Sha256::new();
    let mut record_digest = Sha256::new();
    record_digest.update(RECORD_DIGEST_DOMAIN);
    record_digest.update(first_header);
    let mut remaining = wire_len;
    let mut padding_erased = true;
    let mut chunk = [0_u8; IO_CHUNK_SIZE];
    for index in 0..usize::from(extent_count) {
        let capacity = if index + 1 == usize::from(extent_count) {
            FINAL_EXTENT_PAYLOAD_SIZE
        } else {
            EXTENT_PAYLOAD_SIZE
        };
        let mut within = 0;
        while within < capacity {
            let take = remaining.min(IO_CHUNK_SIZE);
            let offset = extent_offset(first_extent + index) + EXTENT_HEADER_SIZE + within;
            access.read(offset as u32, &mut chunk)?;
            wire_digest.update(&chunk[..take]);
            record_digest.update(&chunk[..take]);
            padding_erased &= chunk[take..].iter().all(|byte| *byte == 0xff);
            remaining -= take;
            within += IO_CHUNK_SIZE;
        }
    }
    debug_assert_eq!(remaining, 0);
    Ok((
        ExactWireDigest::new(wire_digest.finalize().into()),
        record_digest.finalize().into(),
        padding_erased,
    ))
}

struct CandidateCursor<'a> {
    first: &'a [u8],
    second: &'a [u8],
    position: usize,
}

impl<'a> CandidateCursor<'a> {
    fn new(candidate: InboundMessageCandidate<'a>) -> Self {
        let segments = candidate.segments();
        Self {
            first: segments.first(),
            second: segments.second(),
            position: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.first
            .len()
            .saturating_add(self.second.len())
            .saturating_sub(self.position)
    }

    fn fill(&mut self, output: &mut [u8]) -> usize {
        let mut written = 0;
        while written < output.len() && self.remaining() > 0 {
            let (segment, offset) = if self.position < self.first.len() {
                (self.first, self.position)
            } else {
                (self.second, self.position - self.first.len())
            };
            let take = (output.len() - written).min(segment.len() - offset);
            output[written..written + take].copy_from_slice(&segment[offset..offset + take]);
            written += take;
            self.position += take;
        }
        written
    }
}

fn program_candidate_wire<A>(
    access: &mut A,
    first_extent: usize,
    extent_count: u16,
    candidate: InboundMessageCandidate<'_>,
) -> Result<(), LxmfCommitError<A::Error>>
where
    A: BoundLxmfStoreAccess,
{
    let mut cursor = CandidateCursor::new(candidate);
    for index in 0..usize::from(extent_count) {
        let capacity = if index + 1 == usize::from(extent_count) {
            FINAL_EXTENT_PAYLOAD_SIZE
        } else {
            EXTENT_PAYLOAD_SIZE
        };
        let mut within = 0;
        while within < capacity && cursor.remaining() > 0 {
            let mut page = [0xff_u8; IO_CHUNK_SIZE];
            cursor.fill(&mut page);
            let offset = extent_offset(first_extent + index) + EXTENT_HEADER_SIZE + within;
            program_exact(
                access,
                offset,
                &page,
                &page,
                LxmfProgramStage::Wire,
                first_extent,
            )?;
            within += IO_CHUNK_SIZE;
        }
    }
    debug_assert_eq!(cursor.remaining(), 0);
    Ok(())
}

fn program_exact<A>(
    access: &mut A,
    offset: usize,
    programmed: &[u8],
    expected_readback: &[u8],
    stage: LxmfProgramStage,
    first_extent: usize,
) -> Result<(), LxmfCommitError<A::Error>>
where
    A: BoundLxmfStoreAccess,
{
    debug_assert_eq!(programmed.len(), IO_CHUNK_SIZE);
    debug_assert_eq!(expected_readback.len(), IO_CHUNK_SIZE);
    let write_result = access.write(offset as u32, programmed);
    let mut readback = [0_u8; IO_CHUNK_SIZE];
    let read_result = access.read(offset as u32, &mut readback);
    match (write_result, read_result) {
        (_, Ok(())) if readback == expected_readback => Ok(()),
        (Err(error), _) => Err(LxmfCommitError::Backend { stage, error }),
        (Ok(()), Err(error)) => Err(LxmfCommitError::Backend { stage, error }),
        (Ok(()), Ok(())) => Err(LxmfCommitError::Fault(LxmfStoreFault::ReadbackMismatch {
            stage,
            extent: first_extent,
        })),
    }
}

fn program_claim<A>(
    access: &mut A,
    offset: usize,
    claim_overlay: &[u8; IO_CHUNK_SIZE],
    intended_header_page: &[u8; IO_CHUNK_SIZE],
    first_extent: usize,
) -> Result<(), LxmfCommitError<A::Error>>
where
    A: BoundLxmfStoreAccess,
{
    let write_result = access.write(offset as u32, claim_overlay);
    let mut readback = [0_u8; IO_CHUNK_SIZE];
    let read_result = access.read(offset as u32, &mut readback);
    let compatible = readback[..CLAIM_MARKER.len()] == CLAIM_MARKER
        && monotonic_partial(
            &readback[CLAIM_MARKER.len()..],
            &intended_header_page[CLAIM_MARKER.len()..],
        );
    match (write_result, read_result) {
        (_, Ok(())) if compatible => Ok(()),
        (Err(error), _) => Err(LxmfCommitError::Backend {
            stage: LxmfProgramStage::Claim,
            error,
        }),
        (Ok(()), Err(error)) => Err(LxmfCommitError::Backend {
            stage: LxmfProgramStage::Claim,
            error,
        }),
        (Ok(()), Ok(())) => Err(LxmfCommitError::Fault(LxmfStoreFault::ReadbackMismatch {
            stage: LxmfProgramStage::Claim,
            extent: first_extent,
        })),
    }
}

fn program_footer_base<A>(
    access: &mut A,
    offset: usize,
    footer_before_commit: &[u8; RECORD_FOOTER_SIZE],
    first_extent: usize,
) -> Result<(), LxmfCommitError<A::Error>>
where
    A: BoundLxmfStoreAccess,
{
    let write_result = access.write(offset as u32, footer_before_commit);
    let mut readback = [0_u8; RECORD_FOOTER_SIZE];
    let read_result = access.read(offset as u32, &mut readback);
    let compatible = readback[..FOOTER_COMMIT_OFFSET]
        == footer_before_commit[..FOOTER_COMMIT_OFFSET]
        && monotonic_partial(&readback[FOOTER_COMMIT_OFFSET..], &COMMIT_MARKER);
    match (write_result, read_result) {
        (_, Ok(())) if compatible => Ok(()),
        (Err(error), _) => Err(LxmfCommitError::Backend {
            stage: LxmfProgramStage::Footer,
            error,
        }),
        (Ok(()), Err(error)) => Err(LxmfCommitError::Backend {
            stage: LxmfProgramStage::Footer,
            error,
        }),
        (Ok(()), Ok(())) => Err(LxmfCommitError::Fault(LxmfStoreFault::ReadbackMismatch {
            stage: LxmfProgramStage::Footer,
            extent: first_extent,
        })),
    }
}

fn validate_read_access<A>(access: &A) -> Result<LxmfStoreBinding, LxmfStoreBindingError>
where
    A: BoundLxmfStoreReadAccess,
{
    let binding = access.lxmf_store_binding();
    validate_binding_shape(binding, access.capacity())?;
    if A::READ_SIZE == 0
        || !IO_CHUNK_SIZE.is_multiple_of(A::READ_SIZE)
        || !binding.absolute_offset().is_multiple_of(A::READ_SIZE)
    {
        return Err(LxmfStoreBindingError::ReadAlignmentMismatch {
            read_size: A::READ_SIZE,
        });
    }
    Ok(binding)
}

fn validate_write_access<A>(access: &A) -> Result<LxmfStoreBinding, LxmfStoreBindingError>
where
    A: BoundLxmfStoreAccess,
{
    let binding = validate_read_access(access)?;
    if A::WRITE_SIZE == 0
        || !IO_CHUNK_SIZE.is_multiple_of(A::WRITE_SIZE)
        || !binding.absolute_offset().is_multiple_of(A::WRITE_SIZE)
    {
        return Err(LxmfStoreBindingError::WriteAlignmentMismatch {
            write_size: A::WRITE_SIZE,
        });
    }
    if A::ERASE_SIZE == 0
        || !EXTENT_SIZE.is_multiple_of(A::ERASE_SIZE)
        || !binding.absolute_offset().is_multiple_of(A::ERASE_SIZE)
    {
        return Err(LxmfStoreBindingError::EraseAlignmentMismatch {
            erase_size: A::ERASE_SIZE,
        });
    }
    Ok(binding)
}

fn validate_binding_shape(
    binding: LxmfStoreBinding,
    capacity: usize,
) -> Result<(), LxmfStoreBindingError> {
    if binding.format_version() != PHYSICAL_FORMAT_VERSION {
        return Err(LxmfStoreBindingError::FormatVersionMismatch {
            expected: PHYSICAL_FORMAT_VERSION,
            actual: binding.format_version(),
        });
    }
    if binding.length() == 0 || !binding.length().is_multiple_of(EXTENT_SIZE) {
        return Err(LxmfStoreBindingError::LengthNotExtentMultiple {
            actual: binding.length(),
        });
    }
    if binding
        .absolute_offset()
        .checked_add(binding.length())
        .is_none()
        || binding.length() > u32::MAX as usize
    {
        return Err(LxmfStoreBindingError::RangeOverflow {
            absolute_offset: binding.absolute_offset(),
            length: binding.length(),
        });
    }
    if capacity != binding.length() {
        return Err(LxmfStoreBindingError::CapacityMismatch {
            expected: binding.length(),
            actual: capacity,
        });
    }
    Ok(())
}

fn validate_same_binding(
    expected: LxmfStoreBinding,
    actual: LxmfStoreBinding,
) -> Result<LxmfStoreBinding, LxmfStoreBindingError> {
    if expected.device() != actual.device() {
        return Err(LxmfStoreBindingError::DeviceMismatch {
            expected: expected.device(),
            actual: actual.device(),
        });
    }
    if expected.absolute_offset() != actual.absolute_offset()
        || expected.length() != actual.length()
    {
        return Err(LxmfStoreBindingError::RangeMismatch {
            expected_offset: expected.absolute_offset(),
            expected_length: expected.length(),
            actual_offset: actual.absolute_offset(),
            actual_length: actual.length(),
        });
    }
    if expected.format_version() != actual.format_version() {
        return Err(LxmfStoreBindingError::FormatVersionMismatch {
            expected: expected.format_version(),
            actual: actual.format_version(),
        });
    }
    Ok(actual)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0_u8; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
