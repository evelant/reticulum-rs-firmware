//! Power-loss-safe raw-NOR storage for the device API credential authority.
//!
//! The format owns exactly two 4 KiB erase sectors. Each sector contains one
//! complete 2 KiB semantic authority image, a domain-separated digest, an
//! irregular commit marker programmed last, and a separate retirement marker.
//! Successors commit in the inactive sector, retire their predecessor, and only
//! then become publishable to live request service. Mount remains read-only and
//! reports any retirement or erase cleanup explicitly.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::result_large_err,
    reason = "no-alloc ownership errors intentionally retain secret-bearing authorities for exact retry"
)]

use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use reticulum_device_api_credentials::{
    AuthorityRevision, CREDENTIAL_SNAPSHOT_IMAGE_SIZE, CREDENTIAL_SNAPSHOT_SLOT_SIZE,
    CredentialAuthority, CredentialAuthorityBuilder, CredentialSnapshotImage,
    CredentialSuccessorFaultKind, E290_CREDENTIAL_RECORD_CAPACITY, PlannedCredentialSuccessor,
    decode_e290_credential_snapshot, encode_e290_credential_snapshot,
};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[cfg(test)]
mod tests;

/// Exact raw-NOR partition length required by physical layout version 1.
pub const PARTITION_SIZE: usize = 8_192;
/// Exact size of either alternating snapshot sector.
pub const SECTOR_SIZE: usize = 4_096;
/// Current physical layout version.
pub const PHYSICAL_FORMAT_VERSION: u16 = 1;
/// Current credential snapshot semantic version.
pub const SEMANTIC_FORMAT_VERSION: u16 = 1;
/// Fixed credential capacity encoded by this store.
pub const RECORD_CAPACITY: usize = E290_CREDENTIAL_RECORD_CAPACITY;

/// Offset of the secret-bearing semantic image within one sector.
pub const SNAPSHOT_IMAGE_OFFSET: usize = 128;
/// End of the secret-bearing image and protected digest input.
pub const SNAPSHOT_DIGEST_OFFSET: usize = 2_176;
/// Offset of the committed-snapshot SHA-256 digest.
pub const COMMIT_DIGEST_OFFSET: usize = SNAPSHOT_DIGEST_OFFSET;
/// Offset of the commit marker programmed after exact prefix readback.
pub const COMMIT_MARKER_OFFSET: usize = 2_208;
/// Offset of the predecessor-retirement marker.
pub const RETIREMENT_MARKER_OFFSET: usize = 2_240;
/// First byte that must remain erased in a complete sector.
pub const ERASED_TAIL_OFFSET: usize = 2_272;

const HEADER_SIZE: usize = SNAPSHOT_IMAGE_OFFSET;
const DIGEST_SIZE: usize = 32;
const MARKER_SIZE: usize = 32;
const SNAPSHOT_PREFIX_SIZE: usize = SNAPSHOT_DIGEST_OFFSET;
const MAGIC: &[u8; 8] = b"RDAUTH01";
const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api-credential-store/snapshot/v1\0";
const SHA256_BLOCK_SIZE: usize = 64;
// `sha2` 0.10 does not clear its internal block buffer. The fixed prefix
// leaves 62 secret-bearing bytes buffered, so one exact public block is
// appended: its first two bytes compress that secret tail and its remaining
// 62 bytes overwrite the buffer before finalization.
const DIGEST_FLUSH_TRAILER: [u8; SHA256_BLOCK_SIZE] = [0xa5; SHA256_BLOCK_SIZE];

// SHA-256-derived irregular values are intentionally unlike erased or zeroed
// media. Their exact origin is not a security boundary; the protected digest
// and predecessor link provide integrity and trajectory authority.
const COMMIT_MARKER: [u8; MARKER_SIZE] = [
    0xa3, 0x51, 0x0e, 0xd8, 0x76, 0x2b, 0xc4, 0x19, 0x5f, 0x90, 0x37, 0xea, 0x6d, 0x04, 0xb2, 0x8c,
    0x41, 0xf7, 0x2d, 0x63, 0x98, 0x15, 0xce, 0x70, 0x0b, 0xd4, 0x56, 0xa9, 0x32, 0xef, 0x84, 0x1a,
];
const RETIREMENT_MARKER: [u8; MARKER_SIZE] = [
    0x6e, 0x13, 0xc9, 0x42, 0xaf, 0x75, 0x08, 0xe1, 0x34, 0x9b, 0x5d, 0xf0, 0x27, 0x86, 0xbc, 0x4a,
    0xd5, 0x01, 0x79, 0x3e, 0x92, 0x68, 0x14, 0xcb, 0xf6, 0x2c, 0x83, 0x50, 0xad, 0x07, 0xe9, 0x31,
];

const ZERO_DIGEST: [u8; DIGEST_SIZE] = [0; DIGEST_SIZE];

const _: () = assert!(PARTITION_SIZE == 2 * SECTOR_SIZE);
const _: () = assert!(HEADER_SIZE + CREDENTIAL_SNAPSHOT_IMAGE_SIZE == SNAPSHOT_DIGEST_OFFSET);
const _: () = assert!(CREDENTIAL_SNAPSHOT_IMAGE_SIZE == 2_048);
const _: () = assert!(CREDENTIAL_SNAPSHOT_SLOT_SIZE == 128);
const _: () = assert!(RECORD_CAPACITY == 16);
const _: () = assert!(COMMIT_DIGEST_OFFSET + DIGEST_SIZE == COMMIT_MARKER_OFFSET);
const _: () = assert!(COMMIT_MARKER_OFFSET + MARKER_SIZE == RETIREMENT_MARKER_OFFSET);
const _: () = assert!(RETIREMENT_MARKER_OFFSET + MARKER_SIZE == ERASED_TAIL_OFFSET);
const _: () = assert!(DIGEST_DOMAIN.len() == 62);
const _: () = assert!((DIGEST_DOMAIN.len() + SNAPSHOT_PREFIX_SIZE) % SHA256_BLOCK_SIZE == 62);
const _: () = assert!(DIGEST_FLUSH_TRAILER.len() == SHA256_BLOCK_SIZE);

/// Stable physical identity of the flash device containing credential state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CredentialStoreDeviceId([u8; 16]);

impl CredentialStoreDeviceId {
    /// Construct a device identity from exact product-supplied bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the exact device identity bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Exact physical provenance and layout represented by one backend view.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CredentialStoreBinding {
    device: CredentialStoreDeviceId,
    absolute_offset: usize,
    length: usize,
    layout_version: u16,
}

impl CredentialStoreBinding {
    /// Construct one operation-scoped store binding.
    pub const fn new(
        device: CredentialStoreDeviceId,
        absolute_offset: usize,
        length: usize,
        layout_version: u16,
    ) -> Self {
        Self {
            device,
            absolute_offset,
            length,
            layout_version,
        }
    }

    /// Physical device containing this store.
    pub const fn device(self) -> CredentialStoreDeviceId {
        self.device
    }

