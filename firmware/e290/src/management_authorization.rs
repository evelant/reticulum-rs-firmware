//! Product-owned management authorization and physical-presence policy.
//!
//! PRNS remains the live request gate. This module stores only the Reticulum
//! identity hashes that the product asks PRNS to admit, and decides when a
//! local button gesture permits one new durable enrollment. The format owns
//! two erase sectors inside the app-neutral `product_state` arena; it does not
//! create a board partition or encode any enabled-app combination.

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use sha2::{Digest, Sha256};

/// Maximum number of Reticulum identities authorized to manage one appliance.
pub const MANAGEMENT_IDENTITY_CAPACITY: usize = 8;
/// Bytes occupied by the mirrored allow-list quota.
pub const MANAGEMENT_AUTHORIZATION_BYTES: usize = 2 * SECTOR_SIZE;
/// Continuous active-low hold required after an observed release.
pub const ENROLLMENT_BUTTON_HOLD_MILLIS: u64 = 1_000;
/// Time during which one identified Link peer may enroll after the hold.
pub const ENROLLMENT_WINDOW_MILLIS: u64 = 60_000;

const SECTOR_SIZE: usize = 4096;
const SECTOR_COUNT: usize = 2;
const RECORD_SIZE: usize = 256;
const MAGIC: [u8; 8] = *b"E290MGT1";
const FORMAT_VERSION: u32 = 1;
const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 8;
const COUNT_OFFSET: usize = 12;
const GENERATION_OFFSET: usize = 16;
const PREDECESSOR_GENERATION_OFFSET: usize = 24;
const PREDECESSOR_DIGEST_OFFSET: usize = 32;
const IDENTITIES_OFFSET: usize = 64;
const IDENTITIES_END: usize =
    IDENTITIES_OFFSET + MANAGEMENT_IDENTITY_CAPACITY * ManagementIdentity::BYTES;
const DIGEST_OFFSET: usize = 192;
const DIGEST_SIZE: usize = 32;
const COMMIT_OFFSET: usize = DIGEST_OFFSET + DIGEST_SIZE;
const COMMIT_SIZE: usize = 32;
const ZERO_DIGEST: [u8; DIGEST_SIZE] = [0; DIGEST_SIZE];
const COMMIT_MARKER: [u8; COMMIT_SIZE] = [
    0x36, 0xa9, 0x51, 0x0c, 0xe2, 0x47, 0x8b, 0x5d, 0x19, 0xf0, 0x62, 0xad, 0x84, 0x33, 0xc7, 0x5a,
    0x91, 0x2e, 0x48, 0xb6, 0x07, 0xdc, 0x69, 0x14, 0xfa, 0x25, 0x80, 0x4d, 0xb3, 0x7e, 0x01, 0xcc,
];

const _: () = assert!(IDENTITIES_END <= DIGEST_OFFSET);
const _: () = assert!(COMMIT_OFFSET + COMMIT_SIZE == RECORD_SIZE);
const _: () = assert!(MANAGEMENT_AUTHORIZATION_BYTES == SECTOR_COUNT * SECTOR_SIZE);

/// Product representation of one Reticulum identity hash.
///
/// It is intentionally independent of PRNS types so persisted product data is
/// not coupled to one runtime implementation's Rust API.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ManagementIdentity([u8; Self::BYTES]);

impl ManagementIdentity {
    /// Reticulum identity-hash width.
    pub const BYTES: usize = 16;

    /// Construct an identity from its complete Reticulum hash.
    pub const fn new(bytes: [u8; Self::BYTES]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete Reticulum identity hash.
    pub const fn as_bytes(&self) -> &[u8; Self::BYTES] {
        &self.0
    }
}

const EMPTY_IDENTITY: ManagementIdentity = ManagementIdentity::new([0; ManagementIdentity::BYTES]);

/// Mounted durable identity set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementAllowlist {
    generation: u64,
    active_sector: Option<u8>,
    digest: [u8; DIGEST_SIZE],
    predecessor_generation: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
    identities: [ManagementIdentity; MANAGEMENT_IDENTITY_CAPACITY],
    count: u8,
}

