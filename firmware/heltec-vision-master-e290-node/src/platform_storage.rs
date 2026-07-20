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
use reticulum_device_api::{CapabilityAvailability, IdentitySummary};
use reticulum_device_api_adapter::{
    InboundMailboxItem, InboundMailboxPort, InboundMailboxPortError, SubmissionAcceptance,
    SubmissionPort, SubmissionPortError,
};
use reticulum_device_api_credential_store::{
    BoundCredentialStore, CredentialStoreBinding, CredentialStoreRecovery, MountedCredentialStore,
};
use reticulum_device_api_credentials::{CredentialId, SelectedCredential};
use reticulum_device_api_handoff::{LocalApiReply, LocalApiRequest};
use reticulum_device_api_pairing::{
    AbortCurrentResponse, AbortResult, ActivateResponse, BeginResponse, DeviceId, PairingRequest,
    PairingResponse, ProofStartResponse,
};
use reticulum_device_api_pairing_policy::{
    AcquirePairingExclusive, ActiveLowButton, ButtonEffect, ConnectionId, ConnectionRefusal,
    ExclusiveAcquireOutcome, MonotonicMillis as PairingMillis, PolicyEvent, WindowClosed,
};
use reticulum_device_api_session::AuthenticatedGrant;
use reticulum_device_identity_store::{
    DurableIdentityMaterial, IdentityBootReport, IdentityPreflight, IdentityStoreError,
    boot_load_or_provision, inspect_identity,
};
use reticulum_heltec_vision_master_e290_node::authenticated_api_node::AuthenticatedApiDispatchFailure;
use reticulum_heltec_vision_master_e290_node::credential_boot::{
    CredentialBootOutcome, CredentialBootState, boot_credentials,
};
use reticulum_heltec_vision_master_e290_node::credential_pairing::{
    AbortAdmission, ActivateAdmission, BeginAdmission, CredentialPairingDriveOutcome,
    CredentialPairingStatus, PairingMutation, ProofStartAdmission,
};
use reticulum_heltec_vision_master_e290_node::credential_runtime::{
    CredentialInitializationStatus, CredentialRuntime, InitializationAccepted,
    InitializationDriveOutcome, InitializationRequestRefusal, MAXIMUM_CREDENTIAL_RUNTIME_BYTES,
    OrdinarySessionSelectionRefusal,
};
use reticulum_heltec_vision_master_e290_node::cross_store_gate::{
    CredentialPhysicalMutationGate, InboundMailboxMutationGate, JournalMutationGate,
    credential_physical_mutation_gate, inbound_mailbox_mutation_gate,
    journal_mutation_gate as cross_store_journal_mutation_gate,
};
use reticulum_heltec_vision_master_e290_node::partition_contract::{
    ANNOUNCE_CLOCK_LABEL_BYTES, ANNOUNCE_CLOCK_LEN, ANNOUNCE_CLOCK_OFFSET,
    API_CREDENTIALS_LABEL_BYTES, API_CREDENTIALS_LEN, API_CREDENTIALS_OFFSET, DATA_PARTITION_TYPE,
    DEVICE_CONFIG_LABEL_BYTES, DEVICE_CONFIG_LEN, DEVICE_CONFIG_OFFSET, MESSAGE_STORE_LABEL_BYTES,
    MESSAGE_STORE_LEN, MESSAGE_STORE_OFFSET, NODE_IDENTITY_LABEL_BYTES, NODE_IDENTITY_LEN,
    NODE_IDENTITY_OFFSET, NODE_JOURNAL_LABEL_BYTES, NODE_JOURNAL_LEN, NODE_JOURNAL_OFFSET,
    NVS_DATA_SUBTYPE, REQUIRED_FLASH_BYTES, UNDEFINED_DATA_SUBTYPE,
};
use reticulum_heltec_vision_master_e290_node::{
    api_credentials_binding, node_journal_binding, rns_inbox_binding,
};
use reticulum_node_core::{
    AuthorizedFrameObservation, MonotonicMillis, MonotonicSeconds, TxLeaseDeadline,
};
use reticulum_nor_flash_region::{PartitionNorFlash, RegionError};
use reticulum_rns_inbox_store::{
    BoundInboxStore, InboxAdmissionError, InboxAdmissionOutcome, InboxCandidate, InboxItemId,
    InboxStoreBinding, InboxStoreMountError, InboxStoreState, MountedInboxStore,
    mount as mount_inbox_store,
};
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
#[cfg(feature = "rns-inbox-commit-fault-hil")]
use reticulum_heltec_vision_master_e290_node::inbox_admission_fault_hil::{
    SuppressThirdWrite, observe_product_quarantine,
};

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
        api_credentials: u8,
        device_config: u8,
        node_journal: u8,
        message_store: u8,
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
                api_credentials,
                device_config,
                node_journal,
                message_store,
            } => write!(
                formatter,
                "partition-cardinality identity={identity} announce_clock={announce_clock} api_credentials={api_credentials} device_config={device_config} node_journal={node_journal} message_store={message_store}"
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

/// Failure while read-only mounting the bounded inbound qualification store.
pub(crate) type BootInboxMountError = InboxStoreMountError<ProductRegionError>;

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

/// Credential admission result plus non-secret physical mutation evidence.
pub(crate) struct BootCredentialStore {
    outcome: CredentialBootOutcome,
    raw_write_calls: u32,
    raw_erase_calls: u32,
}

impl BootCredentialStore {
    pub(crate) const fn state(&self) -> CredentialBootState {
        self.outcome.state()
    }

    pub(crate) const fn binding(&self) -> CredentialStoreBinding {
        self.outcome.binding()
    }

    pub(crate) fn revision(&self) -> Option<u64> {
        self.outcome
            .mounted()
            .map(|mounted| mounted.revision().get())
    }

    pub(crate) fn recovery(&self) -> Option<CredentialStoreRecovery> {
        self.outcome.mounted().map(MountedCredentialStore::recovery)
    }

    pub(crate) const fn completed_recovery_steps(&self) -> u8 {
        self.outcome.completed_recovery_steps()
    }

    pub(crate) const fn raw_write_calls(&self) -> u32 {
        self.raw_write_calls
    }

    pub(crate) const fn raw_erase_calls(&self) -> u32 {
        self.raw_erase_calls
    }
}

/// Unique owner of the one `esp-storage` instance in the permanent image.
pub(crate) struct ProductFlashOwner {
    flash: &'static mut FlashStorage<'static>,
    journal_binding: JournalBinding,
    credential_binding: CredentialStoreBinding,
    inbox_binding: InboxStoreBinding,
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
    credential_runtime: CredentialRuntime,
    runtime: Option<ProductSubmissionRuntime>,
    submission_service_enabled: bool,
    inbox: Option<MountedInboxStore>,
    inbox_service_enabled: bool,
    inbox_dropped_since_boot: u64,
}

/// Short-lived logical API view over fields disjoint from credential authority.
///
/// Constructing this view freezes the credential-mutation fact used by one
/// synchronous adapter dispatch. It never gains access to credential records,
/// pairing policy, or the raw credential partition.
struct ProductSubmissionPort<'a> {
    flash: &'a mut FlashStorage<'static>,
    journal_binding: JournalBinding,
    runtime: &'a mut Option<ProductSubmissionRuntime>,
    submission_service_enabled: bool,
    credential_physical_mutation_outstanding: bool,
}

