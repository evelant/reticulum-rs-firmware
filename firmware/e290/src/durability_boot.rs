//! Cross-store boot policy for durable identity and announce ordering.

use reticulum_announce_clock::FreshClockPolicy;
use reticulum_device_identity_store::IdentityPreflight;

/// Product action for the submission journal while identity state is unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalBootPolicy {
    /// Establish or resume the first format while factory identity is vacant.
    ProvisionFactoryFirst,
    /// Make no provisioning mutation; the later strict runtime mount decides.
    MountExisting,
}

/// Select the only safe fresh-clock policy from a mutation-free identity scan.
///
/// A vacant identity permits the clock to establish its first high-water mark
/// before identity provisioning begins. Once an identity is committed, missing
/// clock state must fail closed rather than restart announce emission times.
pub const fn announce_clock_policy(identity: IdentityPreflight) -> FreshClockPolicy {
    match identity {
        IdentityPreflight::Vacant => FreshClockPolicy::AllowFirstProvision,
        IdentityPreflight::Committed { .. } => FreshClockPolicy::Reject,
    }
}

/// Select journal provisioning authority from durable identity state.
///
/// Journal provisioning runs before identity mutation. A vacant identity is
/// therefore a durable factory transaction marker across a torn first journal
/// write. Once any identity copy is committed, boot may only mount the existing
/// journal.
pub const fn journal_boot_policy(identity: IdentityPreflight) -> JournalBootPolicy {
    match identity {
        IdentityPreflight::Vacant => JournalBootPolicy::ProvisionFactoryFirst,
        IdentityPreflight::Committed { .. } => JournalBootPolicy::MountExisting,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use embedded_storage::nor_flash::{
        ErrorType, MultiwriteNorFlash, NorFlash, NorFlashErrorKind, ReadNorFlash, check_erase,
        check_read, check_write,
    };
    use rand_core::{CryptoRng, RngCore};
    use reticulum_announce_clock::{
        AnnounceOrdinal, FreshClockPolicy, MAX_ANNOUNCE_ORDINAL, ReserveError,
        reserve_next_boot_epoch,
    };
    use reticulum_device_identity_store::{
        IdentityMirrorCoverage, IdentityPreflight, boot_load_or_provision, inspect_identity,
    };
    use reticulum_nor_flash_region::PartitionNorFlash;

    use super::{JournalBootPolicy, announce_clock_policy, journal_boot_policy};

    const IDENTITY_START: u32 = 0;
    const CLOCK_START: u32 = reticulum_device_identity_store::PARTITION_SIZE as u32;
    const TOTAL_BYTES: usize =
        reticulum_device_identity_store::PARTITION_SIZE + reticulum_announce_clock::PARTITION_SIZE;

    #[test]
    fn ordinary_policy_never_provisions_a_journal_for_committed_identity() {
        assert_eq!(
            journal_boot_policy(IdentityPreflight::Vacant),
            JournalBootPolicy::ProvisionFactoryFirst
        );
        assert_eq!(
            journal_boot_policy(IdentityPreflight::Committed {
                coverage: IdentityMirrorCoverage::Single,
            }),
            JournalBootPolicy::MountExisting
        );
        assert_eq!(
            journal_boot_policy(IdentityPreflight::Committed {
                coverage: IdentityMirrorCoverage::Redundant,
            }),
            JournalBootPolicy::MountExisting
        );
    }

    #[test]
    fn power_cut_after_clock_reservation_skips_epoch_before_identity_provisioning() {
        let mut flash = FakeNor::erased();

        let first_preflight = inspect(&mut flash);
        assert_eq!(first_preflight, IdentityPreflight::Vacant);
        let first = reserve(&mut flash, announce_clock_policy(first_preflight)).unwrap();
        assert_eq!(first.epoch().get(), 1);

        // Simulated power loss: the clock commit survives, but identity
        // provisioning was never entered. Reboot performs the same read-only
        // preflight and safely skips the possibly used first epoch.
        let reboot_preflight = inspect(&mut flash);
        assert_eq!(reboot_preflight, IdentityPreflight::Vacant);
        let second = reserve(&mut flash, announce_clock_policy(reboot_preflight)).unwrap();
        assert_eq!(second.epoch().get(), 2);
        assert_eq!(second.report().previous_high_water(), Some(first.epoch()));

        let prior_max = first
            .epoch()
            .timestamp(AnnounceOrdinal::new(MAX_ANNOUNCE_ORDINAL).unwrap());
        let reboot_first = second.epoch().timestamp(AnnounceOrdinal::ZERO);
        assert!(reboot_first > prior_max);

        let mut rng = FixedRng::new(0x31);
        let (_, report) = provision(&mut flash, &mut rng);
        assert_eq!(report.coverage(), IdentityMirrorCoverage::Redundant);
        assert!(matches!(
            inspect(&mut flash),
            IdentityPreflight::Committed {
                coverage: IdentityMirrorCoverage::Redundant
            }
        ));
    }

    #[test]
    fn committed_identity_with_blank_clock_rejects_without_mutation() {
        let mut flash = FakeNor::erased();
        let mut rng = FixedRng::new(0x72);
        let (_, report) = provision(&mut flash, &mut rng);
        assert_eq!(report.coverage(), IdentityMirrorCoverage::Redundant);

        let preflight = inspect(&mut flash);
        assert!(matches!(
            preflight,
            IdentityPreflight::Committed {
                coverage: IdentityMirrorCoverage::Redundant
            }
        ));
        let policy = announce_clock_policy(preflight);
        assert_eq!(policy, FreshClockPolicy::Reject);

        let writes_before = flash.write_calls;
        let erases_before = flash.erase_calls;
        let (result, region_writes, region_erases) = {
            let mut clock = PartitionNorFlash::new(
                &mut flash,
                CLOCK_START,
                reticulum_announce_clock::PARTITION_SIZE as u32,
            );
            let result = reserve_next_boot_epoch(&mut clock, policy);
            (result, clock.write_calls(), clock.erase_calls())
        };

        assert_eq!(result, Err(ReserveError::MissingHighWater));
        assert_eq!(region_writes, 0);
        assert_eq!(region_erases, 0);
        assert_eq!(flash.write_calls, writes_before);
        assert_eq!(flash.erase_calls, erases_before);
        assert!(
            flash.bytes[CLOCK_START as usize..]
                .iter()
                .all(|byte| *byte == 0xff)
        );
    }

    fn inspect(flash: &mut FakeNor) -> IdentityPreflight {
        let mut identity = PartitionNorFlash::new(
            flash,
            IDENTITY_START,
            reticulum_device_identity_store::PARTITION_SIZE as u32,
        );
        inspect_identity(&mut identity).unwrap()
    }

    fn reserve(
        flash: &mut FakeNor,
        policy: FreshClockPolicy,
    ) -> Result<
        reticulum_announce_clock::BootEpochReservation,
        ReserveError<reticulum_nor_flash_region::RegionError<NorFlashErrorKind>>,
    > {
        let mut clock = PartitionNorFlash::new(
            flash,
            CLOCK_START,
            reticulum_announce_clock::PARTITION_SIZE as u32,
        );
        reserve_next_boot_epoch(&mut clock, policy)
    }

    fn provision(
        flash: &mut FakeNor,
        rng: &mut FixedRng,
    ) -> (
        reticulum_device_identity_store::DurableIdentityMaterial,
        reticulum_device_identity_store::IdentityBootReport,
    ) {
        let mut identity = PartitionNorFlash::new(
            flash,
            IDENTITY_START,
            reticulum_device_identity_store::PARTITION_SIZE as u32,
        );
        boot_load_or_provision(&mut identity, rng).unwrap()
    }

    struct FakeNor {
        bytes: [u8; TOTAL_BYTES],
        write_calls: u32,
        erase_calls: u32,
    }

    impl FakeNor {
        const fn erased() -> Self {
            Self {
                bytes: [0xff; TOTAL_BYTES],
                write_calls: 0,
                erase_calls: 0,
            }
        }
    }

    impl ErrorType for FakeNor {
        type Error = NorFlashErrorKind;
    }

    impl ReadNorFlash for FakeNor {
        const READ_SIZE: usize = 4;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            check_read(self, offset, bytes.len())?;
            let start = offset as usize;
            bytes.copy_from_slice(&self.bytes[start..start + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }

    impl NorFlash for FakeNor {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 0x1000;

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            check_write(self, offset, bytes.len())?;
            let start = offset as usize;
            let destination = &mut self.bytes[start..start + bytes.len()];
            if destination
                .iter()
                .zip(bytes)
                .any(|(current, programmed)| current & programmed != *programmed)
            {
                return Err(NorFlashErrorKind::Other);
            }
            for (current, programmed) in destination.iter_mut().zip(bytes) {
                *current &= *programmed;
            }
            self.write_calls = self.write_calls.saturating_add(1);
            Ok(())
        }

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            check_erase(self, from, to)?;
            self.bytes[from as usize..to as usize].fill(0xff);
            self.erase_calls = self.erase_calls.saturating_add(1);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for FakeNor {}

    struct FixedRng(u8);

    impl FixedRng {
        const fn new(seed: u8) -> Self {
            Self(seed)
        }
    }

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0_u8; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0_u8; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for byte in destination {
                *byte = self.0;
                self.0 = self.0.wrapping_add(0x3d);
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for FixedRng {}
}
