//! Mirrored product-owned appliance settings.
//!
//! This record belongs to the generic product-state arena rather than PRNS or
//! any one Reticulum application. A label therefore identifies the physical
//! appliance in product UI without becoming LXMF or NomadNet announce data.

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use sha2::{Digest, Sha256};

/// Complete two-sector quota required by this format.
pub const APPLIANCE_SETTINGS_BYTES: usize = 2 * 4096;
/// Maximum UTF-8 appliance-label bytes retained by the product.
pub const APPLIANCE_LABEL_BYTES: usize = 32;

const SECTOR_SIZE: usize = 4096;
const RECORD_SIZE: usize = 256;
const MAGIC: [u8; 8] = *b"E290LBL1";
const FORMAT_VERSION: u32 = 1;
const VERSION_OFFSET: usize = 8;
const GENERATION_OFFSET: usize = 16;
const PREDECESSOR_GENERATION_OFFSET: usize = 24;
const PREDECESSOR_DIGEST_OFFSET: usize = 32;
const LABEL_LENGTH_OFFSET: usize = 64;
const LABEL_OFFSET: usize = 68;
const LABEL_END: usize = LABEL_OFFSET + APPLIANCE_LABEL_BYTES;
const DIGEST_OFFSET: usize = 192;
const DIGEST_SIZE: usize = 32;
const COMMIT_OFFSET: usize = 224;
const COMMIT_MARKER: [u8; 32] = [
    0xc7, 0x19, 0x4e, 0xa2, 0x53, 0xb8, 0x0d, 0x91, 0x6f, 0x35, 0xea, 0x48, 0x07, 0xdc, 0xb1, 0x62,
    0x8a, 0xf0, 0x2d, 0x75, 0x3c, 0x99, 0xe4, 0x16, 0xb7, 0x43, 0x58, 0xcd, 0x20, 0x6e, 0x81, 0x3a,
];
const ZERO_DIGEST: [u8; DIGEST_SIZE] = [0; DIGEST_SIZE];

const _: () = assert!(LABEL_END <= DIGEST_OFFSET);
const _: () = assert!(COMMIT_OFFSET + COMMIT_MARKER.len() == RECORD_SIZE);
const _: () = assert!(APPLIANCE_SETTINGS_BYTES == 2 * SECTOR_SIZE);

/// Validated owned appliance label.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ApplianceLabel {
    bytes: [u8; APPLIANCE_LABEL_BYTES],
    len: u8,
}

impl ApplianceLabel {
    /// Validate and copy one non-empty single-line UTF-8 label.
    pub fn new(label: &str) -> Result<Self, ApplianceSettingsFault> {
        if label.is_empty()
            || label.len() > APPLIANCE_LABEL_BYTES
            || label.chars().any(|character| {
                character.is_control() || matches!(character, '\u{2028}' | '\u{2029}')
            })
        {
            return Err(ApplianceSettingsFault::InvalidLabel);
        }
        let mut bytes = [0; APPLIANCE_LABEL_BYTES];
        bytes[..label.len()].copy_from_slice(label.as_bytes());
        Ok(Self {
            bytes,
            len: label.len() as u8,
        })
    }

    /// Borrow the validated label as UTF-8.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("ApplianceLabel is constructed from UTF-8")
    }
}

impl core::fmt::Debug for ApplianceLabel {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_tuple("ApplianceLabel")
            .field(&self.as_str())
            .finish()
    }
}

/// Mounted appliance-settings generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplianceSettings {
    generation: u64,
    active_sector: Option<u8>,
    digest: [u8; DIGEST_SIZE],
    predecessor_generation: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
    label: Option<ApplianceLabel>,
}

impl ApplianceSettings {
    const fn empty() -> Self {
        Self {
            generation: 0,
            active_sector: None,
            digest: ZERO_DIGEST,
            predecessor_generation: 0,
            predecessor_digest: ZERO_DIGEST,
            label: None,
        }
    }

    /// Monotonic durable revision, or zero on erased media.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Current optional appliance label.
    pub const fn label(&self) -> Option<ApplianceLabel> {
        self.label
    }
}

/// Result of a durable appliance-settings update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplianceSettingsCommit {
    /// The requested value was already current and flash was not changed.
    Unchanged {
        /// Current durable revision.
        revision: u64,
    },
    /// A successor generation committed and verified.
    Updated {
        /// New durable revision.
        revision: u64,
    },
}

/// Format or policy fault independent of the flash driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplianceSettingsFault {
    /// Flash geometry cannot represent the mirrored format.
    IncompatibleFlashGeometry,
    /// Programmed media has no unambiguous valid committed generation.
    Corrupt,
    /// The durable generation cannot advance without wrapping.
    GenerationExhausted,
    /// Written bytes did not read back exactly.
    Verification,
    /// The supplied label violates the bounded display-label contract.
    InvalidLabel,
    /// A prior ambiguous access disabled settings for this boot.
    Unavailable,
}