    /// Absolute offset in the containing physical device.
    pub const fn absolute_offset(self) -> usize {
        self.absolute_offset
    }

    /// Exact bound range length.
    pub const fn length(self) -> usize {
        self.length
    }

    /// Expected physical layout version.
    pub const fn layout_version(self) -> u16 {
        self.layout_version
    }
}

/// Binding validation failure detected before any store I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialStoreBindingError {
    /// A later operation names a different physical device.
    DeviceMismatch {
        /// Device retained by mounted state.
        expected: CredentialStoreDeviceId,
        /// Device supplied by the later view.
        actual: CredentialStoreDeviceId,
    },
    /// A later operation names a different absolute range.
    RangeMismatch {
        /// Retained absolute start.
        expected_offset: usize,
        /// Retained length.
        expected_length: usize,
        /// Supplied absolute start.
        actual_offset: usize,
        /// Supplied length.
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
    LayoutVersionMismatch {
        /// Required version.
        expected: u16,
        /// Supplied version.
        actual: u16,
    },
    /// The bound range is not exactly 8 KiB.
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
    /// The range or fixed operations violate backend granularity.
    AlignmentMismatch {
        /// Absolute range start.
        absolute_offset: usize,
        /// Range length.
        length: usize,
        /// Backend read granularity.
        read_size: usize,
        /// Backend program granularity.
        write_size: usize,
        /// Backend erase granularity.
        erase_size: usize,
    },
}

/// A credential-store-relative backend carrying physical provenance.
pub struct BoundCredentialStore<F> {
    backend: F,
    binding: CredentialStoreBinding,
}

impl<F> BoundCredentialStore<F> {
    /// Attach binding provenance to one range-restricted backend.
    pub const fn new(backend: F, binding: CredentialStoreBinding) -> Self {
        Self { backend, binding }
    }

    /// Binding carried by this view.
    pub const fn binding(&self) -> CredentialStoreBinding {
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

impl<F: ErrorType> ErrorType for BoundCredentialStore<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for BoundCredentialStore<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.backend.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.backend.capacity()
    }
}

impl<F: NorFlash> NorFlash for BoundCredentialStore<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.backend.write(offset, bytes)
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.backend.erase(from, to)
    }
}

impl<F: MultiwriteNorFlash> MultiwriteNorFlash for BoundCredentialStore<F> {}

/// NOR access that proves the exact credential-store device and range.
pub trait BoundCredentialStoreAccess: MultiwriteNorFlash {
    /// Binding represented by this operation-scoped view.
    fn credential_store_binding(&self) -> CredentialStoreBinding;
}

impl<F: MultiwriteNorFlash> BoundCredentialStoreAccess for BoundCredentialStore<F> {
    fn credential_store_binding(&self) -> CredentialStoreBinding {
        self.binding
    }
}

/// One of the two alternating full-snapshot sectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialStoreSector {
    /// Sector at partition-relative offset zero.
    A,
    /// Sector at partition-relative offset 4096.
    B,
}

impl CredentialStoreSector {
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

/// Recovery required before another credential mutation may begin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialStoreRecovery {
    /// Active authority is clean and the inactive sector is erased.
    Clean,
    /// The newer snapshot is authoritative but its predecessor must be retired.
    RetirePredecessor {
        /// Older sector whose retirement marker must complete.
        sector: CredentialStoreSector,
    },
    /// A non-authoritative inactive sector must be erased.
    CleanupInactive {
        /// Sector safe to erase.
        sector: CredentialStoreSector,
    },
}

/// Stable non-secret fault classification from media inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialStoreFault {
    /// Both sectors are fully erased.
    UnformattedErased,
    /// No committed authority exists and programmed bytes remain.
    UnformattedNonErased,
    /// A committed snapshot used an unsupported physical version.
    UnsupportedPhysicalVersion(u16),
    /// A committed snapshot used an unsupported semantic version.
    UnsupportedSemanticVersion(u16),
    /// A committed header names a different device.
    DeviceBindingMismatch {
        /// Sector containing the mismatched header.
        sector: CredentialStoreSector,
    },
    /// A committed snapshot failed canonical validation.
    CommittedSnapshotCorrupt {
        /// Sector containing corruption.
        sector: CredentialStoreSector,
    },
    /// Programmed bytes or markers are not on a recognized trajectory.
    UnknownProgrammedData {
        /// Sector containing unknown data.
        sector: CredentialStoreSector,
    },
    /// Two committed snapshots cannot belong to one linear authority history.
    SnapshotConflict,
    /// A retired authority exists without its committed successor.
    RetiredWithoutSuccessor,
    /// A requested operation is incompatible with current recovery state.
    RecoveryRequired,
    /// An exact readback did not establish the intended bytes.
    ReadbackMismatch {
        /// Sector whose operation could not be established.
        sector: CredentialStoreSector,
    },
}

/// Mount failure, separated into pre-I/O binding, backend, and media faults.
pub enum CredentialStoreMountError<E> {
    /// Binding rejected before I/O.
    Binding(CredentialStoreBindingError),
    /// Raw NOR backend failure.
    Backend(E),
    /// Stable media classification.
    Fault(CredentialStoreFault),
}

/// Successfully decoded authority plus its exact physical ownership state.
#[must_use = "mounted credential ownership must be recovered or deliberately published"]
pub struct MountedCredentialStore {
    binding: CredentialStoreBinding,
    active_sector: CredentialStoreSector,
    committed_digest: [u8; DIGEST_SIZE],
    authority: CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>,
    recovery: CredentialStoreRecovery,
}

impl MountedCredentialStore {
    /// Exact physical binding retained at mount.
    pub const fn binding(&self) -> CredentialStoreBinding {
        self.binding
    }

    /// Sector containing the authoritative committed snapshot.
    pub const fn active_sector(&self) -> CredentialStoreSector {
        self.active_sector
    }

    /// Current authority revision.
    pub const fn revision(&self) -> AuthorityRevision {
        self.authority.revision()
    }

    /// Borrow the authority only after predecessor retirement is established.
    ///
    /// `None` forces boot integration to finish an explicit
    /// [`CredentialStoreRecovery::RetirePredecessor`] before request service can
    /// observe the newer authority. Erase-only inactive cleanup does not block
    /// publication.
    pub const fn publishable_authority(
        &self,
    ) -> Option<&CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>> {
        match self.recovery {
            CredentialStoreRecovery::RetirePredecessor { .. } => None,
            CredentialStoreRecovery::Clean | CredentialStoreRecovery::CleanupInactive { .. } => {
                Some(&self.authority)
            }
        }
    }

    /// Required physical recovery, if any.
    pub const fn recovery(&self) -> CredentialStoreRecovery {
        self.recovery
    }

    /// Consume mounted ownership into the live semantic authority.
    ///
    /// Product integration remains responsible for scheduling any reported
    /// cleanup before admitting another credential mutation.
    pub fn into_authority(
        self,
    ) -> Result<CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>, Self> {
        if matches!(
            self.recovery,
            CredentialStoreRecovery::RetirePredecessor { .. }
        ) {
            Err(self)
        } else {
            Ok(self.authority)
        }
    }
}