/// Short-lived read-only logical API view over resident mailbox semantics.
struct ProductInboundMailboxPort<'a> {
    inbox: &'a Option<MountedInboxStore>,
    service_enabled: bool,
    dropped_since_boot: u64,
}

/// Product result of offering one decrypted local DATA item to durable storage.
#[allow(
    clippy::large_enum_variant,
    reason = "allocation-free deferral must return the exact fixed-capacity plaintext owner"
)]
pub(crate) enum ProductInboundAdmission {
    /// The item became visible only after a complete commit and readback.
    Committed(InboxItemId),
    /// Another store retains physical mutation; retry this exact candidate.
    Deferred(InboxCandidate),
    /// The one qualification slot was already occupied; newest input was dropped.
    DroppedFull,
    /// No mounted mailbox service exists; newest input was dropped.
    DroppedUnavailable,
    /// A non-reconciled physical failure disabled inbox service for this boot.
    DroppedFaulted(InboxAdmissionError<ProductRegionError>),
}

/// Product-level result of admission-time identity preflight and initialization policy.
pub(crate) enum ProductInitializationRequest {
    /// Identity media could not be inspected, so no policy request ran.
    IdentityUnavailable,
    /// Retained journal mutation work must settle before policy admission.
    DeferredForJournalMutation,
    /// Fresh identity preflight completed and the resident runtime decided.
    Runtime(Result<InitializationAccepted, InitializationRequestRefusal>),
}

/// Product-level result of one same-boot physical initialization drive.
pub(crate) enum ProductInitializationDrive {
    /// Identity media could not be inspected, so credential media was untouched.
    IdentityUnavailable,
    /// Fresh identity preflight completed and the resident runtime decided.
    Runtime(InitializationDriveOutcome<ProductRegionError>),
}

/// Product-level admission result for one owning live-pairing request.
pub(crate) enum ProductLivePairingAdmission {
    /// Journal mutation is still retained; the exact request owner must retry.
    DeferredForJournalMutation(PairingRequest),
    /// A challenge or coarse refusal is immediately ready for the bearer.
    Immediate(PairingResponse),
    /// One durable mutation was admitted and must be driven before replying.
    MutationAccepted(PairingMutation),
}

