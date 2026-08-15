//! Power-loss-safe durable LXMF mailbox collection watermark.
//!
//! The format owns two 4 KiB erase sectors. A complete device-bound snapshot
//! is written and verified in the inactive sector before it can supersede the
//! prior committed snapshot. The old committed sector remains intact until a
//! later mutation needs to reuse it, so no post-commit cleanup failure can make
//! a successful acknowledgement ambiguous.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::num::NonZeroU64;

use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// Exact raw-NOR range required by physical format version 1.
pub const PARTITION_SIZE: usize = 8_192;
/// Size of either alternating snapshot sector.
pub const SECTOR_SIZE: usize = 4_096;
/// Current on-flash physical format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;

const PROTECTED_SIZE: usize = 256;
const DIGEST_SIZE: usize = 32;
const COMMIT_SIZE: usize = 32;
const DIGEST_OFFSET: usize = PROTECTED_SIZE;
const COMMIT_OFFSET: usize = DIGEST_OFFSET + DIGEST_SIZE;
const RECORD_SIZE: usize = COMMIT_OFFSET + COMMIT_SIZE;

const CLAIM_OFFSET: usize = 0;
const MAGIC_OFFSET: usize = 32;
const VERSION_OFFSET: usize = 40;
const RECORD_SIZE_OFFSET: usize = 42;
const DEVICE_OFFSET: usize = 48;
const ABSOLUTE_OFFSET_OFFSET: usize = 64;
const PARTITION_LENGTH_OFFSET: usize = 72;
const GENERATION_OFFSET: usize = 80;
const ACKNOWLEDGED_THROUGH_OFFSET: usize = 88;
const ACKNOWLEDGED_MESSAGE_ID_OFFSET: usize = 96;
const ACKNOWLEDGED_WIRE_DIGEST_OFFSET: usize = 128;
const RESERVED_OFFSET: usize = 160;

const CLAIM_MARKER: [u8; 32] = [
    0x5e, 0x17, 0xa4, 0xc9, 0x32, 0x6b, 0xf0, 0x48, 0x91, 0x2d, 0x73, 0xbe, 0x0a, 0xd6, 0x45, 0x8f,
    0xc1, 0x39, 0x64, 0xea, 0x27, 0x95, 0x0d, 0xb8, 0x52, 0xfc, 0x16, 0x7a, 0xa3, 0x40, 0xdd, 0x69,
];
const COMMIT_MARKER: [u8; COMMIT_SIZE] = [
    0x83, 0x2a, 0xd1, 0x57, 0xbc, 0x09, 0x6e, 0xf4, 0x35, 0x98, 0x41, 0xca, 0x70, 0x1d, 0xe6, 0x5b,
    0x24, 0xaf, 0x62, 0x0c, 0xd8, 0x93, 0x47, 0xb1, 0x7e, 0x30, 0xf9, 0x15, 0x56, 0xcb, 0x08, 0xe2,
];
const MAGIC: &[u8; 8] = b"LXMFBX01";
const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/lxmf-mailbox-store/snapshot/v1\0";

const _: () = assert!(PARTITION_SIZE == 2 * SECTOR_SIZE);
const _: () = assert!(RECORD_SIZE <= SECTOR_SIZE);

/// Stable physical identity of the flash device containing this store.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MailboxStoreDeviceId([u8; 16]);

impl MailboxStoreDeviceId {
    /// Construct an identity from exact product-supplied bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete identity.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Exact physical provenance carried by one operation-scoped backend view.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MailboxStoreBinding {
    device: MailboxStoreDeviceId,
    absolute_offset: usize,
    length: usize,
    format_version: u16,
}

