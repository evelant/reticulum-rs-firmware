//! Power-loss-safe immutable device-identity storage over raw NOR flash.
//!
//! The physical format occupies exactly two 4 KiB mirror sectors. Each sector
//! contains one canonical 256-byte record followed by an erased tail:
//!
//! ```text
//! 0x000..0x020  fixed claim marker
//! 0x020..0x040  versioned canonical header
//! 0x040..0x080  64-byte Reticulum combined private key
//! 0x080..0x0c0  zeroed reserved bytes
//! 0x0c0..0x0e0  domain-separated SHA-256 digest
//! 0x0e0..0x100  commit marker programmed last
//! 0x100..0x1000 erased tail
//! ```
//!
//! A complete scan precedes every boot decision. A sole valid mirror is
//! authoritative and may repair only its peer; it is never erased. Two valid
//! mirrors must contain the exact same private bytes. Unknown programmed data
//! without a valid mirror fails closed. The store performs no routine mutation
//! after both mirrors are committed.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use embedded_storage::nor_flash::{MultiwriteNorFlash, NorFlash, ReadNorFlash};
use rand_core::{CryptoRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[cfg(test)]
mod tests;

/// Exact byte length of one Reticulum combined X25519-plus-Ed25519 private key.
pub const PRIVATE_KEY_SIZE: usize = 64;
/// Exact raw-NOR partition capacity required by physical format version 1.
pub const PARTITION_SIZE: usize = 0x2000;
/// Exact erase-sector and mirror size required by physical format version 1.
pub const MIRROR_SECTOR_SIZE: usize = 0x1000;
/// Exact committed record size at the beginning of either mirror sector.
pub const RECORD_SIZE: usize = 0x100;
/// Current physical on-flash format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;

const CLAIM_OFFSET: usize = 0x000;
const CLAIM_SIZE: usize = 32;
const HEADER_OFFSET: usize = 0x020;
const HEADER_SIZE: usize = 32;
const PRIVATE_KEY_OFFSET: usize = 0x040;
const RESERVED_OFFSET: usize = 0x080;
const RESERVED_SIZE: usize = 64;
const DIGEST_OFFSET: usize = 0x0c0;
const DIGEST_SIZE: usize = 32;
const COMMIT_OFFSET: usize = 0x0e0;
const COMMIT_SIZE: usize = 32;
const BODY_WRITE_SIZE: usize = COMMIT_OFFSET - HEADER_OFFSET;
const TAIL_SCAN_CHUNK_SIZE: usize = 32;

const KEY_FORMAT_RNS_COMBINED_PRIVATE: u16 = 1;
const IDENTITY_GENERATION: u64 = 1;
const HEADER_MAGIC: &[u8; 8] = b"RTIDREC1";
const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-identity/record/v1\0";

const CLAIM_MARKER: [u8; CLAIM_SIZE] = [
    0x8d, 0x21, 0xe6, 0x4a, 0x93, 0x5c, 0x17, 0xb8, 0x2f, 0xc0, 0x69, 0xd4, 0x35, 0x7a, 0xa1, 0x0e,
    0xf2, 0x48, 0x9b, 0x63, 0x1d, 0xc7, 0x54, 0xae, 0x70, 0x06, 0xdb, 0x39, 0x85, 0xec, 0x12, 0x5f,
];
const COMMIT_MARKER: [u8; COMMIT_SIZE] = [
    0x47, 0xb2, 0x0c, 0xf1, 0x68, 0x9d, 0x35, 0xe4, 0x1a, 0x73, 0xc8, 0x56, 0xaf, 0x04, 0xdb, 0x91,
    0x2e, 0x65, 0xf9, 0x18, 0x83, 0x4c, 0xb7, 0x20, 0xda, 0x5b, 0x0f, 0xe6, 0x74, 0xa9, 0x31, 0xcd,
];

const _: () = assert!(CLAIM_OFFSET + CLAIM_SIZE == HEADER_OFFSET);
const _: () = assert!(HEADER_OFFSET + HEADER_SIZE == PRIVATE_KEY_OFFSET);
const _: () = assert!(PRIVATE_KEY_OFFSET + PRIVATE_KEY_SIZE == RESERVED_OFFSET);
const _: () = assert!(RESERVED_OFFSET + RESERVED_SIZE == DIGEST_OFFSET);
const _: () = assert!(DIGEST_OFFSET + DIGEST_SIZE == COMMIT_OFFSET);
const _: () = assert!(COMMIT_OFFSET + COMMIT_SIZE == RECORD_SIZE);
const _: () = assert!(PARTITION_SIZE == MIRROR_SECTOR_SIZE * 2);

/// One physical identity mirror sector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityMirror {
    /// First sector at partition-relative offset zero.
    A,
    /// Second sector at partition-relative offset 4 KiB.
    B,
}

