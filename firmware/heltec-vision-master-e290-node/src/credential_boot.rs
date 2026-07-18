//! Bounded credential-store boot recovery and local-API admission policy.
//!
//! This policy never initializes erased media. It mounts once and performs at
//! most the exact predecessor-retirement and inactive-cleanup operations
//! reported by the portable store. Credential failure is represented as local
//! API state; callers remain responsible for starting unrelated LoRa and
//! durable-submission services.

use reticulum_device_api_credential_store::{
    BoundCredentialStoreAccess, CredentialStoreBindingError, CredentialStoreFault,
    CredentialStoreMountError, CredentialStoreRecovery, MountedCredentialStore, mount,
    recover_once,
};

/// Maximum internal RAM admitted for one boot outcome and retained authority.
pub const MAXIMUM_CREDENTIAL_BOOT_OUTCOME_BYTES: usize = 2_560;

/// Physical operation whose result controls credential admission at boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialBootPhase {
    /// Read-only mount and media selection.
    Mount,
    /// Monotonic predecessor retirement required before publication.
    RetirePredecessor,
    /// Erase-only cleanup of a non-authoritative inactive sector.
    CleanupInactive,
}

/// Non-secret failure class retained by the boot policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialBootFailure {
    /// The operation-scoped view did not match the retained physical binding.
    Binding(CredentialStoreBindingError),
    /// The raw NOR backend returned an error.
    Backend,
    /// Media or exact readback violated the credential-store contract.
    Fault(CredentialStoreFault),
}

/// Local-API credential admission state selected at boot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialBootState {
    /// Authority is publishable and inactive media is clean.
    Ready,
    /// Existing credentials may authenticate, but cleanup failure prohibits
    /// pairing, rotation, or any other credential mutation this boot.
    AuthenticationOnly {
        /// Why inactive cleanup could not finish.
        cleanup_failure: CredentialBootFailure,
    },
    /// Both sectors are erased; explicit physical-presence initialization is
    /// required and boot performed no mutation.
    UninitializedErased,
    /// A mounted authority cannot be published because required retirement
    /// or another ownership-preserving recovery condition remains unresolved.
    Blocked {
        /// Operation that must be established before publication.
        phase: CredentialBootPhase,
        /// Non-secret failure class.
        failure: CredentialBootFailure,
    },
    /// Programmed media is corrupt, conflicting, unsupported, or belongs to a
    /// different physical device.
    Corrupt {
        /// Stable fail-closed media classification.
        fault: CredentialStoreFault,
    },
    /// Initial mount could not inspect the backend, so no authority exists in
    /// RAM and local API admission remains disabled.
    Backend {
        /// Operation that encountered the backend error.
        phase: CredentialBootPhase,
    },
}

impl CredentialBootState {
    /// Whether an authority is publishable to a future session service.
    pub const fn authority_publishable(self) -> bool {
        matches!(self, Self::Ready | Self::AuthenticationOnly { .. })
    }