impl MailboxStoreBinding {
    /// Construct a complete physical binding.
    pub const fn new(
        device: MailboxStoreDeviceId,
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

    /// Physical device identity.
    pub const fn device(self) -> MailboxStoreDeviceId {
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

/// Range-restricted backend carrying exact store provenance.
pub struct BoundMailboxStore<F> {
    backend: F,
    binding: MailboxStoreBinding,
}

impl<F> BoundMailboxStore<F> {
    /// Attach provenance to one range-restricted backend.
    pub const fn new(backend: F, binding: MailboxStoreBinding) -> Self {
        Self { backend, binding }
    }

    /// Binding carried by this view.
    pub const fn binding(&self) -> MailboxStoreBinding {
        self.binding
    }

    /// Recover the wrapped backend.
    pub fn into_backend(self) -> F {
        self.backend
    }
}

impl<F: ErrorType> ErrorType for BoundMailboxStore<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for BoundMailboxStore<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.backend.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.backend.capacity()
    }
}

impl<F: NorFlash> NorFlash for BoundMailboxStore<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.backend.write(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.backend.erase(from, to)
    }
}

impl<F: MultiwriteNorFlash> MultiwriteNorFlash for BoundMailboxStore<F> {}

/// Read-only access proving an exact mailbox-store binding.
pub trait BoundMailboxStoreReadAccess: ReadNorFlash {
    /// Binding represented by this view.
    fn mailbox_store_binding(&self) -> MailboxStoreBinding;
}

impl<F: ReadNorFlash> BoundMailboxStoreReadAccess for BoundMailboxStore<F> {
    fn mailbox_store_binding(&self) -> MailboxStoreBinding {
        self.binding
    }
}

/// Writable access proving an exact mailbox-store binding.
pub trait BoundMailboxStoreAccess: BoundMailboxStoreReadAccess + MultiwriteNorFlash {}

impl<F: MultiwriteNorFlash> BoundMailboxStoreAccess for BoundMailboxStore<F> {}

/// One of the two alternating physical sectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxStoreSector {
    /// First sector.
    A,
    /// Second sector.
    B,
}

impl MailboxStoreSector {
    const fn offset(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => SECTOR_SIZE,
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// Identity-bound cursor for one acknowledged durable LXMF receipt.
///
/// The numeric handle alone is not an incarnation boundary: an erased LXMF
/// store can allocate the same handle to a different message. Persisting both
/// the logical message ID and exact retained-wire digest lets the product
/// reject that stale interpretation before admitting later ingress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MailboxStoreCursor {
    handle: NonZeroU64,
    message_id: [u8; 32],
    exact_wire_digest: [u8; 32],
}

impl MailboxStoreCursor {
    /// Construct a cursor from one durable LXMF receipt.
    pub const fn new(
        handle: NonZeroU64,
        message_id: [u8; 32],
        exact_wire_digest: [u8; 32],
    ) -> Self {
        Self {
            handle,
            message_id,
            exact_wire_digest,
        }
    }

    /// Stable nonzero LXMF handle.
    pub const fn handle(self) -> NonZeroU64 {
        self.handle
    }

    /// Python-compatible authenticated LXMF message ID at this handle.
    pub const fn message_id(self) -> [u8; 32] {
        self.message_id
    }

    /// Digest of the exact retained LXMF wire representation at this handle.
    pub const fn exact_wire_digest(self) -> [u8; 32] {
        self.exact_wire_digest
    }
}

/// Durable collection watermark recovered from committed media.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MountedMailboxStore {
    binding: MailboxStoreBinding,
    generation: NonZeroU64,
    acknowledged: Option<MailboxStoreCursor>,
    active_sector: MailboxStoreSector,
}

impl MountedMailboxStore {
    /// Exact physical binding established at mount.
    pub const fn binding(self) -> MailboxStoreBinding {
        self.binding
    }

    /// Monotonic snapshot generation.
    pub const fn generation(self) -> NonZeroU64 {
        self.generation
    }

    /// Highest collected LXMF handle, or zero before any message.
    pub const fn acknowledged_through(self) -> u64 {
        match self.acknowledged {
            Some(cursor) => cursor.handle.get(),
            None => 0,
        }
    }

    /// Identity-bound acknowledged receipt, or `None` before any message.
    pub const fn acknowledged_cursor(self) -> Option<MailboxStoreCursor> {
        self.acknowledged
    }

    /// Sector containing the authoritative snapshot.
    pub const fn active_sector(self) -> MailboxStoreSector {
        self.active_sector
    }
}