struct ValidSnapshot {
    sector: CredentialStoreSector,
    record_count: u8,
    revision: u64,
    predecessor_revision: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
    digest: [u8; DIGEST_SIZE],
    image: CredentialSnapshotImage,
}

enum SectorStatus {
    Erased,
    Uncommitted(CredentialStoreSector),
    Active(ValidSnapshot),
    Retiring(ValidSnapshot),
    Retired(ValidSnapshot),
    RetirementDebris {
        sector: CredentialStoreSector,
        observed_digest: [u8; DIGEST_SIZE],
    },
    Invalid {
        sector: CredentialStoreSector,
        kind: InvalidSectorKind,
    },
}

#[derive(Clone, Copy)]
enum InvalidSectorKind {
    UnsupportedPhysical(u16),
    UnsupportedSemantic(u16),
    WrongDevice,
    CommittedInvalid,
    Unknown,
}

struct SelectedSnapshot {
    snapshot: ValidSnapshot,
    recovery: CredentialStoreRecovery,
}

/// Read and select the credential authority without programming or erasing.
pub fn mount<A>(
    access: &mut A,
) -> Result<MountedCredentialStore, CredentialStoreMountError<A::Error>>
where
    A: BoundCredentialStoreAccess,
{
    validate_binding(access).map_err(CredentialStoreMountError::Binding)?;
    let binding = access.credential_store_binding();
    let a = scan_sector(access, binding, CredentialStoreSector::A)
        .map_err(CredentialStoreMountError::Backend)?;
    let b = scan_sector(access, binding, CredentialStoreSector::B)
        .map_err(CredentialStoreMountError::Backend)?;
    let selected = select(a, b).map_err(CredentialStoreMountError::Fault)?;
    materialize(binding, selected).map_err(CredentialStoreMountError::Fault)
}

fn materialize(
    binding: CredentialStoreBinding,
    selected: SelectedSnapshot,
) -> Result<MountedCredentialStore, CredentialStoreFault> {
    let snapshot = selected.snapshot;
    let authority =
        decode_e290_credential_snapshot(AuthorityRevision::new(snapshot.revision), snapshot.image)
            .map_err(|_| CredentialStoreFault::CommittedSnapshotCorrupt {
                sector: snapshot.sector,
            })?;
    if authority.record_count() != usize::from(snapshot.record_count) {
        return Err(CredentialStoreFault::CommittedSnapshotCorrupt {
            sector: snapshot.sector,
        });
    }
    Ok(MountedCredentialStore {
        binding,
        active_sector: snapshot.sector,
        committed_digest: snapshot.digest,
        authority,
        recovery: selected.recovery,
    })
}

fn scan_sector<A: ReadNorFlash>(
    access: &mut A,
    binding: CredentialStoreBinding,
    sector: CredentialStoreSector,
) -> Result<SectorStatus, A::Error> {
    let base = sector.offset();
    let mut header = [0_u8; HEADER_SIZE];
    let mut image = CredentialSnapshotImage::zeroed();
    let mut raw_digest = [0_u8; DIGEST_SIZE];
    let mut commit = [0_u8; MARKER_SIZE];
    let mut retirement = [0_u8; MARKER_SIZE];
    access.read(base as u32, &mut header)?;
    access.read((base + SNAPSHOT_IMAGE_OFFSET) as u32, image.as_mut_bytes())?;
    access.read((base + COMMIT_DIGEST_OFFSET) as u32, &mut raw_digest)?;
    access.read((base + COMMIT_MARKER_OFFSET) as u32, &mut commit)?;
    access.read((base + RETIREMENT_MARKER_OFFSET) as u32, &mut retirement)?;
    let tail_erased = region_is_erased(
        access,
        base + ERASED_TAIL_OFFSET,
        SECTOR_SIZE - ERASED_TAIL_OFFSET,
    )?;
    let complete_erased = header.iter().all(|byte| *byte == 0xff)
        && image.as_bytes().iter().all(|byte| *byte == 0xff)
        && raw_digest.iter().all(|byte| *byte == 0xff)
        && commit.iter().all(|byte| *byte == 0xff)
        && retirement.iter().all(|byte| *byte == 0xff)
        && tail_erased;
    if complete_erased {
        return Ok(SectorStatus::Erased);
    }
    if !tail_erased {
        return Ok(invalid_status(sector, InvalidSectorKind::Unknown));
    }

    let retirement_state = if retirement == RETIREMENT_MARKER {
        2_u8
    } else if retirement.iter().all(|byte| *byte == 0xff) {
        0_u8
    } else if monotonic_programming_compatible(&retirement, &RETIREMENT_MARKER) {
        1_u8
    } else {
        return Ok(invalid_status(sector, InvalidSectorKind::Unknown));
    };

    if commit != COMMIT_MARKER {
        if monotonic_programming_compatible(&commit, &COMMIT_MARKER) {
            if retirement_state == 0 {
                return Ok(SectorStatus::Uncommitted(sector));
            }
            return Ok(SectorStatus::RetirementDebris {
                sector,
                observed_digest: raw_digest,
            });
        }
        return Ok(invalid_status(sector, InvalidSectorKind::Unknown));
    }

    if &header[..8] != MAGIC {
        return if retirement_state == 0 {
            Ok(invalid_status(sector, InvalidSectorKind::CommittedInvalid))
        } else {
            Ok(SectorStatus::RetirementDebris {
                sector,
                observed_digest: raw_digest,
            })
        };
    }
    let physical = read_u16(&header, 8);
    if physical != PHYSICAL_FORMAT_VERSION {
        return Ok(invalid_status(
            sector,
            InvalidSectorKind::UnsupportedPhysical(physical),
        ));
    }
    let semantic = read_u16(&header, 10);
    if semantic != SEMANTIC_FORMAT_VERSION {
        return Ok(invalid_status(
            sector,
            InvalidSectorKind::UnsupportedSemantic(semantic),
        ));
    }
    if header[12] != sector.id()
        || header[14..16].iter().any(|byte| *byte != 0)
        || header[96..HEADER_SIZE].iter().any(|byte| *byte != 0)
        || read_u32(&header, 80) != RECORD_CAPACITY as u32
        || read_u32(&header, 84) != CREDENTIAL_SNAPSHOT_SLOT_SIZE as u32
        || read_u64(&header, 88) != CREDENTIAL_SNAPSHOT_IMAGE_SIZE as u64
    {
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedInvalid));
    }
    if header[64..80] != binding.device.0 {
        return Ok(invalid_status(sector, InvalidSectorKind::WrongDevice));
    }
    let record_count = header[13];
    let revision = read_u64(&header, 16);
    let predecessor_revision = read_u64(&header, 24);
    let mut predecessor_digest = [0_u8; DIGEST_SIZE];
    predecessor_digest.copy_from_slice(&header[32..64]);
    if revision == 0
        || usize::from(record_count) > RECORD_CAPACITY
        || (revision == 1 && (predecessor_revision != 0 || predecessor_digest != ZERO_DIGEST))
        || (revision > 1
            && (predecessor_revision != revision - 1 || predecessor_digest == ZERO_DIGEST))
    {
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedInvalid));
    }
    let expected_digest = snapshot_digest_parts(&header, image.as_bytes());
    if raw_digest != expected_digest {
        if retirement_state != 0 {
            return Ok(SectorStatus::RetirementDebris {
                sector,
                observed_digest: raw_digest,
            });
        }
        return Ok(invalid_status(sector, InvalidSectorKind::CommittedInvalid));
    }
    let snapshot = ValidSnapshot {
        sector,
        record_count,
        revision,
        predecessor_revision,
        predecessor_digest,
        digest: expected_digest,
        image,
    };
    Ok(match retirement_state {
        0 => SectorStatus::Active(snapshot),
        1 => SectorStatus::Retiring(snapshot),
        _ => SectorStatus::Retired(snapshot),
    })
}