impl IdentityMirror {
    const fn offset(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => MIRROR_SECTOR_SIZE,
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// How the durable identity became available during this boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityBootSource {
    /// Both exact mirrors were already valid; no mutation was attempted.
    Loaded,
    /// No committed identity existed, so qualified entropy created one.
    Provisioned,
    /// One pre-existing valid mirror was copied exactly into its peer.
    Repaired,
}

/// Durable mirror coverage established when boot returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityMirrorCoverage {
    /// Both mirrors contain the same completely validated identity.
    Redundant,
    /// One mirror remains valid after a peer repair could not be completed.
    Single,
}

/// Non-secret result of a read-only identity-partition inspection.
///
/// This value contains no private-key material. Inspection validates complete
/// committed records and compares mirror keys internally, then zeroizes those
/// temporary copies before returning only this state classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityPreflight {
    /// Neither mirror contains a committed identity; all programmed bytes are
    /// blank or recognizable commit-protocol remnants that provisioning owns.
    Vacant,
    /// At least one fully validated committed identity exists.
    Committed {
        /// Verified mirror coverage, without any identity bytes.
        coverage: IdentityMirrorCoverage,
    },
}

/// Non-secret result metadata for one successful boot load or provisioning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentityBootReport {
    source: IdentityBootSource,
    coverage: IdentityMirrorCoverage,
    repair_deferred: bool,
}

impl IdentityBootReport {
    /// How the durable identity became available during this boot.
    pub const fn source(self) -> IdentityBootSource {
        self.source
    }

    /// Number-of-mirrors class verified before returning.
    pub const fn coverage(self) -> IdentityMirrorCoverage {
        self.coverage
    }

    /// Whether one exact valid mirror remains but peer repair must be retried.
    pub const fn repair_deferred(self) -> bool {
        self.repair_deferred
    }
}

/// Zeroizing durable Reticulum private-key material reconstructed from flash.
///
/// This type intentionally implements neither `Clone`, `Copy`, nor `Debug`.
/// Callers may borrow the exact bytes long enough to construct the protocol
/// identity; dropping this owner zeroizes its retained copy.
///
/// ```compile_fail
/// use reticulum_e290_firmware::product_identity::DurableIdentityMaterial;
/// fn require_clone<T: Clone>() {}
/// require_clone::<DurableIdentityMaterial>();
/// ```
///
/// ```compile_fail
/// use reticulum_e290_firmware::product_identity::DurableIdentityMaterial;
/// fn require_copy<T: Copy>() {}
/// require_copy::<DurableIdentityMaterial>();
/// ```
///
/// ```compile_fail
/// use reticulum_e290_firmware::product_identity::DurableIdentityMaterial;
/// fn require_debug<T: core::fmt::Debug>() {}
/// require_debug::<DurableIdentityMaterial>();
/// ```
pub struct DurableIdentityMaterial(Zeroizing<[u8; PRIVATE_KEY_SIZE]>);

impl DurableIdentityMaterial {
    /// Borrow the exact persisted 64-byte combined private-key representation.
    pub fn as_bytes(&self) -> &[u8; PRIVATE_KEY_SIZE] {
        &self.0
    }
}

