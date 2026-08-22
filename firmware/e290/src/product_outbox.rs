//! Product-owned durable outbound LXMF intent above PRNS.
//!
//! Each immutable intent occupies one erase sector. The exact signed LXMF wire
//! is committed before an API can report acceptance. PRNS later receives the
//! ordinary opportunistic carrier and remains the sole owner of routing,
//! packet receipts, proofs, and retries inside one command. A reboot may retry
//! an unsettled record byte-for-byte; LXMF message-ID deduplication makes that
//! application retry safe without adding an admission state to PRNS.

use embedded_storage::nor_flash::{MultiwriteNorFlash, NorFlash, ReadNorFlash};
use sha2::{Digest, Sha256};

/// Erase-sector bytes owned by one immutable outbound intent.
pub const OUTBOX_RECORD_SIZE: usize = 4_096;
/// Maximum exact complete LXMF wire retained by one record.
pub const OUTBOX_WIRE_CAPACITY: usize = 512;
/// Number of records in the E290's initial logical product-store quota.
pub const OUTBOX_RECORD_CAPACITY: usize = 64;
/// Exact initial logical outbox quota length.
pub const OUTBOX_STORE_SIZE: usize = OUTBOX_RECORD_SIZE * OUTBOX_RECORD_CAPACITY;
/// Current physical format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;

const CLAIM_OFFSET: usize = 0;
const CLAIM_SIZE: usize = 32;
const HEADER_OFFSET: usize = 32;
const HEADER_SIZE: usize = 224;
const WIRE_OFFSET: usize = HEADER_OFFSET + HEADER_SIZE;
const DIGEST_OFFSET: usize = WIRE_OFFSET + OUTBOX_WIRE_CAPACITY;
const DIGEST_SIZE: usize = 32;
const COMMIT_OFFSET: usize = DIGEST_OFFSET + DIGEST_SIZE;
const COMMIT_SIZE: usize = 32;
const DELIVERED_OFFSET: usize = COMMIT_OFFSET + COMMIT_SIZE;
const DELIVERED_SIZE: usize = 32;
const RETAINED_PREFIX_SIZE: usize = DELIVERED_OFFSET + DELIVERED_SIZE;

const MAGIC_OFFSET: usize = HEADER_OFFSET;
const VERSION_OFFSET: usize = MAGIC_OFFSET + 8;
const RECORD_SIZE_OFFSET: usize = VERSION_OFFSET + 2;
const DEVICE_OFFSET: usize = HEADER_OFFSET + 16;
const ABSOLUTE_OFFSET_OFFSET: usize = DEVICE_OFFSET + 16;
const PARTITION_LENGTH_OFFSET: usize = ABSOLUTE_OFFSET_OFFSET + 8;
const SLOT_OFFSET: usize = PARTITION_LENGTH_OFFSET + 4;
const WIRE_LENGTH_OFFSET: usize = SLOT_OFFSET + 2;
const SUBMISSION_ID_OFFSET: usize = WIRE_LENGTH_OFFSET + 2;
const PRINCIPAL_OFFSET: usize = SUBMISSION_ID_OFFSET + 8;
const IDEMPOTENCY_OFFSET: usize = PRINCIPAL_OFFSET + 16;
const DESTINATION_OFFSET: usize = IDEMPOTENCY_OFFSET + 16;
const MESSAGE_ID_OFFSET: usize = DESTINATION_OFFSET + 16;
const WIRE_DIGEST_OFFSET: usize = MESSAGE_ID_OFFSET + 32;
const RESERVED_OFFSET: usize = WIRE_DIGEST_OFFSET + 32;

const MAGIC: &[u8; 8] = b"LXOBX001";
const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/product-outbox/record/v1\0";
const CLAIM_MARKER: [u8; CLAIM_SIZE] = [
    0x6d, 0x21, 0xe8, 0x43, 0x95, 0x0a, 0xbc, 0x57, 0x31, 0xf4, 0x76, 0x9b, 0x02, 0xcd, 0x58, 0xa1,
    0x8e, 0x39, 0xd2, 0x64, 0x17, 0xfa, 0x40, 0xb5, 0x7c, 0x03, 0x99, 0xe6, 0x2a, 0x51, 0xdf, 0x08,
];
const COMMIT_MARKER: [u8; COMMIT_SIZE] = [
    0x93, 0x4c, 0x17, 0xea, 0x60, 0xbd, 0x28, 0xf5, 0x0b, 0x76, 0xd1, 0x49, 0xae, 0x32, 0x85, 0xfc,
    0x54, 0x09, 0xc7, 0x3e, 0x91, 0x6a, 0x20, 0xdb, 0x47, 0xf8, 0x13, 0x65, 0xba, 0x0e, 0xd4, 0x79,
];
const DELIVERED_MARKER: [u8; DELIVERED_SIZE] = [
    0x2f, 0xd8, 0x64, 0x0b, 0xb1, 0x47, 0x9a, 0x35, 0xec, 0x70, 0x16, 0xc9, 0x52, 0x8d, 0x03, 0xfa,
    0xa4, 0x1e, 0x78, 0xd3, 0x09, 0xb6, 0x5c, 0xe1, 0x43, 0x97, 0x2a, 0xf0, 0x6d, 0x18, 0xcb, 0x85,
];

