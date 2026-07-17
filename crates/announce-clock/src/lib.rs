//! Durable, restart-safe Reticulum announce-emission epochs.
//!
//! Reticulum embeds a five-byte emission time in each announce. A device that
//! keeps its identity across reboot must not restart that value at monotonic
//! uptime zero. This crate owns a deliberately small NOR format that reserves a
//! new 20-bit boot epoch before firmware can emit an announce. The caller then
//! combines that epoch with a per-boot 20-bit announce ordinal.
//!
//! The exact partition is two 4 KiB erase sectors. Each sector is an append log
//! of 32 fixed 128-byte records. A reservation returns only after its new epoch
//! has a committed, validated record in both sectors. Rotation always preserves
//! a valid high-water record in the other sector.
//!
//! Blank or torn-only media is accepted only through an explicit
//! [`FreshClockPolicy::AllowFirstProvision`] decision after a read-only preflight
//! proves that no committed identity exists. The clock reservation must precede
//! identity provisioning in that transaction. Loading an existing identity with
//! missing clock state must use [`FreshClockPolicy::Reject`] and fails without
//! mutation.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use embedded_storage::nor_flash::MultiwriteNorFlash;
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// Exact partition size required by format version 1.
pub const PARTITION_SIZE: usize = 0x2000;
/// Exact erase-sector size required by format version 1.
pub const SECTOR_SIZE: usize = 0x1000;
/// Size of one append record.
pub const SLOT_SIZE: usize = 128;
/// Number of append records in either sector.
pub const SLOTS_PER_SECTOR: usize = SECTOR_SIZE / SLOT_SIZE;
/// Number of persisted boot-epoch bits in the 40-bit announce time.
pub const BOOT_EPOCH_BITS: u32 = 20;
/// Number of volatile per-boot ordinal bits in the 40-bit announce time.
pub const ANNOUNCE_ORDINAL_BITS: u32 = 20;
/// Largest durable boot epoch representable in the upper 20 bits.
pub const MAX_BOOT_EPOCH: u32 = 0x000f_ffff;
/// Largest volatile announce ordinal representable in the lower 20 bits.
pub const MAX_ANNOUNCE_ORDINAL: u32 = 0x000f_ffff;
/// Largest complete Reticulum announce-emission time representable in 40 bits.
pub const MAX_ANNOUNCE_EMISSION_TIMESTAMP: u64 = 0x00ff_ffff_ffff;
/// Current physical record format version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;

const DOMAIN_CLAIM: &[u8; 16] = b"RNSANNOUNCECLK1!";
const HEADER_MAGIC: &[u8; 8] = b"RACLOCK1";
const RECORD_LENGTH: u16 = SLOT_SIZE as u16;
const HEADER_OFFSET: usize = 16;
const RESERVED_OFFSET: usize = 32;
const DIGEST_OFFSET: usize = 64;
const COMMIT_OFFSET: usize = 96;
const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/announce-clock/record/v1\0";

// SHA-256("reticulum-rs-firmware/announce-clock/commit/v1\0"). A fixed,
// irregular marker makes monotonic partial programming distinguishable from
// an impossible marker without interpreting an uncommitted record body.
const COMMIT_MARKER: [u8; 32] = [
    0x27, 0x5c, 0xeb, 0x6e, 0x23, 0x8f, 0x08, 0x30, 0x6a, 0x76, 0x86, 0x16, 0x43, 0x41, 0xee, 0x6a,
    0x10, 0x55, 0x38, 0x87, 0x4f, 0x07, 0xc4, 0x2f, 0xac, 0x5e, 0x0e, 0xec, 0x62, 0xe5, 0x50, 0xd1,
];

const _: () = assert!(PARTITION_SIZE == 2 * SECTOR_SIZE);
const _: () = assert!(SLOTS_PER_SECTOR == 32);
const _: () = assert!(COMMIT_OFFSET + COMMIT_MARKER.len() == SLOT_SIZE);

/// One of the two mirrored append-log sectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sector {
    /// Sector at partition offset `0x0000`.
    A,
    /// Sector at partition offset `0x1000`.
    B,
}