impl ManagementAllowlist {
    const fn empty() -> Self {
        Self {
            generation: 0,
            active_sector: None,
            digest: ZERO_DIGEST,
            predecessor_generation: 0,
            predecessor_digest: ZERO_DIGEST,
            identities: [EMPTY_IDENTITY; MANAGEMENT_IDENTITY_CAPACITY],
            count: 0,
        }
    }

    /// Monotonic committed generation, or zero on exactly erased media.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of authorized identities.
    pub const fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the allow-list contains no identities.
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Iterate over the committed identity hashes.
    pub fn identities(&self) -> impl ExactSizeIterator<Item = ManagementIdentity> + '_ {
        self.identities[..self.len()].iter().copied()
    }

    /// Whether an identity is already authorized.
    pub fn contains(&self, identity: ManagementIdentity) -> bool {
        self.identities().any(|candidate| candidate == identity)
    }
}

/// Result of one durable enrollment attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnrollmentCommit {
    /// The identity was already durable, so flash was not mutated.
    AlreadyPresent,
    /// A new mirrored generation was committed and verified.
    Added {
        /// New durable generation.
        generation: u64,
        /// Number of authorized identities after the commit.
        identity_count: usize,
    },
}

/// A format or policy fault independent of the flash driver's error type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementAuthorizationFault {
    /// Flash read, write, or erase geometry cannot represent the format.
    IncompatibleFlashGeometry,
    /// Programmed media has no unambiguous valid committed generation.
    Corrupt,
    /// The fixed identity set is full.
    Full,
    /// The current generation cannot be advanced without wrapping.
    GenerationExhausted,
    /// Written bytes did not read back exactly.
    Verification,
    /// A prior mount or ambiguous mutation disabled authorization for this boot.
    Unavailable,
}

/// Durable allow-list access failure.
#[derive(Debug, Eq, PartialEq)]
pub enum ManagementAuthorizationError<E> {
    /// Underlying flash driver failure.
    Storage(E),
    /// Product format or capacity fault.
    Fault(ManagementAuthorizationFault),
}

/// Mount the newest valid allow-list generation without mutating flash.
///
/// Exactly erased media is the empty generation-zero list. An interrupted
/// write in the inactive sector falls back to the previous valid generation;
/// committed corruption fails closed.
pub fn mount_management_allowlist<F>(
    flash: &mut F,
) -> Result<ManagementAllowlist, ManagementAuthorizationError<F::Error>>
where
    F: ReadNorFlash,
{
    validate_read_geometry::<F>()?;
    if flash.capacity() < MANAGEMENT_AUTHORIZATION_BYTES {
        return Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::IncompatibleFlashGeometry,
        ));
    }
    let first = inspect_sector(flash, 0)?;
    let second = inspect_sector(flash, 1)?;
    select_generation(first, second)
}