const fn invalid_status(sector: CredentialStoreSector, kind: InvalidSectorKind) -> SectorStatus {
    SectorStatus::Invalid { sector, kind }
}

fn select(a: SectorStatus, b: SectorStatus) -> Result<SelectedSnapshot, CredentialStoreFault> {
    match (a, b) {
        (SectorStatus::Erased, SectorStatus::Erased) => {
            Err(CredentialStoreFault::UnformattedErased)
        }
        (SectorStatus::Active(a), SectorStatus::Active(b)) => select_two_active(a, b),
        (SectorStatus::Active(active), other) | (other, SectorStatus::Active(active)) => {
            select_one_active(active, other)
        }
        (SectorStatus::Retired(_), _)
        | (_, SectorStatus::Retired(_))
        | (SectorStatus::Retiring(_), _)
        | (_, SectorStatus::Retiring(_)) => Err(CredentialStoreFault::RetiredWithoutSuccessor),
        (SectorStatus::Invalid { sector, kind, .. }, _)
        | (_, SectorStatus::Invalid { sector, kind, .. }) => Err(map_invalid(sector, kind)),
        (SectorStatus::Uncommitted(_), _)
        | (_, SectorStatus::Uncommitted(_))
        | (SectorStatus::RetirementDebris { .. }, _)
        | (_, SectorStatus::RetirementDebris { .. }) => {
            Err(CredentialStoreFault::UnformattedNonErased)
        }
    }
}

fn select_two_active(
    a: ValidSnapshot,
    b: ValidSnapshot,
) -> Result<SelectedSnapshot, CredentialStoreFault> {
    let (newer, older) = if a.revision > b.revision {
        (a, b)
    } else {
        (b, a)
    };
    if newer.revision != older.revision.checked_add(1).unwrap_or(0)
        || newer.predecessor_revision != older.revision
        || newer.predecessor_digest != older.digest
    {
        return Err(CredentialStoreFault::SnapshotConflict);
    }
    Ok(SelectedSnapshot {
        recovery: CredentialStoreRecovery::RetirePredecessor {
            sector: older.sector,
        },
        snapshot: newer,
    })
}

fn select_one_active(
    active: ValidSnapshot,
    other: SectorStatus,
) -> Result<SelectedSnapshot, CredentialStoreFault> {
    let recovery = match other {
        SectorStatus::Erased => CredentialStoreRecovery::Clean,
        SectorStatus::Uncommitted(sector) => CredentialStoreRecovery::CleanupInactive { sector },
        SectorStatus::Retiring(older) => {
            if !linked_predecessor(&active, &older) {
                return Err(CredentialStoreFault::SnapshotConflict);
            }
            CredentialStoreRecovery::RetirePredecessor {
                sector: older.sector,
            }
        }
        SectorStatus::Retired(older) => {
            if !linked_predecessor(&active, &older) {
                return Err(CredentialStoreFault::SnapshotConflict);
            }
            CredentialStoreRecovery::CleanupInactive {
                sector: older.sector,
            }
        }
        SectorStatus::RetirementDebris {
            sector,
            observed_digest,
        } => {
            if active.predecessor_revision == 0
                || !monotonic_erasure_compatible(&observed_digest, &active.predecessor_digest)
            {
                return Err(CredentialStoreFault::SnapshotConflict);
            }
            CredentialStoreRecovery::CleanupInactive { sector }
        }
        SectorStatus::Invalid { sector, kind } => return Err(map_invalid(sector, kind)),
        SectorStatus::Active(_) => return Err(CredentialStoreFault::SnapshotConflict),
    };
    Ok(SelectedSnapshot {
        snapshot: active,
        recovery,
    })
}

const fn map_invalid(
    sector: CredentialStoreSector,
    kind: InvalidSectorKind,
) -> CredentialStoreFault {
    match kind {
        InvalidSectorKind::UnsupportedPhysical(version) => {
            CredentialStoreFault::UnsupportedPhysicalVersion(version)
        }
        InvalidSectorKind::UnsupportedSemantic(version) => {
            CredentialStoreFault::UnsupportedSemanticVersion(version)
        }
        InvalidSectorKind::WrongDevice => CredentialStoreFault::DeviceBindingMismatch { sector },
        InvalidSectorKind::CommittedInvalid => {
            CredentialStoreFault::CommittedSnapshotCorrupt { sector }
        }
        InvalidSectorKind::Unknown => CredentialStoreFault::UnknownProgrammedData { sector },
    }
}

fn linked_predecessor(newer: &ValidSnapshot, older: &ValidSnapshot) -> bool {
    newer.revision == older.revision.checked_add(1).unwrap_or(0)
        && newer.predecessor_revision == older.revision
        && newer.predecessor_digest == older.digest
}

fn snapshot_digest(data: &[u8; SNAPSHOT_PREFIX_SIZE]) -> [u8; DIGEST_SIZE] {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(data);
    hasher.update(DIGEST_FLUSH_TRAILER);
    hasher.finalize().into()
}

fn snapshot_digest_parts(
    header: &[u8; HEADER_SIZE],
    image: &[u8; CREDENTIAL_SNAPSHOT_IMAGE_SIZE],
) -> [u8; DIGEST_SIZE] {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(header);
    hasher.update(image);
    hasher.update(DIGEST_FLUSH_TRAILER);
    hasher.finalize().into()
}

fn monotonic_programming_compatible(actual: &[u8], target: &[u8]) -> bool {
    actual.len() == target.len()
        && actual
            .iter()
            .zip(target)
            .all(|(actual, target)| actual & target == *target)
}

fn monotonic_erasure_compatible(actual: &[u8], original: &[u8]) -> bool {
    actual.len() == original.len()
        && actual
            .iter()
            .zip(original)
            .all(|(actual, original)| actual | original == *actual)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("fixed u16"))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed u32"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed u64"))
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