const _: () = assert!(RESERVED_OFFSET <= HEADER_OFFSET + HEADER_SIZE);
const _: () = assert!(RETAINED_PREFIX_SIZE <= OUTBOX_RECORD_SIZE);
const _: () = assert!(OUTBOX_STORE_SIZE == 0x0004_0000);

/// Stable physical provenance for the logical outbox quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboxBinding {
    device: [u8; 16],
    absolute_offset: usize,
    length: usize,
}

impl OutboxBinding {
    /// Bind the initial outbox format to one flash device and exact range.
    pub const fn new(device: [u8; 16], absolute_offset: usize, length: usize) -> Self {
        Self {
            device,
            absolute_offset,
            length,
        }
    }

    /// Exact logical-store length.
    pub const fn length(self) -> usize {
        self.length
    }
}

/// Product-local submission identifier, stable until the clean-reset boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutboxSubmissionId(u64);

impl OutboxSubmissionId {
    /// Construct a nonzero in-range identifier.
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 || value > OUTBOX_RECORD_CAPACITY as u64 {
            None
        } else {
            Some(Self(value))
        }
    }

    /// Complete nonzero representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Immutable metadata for one durable outbound intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboxEntry {
    slot: u16,
    submission_id: OutboxSubmissionId,
    principal: [u8; 16],
    idempotency_key: [u8; 16],
    destination: [u8; 16],
    message_id: [u8; 32],
    wire_digest: [u8; 32],
    wire_len: u16,
    delivered: bool,
}

impl OutboxEntry {
    /// Stable product-local submission identifier.
    pub const fn submission_id(self) -> OutboxSubmissionId {
        self.submission_id
    }

    /// Authenticated management identity that created the intent.
    pub const fn principal(self) -> [u8; 16] {
        self.principal
    }

    /// Caller idempotency key scoped by the authenticated principal.
    pub const fn idempotency_key(self) -> [u8; 16] {
        self.idempotency_key
    }

    /// Remote `lxmf.delivery` destination.
    pub const fn destination(self) -> [u8; 16] {
        self.destination
    }

    /// Python-compatible LXMF message identifier.
    pub const fn message_id(self) -> [u8; 32] {
        self.message_id
    }

    /// SHA-256 of the complete signed LXMF wire.
    pub const fn wire_digest(self) -> [u8; 32] {
        self.wire_digest
    }

    /// Complete signed LXMF wire length.
    pub const fn wire_len(self) -> u16 {
        self.wire_len
    }

    /// Whether a PRNS delivery receipt was durably recorded.
    pub const fn delivered(self) -> bool {
        self.delivered
    }
}

/// Caller-owned index slot used to keep the mounted flash scan off the stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboxIndexSlot(Option<OutboxEntry>);

impl OutboxIndexSlot {
    /// Empty index storage suitable for static allocation.
    pub const EMPTY: Self = Self(None);
}

/// Mounted immutable-record outbox.
pub struct MountedOutbox<'a> {
    binding: OutboxBinding,
    index: &'a mut [OutboxIndexSlot],
    entry_count: usize,
    next_slot: usize,
}

impl MountedOutbox<'_> {
    /// Number of committed intents.
    pub const fn len(&self) -> usize {
        self.entry_count
    }

    /// Whether no durable intent exists.
    pub const fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// First undelivered intent in physical commit order.
    pub fn next_pending(&self) -> Option<OutboxEntry> {
        self.entries().find(|entry| !entry.delivered)
    }

    /// Principal-owned submission metadata, hiding foreign identifiers.
    pub fn submission(&self, principal: [u8; 16], id: OutboxSubmissionId) -> Option<OutboxEntry> {
        self.entries()
            .find(|entry| entry.principal == principal && entry.submission_id == id)
    }

    /// Existing principal-scoped idempotency record.
    pub fn idempotency(&self, principal: [u8; 16], key: [u8; 16]) -> Option<OutboxEntry> {
        self.entries()
            .find(|entry| entry.principal == principal && entry.idempotency_key == key)
    }

    fn entries(&self) -> impl Iterator<Item = OutboxEntry> + '_ {
        self.index[..self.entry_count]
            .iter()
            .filter_map(|slot| slot.0)
    }
}