/// Product result of attempting one durable-submission runtime step.
pub(crate) enum ProductSubmissionDrive {
    /// One mounted runtime step ran and returned its exact result.
    Runtime(Result<RuntimeStep, RuntimeError<ProductRegionError>>),
    /// Credential initialization, live pairing, or recovery owns mutation access.
    DeferredForCredentialMutation,
    /// Durable submission service has no usable resident runtime.
    RuntimeUnavailable,
}

/// Sole-owner surface invoked by the node-side local pairing-control lane.
pub(crate) trait ProductCredentialInitializationPort {
    /// Current non-secret resident initialization ownership.
    fn initialization_status(&self) -> CredentialInitializationStatus;

    /// Accept one strictly increasing local connection epoch.
    fn pairing_connected(
        &mut self,
        now: PairingMillis,
        connection: ConnectionId,
    ) -> Option<Result<Option<WindowClosed>, ConnectionRefusal>>;

    /// Close one matching local connection without discarding physical ownership.
    fn pairing_disconnected(
        &mut self,
        now: PairingMillis,
        connection: ConnectionId,
    ) -> Option<PolicyEvent>;

    /// Forward one debounced physical-presence sample.
    fn pairing_observe_button(
        &mut self,
        now: PairingMillis,
        level: ActiveLowButton,
    ) -> Option<ButtonEffect>;

    /// Confirm that the local bearer granted exclusive pairing ownership.
    fn pairing_exclusive_acquired(
        &mut self,
        now: PairingMillis,
        effect: AcquirePairingExclusive,
    ) -> Option<ExclusiveAcquireOutcome>;

    /// Poll one physical-presence window deadline.
    fn pairing_poll_timeout(&mut self, now: PairingMillis) -> Option<PolicyEvent>;

    /// Reinspect identity media and request explicit initialization.
    fn request_initialization(
        &mut self,
        now: PairingMillis,
        connection: ConnectionId,
    ) -> ProductInitializationRequest;

    /// Reinspect identity media, construct the exact short-lived credential
    /// view, and advance an already admitted initialization.
    fn drive_initialization(&mut self) -> ProductInitializationDrive;
}

const MAXIMUM_PRODUCT_STORAGE_COORDINATOR_CREDENTIAL_OVERHEAD_BYTES: usize =
    MAXIMUM_CREDENTIAL_RUNTIME_BYTES + 256;
const MAXIMUM_PRODUCT_STORAGE_COORDINATOR_BYTES: usize = config::MAXIMUM_DURABLE_RUNTIME_BYTES
    + MAXIMUM_PRODUCT_STORAGE_COORDINATOR_CREDENTIAL_OVERHEAD_BYTES
    + mem::size_of::<MountedInboxStore>()
    + 128;