/// Failure to load, provision, reconcile, or repair the immutable identity.
#[derive(Debug)]
pub enum IdentityStoreError<E> {
    /// The raw NOR backend failed; completion of its latest operation may be ambiguous.
    Backend(E),
    /// Capacity or read/program/erase geometry does not match physical format version 1.
    IncompatibleFlash,
    /// Qualified entropy failed before any provisioning write was attempted.
    Entropy(rand_core::Error),
    /// No valid peer exists and this committed mirror fails canonical validation.
    CommittedCorrupt {
        /// Mirror containing the invalid committed record.
        mirror: IdentityMirror,
    },
    /// No valid peer exists and programmed bytes cannot be recognized as this format.
    UnknownProgrammedData {
        /// Mirror containing the unknown bytes.
        mirror: IdentityMirror,
    },
    /// Both mirrors are valid but contain different private-key bytes.
    MirrorConflict,
    /// An operation reported success but exact readback did not establish its record.
    ReadbackMismatch {
        /// Mirror whose exact readback failed.
        mirror: IdentityMirror,
    },
    /// Recovery ended without any completely committed identity.
    NoCommittedIdentity,
}

/// Inspect identity durability without programming or erasing flash.
///
/// This performs the same complete mirror and erased-tail validation used by
/// [`boot_load_or_provision`]. Blank and recognizable torn states report
/// [`IdentityPreflight::Vacant`]. One valid mirror is authoritative and reports
/// committed single-mirror coverage without attempting repair; two valid
/// mirrors must contain the same exact key. Unknown programmed data without a
/// valid peer, sole committed corruption, and mirror conflicts fail closed with
/// the same typed errors as boot loading.
///
/// The returned value contains no private bytes, and this function performs no
/// writes or erases. Its only flash requirement is read access with geometry
/// compatible with the physical format.
pub fn inspect_identity<F>(flash: &mut F) -> Result<IdentityPreflight, IdentityStoreError<F::Error>>
where
    F: ReadNorFlash,
{
    validate_read_flash(flash)?;
    match select(scan_both(flash)?).map_err(map_selection_error)? {
        Selection::Redundant(_) => Ok(IdentityPreflight::Committed {
            coverage: IdentityMirrorCoverage::Redundant,
        }),
        Selection::Single { .. } => Ok(IdentityPreflight::Committed {
            coverage: IdentityMirrorCoverage::Single,
        }),
        Selection::Vacant { .. } => Ok(IdentityPreflight::Vacant),
    }
}

/// Load, reconcile, or first-provision one immutable Reticulum device identity.
///
/// Both mirror sectors are completely scanned before the RNG or any mutation is
/// used. First provisioning obtains exactly 64 bytes from `rng`, clamps only
/// the generated X25519 scalar representation, commits mirror A, and copies
/// those exact bytes to mirror B. Existing and imported on-flash key bytes are
/// never normalized. Returned material is always reconstructed from a fully
/// committed, digest-validated flash record.
///
/// A sole valid mirror is sufficient authority to reproduce its exact bytes in
/// the other sector. If peer repair encounters an ambiguous backend failure,
/// the complete partition is rescanned; the valid mirror may be returned with
/// [`IdentityMirrorCoverage::Single`] and `repair_deferred=true`. The sole valid
/// sector is never erased.
pub fn boot_load_or_provision<F, R>(
    flash: &mut F,
    rng: &mut R,
) -> Result<(DurableIdentityMaterial, IdentityBootReport), IdentityStoreError<F::Error>>
where
    F: MultiwriteNorFlash,
    R: RngCore + CryptoRng,
{
    validate_flash::<F>(flash)?;
    match select(scan_both(flash)?).map_err(map_selection_error)? {
        Selection::Redundant(record) => Ok((
            record.into_material(),
            report(
                IdentityBootSource::Loaded,
                IdentityMirrorCoverage::Redundant,
                false,
            ),
        )),
        Selection::Single {
            authoritative,
            record,
            other,
        } => finish_single(flash, authoritative, record, other, false),
        Selection::Vacant { a, b } => {
            let mut generated = Zeroizing::new([0_u8; PRIVATE_KEY_SIZE]);
            rng.try_fill_bytes(&mut generated[..])
                .map_err(IdentityStoreError::Entropy)?;
            // Python Reticulum serializes newly generated X25519 material in
            // its clamped canonical representation. Imported/reloaded records
            // deliberately bypass this generation-only normalization.
            generated[0] &= 248;
            generated[31] &= 127;
            generated[31] |= 64;

            let primary_result = repair_mirror(flash, IdentityMirror::A, a, &generated);
            drop(b);
            let after_primary = select(scan_both(flash)?).map_err(map_selection_error)?;
            match after_primary {
                Selection::Redundant(record) => {
                    ensure_expected(&record, &generated)?;
                    Ok((
                        record.into_material(),
                        report(
                            IdentityBootSource::Provisioned,
                            IdentityMirrorCoverage::Redundant,
                            false,
                        ),
                    ))
                }
                Selection::Single {
                    authoritative,
                    record,
                    other,
                } => {
                    ensure_expected(&record, &generated)?;
                    finish_single(flash, authoritative, record, other, true)
                }
                Selection::Vacant { .. } => {
                    Err(primary_result
                        .err()
                        .unwrap_or(IdentityStoreError::ReadbackMismatch {
                            mirror: IdentityMirror::A,
                        }))
                }
            }
        }
    }
}