/// Stable binding or media contradiction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxStoreFault {
    /// Binding geometry or backend capacity is incompatible.
    InvalidBinding,
    /// Programmed data is neither a valid snapshot nor a recognizable torn write.
    UnknownProgrammedData {
        /// Damaged sector.
        sector: MailboxStoreSector,
    },
    /// A committed snapshot failed canonical validation.
    CommittedSnapshotCorrupt {
        /// Damaged sector.
        sector: MailboxStoreSector,
    },
    /// Two committed snapshots claim the same generation with different state.
    ConflictingGeneration,
    /// One numeric acknowledgement handle was paired with conflicting identity.
    CursorIdentityConflict,
    /// Snapshot generation space is exhausted.
    GenerationExhausted,
    /// Program or erase readback did not match the intended state.
    ReadbackMismatch {
        /// Sector whose operation did not verify.
        sector: MailboxStoreSector,
    },
}

/// Store I/O or stable semantic failure.
#[derive(Debug, Eq, PartialEq)]
pub enum MailboxStoreError<E> {
    /// Backend access failed.
    Backend(E),
    /// Stable fail-closed fault.
    Fault(MailboxStoreFault),
}

/// Read-only mount result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxStoreMount {
    /// At least one valid committed snapshot exists.
    Mounted(MountedMailboxStore),
    /// No committed authority exists; media is erased or only recognizably torn.
    Unprovisioned,
}

/// Mount without mutating either sector.
pub fn mount<A>(access: &mut A) -> Result<MailboxStoreMount, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreReadAccess,
{
    let binding = validate_read_access(access)?;
    let a = inspect_sector(access, binding, MailboxStoreSector::A)?;
    let b = inspect_sector(access, binding, MailboxStoreSector::B)?;
    match (a, b) {
        (SectorState::Valid(a), SectorState::Valid(b)) => {
            if a.generation == b.generation && a.acknowledged != b.acknowledged {
                return Err(MailboxStoreError::Fault(
                    MailboxStoreFault::ConflictingGeneration,
                ));
            }
            let selected = if b.generation > a.generation { b } else { a };
            Ok(MailboxStoreMount::Mounted(selected))
        }
        (SectorState::Valid(value), _) | (_, SectorState::Valid(value)) => {
            Ok(MailboxStoreMount::Mounted(value))
        }
        (SectorState::Erased | SectorState::Torn, SectorState::Erased | SectorState::Torn) => {
            Ok(MailboxStoreMount::Unprovisioned)
        }
        (SectorState::Unknown, _) => Err(MailboxStoreError::Fault(
            MailboxStoreFault::UnknownProgrammedData {
                sector: MailboxStoreSector::A,
            },
        )),
        (_, SectorState::Unknown) => Err(MailboxStoreError::Fault(
            MailboxStoreFault::UnknownProgrammedData {
                sector: MailboxStoreSector::B,
            },
        )),
    }
}

/// Establish the first durable watermark on erased or recognizably torn media.
///
/// This is also the migration primitive: callers may baseline the first
/// snapshot to the latest pre-existing LXMF handle before opening ingress.
pub fn provision<A>(
    access: &mut A,
    acknowledged: Option<MailboxStoreCursor>,
) -> Result<MountedMailboxStore, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreAccess,
{
    let binding = validate_write_access(access)?;
    if matches!(mount(access)?, MailboxStoreMount::Mounted(_)) {
        return Err(MailboxStoreError::Fault(
            MailboxStoreFault::ConflictingGeneration,
        ));
    }
    ensure_erased(access, MailboxStoreSector::A)?;
    let generation = NonZeroU64::MIN;
    write_snapshot(
        access,
        binding,
        MailboxStoreSector::A,
        generation,
        acknowledged,
    )
}