    /// Whether the authority is eligible for a future credential mutation.
    pub const fn mutation_eligible(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Credential boot result retaining any successfully mounted authority owner.
#[must_use = "credential boot ownership must remain with the sole storage coordinator"]
pub struct CredentialBootOutcome {
    binding: reticulum_device_api_credential_store::CredentialStoreBinding,
    state: CredentialBootState,
    mounted: Option<MountedCredentialStore>,
    completed_recovery_steps: u8,
}

const _: () =
    assert!(core::mem::size_of::<CredentialBootOutcome>() <= MAXIMUM_CREDENTIAL_BOOT_OUTCOME_BYTES);

impl CredentialBootOutcome {
    /// Exact physical binding used for the boot scan.
    pub const fn binding(&self) -> reticulum_device_api_credential_store::CredentialStoreBinding {
        self.binding
    }

    /// Selected non-secret admission state.
    pub const fn state(&self) -> CredentialBootState {
        self.state
    }

    /// Mounted authority owner, including blocked or cleanup-pending state.
    pub const fn mounted(&self) -> Option<&MountedCredentialStore> {
        self.mounted.as_ref()
    }

    /// Number of exact retirement or cleanup operations completed this boot.
    pub const fn completed_recovery_steps(&self) -> u8 {
        self.completed_recovery_steps
    }

    /// Transfer ownership only when this report belongs to the expected flash.
    ///
    /// A detached report can never publish an authority through a different
    /// product flash owner, including in release builds.
    pub fn into_parts_for_binding(
        self,
        expected: reticulum_device_api_credential_store::CredentialStoreBinding,
    ) -> (CredentialBootState, Option<MountedCredentialStore>) {
        if let Some(error) = exact_binding_error(expected, self.binding) {
            return (
                CredentialBootState::Blocked {
                    phase: CredentialBootPhase::Mount,
                    failure: CredentialBootFailure::Binding(error),
                },
                None,
            );
        }
        (self.state, self.mounted)
    }
}

/// Mount credentials and perform only the bounded recovery explicitly named by media.
///
/// Fully erased media is returned as [`CredentialBootState::UninitializedErased`]
/// without writes or erases. This function intentionally has no provisioning
/// path. Retirement is attempted before cleanup, and each step is attempted at
/// most once per call.
pub fn boot_credentials<A>(access: &mut A) -> CredentialBootOutcome
where
    A: BoundCredentialStoreAccess,
{
    let binding = access.credential_store_binding();
    let mounted = match mount(access) {
        Ok(mounted) => mounted,
        Err(CredentialStoreMountError::Fault(CredentialStoreFault::UnformattedErased)) => {
            return outcome(binding, CredentialBootState::UninitializedErased, None, 0);
        }
        Err(CredentialStoreMountError::Fault(fault)) => {
            return outcome(binding, CredentialBootState::Corrupt { fault }, None, 0);
        }
        Err(CredentialStoreMountError::Backend(_)) => {
            return outcome(
                binding,
                CredentialBootState::Backend {
                    phase: CredentialBootPhase::Mount,
                },
                None,
                0,
            );
        }
        Err(CredentialStoreMountError::Binding(error)) => {
            return outcome(
                binding,
                CredentialBootState::Blocked {
                    phase: CredentialBootPhase::Mount,
                    failure: CredentialBootFailure::Binding(error),
                },
                None,
                0,
            );
        }
    };

    match mounted.recovery() {
        CredentialStoreRecovery::Clean => {
            outcome(binding, CredentialBootState::Ready, Some(mounted), 0)
        }
        CredentialStoreRecovery::CleanupInactive { .. } => {
            finish_cleanup(binding, mounted, access, 0)
        }
        CredentialStoreRecovery::RetirePredecessor { .. } => match recover_once(mounted, access) {
            Ok(mounted) => finish_cleanup(binding, mounted, access, 1),
            Err(error) => {
                let failure = classify(error.error());
                let mounted = error.into_store();
                outcome(
                    binding,
                    CredentialBootState::Blocked {
                        phase: CredentialBootPhase::RetirePredecessor,
                        failure,
                    },
                    Some(mounted),
                    0,
                )
            }
        },
    }
}

fn finish_cleanup<A>(
    binding: reticulum_device_api_credential_store::CredentialStoreBinding,
    mounted: MountedCredentialStore,
    access: &mut A,
    completed_recovery_steps: u8,
) -> CredentialBootOutcome
where
    A: BoundCredentialStoreAccess,
{
    match mounted.recovery() {
        CredentialStoreRecovery::Clean => outcome(
            binding,
            CredentialBootState::Ready,
            Some(mounted),
            completed_recovery_steps,
        ),
        CredentialStoreRecovery::CleanupInactive { .. } => match recover_once(mounted, access) {
            Ok(mounted) if mounted.recovery() == CredentialStoreRecovery::Clean => outcome(
                binding,
                CredentialBootState::Ready,
                Some(mounted),
                completed_recovery_steps.saturating_add(1),
            ),
            Ok(mounted) => outcome(
                binding,
                CredentialBootState::Blocked {
                    phase: CredentialBootPhase::CleanupInactive,
                    failure: CredentialBootFailure::Fault(CredentialStoreFault::RecoveryRequired),
                },
                Some(mounted),
                completed_recovery_steps.saturating_add(1),
            ),
            Err(error) => {
                let failure = classify(error.error());
                let mounted = error.into_store();
                let state = if mounted.publishable_authority().is_some() {
                    CredentialBootState::AuthenticationOnly {
                        cleanup_failure: failure,
                    }
                } else {
                    CredentialBootState::Blocked {
                        phase: CredentialBootPhase::CleanupInactive,
                        failure,
                    }
                };
                outcome(binding, state, Some(mounted), completed_recovery_steps)
            }
        },
        CredentialStoreRecovery::RetirePredecessor { .. } => outcome(
            binding,
            CredentialBootState::Blocked {
                phase: CredentialBootPhase::RetirePredecessor,
                failure: CredentialBootFailure::Fault(CredentialStoreFault::RecoveryRequired),
            },
            Some(mounted),
            completed_recovery_steps,
        ),
    }
}

fn exact_binding_error(
    expected: reticulum_device_api_credential_store::CredentialStoreBinding,
    actual: reticulum_device_api_credential_store::CredentialStoreBinding,
) -> Option<CredentialStoreBindingError> {
    if expected.device() != actual.device() {
        return Some(CredentialStoreBindingError::DeviceMismatch {
            expected: expected.device(),
            actual: actual.device(),
        });
    }
    if expected.absolute_offset() != actual.absolute_offset()
        || expected.length() != actual.length()
    {
        return Some(CredentialStoreBindingError::RangeMismatch {
            expected_offset: expected.absolute_offset(),
            expected_length: expected.length(),
            actual_offset: actual.absolute_offset(),
            actual_length: actual.length(),
        });
    }
    if expected.layout_version() != actual.layout_version() {
        return Some(CredentialStoreBindingError::LayoutVersionMismatch {
            expected: expected.layout_version(),
            actual: actual.layout_version(),
        });
    }
    None
}

fn classify<E>(error: &CredentialStoreMountError<E>) -> CredentialBootFailure {
    match error {
        CredentialStoreMountError::Binding(error) => CredentialBootFailure::Binding(*error),
        CredentialStoreMountError::Backend(_) => CredentialBootFailure::Backend,
        CredentialStoreMountError::Fault(fault) => CredentialBootFailure::Fault(*fault),
    }
}

const fn outcome(
    binding: reticulum_device_api_credential_store::CredentialStoreBinding,
    state: CredentialBootState,
    mounted: Option<MountedCredentialStore>,
    completed_recovery_steps: u8,
) -> CredentialBootOutcome {
    CredentialBootOutcome {
        binding,
        state,
        mounted,
        completed_recovery_steps,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embedded_storage::nor_flash::{
        ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
    };
    use reticulum_device_api_credential_store::{
        BoundCredentialStore, COMMIT_DIGEST_OFFSET, COMMIT_MARKER_OFFSET, CredentialStoreBinding,
        CredentialStoreBindingError, CredentialStoreDeviceId, CredentialStoreFault,
        CredentialStoreRecovery, PHYSICAL_FORMAT_VERSION, RETIREMENT_MARKER_OFFSET,
        SNAPSHOT_DIGEST_OFFSET, SNAPSHOT_IMAGE_OFFSET,
    };
    use sha2::{Digest, Sha256};
    use std::vec;
    use std::vec::Vec;

    use super::{
        CredentialBootFailure, CredentialBootPhase, CredentialBootState, boot_credentials,
    };
    use crate::{api_credentials_binding, storage_device_id_from_eui48};

    const SECTOR_SIZE: usize = 4_096;
    const PARTITION_SIZE: usize = 8_192;
    const COMMIT_MARKER: [u8; 32] = [
        0xa3, 0x51, 0x0e, 0xd8, 0x76, 0x2b, 0xc4, 0x19, 0x5f, 0x90, 0x37, 0xea, 0x6d, 0x04, 0xb2,
        0x8c, 0x41, 0xf7, 0x2d, 0x63, 0x98, 0x15, 0xce, 0x70, 0x0b, 0xd4, 0x56, 0xa9, 0x32, 0xef,
        0x84, 0x1a,
    ];
    const DIGEST_DOMAIN: &[u8] = b"reticulum-rs-firmware/device-api-credential-store/snapshot/v1\0";
    const DIGEST_FLUSH_TRAILER: [u8; 64] = [0xa5; 64];

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FakeError {
        Backend,
        Bounds,
        IllegalProgramming,
    }

    impl NorFlashError for FakeError {
        fn kind(&self) -> NorFlashErrorKind {
            match self {
                Self::Bounds => NorFlashErrorKind::OutOfBounds,
                Self::Backend | Self::IllegalProgramming => NorFlashErrorKind::Other,
            }
        }
    }

    struct FakeNor {
        bytes: Vec<u8>,
        reads: usize,
        writes: usize,
        erases: usize,
        fail_read: bool,
        fail_write: bool,
        fail_erase: bool,
    }

    impl FakeNor {
        fn erased() -> Self {
            Self {
                bytes: vec![0xff; PARTITION_SIZE],
                reads: 0,
                writes: 0,
                erases: 0,
                fail_read: false,
                fail_write: false,
                fail_erase: false,
            }
        }
    }

    impl ErrorType for FakeNor {
        type Error = FakeError;
    }

    impl ReadNorFlash for FakeNor {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            self.reads += 1;
            if self.fail_read {
                return Err(FakeError::Backend);
            }
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
            self.writes += 1;
            if self.fail_write {
                return Err(FakeError::Backend);
            }
            let start = offset as usize;
            let end = start.checked_add(bytes.len()).ok_or(FakeError::Bounds)?;
            let destination = self.bytes.get_mut(start..end).ok_or(FakeError::Bounds)?;
            if destination
                .iter()
                .zip(bytes)
                .any(|(current, next)| current & next != *next)
            {
                return Err(FakeError::IllegalProgramming);
            }
            destination.copy_from_slice(bytes);
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            self.erases += 1;
            if self.fail_erase {
                return Err(FakeError::Backend);
            }
            let destination = self
                .bytes
                .get_mut(from as usize..to as usize)
                .ok_or(FakeError::Bounds)?;
            destination.fill(0xff);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for FakeNor {}

    fn binding(mac: [u8; 6]) -> CredentialStoreBinding {
        api_credentials_binding(storage_device_id_from_eui48(mac))
    }

    fn bound(flash: FakeNor, binding: CredentialStoreBinding) -> BoundCredentialStore<FakeNor> {
        BoundCredentialStore::new(flash, binding)
    }

    fn install_snapshot(
        flash: &mut FakeNor,
        binding: CredentialStoreBinding,
        sector: usize,
        revision: u64,
        predecessor_revision: u64,
        predecessor_digest: [u8; 32],
    ) -> [u8; 32] {
        let base = sector * SECTOR_SIZE;
        let mut prefix = [0_u8; SNAPSHOT_DIGEST_OFFSET];
        prefix[..8].copy_from_slice(b"RDAUTH01");
        prefix[8..10].copy_from_slice(&PHYSICAL_FORMAT_VERSION.to_le_bytes());
        prefix[10..12].copy_from_slice(&1_u16.to_le_bytes());
        prefix[12] = sector as u8;
        prefix[16..24].copy_from_slice(&revision.to_le_bytes());
        prefix[24..32].copy_from_slice(&predecessor_revision.to_le_bytes());
        prefix[32..64].copy_from_slice(&predecessor_digest);
        prefix[64..80].copy_from_slice(binding.device().as_bytes());
        prefix[80..84].copy_from_slice(&16_u32.to_le_bytes());
        prefix[84..88].copy_from_slice(&128_u32.to_le_bytes());
        prefix[88..96].copy_from_slice(&2_048_u64.to_le_bytes());
        assert!(
            prefix[SNAPSHOT_IMAGE_OFFSET..]
                .iter()
                .all(|byte| *byte == 0)
        );

        let mut hasher = Sha256::new();
        hasher.update(DIGEST_DOMAIN);
        hasher.update(prefix);
        hasher.update(DIGEST_FLUSH_TRAILER);
        let digest: [u8; 32] = hasher.finalize().into();
        flash.bytes[base..base + SNAPSHOT_DIGEST_OFFSET].copy_from_slice(&prefix);
        flash.bytes[base + COMMIT_DIGEST_OFFSET..base + COMMIT_MARKER_OFFSET]
            .copy_from_slice(&digest);
        flash.bytes[base + COMMIT_MARKER_OFFSET..base + RETIREMENT_MARKER_OFFSET]
            .copy_from_slice(&COMMIT_MARKER);
        digest
    }

    fn clean_revision_one() -> (CredentialStoreBinding, FakeNor) {
        let binding = binding([1, 2, 3, 4, 5, 6]);
        let mut flash = FakeNor::erased();
        install_snapshot(&mut flash, binding, 0, 1, 0, [0; 32]);
        (binding, flash)
    }

    fn linked_revisions() -> (CredentialStoreBinding, FakeNor) {
        let (binding, mut flash) = clean_revision_one();
        let predecessor = flash.bytes[COMMIT_DIGEST_OFFSET..COMMIT_MARKER_OFFSET]
            .try_into()
            .expect("fixed digest");
        install_snapshot(&mut flash, binding, 1, 2, 1, predecessor);
        (binding, flash)
    }

    #[test]
    fn erased_media_is_uninitialized_without_mutation() {
        let binding = binding([1, 2, 3, 4, 5, 6]);
        let mut access = bound(FakeNor::erased(), binding);
        let outcome = boot_credentials(&mut access);
        assert_eq!(outcome.state(), CredentialBootState::UninitializedErased);
        assert!(outcome.mounted().is_none());
        assert_eq!(outcome.completed_recovery_steps(), 0);
        assert!(access.backend().reads > 0);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }

    #[test]
    fn clean_authority_is_ready_without_mutation() {
        let (binding, flash) = clean_revision_one();
        let mut access = bound(flash, binding);
        let outcome = boot_credentials(&mut access);
        assert_eq!(outcome.binding(), binding);
        assert_eq!(outcome.state(), CredentialBootState::Ready);
        let mounted = outcome.mounted().expect("clean authority retained");
        assert_eq!(mounted.revision().get(), 1);
        assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
        assert!(mounted.publishable_authority().is_some());
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }

    #[test]
    fn detached_report_cannot_publish_through_another_flash_owner() {
        let (report_binding, flash) = clean_revision_one();
        let mut access = bound(flash, report_binding);
        let outcome = boot_credentials(&mut access);
        let owner_binding = CredentialStoreBinding::new(
            CredentialStoreDeviceId::new([0x55; 16]),
            report_binding.absolute_offset(),
            report_binding.length(),
            report_binding.layout_version(),
        );

        let (state, mounted) = outcome.into_parts_for_binding(owner_binding);
        assert_eq!(
            state,
            CredentialBootState::Blocked {
                phase: CredentialBootPhase::Mount,
                failure: CredentialBootFailure::Binding(
                    CredentialStoreBindingError::DeviceMismatch {
                        expected: owner_binding.device(),
                        actual: report_binding.device(),
                    },
                ),
            }
        );
        assert!(mounted.is_none());
    }

    #[test]
    fn linked_authorities_retire_then_cleanup_exactly_once() {
        let (binding, flash) = linked_revisions();
        let mut access = bound(flash, binding);
        let outcome = boot_credentials(&mut access);
        assert_eq!(outcome.state(), CredentialBootState::Ready);
        assert_eq!(outcome.completed_recovery_steps(), 2);
        let mounted = outcome.mounted().expect("newer authority retained");
        assert_eq!(mounted.revision().get(), 2);
        assert_eq!(mounted.recovery(), CredentialStoreRecovery::Clean);
        assert_eq!(access.backend().writes, 1);
        assert_eq!(access.backend().erases, 1);
        assert!(
            access.backend().bytes[..SECTOR_SIZE]
                .iter()
                .all(|byte| *byte == 0xff)
        );
    }

    #[test]
    fn retirement_failure_blocks_publication_and_retains_owner() {
        let (binding, mut flash) = linked_revisions();
        flash.fail_write = true;
        let mut access = bound(flash, binding);
        let outcome = boot_credentials(&mut access);
        assert_eq!(
            outcome.state(),
            CredentialBootState::Blocked {
                phase: CredentialBootPhase::RetirePredecessor,
                failure: CredentialBootFailure::Backend,
            }
        );
        let mounted = outcome.mounted().expect("blocked owner retained");
        assert!(matches!(
            mounted.recovery(),
            CredentialStoreRecovery::RetirePredecessor { .. }
        ));
        assert!(mounted.publishable_authority().is_none());
        assert!(!outcome.state().authority_publishable());
        assert_eq!(access.backend().writes, 1);
        assert_eq!(access.backend().erases, 0);
    }

    #[test]
    fn cleanup_failure_allows_authentication_but_forbids_mutation() {
        let (binding, mut flash) = linked_revisions();
        flash.fail_erase = true;
        let mut access = bound(flash, binding);
        let outcome = boot_credentials(&mut access);
        assert_eq!(
            outcome.state(),
            CredentialBootState::AuthenticationOnly {
                cleanup_failure: CredentialBootFailure::Backend,
            }
        );
        assert_eq!(outcome.completed_recovery_steps(), 1);
        let mounted = outcome.mounted().expect("publishable owner retained");
        assert_eq!(mounted.revision().get(), 2);
        assert!(matches!(
            mounted.recovery(),
            CredentialStoreRecovery::CleanupInactive { .. }
        ));
        assert!(mounted.publishable_authority().is_some());
        assert!(outcome.state().authority_publishable());
        assert!(!outcome.state().mutation_eligible());
        assert_eq!(access.backend().writes, 1);
        assert_eq!(access.backend().erases, 1);
    }

    #[test]
    fn corrupt_and_wrong_device_media_fail_closed_without_mutation() {
        let (binding, mut corrupt) = clean_revision_one();
        corrupt.bytes[SNAPSHOT_IMAGE_OFFSET] ^= 1;
        let mut corrupt = bound(corrupt, binding);
        let outcome = boot_credentials(&mut corrupt);
        assert!(matches!(
            outcome.state(),
            CredentialBootState::Corrupt {
                fault: CredentialStoreFault::CommittedSnapshotCorrupt { .. }
            }
        ));
        assert!(outcome.mounted().is_none());
        assert_eq!(corrupt.backend().writes, 0);
        assert_eq!(corrupt.backend().erases, 0);

        let (media_binding, media) = clean_revision_one();
        let wrong_binding = CredentialStoreBinding::new(
            CredentialStoreDeviceId::new([0x55; 16]),
            media_binding.absolute_offset(),
            media_binding.length(),
            media_binding.layout_version(),
        );
        let mut wrong = bound(media, wrong_binding);
        let outcome = boot_credentials(&mut wrong);
        assert!(matches!(
            outcome.state(),
            CredentialBootState::Corrupt {
                fault: CredentialStoreFault::DeviceBindingMismatch { .. }
            }
        ));
        assert!(outcome.mounted().is_none());
        assert_eq!(wrong.backend().writes, 0);
        assert_eq!(wrong.backend().erases, 0);
    }

    #[test]
    fn mount_backend_failure_is_distinct_and_local_api_only() {
        let binding = binding([1, 2, 3, 4, 5, 6]);
        let mut flash = FakeNor::erased();
        flash.fail_read = true;
        let mut access = bound(flash, binding);
        let outcome = boot_credentials(&mut access);
        assert_eq!(
            outcome.state(),
            CredentialBootState::Backend {
                phase: CredentialBootPhase::Mount
            }
        );
        assert!(!outcome.state().authority_publishable());
        assert!(!outcome.state().mutation_eligible());
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }

    #[test]
    fn invalid_binding_shape_blocks_before_any_io() {
        let expected = binding([1, 2, 3, 4, 5, 6]);
        let invalid = CredentialStoreBinding::new(
            expected.device(),
            expected.absolute_offset(),
            expected.length() / 2,
            expected.layout_version(),
        );
        let mut access = bound(FakeNor::erased(), invalid);
        let outcome = boot_credentials(&mut access);
        assert!(matches!(
            outcome.state(),
            CredentialBootState::Blocked {
                phase: CredentialBootPhase::Mount,
                failure: CredentialBootFailure::Binding(
                    CredentialStoreBindingError::LengthMismatch { .. }
                )
            }
        ));
        assert!(outcome.mounted().is_none());
        assert_eq!(access.backend().reads, 0);
        assert_eq!(access.backend().writes, 0);
        assert_eq!(access.backend().erases, 0);
    }
}