fn validate_binding<A: BoundCredentialStoreAccess>(
    access: &A,
) -> Result<(), CredentialStoreBindingError> {
    let binding = access.credential_store_binding();
    if binding.layout_version != PHYSICAL_FORMAT_VERSION {
        return Err(CredentialStoreBindingError::LayoutVersionMismatch {
            expected: PHYSICAL_FORMAT_VERSION,
            actual: binding.layout_version,
        });
    }
    if binding.length != PARTITION_SIZE {
        return Err(CredentialStoreBindingError::LengthMismatch {
            expected: PARTITION_SIZE,
            actual: binding.length,
        });
    }
    if binding
        .absolute_offset
        .checked_add(binding.length)
        .is_none()
    {
        return Err(CredentialStoreBindingError::RangeOverflow {
            absolute_offset: binding.absolute_offset,
            length: binding.length,
        });
    }
    let read_size = <A as ReadNorFlash>::READ_SIZE;
    let write_size = <A as NorFlash>::WRITE_SIZE;
    let erase_size = <A as NorFlash>::ERASE_SIZE;
    let aligned =
        |value: usize, alignment: usize| alignment != 0 && value.is_multiple_of(alignment);
    let read_values = [
        binding.absolute_offset,
        binding.length,
        SECTOR_SIZE,
        SNAPSHOT_PREFIX_SIZE,
        COMMIT_DIGEST_OFFSET,
        COMMIT_MARKER_OFFSET,
        RETIREMENT_MARKER_OFFSET,
        MARKER_SIZE,
    ];
    let write_values = [
        binding.absolute_offset,
        SNAPSHOT_PREFIX_SIZE,
        COMMIT_DIGEST_OFFSET,
        COMMIT_MARKER_OFFSET,
        RETIREMENT_MARKER_OFFSET,
        DIGEST_SIZE,
        MARKER_SIZE,
    ];
    if read_values
        .into_iter()
        .any(|value| !aligned(value, read_size))
        || write_values
            .into_iter()
            .any(|value| !aligned(value, write_size))
        || !aligned(binding.absolute_offset, erase_size)
        || !aligned(binding.length, erase_size)
        || !aligned(SECTOR_SIZE, erase_size)
    {
        return Err(CredentialStoreBindingError::AlignmentMismatch {
            absolute_offset: binding.absolute_offset,
            length: binding.length,
            read_size,
            write_size,
            erase_size,
        });
    }
    if access.capacity() != binding.length {
        return Err(CredentialStoreBindingError::CapacityMismatch {
            expected: binding.length,
            actual: access.capacity(),
        });
    }
    Ok(())
}

fn validate_exact_binding(
    expected: CredentialStoreBinding,
    actual: CredentialStoreBinding,
) -> Result<(), CredentialStoreBindingError> {
    if actual.device != expected.device {
        return Err(CredentialStoreBindingError::DeviceMismatch {
            expected: expected.device,
            actual: actual.device,
        });
    }
    if actual.absolute_offset != expected.absolute_offset || actual.length != expected.length {
        return Err(CredentialStoreBindingError::RangeMismatch {
            expected_offset: expected.absolute_offset,
            expected_length: expected.length,
            actual_offset: actual.absolute_offset,
            actual_length: actual.length,
        });
    }
    if actual.layout_version != expected.layout_version {
        return Err(CredentialStoreBindingError::LayoutVersionMismatch {
            expected: expected.layout_version,
            actual: actual.layout_version,
        });
    }
    Ok(())
}

fn validate_mounted_access<A: BoundCredentialStoreAccess>(
    binding: CredentialStoreBinding,
    access: &A,
) -> Result<(), CredentialStoreBindingError> {
    validate_exact_binding(binding, access.credential_store_binding())?;
    validate_binding(access)
}

fn encode_snapshot_prefix(
    authority: &CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>,
    binding: CredentialStoreBinding,
    sector: CredentialStoreSector,
    predecessor_revision: u64,
    predecessor_digest: [u8; DIGEST_SIZE],
) -> Zeroizing<[u8; SNAPSHOT_PREFIX_SIZE]> {
    let mut prefix = Zeroizing::new([0_u8; SNAPSHOT_PREFIX_SIZE]);
    prefix[..8].copy_from_slice(MAGIC);
    put_u16(&mut prefix[..], 8, PHYSICAL_FORMAT_VERSION);
    put_u16(&mut prefix[..], 10, SEMANTIC_FORMAT_VERSION);
    prefix[12] = sector.id();
    prefix[13] = u8::try_from(authority.record_count()).expect("capacity is sixteen");
    put_u64(&mut prefix[..], 16, authority.revision().get());
    put_u64(&mut prefix[..], 24, predecessor_revision);
    prefix[32..64].copy_from_slice(&predecessor_digest);
    prefix[64..80].copy_from_slice(binding.device.as_bytes());
    put_u32(&mut prefix[..], 80, RECORD_CAPACITY as u32);
    put_u32(&mut prefix[..], 84, CREDENTIAL_SNAPSHOT_SLOT_SIZE as u32);
    put_u64(&mut prefix[..], 88, CREDENTIAL_SNAPSHOT_IMAGE_SIZE as u64);
    let image = encode_e290_credential_snapshot(authority);
    prefix[SNAPSHOT_IMAGE_OFFSET..].copy_from_slice(image.as_bytes());
    prefix
}

fn write_exact<A: MultiwriteNorFlash>(
    access: &mut A,
    offset: usize,
    bytes: &[u8],
) -> Result<(), PhysicalError<A::Error>> {
    access
        .write(offset as u32, bytes)
        .map_err(PhysicalError::Backend)?;
    let mut readback = Zeroizing::new([0_u8; DIGEST_SIZE]);
    for (chunk_index, expected) in bytes.chunks(DIGEST_SIZE).enumerate() {
        let actual = &mut readback[..expected.len()];
        access
            .read((offset + chunk_index * DIGEST_SIZE) as u32, actual)
            .map_err(PhysicalError::Backend)?;
        if actual != expected {
            return Err(PhysicalError::Readback);
        }
    }
    Ok(())
}

fn erase_exact<A: MultiwriteNorFlash>(
    access: &mut A,
    sector: CredentialStoreSector,
) -> Result<(), PhysicalError<A::Error>> {
    let from = sector.offset() as u32;
    let to = (sector.offset() + SECTOR_SIZE) as u32;
    access.erase(from, to).map_err(PhysicalError::Backend)?;
    let mut readback = Zeroizing::new([0_u8; DIGEST_SIZE]);
    for chunk_offset in (0..SECTOR_SIZE).step_by(DIGEST_SIZE) {
        access
            .read((sector.offset() + chunk_offset) as u32, &mut readback[..])
            .map_err(PhysicalError::Backend)?;
        if readback.iter().any(|byte| *byte != 0xff) {
            return Err(PhysicalError::Readback);
        }
    }
    Ok(())
}

fn region_is_erased<A: ReadNorFlash>(
    access: &mut A,
    offset: usize,
    length: usize,
) -> Result<bool, A::Error> {
    let mut readback = Zeroizing::new([0_u8; DIGEST_SIZE]);
    let mut cursor = 0_usize;
    while cursor < length {
        let chunk = core::cmp::min(DIGEST_SIZE, length - cursor);
        access.read((offset + cursor) as u32, &mut readback[..chunk])?;
        if readback[..chunk].iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        cursor += chunk;
    }
    Ok(true)
}