/// Complete source-free intent after product LXMF composition.
pub struct OutboxCandidate<'a> {
    /// Authenticated management identity.
    pub principal: [u8; 16],
    /// Caller-selected idempotency key.
    pub idempotency_key: [u8; 16],
    /// Remote `lxmf.delivery` destination.
    pub destination: [u8; 16],
    /// Python-compatible LXMF message identifier.
    pub message_id: [u8; 32],
    /// Exact complete signed LXMF wire.
    pub wire: &'a [u8],
}

/// Durable acceptance result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxAcceptance {
    /// A new immutable record committed.
    Accepted(OutboxEntry),
    /// The same principal/key already names this exact composed message.
    Replay(OutboxEntry),
    /// The principal/key already names different message semantics.
    IdempotencyConflict,
    /// No erased record remains in the bounded quota.
    CapacityExhausted,
}

/// Stable format or backend failure.
#[derive(Debug, Eq, PartialEq)]
pub enum OutboxError<E> {
    /// Raw NOR access failed.
    Backend(E),
    /// Backend capacity or I/O geometry does not match the binding.
    InvalidBinding,
    /// Programmed media is not a valid record or recognizable torn append.
    Corrupt {
        /// Zero-based physical record slot.
        slot: usize,
    },
    /// The caller supplied an invalid candidate.
    InvalidCandidate,
    /// Program readback did not establish the intended durable state.
    Verification {
        /// Zero-based physical record slot.
        slot: usize,
    },
}

enum RecordState {
    Erased,
    Torn,
    Committed(OutboxEntry),
}

/// Mount and fully validate the append-only outbox without mutating flash.
pub fn mount<'a, A>(
    access: &mut A,
    binding: OutboxBinding,
    index: &'a mut [OutboxIndexSlot],
) -> Result<MountedOutbox<'a>, OutboxError<A::Error>>
where
    A: ReadNorFlash,
{
    validate_access::<A>(access, binding, index)?;
    index.fill(OutboxIndexSlot::EMPTY);
    let mut entry_count = 0;
    let mut next_slot = OUTBOX_RECORD_CAPACITY;
    let mut erased_seen = false;
    for slot in 0..OUTBOX_RECORD_CAPACITY {
        match inspect_record(access, binding, slot)? {
            RecordState::Erased => {
                if !erased_seen {
                    next_slot = slot;
                    erased_seen = true;
                }
            }
            RecordState::Torn => {
                if erased_seen {
                    return Err(OutboxError::Corrupt { slot });
                }
            }
            RecordState::Committed(entry) => {
                if erased_seen {
                    return Err(OutboxError::Corrupt { slot });
                }
                index[entry_count] = OutboxIndexSlot(Some(entry));
                entry_count += 1;
            }
        }
    }
    Ok(MountedOutbox {
        binding,
        index,
        entry_count,
        next_slot,
    })
}

/// Commit or replay one exact composed message.
pub fn accept<A>(
    current: &mut MountedOutbox<'_>,
    access: &mut A,
    candidate: OutboxCandidate<'_>,
) -> Result<OutboxAcceptance, OutboxError<A::Error>>
where
    A: MultiwriteNorFlash,
{
    validate_access::<A>(access, current.binding, current.index)?;
    reconcile_append_frontier(current, access)?;
    if candidate.wire.len() < 96
        || candidate.wire.len() > OUTBOX_WIRE_CAPACITY
        || candidate.wire[..16] != candidate.destination
    {
        return Err(OutboxError::InvalidCandidate);
    }
    let wire_digest: [u8; 32] = Sha256::digest(candidate.wire).into();
    if let Some(existing) = current.idempotency(candidate.principal, candidate.idempotency_key) {
        return if existing.destination == candidate.destination
            && existing.message_id == candidate.message_id
            && existing.wire_digest == wire_digest
            && usize::from(existing.wire_len) == candidate.wire.len()
        {
            Ok(OutboxAcceptance::Replay(existing))
        } else {
            Ok(OutboxAcceptance::IdempotencyConflict)
        };
    }
    if current.next_slot >= OUTBOX_RECORD_CAPACITY {
        return Ok(OutboxAcceptance::CapacityExhausted);
    }
    let slot = current.next_slot;
    let submission_id = OutboxSubmissionId((slot as u64) + 1);
    let entry = OutboxEntry {
        slot: slot as u16,
        submission_id,
        principal: candidate.principal,
        idempotency_key: candidate.idempotency_key,
        destination: candidate.destination,
        message_id: candidate.message_id,
        wire_digest,
        wire_len: candidate.wire.len() as u16,
        delivered: false,
    };
    if let Err(write_error) = write_record(access, current.binding, entry, candidate.wire) {
        match reconcile_append_frontier(current, access) {
            Ok(()) => {
                if let Some(recovered) =
                    current.idempotency(candidate.principal, candidate.idempotency_key)
                    && recovered.destination == candidate.destination
                    && recovered.message_id == candidate.message_id
                    && recovered.wire_digest == wire_digest
                    && usize::from(recovered.wire_len) == candidate.wire.len()
                {
                    return Ok(OutboxAcceptance::Replay(recovered));
                }
            }
            Err(reconcile_error) => return Err(reconcile_error),
        }
        return Err(write_error);
    }
    let recovered = match inspect_record(access, current.binding, slot)? {
        RecordState::Committed(recovered) if recovered == entry => recovered,
        _ => return Err(OutboxError::Verification { slot }),
    };
    current.index[current.entry_count] = OutboxIndexSlot(Some(recovered));
    current.entry_count += 1;
    current.next_slot += 1;
    Ok(OutboxAcceptance::Accepted(recovered))
}

