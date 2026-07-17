//! Sole ESP flash owner and exact permanent-image partition gate.

use core::{
    fmt::{self, Display, Formatter},
    mem,
};

use embedded_storage::nor_flash::ReadNorFlash;
use esp_bootloader_esp_idf::partitions::{PARTITION_TABLE_MAX_LEN, read_partition_table};
use esp_storage::{FlashStorage, FlashStorageError};
use rand_core::{CryptoRng, RngCore};
use reticulum_announce_clock::{
    BootEpochReservation, FreshClockPolicy, ReserveError, reserve_next_boot_epoch,
};
use reticulum_device_api::CapabilityAvailability;
use reticulum_device_api_adapter::{SubmissionAcceptance, SubmissionPort, SubmissionPortError};
use reticulum_device_identity_store::{
    DurableIdentityMaterial, IdentityBootReport, IdentityPreflight, IdentityStoreError,
    boot_load_or_provision, inspect_identity,
};
use reticulum_heltec_vision_master_e290_node::node_journal_binding;
use reticulum_heltec_vision_master_e290_node::partition_contract::{
    ANNOUNCE_CLOCK_LABEL_BYTES, ANNOUNCE_CLOCK_LEN, ANNOUNCE_CLOCK_OFFSET, DATA_PARTITION_TYPE,
    DEVICE_CONFIG_LEN, DEVICE_CONFIG_OFFSET, NODE_IDENTITY_LABEL_BYTES, NODE_IDENTITY_LEN,
    NODE_IDENTITY_OFFSET, NODE_JOURNAL_LABEL_BYTES, NODE_JOURNAL_LEN, NODE_JOURNAL_OFFSET,
    REQUIRED_FLASH_BYTES, UNDEFINED_DATA_SUBTYPE,
};
use reticulum_node_core::{
    AuthorizedFrameObservation, MonotonicMillis, MonotonicSeconds, TxLeaseDeadline,
};
use reticulum_nor_flash_region::{PartitionNorFlash, RegionError};
use reticulum_storage_actor::{
    AcceptanceProgress, BootRecoveryProgress, BoundJournal, DriveError, JournalBinding, MountError,
    ProjectorOperationError, StorageDeviceId,
};
use reticulum_storage_journal::{
    FirstProvisionError, FreshJournalPolicy, JournalState, provision_first,
};
use reticulum_storage_model::{
    AcceptOutcome, AcceptanceCandidate, LifecycleState, PrincipalId, SubmissionId,
};
use reticulum_submission_runtime::{
    FrameOfferProgress, RecoveryStep, RuntimeControlError, RuntimeError, RuntimeStep,
    SubmissionNodePort, SubmissionRuntime,
};

use crate::config;
use reticulum_heltec_vision_master_e290_node::durability_boot::JournalBootPolicy;

const NVS_DATA_SUBTYPE: u8 = 0x02;
const DEVICE_CONFIG_LABEL_BYTES: [u8; 16] = [
    b'd', b'e', b'v', b'i', b'c', b'e', b'_', b'c', b'o', b'n', b'f', b'i', b'g', 0, 0, 0,
];

type ProductRegionError = RegionError<FlashStorageError>;
pub(crate) type ProductSubmissionRuntime =
    SubmissionRuntime<{ config::DURABLE_SUBMISSIONS }, { config::DURABLE_PROJECTED_SUBMISSIONS }>;

/// Failure before the required durable application partitions can be accessed.
#[derive(Debug)]
pub(crate) enum ProductFlashOpenError {
    /// This developer image has no qualified encrypted-flash storage path.
    FlashEncryptionEnabled,
    /// Physical flash capacity differs from the exact permanent-image contract.
    FlashCapacity { expected: usize, actual: usize },
    /// The bootloader partition table failed parsing or MD5 validation.
    PartitionTable,
    /// A required label is absent or duplicated.
    PartitionCardinality {
        identity: u8,
        announce_clock: u8,
        device_config: u8,
        node_journal: u8,
    },
    /// One uniquely named required partition has the wrong type, range, or flags.
    PartitionShape(&'static str),
    /// Another partition overlaps a reserved product partition.
    PartitionOverlap(&'static str),
}

impl Display for ProductFlashOpenError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::FlashEncryptionEnabled => formatter.write_str("flash-encryption-enabled"),
            Self::FlashCapacity { expected, actual } => {
                write!(
                    formatter,
                    "flash-capacity expected={expected} actual={actual}"
                )
            }
            Self::PartitionTable => formatter.write_str("invalid-partition-table"),
            Self::PartitionCardinality {
                identity,
                announce_clock,
                device_config,
                node_journal,
            } => write!(
                formatter,
                "partition-cardinality identity={identity} announce_clock={announce_clock} device_config={device_config} node_journal={node_journal}"
            ),
            Self::PartitionShape(name) => write!(formatter, "partition-shape name={name}"),
            Self::PartitionOverlap(name) => write!(formatter, "partition-overlap name={name}"),
        }
    }
}