/// Durably add one identity using the inactive mirror and a commit-last record.
///
/// The caller must stop trusting the in-memory list if this returns an error:
/// a flash failure after a write can be ambiguous until a clean remount.
pub fn enroll_management_identity<F>(
    flash: &mut F,
    current: &mut ManagementAllowlist,
    identity: ManagementIdentity,
) -> Result<EnrollmentCommit, ManagementAuthorizationError<F::Error>>
where
    F: NorFlash,
{
    validate_read_geometry::<F>()?;
    validate_write_geometry::<F>()?;
    if flash.capacity() < MANAGEMENT_AUTHORIZATION_BYTES {
        return Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::IncompatibleFlashGeometry,
        ));
    }
    if current.contains(identity) {
        return Ok(EnrollmentCommit::AlreadyPresent);
    }
    if current.len() == MANAGEMENT_IDENTITY_CAPACITY {
        return Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::Full,
        ));
    }
    let generation =
        current
            .generation
            .checked_add(1)
            .ok_or(ManagementAuthorizationError::Fault(
                ManagementAuthorizationFault::GenerationExhausted,
            ))?;
    let target = match current.active_sector {
        Some(0) => 1,
        Some(1) | None => 0,
        Some(_) => {
            return Err(ManagementAuthorizationError::Fault(
                ManagementAuthorizationFault::Corrupt,
            ));
        }
    };
    let mut identities = current.identities;
    identities[current.len()] = identity;
    let count = current.count + 1;
    let record = encode_record(
        generation,
        current.generation,
        current.digest,
        &identities,
        count,
    );
    let base = sector_offset(target);
    flash
        .erase(base, base + SECTOR_SIZE as u32)
        .map_err(ManagementAuthorizationError::Storage)?;
    write_verified(flash, base, &record[..COMMIT_OFFSET])?;
    write_verified(flash, base + COMMIT_OFFSET as u32, &record[COMMIT_OFFSET..])?;

    let mounted = mount_management_allowlist(flash)?;
    if mounted.generation != generation
        || mounted.count != count
        || !mounted.contains(identity)
        || mounted.active_sector != Some(target)
    {
        return Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::Verification,
        ));
    }
    *current = mounted;
    Ok(EnrollmentCommit::Added {
        generation,
        identity_count: current.len(),
    })
}

#[derive(Clone, Copy)]
#[allow(
    clippy::large_enum_variant,
    reason = "the bounded allow-list is intentionally read without heap allocation during boot"
)]
enum SectorState {
    Erased,
    Incomplete,
    Valid(ManagementAllowlist),
    Corrupt,
}

fn select_generation<E>(
    first: SectorState,
    second: SectorState,
) -> Result<ManagementAllowlist, ManagementAuthorizationError<E>> {
    use SectorState::{Corrupt, Erased, Incomplete, Valid};
    match (first, second) {
        (Erased, Erased) => Ok(ManagementAllowlist::empty()),
        (Corrupt, _) | (_, Corrupt) => Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::Corrupt,
        )),
        (Valid(current), Erased | Incomplete) | (Erased | Incomplete, Valid(current)) => {
            Ok(current)
        }
        (Valid(first), Valid(second)) => {
            let (newer, older) = if first.generation > second.generation {
                (first, second)
            } else if second.generation > first.generation {
                (second, first)
            } else {
                return Err(ManagementAuthorizationError::Fault(
                    ManagementAuthorizationFault::Corrupt,
                ));
            };
            if newer.generation != older.generation.checked_add(1).unwrap_or(0)
                || newer_predecessor(&newer) != (older.generation, older.digest)
            {
                return Err(ManagementAuthorizationError::Fault(
                    ManagementAuthorizationFault::Corrupt,
                ));
            }
            Ok(newer)
        }
        (Erased | Incomplete, Erased | Incomplete) => Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::Corrupt,
        )),
    }
}

fn inspect_sector<F>(
    flash: &mut F,
    sector: u8,
) -> Result<SectorState, ManagementAuthorizationError<F::Error>>
where
    F: ReadNorFlash,
{
    let base = sector_offset(sector);
    let mut record = [0xff; RECORD_SIZE];
    flash
        .read(base, &mut record)
        .map_err(ManagementAuthorizationError::Storage)?;
    let mut erased = record.iter().all(|byte| *byte == 0xff);
    let mut chunk = [0xff; RECORD_SIZE];
    let mut offset = RECORD_SIZE;
    while offset < SECTOR_SIZE {
        flash
            .read(base + offset as u32, &mut chunk)
            .map_err(ManagementAuthorizationError::Storage)?;
        erased &= chunk.iter().all(|byte| *byte == 0xff);
        offset += chunk.len();
    }
    if erased {
        return Ok(SectorState::Erased);
    }

    let commit = &record[COMMIT_OFFSET..RECORD_SIZE];
    if commit != COMMIT_MARKER {
        return if monotonic_partial(commit, &COMMIT_MARKER) {
            Ok(SectorState::Incomplete)
        } else {
            Ok(SectorState::Corrupt)
        };
    }
    parse_record(&record, sector)
        .map(SectorState::Valid)
        .or(Ok(SectorState::Corrupt))
}