/// Read exact complete LXMF wire for one indexed record.
pub fn read_wire<A>(
    current: &MountedOutbox<'_>,
    access: &mut A,
    entry: OutboxEntry,
    output: &mut [u8],
) -> Result<usize, OutboxError<A::Error>>
where
    A: ReadNorFlash,
{
    validate_access::<A>(access, current.binding, current.index)?;
    let indexed = current
        .entries()
        .find(|candidate| candidate.submission_id == entry.submission_id)
        .ok_or(OutboxError::Corrupt {
            slot: entry.slot as usize,
        })?;
    if indexed != entry || output.len() < usize::from(entry.wire_len) {
        return Err(OutboxError::InvalidCandidate);
    }
    let len = usize::from(entry.wire_len);
    let read_len = len.next_multiple_of(A::READ_SIZE);
    let mut aligned_wire = [0_u8; OUTBOX_WIRE_CAPACITY];
    access
        .read(
            record_offset(entry.slot as usize, WIRE_OFFSET)?,
            &mut aligned_wire[..read_len],
        )
        .map_err(OutboxError::Backend)?;
    if <[u8; 32]>::from(Sha256::digest(&aligned_wire[..len])) != entry.wire_digest {
        return Err(OutboxError::Corrupt {
            slot: entry.slot as usize,
        });
    }
    output[..len].copy_from_slice(&aligned_wire[..len]);
    Ok(len)
}

/// Durably record one PRNS delivery receipt.
///
/// A torn marker remains retryable on remount. Retrying uses the exact same
/// LXMF message ID and is therefore an application replay, not a new intent.
pub fn mark_delivered<A>(
    current: &mut MountedOutbox<'_>,
    access: &mut A,
    entry: OutboxEntry,
) -> Result<OutboxEntry, OutboxError<A::Error>>
where
    A: MultiwriteNorFlash,
{
    validate_access::<A>(access, current.binding, current.index)?;
    let position = current.index[..current.entry_count]
        .iter()
        .position(|slot| slot.0.is_some_and(|candidate| candidate == entry))
        .ok_or(OutboxError::Corrupt {
            slot: entry.slot as usize,
        })?;
    if entry.delivered {
        return Ok(entry);
    }
    let write_error = access
        .write(
            record_offset(entry.slot as usize, DELIVERED_OFFSET)?,
            &DELIVERED_MARKER,
        )
        .err();
    let recovered = match inspect_record(access, current.binding, entry.slot as usize)? {
        RecordState::Committed(recovered) if recovered.delivered => recovered,
        _ if write_error.is_some() => {
            return Err(OutboxError::Backend(
                write_error.expect("the guarded backend error is present"),
            ));
        }
        _ => {
            return Err(OutboxError::Verification {
                slot: entry.slot as usize,
            });
        }
    };
    current.index[position] = OutboxIndexSlot(Some(recovered));
    Ok(recovered)
}

fn reconcile_append_frontier<A>(
    current: &mut MountedOutbox<'_>,
    access: &mut A,
) -> Result<(), OutboxError<A::Error>>
where
    A: ReadNorFlash,
{
    while current.next_slot < OUTBOX_RECORD_CAPACITY {
        match inspect_record(access, current.binding, current.next_slot)? {
            RecordState::Erased => return Ok(()),
            RecordState::Torn => current.next_slot += 1,
            RecordState::Committed(entry) => {
                if current.entry_count >= current.index.len()
                    || current
                        .entries()
                        .any(|indexed| indexed.submission_id == entry.submission_id)
                {
                    return Err(OutboxError::Corrupt {
                        slot: current.next_slot,
                    });
                }
                current.index[current.entry_count] = OutboxIndexSlot(Some(entry));
                current.entry_count += 1;
                current.next_slot += 1;
            }
        }
    }
    Ok(())
}