fn inspect_program_stage<A: ReadNorFlash>(
    access: &mut A,
    offset: usize,
    expected: &[u8],
) -> Result<(bool, bool, bool), A::Error> {
    let mut readback = Zeroizing::new([0_u8; DIGEST_SIZE]);
    let mut exact = true;
    let mut compatible = true;
    let mut started = false;
    for (chunk_index, target) in expected.chunks(DIGEST_SIZE).enumerate() {
        let actual = &mut readback[..target.len()];
        access.read((offset + chunk_index * DIGEST_SIZE) as u32, actual)?;
        exact &= actual == target;
        compatible &= monotonic_programming_compatible(actual, target);
        started |= actual.iter().any(|byte| *byte != 0xff);
    }
    Ok((exact, compatible, started))
}

enum PhysicalError<E> {
    Backend(E),
    Readback,
}

/// Explicitly provision an empty revision-1 authority on fully erased media.
///
/// This operation never erases. Any programmed byte causes a read-only failure.
pub fn provision_empty<A>(
    access: &mut A,
) -> Result<MountedCredentialStore, CredentialStoreMountError<A::Error>>
where
    A: BoundCredentialStoreAccess,
{
    validate_binding(access).map_err(CredentialStoreMountError::Binding)?;
    let binding = access.credential_store_binding();
    let a = scan_sector(access, binding, CredentialStoreSector::A)
        .map_err(CredentialStoreMountError::Backend)?;
    let b = scan_sector(access, binding, CredentialStoreSector::B)
        .map_err(CredentialStoreMountError::Backend)?;
    if !matches!(a, SectorStatus::Erased) || !matches!(b, SectorStatus::Erased) {
        return Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::UnformattedNonErased,
        ));
    }
    let authority = CredentialAuthorityBuilder::<E290_CREDENTIAL_RECORD_CAPACITY>::new(
        AuthorityRevision::new(1),
    )
    .map_err(|_| {
        CredentialStoreMountError::Fault(CredentialStoreFault::CommittedSnapshotCorrupt {
            sector: CredentialStoreSector::A,
        })
    })?
    .finish();
    let prefix = encode_snapshot_prefix(
        &authority,
        binding,
        CredentialStoreSector::A,
        0,
        ZERO_DIGEST,
    );
    let digest = snapshot_digest(&prefix);
    for (offset, bytes) in [
        (CredentialStoreSector::A.offset(), &prefix[..]),
        (COMMIT_DIGEST_OFFSET, &digest[..]),
        (COMMIT_MARKER_OFFSET, &COMMIT_MARKER[..]),
    ] {
        if let Err(error) = write_exact(access, offset, bytes) {
            return Err(match error {
                PhysicalError::Backend(error) => CredentialStoreMountError::Backend(error),
                PhysicalError::Readback => {
                    CredentialStoreMountError::Fault(CredentialStoreFault::ReadbackMismatch {
                        sector: CredentialStoreSector::A,
                    })
                }
            });
        }
    }
    let established = scan_sector(access, binding, CredentialStoreSector::A)
        .map_err(CredentialStoreMountError::Backend)?;
    let selected = match established {
        SectorStatus::Active(snapshot)
            if snapshot.revision == 1
                && snapshot.record_count == 0
                && snapshot.predecessor_revision == 0
                && snapshot.predecessor_digest == ZERO_DIGEST
                && snapshot.digest == digest =>
        {
            SelectedSnapshot {
                snapshot,
                recovery: CredentialStoreRecovery::Clean,
            }
        }
        _ => {
            return Err(CredentialStoreMountError::Fault(
                CredentialStoreFault::ReadbackMismatch {
                    sector: CredentialStoreSector::A,
                },
            ));
        }
    };
    // Decode the bytes just re-read from flash rather than publishing the
    // in-memory encoder input.
    drop(authority);
    materialize(binding, selected).map_err(CredentialStoreMountError::Fault)
}

/// Resume only the exact interrupted revision-1 empty provisioning trajectory.
///
/// This operation never erases. Sector B must remain completely erased.
/// Sector A must be a monotonic program prefix of the canonical, device-bound
/// empty snapshot, and later stages may contain bytes only after every earlier
/// stage is exact. Arbitrary programmed media is rejected without mutation.
pub fn recover_empty_provision<A>(
    access: &mut A,
) -> Result<MountedCredentialStore, CredentialStoreMountError<A::Error>>
where
    A: BoundCredentialStoreAccess,
{
    validate_binding(access).map_err(CredentialStoreMountError::Binding)?;
    let binding = access.credential_store_binding();
    let authority = CredentialAuthorityBuilder::<E290_CREDENTIAL_RECORD_CAPACITY>::new(
        AuthorityRevision::new(1),
    )
    .map_err(|_| {
        CredentialStoreMountError::Fault(CredentialStoreFault::CommittedSnapshotCorrupt {
            sector: CredentialStoreSector::A,
        })
    })?
    .finish();
    let prefix = encode_snapshot_prefix(
        &authority,
        binding,
        CredentialStoreSector::A,
        0,
        ZERO_DIGEST,
    );
    let digest = snapshot_digest(&prefix);
    let b_erased = region_is_erased(access, CredentialStoreSector::B.offset(), SECTOR_SIZE)
        .map_err(CredentialStoreMountError::Backend)?;
    let (prefix_exact, prefix_compatible, _) =
        inspect_program_stage(access, CredentialStoreSector::A.offset(), &prefix[..])
            .map_err(CredentialStoreMountError::Backend)?;
    let (digest_exact, digest_compatible, digest_started) =
        inspect_program_stage(access, COMMIT_DIGEST_OFFSET, &digest)
            .map_err(CredentialStoreMountError::Backend)?;
    let (_, commit_compatible, commit_started) =
        inspect_program_stage(access, COMMIT_MARKER_OFFSET, &COMMIT_MARKER)
            .map_err(CredentialStoreMountError::Backend)?;
    let retirement_and_tail_erased = region_is_erased(
        access,
        RETIREMENT_MARKER_OFFSET,
        SECTOR_SIZE - RETIREMENT_MARKER_OFFSET,
    )
    .map_err(CredentialStoreMountError::Backend)?;
    if !b_erased
        || !prefix_compatible
        || !digest_compatible
        || !commit_compatible
        || !retirement_and_tail_erased
        || (digest_started && !prefix_exact)
        || (commit_started && (!prefix_exact || !digest_exact))
    {
        return Err(CredentialStoreMountError::Fault(
            CredentialStoreFault::UnformattedNonErased,
        ));
    }

    for (offset, bytes) in [
        (CredentialStoreSector::A.offset(), &prefix[..]),
        (COMMIT_DIGEST_OFFSET, &digest[..]),
        (COMMIT_MARKER_OFFSET, &COMMIT_MARKER[..]),
    ] {
        if let Err(error) = write_exact(access, offset, bytes) {
            return Err(map_physical_error(error, CredentialStoreSector::A));
        }
    }
    drop(authority);
    let selected = match scan_sector(access, binding, CredentialStoreSector::A)
        .map_err(CredentialStoreMountError::Backend)?
    {
        SectorStatus::Active(snapshot)
            if snapshot.revision == 1
                && snapshot.record_count == 0
                && snapshot.predecessor_revision == 0
                && snapshot.predecessor_digest == ZERO_DIGEST
                && snapshot.digest == digest =>
        {
            SelectedSnapshot {
                snapshot,
                recovery: CredentialStoreRecovery::Clean,
            }
        }
        _ => {
            return Err(CredentialStoreMountError::Fault(
                CredentialStoreFault::ReadbackMismatch {
                    sector: CredentialStoreSector::A,
                },
            ));
        }
    };
    materialize(binding, selected).map_err(CredentialStoreMountError::Fault)
}