fn report(
    source: IdentityBootSource,
    coverage: IdentityMirrorCoverage,
    repair_deferred: bool,
) -> IdentityBootReport {
    IdentityBootReport {
        source,
        coverage,
        repair_deferred,
    }
}

fn finish_single<F>(
    flash: &mut F,
    authoritative: IdentityMirror,
    record: DecodedRecord,
    other: SectorState,
    provisioned: bool,
) -> Result<(DurableIdentityMaterial, IdentityBootReport), IdentityStoreError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    let repair_error = repair_mirror(flash, authoritative.other(), other, record.as_bytes()).err();
    let after_repair = select(scan_both(flash)?).map_err(map_selection_error)?;
    match after_repair {
        Selection::Redundant(reloaded) => {
            ensure_expected(&reloaded, record.as_bytes())?;
            Ok((
                reloaded.into_material(),
                report(
                    if provisioned {
                        IdentityBootSource::Provisioned
                    } else {
                        IdentityBootSource::Repaired
                    },
                    IdentityMirrorCoverage::Redundant,
                    false,
                ),
            ))
        }
        Selection::Single {
            record: reloaded, ..
        } => {
            ensure_expected(&reloaded, record.as_bytes())?;
            if repair_error.is_none() {
                return Err(IdentityStoreError::ReadbackMismatch {
                    mirror: authoritative.other(),
                });
            }
            Ok((
                reloaded.into_material(),
                report(
                    if provisioned {
                        IdentityBootSource::Provisioned
                    } else {
                        IdentityBootSource::Loaded
                    },
                    IdentityMirrorCoverage::Single,
                    true,
                ),
            ))
        }
        Selection::Vacant { .. } => {
            Err(repair_error.unwrap_or(IdentityStoreError::NoCommittedIdentity))
        }
    }
}

fn ensure_expected<E>(
    record: &DecodedRecord,
    expected: &[u8; PRIVATE_KEY_SIZE],
) -> Result<(), IdentityStoreError<E>> {
    if record.as_bytes() == expected {
        Ok(())
    } else {
        Err(IdentityStoreError::MirrorConflict)
    }
}

fn validate_flash<F>(flash: &F) -> Result<(), IdentityStoreError<F::Error>>
where
    F: NorFlash,
{
    validate_read_flash(flash)?;
    if F::WRITE_SIZE == 0
        || F::ERASE_SIZE != MIRROR_SECTOR_SIZE
        || !CLAIM_SIZE.is_multiple_of(F::WRITE_SIZE)
        || !BODY_WRITE_SIZE.is_multiple_of(F::WRITE_SIZE)
        || !COMMIT_SIZE.is_multiple_of(F::WRITE_SIZE)
    {
        return Err(IdentityStoreError::IncompatibleFlash);
    }
    Ok(())
}