const _: () = assert!(
    mem::size_of::<ProductStorageCoordinator>() <= MAXIMUM_PRODUCT_STORAGE_COORDINATOR_BYTES
);

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
            credential_binding: api_credentials_binding(storage_device_id),
            inbox_binding: rns_inbox_binding(storage_device_id),
        })
    }

    /// Mount and perform only explicitly reported credential recovery steps.
    ///
    /// This operation never initializes erased media and always returns a
    /// local-API admission classification so unrelated stores and LoRa startup
    /// can continue after credential-media failure.
    #[inline(never)]
    pub(crate) fn boot_credentials(&mut self) -> BootCredentialStore {
        let region = PartitionNorFlash::new(
            &mut *self.flash,
            API_CREDENTIALS_OFFSET,
            API_CREDENTIALS_LEN,
        );
        let mut access = BoundCredentialStore::new(region, self.credential_binding);
        let outcome = boot_credentials(&mut access);
        let region = access.into_backend();
        BootCredentialStore {
            outcome,
            raw_write_calls: region.write_calls(),
            raw_erase_calls: region.erase_calls(),
        }
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

    /// Mount the inbound qualification store without formatting erased media.
    ///
    /// A fault disables only client inbox service. The caller retains the sole
    /// flash owner and may continue route-only LoRa operation.
    pub(crate) fn mount_inbox(&mut self) -> Result<MountedInboxStore, BootInboxMountError> {
        let region =
            PartitionNorFlash::new(&mut *self.flash, MESSAGE_STORE_OFFSET, MESSAGE_STORE_LEN);
        let mut access = BoundInboxStore::new(region, self.inbox_binding);
        mount_inbox_store(&mut access)
    }

    /// Transfer the sole physical flash owner into its resident coordinator.
    ///
    /// A missing runtime disables only durable local submission service. The
    /// coordinator still retains exclusive flash authority so LoRa routing can
    /// continue without exposing an alias after a journal mount failure.
    pub(crate) fn into_storage_coordinator(
        self,
        runtime: Option<ProductSubmissionRuntime>,
        inbox: Option<MountedInboxStore>,
        credentials: BootCredentialStore,
        device_api_id: DeviceId,
    ) -> ProductStorageCoordinator {
        let credential_runtime = CredentialRuntime::from_boot(
            credentials.outcome,
            self.credential_binding,
            device_api_id,
        );
        let submission_service_enabled = runtime.is_some();
        let inbox_service_enabled = inbox.is_some();
        ProductStorageCoordinator {
            flash: self.flash,
            journal_binding: self.journal_binding,
            credential_runtime,
            runtime,
            submission_service_enabled,
            inbox,
            inbox_service_enabled,
            inbox_dropped_since_boot: 0,
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
    /// Credential boot admission retained by the sole flash coordinator.
    pub(crate) const fn credential_boot_state(&self) -> CredentialBootState {
        self.credential_runtime.credential_boot_state()
    }

    /// Exact credential partition binding retained for later API operations.
    pub(crate) const fn credential_binding(&self) -> CredentialStoreBinding {
        self.credential_runtime.binding()
    }

    /// Non-secret mounted authority revision, when media mounted successfully.
    pub(crate) fn credential_revision(&self) -> Option<u64> {
        self.credential_runtime.revision()
    }

    /// Whether the retained authority may authenticate existing credentials.
    pub(crate) fn credential_authority_publishable(&self) -> bool {
        self.credential_runtime.authority_publishable()
    }

    /// Whether the retained authority is clean enough for a future mutation.
    pub(crate) fn credential_mutation_eligible(&self) -> bool {
        self.credential_runtime.mutation_eligible()
    }

    /// Whether the resident credential state constructed a pairing policy.
    pub(crate) const fn credential_pairing_policy_available(&self) -> bool {
        self.credential_runtime.pairing_policy_available()
    }

    /// Whether the durable local submission runtime mounted successfully.
    pub(crate) const fn submission_service_available(&self) -> bool {
        self.submission_service_enabled && self.runtime.is_some()
    }

    /// Whether the durable inbound qualification store mounted successfully.
    pub(crate) const fn inbox_service_available(&self) -> bool {
        self.inbox_service_enabled && self.inbox.is_some()
    }

    /// Current non-secret resident live-pairing ownership.
    pub(crate) fn live_pairing_status(&self) -> CredentialPairingStatus {
        self.credential_runtime.live_pairing_status()
    }

    /// Permanently close local submission admission and runtime driving for
    /// this boot without dropping backend-independent durable state.
    pub(crate) fn disable_submission_service(&mut self) {
        self.submission_service_enabled = false;
    }

    fn journal_mutation_pending(&self) -> (bool, bool) {
        self.runtime
            .as_ref()
            .map(|runtime| {
                (
                    runtime.storage().pending_kind().is_some(),
                    runtime
                        .storage()
                        .projector()
                        .pending_persistence()
                        .next()
                        .is_some(),
                )
            })
            .unwrap_or((false, false))
    }

    fn credential_physical_mutation_gate(&self) -> CredentialPhysicalMutationGate {
        let (journal_actor_pending, journal_projector_pending) = self.journal_mutation_pending();
        credential_physical_mutation_gate(journal_actor_pending, journal_projector_pending)
    }

    fn journal_mutation_gate(&self) -> JournalMutationGate {
        cross_store_journal_mutation_gate(
            self.submission_service_available(),
            self.credential_runtime
                .credential_physical_mutation_outstanding(),
        )
    }

    fn inbound_mailbox_mutation_gate(&self) -> InboundMailboxMutationGate {
        let (journal_actor_pending, journal_projector_pending) = self.journal_mutation_pending();
        inbound_mailbox_mutation_gate(
            self.inbox_service_available(),
            self.credential_runtime
                .credential_physical_mutation_outstanding(),
            journal_actor_pending,
            journal_projector_pending,
        )
    }

    fn submission_port(&mut self) -> ProductSubmissionPort<'_> {
        let credential_physical_mutation_outstanding = self
            .credential_runtime
            .credential_physical_mutation_outstanding();
        ProductSubmissionPort {
            flash: &mut *self.flash,
            journal_binding: self.journal_binding,
            runtime: &mut self.runtime,
            submission_service_enabled: self.submission_service_enabled,
            credential_physical_mutation_outstanding,
        }
    }

    fn inbox_port(&self) -> ProductInboundMailboxPort<'_> {
        ProductInboundMailboxPort {
            inbox: &self.inbox,
            service_enabled: self.inbox_service_enabled,
            dropped_since_boot: self.inbox_dropped_since_boot,
        }
    }

    /// Offer one exact decrypted DATA item to the one-entry durable mailbox.
    ///
    /// Deferral occurs before any mailbox I/O and returns the complete fixed
    /// candidate. Full, unavailable, and fault outcomes implement the explicit
    /// drop-newest policy and increment its saturating boot-local counter.
    pub(crate) fn offer_inbound(&mut self, candidate: InboxCandidate) -> ProductInboundAdmission {
        match self.inbound_mailbox_mutation_gate() {
            InboundMailboxMutationGate::Ready => {}
            InboundMailboxMutationGate::DeferredForCredentialMutation
            | InboundMailboxMutationGate::DeferredForJournalMutation => {
                return ProductInboundAdmission::Deferred(candidate);
            }
            InboundMailboxMutationGate::RuntimeUnavailable => {
                self.record_inbound_drop();
                return ProductInboundAdmission::DroppedUnavailable;
            }
        }

        let binding = self
            .inbox
            .as_ref()
            .expect("ready inbox gate requires a mounted runtime")
            .binding();
        let region =
            PartitionNorFlash::new(&mut *self.flash, MESSAGE_STORE_OFFSET, MESSAGE_STORE_LEN);
        #[cfg(feature = "rns-inbox-commit-fault-hil")]
        let region = SuppressThirdWrite::new(region);
        let mut access = BoundInboxStore::new(region, binding);
        let outcome = self
            .inbox
            .as_mut()
            .expect("ready inbox gate requires a mounted runtime")
            .accept(&mut access, candidate);
        match outcome {
            Ok(InboxAdmissionOutcome::Accepted(id)) => ProductInboundAdmission::Committed(id),
            Ok(InboxAdmissionOutcome::Full(_candidate)) => {
                self.record_inbound_drop();
                ProductInboundAdmission::DroppedFull
            }
            Err(failure) => {
                let (_candidate, error) = failure.into_parts();
                self.inbox_service_enabled = false;
                self.record_inbound_drop();
                #[cfg(feature = "rns-inbox-commit-fault-hil")]
                observe_product_quarantine(
                    &error,
                    !self.inbox_service_enabled,
                    self.inbox_dropped_since_boot,
                );
                ProductInboundAdmission::DroppedFaulted(error)
            }
        }
    }

    /// Count one input discarded before it could be represented as a store candidate.
    pub(crate) fn record_inbound_drop(&mut self) {
        self.inbox_dropped_since_boot = self.inbox_dropped_since_boot.saturating_add(1);
    }

    /// Dispatch one exact session-authenticated request while credential and
    /// journal and inbox fields remain disjointly borrowed by the sole storage owner.
    /// The public identity summary is copied in from the node owner and is not
    /// stored in or derived by this flash coordinator.
    #[allow(
        clippy::result_large_err,
        reason = "terminal failure must retain the exact allocation-free request owner"
    )]
    pub(crate) fn dispatch_authenticated_request(
        &mut self,
        request: LocalApiRequest<AuthenticatedGrant>,
        identity: IdentitySummary,
    ) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure> {
        let credential_physical_mutation_outstanding = self
            .credential_runtime
            .credential_physical_mutation_outstanding();
        let Self {
            flash,
            journal_binding,
            credential_runtime,
            runtime,
            submission_service_enabled,
            inbox,
            inbox_service_enabled,
            inbox_dropped_since_boot,
        } = self;
        let mut submission_port = ProductSubmissionPort {
            flash,
            journal_binding: *journal_binding,
            runtime,
            submission_service_enabled: *submission_service_enabled,
            credential_physical_mutation_outstanding,
        };
        let mut inbox_port = ProductInboundMailboxPort {
            inbox,
            service_enabled: *inbox_service_enabled,
            dropped_since_boot: *inbox_dropped_since_boot,
        };
        credential_runtime.dispatch_authenticated_request(
            request,
            identity,
            &mut submission_port,
            &mut inbox_port,
        )
    }

    /// Admit one ordinary authenticated session and return its exact selected
    /// zeroizing credential owner without exposing the resident authority.
    pub(crate) fn select_ordinary_session(
        &mut self,
        at: PairingMillis,
        connection: ConnectionId,
        credential_id: CredentialId,
    ) -> Result<SelectedCredential, OrdinarySessionSelectionRefusal> {
        self.credential_runtime
            .select_ordinary_session(at, connection, credential_id)
    }

    /// Admit one transport-neutral live-pairing request without exposing flash.
    ///
    /// Mutation-producing requests retain their complete owner while journal
    /// work is pending. ProofStart remains challenge-only and bypasses the
    /// physical-mutation gate. Accepted mutations return no response until the
    /// caller drives their durable terminal outcome.
    pub(crate) fn request_live_pairing<R>(
        &mut self,
        at: PairingMillis,
        connection: ConnectionId,
        request: PairingRequest,
        rng: &mut R,
    ) -> ProductLivePairingAdmission
    where
        R: RngCore + CryptoRng,
    {
        let mutation_request = !matches!(&request, PairingRequest::ProofStart(_));
        if mutation_request
            && self.credential_physical_mutation_gate()
                == CredentialPhysicalMutationGate::DeferredForJournalMutation
        {
            return ProductLivePairingAdmission::DeferredForJournalMutation(request);
        }

        match request {
            PairingRequest::Begin(request) => {
                let sequence = request.sequence();
                match self
                    .credential_runtime
                    .request_pairing_begin(at, connection, rng)
                {
                    BeginAdmission::Accepted => {
                        ProductLivePairingAdmission::MutationAccepted(PairingMutation::AddPending)
                    }
                    BeginAdmission::Refused(failure) => ProductLivePairingAdmission::Immediate(
                        PairingResponse::Begin(BeginResponse::failure(sequence, failure)),
                    ),
                }
            }
            PairingRequest::ProofStart(request) => {
                let sequence = request.sequence();
                match self
                    .credential_runtime
                    .request_pairing_proof_start(at, connection, &request, rng)
                {
                    ProofStartAdmission::Challenge(challenge) => {
                        ProductLivePairingAdmission::Immediate(PairingResponse::ProofStart(
                            ProofStartResponse::challenge(sequence, challenge),
                        ))
                    }
                    ProofStartAdmission::Refused(failure) => {
                        ProductLivePairingAdmission::Immediate(PairingResponse::ProofStart(
                            ProofStartResponse::failure(sequence, failure),
                        ))
                    }
                }
            }
            PairingRequest::Activate(request) => {
                let sequence = request.sequence();
                match self
                    .credential_runtime
                    .request_pairing_activate(at, connection, request)
                {
                    ActivateAdmission::Accepted => ProductLivePairingAdmission::MutationAccepted(
                        PairingMutation::ActivatePending,
                    ),
                    ActivateAdmission::Refused(failure) => ProductLivePairingAdmission::Immediate(
                        PairingResponse::Activate(ActivateResponse::failure(sequence, failure)),
                    ),
                }
            }
            PairingRequest::AbortCurrent(request) => {
                let sequence = request.sequence();
                match self
                    .credential_runtime
                    .request_pairing_abort_current(at, connection)
                {
                    AbortAdmission::Accepted => {
                        ProductLivePairingAdmission::MutationAccepted(PairingMutation::AbortPending)
                    }
                    AbortAdmission::Refused(failure) => {
                        ProductLivePairingAdmission::Immediate(PairingResponse::AbortCurrent(
                            AbortCurrentResponse::new(sequence, abort_result(failure)),
                        ))
                    }
                }
            }
        }
    }

    /// Advance one cleanup, commit, or reconciliation step through a fresh
    /// operation-scoped credential-store view.
    pub(crate) fn drive_live_pairing(&mut self) -> CredentialPairingDriveOutcome {
        let region = PartitionNorFlash::new(
            &mut *self.flash,
            API_CREDENTIALS_OFFSET,
            API_CREDENTIALS_LEN,
        );
        let mut access = BoundCredentialStore::new(region, self.credential_runtime.binding());
        self.credential_runtime.drive_live_pairing(&mut access)
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
    ///
    /// Credential mutation deferral is explicit and never aliases runtime
    /// absence, so the node retains journal service and retries later.
    pub(crate) fn drive_submission_step<N, R>(
        &mut self,
        node: &mut N,
        rns_now: MonotonicSeconds,
        owner_now: MonotonicMillis,
        deadline: TxLeaseDeadline,
        rng: &mut R,
    ) -> ProductSubmissionDrive
    where
        N: SubmissionNodePort,
        R: RngCore + CryptoRng,
    {
        match self.journal_mutation_gate() {
            JournalMutationGate::Ready => {}
            JournalMutationGate::DeferredForCredentialMutation => {
                return ProductSubmissionDrive::DeferredForCredentialMutation;
            }
            JournalMutationGate::RuntimeUnavailable => {
                return ProductSubmissionDrive::RuntimeUnavailable;
            }
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return ProductSubmissionDrive::RuntimeUnavailable;
        };
        let region =
            PartitionNorFlash::new(&mut *self.flash, NODE_JOURNAL_OFFSET, NODE_JOURNAL_LEN);
        let mut journal = BoundJournal::new(region, self.journal_binding);
        ProductSubmissionDrive::Runtime(runtime.drive_step(
            &mut journal,
            node,
            rns_now,
            owner_now,
            deadline,
            rng,
        ))
    }
}