fn parse_record(record: &[u8; RECORD_SIZE], sector: u8) -> Result<ManagementAllowlist, ()> {
    if record[MAGIC_OFFSET..VERSION_OFFSET] != MAGIC
        || read_u32(record, VERSION_OFFSET) != FORMAT_VERSION
        || record[IDENTITIES_END..DIGEST_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        || record[DIGEST_OFFSET..COMMIT_OFFSET] != digest_record(record)
    {
        return Err(());
    }
    let generation = read_u64(record, GENERATION_OFFSET);
    let predecessor_generation = read_u64(record, PREDECESSOR_GENERATION_OFFSET);
    let mut predecessor_digest = [0; DIGEST_SIZE];
    predecessor_digest.copy_from_slice(
        &record[PREDECESSOR_DIGEST_OFFSET..PREDECESSOR_DIGEST_OFFSET + DIGEST_SIZE],
    );
    if generation == 0
        || (generation == 1 && (predecessor_generation != 0 || predecessor_digest != ZERO_DIGEST))
        || (generation > 1
            && (predecessor_generation != generation - 1 || predecessor_digest == ZERO_DIGEST))
    {
        return Err(());
    }
    let count = read_u32(record, COUNT_OFFSET);
    if count as usize > MANAGEMENT_IDENTITY_CAPACITY {
        return Err(());
    }
    let mut identities = [EMPTY_IDENTITY; MANAGEMENT_IDENTITY_CAPACITY];
    for (index, slot) in identities.iter_mut().enumerate() {
        let start = IDENTITIES_OFFSET + index * ManagementIdentity::BYTES;
        let mut bytes = [0; ManagementIdentity::BYTES];
        bytes.copy_from_slice(&record[start..start + ManagementIdentity::BYTES]);
        *slot = ManagementIdentity::new(bytes);
        if index >= count as usize && bytes != [0; ManagementIdentity::BYTES] {
            return Err(());
        }
    }
    if identities[..count as usize]
        .iter()
        .enumerate()
        .any(|(index, identity)| identities[..index].contains(identity))
    {
        return Err(());
    }
    let mut digest = [0; DIGEST_SIZE];
    digest.copy_from_slice(&record[DIGEST_OFFSET..COMMIT_OFFSET]);
    Ok(ManagementAllowlist {
        generation,
        active_sector: Some(sector),
        digest,
        predecessor_generation,
        predecessor_digest,
        identities,
        count: count as u8,
    })
}

fn encode_record(
    generation: u64,
    predecessor_generation: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
    identities: &[ManagementIdentity; MANAGEMENT_IDENTITY_CAPACITY],
    count: u8,
) -> [u8; RECORD_SIZE] {
    let mut record = [0; RECORD_SIZE];
    record[MAGIC_OFFSET..VERSION_OFFSET].copy_from_slice(&MAGIC);
    put_u32(&mut record, VERSION_OFFSET, FORMAT_VERSION);
    put_u32(&mut record, COUNT_OFFSET, u32::from(count));
    put_u64(&mut record, GENERATION_OFFSET, generation);
    put_u64(
        &mut record,
        PREDECESSOR_GENERATION_OFFSET,
        predecessor_generation,
    );
    record[PREDECESSOR_DIGEST_OFFSET..PREDECESSOR_DIGEST_OFFSET + DIGEST_SIZE]
        .copy_from_slice(&predecessor_digest);
    for (index, identity) in identities.iter().take(count as usize).enumerate() {
        let start = IDENTITIES_OFFSET + index * ManagementIdentity::BYTES;
        record[start..start + ManagementIdentity::BYTES].copy_from_slice(identity.as_bytes());
    }
    let digest = digest_record(&record);
    record[DIGEST_OFFSET..COMMIT_OFFSET].copy_from_slice(&digest);
    record[COMMIT_OFFSET..].copy_from_slice(&COMMIT_MARKER);
    record
}

fn digest_record(record: &[u8; RECORD_SIZE]) -> [u8; DIGEST_SIZE] {
    Sha256::digest(&record[..DIGEST_OFFSET]).into()
}

fn newer_predecessor(list: &ManagementAllowlist) -> (u64, [u8; DIGEST_SIZE]) {
    (list.predecessor_generation, list.predecessor_digest)
}

fn write_verified<F>(
    flash: &mut F,
    offset: u32,
    bytes: &[u8],
) -> Result<(), ManagementAuthorizationError<F::Error>>
where
    F: NorFlash,
{
    flash
        .write(offset, bytes)
        .map_err(ManagementAuthorizationError::Storage)?;
    let mut readback = [0; COMMIT_OFFSET];
    let candidate = &mut readback[..bytes.len()];
    flash
        .read(offset, candidate)
        .map_err(ManagementAuthorizationError::Storage)?;
    if candidate != bytes {
        return Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::Verification,
        ));
    }
    Ok(())
}