fn validate_read_flash<F>(flash: &F) -> Result<(), IdentityStoreError<F::Error>>
where
    F: ReadNorFlash,
{
    if flash.capacity() != PARTITION_SIZE
        || F::READ_SIZE == 0
        || !MIRROR_SECTOR_SIZE.is_multiple_of(F::READ_SIZE)
        || !RECORD_SIZE.is_multiple_of(F::READ_SIZE)
        || !TAIL_SCAN_CHUNK_SIZE.is_multiple_of(F::READ_SIZE)
    {
        return Err(IdentityStoreError::IncompatibleFlash);
    }
    Ok(())
}

fn scan_both<F>(flash: &mut F) -> Result<[SectorState; 2], IdentityStoreError<F::Error>>
where
    F: ReadNorFlash,
{
    Ok([
        scan_mirror(flash, IdentityMirror::A)?,
        scan_mirror(flash, IdentityMirror::B)?,
    ])
}

fn scan_mirror<F>(
    flash: &mut F,
    mirror: IdentityMirror,
) -> Result<SectorState, IdentityStoreError<F::Error>>
where
    F: ReadNorFlash,
{
    let mut record = Zeroizing::new([0_u8; RECORD_SIZE]);
    flash
        .read(mirror.offset() as u32, &mut record[..])
        .map_err(IdentityStoreError::Backend)?;

    let mut tail_is_erased = true;
    let mut tail = Zeroizing::new([0_u8; TAIL_SCAN_CHUNK_SIZE]);
    let mut offset = RECORD_SIZE;
    while offset < MIRROR_SECTOR_SIZE {
        flash
            .read(
                (mirror.offset() + offset) as u32,
                &mut tail[..TAIL_SCAN_CHUNK_SIZE],
            )
            .map_err(IdentityStoreError::Backend)?;
        tail_is_erased &= tail.iter().all(|byte| *byte == 0xff);
        offset += TAIL_SCAN_CHUNK_SIZE;
    }

    let record_is_erased = record.iter().all(|byte| *byte == 0xff);
    if record_is_erased && tail_is_erased {
        return Ok(SectorState::Erased);
    }
    if !tail_is_erased {
        return Ok(SectorState::Unknown);
    }

    let claim: &[u8; CLAIM_SIZE] = record[CLAIM_OFFSET..HEADER_OFFSET]
        .try_into()
        .map_err(|_| IdentityStoreError::ReadbackMismatch { mirror })?;
    if claim != &CLAIM_MARKER {
        if monotonic_partial(claim, &CLAIM_MARKER)
            && record[HEADER_OFFSET..].iter().all(|byte| *byte == 0xff)
        {
            return Ok(SectorState::Torn);
        }
        return Ok(SectorState::Unknown);
    }

    let commit: &[u8; COMMIT_SIZE] = record[COMMIT_OFFSET..RECORD_SIZE]
        .try_into()
        .map_err(|_| IdentityStoreError::ReadbackMismatch { mirror })?;
    if commit == &COMMIT_MARKER {
        return Ok(match decode_record(&record[..]) {
            Some(record) => SectorState::Valid(record),
            None => SectorState::CommittedCorrupt,
        });
    }
    if monotonic_partial(commit, &COMMIT_MARKER) {
        return Ok(SectorState::Torn);
    }
    Ok(SectorState::Unknown)
}

fn repair_mirror<F>(
    flash: &mut F,
    mirror: IdentityMirror,
    state: SectorState,
    private_key: &[u8; PRIVATE_KEY_SIZE],
) -> Result<(), IdentityStoreError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    match state {
        SectorState::Erased => {}
        SectorState::Torn | SectorState::CommittedCorrupt | SectorState::Unknown => {
            erase_mirror(flash, mirror)?;
        }
        SectorState::Valid(existing) => {
            return ensure_expected(&existing, private_key);
        }
    }
    write_record(flash, mirror, private_key)
}

fn erase_mirror<F>(
    flash: &mut F,
    mirror: IdentityMirror,
) -> Result<(), IdentityStoreError<F::Error>>
where
    F: NorFlash,
{
    let start = mirror.offset();
    flash
        .erase(start as u32, (start + MIRROR_SECTOR_SIZE) as u32)
        .map_err(IdentityStoreError::Backend)?;
    match scan_mirror(flash, mirror)? {
        SectorState::Erased => Ok(()),
        _ => Err(IdentityStoreError::ReadbackMismatch { mirror }),
    }
}