fn validate_access<A>(
    access: &A,
    binding: OutboxBinding,
    index: &[OutboxIndexSlot],
) -> Result<(), OutboxError<A::Error>>
where
    A: ReadNorFlash,
{
    if binding.length != OUTBOX_STORE_SIZE
        || access.capacity() != OUTBOX_STORE_SIZE
        || !binding.absolute_offset.is_multiple_of(OUTBOX_RECORD_SIZE)
        || index.len() != OUTBOX_RECORD_CAPACITY
        || A::READ_SIZE == 0
        || A::READ_SIZE > OUTBOX_WIRE_CAPACITY
        || !WIRE_OFFSET.is_multiple_of(A::READ_SIZE)
        || !OUTBOX_WIRE_CAPACITY.is_multiple_of(A::READ_SIZE)
        || !RETAINED_PREFIX_SIZE.is_multiple_of(A::READ_SIZE)
        || !OUTBOX_RECORD_SIZE.is_multiple_of(A::READ_SIZE)
    {
        return Err(OutboxError::InvalidBinding);
    }
    Ok(())
}

fn validate_write_access<A>(_: &A) -> Result<(), OutboxError<A::Error>>
where
    A: NorFlash,
{
    if A::WRITE_SIZE == 0
        || A::ERASE_SIZE != OUTBOX_RECORD_SIZE
        || !CLAIM_SIZE.is_multiple_of(A::WRITE_SIZE)
        || !(COMMIT_OFFSET - HEADER_OFFSET).is_multiple_of(A::WRITE_SIZE)
        || !COMMIT_SIZE.is_multiple_of(A::WRITE_SIZE)
        || !DELIVERED_SIZE.is_multiple_of(A::WRITE_SIZE)
    {
        return Err(OutboxError::InvalidBinding);
    }
    Ok(())
}

fn write_record<A>(
    access: &mut A,
    binding: OutboxBinding,
    entry: OutboxEntry,
    wire: &[u8],
) -> Result<(), OutboxError<A::Error>>
where
    A: MultiwriteNorFlash,
{
    validate_write_access(access)?;
    ensure_erased(access, entry.slot as usize)?;
    let base = entry.slot as usize * OUTBOX_RECORD_SIZE;
    access
        .write(base as u32 + CLAIM_OFFSET as u32, &CLAIM_MARKER)
        .map_err(OutboxError::Backend)?;

    let mut body = [0xff_u8; COMMIT_OFFSET - HEADER_OFFSET];
    let (header, wire_and_digest) = body.split_at_mut(HEADER_SIZE);
    header.fill(0);
    header[MAGIC_OFFSET - HEADER_OFFSET..VERSION_OFFSET - HEADER_OFFSET].copy_from_slice(MAGIC);
    put_u16(
        header,
        VERSION_OFFSET - HEADER_OFFSET,
        PHYSICAL_FORMAT_VERSION,
    );
    put_u16(
        header,
        RECORD_SIZE_OFFSET - HEADER_OFFSET,
        OUTBOX_RECORD_SIZE as u16,
    );
    header[DEVICE_OFFSET - HEADER_OFFSET..ABSOLUTE_OFFSET_OFFSET - HEADER_OFFSET]
        .copy_from_slice(&binding.device);
    put_u64(
        header,
        ABSOLUTE_OFFSET_OFFSET - HEADER_OFFSET,
        (binding.absolute_offset + base) as u64,
    );
    put_u32(
        header,
        PARTITION_LENGTH_OFFSET - HEADER_OFFSET,
        binding.length as u32,
    );
    put_u16(header, SLOT_OFFSET - HEADER_OFFSET, entry.slot);
    put_u16(header, WIRE_LENGTH_OFFSET - HEADER_OFFSET, entry.wire_len);
    put_u64(
        header,
        SUBMISSION_ID_OFFSET - HEADER_OFFSET,
        entry.submission_id.get(),
    );
    header[PRINCIPAL_OFFSET - HEADER_OFFSET..IDEMPOTENCY_OFFSET - HEADER_OFFSET]
        .copy_from_slice(&entry.principal);
    header[IDEMPOTENCY_OFFSET - HEADER_OFFSET..DESTINATION_OFFSET - HEADER_OFFSET]
        .copy_from_slice(&entry.idempotency_key);
    header[DESTINATION_OFFSET - HEADER_OFFSET..MESSAGE_ID_OFFSET - HEADER_OFFSET]
        .copy_from_slice(&entry.destination);
    header[MESSAGE_ID_OFFSET - HEADER_OFFSET..WIRE_DIGEST_OFFSET - HEADER_OFFSET]
        .copy_from_slice(&entry.message_id);
    header[WIRE_DIGEST_OFFSET - HEADER_OFFSET..RESERVED_OFFSET - HEADER_OFFSET]
        .copy_from_slice(&entry.wire_digest);
    wire_and_digest[..wire.len()].copy_from_slice(wire);
    let digest = record_digest(header, wire);
    wire_and_digest[DIGEST_OFFSET - WIRE_OFFSET..COMMIT_OFFSET - WIRE_OFFSET]
        .copy_from_slice(&digest);
    access
        .write(base as u32 + HEADER_OFFSET as u32, &body)
        .map_err(OutboxError::Backend)?;
    access
        .write(base as u32 + COMMIT_OFFSET as u32, &COMMIT_MARKER)
        .map_err(OutboxError::Backend)
}