impl Sector {
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

/// A valid 20-bit boot epoch value for local Reticulum announces.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BootEpoch(u32);

impl BootEpoch {
    /// Construct a nonzero epoch if it fits the upper 20-bit field.
    ///
    /// This validates only the integer representation. Only an epoch returned
    /// by [`reserve_next_boot_epoch`] carries this crate's durability guarantee.
    pub const fn new(value: u32) -> Option<Self> {
        if value != 0 && value <= MAX_BOOT_EPOCH {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Return the nonzero 20-bit epoch value.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Combine this epoch with a per-boot ordinal into Reticulum's 40-bit time.
    ///
    /// The caller must allocate ordinals monotonically within this boot. The
    /// complete value is `(epoch << 20) | announce_ordinal` and therefore never
    /// exceeds `0xffffffffff` for an epoch produced by this crate.
    pub const fn timestamp(self, announce_ordinal: AnnounceOrdinal) -> AnnounceEmissionTimestamp {
        AnnounceEmissionTimestamp(
            ((self.0 as u64) << ANNOUNCE_ORDINAL_BITS) | announce_ordinal.0 as u64,
        )
    }
}

/// A volatile 20-bit announce ordinal allocated monotonically within one boot.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AnnounceOrdinal(u32);

impl AnnounceOrdinal {
    /// First ordinal available within a newly reserved boot epoch.
    pub const ZERO: Self = Self(0);

    /// Construct an ordinal if it fits the lower 20-bit field.
    pub const fn new(value: u32) -> Option<Self> {
        if value <= MAX_ANNOUNCE_ORDINAL {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Return the 20-bit ordinal value.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Complete 40-bit Reticulum announce-emission timestamp.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AnnounceEmissionTimestamp(u64);

impl AnnounceEmissionTimestamp {
    /// Return the timestamp value suitable for Rete announce creation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Why a sector had to be erased instead of appended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RotationReason {
    /// All 32 append slots were consumed by committed or recognizable torn records.
    Full,
    /// Unknown or committed-corrupt data required replacement from the other copy.
    Repair,
}

/// Physical update made to one mirrored sector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectorUpdate {
    /// The record was committed into an already erased slot.
    Appended {
        /// Zero-based physical slot within the sector.
        slot: u8,
    },
    /// The sector was safely erased and the record was committed in slot zero.
    Rotated {
        /// Condition that required rotation.
        reason: RotationReason,
    },
}

/// Diagnostic account of one successful mirrored reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReservationReport {
    previous_high_water: Option<BootEpoch>,
    sector_a: SectorUpdate,
    sector_b: SectorUpdate,
}

impl ReservationReport {
    /// Highest committed epoch observed before this reservation, if any.
    pub const fn previous_high_water(self) -> Option<BootEpoch> {
        self.previous_high_water
    }

    /// Update made to sector A.
    pub const fn sector_a(self) -> SectorUpdate {
        self.sector_a
    }

    /// Update made to sector B.
    pub const fn sector_b(self) -> SectorUpdate {
        self.sector_b
    }
}

/// A boot epoch durably committed in both sectors and its physical report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootEpochReservation {
    epoch: BootEpoch,
    report: ReservationReport,
}

impl BootEpochReservation {
    /// Newly reserved boot epoch.
    pub const fn epoch(self) -> BootEpoch {
        self.epoch
    }

    /// Physical append/rotation report.
    pub const fn report(self) -> ReservationReport {
        self.report
    }

    /// Split the reservation into its epoch and report.
    pub const fn into_parts(self) -> (BootEpoch, ReservationReport) {
        (self.epoch, self.report)
    }
}

/// Policy for clock media with no valid high-water record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshClockPolicy {
    /// Fail without mutation; use when a persistent identity already exists.
    Reject,
    /// Permit epoch one only after preflight proves no committed identity exists.
    AllowFirstProvision,
}

/// Protected field that failed validation despite an exact commit marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorruptionReason {
    /// The 16-byte domain claim differs.
    DomainClaim,
    /// Header magic, version, or encoded length differs.
    Header,
    /// Epoch is zero or exceeds the 20-bit format limit.
    Epoch,
    /// Reserved bytes are nonzero.
    Reserved,
    /// SHA-256 does not authenticate the protected prefix.
    Digest,
}

/// Failure to scan, repair, or reserve the next boot epoch.
#[derive(Debug, Eq, PartialEq)]
pub enum ReserveError<E> {
    /// Underlying NOR operation failed; completion may be ambiguous.
    Backend(E),
    /// Capacity or read/write/erase geometry is incompatible with format version 1.
    IncompatibleFlash,
    /// Blank or torn-only media was rejected because no durable high-water exists.
    MissingHighWater,
    /// No valid high-water copy exists and a slot contains unclaimed data.
    UnknownProgrammedData {
        /// Sector containing the first unknown slot.
        sector: Sector,
        /// Zero-based physical slot.
        slot: u8,
    },
    /// No valid high-water copy exists and an exactly committed record is corrupt.
    CommittedCorrupt {
        /// Sector containing the first corrupt committed slot.
        sector: Sector,
        /// Zero-based physical slot.
        slot: u8,
        /// Protected field that failed validation.
        reason: CorruptionReason,
    },
    /// The 20-bit epoch namespace is exhausted.
    EpochExhausted,
    /// Program or erase acknowledgement did not match exact readback.
    ReadbackMismatch {
        /// Sector being verified.
        sector: Sector,
        /// Slot being verified, or `None` for whole-sector erase verification.
        slot: Option<u8>,
    },
    /// No safe operation order could preserve the current high-water copy.
    HighWaterInvariant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotStatus {
    Erased,
    Torn,
    Valid(BootEpoch),
    Unknown,
    CommittedCorrupt(CorruptionReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Damage {
    Unknown { slot: u8 },
    CommittedCorrupt { slot: u8, reason: CorruptionReason },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SectorState {
    max_epoch: Option<BootEpoch>,
    first_erased: Option<u8>,
    damage: Option<Damage>,
}

impl SectorState {
    const fn needs_rotation(self) -> bool {
        self.damage.is_some() || self.first_erased.is_none()
    }

    const fn rotation_reason(self) -> RotationReason {
        if self.damage.is_some() {
            RotationReason::Repair
        } else {
            RotationReason::Full
        }
    }
}

/// Reserve and mirror the next durable announce-emission boot epoch.
///
/// Both sectors are scanned completely. The new value is one greater than the
/// largest valid record in either sector. An erased/torn-only fresh partition
/// starts at one only under [`FreshClockPolicy::AllowFirstProvision`]; persistent
/// identities must select [`FreshClockPolicy::Reject`]. Unknown or
/// committed-corrupt data fails closed when no valid high-water copy exists.
/// Once a valid copy exists, a damaged sector is safely rebuilt from the other
/// copy.
///
/// Backend errors are intentionally not reconciled inside this call. Retrying
/// rescans the media and reserves one greater than any record that may have
/// committed before an ambiguous error, so a returned epoch is never reused.
///
/// The caller must provide exclusive access to the complete 8 KiB partition.
pub fn reserve_next_boot_epoch<F>(
    flash: &mut F,
    fresh_policy: FreshClockPolicy,
) -> Result<BootEpochReservation, ReserveError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    validate_flash::<F>(flash)?;
    let initial_a = scan_sector(flash, Sector::A)?;
    let initial_b = scan_sector(flash, Sector::B)?;
    let previous = max_optional_epoch(initial_a.max_epoch, initial_b.max_epoch);

    if previous.is_none() {
        fail_without_high_water::<F::Error>(Sector::A, initial_a)?;
        fail_without_high_water::<F::Error>(Sector::B, initial_b)?;
        if fresh_policy == FreshClockPolicy::Reject {
            return Err(ReserveError::MissingHighWater);
        }
    }

    let next_value = match previous {
        Some(epoch) => epoch
            .get()
            .checked_add(1)
            .filter(|value| *value <= MAX_BOOT_EPOCH)
            .ok_or(ReserveError::EpochExhausted)?,
        None => 1,
    };
    let next = BootEpoch::new(next_value).ok_or(ReserveError::HighWaterInvariant)?;

    let first = select_first_sector(initial_a, initial_b, previous)?;
    let second = first.other();
    let first_state = state_for(first, initial_a, initial_b);
    let first_other = state_for(second, initial_a, initial_b);
    let first_erase_is_safe = previous
        .map(|high| first_other.max_epoch == Some(high))
        .unwrap_or(true);

    let mut update_a = None;
    let mut update_b = None;
    let first_update = ensure_epoch(flash, first, first_state, next, first_erase_is_safe)?;
    set_update(first, first_update, &mut update_a, &mut update_b);

    let first_after = scan_sector(flash, first)?;
    if first_after.max_epoch != Some(next) || first_after.damage.is_some() {
        return Err(ReserveError::HighWaterInvariant);
    }

    let second_state = scan_sector(flash, second)?;
    let second_update = ensure_epoch(flash, second, second_state, next, true)?;
    set_update(second, second_update, &mut update_a, &mut update_b);

    let final_a = scan_sector(flash, Sector::A)?;
    let final_b = scan_sector(flash, Sector::B)?;
    if final_a.max_epoch != Some(next)
        || final_b.max_epoch != Some(next)
        || final_a.damage.is_some()
        || final_b.damage.is_some()
    {
        return Err(ReserveError::HighWaterInvariant);
    }

    Ok(BootEpochReservation {
        epoch: next,
        report: ReservationReport {
            previous_high_water: previous,
            sector_a: update_a.ok_or(ReserveError::HighWaterInvariant)?,
            sector_b: update_b.ok_or(ReserveError::HighWaterInvariant)?,
        },
    })
}

fn select_first_sector<E>(
    a: SectorState,
    b: SectorState,
    high_water: Option<BootEpoch>,
) -> Result<Sector, ReserveError<E>> {
    if !a.needs_rotation() {
        return Ok(Sector::A);
    }
    if !b.needs_rotation() {
        return Ok(Sector::B);
    }
    match high_water {
        None => Ok(Sector::A),
        Some(high) if b.max_epoch == Some(high) => Ok(Sector::A),
        Some(high) if a.max_epoch == Some(high) => Ok(Sector::B),
        Some(_) => Err(ReserveError::HighWaterInvariant),
    }
}

fn state_for(sector: Sector, a: SectorState, b: SectorState) -> SectorState {
    match sector {
        Sector::A => a,
        Sector::B => b,
    }
}

fn set_update(
    sector: Sector,
    update: SectorUpdate,
    a: &mut Option<SectorUpdate>,
    b: &mut Option<SectorUpdate>,
) {
    match sector {
        Sector::A => *a = Some(update),
        Sector::B => *b = Some(update),
    }
}

fn ensure_epoch<F>(
    flash: &mut F,
    sector: Sector,
    state: SectorState,
    epoch: BootEpoch,
    erase_is_safe: bool,
) -> Result<SectorUpdate, ReserveError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    if state.needs_rotation() {
        if !erase_is_safe {
            return Err(ReserveError::HighWaterInvariant);
        }
        let reason = state.rotation_reason();
        erase_sector(flash, sector)?;
        commit_epoch(flash, sector, 0, epoch)?;
        Ok(SectorUpdate::Rotated { reason })
    } else {
        let slot = state.first_erased.ok_or(ReserveError::HighWaterInvariant)?;
        commit_epoch(flash, sector, slot, epoch)?;
        Ok(SectorUpdate::Appended { slot })
    }
}

fn fail_without_high_water<E>(sector: Sector, state: SectorState) -> Result<(), ReserveError<E>> {
    match state.damage {
        None => Ok(()),
        Some(Damage::Unknown { slot }) => Err(ReserveError::UnknownProgrammedData { sector, slot }),
        Some(Damage::CommittedCorrupt { slot, reason }) => Err(ReserveError::CommittedCorrupt {
            sector,
            slot,
            reason,
        }),
    }
}

fn scan_sector<F>(flash: &mut F, sector: Sector) -> Result<SectorState, ReserveError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    let mut state = SectorState {
        max_epoch: None,
        first_erased: None,
        damage: None,
    };
    let mut encoded = [0_u8; SLOT_SIZE];
    for slot in 0..SLOTS_PER_SECTOR {
        let offset = sector.offset() + slot * SLOT_SIZE;
        flash
            .read(offset as u32, &mut encoded)
            .map_err(ReserveError::Backend)?;
        match classify_slot(&encoded) {
            SlotStatus::Erased => {
                if state.first_erased.is_none() {
                    state.first_erased = Some(slot as u8);
                }
            }
            SlotStatus::Torn => {}
            SlotStatus::Valid(epoch) => {
                state.max_epoch = max_optional_epoch(state.max_epoch, Some(epoch));
            }
            SlotStatus::Unknown => {
                if state.damage.is_none() {
                    state.damage = Some(Damage::Unknown { slot: slot as u8 });
                }
            }
            SlotStatus::CommittedCorrupt(reason) => {
                if state.damage.is_none() {
                    state.damage = Some(Damage::CommittedCorrupt {
                        slot: slot as u8,
                        reason,
                    });
                }
            }
        }
    }
    Ok(state)
}

fn classify_slot(encoded: &[u8; SLOT_SIZE]) -> SlotStatus {
    if encoded.iter().all(|byte| *byte == 0xff) {
        return SlotStatus::Erased;
    }

    let commit = &encoded[COMMIT_OFFSET..SLOT_SIZE];
    if commit == COMMIT_MARKER {
        if &encoded[..DOMAIN_CLAIM.len()] != DOMAIN_CLAIM {
            return SlotStatus::CommittedCorrupt(CorruptionReason::DomainClaim);
        }
        if &encoded[HEADER_OFFSET..HEADER_OFFSET + HEADER_MAGIC.len()] != HEADER_MAGIC
            || read_u16(encoded, 24) != PHYSICAL_FORMAT_VERSION
            || read_u16(encoded, 26) != RECORD_LENGTH
        {
            return SlotStatus::CommittedCorrupt(CorruptionReason::Header);
        }
        let Some(epoch) = BootEpoch::new(read_u32(encoded, 28)) else {
            return SlotStatus::CommittedCorrupt(CorruptionReason::Epoch);
        };
        if encoded[RESERVED_OFFSET..DIGEST_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        {
            return SlotStatus::CommittedCorrupt(CorruptionReason::Reserved);
        }
        let expected = record_digest(&encoded[..DIGEST_OFFSET]);
        if encoded[DIGEST_OFFSET..COMMIT_OFFSET] != expected {
            return SlotStatus::CommittedCorrupt(CorruptionReason::Digest);
        }
        return SlotStatus::Valid(epoch);
    }

    if claim_compatible_torn(encoded) {
        SlotStatus::Torn
    } else {
        SlotStatus::Unknown
    }
}

fn claim_compatible_torn(encoded: &[u8; SLOT_SIZE]) -> bool {
    let claim_or_magic_programmed = encoded[..DOMAIN_CLAIM.len()]
        .iter()
        .chain(&encoded[HEADER_OFFSET..HEADER_OFFSET + HEADER_MAGIC.len()])
        .any(|byte| *byte != 0xff);
    if !claim_or_magic_programmed {
        return false;
    }

    partial_toward(&encoded[..DOMAIN_CLAIM.len()], DOMAIN_CLAIM)
        && partial_toward(
            &encoded[HEADER_OFFSET..HEADER_OFFSET + HEADER_MAGIC.len()],
            HEADER_MAGIC,
        )
        && partial_toward(&encoded[24..26], &PHYSICAL_FORMAT_VERSION.to_le_bytes())
        && partial_toward(&encoded[26..28], &RECORD_LENGTH.to_le_bytes())
        && partial_toward(&encoded[COMMIT_OFFSET..], &COMMIT_MARKER)
}

fn partial_toward(actual: &[u8], target: &[u8]) -> bool {
    actual
        .iter()
        .zip(target)
        .all(|(actual, target)| actual & target == *target)
}

fn commit_epoch<F>(
    flash: &mut F,
    sector: Sector,
    slot: u8,
    epoch: BootEpoch,
) -> Result<(), ReserveError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    let encoded = encode_record(epoch);
    let offset = sector.offset() + usize::from(slot) * SLOT_SIZE;
    flash
        .write(offset as u32, &encoded[..COMMIT_OFFSET])
        .map_err(ReserveError::Backend)?;

    let mut prefix = [0_u8; COMMIT_OFFSET];
    flash
        .read(offset as u32, &mut prefix)
        .map_err(ReserveError::Backend)?;
    if prefix != encoded[..COMMIT_OFFSET] {
        return Err(ReserveError::ReadbackMismatch {
            sector,
            slot: Some(slot),
        });
    }

    flash
        .write((offset + COMMIT_OFFSET) as u32, &encoded[COMMIT_OFFSET..])
        .map_err(ReserveError::Backend)?;
    let mut readback = [0_u8; SLOT_SIZE];
    flash
        .read(offset as u32, &mut readback)
        .map_err(ReserveError::Backend)?;
    if readback != encoded || classify_slot(&readback) != SlotStatus::Valid(epoch) {
        return Err(ReserveError::ReadbackMismatch {
            sector,
            slot: Some(slot),
        });
    }
    Ok(())
}

fn erase_sector<F>(flash: &mut F, sector: Sector) -> Result<(), ReserveError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    let offset = sector.offset();
    flash
        .erase(offset as u32, (offset + SECTOR_SIZE) as u32)
        .map_err(ReserveError::Backend)?;
    let mut slot = [0_u8; SLOT_SIZE];
    for index in 0..SLOTS_PER_SECTOR {
        flash
            .read((offset + index * SLOT_SIZE) as u32, &mut slot)
            .map_err(ReserveError::Backend)?;
        if slot.iter().any(|byte| *byte != 0xff) {
            return Err(ReserveError::ReadbackMismatch { sector, slot: None });
        }
    }
    Ok(())
}

fn encode_record(epoch: BootEpoch) -> [u8; SLOT_SIZE] {
    let mut encoded = [0xff_u8; SLOT_SIZE];
    encoded[..DOMAIN_CLAIM.len()].copy_from_slice(DOMAIN_CLAIM);
    encoded[HEADER_OFFSET..HEADER_OFFSET + HEADER_MAGIC.len()].copy_from_slice(HEADER_MAGIC);
    put_u16(&mut encoded, 24, PHYSICAL_FORMAT_VERSION);
    put_u16(&mut encoded, 26, RECORD_LENGTH);
    put_u32(&mut encoded, 28, epoch.get());
    encoded[RESERVED_OFFSET..DIGEST_OFFSET].fill(0);
    let digest = record_digest(&encoded[..DIGEST_OFFSET]);
    encoded[DIGEST_OFFSET..COMMIT_OFFSET].copy_from_slice(&digest);
    encoded[COMMIT_OFFSET..].copy_from_slice(&COMMIT_MARKER);
    encoded
}

fn record_digest(prefix: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(prefix);
    hasher.finalize().into()
}

fn validate_flash<F>(flash: &F) -> Result<(), ReserveError<F::Error>>
where
    F: MultiwriteNorFlash,
{
    if flash.capacity() != PARTITION_SIZE
        || F::READ_SIZE == 0
        || F::WRITE_SIZE == 0
        || F::ERASE_SIZE != SECTOR_SIZE
        || !SLOT_SIZE.is_multiple_of(F::READ_SIZE)
        || !COMMIT_OFFSET.is_multiple_of(F::READ_SIZE)
        || !SLOT_SIZE.is_multiple_of(F::WRITE_SIZE)
        || !COMMIT_OFFSET.is_multiple_of(F::WRITE_SIZE)
        || !(SLOT_SIZE - COMMIT_OFFSET).is_multiple_of(F::WRITE_SIZE)
    {
        return Err(ReserveError::IncompatibleFlash);
    }
    Ok(())
}

fn max_optional_epoch(left: Option<BootEpoch>, right: Option<BootEpoch>) -> Option<BootEpoch> {
    match (left, right) {
        (Some(left), Some(right)) => Some(if left >= right { left } else { right }),
        (Some(epoch), None) | (None, Some(epoch)) => Some(epoch),
        (None, None) => None,
    }
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

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