fn write_record<F>(
    flash: &mut F,
    mirror: IdentityMirror,
    private_key: &[u8; PRIVATE_KEY_SIZE],
) -> Result<(), IdentityStoreError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    let encoded = encode_record(private_key);
    let base = mirror.offset();

    flash
        .write(base as u32, &encoded[CLAIM_OFFSET..HEADER_OFFSET])
        .map_err(IdentityStoreError::Backend)?;
    let mut claim = [0_u8; CLAIM_SIZE];
    flash
        .read(base as u32, &mut claim)
        .map_err(IdentityStoreError::Backend)?;
    if claim != encoded[CLAIM_OFFSET..HEADER_OFFSET] {
        return Err(IdentityStoreError::ReadbackMismatch { mirror });
    }

    flash
        .write(
            (base + HEADER_OFFSET) as u32,
            &encoded[HEADER_OFFSET..COMMIT_OFFSET],
        )
        .map_err(IdentityStoreError::Backend)?;
    let mut body = Zeroizing::new([0_u8; BODY_WRITE_SIZE]);
    flash
        .read((base + HEADER_OFFSET) as u32, &mut body[..])
        .map_err(IdentityStoreError::Backend)?;
    if body[..] != encoded[HEADER_OFFSET..COMMIT_OFFSET] {
        return Err(IdentityStoreError::ReadbackMismatch { mirror });
    }
    drop(body);

    flash
        .write(
            (base + COMMIT_OFFSET) as u32,
            &encoded[COMMIT_OFFSET..RECORD_SIZE],
        )
        .map_err(IdentityStoreError::Backend)?;
    drop(encoded);
    match scan_mirror(flash, mirror)? {
        SectorState::Valid(record) if record.as_bytes() == private_key => Ok(()),
        _ => Err(IdentityStoreError::ReadbackMismatch { mirror }),
    }
}

fn encode_record(private_key: &[u8; PRIVATE_KEY_SIZE]) -> Zeroizing<[u8; RECORD_SIZE]> {
    let mut encoded = Zeroizing::new([0xff_u8; RECORD_SIZE]);
    encoded[CLAIM_OFFSET..HEADER_OFFSET].copy_from_slice(&CLAIM_MARKER);
    encoded[HEADER_OFFSET..HEADER_OFFSET + 8].copy_from_slice(HEADER_MAGIC);
    put_u16(&mut encoded[..], HEADER_OFFSET + 8, PHYSICAL_FORMAT_VERSION);
    put_u16(
        &mut encoded[..],
        HEADER_OFFSET + 10,
        KEY_FORMAT_RNS_COMBINED_PRIVATE,
    );
    put_u16(&mut encoded[..], HEADER_OFFSET + 12, RECORD_SIZE as u16);
    put_u16(
        &mut encoded[..],
        HEADER_OFFSET + 14,
        PRIVATE_KEY_SIZE as u16,
    );
    put_u64(&mut encoded[..], HEADER_OFFSET + 16, IDENTITY_GENERATION);
    encoded[HEADER_OFFSET + 24..PRIVATE_KEY_OFFSET].fill(0);
    encoded[PRIVATE_KEY_OFFSET..RESERVED_OFFSET].copy_from_slice(private_key);
    encoded[RESERVED_OFFSET..DIGEST_OFFSET].fill(0);
    let digest = record_digest(&encoded[..]);
    encoded[DIGEST_OFFSET..COMMIT_OFFSET].copy_from_slice(&digest);
    encoded[COMMIT_OFFSET..RECORD_SIZE].copy_from_slice(&COMMIT_MARKER);
    encoded
}