/// Identity material plus non-secret physical mutation evidence for this boot.
pub(crate) struct BootIdentity {
    pub(crate) material: DurableIdentityMaterial,
    pub(crate) report: IdentityBootReport,
    pub(crate) raw_write_calls: u32,
    pub(crate) raw_erase_calls: u32,
}

/// Failure while mounting, provisioning, or repairing the dedicated identity mirrors.
pub(crate) type BootIdentityError = IdentityStoreError<ProductRegionError>;

/// Durable boot epoch plus non-secret physical mutation evidence for this boot.
pub(crate) struct BootAnnounceEpoch {
    pub(crate) reservation: BootEpochReservation,
    pub(crate) raw_write_calls: u32,
    pub(crate) raw_erase_calls: u32,
}

/// Failure while reserving and mirroring the next announce-emission epoch.
pub(crate) type BootAnnounceEpochError = ReserveError<ProductRegionError>;

/// Successful factory provision/repair or explicit erased-journal reprovision.
pub(crate) struct BootJournalProvision {
    pub(crate) state: JournalState,
    pub(crate) raw_write_calls: u32,
    pub(crate) raw_erase_calls: u32,
}

/// Failure while recognizing or establishing the canonical first journal.
pub(crate) type BootJournalProvisionError = FirstProvisionError<ProductRegionError>;

/// Failure while strictly mounting or conservatively recovering the journal.
#[derive(Debug)]
pub(crate) enum BootJournalMountError {
    Mount(MountError<ProductRegionError>),
    UnsupportedHistory {
        accepted_submissions: usize,
        supported: usize,
    },
    Recovery(RuntimeError<ProductRegionError>),
}

impl BootJournalMountError {
    pub(crate) const fn stage(&self) -> &'static str {
        match self {
            Self::Mount(error) => {
                let _ = error;
                "mount"
            }
            Self::UnsupportedHistory {
                accepted_submissions,
                supported,
            } => {
                let _ = (accepted_submissions, supported);
                "profile"
            }
            Self::Recovery(error) => {
                let _ = error;
                "recovery"
            }
        }
    }
}

/// Complete boot-only physical journal evidence collected before service opens.
pub(crate) struct BootJournalMountReport {
    pub(crate) state: JournalState,
    pub(crate) replayed_submissions: usize,
    pub(crate) finalized_submissions: usize,
    pub(crate) queued_submissions: usize,
    pub(crate) already_final_submissions: usize,
    pub(crate) raw_write_calls: u32,
    pub(crate) raw_erase_calls: u32,
}

/// Unique owner of the one `esp-storage` instance in the permanent image.
pub(crate) struct ProductFlashOwner {
    flash: &'static mut FlashStorage<'static>,
    journal_binding: JournalBinding,
}

/// Resident sole owner of physical flash and durable submission scheduling.
///
/// The runtime retains only backend-independent semantic state. Every bounded
/// operation creates a fresh journal-relative view with the exact physical
/// binding established at mount, so later config and message-store operations
/// can share this coordinator without leaking raw flash access to protocol or
/// interface actors.
pub(crate) struct ProductStorageCoordinator {
    flash: &'static mut FlashStorage<'static>,
    journal_binding: JournalBinding,
    runtime: Option<ProductSubmissionRuntime>,
    submission_service_enabled: bool,
}