impl ProductCredentialInitializationPort for ProductStorageCoordinator {
    fn initialization_status(&self) -> CredentialInitializationStatus {
        self.credential_runtime.initialization_status()
    }

    fn pairing_connected(
        &mut self,
        now: PairingMillis,
        connection: ConnectionId,
    ) -> Option<Result<Option<WindowClosed>, ConnectionRefusal>> {
        self.credential_runtime.pairing_connected(now, connection)
    }

    fn pairing_disconnected(
        &mut self,
        now: PairingMillis,
        connection: ConnectionId,
    ) -> Option<PolicyEvent> {
        self.credential_runtime
            .pairing_disconnected(now, connection)
    }

    fn pairing_observe_button(
        &mut self,
        now: PairingMillis,
        level: ActiveLowButton,
    ) -> Option<ButtonEffect> {
        self.credential_runtime.pairing_observe_button(now, level)
    }

    fn pairing_exclusive_acquired(
        &mut self,
        now: PairingMillis,
        effect: AcquirePairingExclusive,
    ) -> Option<ExclusiveAcquireOutcome> {
        self.credential_runtime
            .pairing_exclusive_acquired(now, effect)
    }

    fn pairing_poll_timeout(&mut self, now: PairingMillis) -> Option<PolicyEvent> {
        self.credential_runtime.pairing_poll_timeout(now)
    }