/// Monotonically advance the durable collection watermark.
///
/// Equal or lower requests are exact idempotent successes and perform no I/O.
pub fn acknowledge_through<A>(
    current: MountedMailboxStore,
    access: &mut A,
    acknowledged: MailboxStoreCursor,
) -> Result<MountedMailboxStore, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreAccess,
{
    let binding = validate_write_access(access)?;
    if binding != current.binding {
        return Err(MailboxStoreError::Fault(MailboxStoreFault::InvalidBinding));
    }
    if acknowledged.handle.get() < current.acknowledged_through() {
        return Ok(current);
    }
    if acknowledged.handle.get() == current.acknowledged_through() {
        return if current.acknowledged == Some(acknowledged) {
            Ok(current)
        } else {
            Err(MailboxStoreError::Fault(
                MailboxStoreFault::CursorIdentityConflict,
            ))
        };
    }
    write_successor(current, access, Some(acknowledged))
}

/// Rebind notification state to the established tail of a new LXMF-store
/// incarnation.
///
/// Unlike [`acknowledge_through`], this operation may move the numeric handle
/// backwards. Callers must use it only after proving that the mounted
/// acknowledgement identity is absent or contradictory in the current LXMF
/// store. The complete identity-bound successor is still committed atomically.
pub fn rebaseline<A>(
    current: MountedMailboxStore,
    access: &mut A,
    acknowledged: Option<MailboxStoreCursor>,
) -> Result<MountedMailboxStore, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreAccess,
{
    let binding = validate_write_access(access)?;
    if binding != current.binding {
        return Err(MailboxStoreError::Fault(MailboxStoreFault::InvalidBinding));
    }
    if acknowledged == current.acknowledged {
        return Ok(current);
    }
    write_successor(current, access, acknowledged)
}

fn write_successor<A>(
    current: MountedMailboxStore,
    access: &mut A,
    acknowledged: Option<MailboxStoreCursor>,
) -> Result<MountedMailboxStore, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreAccess,
{
    let generation = NonZeroU64::new(current.generation.get().checked_add(1).ok_or(
        MailboxStoreError::Fault(MailboxStoreFault::GenerationExhausted),
    )?)
    .expect("a successful increment of a nonzero generation remains nonzero");
    let target = current.active_sector.other();
    ensure_erased(access, target)?;
    write_snapshot(access, current.binding, target, generation, acknowledged)
}

fn validate_read_access<A>(access: &A) -> Result<MailboxStoreBinding, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreReadAccess,
{
    let binding = access.mailbox_store_binding();
    if binding.length() != PARTITION_SIZE
        || binding.format_version() != PHYSICAL_FORMAT_VERSION
        || !binding.absolute_offset().is_multiple_of(SECTOR_SIZE)
        || access.capacity() != PARTITION_SIZE
        || A::READ_SIZE == 0
        || !RECORD_SIZE.is_multiple_of(A::READ_SIZE)
        || !SECTOR_SIZE.is_multiple_of(A::READ_SIZE)
    {
        return Err(MailboxStoreError::Fault(MailboxStoreFault::InvalidBinding));
    }
    Ok(binding)
}

fn validate_write_access<A>(access: &A) -> Result<MailboxStoreBinding, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreAccess,
{
    let binding = validate_read_access(access)?;
    if A::WRITE_SIZE == 0
        || A::ERASE_SIZE == 0
        || !DIGEST_OFFSET.is_multiple_of(A::WRITE_SIZE)
        || !COMMIT_SIZE.is_multiple_of(A::WRITE_SIZE)
        || !SECTOR_SIZE.is_multiple_of(A::ERASE_SIZE)
    {
        return Err(MailboxStoreError::Fault(MailboxStoreFault::InvalidBinding));
    }
    Ok(binding)
}

#[derive(Clone, Copy)]
enum SectorState {
    Erased,
    Torn,
    Unknown,
    Valid(MountedMailboxStore),
}