/// Appliance-settings access failure.
#[derive(Debug, Eq, PartialEq)]
pub enum ApplianceSettingsError<E> {
    /// Underlying flash-driver failure.
    Storage(E),
    /// Product format or policy fault.
    Fault(ApplianceSettingsFault),
}

/// Mount the newest valid settings generation without mutating flash.
pub fn mount_appliance_settings<F>(
    flash: &mut F,
) -> Result<ApplianceSettings, ApplianceSettingsError<F::Error>>
where
    F: ReadNorFlash,
{
    validate_read_geometry::<F>()?;
    if flash.capacity() < APPLIANCE_SETTINGS_BYTES {
        return Err(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::IncompatibleFlashGeometry,
        ));
    }
    select_generation(inspect_sector(flash, 0)?, inspect_sector(flash, 1)?)
}

/// Commit a replacement label to the inactive mirror and verify remount.
pub fn set_appliance_label<F>(
    flash: &mut F,
    current: &mut ApplianceSettings,
    label: Option<ApplianceLabel>,
) -> Result<ApplianceSettingsCommit, ApplianceSettingsError<F::Error>>
where
    F: NorFlash,
{
    validate_read_geometry::<F>()?;
    validate_write_geometry::<F>()?;
    if flash.capacity() < APPLIANCE_SETTINGS_BYTES {
        return Err(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::IncompatibleFlashGeometry,
        ));
    }
    if current.label == label {
        return Ok(ApplianceSettingsCommit::Unchanged {
            revision: current.generation,
        });
    }
    let generation = current
        .generation
        .checked_add(1)
        .ok_or(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::GenerationExhausted,
        ))?;
    let target = match current.active_sector {
        Some(0) => 1,
        Some(1) | None => 0,
        Some(_) => {
            return Err(ApplianceSettingsError::Fault(
                ApplianceSettingsFault::Corrupt,
            ));
        }
    };
    let record = encode_record(generation, current.generation, current.digest, label);
    let base = sector_offset(target);
    flash
        .erase(base, base + SECTOR_SIZE as u32)
        .map_err(ApplianceSettingsError::Storage)?;
    write_verified(flash, base, &record[..COMMIT_OFFSET])?;
    write_verified(flash, base + COMMIT_OFFSET as u32, &record[COMMIT_OFFSET..])?;

    let mounted = mount_appliance_settings(flash)?;
    if mounted.generation != generation
        || mounted.label != label
        || mounted.active_sector != Some(target)
    {
        return Err(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::Verification,
        ));
    }
    *current = mounted;
    Ok(ApplianceSettingsCommit::Updated {
        revision: generation,
    })
}

#[derive(Clone, Copy)]
enum SectorState {
    Erased,
    Incomplete,
    Valid(ApplianceSettings),
    Corrupt,
}

fn select_generation<E>(
    first: SectorState,
    second: SectorState,
) -> Result<ApplianceSettings, ApplianceSettingsError<E>> {
    use SectorState::{Corrupt, Erased, Incomplete, Valid};
    match (first, second) {
        (Erased, Erased) => Ok(ApplianceSettings::empty()),
        (Corrupt, _) | (_, Corrupt) => Err(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::Corrupt,
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
                return Err(ApplianceSettingsError::Fault(
                    ApplianceSettingsFault::Corrupt,
                ));
            };
            if newer.generation != older.generation.checked_add(1).unwrap_or(0)
                || (newer.predecessor_generation, newer.predecessor_digest)
                    != (older.generation, older.digest)
            {
                return Err(ApplianceSettingsError::Fault(
                    ApplianceSettingsFault::Corrupt,
                ));
            }
            Ok(newer)
        }
        (Erased | Incomplete, Erased | Incomplete) => Err(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::Corrupt,
        )),
    }
}

fn inspect_sector<F>(
    flash: &mut F,
    sector: u8,
) -> Result<SectorState, ApplianceSettingsError<F::Error>>
where
    F: ReadNorFlash,
{
    let base = sector_offset(sector);
    let mut record = [0xff; RECORD_SIZE];
    flash
        .read(base, &mut record)
        .map_err(ApplianceSettingsError::Storage)?;
    let mut erased = record.iter().all(|byte| *byte == 0xff);
    let mut chunk = [0xff; RECORD_SIZE];
    let mut offset = RECORD_SIZE;
    while offset < SECTOR_SIZE {
        flash
            .read(base + offset as u32, &mut chunk)
            .map_err(ApplianceSettingsError::Storage)?;
        erased &= chunk.iter().all(|byte| *byte == 0xff);
        offset += chunk.len();
    }
    if erased {
        return Ok(SectorState::Erased);
    }
    let commit = &record[COMMIT_OFFSET..];
    if commit != COMMIT_MARKER {
        return if monotonic_partial(commit, &COMMIT_MARKER) {
            Ok(SectorState::Incomplete)
        } else {
            Ok(SectorState::Corrupt)
        };
    }
    Ok(parse_record(&record, sector)
        .map(SectorState::Valid)
        .unwrap_or(SectorState::Corrupt))
}