impl ProductFlashOwner {
    /// Validate physical capacity, security state, and every directly adjacent
    /// product partition before returning the sole flash owner.
    pub(crate) fn open(
        flash: &'static mut FlashStorage<'static>,
        storage_device_id: StorageDeviceId,
    ) -> Result<Self, ProductFlashOpenError> {
        if esp_hal::efuse::flash_encryption() {
            return Err(ProductFlashOpenError::FlashEncryptionEnabled);
        }
        let actual = ReadNorFlash::capacity(flash);
        if actual != REQUIRED_FLASH_BYTES {
            return Err(ProductFlashOpenError::FlashCapacity {
                expected: REQUIRED_FLASH_BYTES,
                actual,
            });
        }
        validate_partition_table(flash)?;
        Ok(Self {
            flash,
            journal_binding: node_journal_binding(storage_device_id),
        })
    }

    /// Classify identity media without mutation so the clock can be reserved
    /// before first-provisioning changes the identity partition.
    pub(crate) fn inspect_identity(&mut self) -> Result<IdentityPreflight, BootIdentityError> {
        let mut region =
            PartitionNorFlash::new(&mut *self.flash, NODE_IDENTITY_OFFSET, NODE_IDENTITY_LEN);
        inspect_identity(&mut region)
    }

    /// Load or first-provision the immutable identity before protocol service opens.
    pub(crate) fn boot_identity<R>(
        &mut self,
        rng: &mut R,
    ) -> Result<BootIdentity, BootIdentityError>
    where
        R: RngCore + CryptoRng,
    {
        let mut region =
            PartitionNorFlash::new(&mut *self.flash, NODE_IDENTITY_OFFSET, NODE_IDENTITY_LEN);
        let (material, report) = boot_load_or_provision(&mut region, rng)?;
        Ok(BootIdentity {
            material,
            report,
            raw_write_calls: region.write_calls(),
            raw_erase_calls: region.erase_calls(),
        })
    }

    /// Commit the next boot epoch to both dedicated clock sectors before any
    /// task can construct a local announce.
    pub(crate) fn reserve_announce_epoch(
        &mut self,
        fresh_policy: FreshClockPolicy,
    ) -> Result<BootAnnounceEpoch, BootAnnounceEpochError> {
        let mut region =
            PartitionNorFlash::new(&mut *self.flash, ANNOUNCE_CLOCK_OFFSET, ANNOUNCE_CLOCK_LEN);
        let reservation = reserve_next_boot_epoch(&mut region, fresh_policy)?;
        Ok(BootAnnounceEpoch {
            reservation,
            raw_write_calls: region.write_calls(),
            raw_erase_calls: region.erase_calls(),
        })
    }

    /// Establish the exact empty first journal under the selected boot policy.
    ///
    /// Factory provisioning may resume its own torn first-format trajectory
    /// while identity remains vacant. The schema-2 development path is
    /// deliberately narrower: it scans the complete journal partition and
    /// accepts only erased media before entering `provision_first`. Neither
    /// path erases any bytes.
    pub(crate) fn provision_node_journal(
        &mut self,
        policy: JournalBootPolicy,
    ) -> Result<Option<BootJournalProvision>, BootJournalProvisionError> {
        let erased_only = match policy {
            JournalBootPolicy::MountExisting => return Ok(None),
            JournalBootPolicy::ProvisionFactoryFirst => false,
            JournalBootPolicy::ProvisionErasedSchema2Development => true,
        };

        let mut region =
            PartitionNorFlash::new(&mut *self.flash, NODE_JOURNAL_OFFSET, NODE_JOURNAL_LEN);
        if erased_only
            && !node_journal_is_erased(&mut region).map_err(FirstProvisionError::Backend)?
        {
            return Err(FirstProvisionError::MediaNotProvisionable);
        }

        let state = provision_first(&mut region, FreshJournalPolicy::AllowFirstProvision)?;
        Ok(Some(BootJournalProvision {
            state,
            raw_write_calls: region.write_calls(),
            raw_erase_calls: region.erase_calls(),
        }))
    }