fn validate_read_geometry<F>() -> Result<(), ManagementAuthorizationError<F::Error>>
where
    F: ReadNorFlash,
{
    if F::READ_SIZE == 0 || !RECORD_SIZE.is_multiple_of(F::READ_SIZE) || F::READ_SIZE > RECORD_SIZE
    {
        return Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::IncompatibleFlashGeometry,
        ));
    }
    Ok(())
}

fn validate_write_geometry<F>() -> Result<(), ManagementAuthorizationError<F::Error>>
where
    F: NorFlash,
{
    if F::WRITE_SIZE == 0
        || F::ERASE_SIZE == 0
        || !COMMIT_OFFSET.is_multiple_of(F::WRITE_SIZE)
        || !COMMIT_SIZE.is_multiple_of(F::WRITE_SIZE)
        || !SECTOR_SIZE.is_multiple_of(F::ERASE_SIZE)
    {
        return Err(ManagementAuthorizationError::Fault(
            ManagementAuthorizationFault::IncompatibleFlashGeometry,
        ));
    }
    Ok(())
}

const fn sector_offset(sector: u8) -> u32 {
    sector as u32 * SECTOR_SIZE as u32
}

fn monotonic_partial(candidate: &[u8], target: &[u8]) -> bool {
    candidate
        .iter()
        .zip(target)
        .all(|(candidate, target)| (*candidate & *target) == *target)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("fixed field"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().expect("fixed field"))
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
}

/// Pure release-then-hold policy for one-at-a-time enrollment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnrollmentWindow {
    released: bool,
    low_since: Option<u64>,
    open_until: Option<u64>,
}

impl EnrollmentWindow {
    /// Start closed. A high sample must be observed before a hold can qualify.
    pub const fn new() -> Self {
        Self {
            released: false,
            low_since: None,
            open_until: None,
        }
    }

    /// Observe the active-low button and report whether a new window opened.
    pub fn observe(&mut self, now_millis: u64, asserted_low: bool) -> bool {
        self.expire(now_millis);
        if self.open_until.is_some() {
            if !asserted_low {
                self.released = true;
            }
            return false;
        }
        if !asserted_low {
            self.released = true;
            self.low_since = None;
            return false;
        }
        if !self.released {
            return false;
        }
        let started = *self.low_since.get_or_insert(now_millis);
        if now_millis.saturating_sub(started) < ENROLLMENT_BUTTON_HOLD_MILLIS {
            return false;
        }
        self.open_until = now_millis.checked_add(ENROLLMENT_WINDOW_MILLIS);
        self.low_since = None;
        self.released = false;
        self.open_until.is_some()
    }