fn inspect_record<A>(
    access: &mut A,
    binding: OutboxBinding,
    slot: usize,
) -> Result<RecordState, OutboxError<A::Error>>
where
    A: ReadNorFlash,
{
    let mut record = [0xff_u8; RETAINED_PREFIX_SIZE];
    access
        .read(record_offset(slot, 0)?, &mut record)
        .map_err(OutboxError::Backend)?;
    if record.iter().all(|byte| *byte == 0xff) {
        if tail_is_erased(access, slot, RETAINED_PREFIX_SIZE)? {
            return Ok(RecordState::Erased);
        }
        return Err(OutboxError::Corrupt { slot });
    }
    let claim = &record[CLAIM_OFFSET..CLAIM_OFFSET + CLAIM_SIZE];
    let commit = &record[COMMIT_OFFSET..COMMIT_OFFSET + COMMIT_SIZE];
    if commit != COMMIT_MARKER {
        if marker_write_compatible(claim, &CLAIM_MARKER) {
            return Ok(RecordState::Torn);
        }
        return Err(OutboxError::Corrupt { slot });
    }
    if claim != CLAIM_MARKER || !tail_is_erased(access, slot, RETAINED_PREFIX_SIZE)? {
        return Err(OutboxError::Corrupt { slot });
    }
    let header = &record[HEADER_OFFSET..WIRE_OFFSET];
    if &header[MAGIC_OFFSET - HEADER_OFFSET..VERSION_OFFSET - HEADER_OFFSET] != MAGIC
        || get_u16(header, VERSION_OFFSET - HEADER_OFFSET) != PHYSICAL_FORMAT_VERSION
        || get_u16(header, RECORD_SIZE_OFFSET - HEADER_OFFSET) != OUTBOX_RECORD_SIZE as u16
        || header[DEVICE_OFFSET - HEADER_OFFSET..ABSOLUTE_OFFSET_OFFSET - HEADER_OFFSET]
            != binding.device
        || get_u64(header, ABSOLUTE_OFFSET_OFFSET - HEADER_OFFSET)
            != (binding.absolute_offset + slot * OUTBOX_RECORD_SIZE) as u64
        || get_u32(header, PARTITION_LENGTH_OFFSET - HEADER_OFFSET) != binding.length as u32
        || get_u16(header, SLOT_OFFSET - HEADER_OFFSET) as usize != slot
        || header[RESERVED_OFFSET - HEADER_OFFSET..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(OutboxError::Corrupt { slot });
    }
    let wire_len = get_u16(header, WIRE_LENGTH_OFFSET - HEADER_OFFSET);
    let submission_id = get_u64(header, SUBMISSION_ID_OFFSET - HEADER_OFFSET);
    if !(96..=OUTBOX_WIRE_CAPACITY).contains(&usize::from(wire_len))
        || submission_id != slot as u64 + 1
    {
        return Err(OutboxError::Corrupt { slot });
    }
    let wire = &record[WIRE_OFFSET..WIRE_OFFSET + usize::from(wire_len)];
    if record[WIRE_OFFSET + usize::from(wire_len)..DIGEST_OFFSET]
        .iter()
        .any(|byte| *byte != 0xff)
        || record[DIGEST_OFFSET..COMMIT_OFFSET] != record_digest(header, wire)
    {
        return Err(OutboxError::Corrupt { slot });
    }
    let destination =
        copy_array(&header[DESTINATION_OFFSET - HEADER_OFFSET..MESSAGE_ID_OFFSET - HEADER_OFFSET]);
    if wire[..16] != destination {
        return Err(OutboxError::Corrupt { slot });
    }
    let wire_digest =
        copy_array(&header[WIRE_DIGEST_OFFSET - HEADER_OFFSET..RESERVED_OFFSET - HEADER_OFFSET]);
    if <[u8; 32]>::from(Sha256::digest(wire)) != wire_digest {
        return Err(OutboxError::Corrupt { slot });
    }
    let delivered_bytes = &record[DELIVERED_OFFSET..DELIVERED_OFFSET + DELIVERED_SIZE];
    let delivered = if delivered_bytes == DELIVERED_MARKER {
        true
    } else if delivered_bytes.iter().all(|byte| *byte == 0xff)
        || marker_write_compatible(delivered_bytes, &DELIVERED_MARKER)
    {
        false
    } else {
        return Err(OutboxError::Corrupt { slot });
    };
    Ok(RecordState::Committed(OutboxEntry {
        slot: slot as u16,
        submission_id: OutboxSubmissionId(submission_id),
        principal: copy_array(
            &header[PRINCIPAL_OFFSET - HEADER_OFFSET..IDEMPOTENCY_OFFSET - HEADER_OFFSET],
        ),
        idempotency_key: copy_array(
            &header[IDEMPOTENCY_OFFSET - HEADER_OFFSET..DESTINATION_OFFSET - HEADER_OFFSET],
        ),
        destination,
        message_id: copy_array(
            &header[MESSAGE_ID_OFFSET - HEADER_OFFSET..WIRE_DIGEST_OFFSET - HEADER_OFFSET],
        ),
        wire_digest,
        wire_len,
        delivered,
    }))
}