fn parse_record(record: &[u8; RECORD_SIZE], sector: u8) -> Result<ApplianceSettings, ()> {
    if record[..MAGIC.len()] != MAGIC
        || read_u32(record, VERSION_OFFSET) != FORMAT_VERSION
        || record[12..GENERATION_OFFSET].iter().any(|byte| *byte != 0)
        || record[LABEL_END..DIGEST_OFFSET]
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
    let label_len = read_u32(record, LABEL_LENGTH_OFFSET) as usize;
    if label_len > APPLIANCE_LABEL_BYTES
        || record[LABEL_OFFSET + label_len..LABEL_END]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(());
    }
    let label = if label_len == 0 {
        None
    } else {
        let text = core::str::from_utf8(&record[LABEL_OFFSET..LABEL_OFFSET + label_len])
            .map_err(|_| ())?;
        Some(ApplianceLabel::new(text).map_err(|_| ())?)
    };
    let mut digest = [0; DIGEST_SIZE];
    digest.copy_from_slice(&record[DIGEST_OFFSET..COMMIT_OFFSET]);
    Ok(ApplianceSettings {
        generation,
        active_sector: Some(sector),
        digest,
        predecessor_generation,
        predecessor_digest,
        label,
    })
}

fn encode_record(
    generation: u64,
    predecessor_generation: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
    label: Option<ApplianceLabel>,
) -> [u8; RECORD_SIZE] {
    let mut record = [0; RECORD_SIZE];
    record[..MAGIC.len()].copy_from_slice(&MAGIC);
    put_u32(&mut record, VERSION_OFFSET, FORMAT_VERSION);
    put_u64(&mut record, GENERATION_OFFSET, generation);
    put_u64(
        &mut record,
        PREDECESSOR_GENERATION_OFFSET,
        predecessor_generation,
    );
    record[PREDECESSOR_DIGEST_OFFSET..PREDECESSOR_DIGEST_OFFSET + DIGEST_SIZE]
        .copy_from_slice(&predecessor_digest);
    if let Some(label) = label {
        put_u32(
            &mut record,
            LABEL_LENGTH_OFFSET,
            label.as_str().len() as u32,
        );
        record[LABEL_OFFSET..LABEL_OFFSET + label.as_str().len()]
            .copy_from_slice(label.as_str().as_bytes());
    }
    let digest = digest_record(&record);
    record[DIGEST_OFFSET..COMMIT_OFFSET].copy_from_slice(&digest);
    record[COMMIT_OFFSET..].copy_from_slice(&COMMIT_MARKER);
    record
}

fn digest_record(record: &[u8; RECORD_SIZE]) -> [u8; DIGEST_SIZE] {
    Sha256::digest(&record[..DIGEST_OFFSET]).into()
}

fn write_verified<F>(
    flash: &mut F,
    offset: u32,
    bytes: &[u8],
) -> Result<(), ApplianceSettingsError<F::Error>>
where
    F: NorFlash,
{
    flash
        .write(offset, bytes)
        .map_err(ApplianceSettingsError::Storage)?;
    let mut readback = [0; COMMIT_OFFSET];
    let candidate = &mut readback[..bytes.len()];
    flash
        .read(offset, candidate)
        .map_err(ApplianceSettingsError::Storage)?;
    if candidate != bytes {
        return Err(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::Verification,
        ));
    }
    Ok(())
}

fn validate_read_geometry<F>() -> Result<(), ApplianceSettingsError<F::Error>>
where
    F: ReadNorFlash,
{
    if F::READ_SIZE == 0 || !RECORD_SIZE.is_multiple_of(F::READ_SIZE) || F::READ_SIZE > RECORD_SIZE
    {
        return Err(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::IncompatibleFlashGeometry,
        ));
    }
    Ok(())
}