/// Failure from one explicit mounted recovery operation.
#[must_use = "recovery failure retains the mounted credential authority"]
pub struct CredentialStoreRecoveryError<E> {
    error: CredentialStoreMountError<E>,
    store: MountedCredentialStore,
}

impl<E> CredentialStoreRecoveryError<E> {
    /// Borrow the non-secret error classification.
    pub const fn error(&self) -> &CredentialStoreMountError<E> {
        &self.error
    }

    /// Recover mounted authority ownership for an exact retry.
    pub fn into_store(self) -> MountedCredentialStore {
        self.store
    }
}

/// Perform exactly one reported retirement or cleanup step.
pub fn recover_once<A>(
    mut store: MountedCredentialStore,
    access: &mut A,
) -> Result<MountedCredentialStore, CredentialStoreRecoveryError<A::Error>>
where
    A: BoundCredentialStoreAccess,
{
    if let Err(error) = validate_mounted_access(store.binding, access) {
        return Err(CredentialStoreRecoveryError {
            error: CredentialStoreMountError::Binding(error),
            store,
        });
    }
    match store.recovery {
        CredentialStoreRecovery::Clean => Ok(store),
        CredentialStoreRecovery::RetirePredecessor { sector } => {
            let offset = sector.offset() + RETIREMENT_MARKER_OFFSET;
            match write_exact(access, offset, &RETIREMENT_MARKER) {
                Ok(()) => {
                    store.recovery = CredentialStoreRecovery::CleanupInactive { sector };
                    Ok(store)
                }
                Err(error) => Err(CredentialStoreRecoveryError {
                    error: map_physical_error(error, sector),
                    store,
                }),
            }
        }
        CredentialStoreRecovery::CleanupInactive { sector } => match erase_exact(access, sector) {
            Ok(()) => {
                store.recovery = CredentialStoreRecovery::Clean;
                Ok(store)
            }
            Err(error) => Err(CredentialStoreRecoveryError {
                error: map_physical_error(error, sector),
                store,
            }),
        },
    }
}

fn map_physical_error<E>(
    error: PhysicalError<E>,
    sector: CredentialStoreSector,
) -> CredentialStoreMountError<E> {
    match error {
        PhysicalError::Backend(error) => CredentialStoreMountError::Backend(error),
        PhysicalError::Readback => {
            CredentialStoreMountError::Fault(CredentialStoreFault::ReadbackMismatch { sector })
        }
    }
}

/// Exact unpublished successor retained across an ambiguous physical result.
#[must_use = "pending credential mutation must be reconciled or explicitly canceled"]
pub struct PendingCredentialSuccessor {
    current: MountedCredentialStore,
    candidate: CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>,
    media_ambiguous: bool,
}

impl PendingCredentialSuccessor {
    /// Current committed authority revision at mutation start.
    pub const fn current_revision(&self) -> AuthorityRevision {
        self.current.revision()
    }

    /// Unpublished candidate revision retained for reconciliation.
    pub const fn candidate_revision(&self) -> AuthorityRevision {
        self.candidate.revision()
    }

    /// Exact binding required for reconciliation.
    pub const fn binding(&self) -> CredentialStoreBinding {
        self.current.binding
    }

    /// Whether media may already contain some or all of the candidate mutation.
    ///
    /// Once this becomes `true`, ownership cannot be split until
    /// [`reconcile_successor`] establishes and finishes the authoritative
    /// on-media state.
    pub const fn media_ambiguous(&self) -> bool {
        self.media_ambiguous
    }

    /// Cancel an I/O-free mutation and recover both exact owners.
    ///
    /// This succeeds only while no physical mutation has been attempted and
    /// no candidate evidence has been observed. After media becomes
    /// ambiguous, the undivided pending owner is returned and must be passed
    /// to [`reconcile_successor`].
    pub fn into_parts(
        self,
    ) -> Result<
        (
            MountedCredentialStore,
            CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>,
        ),
        Self,
    > {
        if self.media_ambiguous {
            return Err(self);
        }
        Ok((self.current, self.candidate))
    }
}

/// Successor mutation failure retaining both authority owners.
#[must_use = "successor failure retains exact mutation ownership"]
pub struct CredentialSuccessorStoreError<E> {
    error: CredentialStoreMountError<E>,
    pending: PendingCredentialSuccessor,
}

impl<E> CredentialSuccessorStoreError<E> {
    /// Borrow the non-secret physical error.
    pub const fn error(&self) -> &CredentialStoreMountError<E> {
        &self.error
    }

    /// Recover the exact pending mutation for reconciliation.
    pub fn into_pending(self) -> PendingCredentialSuccessor {
        self.pending
    }
}

/// Semantic successor rejection retaining the original mounted owner and candidate.
#[must_use = "semantic rejection retains both credential authority owners"]
pub struct CredentialSuccessorSemanticError {
    kind: CredentialSuccessorFaultKind,
    /// Mounted current authority, known to remain physically unchanged.
    pub current: MountedCredentialStore,
    /// Rejected unpublished candidate.
    pub candidate: CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>,
}

impl CredentialSuccessorSemanticError {
    /// Non-secret semantic rejection category.
    pub const fn kind(&self) -> CredentialSuccessorFaultKind {
        self.kind
    }
}

/// Start and drive one exact successor through target commit and source retirement.
///
/// The candidate is not published until both target commit and predecessor
/// retirement have exact readback. Backend ambiguity returns a pending owner;
/// callers must reconcile it before using either authority for mutation.
pub fn commit_successor<A>(
    current: MountedCredentialStore,
    access: &mut A,
    candidate: CredentialAuthority<E290_CREDENTIAL_RECORD_CAPACITY>,
) -> Result<MountedCredentialStore, CommitSuccessorError<A::Error>>
where
    A: BoundCredentialStoreAccess,
{
    let candidate = match current.authority.plan_successor(candidate) {
        Ok(plan) => plan.into_unpublished_candidate(),
        Err(fault) => {
            let kind = fault.kind();
            return Err(CommitSuccessorError::Semantic(
                CredentialSuccessorSemanticError {
                    kind,
                    current,
                    candidate: fault.into_candidate(),
                },
            ));
        }
    };
    let pending = PendingCredentialSuccessor {
        current,
        candidate,
        media_ambiguous: false,
    };
    if pending.current.recovery != CredentialStoreRecovery::Clean {
        return Err(CommitSuccessorError::Physical(
            CredentialSuccessorStoreError {
                error: CredentialStoreMountError::Fault(CredentialStoreFault::RecoveryRequired),
                pending,
            },
        ));
    }
    drive_pending_successor(pending, access).map_err(CommitSuccessorError::Physical)
}

