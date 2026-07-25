//! Power-loss-safe storage for one authenticated BLE bond.
//!
//! The physical format owns exactly 8 KiB as two alternating 4 KiB logical
//! sectors. A successor is written only to the inactive sector: its protected
//! prefix and SHA-256 digest are programmed and read back first, then an
//! irregular commit marker is programmed last. Until that final marker is
//! complete, the predecessor remains authoritative.
//!
//! This crate deliberately has no dependency on a Bluetooth stack. It stores
//! only the portable bond material needed by an integration: the peer BD_ADDR,
//! optional IRK, mandatory LTK, and the invariant that the bond was established
//! with authenticated security.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::num::NonZeroU64;

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// Exact raw-NOR partition length required by physical format version 1.
pub const PARTITION_SIZE: usize = 8_192;
/// Size of either alternating logical sector.
pub const SECTOR_SIZE: usize = 4_096;
/// Current on-flash physical format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;
/// Current portable bond semantic version.
pub const SEMANTIC_FORMAT_VERSION: u16 = 1;
/// Number of bonds retained by this format.
pub const BOND_CAPACITY: usize = 1;

const RECORD_SIZE: usize = 512;
const PROTECTED_SIZE: usize = 96;
const DIGEST_OFFSET: usize = PROTECTED_SIZE;
const DIGEST_SIZE: usize = 32;
const PREFIX_SIZE: usize = 256;
const COMMIT_OFFSET: usize = PREFIX_SIZE;
const COMMIT_SIZE: usize = RECORD_SIZE - COMMIT_OFFSET;
const INSPECTION_CHUNK_SIZE: usize = 256;

const MAGIC: &[u8; 8] = b"BLEBOND1";
const RECORD_KIND_BOND: u8 = 1;
const RECORD_KIND_CLEARED: u8 = 2;
const FLAG_BONDED: u8 = 1 << 0;
const FLAG_AUTHENTICATED: u8 = 1 << 1;
const FLAG_IRK_PRESENT: u8 = 1 << 2;
const REQUIRED_BOND_FLAGS: u8 = FLAG_BONDED | FLAG_AUTHENTICATED;

const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/ble-bond-store/sector-record/v1\0";
// The domain plus protected bytes leave 22 secret-bearing bytes in sha2's
// internal block buffer. These public bytes complete that block before
// finalization so no raw key bytes remain buffered in the hasher.
const DIGEST_FLUSH_TRAILER: [u8; 42] = [0xa5; 42];

const COMMIT_MARKER: [u8; COMMIT_SIZE] = make_commit_marker();

const _: () = assert!(PARTITION_SIZE == 2 * SECTOR_SIZE);
const _: () = assert!(DIGEST_OFFSET + DIGEST_SIZE <= PREFIX_SIZE);
const _: () = assert!(COMMIT_OFFSET + COMMIT_SIZE == RECORD_SIZE);
const _: () = assert!(RECORD_SIZE < SECTOR_SIZE);
const _: () =
    assert!((DIGEST_DOMAIN.len() + PROTECTED_SIZE + DIGEST_FLUSH_TRAILER.len()).is_multiple_of(64));

const fn make_commit_marker() -> [u8; COMMIT_SIZE] {
    let mut marker = [0_u8; COMMIT_SIZE];
    let mut state = 0x6d2b_79f5_u32;
    let mut index = 0_usize;
    while index < COMMIT_SIZE {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let mut byte = (state >> 24) as u8;
        if byte == 0 || byte == 0xff {
            byte ^= 0x5a;
        }
        marker[index] = byte;
        index += 1;
    }
    marker
}

/// One of the two alternating logical sectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BondStoreSector {
    /// Sector at partition-relative offset zero.
    A,
    /// Sector at partition-relative offset 4096.
    B,
}