    /// Strictly mount the journal, reject unsupported history before any
    /// recovery mutation, and finish the bootstrap recovery gate.
    ///
    /// This first admission profile permits at most one accepted submission so
    /// its complete live and remount path can be qualified without implying a
    /// product retention capacity. The LoRa dispatcher retains and re-offers
    /// complete authorized-frame observations, and the mounted runtime remains
    /// resident rather than abandoning durable state after boot.
    pub(crate) fn mount_node_runtime(
        &mut self,
        boot_sequence: u64,
    ) -> Result<(ProductSubmissionRuntime, BootJournalMountReport), BootJournalMountError> {
        let (runtime, report) = {
            let region =
                PartitionNorFlash::new(&mut *self.flash, NODE_JOURNAL_OFFSET, NODE_JOURNAL_LEN);
            let mut journal = BoundJournal::new(region, self.journal_binding);
            let mut runtime = ProductSubmissionRuntime::mount(
                &mut journal,
                SubmissionId::new(config::FIRST_SUBMISSION_ID),
                boot_sequence,
            )
            .map_err(BootJournalMountError::Mount)?;

            let mounted_state = runtime.storage().state();
            if mounted_state.accepted_submissions() > config::DURABLE_ACCEPTED_SUBMISSION_LIMIT {
                return Err(BootJournalMountError::UnsupportedHistory {
                    accepted_submissions: mounted_state.accepted_submissions(),
                    supported: config::DURABLE_ACCEPTED_SUBMISSION_LIMIT,
                });
            }

            let mut replayed_submissions = 0_usize;
            let mut finalized_submissions = 0_usize;
            let mut queued_submissions = 0_usize;
            let mut already_final_submissions = 0_usize;
            while let RecoveryStep::Submission { progress, .. } = runtime
                .recover_boot_step(&mut journal)
                .map_err(BootJournalMountError::Recovery)?
            {
                replayed_submissions = replayed_submissions.saturating_add(1);
                match progress {
                    BootRecoveryProgress::ReplayQueued => {
                        queued_submissions = queued_submissions.saturating_add(1);
                    }
                    BootRecoveryProgress::AlreadyFinal => {
                        already_final_submissions = already_final_submissions.saturating_add(1);
                    }
                    BootRecoveryProgress::Finalized => {
                        finalized_submissions = finalized_submissions.saturating_add(1);
                    }
                }
            }

            let state = runtime.storage().state();
            let region = journal.into_backend();
            let report = BootJournalMountReport {
                state,
                replayed_submissions,
                finalized_submissions,
                queued_submissions,
                already_final_submissions,
                raw_write_calls: region.write_calls(),
                raw_erase_calls: region.erase_calls(),
            };
            (runtime, report)
        };

        Ok((runtime, report))
    }

    /// Transfer the sole physical flash owner into its resident coordinator.
    ///
    /// A missing runtime disables only durable local submission service. The
    /// coordinator still retains exclusive flash authority so LoRa routing can
    /// continue without exposing an alias after a journal mount failure.
    pub(crate) fn into_storage_coordinator(
        self,
        runtime: Option<ProductSubmissionRuntime>,
    ) -> ProductStorageCoordinator {
        let submission_service_enabled = runtime.is_some();
        ProductStorageCoordinator {
            flash: self.flash,
            journal_binding: self.journal_binding,
            runtime,
            submission_service_enabled,
        }
    }
}

/// Prove that the complete journal partition is erased without mutation.
fn node_journal_is_erased(
    region: &mut PartitionNorFlash<'_, FlashStorage<'static>>,
) -> Result<bool, ProductRegionError> {
    const READ_CHUNK: usize = 256;

    let mut bytes = [0_u8; READ_CHUNK];
    let mut offset = 0_u32;
    while offset < NODE_JOURNAL_LEN {
        ReadNorFlash::read(region, offset, &mut bytes)?;
        if bytes.iter().any(|byte| *byte != 0xff) {
            return Ok(false);
        }
        offset += READ_CHUNK as u32;
    }
    Ok(true)
}

impl ProductStorageCoordinator {
    /// Whether the durable local submission runtime mounted successfully.
    pub(crate) const fn submission_service_available(&self) -> bool {
        self.submission_service_enabled && self.runtime.is_some()
    }

    /// Permanently close local submission admission and runtime driving for
    /// this boot without dropping backend-independent durable state.
    pub(crate) fn disable_submission_service(&mut self) {
        self.submission_service_enabled = false;
    }