    fn request_initialization(
        &mut self,
        now: PairingMillis,
        connection: ConnectionId,
    ) -> ProductInitializationRequest {
        if matches!(
            self.credential_physical_mutation_gate(),
            CredentialPhysicalMutationGate::DeferredForJournalMutation
        ) {
            return ProductInitializationRequest::DeferredForJournalMutation;
        }

        let identity_preflight = {
            let mut region =
                PartitionNorFlash::new(&mut *self.flash, NODE_IDENTITY_OFFSET, NODE_IDENTITY_LEN);
            inspect_identity(&mut region)
        };
        let identity_ready = match identity_preflight {
            Ok(IdentityPreflight::Committed { .. }) => true,
            Ok(IdentityPreflight::Vacant) => false,
            Err(_) => return ProductInitializationRequest::IdentityUnavailable,
        };
        ProductInitializationRequest::Runtime(self.credential_runtime.request_initialization(
            now,
            connection,
            identity_ready,
        ))
    }

    fn drive_initialization(&mut self) -> ProductInitializationDrive {
        let identity_preflight = {
            let mut region =
                PartitionNorFlash::new(&mut *self.flash, NODE_IDENTITY_OFFSET, NODE_IDENTITY_LEN);
            inspect_identity(&mut region)
        };
        let identity_ready = match identity_preflight {
            Ok(IdentityPreflight::Committed { .. }) => true,
            Ok(IdentityPreflight::Vacant) => false,
            Err(_) => return ProductInitializationDrive::IdentityUnavailable,
        };
        let region = PartitionNorFlash::new(
            &mut *self.flash,
            API_CREDENTIALS_OFFSET,
            API_CREDENTIALS_LEN,
        );
        let mut access = BoundCredentialStore::new(region, self.credential_runtime.binding());
        ProductInitializationDrive::Runtime(
            self.credential_runtime
                .drive_initialization(&mut access, identity_ready),
        )
    }
}