fn inspect_sector<A>(
    access: &mut A,
    binding: MailboxStoreBinding,
    sector: MailboxStoreSector,
) -> Result<SectorState, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreReadAccess,
{
    let mut record = [0xff_u8; RECORD_SIZE];
    access
        .read(sector.offset() as u32, &mut record)
        .map_err(MailboxStoreError::Backend)?;
    let all_erased =
        record.iter().all(|byte| *byte == 0xff) && remainder_is_erased(access, sector)?;
    if all_erased {
        return Ok(SectorState::Erased);
    }
    if !remainder_is_erased(access, sector)? {
        return Ok(SectorState::Unknown);
    }
    let commit = &record[COMMIT_OFFSET..];
    if commit != COMMIT_MARKER {
        if monotonic_partial(commit, &COMMIT_MARKER) && torn_prefix_compatible(binding, &record) {
            return Ok(SectorState::Torn);
        }
        return Ok(SectorState::Unknown);
    }
    Ok(decode_snapshot(binding, sector, &record)
        .map(SectorState::Valid)
        .unwrap_or(SectorState::Unknown))
}

fn remainder_is_erased<A>(
    access: &mut A,
    sector: MailboxStoreSector,
) -> Result<bool, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreReadAccess,
{
    let mut bytes = [0_u8; 64];
    let mut offset = sector.offset() + RECORD_SIZE;
    while offset < sector.offset() + SECTOR_SIZE {
        access
            .read(offset as u32, &mut bytes)
            .map_err(MailboxStoreError::Backend)?;
        if bytes.iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        offset += bytes.len();
    }
    Ok(true)
}

fn ensure_erased<A>(
    access: &mut A,
    sector: MailboxStoreSector,
) -> Result<(), MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreAccess,
{
    let from = sector.offset() as u32;
    let to = (sector.offset() + SECTOR_SIZE) as u32;
    access.erase(from, to).map_err(MailboxStoreError::Backend)?;
    let mut bytes = [0_u8; 256];
    let mut offset = sector.offset();
    while offset < sector.offset() + SECTOR_SIZE {
        access
            .read(offset as u32, &mut bytes)
            .map_err(MailboxStoreError::Backend)?;
        if bytes.iter().any(|byte| *byte != 0xff) {
            return Err(MailboxStoreError::Fault(
                MailboxStoreFault::ReadbackMismatch { sector },
            ));
        }
        offset += bytes.len();
    }
    Ok(())
}

fn write_snapshot<A>(
    access: &mut A,
    binding: MailboxStoreBinding,
    sector: MailboxStoreSector,
    generation: NonZeroU64,
    acknowledged: Option<MailboxStoreCursor>,
) -> Result<MountedMailboxStore, MailboxStoreError<A::Error>>
where
    A: BoundMailboxStoreAccess,
{
    let prefix = encode_protected_prefix(binding, generation.get(), acknowledged);
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    digest.update(prefix);
    let digest: [u8; DIGEST_SIZE] = digest.finalize().into();

    access
        .write(sector.offset() as u32, &prefix)
        .map_err(MailboxStoreError::Backend)?;
    access
        .write((sector.offset() + DIGEST_OFFSET) as u32, &digest)
        .map_err(MailboxStoreError::Backend)?;
    access
        .write((sector.offset() + COMMIT_OFFSET) as u32, &COMMIT_MARKER)
        .map_err(MailboxStoreError::Backend)?;

    let mut record = [0_u8; RECORD_SIZE];
    access
        .read(sector.offset() as u32, &mut record)
        .map_err(MailboxStoreError::Backend)?;
    decode_snapshot(binding, sector, &record).ok_or(MailboxStoreError::Fault(
        MailboxStoreFault::ReadbackMismatch { sector },
    ))
}