fn validate_write_geometry<F>() -> Result<(), ApplianceSettingsError<F::Error>>
where
    F: NorFlash,
{
    if F::WRITE_SIZE == 0
        || F::ERASE_SIZE == 0
        || !COMMIT_OFFSET.is_multiple_of(F::WRITE_SIZE)
        || !COMMIT_MARKER.len().is_multiple_of(F::WRITE_SIZE)
        || !SECTOR_SIZE.is_multiple_of(F::ERASE_SIZE)
    {
        return Err(ApplianceSettingsError::Fault(
            ApplianceSettingsFault::IncompatibleFlashGeometry,
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
        bytes: [u8; APPLIANCE_SETTINGS_BYTES],
    }

    impl FakeNor {
        fn erased() -> Self {
            Self {
                bytes: [0xff; APPLIANCE_SETTINGS_BYTES],
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
            let source = self
                .bytes
                .get(start..start + bytes.len())
                .ok_or(FakeError::Bounds)?;
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
            let target = self
                .bytes
                .get_mut(start..start + bytes.len())
                .ok_or(FakeError::Bounds)?;
            if target
                .iter()
                .zip(bytes)
                .any(|(current, next)| current & next != *next)
            {
                return Err(FakeError::Programming);
            }
            for (current, next) in target.iter_mut().zip(bytes) {
                *current &= *next;
            }
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            if !(from as usize).is_multiple_of(SECTOR_SIZE)
                || !(to as usize).is_multiple_of(SECTOR_SIZE)
            {
                return Err(FakeError::Alignment);
            }
            self.bytes
                .get_mut(from as usize..to as usize)
                .ok_or(FakeError::Bounds)?
                .fill(0xff);
            Ok(())
        }
    }

    #[test]
    fn erased_media_round_trips_set_clear_and_noop() {
        let mut flash = FakeNor::erased();
        let mut settings = mount_appliance_settings(&mut flash).unwrap();
        assert_eq!(settings.generation(), 0);
        assert_eq!(settings.label(), None);

        let north = ApplianceLabel::new("North relay").unwrap();
        assert_eq!(
            set_appliance_label(&mut flash, &mut settings, Some(north)).unwrap(),
            ApplianceSettingsCommit::Updated { revision: 1 }
        );
        assert_eq!(settings.label(), Some(north));
        assert_eq!(
            set_appliance_label(&mut flash, &mut settings, Some(north)).unwrap(),
            ApplianceSettingsCommit::Unchanged { revision: 1 }
        );
        assert_eq!(
            set_appliance_label(&mut flash, &mut settings, None).unwrap(),
            ApplianceSettingsCommit::Updated { revision: 2 }
        );
        assert_eq!(mount_appliance_settings(&mut flash).unwrap(), settings);
    }

    #[test]
    fn labels_are_bounded_utf8_and_single_line() {
        assert!(ApplianceLabel::new("").is_err());
        assert!(ApplianceLabel::new("line\nbreak").is_err());
        assert!(ApplianceLabel::new("line\u{2028}break").is_err());
        assert!(ApplianceLabel::new("line\u{2029}break").is_err());
        assert!(ApplianceLabel::new("123456789012345678901234567890123").is_err());
        assert_eq!(
            ApplianceLabel::new("Metalbeard").unwrap().as_str(),
            "Metalbeard"
        );
    }

    #[test]
    fn incomplete_successor_keeps_the_committed_predecessor() {
        let mut flash = FakeNor::erased();
        let mut settings = mount_appliance_settings(&mut flash).unwrap();
        let north = ApplianceLabel::new("North relay").unwrap();
        set_appliance_label(&mut flash, &mut settings, Some(north)).unwrap();

        let south = ApplianceLabel::new("South relay").unwrap();
        let successor = encode_record(
            settings.generation + 1,
            settings.generation,
            settings.digest,
            Some(south),
        );
        let successor_base = sector_offset(1) as usize;
        flash.bytes[successor_base..successor_base + COMMIT_OFFSET]
            .copy_from_slice(&successor[..COMMIT_OFFSET]);
        flash.bytes[successor_base + COMMIT_OFFSET..successor_base + COMMIT_OFFSET + 8]
            .copy_from_slice(&successor[COMMIT_OFFSET..COMMIT_OFFSET + 8]);

        let remounted = mount_appliance_settings(&mut flash).unwrap();
        assert_eq!(remounted.generation(), 1);
        assert_eq!(remounted.label(), Some(north));
    }

    #[test]
    fn committed_record_corruption_fails_closed() {
        let mut flash = FakeNor::erased();
        let mut settings = mount_appliance_settings(&mut flash).unwrap();
        set_appliance_label(
            &mut flash,
            &mut settings,
            Some(ApplianceLabel::new("North relay").unwrap()),
        )
        .unwrap();

        flash.bytes[LABEL_OFFSET] ^= 1;

        assert_eq!(
            mount_appliance_settings(&mut flash),
            Err(ApplianceSettingsError::Fault(
                ApplianceSettingsFault::Corrupt
            ))
        );
    }
}