impl BondStoreSector {
    const fn id(self) -> u8 {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

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

/// Owned portable material for one authenticated BLE bond.
///
/// The type intentionally implements neither `Clone`, `Copy`, nor `Debug`.
/// Dropping it zeroizes the IRK and LTK. A value can only represent
/// `bonded = true` with authenticated security; weaker security modes have no
/// constructor or on-flash representation accepted by this crate.
///
/// ```compile_fail
/// use reticulum_ble_bond_store::BleBond;
///
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<BleBond>();
/// ```
///
/// ```compile_fail
/// use reticulum_ble_bond_store::BleBond;
///
/// fn requires_debug<T: core::fmt::Debug>() {}
/// requires_debug::<BleBond>();
/// ```
pub struct BleBond {
    address: [u8; 6],
    irk: Option<[u8; 16]>,
    ltk: [u8; 16],
}

impl BleBond {
    /// Construct one bonded, authenticated peer.
    pub const fn new(address: [u8; 6], irk: Option<[u8; 16]>, ltk: [u8; 16]) -> Self {
        Self { address, irk, ltk }
    }

    /// Borrow the peer Bluetooth device address in integration-defined byte
    /// order.
    pub const fn address(&self) -> &[u8; 6] {
        &self.address
    }

    /// Borrow the optional identity resolving key.
    pub const fn irk(&self) -> Option<&[u8; 16]> {
        self.irk.as_ref()
    }

    /// Borrow the long-term key.
    pub const fn ltk(&self) -> &[u8; 16] {
        &self.ltk
    }

    /// This representation always describes a completed bond.
    pub const fn is_bonded(&self) -> bool {
        true
    }

    /// This representation only accepts authenticated security.
    pub const fn is_authenticated(&self) -> bool {
        true
    }
}

impl Zeroize for BleBond {
    fn zeroize(&mut self) {
        self.irk.zeroize();
        self.ltk.zeroize();
    }
}

impl ZeroizeOnDrop for BleBond {}

impl Drop for BleBond {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Explicit cleanup reported by a successful read-only mount.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BondStoreCleanup {
    /// The inactive sector is already fully erased.
    Clean,
    /// This non-authoritative sector may be erased.
    EraseInactive {
        /// Exact sector safe to erase.
        sector: BondStoreSector,
    },
}

/// Successfully mounted authoritative state.
///
/// The type intentionally implements neither `Clone`, `Copy`, nor `Debug`
/// because it may own a [`BleBond`].
#[must_use = "mounted bond ownership must be deliberately retained or dropped"]
pub struct MountedBondStore {
    generation: Option<NonZeroU64>,
    active_sector: Option<BondStoreSector>,
    bond: Option<BleBond>,
    cleanup: BondStoreCleanup,
}

impl MountedBondStore {
    /// Return the authoritative nonzero generation, or `None` for fresh erased
    /// media.
    pub const fn generation(&self) -> Option<NonZeroU64> {
        self.generation
    }

    /// Return the sector containing the authoritative committed state.
    pub const fn active_sector(&self) -> Option<BondStoreSector> {
        self.active_sector
    }

    /// Borrow the stored bond. A committed clear tombstone and fresh media
    /// both return `None`.
    pub const fn bond(&self) -> Option<&BleBond> {
        self.bond.as_ref()
    }

    /// Whether this is untouched, completely erased media.
    pub const fn is_fresh_erased(&self) -> bool {
        self.generation.is_none()
    }

    /// Physical cleanup that may be performed without changing authoritative
    /// state.
    pub const fn cleanup(&self) -> BondStoreCleanup {
        self.cleanup
    }

    /// Consume the mounted state and transfer ownership of its bond.
    ///
    /// This is the only move-out path for secret material loaded from flash;
    /// it does not duplicate the bond and leaves no second owner behind.
    pub fn into_bond(mut self) -> Option<BleBond> {
        self.bond.take()
    }
}

/// Stable non-secret media or operation fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BondStoreFault {
    /// Backend capacity or operation granularities cannot represent this exact
    /// format.
    IncompatibleFlash,
    /// No valid committed state exists and programmed data remains. Explicit
    /// [`recover_empty`] is required before provisioning.
    RecoveryRequired,
    /// Both sectors contain valid committed records at the same generation.
    GenerationConflict,
    /// The current generation is `u64::MAX` and cannot be advanced safely.
    GenerationExhausted,
    /// Exact post-write or post-erase readback did not match the intended
    /// bytes.
    ReadbackMismatch {
        /// Sector whose operation failed verification.
        sector: BondStoreSector,
    },
    /// A completed operation did not remount as the exact intended successor.
    SuccessorVerificationFailed,
    /// Recovery was requested while a valid committed state still exists.
    CommittedStatePresent,
}

/// Failure separated into raw backend errors and stable store faults.
#[derive(Debug, Eq, PartialEq)]
pub enum BondStoreError<E> {
    /// Raw NOR backend failure. Completion may be ambiguous; remount before
    /// retrying.
    Backend(E),
    /// Stable non-secret format or state fault.
    Fault(BondStoreFault),
}

enum RecordValue {
    Bond(BleBond),
    Cleared,
}

struct ValidRecord {
    sector: BondStoreSector,
    generation: NonZeroU64,
    value: RecordValue,
}

enum SectorStatus {
    Erased,
    Valid(ValidRecord),
    Invalid,
}

/// Read and select the newest valid committed sector without modifying flash.
///
/// A corrupt or torn inactive sector never overrides a valid committed sector;
/// it is instead reported through [`MountedBondStore::cleanup`]. If neither
/// sector is valid, only exactly erased media mounts successfully.
pub fn mount<F>(flash: &mut F) -> Result<MountedBondStore, BondStoreError<F::Error>>
where
    F: ReadNorFlash,
{
    validate_read_geometry(flash).map_err(BondStoreError::Fault)?;
    let a = scan_sector(flash, BondStoreSector::A).map_err(BondStoreError::Backend)?;
    let b = scan_sector(flash, BondStoreSector::B).map_err(BondStoreError::Backend)?;
    select(a, b).map_err(BondStoreError::Fault)
}

/// Commit a replacement bond as the next generation.
///
/// The supplied owner is consumed. If an error is returned its secret material
/// is zeroized before return. On a backend error, completion is intentionally
/// treated as ambiguous and callers must [`mount`] before deciding whether to
/// retry.
pub fn commit_bond<F>(
    flash: &mut F,
    bond: BleBond,
) -> Result<MountedBondStore, BondStoreError<F::Error>>
where
    F: NorFlash,
{
    commit_value(flash, Some(&bond))?;
    let mounted = mount(flash)?;
    let Some(stored) = mounted.bond() else {
        return Err(BondStoreError::Fault(
            BondStoreFault::SuccessorVerificationFailed,
        ));
    };
    if !bond_matches(stored, &bond) {
        return Err(BondStoreError::Fault(
            BondStoreFault::SuccessorVerificationFailed,
        ));
    }
    Ok(mounted)
}

/// Commit an empty tombstone as the next generation.
///
/// The predecessor remains physically recoverable until [`cleanup`] succeeds,
/// but the newer tombstone is immediately authoritative after its commit
/// marker is verified.
pub fn clear<F>(flash: &mut F) -> Result<MountedBondStore, BondStoreError<F::Error>>
where
    F: NorFlash,
{
    commit_value(flash, None)?;
    let mounted = mount(flash)?;
    if mounted.bond().is_some() {
        return Err(BondStoreError::Fault(
            BondStoreFault::SuccessorVerificationFailed,
        ));
    }
    Ok(mounted)
}

/// Erase the currently non-authoritative sector, if any, and remount.
///
/// This is safe to retry after interruption. Cleanup is optional before
/// replacement because the next commit erases its inactive target first.
pub fn cleanup<F>(flash: &mut F) -> Result<MountedBondStore, BondStoreError<F::Error>>
where
    F: NorFlash,
{
    validate_write_geometry(flash).map_err(BondStoreError::Fault)?;
    let mounted = mount(flash)?;
    let BondStoreCleanup::EraseInactive { sector } = mounted.cleanup else {
        return Ok(mounted);
    };
    erase_verified(flash, sector)?;
    mount(flash)
}

/// Recover media that contains no valid committed state back to exactly erased
/// form.
///
/// This operation refuses to erase when either sector contains a valid commit.
/// It is safe to repeat after a power cut during either sector erase.
pub fn recover_empty<F>(flash: &mut F) -> Result<MountedBondStore, BondStoreError<F::Error>>
where
    F: NorFlash,
{
    validate_write_geometry(flash).map_err(BondStoreError::Fault)?;
    let a = scan_sector(flash, BondStoreSector::A).map_err(BondStoreError::Backend)?;
    let b = scan_sector(flash, BondStoreSector::B).map_err(BondStoreError::Backend)?;
    if matches!(a, SectorStatus::Valid(_)) || matches!(b, SectorStatus::Valid(_)) {
        return Err(BondStoreError::Fault(BondStoreFault::CommittedStatePresent));
    }
    if !matches!(a, SectorStatus::Erased) {
        erase_verified(flash, BondStoreSector::A)?;
    }
    if !matches!(b, SectorStatus::Erased) {
        erase_verified(flash, BondStoreSector::B)?;
    }
    mount(flash)
}

fn commit_value<F>(flash: &mut F, bond: Option<&BleBond>) -> Result<(), BondStoreError<F::Error>>
where
    F: NorFlash,
{
    validate_write_geometry(flash).map_err(BondStoreError::Fault)?;
    let current = mount(flash)?;
    let generation = match current.generation {
        Some(generation) => NonZeroU64::new(
            generation
                .get()
                .checked_add(1)
                .ok_or(BondStoreError::Fault(BondStoreFault::GenerationExhausted))?,
        )
        .expect("a checked successor of nonzero is nonzero"),
        None => NonZeroU64::new(1).expect("one is nonzero"),
    };
    let target = current
        .active_sector
        .map_or(BondStoreSector::A, BondStoreSector::other);
    let encoded = encode_record(target, generation, bond);
    erase_verified(flash, target)?;
    write_verified(flash, target, 0, &encoded[..PREFIX_SIZE])?;
    write_verified(
        flash,
        target,
        COMMIT_OFFSET,
        &encoded[COMMIT_OFFSET..RECORD_SIZE],
    )?;

    let remounted = mount(flash)?;
    if remounted.generation != Some(generation) || remounted.active_sector != Some(target) {
        return Err(BondStoreError::Fault(
            BondStoreFault::SuccessorVerificationFailed,
        ));
    }
    Ok(())
}

fn encode_record(
    sector: BondStoreSector,
    generation: NonZeroU64,
    bond: Option<&BleBond>,
) -> Zeroizing<[u8; RECORD_SIZE]> {
    let mut record = Zeroizing::new([0_u8; RECORD_SIZE]);
    record[..8].copy_from_slice(MAGIC);
    put_u16(&mut record[..], 8, PHYSICAL_FORMAT_VERSION);
    put_u16(&mut record[..], 10, SEMANTIC_FORMAT_VERSION);
    record[12] = sector.id();
    record[13] = if bond.is_some() {
        RECORD_KIND_BOND
    } else {
        RECORD_KIND_CLEARED
    };
    put_u64(&mut record[..], 16, generation.get());
    if let Some(bond) = bond {
        record[14] = REQUIRED_BOND_FLAGS
            | if bond.irk.is_some() {
                FLAG_IRK_PRESENT
            } else {
                0
            };
        record[24..30].copy_from_slice(&bond.address);
        if let Some(irk) = &bond.irk {
            record[32..48].copy_from_slice(irk);
        }
        record[48..64].copy_from_slice(&bond.ltk);
    }
    let digest = record_digest(&record[..PROTECTED_SIZE]);
    record[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE].copy_from_slice(&digest);
    record[COMMIT_OFFSET..RECORD_SIZE].copy_from_slice(&COMMIT_MARKER);
    record
}

fn scan_sector<F>(flash: &mut F, sector: BondStoreSector) -> Result<SectorStatus, F::Error>
where
    F: ReadNorFlash,
{
    let mut record = Zeroizing::new([0_u8; RECORD_SIZE]);
    flash.read(sector.offset() as u32, &mut record[..])?;
    let tail_erased = region_is_erased(
        flash,
        sector.offset() + RECORD_SIZE,
        SECTOR_SIZE - RECORD_SIZE,
    )?;
    let record_erased = record.iter().all(|byte| *byte == 0xff);
    if record_erased && tail_erased {
        return Ok(SectorStatus::Erased);
    }
    if !tail_erased || record[COMMIT_OFFSET..RECORD_SIZE] != COMMIT_MARKER {
        return Ok(SectorStatus::Invalid);
    }
    if &record[..8] != MAGIC
        || read_u16(&record[..], 8) != PHYSICAL_FORMAT_VERSION
        || read_u16(&record[..], 10) != SEMANTIC_FORMAT_VERSION
        || record[12] != sector.id()
        || record[15] != 0
        || record[30..32].iter().any(|byte| *byte != 0)
        || record[64..PROTECTED_SIZE].iter().any(|byte| *byte != 0)
        || record[DIGEST_OFFSET + DIGEST_SIZE..PREFIX_SIZE]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Ok(SectorStatus::Invalid);
    }
    let Some(generation) = NonZeroU64::new(read_u64(&record[..], 16)) else {
        return Ok(SectorStatus::Invalid);
    };
    let expected_digest = record_digest(&record[..PROTECTED_SIZE]);
    if record[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE] != expected_digest {
        return Ok(SectorStatus::Invalid);
    }

    let value = match record[13] {
        RECORD_KIND_BOND => {
            let flags = record[14];
            if flags != REQUIRED_BOND_FLAGS && flags != REQUIRED_BOND_FLAGS | FLAG_IRK_PRESENT {
                return Ok(SectorStatus::Invalid);
            }
            let mut bond = BleBond::new([0_u8; 6], None, [0_u8; 16]);
            bond.address.copy_from_slice(&record[24..30]);
            if flags & FLAG_IRK_PRESENT != 0 {
                bond.irk = Some([0_u8; 16]);
                bond.irk
                    .as_mut()
                    .expect("IRK was just installed")
                    .copy_from_slice(&record[32..48]);
            } else if record[32..48].iter().any(|byte| *byte != 0) {
                return Ok(SectorStatus::Invalid);
            }
            bond.ltk.copy_from_slice(&record[48..64]);
            RecordValue::Bond(bond)
        }
        RECORD_KIND_CLEARED => {
            if record[14] != 0 || record[24..64].iter().any(|byte| *byte != 0) {
                return Ok(SectorStatus::Invalid);
            }
            RecordValue::Cleared
        }
        _ => return Ok(SectorStatus::Invalid),
    };
    Ok(SectorStatus::Valid(ValidRecord {
        sector,
        generation,
        value,
    }))
}

fn select(a: SectorStatus, b: SectorStatus) -> Result<MountedBondStore, BondStoreFault> {
    match (a, b) {
        (SectorStatus::Erased, SectorStatus::Erased) => Ok(MountedBondStore {
            generation: None,
            active_sector: None,
            bond: None,
            cleanup: BondStoreCleanup::Clean,
        }),
        (SectorStatus::Valid(a), SectorStatus::Valid(b)) => {
            if a.generation == b.generation {
                return Err(BondStoreFault::GenerationConflict);
            }
            let (selected, stale) = if a.generation > b.generation {
                (a, b.sector)
            } else {
                (b, a.sector)
            };
            Ok(materialize(
                selected,
                BondStoreCleanup::EraseInactive { sector: stale },
            ))
        }
        (SectorStatus::Valid(valid), SectorStatus::Erased) => {
            Ok(materialize(valid, BondStoreCleanup::Clean))
        }
        (SectorStatus::Erased, SectorStatus::Valid(valid)) => {
            Ok(materialize(valid, BondStoreCleanup::Clean))
        }
        (SectorStatus::Valid(valid), SectorStatus::Invalid) => {
            let inactive = valid.sector.other();
            Ok(materialize(
                valid,
                BondStoreCleanup::EraseInactive { sector: inactive },
            ))
        }
        (SectorStatus::Invalid, SectorStatus::Valid(valid)) => {
            let inactive = valid.sector.other();
            Ok(materialize(
                valid,
                BondStoreCleanup::EraseInactive { sector: inactive },
            ))
        }
        (SectorStatus::Invalid, SectorStatus::Invalid)
        | (SectorStatus::Invalid, SectorStatus::Erased)
        | (SectorStatus::Erased, SectorStatus::Invalid) => Err(BondStoreFault::RecoveryRequired),
    }
}

fn materialize(valid: ValidRecord, cleanup: BondStoreCleanup) -> MountedBondStore {
    MountedBondStore {
        generation: Some(valid.generation),
        active_sector: Some(valid.sector),
        bond: match valid.value {
            RecordValue::Bond(bond) => Some(bond),
            RecordValue::Cleared => None,
        },
        cleanup,
    }
}

fn validate_read_geometry<F>(flash: &F) -> Result<(), BondStoreFault>
where
    F: ReadNorFlash,
{
    let read_size = F::READ_SIZE;
    if flash.capacity() != PARTITION_SIZE
        || read_size == 0
        || !INSPECTION_CHUNK_SIZE.is_multiple_of(read_size)
        || !RECORD_SIZE.is_multiple_of(read_size)
        || !SECTOR_SIZE.is_multiple_of(read_size)
    {
        return Err(BondStoreFault::IncompatibleFlash);
    }
    Ok(())
}

fn validate_write_geometry<F>(flash: &F) -> Result<(), BondStoreFault>
where
    F: NorFlash,
{
    validate_read_geometry(flash)?;
    let write_size = F::WRITE_SIZE;
    let erase_size = F::ERASE_SIZE;
    if write_size == 0
        || erase_size == 0
        || !PREFIX_SIZE.is_multiple_of(write_size)
        || !COMMIT_OFFSET.is_multiple_of(write_size)
        || !COMMIT_SIZE.is_multiple_of(write_size)
        || !SECTOR_SIZE.is_multiple_of(erase_size)
    {
        return Err(BondStoreFault::IncompatibleFlash);
    }
    Ok(())
}

fn write_verified<F>(
    flash: &mut F,
    sector: BondStoreSector,
    relative_offset: usize,
    bytes: &[u8],
) -> Result<(), BondStoreError<F::Error>>
where
    F: NorFlash,
{
    let offset = sector.offset() + relative_offset;
    flash
        .write(offset as u32, bytes)
        .map_err(BondStoreError::Backend)?;
    let mut readback = Zeroizing::new([0_u8; INSPECTION_CHUNK_SIZE]);
    for (index, expected) in bytes.chunks(INSPECTION_CHUNK_SIZE).enumerate() {
        let actual = &mut readback[..expected.len()];
        flash
            .read((offset + index * INSPECTION_CHUNK_SIZE) as u32, actual)
            .map_err(BondStoreError::Backend)?;
        if actual != expected {
            return Err(BondStoreError::Fault(BondStoreFault::ReadbackMismatch {
                sector,
            }));
        }
    }
    Ok(())
}

fn erase_verified<F>(flash: &mut F, sector: BondStoreSector) -> Result<(), BondStoreError<F::Error>>
where
    F: NorFlash,
{
    flash
        .erase(
            sector.offset() as u32,
            (sector.offset() + SECTOR_SIZE) as u32,
        )
        .map_err(BondStoreError::Backend)?;
    if !region_is_erased(flash, sector.offset(), SECTOR_SIZE).map_err(BondStoreError::Backend)? {
        return Err(BondStoreError::Fault(BondStoreFault::ReadbackMismatch {
            sector,
        }));
    }
    Ok(())
}

fn region_is_erased<F>(flash: &mut F, offset: usize, length: usize) -> Result<bool, F::Error>
where
    F: ReadNorFlash,
{
    let mut readback = Zeroizing::new([0_u8; INSPECTION_CHUNK_SIZE]);
    let mut cursor = 0_usize;
    while cursor < length {
        let chunk = core::cmp::min(INSPECTION_CHUNK_SIZE, length - cursor);
        flash.read((offset + cursor) as u32, &mut readback[..chunk])?;
        if readback[..chunk].iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        cursor += chunk;
    }
    Ok(true)
}

fn record_digest(protected: &[u8]) -> [u8; DIGEST_SIZE] {
    debug_assert_eq!(protected.len(), PROTECTED_SIZE);
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(protected);
    hasher.update(DIGEST_FLUSH_TRAILER);
    hasher.finalize().into()
}

fn bond_matches(left: &BleBond, right: &BleBond) -> bool {
    left.address == right.address
        && constant_time_optional_key_eq(left.irk.as_ref(), right.irk.as_ref())
        && constant_time_key_eq(&left.ltk, &right.ltk)
}

fn constant_time_optional_key_eq(left: Option<&[u8; 16]>, right: Option<&[u8; 16]>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => constant_time_key_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn constant_time_key_eq(left: &[u8; 16], right: &[u8; 16]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("fixed u16"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed u64"))
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