fn ensure_erased<A>(access: &mut A, slot: usize) -> Result<(), OutboxError<A::Error>>
where
    A: ReadNorFlash,
{
    if record_is_erased(access, slot)? {
        Ok(())
    } else {
        Err(OutboxError::Corrupt { slot })
    }
}

fn record_is_erased<A>(access: &mut A, slot: usize) -> Result<bool, OutboxError<A::Error>>
where
    A: ReadNorFlash,
{
    let mut chunk = [0_u8; 256];
    let mut offset = 0;
    while offset < OUTBOX_RECORD_SIZE {
        access
            .read(record_offset(slot, offset)?, &mut chunk)
            .map_err(OutboxError::Backend)?;
        if chunk.iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        offset += chunk.len();
    }
    Ok(true)
}

fn tail_is_erased<A>(
    access: &mut A,
    slot: usize,
    from: usize,
) -> Result<bool, OutboxError<A::Error>>
where
    A: ReadNorFlash,
{
    let mut chunk = [0_u8; 256];
    let mut offset = from;
    while offset < OUTBOX_RECORD_SIZE {
        let len = chunk.len().min(OUTBOX_RECORD_SIZE - offset);
        access
            .read(record_offset(slot, offset)?, &mut chunk[..len])
            .map_err(OutboxError::Backend)?;
        if chunk[..len].iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        offset += len;
    }
    Ok(true)
}

fn marker_write_compatible(actual: &[u8], intended: &[u8]) -> bool {
    actual
        .iter()
        .zip(intended)
        .all(|(actual, intended)| actual & intended == *intended)
}

fn record_digest(header: &[u8], wire: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    digest.update(header);
    digest.update(wire);
    digest.finalize().into()
}

fn record_offset<E>(slot: usize, within: usize) -> Result<u32, OutboxError<E>> {
    let offset = slot
        .checked_mul(OUTBOX_RECORD_SIZE)
        .and_then(|base| base.checked_add(within))
        .and_then(|offset| u32::try_from(offset).ok())
        .ok_or(OutboxError::InvalidBinding)?;
    Ok(offset)
}

fn copy_array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    bytes
        .try_into()
        .expect("fixed format slice has exact length")
}

fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(copy_array(&input[offset..offset + 2]))
}

fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(copy_array(&input[offset..offset + 4]))
}

fn put_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(copy_array(&input[offset..offset + 8]))
}

#[cfg(test)]
mod tests {
    use embedded_storage::nor_flash::{ErrorType, NorFlashError, NorFlashErrorKind};

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TestError;

    impl NorFlashError for TestError {
        fn kind(&self) -> NorFlashErrorKind {
            NorFlashErrorKind::Other
        }
    }

    struct TestFlash {
        bytes: std::vec::Vec<u8>,
    }

    impl TestFlash {
        fn erased() -> Self {
            Self {
                bytes: std::vec![0xff; OUTBOX_STORE_SIZE],
            }
        }
    }

    impl ErrorType for TestFlash {
        type Error = TestError;
    }