impl SubmissionPort for ProductSubmissionPort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        if !self.submission_service_enabled {
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
        match cross_store_journal_mutation_gate(
            self.submission_service_enabled && self.runtime.is_some(),
            self.credential_physical_mutation_outstanding,
        ) {
            JournalMutationGate::Ready => {}
            JournalMutationGate::DeferredForCredentialMutation => {
                return Err(SubmissionPortError::Busy);
            }
            JournalMutationGate::RuntimeUnavailable => {
                return Err(SubmissionPortError::Unavailable);
            }
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

impl SubmissionPort for ProductStorageCoordinator {
    fn availability(&mut self) -> CapabilityAvailability {
        self.submission_port().availability()
    }

    fn submission_state(
        &mut self,
        principal: PrincipalId,
        id: SubmissionId,
    ) -> Result<Option<LifecycleState>, SubmissionPortError> {
        self.submission_port().submission_state(principal, id)
    }

    fn accept(
        &mut self,
        candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        self.submission_port().accept(candidate)
    }
}

impl InboundMailboxPort for ProductInboundMailboxPort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        if self.service_enabled && self.inbox.is_some() {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    }

    fn status(&mut self) -> Result<reticulum_device_api::RnsInboxStatus, InboundMailboxPortError> {
        if self.availability() != CapabilityAvailability::Available {
            return Err(InboundMailboxPortError::Unavailable);
        }
        let inbox = self
            .inbox
            .as_ref()
            .ok_or(InboundMailboxPortError::Unavailable)?;
        let depth = match inbox.state() {
            InboxStoreState::Empty => 0,
            InboxStoreState::Occupied(_) => 1,
        };
        Ok(reticulum_device_api::RnsInboxStatus {
            depth,
            capacity: 1,
            dropped_since_boot: self.dropped_since_boot,
            max_payload_bytes: reticulum_rns_inbox_store::MAX_PAYLOAD_SIZE as u16,
            durable: true,
        })
    }

    fn peek(&mut self) -> Result<Option<InboundMailboxItem>, InboundMailboxPortError> {
        if self.availability() != CapabilityAvailability::Available {
            return Err(InboundMailboxPortError::Unavailable);
        }
        self.inbox
            .as_ref()
            .ok_or(InboundMailboxPortError::Unavailable)?
            .item()
            .map(|item| {
                InboundMailboxItem::new(
                    core::num::NonZeroU64::new(item.id().get())
                        .expect("inbox-store item identifiers are nonzero"),
                    reticulum_device_api::DestinationHash(*item.destination().as_bytes()),
                    item.payload(),
                )
                .map_err(|_| InboundMailboxPortError::Faulted)
            })
            .transpose()
    }
}

impl InboundMailboxPort for ProductStorageCoordinator {
    fn availability(&mut self) -> CapabilityAvailability {
        self.inbox_port().availability()
    }