    /// Offer one exact interface-agnostic authorized-frame observation.
    ///
    /// The interface actor must retain and re-offer the observation until the
    /// runtime reports `Durable`. This projection-only step never borrows or
    /// touches physical flash.
    pub(crate) fn offer_authorized_frame(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> Option<Result<FrameOfferProgress, RuntimeControlError>> {
        if !self.submission_service_enabled {
            return None;
        }
        Some(self.runtime.as_mut()?.offer_authorized_frame(observation))
    }

    /// Advance at most one durable submission transition through the permanent
    /// transport-neutral node owner.
    pub(crate) fn drive_submission_step<N, R>(
        &mut self,
        node: &mut N,
        rns_now: MonotonicSeconds,
        owner_now: MonotonicMillis,
        deadline: TxLeaseDeadline,
        rng: &mut R,
    ) -> Option<Result<RuntimeStep, RuntimeError<ProductRegionError>>>
    where
        N: SubmissionNodePort,
        R: RngCore + CryptoRng,
    {
        if !self.submission_service_enabled {
            return None;
        }
        let runtime = self.runtime.as_mut()?;
        let region =
            PartitionNorFlash::new(&mut *self.flash, NODE_JOURNAL_OFFSET, NODE_JOURNAL_LEN);
        let mut journal = BoundJournal::new(region, self.journal_binding);
        Some(runtime.drive_step(&mut journal, node, rns_now, owner_now, deadline, rng))
    }
}

impl SubmissionPort for ProductStorageCoordinator {
    fn availability(&mut self) -> CapabilityAvailability {
        if !self.submission_service_available() {
            return CapabilityAvailability::Unavailable;
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return CapabilityAvailability::Unavailable;
        };
        if runtime.storage().fault().is_some() || config::DURABLE_ACCEPTED_SUBMISSION_LIMIT == 0 {
            CapabilityAvailability::Disabled
        } else {
            CapabilityAvailability::Available
        }
    }

    fn submission_state(
        &mut self,
        principal: PrincipalId,
        id: SubmissionId,
    ) -> Result<Option<LifecycleState>, SubmissionPortError> {
        if !self.submission_service_enabled {
            return Err(SubmissionPortError::Unavailable);
        }
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(SubmissionPortError::Unavailable)?;
        if runtime.storage().fault().is_some() {
            return Err(SubmissionPortError::Faulted);
        }
        if runtime.storage().pending_kind().is_some() {
            return Err(SubmissionPortError::Busy);
        }
        Ok(runtime.storage().index().get_owned_state(principal, id))
    }