    /// Whether an enrollment is currently authorized.
    pub fn is_open(&mut self, now_millis: u64) -> bool {
        self.expire(now_millis);
        self.open_until.is_some()
    }

    /// Consume the current window after one durable enrollment.
    pub fn consume(&mut self, now_millis: u64) -> bool {
        if !self.is_open(now_millis) {
            return false;
        }
        self.open_until = None;
        self.low_since = None;
        self.released = false;
        true
    }

    fn expire(&mut self, now_millis: u64) {
        if self
            .open_until
            .is_some_and(|deadline| now_millis >= deadline)
        {
            self.open_until = None;
            self.low_since = None;
            self.released = false;
        }
    }
}

impl Default for EnrollmentWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use embedded_storage::nor_flash::{ErrorType, NorFlashError, NorFlashErrorKind, ReadNorFlash};

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FakeError {
        Bounds,
        Alignment,
        Programming,
    }

    impl NorFlashError for FakeError {
        fn kind(&self) -> NorFlashErrorKind {
            match self {
                Self::Bounds => NorFlashErrorKind::OutOfBounds,
                Self::Alignment => NorFlashErrorKind::NotAligned,
                Self::Programming => NorFlashErrorKind::Other,
            }
        }
    }

    struct FakeNor {
        bytes: [u8; MANAGEMENT_AUTHORIZATION_BYTES],
        writes: usize,
        erases: usize,
    }

    impl FakeNor {
        fn erased() -> Self {
            Self {
                bytes: [0xff; MANAGEMENT_AUTHORIZATION_BYTES],
                writes: 0,
                erases: 0,
            }
        }
    }

    impl ErrorType for FakeNor {
        type Error = FakeError;
    }

    impl ReadNorFlash for FakeNor {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            let end = start.checked_add(bytes.len()).ok_or(FakeError::Bounds)?;
            let source = self.bytes.get(start..end).ok_or(FakeError::Bounds)?;
            bytes.copy_from_slice(source);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }

    impl NorFlash for FakeNor {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = SECTOR_SIZE;

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            let end = start.checked_add(bytes.len()).ok_or(FakeError::Bounds)?;
            let destination = self.bytes.get_mut(start..end).ok_or(FakeError::Bounds)?;
            if destination
                .iter()
                .zip(bytes)
                .any(|(old, new)| old & new != *new)
            {
                return Err(FakeError::Programming);
            }
            for (old, new) in destination.iter_mut().zip(bytes) {
                *old &= *new;
            }
            self.writes += 1;
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            if !(from as usize).is_multiple_of(Self::ERASE_SIZE)
                || !(to as usize).is_multiple_of(Self::ERASE_SIZE)
            {
                return Err(FakeError::Alignment);
            }
            let bytes = self
                .bytes
                .get_mut(from as usize..to as usize)
                .ok_or(FakeError::Bounds)?;
            bytes.fill(0xff);
            self.erases += 1;
            Ok(())
        }
    }

    fn identity(byte: u8) -> ManagementIdentity {
        ManagementIdentity::new([byte; ManagementIdentity::BYTES])
    }

    #[test]
    fn erased_media_mounts_as_an_empty_list() {
        let mut flash = FakeNor::erased();
        let mounted = mount_management_allowlist(&mut flash).unwrap();
        assert_eq!(mounted.generation(), 0);
        assert!(mounted.is_empty());
        assert_eq!(flash.writes, 0);
        assert_eq!(flash.erases, 0);
    }

    #[test]
    fn add_is_durable_and_duplicate_is_idempotent() {
        let mut flash = FakeNor::erased();
        let mut mounted = mount_management_allowlist(&mut flash).unwrap();
        assert_eq!(
            enroll_management_identity(&mut flash, &mut mounted, identity(1)).unwrap(),
            EnrollmentCommit::Added {
                generation: 1,
                identity_count: 1,
            }
        );
        let writes = flash.writes;
        let erases = flash.erases;
        assert_eq!(
            enroll_management_identity(&mut flash, &mut mounted, identity(1)).unwrap(),
            EnrollmentCommit::AlreadyPresent
        );
        assert_eq!((flash.writes, flash.erases), (writes, erases));

        let remounted = mount_management_allowlist(&mut flash).unwrap();
        assert_eq!(remounted.generation(), 1);
        assert_eq!(
            remounted.identities().collect::<std::vec::Vec<_>>(),
            [identity(1)]
        );
    }

    #[test]
    fn fixed_set_fails_closed_when_full() {
        let mut flash = FakeNor::erased();
        let mut mounted = mount_management_allowlist(&mut flash).unwrap();
        for byte in 1..=MANAGEMENT_IDENTITY_CAPACITY as u8 {
            enroll_management_identity(&mut flash, &mut mounted, identity(byte)).unwrap();
        }
        assert_eq!(
            enroll_management_identity(&mut flash, &mut mounted, identity(0xfe)),
            Err(ManagementAuthorizationError::Fault(
                ManagementAuthorizationFault::Full
            ))
        );
    }

    #[test]
    fn torn_inactive_commit_falls_back_to_previous_generation() {
        let mut flash = FakeNor::erased();
        let mut mounted = mount_management_allowlist(&mut flash).unwrap();
        enroll_management_identity(&mut flash, &mut mounted, identity(1)).unwrap();
        enroll_management_identity(&mut flash, &mut mounted, identity(2)).unwrap();

        let mut successor = mounted.identities;
        successor[2] = identity(3);
        let record = encode_record(3, 2, mounted.digest, &successor, 3);
        flash.erase(0, SECTOR_SIZE as u32).unwrap();
        flash.write(0, &record[..COMMIT_OFFSET]).unwrap();
        flash
            .write(
                COMMIT_OFFSET as u32,
                &record[COMMIT_OFFSET..COMMIT_OFFSET + 7],
            )
            .unwrap();

        let recovered = mount_management_allowlist(&mut flash).unwrap();
        assert_eq!(recovered.generation(), 2);
        assert_eq!(recovered.len(), 2);
        assert!(!recovered.contains(identity(3)));
    }

    #[test]
    fn committed_corruption_fails_closed_even_with_an_older_mirror() {
        let mut flash = FakeNor::erased();
        let mut mounted = mount_management_allowlist(&mut flash).unwrap();
        enroll_management_identity(&mut flash, &mut mounted, identity(1)).unwrap();
        enroll_management_identity(&mut flash, &mut mounted, identity(2)).unwrap();
        flash.bytes[SECTOR_SIZE + DIGEST_OFFSET] ^= 0x01;
        assert_eq!(
            mount_management_allowlist(&mut flash),
            Err(ManagementAuthorizationError::Fault(
                ManagementAuthorizationFault::Corrupt
            ))
        );
    }

    #[test]
    fn physical_presence_requires_release_then_hold_and_is_single_use() {
        let mut window = EnrollmentWindow::new();
        assert!(!window.observe(0, true));
        assert!(!window.observe(10_000, true));
        assert!(!window.observe(10_001, false));
        assert!(!window.observe(10_002, true));
        assert!(!window.observe(11_001, true));
        assert!(window.observe(11_002, true));
        assert!(window.is_open(11_002 + ENROLLMENT_WINDOW_MILLIS - 1));
        assert!(window.consume(11_100));
        assert!(!window.is_open(11_101));
        assert!(!window.observe(20_000, true));
        assert!(!window.observe(20_001, false));
        assert!(!window.observe(20_002, true));
        assert!(window.observe(21_002, true));
        assert!(!window.is_open(21_002 + ENROLLMENT_WINDOW_MILLIS));
    }
}