fn encode_protected_prefix(
    binding: MailboxStoreBinding,
    generation: u64,
    acknowledged: Option<MailboxStoreCursor>,
) -> [u8; PROTECTED_SIZE] {
    let mut bytes = [0_u8; PROTECTED_SIZE];
    bytes[CLAIM_OFFSET..MAGIC_OFFSET].copy_from_slice(&CLAIM_MARKER);
    bytes[MAGIC_OFFSET..VERSION_OFFSET].copy_from_slice(MAGIC);
    put_u16(&mut bytes, VERSION_OFFSET, PHYSICAL_FORMAT_VERSION);
    put_u16(&mut bytes, RECORD_SIZE_OFFSET, RECORD_SIZE as u16);
    bytes[DEVICE_OFFSET..ABSOLUTE_OFFSET_OFFSET].copy_from_slice(binding.device().as_bytes());
    put_u64(
        &mut bytes,
        ABSOLUTE_OFFSET_OFFSET,
        binding.absolute_offset() as u64,
    );
    put_u64(&mut bytes, PARTITION_LENGTH_OFFSET, binding.length() as u64);
    put_u64(&mut bytes, GENERATION_OFFSET, generation);
    if let Some(cursor) = acknowledged {
        put_u64(&mut bytes, ACKNOWLEDGED_THROUGH_OFFSET, cursor.handle.get());
        bytes[ACKNOWLEDGED_MESSAGE_ID_OFFSET..ACKNOWLEDGED_WIRE_DIGEST_OFFSET]
            .copy_from_slice(&cursor.message_id);
        bytes[ACKNOWLEDGED_WIRE_DIGEST_OFFSET..RESERVED_OFFSET]
            .copy_from_slice(&cursor.exact_wire_digest);
    }
    bytes[RESERVED_OFFSET..].fill(0);
    bytes
}

fn decode_snapshot(
    binding: MailboxStoreBinding,
    sector: MailboxStoreSector,
    record: &[u8; RECORD_SIZE],
) -> Option<MountedMailboxStore> {
    let generation = read_u64(record, GENERATION_OFFSET);
    let acknowledged_through = read_u64(record, ACKNOWLEDGED_THROUGH_OFFSET);
    let acknowledged = if acknowledged_through == 0 {
        if record[ACKNOWLEDGED_MESSAGE_ID_OFFSET..RESERVED_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        {
            return None;
        }
        None
    } else {
        let mut message_id = [0_u8; 32];
        message_id.copy_from_slice(
            &record[ACKNOWLEDGED_MESSAGE_ID_OFFSET..ACKNOWLEDGED_WIRE_DIGEST_OFFSET],
        );
        let mut exact_wire_digest = [0_u8; 32];
        exact_wire_digest
            .copy_from_slice(&record[ACKNOWLEDGED_WIRE_DIGEST_OFFSET..RESERVED_OFFSET]);
        Some(MailboxStoreCursor::new(
            NonZeroU64::new(acknowledged_through)?,
            message_id,
            exact_wire_digest,
        ))
    };
    let expected = encode_protected_prefix(binding, generation, acknowledged);
    if record[..DIGEST_OFFSET] != expected
        || record[COMMIT_OFFSET..] != COMMIT_MARKER
        || record[RESERVED_OFFSET..DIGEST_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
    {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    digest.update(expected);
    let expected_digest: [u8; DIGEST_SIZE] = digest.finalize().into();
    if record[DIGEST_OFFSET..COMMIT_OFFSET] != expected_digest {
        return None;
    }
    Some(MountedMailboxStore {
        binding,
        generation: NonZeroU64::new(generation)?,
        acknowledged,
        active_sector: sector,
    })
}

fn torn_prefix_compatible(binding: MailboxStoreBinding, record: &[u8; RECORD_SIZE]) -> bool {
    let reference = encode_protected_prefix(binding, u64::MAX, None);
    for (offset, length) in [
        (CLAIM_OFFSET, MAGIC_OFFSET - CLAIM_OFFSET),
        (MAGIC_OFFSET, VERSION_OFFSET - MAGIC_OFFSET),
        (VERSION_OFFSET, 4),
        (DEVICE_OFFSET, GENERATION_OFFSET - DEVICE_OFFSET),
        (RESERVED_OFFSET, DIGEST_OFFSET - RESERVED_OFFSET),
    ] {
        if !monotonic_partial(
            &record[offset..offset + length],
            &reference[offset..offset + length],
        ) {
            return false;
        }
    }
    true
}

fn monotonic_partial(observed: &[u8], intended: &[u8]) -> bool {
    observed.len() == intended.len()
        && observed
            .iter()
            .zip(intended)
            .all(|(observed, intended)| observed & intended == *intended)
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed field"))
}