    fn status(&mut self) -> Result<reticulum_device_api::RnsInboxStatus, InboundMailboxPortError> {
        self.inbox_port().status()
    }

    fn peek(&mut self) -> Result<Option<InboundMailboxItem>, InboundMailboxPortError> {
        self.inbox_port().peek()
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

const fn abort_result(failure: reticulum_device_api_pairing::PairingFailure) -> AbortResult {
    match failure {
        reticulum_device_api_pairing::PairingFailure::PhysicalPresenceRequired => {
            AbortResult::PhysicalPresenceRequired
        }
        reticulum_device_api_pairing::PairingFailure::Refused => AbortResult::Refused,
        reticulum_device_api_pairing::PairingFailure::Blocked => AbortResult::Blocked,
        reticulum_device_api_pairing::PairingFailure::Unavailable => AbortResult::Unavailable,
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
    let mut credentials_count = 0_u8;
    let mut config_count = 0_u8;
    let mut journal_count = 0_u8;
    let mut message_store_count = 0_u8;
    let mut identity_valid = false;
    let mut clock_valid = false;
    let mut credentials_valid = false;
    let mut config_valid = false;
    let mut journal_valid = false;
    let mut message_store_valid = false;

    for entry in table.iter() {
        let label = entry.label();
        let named_identity = label == NODE_IDENTITY_LABEL_BYTES;
        let named_clock = label == ANNOUNCE_CLOCK_LABEL_BYTES;
        let named_credentials = label == API_CREDENTIALS_LABEL_BYTES;
        let named_config = label == DEVICE_CONFIG_LABEL_BYTES;
        let named_journal = label == NODE_JOURNAL_LABEL_BYTES;
        let named_message_store = label == MESSAGE_STORE_LABEL_BYTES;
        if named_identity {
            identity_count = identity_count.saturating_add(1);
        }
        if named_clock {
            clock_count = clock_count.saturating_add(1);
        }
        if named_credentials {
            credentials_count = credentials_count.saturating_add(1);
        }
        if named_config {
            config_count = config_count.saturating_add(1);
        }
        if named_journal {
            journal_count = journal_count.saturating_add(1);
        }
        if named_message_store {
            message_store_count = message_store_count.saturating_add(1);
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
                "api_credentials",
                API_CREDENTIALS_OFFSET,
                API_CREDENTIALS_LEN,
                named_credentials,
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
            (
                "message_store",
                MESSAGE_STORE_OFFSET,
                MESSAGE_STORE_LEN,
                named_message_store,
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
        if named_credentials {
            credentials_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
                && entry.offset() == API_CREDENTIALS_OFFSET
                && entry.len() == API_CREDENTIALS_LEN
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
        if named_message_store {
            message_store_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
                && entry.offset() == MESSAGE_STORE_OFFSET
                && entry.len() == MESSAGE_STORE_LEN
                && plain_writable;
        }
    }

    if identity_count != 1
        || clock_count != 1
        || credentials_count != 1
        || config_count != 1
        || journal_count != 1
        || message_store_count != 1
    {
        return Err(ProductFlashOpenError::PartitionCardinality {
            identity: identity_count,
            announce_clock: clock_count,
            api_credentials: credentials_count,
            device_config: config_count,
            node_journal: journal_count,
            message_store: message_store_count,
        });
    }
    for (valid, name) in [
        (identity_valid, "node_identity"),
        (clock_valid, "announce_clock"),
        (credentials_valid, "api_credentials"),
        (config_valid, "device_config"),
        (journal_valid, "node_journal"),
        (message_store_valid, "message_store"),
    ] {
        if !valid {
            return Err(ProductFlashOpenError::PartitionShape(name));
        }
    }
    Ok(())
}

const _: () = assert!(mem::size_of::<FlashStorage<'static>>() <= 32);