/// Either semantic preflight rejection or a retained ambiguous physical mutation.
pub enum CommitSuccessorError<E> {
    /// Candidate is not a valid one-step semantic successor; no I/O occurred.
    Semantic(CredentialSuccessorSemanticError),
    /// Physical operation failed and exact mutation ownership is retained.
    Physical(CredentialSuccessorStoreError<E>),
}

/// Reconcile and continue one exact successor after a physical ambiguity.
pub fn reconcile_successor<A>(
    pending: PendingCredentialSuccessor,
    access: &mut A,
) -> Result<MountedCredentialStore, CredentialSuccessorStoreError<A::Error>>
where
    A: BoundCredentialStoreAccess,
{
    drive_pending_successor(pending, access)
}

fn drive_pending_successor<A>(
    mut pending: PendingCredentialSuccessor,
    access: &mut A,
) -> Result<MountedCredentialStore, CredentialSuccessorStoreError<A::Error>>
where
    A: BoundCredentialStoreAccess,
{
    if pending.current.recovery != CredentialStoreRecovery::Clean {
        return Err(CredentialSuccessorStoreError {
            error: CredentialStoreMountError::Fault(CredentialStoreFault::RecoveryRequired),
            pending,
        });
    }
    if let Err(error) = validate_mounted_access(pending.current.binding, access) {
        return Err(CredentialSuccessorStoreError {
            error: CredentialStoreMountError::Binding(error),
            pending,
        });
    }
    let target = pending.current.active_sector.other();
    let prefix = encode_snapshot_prefix(
        &pending.candidate,
        pending.current.binding,
        target,
        pending.current.revision().get(),
        pending.current.committed_digest,
    );
    let target_digest = snapshot_digest(&prefix);

    let target_status = match scan_sector(access, pending.current.binding, target) {
        Ok(status) => status,
        Err(error) => {
            return Err(CredentialSuccessorStoreError {
                error: CredentialStoreMountError::Backend(error),
                pending,
            });
        }
    };
    let target_needs_write = match target_status {
        SectorStatus::Erased => true,
        SectorStatus::Uncommitted(sector) if sector == target => {
            pending.media_ambiguous = true;
            if let Err(error) = erase_exact(access, target) {
                return Err(CredentialSuccessorStoreError {
                    error: map_physical_error(error, target),
                    pending,
                });
            }
            true
        }
        SectorStatus::Active(snapshot)
            if snapshot.revision == pending.candidate.revision().get()
                && snapshot.predecessor_revision == pending.current.revision().get()
                && snapshot.predecessor_digest == pending.current.committed_digest
                && snapshot.digest == target_digest =>
        {
            pending.media_ambiguous = true;
            false
        }
        _ => {
            pending.media_ambiguous = true;
            return Err(CredentialSuccessorStoreError {
                error: CredentialStoreMountError::Fault(CredentialStoreFault::SnapshotConflict),
                pending,
            });
        }
    };
    if target_needs_write {
        // Set this before the first mutation attempt: a backend error cannot
        // establish whether zero, some or all requested bytes reached media.
        pending.media_ambiguous = true;
        for (offset, bytes) in [
            (target.offset(), &prefix[..]),
            (target.offset() + COMMIT_DIGEST_OFFSET, &target_digest[..]),
            (target.offset() + COMMIT_MARKER_OFFSET, &COMMIT_MARKER[..]),
        ] {
            if let Err(error) = write_exact(access, offset, bytes) {
                return Err(CredentialSuccessorStoreError {
                    error: map_physical_error(error, target),
                    pending,
                });
            }
        }
    }

    // Establish the complete canonical target from a fresh sector scan before
    // changing predecessor state. Per-write readback alone is not publication
    // evidence: this revalidates the tail, header, link, digest and semantic
    // image as one committed authority.
    let established = match scan_sector(access, pending.current.binding, target) {
        Ok(SectorStatus::Active(snapshot))
            if snapshot.revision == pending.candidate.revision().get()
                && snapshot.predecessor_revision == pending.current.revision().get()
                && snapshot.predecessor_digest == pending.current.committed_digest
                && snapshot.digest == target_digest =>
        {
            snapshot
        }
        Ok(_) => {
            return Err(CredentialSuccessorStoreError {
                error: CredentialStoreMountError::Fault(CredentialStoreFault::SnapshotConflict),
                pending,
            });
        }
        Err(error) => {
            return Err(CredentialSuccessorStoreError {
                error: CredentialStoreMountError::Backend(error),
                pending,
            });
        }
    };
    let established_count = established.record_count;
    let established_sector = established.sector;
    let decoded = match decode_e290_credential_snapshot(
        AuthorityRevision::new(established.revision),
        established.image,
    ) {
        Ok(decoded) => decoded,
        Err(_) => {
            return Err(CredentialSuccessorStoreError {
                error: CredentialStoreMountError::Fault(
                    CredentialStoreFault::CommittedSnapshotCorrupt {
                        sector: established_sector,
                    },
                ),
                pending,
            });
        }
    };
    if decoded.record_count() != usize::from(established_count) {
        return Err(CredentialSuccessorStoreError {
            error: CredentialStoreMountError::Fault(
                CredentialStoreFault::CommittedSnapshotCorrupt {
                    sector: established_sector,
                },
            ),
            pending,
        });
    }
    drop(decoded);

    let retirement_offset = pending.current.active_sector.offset() + RETIREMENT_MARKER_OFFSET;
    if let Err(error) = write_exact(access, retirement_offset, &RETIREMENT_MARKER) {
        return Err(CredentialSuccessorStoreError {
            error: map_physical_error(error, pending.current.active_sector),
            pending,
        });
    }

    let PendingCredentialSuccessor {
        current, candidate, ..
    } = pending;
    let plan: PlannedCredentialSuccessor<'_, E290_CREDENTIAL_RECORD_CAPACITY> =
        match current.authority.plan_successor(candidate) {
            Ok(plan) => plan,
            Err(fault) => {
                return Err(CredentialSuccessorStoreError {
                    error: CredentialStoreMountError::Fault(CredentialStoreFault::SnapshotConflict),
                    pending: PendingCredentialSuccessor {
                        current,
                        candidate: fault.into_candidate(),
                        media_ambiguous: true,
                    },
                });
            }
        };
    let authority = plan.publish_after_commit();
    Ok(MountedCredentialStore {
        binding: current.binding,
        active_sector: target,
        committed_digest: target_digest,
        authority,
        recovery: CredentialStoreRecovery::CleanupInactive {
            sector: current.active_sector,
        },
    })
}