    fn accept(
        &mut self,
        candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        if !self.submission_service_enabled {
            return Err(SubmissionPortError::Unavailable);
        }
        let runtime = self
            .runtime
            .as_mut()
            .ok_or(SubmissionPortError::Unavailable)?;

        // Preserve replay and conflict semantics even when the one-live-
        // admission development profile is full. Only a genuinely new
        // acceptance is stopped by the product cap.
        if matches!(
            runtime.storage().index().plan_accept(candidate),
            AcceptOutcome::Accepted(_)
        ) && runtime.storage().state().accepted_submissions()
            >= config::DURABLE_ACCEPTED_SUBMISSION_LIMIT
        {
            return Ok(SubmissionAcceptance::CapacityExhausted);
        }

        let region =
            PartitionNorFlash::new(&mut *self.flash, NODE_JOURNAL_OFFSET, NODE_JOURNAL_LEN);
        let mut journal = BoundJournal::new(region, self.journal_binding);
        runtime
            .accept(&mut journal, candidate)
            .map(map_submission_acceptance)
            .map_err(map_submission_port_runtime_error)
    }
}

fn map_submission_acceptance(progress: AcceptanceProgress) -> SubmissionAcceptance {
    match progress {
        AcceptanceProgress::Accepted(id) => SubmissionAcceptance::Accepted(id),
        AcceptanceProgress::Replay(id) => SubmissionAcceptance::Replay(id),
        AcceptanceProgress::IdempotencyConflict { .. } => SubmissionAcceptance::IdempotencyConflict,
        AcceptanceProgress::IndexExhausted | AcceptanceProgress::JournalCapacityExhausted => {
            SubmissionAcceptance::CapacityExhausted
        }
        AcceptanceProgress::IdentifierExhausted => SubmissionAcceptance::IdentifierExhausted,
    }
}

fn map_submission_port_runtime_error(
    error: RuntimeError<ProductRegionError>,
) -> SubmissionPortError {
    match error {
        RuntimeError::Recovering => SubmissionPortError::Unavailable,
        RuntimeError::Storage(DriveError::Backend(_)) => SubmissionPortError::Backend,
        RuntimeError::Storage(DriveError::Binding(_)) => SubmissionPortError::Binding,
        RuntimeError::Storage(DriveError::Busy { .. }) => SubmissionPortError::Busy,
        RuntimeError::Storage(DriveError::Faulted(_)) => SubmissionPortError::Faulted,
        RuntimeError::Projection(ProjectorOperationError::Busy { .. }) => SubmissionPortError::Busy,
        RuntimeError::Projection(
            ProjectorOperationError::Faulted(_) | ProjectorOperationError::Rejected(_),
        ) => SubmissionPortError::Faulted,
    }
}

fn validate_partition_table(flash: &mut FlashStorage<'_>) -> Result<(), ProductFlashOpenError> {
    let mut bytes = [0_u8; PARTITION_TABLE_MAX_LEN];
    let table = read_partition_table(flash, &mut bytes)
        .map_err(|_| ProductFlashOpenError::PartitionTable)?;

    let mut identity_count = 0_u8;
    let mut clock_count = 0_u8;
    let mut config_count = 0_u8;
    let mut journal_count = 0_u8;
    let mut identity_valid = false;
    let mut clock_valid = false;
    let mut config_valid = false;
    let mut journal_valid = false;

    for entry in table.iter() {
        let label = entry.label();
        let named_identity = label == NODE_IDENTITY_LABEL_BYTES;
        let named_clock = label == ANNOUNCE_CLOCK_LABEL_BYTES;
        let named_config = label == DEVICE_CONFIG_LABEL_BYTES;
        let named_journal = label == NODE_JOURNAL_LABEL_BYTES;
        if named_identity {
            identity_count = identity_count.saturating_add(1);
        }
        if named_clock {
            clock_count = clock_count.saturating_add(1);
        }
        if named_config {
            config_count = config_count.saturating_add(1);
        }
        if named_journal {
            journal_count = journal_count.saturating_add(1);
        }

        let entry_end = entry
            .offset()
            .checked_add(entry.len())
            .ok_or(ProductFlashOpenError::PartitionTable)?;
        for (name, offset, len, named) in [
            (
                "node_identity",
                NODE_IDENTITY_OFFSET,
                NODE_IDENTITY_LEN,
                named_identity,
            ),
            (
                "announce_clock",
                ANNOUNCE_CLOCK_OFFSET,
                ANNOUNCE_CLOCK_LEN,
                named_clock,
            ),
            (
                "device_config",
                DEVICE_CONFIG_OFFSET,
                DEVICE_CONFIG_LEN,
                named_config,
            ),
            (
                "node_journal",
                NODE_JOURNAL_OFFSET,
                NODE_JOURNAL_LEN,
                named_journal,
            ),
        ] {
            let expected_end = offset + len;
            if entry.offset() < expected_end && offset < entry_end && !named {
                return Err(ProductFlashOpenError::PartitionOverlap(name));
            }
        }

        let plain_writable = !entry.is_read_only() && !entry.is_encrypted() && entry.flags() == 0;
        if named_identity {
            identity_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
                && entry.offset() == NODE_IDENTITY_OFFSET
                && entry.len() == NODE_IDENTITY_LEN
                && plain_writable;
        }
        if named_clock {
            clock_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
                && entry.offset() == ANNOUNCE_CLOCK_OFFSET
                && entry.len() == ANNOUNCE_CLOCK_LEN
                && plain_writable;
        }
        if named_config {
            config_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == NVS_DATA_SUBTYPE
                && entry.offset() == DEVICE_CONFIG_OFFSET
                && entry.len() == DEVICE_CONFIG_LEN
                && plain_writable;
        }
        if named_journal {
            journal_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
                && entry.offset() == NODE_JOURNAL_OFFSET
                && entry.len() == NODE_JOURNAL_LEN
                && plain_writable;
        }
    }

    if identity_count != 1 || clock_count != 1 || config_count != 1 || journal_count != 1 {
        return Err(ProductFlashOpenError::PartitionCardinality {
            identity: identity_count,
            announce_clock: clock_count,
            device_config: config_count,
            node_journal: journal_count,
        });
    }
    for (valid, name) in [
        (identity_valid, "node_identity"),
        (clock_valid, "announce_clock"),
        (config_valid, "device_config"),
        (journal_valid, "node_journal"),
    ] {
        if !valid {
            return Err(ProductFlashOpenError::PartitionShape(name));
        }
    }
    Ok(())
}

const _: () = assert!(mem::size_of::<FlashStorage<'static>>() <= 32);