    impl ReadNorFlash for TestFlash {
        const READ_SIZE: usize = 4;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            if !(offset as usize).is_multiple_of(Self::READ_SIZE)
                || !bytes.len().is_multiple_of(Self::READ_SIZE)
            {
                return Err(TestError);
            }
            bytes.copy_from_slice(&self.bytes[offset as usize..offset as usize + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }

    impl NorFlash for TestFlash {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = OUTBOX_RECORD_SIZE;

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            if !(offset as usize).is_multiple_of(Self::WRITE_SIZE)
                || !bytes.len().is_multiple_of(Self::WRITE_SIZE)
            {
                return Err(TestError);
            }
            for (target, value) in self.bytes[offset as usize..].iter_mut().zip(bytes) {
                if *target & *value != *value {
                    return Err(TestError);
                }
                *target &= *value;
            }
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            self.bytes[from as usize..to as usize].fill(0xff);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for TestFlash {}

    const BINDING: OutboxBinding = OutboxBinding::new([0x44; 16], 0x2000, OUTBOX_STORE_SIZE);

    fn candidate<'a>(wire: &'a [u8]) -> OutboxCandidate<'a> {
        OutboxCandidate {
            principal: [0x11; 16],
            idempotency_key: [0x22; 16],
            destination: [0x33; 16],
            message_id: [0x55; 32],
            wire,
        }
    }

    #[test]
    fn acceptance_is_durable_idempotent_and_delivery_remounts() {
        let mut flash = TestFlash::erased();
        let mut index = [OutboxIndexSlot::EMPTY; OUTBOX_RECORD_CAPACITY];
        let mut mounted = mount(&mut flash, BINDING, &mut index).unwrap();
        let mut wire = [0x77; 120];
        wire[..16].fill(0x33);
        let accepted = match accept(&mut mounted, &mut flash, candidate(&wire)).unwrap() {
            OutboxAcceptance::Accepted(entry) => entry,
            other => panic!("unexpected acceptance: {other:?}"),
        };
        assert_eq!(accepted.submission_id().get(), 1);
        assert_eq!(mounted.next_pending(), Some(accepted));
        assert!(matches!(
            accept(&mut mounted, &mut flash, candidate(&wire)).unwrap(),
            OutboxAcceptance::Replay(entry) if entry == accepted
        ));
        let delivered = mark_delivered(&mut mounted, &mut flash, accepted).unwrap();
        assert!(delivered.delivered());
        drop(mounted);

        let mut recovered_index = [OutboxIndexSlot::EMPTY; OUTBOX_RECORD_CAPACITY];
        let recovered = mount(&mut flash, BINDING, &mut recovered_index).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered.next_pending(), None);
        assert_eq!(
            recovered.submission([0x11; 16], OutboxSubmissionId(1)),
            Some(delivered)
        );
    }

    #[test]
    fn odd_length_wire_reads_through_word_aligned_flash() {
        let mut flash = TestFlash::erased();
        let mut index = [OutboxIndexSlot::EMPTY; OUTBOX_RECORD_CAPACITY];
        let mut mounted = mount(&mut flash, BINDING, &mut index).unwrap();
        let mut wire = [0x77; 121];
        wire[..16].fill(0x33);
        let accepted = match accept(&mut mounted, &mut flash, candidate(&wire)).unwrap() {
            OutboxAcceptance::Accepted(entry) => entry,
            other => panic!("unexpected acceptance: {other:?}"),
        };

        let mut recovered = [0_u8; OUTBOX_WIRE_CAPACITY];
        let len = read_wire(&mounted, &mut flash, accepted, &mut recovered).unwrap();

        assert_eq!(len, wire.len());
        assert_eq!(&recovered[..len], &wire);
    }

    #[test]
    fn idempotency_conflict_and_foreign_status_fail_closed() {
        let mut flash = TestFlash::erased();
        let mut index = [OutboxIndexSlot::EMPTY; OUTBOX_RECORD_CAPACITY];
        let mut mounted = mount(&mut flash, BINDING, &mut index).unwrap();
        let mut wire = [0x77; 120];
        wire[..16].fill(0x33);
        let accepted = match accept(&mut mounted, &mut flash, candidate(&wire)).unwrap() {
            OutboxAcceptance::Accepted(entry) => entry,
            _ => panic!("new record accepted"),
        };
        wire[119] ^= 1;
        assert_eq!(
            accept(&mut mounted, &mut flash, candidate(&wire)).unwrap(),
            OutboxAcceptance::IdempotencyConflict
        );
        assert_eq!(
            mounted.submission([0x99; 16], accepted.submission_id()),
            None
        );
    }
}