fn decode_record(record: &[u8]) -> Option<DecodedRecord> {
    if record.len() != RECORD_SIZE
        || record[CLAIM_OFFSET..HEADER_OFFSET] != CLAIM_MARKER
        || &record[HEADER_OFFSET..HEADER_OFFSET + 8] != HEADER_MAGIC
        || read_u16(record, HEADER_OFFSET + 8)? != PHYSICAL_FORMAT_VERSION
        || read_u16(record, HEADER_OFFSET + 10)? != KEY_FORMAT_RNS_COMBINED_PRIVATE
        || read_u16(record, HEADER_OFFSET + 12)? != RECORD_SIZE as u16
        || read_u16(record, HEADER_OFFSET + 14)? != PRIVATE_KEY_SIZE as u16
        || read_u64(record, HEADER_OFFSET + 16)? != IDENTITY_GENERATION
        || record[HEADER_OFFSET + 24..PRIVATE_KEY_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        || record[RESERVED_OFFSET..DIGEST_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        || record[COMMIT_OFFSET..RECORD_SIZE] != COMMIT_MARKER
        || record[DIGEST_OFFSET..COMMIT_OFFSET] != record_digest(record)
    {
        return None;
    }
    let mut private_key = Zeroizing::new([0_u8; PRIVATE_KEY_SIZE]);
    private_key.copy_from_slice(&record[PRIVATE_KEY_OFFSET..RESERVED_OFFSET]);
    Some(DecodedRecord { private_key })
}

fn record_digest(record: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(&record[..DIGEST_OFFSET]);
    hasher.finalize().into()
}

fn monotonic_partial(actual: &[u8], expected: &[u8]) -> bool {
    actual != expected
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| (*actual & *expected) == *expected)
}

fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(input: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        input.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u64(input: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        input.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

struct DecodedRecord {
    private_key: Zeroizing<[u8; PRIVATE_KEY_SIZE]>,
}

impl DecodedRecord {
    fn as_bytes(&self) -> &[u8; PRIVATE_KEY_SIZE] {
        &self.private_key
    }

    fn into_material(self) -> DurableIdentityMaterial {
        DurableIdentityMaterial(self.private_key)
    }
}

enum SectorState {
    Erased,
    Torn,
    Valid(DecodedRecord),
    CommittedCorrupt,
    Unknown,
}

enum Selection {
    Redundant(DecodedRecord),
    Single {
        authoritative: IdentityMirror,
        record: DecodedRecord,
        other: SectorState,
    },
    Vacant {
        a: SectorState,
        b: SectorState,
    },
}

enum SelectionError {
    CommittedCorrupt(IdentityMirror),
    Unknown(IdentityMirror),
    Conflict,
}

fn select([a, b]: [SectorState; 2]) -> Result<Selection, SelectionError> {
    match (a, b) {
        (SectorState::Valid(a), SectorState::Valid(b)) => {
            if a.as_bytes() == b.as_bytes() {
                Ok(Selection::Redundant(a))
            } else {
                Err(SelectionError::Conflict)
            }
        }
        (SectorState::Valid(record), other) => Ok(Selection::Single {
            authoritative: IdentityMirror::A,
            record,
            other,
        }),
        (other, SectorState::Valid(record)) => Ok(Selection::Single {
            authoritative: IdentityMirror::B,
            record,
            other,
        }),
        (a, b) => {
            if matches!(a, SectorState::CommittedCorrupt) {
                return Err(SelectionError::CommittedCorrupt(IdentityMirror::A));
            }
            if matches!(b, SectorState::CommittedCorrupt) {
                return Err(SelectionError::CommittedCorrupt(IdentityMirror::B));
            }
            if matches!(a, SectorState::Unknown) {
                return Err(SelectionError::Unknown(IdentityMirror::A));
            }
            if matches!(b, SectorState::Unknown) {
                return Err(SelectionError::Unknown(IdentityMirror::B));
            }
            Ok(Selection::Vacant { a, b })
        }
    }
}

fn map_selection_error<E>(error: SelectionError) -> IdentityStoreError<E> {
    match error {
        SelectionError::CommittedCorrupt(mirror) => IdentityStoreError::CommittedCorrupt { mirror },
        SelectionError::Unknown(mirror) => IdentityStoreError::UnknownProgrammedData { mirror },
        SelectionError::Conflict => IdentityStoreError::MirrorConflict,
    }
}
