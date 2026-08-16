//! Sole ESP flash owner and exact permanent-image partition gate.

use core::num::NonZeroU64;
use core::{
    fmt::{self, Display, Formatter},
    mem,
};

use allocator_api2::boxed::Box;
#[cfg(feature = "appliance")]
use embedded_storage::nor_flash::NorFlash;
use embedded_storage::nor_flash::ReadNorFlash;
use esp_alloc::ExternalMemory;
use esp_bootloader_esp_idf::partitions::{PARTITION_TABLE_MAX_LEN, read_partition_table};
use esp_storage::{FlashStorage, FlashStorageError};
use rand_core::{CryptoRng, RngCore};
use reticulum_announce_clock::{
    BootEpochReservation, FreshClockPolicy, ReserveError, reserve_next_boot_epoch,
};
#[cfg(feature = "appliance")]
use reticulum_ble_bond_store::{
    BleBond, BondStoreCleanup, BondStoreError, BondStoreFault, MountedBondStore,
    cleanup as cleanup_ble_bond_store, clear as clear_ble_bond_store,
    commit_bond as commit_ble_bond_store, mount as mount_ble_bond_store,
};
use reticulum_board_e290_radio::{E290Na915TxPower, E290RadioConfiguration};
#[cfg(feature = "gateway")]
use reticulum_device_api::ReticulumTcpFailure;
use reticulum_device_api::{
    CapabilityAvailability, DestinationHash as ApiDestinationHash, DeviceName as ApiDeviceName,
    DeviceNameSummary, DiagnosticInterfaceKind, GatewayPolicy, IdentityHash as ApiIdentityHash,
    IdentitySummary, IngressObservation as ApiIngressObservation,
    IngressSignal as ApiIngressSignal, LoraRadioProfile as ApiLoraRadioProfile,
    LoraTransmitPowerDbm, LxmfDiscoveredPeer as ApiLxmfDiscoveredPeer,
    LxmfMailboxStatus as ApiLxmfMailboxStatus, LxmfMessageHandle as ApiLxmfMessageHandle,
    LxmfMessageSummary as ApiLxmfMessageSummary,
    LxmfPeerDiscoveryCursor as ApiLxmfPeerDiscoveryCursor, LxmfPeerDiscoveryIncarnation,
    LxmfPeerDiscoveryPage as ApiLxmfPeerDiscoveryPage, LxmfPeerGeneration as ApiLxmfPeerGeneration,
    LxmfReadChunk as ApiLxmfReadChunk, LxmfReadLength as ApiLxmfReadLength,
    MAX_LXMF_READ_CHUNK_BYTES, ManualServiceAnnounceDisposition, NetworkConfigMutation,
    NetworkConfigMutationOutcome, NetworkConfigMutationRequest, NetworkConfigSnapshot,
    NetworkRuntimeStatus, NodeDiagnosticsSnapshot, PrincipalId as ApiPrincipalId,
    ReticulumTcpPeerConfigSummary, ReticulumTcpPeerHostConfigSummary, ReticulumTcpPeerIpv4Address,
    ReticulumTcpPeerState, RmapConfig, RmapLocation, RouteDiagnosticsPage, RouteDiagnosticsRequest,
    WifiCredentialUpdate, WifiNetworkConfigSummary, WifiNetworkProfileId, WifiStationState,
};
use reticulum_device_api_adapter::{
    LxmfComposeAcceptance, LxmfComposePort, LxmfComposePortError, LxmfComposeRequest,
    LxmfInboxPort, LxmfInboxPortError, ManualServiceAnnouncePort, NetworkConfigPort,
    NetworkConfigPortError, NodeDiagnosticsPort, NodeDiagnosticsPortError, NomadFetchPort,
    PeerDiscoveryPort, PeerDiscoveryPortError, ReticulumProbePort, SubmissionAcceptance,
    SubmissionPort, SubmissionPortError,
};
use reticulum_device_api_credential_store::{
    BoundCredentialStore, CredentialStoreBinding, CredentialStoreRecovery, MountedCredentialStore,
};
use reticulum_device_api_credentials::{CredentialId, SelectedCredential};
use reticulum_device_api_handoff::{LocalApiReply, LocalApiRequest};
use reticulum_device_api_pairing::{
    AbortCurrentResponse, AbortResult, ActivateResponse, BearerBinding, BeginResponse, DeviceId,
    PairingRequest, PairingResponse, ProofStartResponse,
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
use reticulum_e290_firmware::announce_time::{
    ManualAnnounceRequestDisposition, ManualAnnounceSchedule,
};
use reticulum_e290_firmware::authenticated_api_node::AuthenticatedApiDispatchFailure;
use reticulum_e290_firmware::credential_boot::{
    CredentialBootOutcome, CredentialBootState, boot_credentials,
};
use reticulum_e290_firmware::credential_pairing::{
    AbortAdmission, ActivateAdmission, BeginAdmission, CredentialPairingDriveOutcome,
    CredentialPairingStatus, PairingMutation, ProofStartAdmission,
};
use reticulum_e290_firmware::credential_runtime::{
    CredentialInitializationStatus, CredentialRuntime, InitializationAccepted,
    InitializationDriveOutcome, InitializationRequestRefusal, MAXIMUM_CREDENTIAL_RUNTIME_BYTES,
    OrdinarySessionSelectionRefusal,
};
use reticulum_e290_firmware::cross_store_gate::{
    CredentialPhysicalMutationGate, JournalMutationGate, JournalProjectionGate, LxmfMutationGate,
    credential_physical_mutation_gate, journal_mutation_gate as cross_store_journal_mutation_gate,
    journal_projection_gate, lxmf_mutation_gate as cross_store_lxmf_mutation_gate,
};
use reticulum_e290_firmware::partition_contract::{
    ANNOUNCE_CLOCK_LABEL_BYTES, ANNOUNCE_CLOCK_LEN, ANNOUNCE_CLOCK_OFFSET,
    API_CREDENTIALS_LABEL_BYTES, API_CREDENTIALS_LEN, API_CREDENTIALS_OFFSET, BLE_BOND_LABEL_BYTES,
    BLE_BOND_LEN, BLE_BOND_OFFSET, DATA_PARTITION_TYPE, DEVICE_CONFIG_LABEL_BYTES,
    DEVICE_CONFIG_LEN, DEVICE_CONFIG_OFFSET, LXMF_MAILBOX_STATE_LEN, LXMF_MAILBOX_STATE_OFFSET,
    LXMF_STORE_LABEL_BYTES, LXMF_STORE_LEN, LXMF_STORE_OFFSET, NETWORK_CONFIG_LEN,
    NETWORK_CONFIG_OFFSET, NODE_IDENTITY_LABEL_BYTES, NODE_IDENTITY_LEN, NODE_IDENTITY_OFFSET,
    NODE_JOURNAL_LABEL_BYTES, NODE_JOURNAL_LEN, NODE_JOURNAL_OFFSET, REQUIRED_FLASH_BYTES,
    UNDEFINED_DATA_SUBTYPE,
};
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_tcp_profile::{
    WifiTcpBootstrap, WifiTcpFailure, WifiTcpPhase, WifiTcpStatusCell,
};
use reticulum_e290_firmware::{
    api_credentials_binding, lxmf_mailbox_store_binding, lxmf_store_binding,
    network_config_binding, node_journal_binding,
};
use reticulum_lxmf_durable_ingress::{
    DurableIngressOutcome, DurableIngressProofMode, commit_application_event,
};
use reticulum_lxmf_ingress::{
    LocalDeliveryDestination, SourceIdentityResolver, StampPolicy, WireLimits,
};
use reticulum_lxmf_mailbox_store::{
    BoundMailboxStore, MailboxStoreBinding, MailboxStoreCursor, MailboxStoreError,
    MailboxStoreFault, MailboxStoreMount, MountedMailboxStore,
    acknowledge_through as acknowledge_mailbox_through, mount as mount_mailbox_store,
    provision as provision_mailbox_store,
};
use reticulum_lxmf_model::{DurableMessageReceipt, MessageHandle, MessageId};
use reticulum_lxmf_store::{
    BoundLxmfStore, LxmfStoreBinding, LxmfStoreIndexSlot, LxmfStoreMountError, LxmfWireReadError,
    MountedLxmfStore, mount as mount_lxmf_store,
};
use reticulum_network_config_store::{
    BoundNetworkConfigStore, DeviceName as StoredDeviceName,
    LoraRadioProfile as StoredLoraRadioProfile, LoraTxPower, MountedNetworkConfigStore,
    NetworkConfig, NetworkConfigStoreBinding, NetworkConfigStoreError, NetworkConfigStoreFault,
    OutboundTcpPeer, OutboundTcpPeerAddress, PhoneLocation, RedactedNetworkConfig, WifiProfile,
    WifiProfileId, commit_successor as commit_network_config_successor,
    mount as mount_network_config_store, provision_erased as provision_network_config_erased,
};
use reticulum_node_core::{
    ApplicationEventLease, AuthorizedFrameObservation, DelayedProofOwner,
    DestinationHash as NodeDestinationHash, LinkHandle, MAX_DIRECT_LXMF_WIRE, MonotonicMillis,
    MonotonicSeconds, PacketInterfaceId, PrepareBasicLxmfError, PreparedPacket, TxLeaseDeadline,
};
use reticulum_nor_flash_region::{PartitionNorFlash, RegionError};
use reticulum_peer_discovery::{DiscoveredPeers, DiscoveryCursor};
use reticulum_storage_actor::{
    AcceptanceProgress, BootRecoveryProgress, BoundJournal, DriveError, JournalBinding, MountError,
    ProjectorOperationError, StorageDeviceId,
};
use reticulum_storage_journal::{
    FirstProvisionError, FreshJournalPolicy, JournalState, provision_first,
};
use reticulum_storage_model::{
    AcceptOutcome, AcceptanceCandidate, LifecycleState, LxmfMessageIntent, PrincipalId,
    SubmissionId,
};
use reticulum_submission_runtime::{
    FrameOfferProgress, LinkEstablishmentControlError, LinkEstablishmentOffer,
    PathDiscoveryAcknowledgeError, PathDiscoveryOffer, RecoveryStep, RuntimeControlError,
    RuntimeError, RuntimeStep, SubmissionNodePort, SubmissionRuntime,
};
use zeroize::Zeroizing;

use crate::{ProductSupervisor, config};
use reticulum_e290_firmware::diagnostics_api::{
    diagnostic_interface_record, node_diagnostics as project_node_diagnostics,
    radio_trace_page as project_radio_trace_page,
    route_diagnostics_page as project_route_diagnostics_page,
};
use reticulum_e290_firmware::durability_boot::JournalBootPolicy;
use reticulum_e290_firmware::network_config_replay::NetworkConfigMutationReceipt;
use reticulum_e290_firmware::radio_diagnostics::{RadioDiagnosticsCell, RadioRuntimeState};
use reticulum_e290_firmware::radio_trace::RadioTraceCursor;
use reticulum_e290_firmware::rmap_discovery::RmapDiscoveryStatusCell;
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_station_profile::{WifiStationPhase, WifiStationStatusCell};
use reticulum_tx_supervisor::{LxmfMessageLocation, RouteDiagnosticsSnapshot};

type ProductRegionError = RegionError<FlashStorageError>;
pub(crate) type ProductSubmissionRuntime = SubmissionRuntime<
    { config::DURABLE_SUBMISSIONS },
    { config::DURABLE_PROJECTED_SUBMISSIONS },
    { config::LINKS },
>;

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
        ble_bond: u8,
        device_config: u8,
        node_journal: u8,
        lxmf_store: u8,
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
                ble_bond,
                device_config,
                node_journal,
                lxmf_store,
            } => write!(
                formatter,
                "partition-cardinality identity={identity} announce_clock={announce_clock} api_credentials={api_credentials} ble_bond={ble_bond} device_config={device_config} node_journal={node_journal} lxmf_store={lxmf_store}"
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

/// Failure while read-only mounting the append-only LXMF store.
pub(crate) type BootLxmfMountError = LxmfStoreMountError<ProductRegionError>;

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
    pub(crate) pending_lxmf_submissions: usize,
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

/// Failure while strictly read-only mounting the dedicated BLE bond store.
#[cfg(feature = "appliance")]
pub(crate) type BootBleBondMountError = BondStoreError<ProductRegionError>;

/// Boot bond-store result retaining sole ownership of secret BLE bond material.
///
/// The type intentionally implements neither `Clone` nor `Debug`. Moving the
/// optional bond out consumes this owner and the bond itself zeroizes on drop.
/// An ordinary boot mount reports zero mutations. An explicitly authorized
/// bond-only recovery may instead report the bounded writes and erases used to
/// commit a clear tombstone and scrub its non-authoritative predecessor.
#[must_use = "boot-loaded BLE bond ownership must be deliberately transferred or dropped"]
#[cfg(feature = "appliance")]
pub(crate) struct BootBleBondStore {
    mounted: MountedBondStore,
    raw_write_calls: u32,
    raw_erase_calls: u32,
}

#[cfg(feature = "appliance")]
impl BootBleBondStore {
    /// Authoritative nonzero generation, or `None` for erased media.
    pub(crate) const fn generation(&self) -> Option<NonZeroU64> {
        self.mounted.generation()
    }

    /// Whether one authenticated bond was loaded.
    pub(crate) const fn bond_present(&self) -> bool {
        self.mounted.bond().is_some()
    }

    /// Optional non-authoritative cleanup reported by the read-only mount.
    pub(crate) const fn cleanup(&self) -> BondStoreCleanup {
        self.mounted.cleanup()
    }

    /// Raw NOR writes observed during this boot bond-store operation.
    ///
    /// This remains zero for the ordinary read-only mount.
    pub(crate) const fn raw_write_calls(&self) -> u32 {
        self.raw_write_calls
    }

    /// Raw NOR erases observed during this boot bond-store operation.
    ///
    /// This remains zero for the ordinary read-only mount.
    pub(crate) const fn raw_erase_calls(&self) -> u32 {
        self.raw_erase_calls
    }

    /// Consume this boot owner and move out the optional secret bond.
    pub(crate) fn into_bond(self) -> Option<BleBond> {
        self.mounted.into_bond()
    }
}

#[cfg(feature = "appliance")]
fn reconcile_bondless_after_operation_error<F>(
    flash: &mut F,
    operation_error: BondStoreError<F::Error>,
) -> Result<MountedBondStore, BondStoreError<F::Error>>
where
    F: NorFlash,
{
    match mount_ble_bond_store(flash) {
        Ok(mounted) if mounted.bond().is_none() => Ok(mounted),
        Ok(mounted) => {
            // An authorized clear must never republish its predecessor. Drop
            // the remounted owner here so its key material is zeroized, and
            // retain the operation failure as the terminal classification.
            drop(mounted);
            Err(operation_error)
        }
        Err(reconcile_error) => Err(reconcile_error),
    }
}

#[cfg(feature = "appliance")]
fn clear_and_cleanup_mounted_ble_bond<F>(
    flash: &mut F,
    mut mounted: MountedBondStore,
) -> Result<MountedBondStore, BondStoreError<F::Error>>
where
    F: NorFlash,
{
    let bond_present = mounted.bond().is_some();
    if !bond_present && mounted.cleanup() == BondStoreCleanup::Clean {
        // Repeated physical recovery on an already clean, bondless store is
        // deliberately wear-free and does not advance its generation.
        return Ok(mounted);
    }

    if bond_present {
        // Do not retain a second in-memory owner while clear() remounts the
        // current generation internally.
        drop(mounted);
        mounted = match clear_ble_bond_store(flash) {
            Ok(cleared) => cleared,
            Err(error) => reconcile_bondless_after_operation_error(flash, error)?,
        };
    }

    if mounted.bond().is_some() {
        drop(mounted);
        return Err(BondStoreError::Fault(
            BondStoreFault::SuccessorVerificationFailed,
        ));
    }

    if mounted.cleanup() == BondStoreCleanup::Clean {
        return Ok(mounted);
    }

    // A committed clear is already authoritative. Cleanup erases only its
    // inactive predecessor. If that erase reports an error, remount once and
    // accept only a still-bondless authority; never fall back to the old key.
    drop(mounted);
    match cleanup_ble_bond_store(flash) {
        Ok(cleaned) if cleaned.bond().is_none() => Ok(cleaned),
        Ok(cleaned) => {
            drop(cleaned);
            Err(BondStoreError::Fault(
                BondStoreFault::SuccessorVerificationFailed,
            ))
        }
        Err(error) => reconcile_bondless_after_operation_error(flash, error),
    }
}

/// Failure while read-only mounting the bounded network-configuration store.
pub(crate) type BootNetworkConfigMountError = NetworkConfigStoreError<ProductRegionError>;

/// Resident network-configuration authority retained by the sole flash owner.
///
/// Exactly erased media remains available for an explicit first commit. A
/// mounted state owns WPA2 passphrases and intentionally implements neither
/// `Clone` nor `Debug`.
#[allow(
    clippy::large_enum_variant,
    reason = "the allocation-free sole flash owner intentionally retains the bounded secret-bearing snapshot inline"
)]
enum ProductNetworkConfigState {
    FreshErased { binding: NetworkConfigStoreBinding },
    Mounted(MountedNetworkConfigStore),
    Unavailable { binding: NetworkConfigStoreBinding },
}

impl ProductNetworkConfigState {
    const fn unavailable(binding: NetworkConfigStoreBinding) -> Self {
        Self::Unavailable { binding }
    }

    const fn binding(&self) -> NetworkConfigStoreBinding {
        match self {
            Self::FreshErased { binding } | Self::Unavailable { binding } => *binding,
            Self::Mounted(mounted) => mounted.revision().binding(),
        }
    }

    const fn available(&self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }

    fn generation(&self) -> Option<u64> {
        match self {
            Self::Mounted(mounted) => Some(mounted.revision().generation().get()),
            Self::FreshErased { .. } | Self::Unavailable { .. } => None,
        }
    }

    fn wifi_profile_count(&self) -> usize {
        match self {
            Self::Mounted(mounted) => mounted.configuration().wifi_profile_count(),
            Self::FreshErased { .. } | Self::Unavailable { .. } => 0,
        }
    }

    fn tcp_peer_configured(&self) -> bool {
        match self {
            Self::Mounted(mounted) => mounted.configuration().tcp_peer().is_some(),
            Self::FreshErased { .. } | Self::Unavailable { .. } => false,
        }
    }

    fn configuration(&self) -> Option<&NetworkConfig> {
        match self {
            Self::Mounted(mounted) => Some(mounted.configuration()),
            Self::FreshErased { .. } | Self::Unavailable { .. } => None,
        }
    }

    fn redacted(&self) -> RedactedNetworkConfig {
        match self {
            Self::Mounted(mounted) => mounted.redacted(),
            Self::FreshErased { .. } | Self::Unavailable { .. } => {
                reticulum_network_config_store::NetworkConfig::empty().redacted()
            }
        }
    }
}

/// Read-only boot inspection of the network-configuration range.
#[must_use = "boot-loaded network configuration must be transferred to the sole coordinator"]
pub(crate) struct BootNetworkConfigStore {
    state: ProductNetworkConfigState,
    raw_write_calls: u32,
    raw_erase_calls: u32,
}

impl BootNetworkConfigStore {
    /// Construct the disabled state after a mount error.
    pub(crate) const fn unavailable(binding: NetworkConfigStoreBinding) -> Self {
        Self {
            state: ProductNetworkConfigState::unavailable(binding),
            raw_write_calls: 0,
            raw_erase_calls: 0,
        }
    }

    /// Whether media is exactly erased and ready for an explicit first commit.
    pub(crate) const fn is_fresh_erased(&self) -> bool {
        matches!(&self.state, ProductNetworkConfigState::FreshErased { .. })
    }

    /// Mounted nonzero generation, or `None` for fresh/disabled media.
    pub(crate) fn generation(&self) -> Option<u64> {
        self.state.generation()
    }

    /// Number of configured Wi-Fi station profiles.
    pub(crate) fn wifi_profile_count(&self) -> usize {
        self.state.wifi_profile_count()
    }

    /// Whether one outbound Reticulum TCP peer is configured.
    pub(crate) fn tcp_peer_configured(&self) -> bool {
        self.state.tcp_peer_configured()
    }

    /// Raw NOR writes observed during read-only boot mount.
    pub(crate) const fn raw_write_calls(&self) -> u32 {
        self.raw_write_calls
    }

    /// Raw NOR erases observed during read-only boot mount.
    pub(crate) const fn raw_erase_calls(&self) -> u32 {
        self.raw_erase_calls
    }

    fn into_state(self) -> ProductNetworkConfigState {
        self.state
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "appliance")]
struct ProductBleBondStoreState {
    available: bool,
    generation: Option<NonZeroU64>,
    bond_present: bool,
}

#[cfg(feature = "appliance")]
impl ProductBleBondStoreState {
    const fn unavailable() -> Self {
        Self {
            available: false,
            generation: None,
            bond_present: false,
        }
    }

    fn mounted(mounted: &MountedBondStore) -> Self {
        Self {
            available: true,
            generation: mounted.generation(),
            bond_present: mounted.bond().is_some(),
        }
    }
}

/// Stage at which a BLE bond persistence attempt became unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "appliance")]
pub(crate) enum ProductBleBondFailureStage {
    /// The candidate could not complete its commit contract.
    Commit,
    /// The immediate fail-closed remount could not recover stable authority.
    Reconcile,
}

/// Result of synchronously persisting one newly authenticated BLE bond.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "appliance")]
pub(crate) enum ProductBleBondCommitOutcome {
    /// The exact candidate committed, read back, and remounted as authority.
    Committed {
        /// Newly authoritative nonzero generation.
        generation: NonZeroU64,
        /// Optional cleanup that may erase only the inactive predecessor.
        cleanup: BondStoreCleanup,
    },
    /// Commit failed but immediate remount recovered stable durable authority.
    ///
    /// The caller must fail closed for the just-completed volatile pairing and
    /// reboot before trusting the controller's resident bond state.
    ReconciledRebootRequired {
        /// Authoritative generation recovered by remount.
        generation: Option<NonZeroU64>,
        /// Whether the recovered authority contains a bond.
        bond_present: bool,
        /// Optional cleanup reported by the recovered state.
        cleanup: BondStoreCleanup,
    },
    /// No trustworthy bond-store authority remains for this boot.
    StoreUnavailable {
        /// Operation stage whose failure disabled the store.
        stage: ProductBleBondFailureStage,
        /// Stable non-secret store fault, or `None` for a backend failure.
        fault: Option<BondStoreFault>,
    },
}

/// Unique owner of the one `esp-storage` instance in the permanent image.
pub(crate) struct ProductFlashOwner {
    flash: &'static mut FlashStorage<'static>,
    journal_binding: JournalBinding,
    credential_binding: CredentialStoreBinding,
    network_config_binding: NetworkConfigStoreBinding,
    lxmf_mailbox_binding: MailboxStoreBinding,
    #[cfg(feature = "appliance")]
    ble_bond_store: ProductBleBondStoreState,
    lxmf_binding: LxmfStoreBinding,
}

/// Resident sole owner of physical flash and durable submission scheduling.
///
/// The runtime retains only backend-independent semantic state. Every bounded
/// operation creates a fresh journal-relative view with the exact physical
/// binding established at mount, so later config and LXMF-store operations
/// can share this coordinator without leaking raw flash access to protocol or
/// interface actors.
pub(crate) struct ProductStorageCoordinator {
    flash: &'static mut FlashStorage<'static>,
    journal_binding: JournalBinding,
    credential_runtime: CredentialRuntime,
    network_config: ProductNetworkConfigState,
    network_config_radio_applied_revision: u64,
    network_config_last_mutation: Option<NetworkConfigMutationReceipt>,
    #[cfg(feature = "gateway")]
    wifi_station_status: &'static WifiStationStatusCell,
    #[cfg(feature = "gateway")]
    wifi_tcp_status: &'static WifiTcpStatusCell,
    rmap_status: &'static RmapDiscoveryStatusCell,
    #[cfg(feature = "appliance")]
    ble_bond_store: ProductBleBondStoreState,
    runtime: Option<&'static mut ProductSubmissionRuntime>,
    submission_service_enabled: bool,
    lxmf: Option<MountedLxmfStore<'static>>,
    lxmf_mailbox: Option<MountedMailboxStore>,
    lxmf_service_enabled: bool,
}

/// Short-lived logical API view over fields disjoint from credential authority.
///
/// Constructing this view freezes the credential-mutation fact used by one
/// synchronous adapter dispatch. It never gains access to credential records,
/// pairing policy, or the raw credential partition.
struct ProductSubmissionPort<'a> {
    flash: &'a mut FlashStorage<'static>,
    journal_binding: JournalBinding,
    runtime: &'a mut Option<&'static mut ProductSubmissionRuntime>,
    submission_service_enabled: bool,
    credential_physical_mutation_outstanding: bool,
    lxmf_mutation_pending: bool,
}

/// One operation-scoped owner joining logical API capabilities that may each
/// need the sole physical flash device.
///
/// Adapter dispatch calls exactly one semantic method per request. Keeping the
/// capabilities on one value therefore preserves exclusive flash ownership
/// without unsafe aliases or exposing raw storage above this module.
struct ProductAuthenticatedApiPort<'a> {
    flash: &'a mut FlashStorage<'static>,
    journal_binding: JournalBinding,
    runtime: &'a mut Option<&'static mut ProductSubmissionRuntime>,
    submission_service_enabled: bool,
    credential_physical_mutation_outstanding: bool,
    journal_actor_mutation_pending: bool,
    journal_projector_mutation_pending: bool,
    network_config: &'a mut ProductNetworkConfigState,
    network_config_radio_applied_revision: u64,
    network_config_last_mutation: &'a mut Option<NetworkConfigMutationReceipt>,
    #[cfg(feature = "gateway")]
    wifi_station_status: &'static WifiStationStatusCell,
    #[cfg(feature = "gateway")]
    wifi_tcp_status: &'static WifiTcpStatusCell,
    rmap_status: &'static RmapDiscoveryStatusCell,
    lxmf: &'a Option<MountedLxmfStore<'static>>,
    lxmf_mailbox: &'a mut Option<MountedMailboxStore>,
    lxmf_service_enabled: bool,
    lxmf_mutation_pending: bool,
    lxmf_compose_service_enabled: bool,
    supervisor: &'a ProductSupervisor,
    radio_diagnostics: &'a RadioDiagnosticsCell,
    route_diagnostics: &'a mut RouteDiagnosticsSnapshot<{ config::PATHS }>,
    discovered_peers: &'a DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    peer_discovery_incarnation: LxmfPeerDiscoveryIncarnation,
    peer_discovery_now_ms: u64,
    manual_announce_schedule: &'a mut ManualAnnounceSchedule,
    announce_now_seconds: u64,
}

/// Product-level result of admission-time identity preflight and initialization policy.
pub(crate) enum ProductInitializationRequest {
    /// Identity media could not be inspected, so no policy request ran.
    IdentityUnavailable,
    /// Retained journal mutation work must settle before policy admission.
    DeferredForJournalMutation,
    /// Retained LXMF mutation work must settle before policy admission.
    DeferredForLxmfMutation,
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
    /// LXMF mutation is still retained; the exact request owner must retry.
    DeferredForLxmfMutation(PairingRequest),
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
    /// An exact LXMF mutation owns physical access until reconciliation.
    DeferredForLxmfMutation,
    /// Durable submission service has no usable resident runtime.
    RuntimeUnavailable,
}

/// Product failure to correlate a dispatched path request with resident
/// discovery state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductPathDiscoveryAcknowledgeError {
    /// Durable submission service no longer has a resident runtime.
    RuntimeUnavailable,
    /// The runtime retained a different destination or request ordinal.
    Runtime(PathDiscoveryAcknowledgeError),
}

/// Product failure to correlate one locally created or dispatched Link with
/// the resident direct-delivery transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductLinkEstablishmentControlError {
    /// Durable submission service no longer has a resident runtime.
    RuntimeUnavailable,
    /// The runtime retained another submission, generation, phase, or Link.
    Runtime(LinkEstablishmentControlError),
}

/// Product result of one exact LXMF application-event ownership attempt.
#[must_use = "a non-durable LXMF result still owns its exact application event"]
#[allow(
    clippy::large_enum_variant,
    reason = "every non-durable outcome must retain the exact no-alloc application-event owner"
)]
pub(crate) enum ProductLxmfAdmission<'owner, 'slots> {
    /// Credential mutation owns the shared flash device; retry the exact lease.
    DeferredForCredentialMutation(ApplicationEventLease<'owner, 'slots>),
    /// Submission-journal mutation owns the shared flash device; retry the exact lease.
    DeferredForJournalMutation(ApplicationEventLease<'owner, 'slots>),
    /// LXMF service is not mounted or has been cleanly closed for this boot.
    RuntimeUnavailable(ApplicationEventLease<'owner, 'slots>),
    /// Portable validation, commit, replay, or retained ownership result.
    Ingress(DurableIngressOutcome<'owner, 'slots, ProductRegionError>),
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
const MAXIMUM_PRODUCT_STORAGE_COORDINATOR_BYTES: usize =
    MAXIMUM_PRODUCT_STORAGE_COORDINATOR_CREDENTIAL_OVERHEAD_BYTES
        + mem::size_of::<ProductNetworkConfigState>()
        + mem::size_of::<MountedLxmfStore<'static>>()
        + mem::size_of::<MountedMailboxStore>()
        + mem::size_of::<Option<&'static mut ProductSubmissionRuntime>>()
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
            network_config_binding: network_config_binding(storage_device_id),
            lxmf_mailbox_binding: lxmf_mailbox_store_binding(storage_device_id),
            #[cfg(feature = "appliance")]
            ble_bond_store: ProductBleBondStoreState::unavailable(),
            lxmf_binding: lxmf_store_binding(storage_device_id),
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

    /// Strictly mount the dedicated BLE bond store without modifying media.
    ///
    /// Corrupt or torn media returns an error and remains unavailable; boot
    /// never auto-recovers, erases, or provisions this secret-bearing range.
    #[inline(never)]
    #[cfg(feature = "appliance")]
    pub(crate) fn boot_ble_bond(&mut self) -> Result<BootBleBondStore, BootBleBondMountError> {
        let mut region = PartitionNorFlash::new(&mut *self.flash, BLE_BOND_OFFSET, BLE_BOND_LEN);
        let mounted = mount_ble_bond_store(&mut region)?;
        self.ble_bond_store = ProductBleBondStoreState::mounted(&mounted);
        Ok(BootBleBondStore {
            mounted,
            raw_write_calls: region.write_calls(),
            raw_erase_calls: region.erase_calls(),
        })
    }

    /// Clear only the already-mounted BLE bond under boot-time physical recovery.
    ///
    /// The caller must establish physical-presence authorization before
    /// invoking this operation. The consumed boot owner is dropped before a
    /// clear attempt, so its secret material cannot survive as a second owner
    /// or later reach the BLE host. An already clean, bondless store returns
    /// unchanged with no write, erase, or generation advance.
    ///
    /// A clear or cleanup backend error is reconciled by one immediate remount.
    /// Only a bondless remount may return success. If the predecessor remains
    /// authoritative or reconciliation itself fails, the store becomes
    /// unavailable for this boot and no old bond owner is returned.
    #[inline(never)]
    #[cfg(feature = "appliance")]
    pub(crate) fn clear_boot_ble_bond_for_physical_recovery(
        &mut self,
        boot: BootBleBondStore,
    ) -> Result<BootBleBondStore, BootBleBondMountError> {
        let result = {
            let mut region =
                PartitionNorFlash::new(&mut *self.flash, BLE_BOND_OFFSET, BLE_BOND_LEN);
            let result = clear_and_cleanup_mounted_ble_bond(&mut region, boot.mounted);
            let raw_write_calls = region.write_calls();
            let raw_erase_calls = region.erase_calls();
            result.map(|mounted| (mounted, raw_write_calls, raw_erase_calls))
        };

        match result {
            Ok((mounted, raw_write_calls, raw_erase_calls)) => {
                self.ble_bond_store = ProductBleBondStoreState::mounted(&mounted);
                Ok(BootBleBondStore {
                    mounted,
                    raw_write_calls,
                    raw_erase_calls,
                })
            }
            Err(error) => {
                self.ble_bond_store = ProductBleBondStoreState::unavailable();
                Err(error)
            }
        }
    }

    /// Exact network-configuration binding retained for boot-failure isolation.
    pub(crate) const fn network_config_binding(&self) -> NetworkConfigStoreBinding {
        self.network_config_binding
    }

    /// Strictly inspect the network-configuration store without modifying it.
    ///
    /// Exactly erased media remains an available empty configuration and is
    /// provisioned only by the first authenticated mutation. Any other mount
    /// error disables only configuration management and Wi-Fi for this boot.
    #[inline(never)]
    pub(crate) fn boot_network_config(
        &mut self,
    ) -> Result<BootNetworkConfigStore, BootNetworkConfigMountError> {
        let region =
            PartitionNorFlash::new(&mut *self.flash, NETWORK_CONFIG_OFFSET, NETWORK_CONFIG_LEN);
        let mut access = BoundNetworkConfigStore::new(region, self.network_config_binding);
        let mounted = mount_network_config_store(&mut access);
        let region = access.into_backend();
        let raw_write_calls = region.write_calls();
        let raw_erase_calls = region.erase_calls();
        let state = match mounted {
            Ok(mounted) => ProductNetworkConfigState::Mounted(mounted),
            Err(NetworkConfigStoreError::Fault(NetworkConfigStoreFault::UnformattedErased)) => {
                ProductNetworkConfigState::FreshErased {
                    binding: self.network_config_binding,
                }
            }
            Err(error) => return Err(error),
        };
        Ok(BootNetworkConfigStore {
            state,
            raw_write_calls,
            raw_erase_calls,
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
    /// while identity remains vacant. This path does not erase any bytes.
    pub(crate) fn provision_node_journal(
        &mut self,
        policy: JournalBootPolicy,
    ) -> Result<Option<BootJournalProvision>, BootJournalProvisionError> {
        match policy {
            JournalBootPolicy::MountExisting => return Ok(None),
            JournalBootPolicy::ProvisionFactoryFirst => {}
        }

        let mut region =
            PartitionNorFlash::new(&mut *self.flash, NODE_JOURNAL_OFFSET, NODE_JOURNAL_LEN);
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
    /// The appliance profile permits a bounded PSRAM-backed history but
    /// deliberately performs no retirement or journal reclamation. The LoRa
    /// dispatcher retains and re-offers complete authorized-frame
    /// observations, and the mounted runtime remains resident rather than
    /// abandoning durable state after boot.
    pub(crate) fn mount_node_runtime(
        &mut self,
        boot_sequence: u64,
        mut storage: Box<mem::MaybeUninit<ProductSubmissionRuntime>, ExternalMemory>,
    ) -> Result<
        (
            Box<ProductSubmissionRuntime, ExternalMemory>,
            BootJournalMountReport,
        ),
        BootJournalMountError,
    > {
        let (runtime, report) = {
            let region =
                PartitionNorFlash::new(&mut *self.flash, NODE_JOURNAL_OFFSET, NODE_JOURNAL_LEN);
            let mut journal = BoundJournal::new(region, self.journal_binding);
            ProductSubmissionRuntime::mount_into(
                storage.as_mut(),
                &mut journal,
                SubmissionId::new(config::FIRST_SUBMISSION_ID),
                boot_sequence,
            )
            .map_err(BootJournalMountError::Mount)?;
            // SAFETY: `SubmissionRuntime::mount_into` writes every field and
            // returns `Ok` only after physical and semantic replay succeeds.
            // Its error contract leaves `storage` uninitialized, and the `?`
            // above returns before this conversion on every error path.
            let mut runtime = unsafe { storage.assume_init() };

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
            let mut pending_lxmf_submissions = 0_usize;
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
                    BootRecoveryProgress::ReplayPendingLxmf => {
                        pending_lxmf_submissions = pending_lxmf_submissions.saturating_add(1);
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
                pending_lxmf_submissions,
                already_final_submissions,
                raw_write_calls: region.write_calls(),
                raw_erase_calls: region.erase_calls(),
            };
            (runtime, report)
        };

        Ok((runtime, report))
    }

    /// Mount the append-only LXMF store into the exact boot-lifetime index.
    ///
    /// Mount is read-only. A failure disables only the future LXMF delivery
    /// service while the sole flash owner continues routing and existing stores.
    pub(crate) fn mount_lxmf(
        &mut self,
        index: &'static mut [LxmfStoreIndexSlot],
    ) -> Result<MountedLxmfStore<'static>, BootLxmfMountError> {
        let region = PartitionNorFlash::new(&mut *self.flash, LXMF_STORE_OFFSET, LXMF_STORE_LEN);
        let mut access = BoundLxmfStore::new(region, self.lxmf_binding);
        mount_lxmf_store(&mut access, index)
    }

    /// Transfer the sole physical flash owner into its resident coordinator.
    ///
    /// A missing runtime disables only durable local submission service. The
    /// coordinator still retains exclusive flash authority so LoRa routing can
    /// continue without exposing an alias after a journal mount failure.
    #[allow(
        clippy::too_many_arguments,
        reason = "boot transfers each independent mounted authority exactly once into the sole physical owner"
    )]
    pub(crate) fn into_storage_coordinator(
        self,
        runtime: Option<&'static mut ProductSubmissionRuntime>,
        lxmf: Option<MountedLxmfStore<'static>>,
        credentials: BootCredentialStore,
        network_config: BootNetworkConfigStore,
        device_api_id: DeviceId,
        #[cfg(feature = "gateway")] wifi_station_status: &'static WifiStationStatusCell,
        #[cfg(feature = "gateway")] wifi_tcp_status: &'static WifiTcpStatusCell,
        rmap_status: &'static RmapDiscoveryStatusCell,
    ) -> ProductStorageCoordinator {
        let lxmf_mailbox = lxmf.as_ref().and_then(|store| {
            mount_or_provision_lxmf_mailbox(&mut *self.flash, self.lxmf_mailbox_binding, store).ok()
        });
        let credential_runtime = CredentialRuntime::from_boot(
            credentials.outcome,
            self.credential_binding,
            device_api_id,
        );
        let submission_service_enabled = runtime.is_some();
        let network_config_radio_applied_revision = network_config.generation().unwrap_or(0);
        // A fresh notification store is durably initialized before ingress
        // opens. If that cannot be established, fail this service closed rather
        // than classifying messages against an uninitialized watermark.
        let lxmf_service_enabled = lxmf.is_some() && lxmf_mailbox.is_some();
        ProductStorageCoordinator {
            flash: self.flash,
            journal_binding: self.journal_binding,
            credential_runtime,
            network_config: network_config.into_state(),
            network_config_radio_applied_revision,
            network_config_last_mutation: None,
            #[cfg(feature = "gateway")]
            wifi_station_status,
            #[cfg(feature = "gateway")]
            wifi_tcp_status,
            rmap_status,
            #[cfg(feature = "appliance")]
            ble_bond_store: self.ble_bond_store,
            runtime,
            submission_service_enabled,
            lxmf,
            lxmf_mailbox,
            lxmf_service_enabled,
        }
    }
}

fn mount_or_provision_lxmf_mailbox(
    flash: &mut FlashStorage<'static>,
    binding: MailboxStoreBinding,
    lxmf: &MountedLxmfStore<'_>,
) -> Result<MountedMailboxStore, MailboxStoreError<ProductRegionError>> {
    let region = PartitionNorFlash::new(flash, LXMF_MAILBOX_STATE_OFFSET, LXMF_MAILBOX_STATE_LEN);
    let mut access = BoundMailboxStore::new(region, binding);
    match mount_mailbox_store(&mut access)? {
        MailboxStoreMount::Mounted(mounted) => {
            if mailbox_cursor_matches_lxmf(lxmf, mounted.acknowledged_cursor()) {
                Ok(mounted)
            } else {
                Err(MailboxStoreError::Fault(
                    MailboxStoreFault::CursorIdentityConflict,
                ))
            }
        }
        MailboxStoreMount::Unprovisioned => match provision_mailbox_store(&mut access) {
            Ok(mounted) => Ok(mounted),
            Err(error) => match mount_mailbox_store(&mut access) {
                Ok(MailboxStoreMount::Mounted(mounted))
                    if mounted.acknowledged_cursor().is_none() =>
                {
                    Ok(mounted)
                }
                _ => Err(error),
            },
        },
    }
}

fn mailbox_cursor_from_lxmf_receipt(receipt: DurableMessageReceipt) -> MailboxStoreCursor {
    MailboxStoreCursor::new(
        NonZeroU64::new(receipt.handle().get())
            .expect("a durable LXMF receipt always carries a nonzero handle"),
        *receipt.message_id().as_bytes(),
        *receipt.fingerprint().exact_wire_digest().as_bytes(),
    )
}

fn mailbox_cursor_matches_lxmf(
    lxmf: &MountedLxmfStore<'_>,
    cursor: Option<MailboxStoreCursor>,
) -> bool {
    let Some(cursor) = cursor else {
        return true;
    };
    let Ok(handle) = MessageHandle::new(cursor.handle().get()) else {
        return false;
    };
    lxmf.receipt(handle)
        .map(mailbox_cursor_from_lxmf_receipt)
        .is_some_and(|committed| committed == cursor)
}

impl ProductStorageCoordinator {
    /// Whether network configuration is mounted or exactly erased and ready
    /// for an explicit first authenticated commit.
    pub(crate) const fn network_config_store_available(&self) -> bool {
        self.network_config.available()
    }

    /// Current durable network-configuration generation.
    pub(crate) fn network_config_generation(&self) -> Option<u64> {
        self.network_config.generation()
    }

    /// Number of board-owned Wi-Fi profiles in the durable configuration.
    pub(crate) fn network_config_wifi_profile_count(&self) -> usize {
        self.network_config.wifi_profile_count()
    }

    /// Whether one outbound Reticulum TCP peer is durably configured.
    pub(crate) fn network_config_tcp_peer_configured(&self) -> bool {
        self.network_config.tcp_peer_configured()
    }

    /// Boot-applied policy for routine primary/LXMF/NomadNet announces.
    pub(crate) fn automatic_announces_enabled(&self) -> bool {
        self.network_config
            .configuration()
            .is_none_or(NetworkConfig::automatic_announces_enabled)
    }

    /// Whether this boot should publish the LoRa interface through RNS discovery.
    pub(crate) fn rmap_discovery_enabled(&self) -> bool {
        self.network_config
            .configuration()
            .is_some_and(NetworkConfig::rmap_discovery_enabled)
    }

    /// Public phone location selected for this boot's RMAP payload.
    pub(crate) fn rmap_public_location(
        &self,
    ) -> Option<reticulum_network_config_store::PhoneLocation> {
        let configuration = self.network_config.configuration()?;
        if configuration.rmap_share_location() {
            configuration.phone_location()
        } else {
            None
        }
    }

    /// Copy the immutable boot-selected LoRa profile before radio construction.
    pub(crate) fn lora_radio_profile(&self) -> StoredLoraRadioProfile {
        self.network_config
            .configuration()
            .map_or(StoredLoraRadioProfile::DEFAULT, NetworkConfig::lora_profile)
    }

    /// Copy the immutable boot-selected board display name, if configured.
    pub(crate) fn device_name(&self) -> Option<StoredDeviceName> {
        self.network_config
            .configuration()
            .and_then(NetworkConfig::device_name)
    }

    /// Copy the immutable boot-time Wi-Fi decision out of durable authority
    /// before the independent station actor starts.
    #[cfg(feature = "gateway")]
    pub(crate) fn wifi_station_boot_plan(
        &self,
    ) -> Option<reticulum_e290_firmware::wifi_station_profile::WifiStationBootPlan> {
        match &self.network_config {
            ProductNetworkConfigState::Mounted(mounted) => Some(
                reticulum_e290_firmware::wifi_station_profile::WifiStationBootPlan::select(
                    mounted.revision().generation().get(),
                    mounted.configuration(),
                ),
            ),
            ProductNetworkConfigState::FreshErased { .. } => Some(
                reticulum_e290_firmware::wifi_station_profile::WifiStationBootPlan::Disabled {
                    applied_revision: 0,
                },
            ),
            ProductNetworkConfigState::Unavailable { .. } => None,
        }
    }

    /// Copy the enabled immutable outbound TCP endpoint from durable boot
    /// authority before interface actor 2 starts.
    #[cfg(feature = "gateway")]
    pub(crate) fn wifi_tcp_bootstrap(&self) -> Option<WifiTcpBootstrap> {
        let ProductNetworkConfigState::Mounted(mounted) = &self.network_config else {
            return None;
        };
        if !mounted.configuration().wifi_transport_enabled() {
            return None;
        }
        let peer = mounted
            .configuration()
            .tcp_peer()
            .filter(|peer| peer.enabled())?;
        let revision = mounted.revision().generation().get();
        match peer.address() {
            OutboundTcpPeerAddress::Ipv4(ipv4) => {
                WifiTcpBootstrap::new(revision, ipv4, peer.port()).ok()
            }
            OutboundTcpPeerAddress::Dns(hostname) => {
                WifiTcpBootstrap::with_dns(revision, hostname.as_bytes(), peer.port()).ok()
            }
        }
    }

    /// Whether the dedicated BLE bond store mounted and remains trustworthy.
    #[cfg(feature = "appliance")]
    pub(crate) const fn ble_bond_store_available(&self) -> bool {
        self.ble_bond_store.available
    }

    /// Whether current durable BLE bond authority contains one bond.
    #[cfg(feature = "appliance")]
    pub(crate) const fn ble_bond_present(&self) -> bool {
        self.ble_bond_store.available && self.ble_bond_store.bond_present
    }

    /// Current durable BLE bond-store generation, if any.
    #[cfg(feature = "appliance")]
    pub(crate) const fn ble_bond_generation(&self) -> Option<NonZeroU64> {
        self.ble_bond_store.generation
    }

    /// Commit one newly authenticated BLE bond before acknowledging pairing.
    ///
    /// Store success includes exact candidate readback and an exact remount.
    /// Any failure is synchronously remounted once. A recovered durable state
    /// still requires fail-closed pairing teardown and reboot because the
    /// controller's volatile bond may not match that recovered authority.
    #[inline(never)]
    #[cfg(feature = "appliance")]
    pub(crate) fn commit_ble_bond(&mut self, bond: BleBond) -> ProductBleBondCommitOutcome {
        if !self.ble_bond_store.available {
            return ProductBleBondCommitOutcome::StoreUnavailable {
                stage: ProductBleBondFailureStage::Commit,
                fault: None,
            };
        }

        let (state, outcome) = {
            let mut region =
                PartitionNorFlash::new(&mut *self.flash, BLE_BOND_OFFSET, BLE_BOND_LEN);
            match commit_ble_bond_store(&mut region, bond) {
                Ok(mounted) => {
                    let state = ProductBleBondStoreState::mounted(&mounted);
                    let generation = mounted
                        .generation()
                        .expect("a successful bond commit always has a nonzero generation");
                    (
                        state,
                        ProductBleBondCommitOutcome::Committed {
                            generation,
                            cleanup: mounted.cleanup(),
                        },
                    )
                }
                Err(_commit_error) => match mount_ble_bond_store(&mut region) {
                    Ok(mounted) => {
                        let state = ProductBleBondStoreState::mounted(&mounted);
                        (
                            state,
                            ProductBleBondCommitOutcome::ReconciledRebootRequired {
                                generation: mounted.generation(),
                                bond_present: mounted.bond().is_some(),
                                cleanup: mounted.cleanup(),
                            },
                        )
                    }
                    Err(error) => (
                        ProductBleBondStoreState::unavailable(),
                        ProductBleBondCommitOutcome::StoreUnavailable {
                            stage: ProductBleBondFailureStage::Reconcile,
                            fault: bond_store_fault(error),
                        },
                    ),
                },
            }
        };
        self.ble_bond_store = state;
        outcome
    }

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

    /// Active application credentials in the publishable authority, or `None`
    /// when no authority may currently authenticate.
    #[cfg(feature = "display")]
    pub(crate) fn active_credential_count(&self) -> Option<usize> {
        self.credential_runtime.active_credential_count()
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

    /// Whether the next runtime step can emit an ordinary Link-retirement
    /// action and therefore requires a free local action-retention lane.
    pub(crate) fn direct_link_retirement_is_next_step(&self) -> bool {
        self.submission_service_enabled
            && self
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.direct_link_retirement_is_next_step())
    }

    /// Whether the append-only LXMF store mounted into its external index.
    ///
    /// This reports mounted store readiness. `main` uses it later to gate
    /// construction of the local `lxmf.delivery` destination and admission.
    pub(crate) const fn lxmf_service_available(&self) -> bool {
        self.lxmf_service_enabled && self.lxmf.is_some() && self.lxmf_mailbox.is_some()
    }

    /// Cheap in-memory LXMF mailbox collection snapshot for display/runtime use.
    #[cfg(feature = "display")]
    pub(crate) fn lxmf_mailbox_status(&self) -> Option<ApiLxmfMailboxStatus> {
        project_lxmf_mailbox_status(self.lxmf.as_ref()?, self.lxmf_mailbox.as_ref()?)
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
        credential_physical_mutation_gate(
            journal_actor_pending,
            journal_projector_pending,
            self.lxmf_mutation_pending(),
        )
    }

    fn journal_mutation_gate(&self) -> JournalMutationGate {
        cross_store_journal_mutation_gate(
            self.submission_service_available(),
            self.credential_runtime
                .credential_physical_mutation_outstanding(),
            self.lxmf_mutation_pending(),
        )
    }

    fn lxmf_mutation_pending(&self) -> bool {
        self.lxmf
            .as_ref()
            .is_some_and(MountedLxmfStore::has_pending_mutation)
    }

    fn lxmf_mutation_gate(&self) -> LxmfMutationGate {
        let (journal_actor_pending, journal_projector_pending) = self.journal_mutation_pending();
        cross_store_lxmf_mutation_gate(
            self.lxmf_service_available(),
            self.credential_runtime
                .credential_physical_mutation_outstanding(),
            journal_actor_pending,
            journal_projector_pending,
            self.lxmf_mutation_pending(),
        )
    }

    /// Message whose exact retry owns ambiguous LXMF physical mutation.
    pub(crate) fn lxmf_pending_message_id(&self) -> Option<MessageId> {
        self.lxmf
            .as_ref()
            .and_then(MountedLxmfStore::pending_message_id)
    }

    /// Close future LXMF admission only when no ambiguous mutation needs retry.
    ///
    /// Returning `false` preserves the service and its exact reconciliation
    /// owner; disabling while pending would deadlock every other flash store.
    pub(crate) fn disable_lxmf_service_if_clean(&mut self) -> bool {
        if self.lxmf_mutation_pending() {
            false
        } else {
            self.lxmf_service_enabled = false;
            true
        }
    }

    fn submission_port(&mut self) -> ProductSubmissionPort<'_> {
        let credential_physical_mutation_outstanding = self
            .credential_runtime
            .credential_physical_mutation_outstanding();
        let lxmf_mutation_pending = self.lxmf_mutation_pending();
        ProductSubmissionPort {
            flash: &mut *self.flash,
            journal_binding: self.journal_binding,
            runtime: &mut self.runtime,
            submission_service_enabled: self.submission_service_enabled,
            credential_physical_mutation_outstanding,
            lxmf_mutation_pending,
        }
    }

    /// Validate and durably own one exact `lxmf.delivery` application event.
    ///
    /// Cross-store deferrals occur before constructing a partition view and
    /// return the unchanged lease. The coordinator alone creates the bound
    /// writable view over its physical flash owner. Its own pending mutation
    /// never blocks this call; the mounted store decides whether the candidate
    /// is the exact reconciliation retry.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_lxmf_event<'owner, 'slots, 'proof_slots, R>(
        &mut self,
        lease: ApplicationEventLease<'owner, 'slots>,
        delayed_proofs: &mut DelayedProofOwner<'proof_slots>,
        local_destination: LocalDeliveryDestination,
        limits: WireLimits,
        source_identities: &R,
        stamp_policy: StampPolicy<'_>,
    ) -> ProductLxmfAdmission<'owner, 'slots>
    where
        R: SourceIdentityResolver + ?Sized,
    {
        match self.lxmf_mutation_gate() {
            LxmfMutationGate::Ready => {}
            LxmfMutationGate::DeferredForCredentialMutation => {
                return ProductLxmfAdmission::DeferredForCredentialMutation(lease);
            }
            LxmfMutationGate::DeferredForJournalMutation => {
                return ProductLxmfAdmission::DeferredForJournalMutation(lease);
            }
            LxmfMutationGate::RuntimeUnavailable => {
                return ProductLxmfAdmission::RuntimeUnavailable(lease);
            }
        }

        let binding = self
            .lxmf
            .as_ref()
            .expect("ready LXMF gate requires a mounted store")
            .binding();
        let region = PartitionNorFlash::new(&mut *self.flash, LXMF_STORE_OFFSET, LXMF_STORE_LEN);
        let mut access = BoundLxmfStore::new(region, binding);
        let outcome = commit_application_event(
            lease,
            DurableIngressProofMode::Required,
            delayed_proofs,
            local_destination,
            limits,
            source_identities,
            stamp_policy,
            self.lxmf
                .as_mut()
                .expect("ready LXMF gate requires a mounted store"),
            &mut access,
        );
        ProductLxmfAdmission::Ingress(outcome)
    }

    /// Dispatch one exact session-authenticated request while credential and
    /// journal and LXMF fields remain disjointly borrowed by the sole storage owner.
    /// The public identity summary is copied in from the node owner and is not
    /// stored in or derived by this flash coordinator.
    #[allow(
        clippy::too_many_arguments,
        clippy::result_large_err,
        reason = "this sole-owner boundary receives independent bounded semantic owners without storing or aliasing them"
    )]
    pub(crate) fn dispatch_authenticated_request<N, Q>(
        &mut self,
        supervisor: &ProductSupervisor,
        radio_diagnostics: &RadioDiagnosticsCell,
        route_diagnostics: &mut RouteDiagnosticsSnapshot<{ config::PATHS }>,
        discovered_peers: &DiscoveredPeers<
            { config::LXMF_DISCOVERED_PEERS },
            { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
        >,
        peer_discovery_incarnation: LxmfPeerDiscoveryIncarnation,
        peer_discovery_now_ms: u64,
        manual_announce_schedule: &mut ManualAnnounceSchedule,
        announce_now_seconds: u64,
        nomad_port: &mut N,
        probe_port: &mut Q,
        request: LocalApiRequest<AuthenticatedGrant>,
        identity: IdentitySummary,
    ) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
    where
        N: NomadFetchPort,
        Q: ReticulumProbePort,
    {
        let credential_physical_mutation_outstanding = self
            .credential_runtime
            .credential_physical_mutation_outstanding();
        let (journal_actor_mutation_pending, journal_projector_mutation_pending) =
            self.journal_mutation_pending();
        let lxmf_mutation_pending = self.lxmf_mutation_pending();
        let Self {
            flash,
            journal_binding,
            credential_runtime,
            network_config,
            network_config_radio_applied_revision,
            network_config_last_mutation,
            #[cfg(feature = "gateway")]
            wifi_station_status,
            #[cfg(feature = "gateway")]
            wifi_tcp_status,
            rmap_status,
            #[cfg(feature = "appliance")]
                ble_bond_store: _,
            runtime,
            submission_service_enabled,
            lxmf,
            lxmf_mailbox,
            lxmf_service_enabled,
        } = self;
        let mut port = ProductAuthenticatedApiPort {
            flash,
            journal_binding: *journal_binding,
            runtime,
            submission_service_enabled: *submission_service_enabled,
            credential_physical_mutation_outstanding,
            journal_actor_mutation_pending,
            journal_projector_mutation_pending,
            network_config,
            network_config_radio_applied_revision: *network_config_radio_applied_revision,
            network_config_last_mutation,
            #[cfg(feature = "gateway")]
            wifi_station_status,
            #[cfg(feature = "gateway")]
            wifi_tcp_status,
            rmap_status,
            lxmf,
            lxmf_mailbox,
            lxmf_service_enabled: *lxmf_service_enabled,
            lxmf_mutation_pending,
            lxmf_compose_service_enabled: identity.lxmf_delivery_destination().is_some(),
            supervisor,
            radio_diagnostics,
            route_diagnostics,
            discovered_peers,
            peer_discovery_incarnation,
            peer_discovery_now_ms,
            manual_announce_schedule,
            announce_now_seconds,
        };
        credential_runtime.dispatch_authenticated_request_with_probe(
            request, identity, &mut port, nomad_port, probe_port,
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
        bearer: BearerBinding,
        connection: ConnectionId,
        request: PairingRequest,
        rng: &mut R,
    ) -> ProductLivePairingAdmission
    where
        R: RngCore + CryptoRng,
    {
        if matches!(
            &request,
            PairingRequest::ProofStart(request) if request.bearer() != bearer
        ) {
            let sequence = request.sequence();
            return ProductLivePairingAdmission::Immediate(PairingResponse::ProofStart(
                ProofStartResponse::failure(
                    sequence,
                    reticulum_device_api_pairing::PairingFailure::Refused,
                ),
            ));
        }
        let mutation_request = !matches!(&request, PairingRequest::ProofStart(_));
        if mutation_request {
            match self.credential_physical_mutation_gate() {
                CredentialPhysicalMutationGate::Ready => {}
                CredentialPhysicalMutationGate::DeferredForJournalMutation => {
                    return ProductLivePairingAdmission::DeferredForJournalMutation(request);
                }
                CredentialPhysicalMutationGate::DeferredForLxmfMutation => {
                    return ProductLivePairingAdmission::DeferredForLxmfMutation(request);
                }
            }
        }

        match request {
            PairingRequest::Begin(request) => {
                let sequence = request.sequence();
                match self
                    .credential_runtime
                    .request_pairing_begin(bearer, at, connection, rng)
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
                    .request_pairing_activate(bearer, at, connection, request)
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
    /// touches physical flash. It still defers while LXMF owns ambiguous
    /// mutation because projection can create journal persistence work and
    /// deadlock the two retained owners.
    pub(crate) fn offer_authorized_frame(
        &mut self,
        observation: AuthorizedFrameObservation,
    ) -> Option<Result<FrameOfferProgress, RuntimeControlError>> {
        if !self.submission_service_enabled {
            return None;
        }
        if journal_projection_gate(self.lxmf_mutation_pending())
            == JournalProjectionGate::RetainForLxmfMutation
        {
            return Some(Ok(FrameOfferProgress::Retain));
        }
        Some(self.runtime.as_mut()?.offer_authorized_frame(observation))
    }

    /// Recover immutable durable context for one exact routed DATA packet.
    ///
    /// The volatile projector performs the handle-and-token correlation. The
    /// durable index then supplies the destination accepted with that same
    /// submission. This is diagnostics-only and cannot mutate either owner.
    pub(crate) fn routed_submission_context(
        &self,
        prepared: PreparedPacket,
    ) -> Option<(SubmissionId, NodeDestinationHash)> {
        let runtime = self.runtime.as_ref()?;
        let submission = runtime
            .storage()
            .projector()
            .submission_for_prepared(prepared)?;
        let accepted = runtime.storage().index().get(submission)?.accepted();
        Some((
            submission,
            NodeDestinationHash::new(*accepted.intent().destination().as_bytes()),
        ))
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
            JournalMutationGate::DeferredForLxmfMutation => {
                return ProductSubmissionDrive::DeferredForLxmfMutation;
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

    /// Confirm complete interface transmission of one exact path request.
    ///
    /// This is intentionally independent of physical flash mutation and must
    /// remain callable while another store owns the shared NOR device.
    pub(crate) fn acknowledge_path_request_dispatched(
        &mut self,
        offer: PathDiscoveryOffer,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), ProductPathDiscoveryAcknowledgeError> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or(ProductPathDiscoveryAcknowledgeError::RuntimeUnavailable)?;
        runtime
            .acknowledge_path_request_dispatched(offer, dispatched_at)
            .map_err(ProductPathDiscoveryAcknowledgeError::Runtime)
    }

    /// Return one exact path request after all eligible interface hops ended
    /// without a confirmed complete transmission.
    ///
    /// This backend-free edge preserves the offer and ordinal while delaying
    /// its next emission, so CAD denial, expiry, and transport faults do not
    /// consume a discovery attempt or create a hot retry loop.
    pub(crate) fn retry_path_request_dispatch(
        &mut self,
        offer: PathDiscoveryOffer,
        retry_not_before: MonotonicMillis,
    ) -> Result<(), ProductPathDiscoveryAcknowledgeError> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or(ProductPathDiscoveryAcknowledgeError::RuntimeUnavailable)?;
        runtime
            .retry_path_request_dispatch(offer, retry_not_before)
            .map_err(ProductPathDiscoveryAcknowledgeError::Runtime)
    }

    /// Bind one exact locally created Link to the resident direct-delivery
    /// transaction before its ordinary action leaves product ownership.
    ///
    /// This control step is backend-free and does not borrow physical flash.
    pub(crate) fn attach_created_link(
        &mut self,
        offer: LinkEstablishmentOffer,
        link: LinkHandle,
    ) -> Result<(), ProductLinkEstablishmentControlError> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or(ProductLinkEstablishmentControlError::RuntimeUnavailable)?;
        runtime
            .attach_created_link(offer, link)
            .map_err(ProductLinkEstablishmentControlError::Runtime)
    }

    /// Confirm the first real router dispatch of one exact LINKREQUEST.
    ///
    /// The runtime starts its bounded establishment deadline only at this
    /// dispatch edge. This control step is backend-free and remains callable
    /// while another store owns the shared NOR device.
    pub(crate) fn acknowledge_link_request_dispatched(
        &mut self,
        offer: LinkEstablishmentOffer,
        link: LinkHandle,
        dispatched_at: MonotonicMillis,
    ) -> Result<(), ProductLinkEstablishmentControlError> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or(ProductLinkEstablishmentControlError::RuntimeUnavailable)?;
        runtime
            .acknowledge_link_request_dispatched(offer, link, dispatched_at)
            .map_err(ProductLinkEstablishmentControlError::Runtime)
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
        match self.credential_physical_mutation_gate() {
            CredentialPhysicalMutationGate::Ready => {}
            CredentialPhysicalMutationGate::DeferredForJournalMutation => {
                return ProductInitializationRequest::DeferredForJournalMutation;
            }
            CredentialPhysicalMutationGate::DeferredForLxmfMutation => {
                return ProductInitializationRequest::DeferredForLxmfMutation;
            }
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
            self.lxmf_mutation_pending,
        ) {
            JournalMutationGate::Ready => {}
            JournalMutationGate::DeferredForCredentialMutation => {
                return Err(SubmissionPortError::Busy);
            }
            JournalMutationGate::DeferredForLxmfMutation => {
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

        // Preserve replay and conflict semantics even when the bounded
        // admission profile is full. Only a genuinely new acceptance is
        // stopped by the product cap.
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

fn api_network_config_snapshot(
    revision: u64,
    redacted: RedactedNetworkConfig,
) -> Result<NetworkConfigSnapshot, NetworkConfigPortError> {
    let mut wifi_profiles = [None; reticulum_device_api::MAX_WIFI_NETWORK_PROFILES];
    for (destination, source) in wifi_profiles.iter_mut().zip(redacted.wifi_profiles()) {
        let profile_id = WifiNetworkProfileId::new(*source.id().as_bytes())
            .map_err(|_| NetworkConfigPortError::Faulted)?;
        *destination = Some(
            WifiNetworkConfigSummary::new(
                profile_id,
                source.enabled(),
                source.priority(),
                source.ssid(),
                source.password_configured(),
            )
            .map_err(|_| NetworkConfigPortError::Faulted)?,
        );
    }
    let (tcp_peer, tcp_host_peer) = match redacted.tcp_peer() {
        Some(peer) => match peer.address() {
            OutboundTcpPeerAddress::Ipv4(ipv4) => {
                let address = ReticulumTcpPeerIpv4Address::new(ipv4)
                    .map_err(|_| NetworkConfigPortError::Faulted)?;
                let peer = ReticulumTcpPeerConfigSummary::new(peer.enabled(), address, peer.port())
                    .map_err(|_| NetworkConfigPortError::Faulted)?;
                (Some(peer), None)
            }
            OutboundTcpPeerAddress::Dns(hostname) => {
                let hostname = core::str::from_utf8(hostname.as_bytes())
                    .map_err(|_| NetworkConfigPortError::Faulted)?;
                let peer =
                    ReticulumTcpPeerHostConfigSummary::new(peer.enabled(), hostname, peer.port())
                        .map_err(|_| NetworkConfigPortError::Faulted)?;
                (None, Some(peer))
            }
        },
        None => (None, None),
    };
    let gateway_policy = GatewayPolicy::new(
        redacted.wifi_transport_enabled(),
        redacted.automatic_announces_enabled(),
    );
    let phone_location = redacted
        .phone_location()
        .map(|location| RmapLocation::new(location.latitude_e6(), location.longitude_e6()))
        .transpose()
        .map_err(|_| NetworkConfigPortError::Faulted)?;
    let rmap_config = RmapConfig::new(
        redacted.rmap_discovery_enabled(),
        redacted.rmap_share_location(),
        phone_location,
    );
    let stored_lora_profile = redacted.lora_profile();
    let lora_tx_power_dbm =
        LoraTransmitPowerDbm::new(stored_lora_profile.tx_power().requested_dbm() as u8)
            .map_err(|_| NetworkConfigPortError::Faulted)?;
    let lora_profile = ApiLoraRadioProfile::new(
        stored_lora_profile.frequency_hz(),
        stored_lora_profile.bandwidth_hz(),
        stored_lora_profile.spreading_factor(),
        stored_lora_profile.coding_rate_denominator(),
        lora_tx_power_dbm,
    )
    .map_err(|_| NetworkConfigPortError::Faulted)?;
    let device_name = redacted
        .device_name()
        .map(|name| DeviceNameSummary::new(name.as_str()))
        .transpose()
        .map_err(|_| NetworkConfigPortError::Faulted)?;
    NetworkConfigSnapshot::new(
        revision,
        wifi_profiles,
        tcp_peer,
        tcp_host_peer,
        gateway_policy,
        rmap_config,
        lora_profile,
        device_name,
    )
    .map_err(|_| NetworkConfigPortError::Invariant)
}

fn store_wifi_profile_id(
    profile_id: WifiNetworkProfileId,
) -> Result<WifiProfileId, NetworkConfigPortError> {
    WifiProfileId::new(*profile_id.as_bytes()).map_err(|_| NetworkConfigPortError::InvalidRequest)
}

fn store_tcp_peer(
    peer: reticulum_device_api::ReticulumTcpPeerUpdate,
) -> Result<OutboundTcpPeer, NetworkConfigPortError> {
    OutboundTcpPeer::new(peer.ipv4_address().octets(), peer.port(), peer.enabled())
        .map_err(|_| NetworkConfigPortError::InvalidRequest)
}

fn store_tcp_host_peer(
    peer: reticulum_device_api::ReticulumTcpPeerHostUpdate<'_>,
) -> Result<OutboundTcpPeer, NetworkConfigPortError> {
    OutboundTcpPeer::with_dns_hostname(
        peer.hostname().as_str().as_bytes(),
        peer.port(),
        peer.enabled(),
    )
    .map_err(|_| NetworkConfigPortError::InvalidRequest)
}

fn store_rmap_location(location: RmapLocation) -> Result<PhoneLocation, NetworkConfigPortError> {
    PhoneLocation::new(location.latitude_e6(), location.longitude_e6())
        .map_err(|_| NetworkConfigPortError::InvalidRequest)
}

fn store_device_name(
    name: Option<ApiDeviceName<'_>>,
) -> Result<Option<StoredDeviceName>, NetworkConfigPortError> {
    name.map(|name| StoredDeviceName::new(name.as_str()))
        .transpose()
        .map_err(|_| NetworkConfigPortError::InvalidRequest)
}

fn store_lora_profile(
    profile: ApiLoraRadioProfile,
) -> Result<StoredLoraRadioProfile, NetworkConfigPortError> {
    let power = LoraTxPower::try_from_dbm(i32::from(profile.tx_power_dbm().get()))
        .map_err(|_| NetworkConfigPortError::InvalidRequest)?;
    let board_power = E290Na915TxPower::try_from_dbm(power.requested_dbm())
        .map_err(|_| NetworkConfigPortError::InvalidRequest)?;
    E290RadioConfiguration::try_from_profile(
        profile.frequency_hz(),
        profile.bandwidth_hz(),
        profile.spreading_factor(),
        profile.coding_rate_denominator(),
        board_power,
    )
    .map_err(|_| NetworkConfigPortError::InvalidRequest)?;
    StoredLoraRadioProfile::new(
        profile.frequency_hz(),
        profile.bandwidth_hz(),
        profile.spreading_factor(),
        profile.coding_rate_denominator(),
        power,
    )
    .map_err(|_| NetworkConfigPortError::InvalidRequest)
}

fn network_config_mutation_is_noop(
    state: &ProductNetworkConfigState,
    mutation: NetworkConfigMutation<'_>,
) -> Result<bool, NetworkConfigPortError> {
    let configuration = match state {
        ProductNetworkConfigState::Mounted(mounted) => Some(mounted.configuration()),
        ProductNetworkConfigState::FreshErased { .. } => None,
        ProductNetworkConfigState::Unavailable { .. } => {
            return Err(NetworkConfigPortError::Unavailable);
        }
    };
    match mutation {
        NetworkConfigMutation::UpsertWifi {
            profile_id,
            network,
        } => {
            let profile_id = store_wifi_profile_id(profile_id)?;
            let existing = configuration.and_then(|configuration| {
                configuration
                    .wifi_profiles()
                    .find(|profile| profile.id() == profile_id)
            });
            let Some(existing) = existing else {
                if matches!(network.credential(), WifiCredentialUpdate::Keep) {
                    return Err(NetworkConfigPortError::InvalidRequest);
                }
                if configuration.is_some_and(|configuration| {
                    configuration.wifi_profile_count()
                        >= reticulum_network_config_store::WIFI_PROFILE_CAPACITY
                }) {
                    return Err(NetworkConfigPortError::InvalidRequest);
                }
                return Ok(false);
            };
            // A replacement is always material. Treating equality as a no-op
            // would expose a revision/timing oracle for the stored secret.
            let credential_matches = matches!(network.credential(), WifiCredentialUpdate::Keep);
            Ok(existing.enabled() == network.enabled()
                && existing.priority() == network.priority()
                && existing.ssid() == network.ssid().as_bytes()
                && credential_matches)
        }
        NetworkConfigMutation::RemoveWifi { profile_id } => {
            let profile_id = store_wifi_profile_id(profile_id)?;
            Ok(configuration.is_none_or(|configuration| {
                !configuration
                    .wifi_profiles()
                    .any(|profile| profile.id() == profile_id)
            }))
        }
        NetworkConfigMutation::ReplaceTcpPeer(peer) => {
            let requested = peer.map(store_tcp_peer).transpose()?;
            Ok(configuration.and_then(NetworkConfig::tcp_peer) == requested)
        }
        NetworkConfigMutation::ReplaceTcpHostPeer(peer) => {
            let requested = peer.map(store_tcp_host_peer).transpose()?;
            Ok(configuration.and_then(NetworkConfig::tcp_peer) == requested)
        }
        NetworkConfigMutation::SetGatewayPolicy(policy) => {
            let wifi_transport_enabled =
                configuration.is_none_or(|value| value.wifi_transport_enabled());
            let automatic_announces_enabled =
                configuration.is_none_or(|value| value.automatic_announces_enabled());
            Ok(wifi_transport_enabled == policy.wifi_transport_enabled()
                && automatic_announces_enabled == policy.automatic_announces_enabled())
        }
        NetworkConfigMutation::SetRmapConfig(config) => {
            let requested_location = config
                .phone_location()
                .map(store_rmap_location)
                .transpose()?;
            let (discovery_enabled, share_location, phone_location) =
                configuration.map_or((false, false, None), |value| {
                    (
                        value.rmap_discovery_enabled(),
                        value.rmap_share_location(),
                        value.phone_location(),
                    )
                });
            Ok(discovery_enabled == config.discovery_enabled()
                && share_location == config.share_location()
                && phone_location == requested_location)
        }
        NetworkConfigMutation::SetLoraTxPower(power) => {
            let selected = configuration.map_or(LoraTxPower::Dbm14, NetworkConfig::lora_tx_power);
            Ok(selected.requested_dbm() == i32::from(power.get()))
        }
        NetworkConfigMutation::SetLoraProfile(profile) => {
            let requested = store_lora_profile(profile)?;
            Ok(
                configuration.map_or(StoredLoraRadioProfile::DEFAULT, NetworkConfig::lora_profile)
                    == requested,
            )
        }
        NetworkConfigMutation::SetDeviceName(name) => {
            let requested = store_device_name(name)?;
            Ok(configuration.and_then(NetworkConfig::device_name) == requested)
        }
    }
}

fn apply_network_config_mutation(
    configuration: &mut NetworkConfig,
    mutation: NetworkConfigMutation<'_>,
) -> Result<(), NetworkConfigPortError> {
    match mutation {
        NetworkConfigMutation::UpsertWifi {
            profile_id,
            network,
        } => {
            let profile_id = store_wifi_profile_id(profile_id)?;
            let mut retained_password =
                Zeroizing::new([0_u8; reticulum_network_config_store::MAX_WPA2_PASSWORD_LENGTH]);
            let password = match network.credential() {
                WifiCredentialUpdate::Keep => {
                    let existing = configuration
                        .wifi_profiles()
                        .find(|profile| profile.id() == profile_id)
                        .ok_or(NetworkConfigPortError::InvalidRequest)?;
                    let length = existing.password().len();
                    retained_password[..length].copy_from_slice(existing.password());
                    &retained_password[..length]
                }
                WifiCredentialUpdate::Replace(password) => password,
            };
            let profile = WifiProfile::new(
                profile_id,
                network.ssid().as_bytes(),
                password,
                network.enabled(),
                network.priority(),
            )
            .map_err(|_| NetworkConfigPortError::InvalidRequest)?;
            configuration
                .upsert_wifi_profile(profile)
                .map_err(|_| NetworkConfigPortError::InvalidRequest)
        }
        NetworkConfigMutation::RemoveWifi { profile_id } => {
            let profile_id = store_wifi_profile_id(profile_id)?;
            configuration.remove_wifi_profile(profile_id);
            Ok(())
        }
        NetworkConfigMutation::ReplaceTcpPeer(peer) => {
            configuration.set_tcp_peer(peer.map(store_tcp_peer).transpose()?);
            Ok(())
        }
        NetworkConfigMutation::ReplaceTcpHostPeer(peer) => {
            configuration.set_tcp_peer(peer.map(store_tcp_host_peer).transpose()?);
            Ok(())
        }
        NetworkConfigMutation::SetGatewayPolicy(policy) => {
            configuration.set_wifi_transport_enabled(policy.wifi_transport_enabled());
            configuration.set_automatic_announces_enabled(policy.automatic_announces_enabled());
            Ok(())
        }
        NetworkConfigMutation::SetRmapConfig(config) => {
            configuration.set_rmap_discovery_enabled(config.discovery_enabled());
            configuration.set_rmap_share_location(config.share_location());
            configuration.set_phone_location(
                config
                    .phone_location()
                    .map(store_rmap_location)
                    .transpose()?,
            );
            Ok(())
        }
        NetworkConfigMutation::SetLoraTxPower(power) => {
            let power = LoraTxPower::try_from_dbm(i32::from(power.get()))
                .map_err(|_| NetworkConfigPortError::InvalidRequest)?;
            configuration.set_lora_tx_power(power);
            Ok(())
        }
        NetworkConfigMutation::SetLoraProfile(profile) => {
            configuration.set_lora_profile(store_lora_profile(profile)?);
            Ok(())
        }
        NetworkConfigMutation::SetDeviceName(name) => {
            configuration.set_device_name(store_device_name(name)?);
            Ok(())
        }
    }
}

fn network_configurations_equal(left: &NetworkConfig, right: &NetworkConfig) -> bool {
    let mut left_profiles = left.wifi_profiles();
    let mut right_profiles = right.wifi_profiles();
    loop {
        match (left_profiles.next(), right_profiles.next()) {
            (None, None) => break,
            (Some(left), Some(right))
                if left.id() == right.id()
                    && left.ssid() == right.ssid()
                    && left.password() == right.password()
                    && left.enabled() == right.enabled()
                    && left.priority() == right.priority() => {}
            _ => return false,
        }
    }
    left.tcp_peer() == right.tcp_peer()
        && left.wifi_transport_enabled() == right.wifi_transport_enabled()
        && left.automatic_announces_enabled() == right.automatic_announces_enabled()
        && left.rmap_discovery_enabled() == right.rmap_discovery_enabled()
        && left.rmap_share_location() == right.rmap_share_location()
        && left.phone_location() == right.phone_location()
        && left.lora_profile() == right.lora_profile()
        && left.device_name() == right.device_name()
}

fn map_network_config_store_error(
    error: NetworkConfigStoreError<ProductRegionError>,
) -> NetworkConfigPortError {
    match error {
        NetworkConfigStoreError::Binding(_) => NetworkConfigPortError::Binding,
        NetworkConfigStoreError::Backend(_) => NetworkConfigPortError::Backend,
        NetworkConfigStoreError::Fault(_) => NetworkConfigPortError::Faulted,
    }
}

impl ProductAuthenticatedApiPort<'_> {
    fn network_config_revision(&self) -> Result<u64, NetworkConfigPortError> {
        match &*self.network_config {
            ProductNetworkConfigState::FreshErased { .. } => Ok(0),
            ProductNetworkConfigState::Mounted(mounted) => {
                Ok(mounted.revision().generation().get())
            }
            ProductNetworkConfigState::Unavailable { .. } => {
                Err(NetworkConfigPortError::Unavailable)
            }
        }
    }

    fn network_config_write_blocked(&self) -> bool {
        self.credential_physical_mutation_outstanding
            || self.journal_actor_mutation_pending
            || self.journal_projector_mutation_pending
            || self.lxmf_mutation_pending
    }

    fn network_config_applied_revision(&self) -> u64 {
        #[cfg(feature = "gateway")]
        {
            self.network_config_radio_applied_revision
                .min(self.wifi_station_status.snapshot().applied_revision)
                .min(self.wifi_tcp_status.snapshot().applied_revision)
        }
        #[cfg(not(feature = "gateway"))]
        {
            self.network_config_radio_applied_revision
        }
    }

    fn replay_matches(
        &self,
        principal: ApiPrincipalId,
        request: NetworkConfigMutationRequest<'_>,
        current_revision: u64,
    ) -> Result<bool, NetworkConfigPortError> {
        let Some(receipt) = *self.network_config_last_mutation else {
            return Ok(false);
        };
        Ok(receipt.matches(principal, request, current_revision))
    }

    fn remember_network_config_mutation(
        &mut self,
        principal: ApiPrincipalId,
        request: NetworkConfigMutationRequest<'_>,
        applied_revision: u64,
    ) {
        *self.network_config_last_mutation = Some(NetworkConfigMutationReceipt::new(
            principal,
            request,
            applied_revision,
        ));
    }

    fn commit_network_config_mutation(
        &mut self,
        principal: ApiPrincipalId,
        request: NetworkConfigMutationRequest<'_>,
    ) -> Result<NetworkConfigMutationOutcome, NetworkConfigPortError> {
        let binding = self.network_config.binding();
        let prior = mem::replace(
            self.network_config,
            ProductNetworkConfigState::Unavailable { binding },
        );
        let (predecessor, mut configuration) = match prior {
            ProductNetworkConfigState::FreshErased { .. } => (None, NetworkConfig::empty()),
            ProductNetworkConfigState::Mounted(mounted) => {
                let revision = mounted.revision();
                (Some(revision), mounted.into_configuration())
            }
            ProductNetworkConfigState::Unavailable { .. } => {
                return Err(NetworkConfigPortError::Unavailable);
            }
        };
        if let Err(error) = apply_network_config_mutation(&mut configuration, request.mutation()) {
            let region =
                PartitionNorFlash::new(&mut *self.flash, NETWORK_CONFIG_OFFSET, NETWORK_CONFIG_LEN);
            let mut access = BoundNetworkConfigStore::new(region, binding);
            *self.network_config = match mount_network_config_store(&mut access) {
                Ok(mounted) => ProductNetworkConfigState::Mounted(mounted),
                Err(NetworkConfigStoreError::Fault(NetworkConfigStoreFault::UnformattedErased))
                    if predecessor.is_none() =>
                {
                    ProductNetworkConfigState::FreshErased { binding }
                }
                Err(_) => ProductNetworkConfigState::Unavailable { binding },
            };
            return Err(error);
        }

        // Preserve the prior exact-replay receipt until a successor becomes
        // authoritative. If this commit fails and remount retains the
        // predecessor, an ambiguous retry of that predecessor's mutation must
        // still be recognizable. A successful/reconciled successor overwrites
        // the receipt below with its own request fingerprint.
        let region =
            PartitionNorFlash::new(&mut *self.flash, NETWORK_CONFIG_OFFSET, NETWORK_CONFIG_LEN);
        let mut access = BoundNetworkConfigStore::new(region, binding);
        let commit = match predecessor {
            Some(predecessor) => {
                commit_network_config_successor(&mut access, predecessor, &configuration)
            }
            None => provision_network_config_erased(&mut access, &configuration),
        };
        match commit {
            Ok(mounted) => {
                let revision = mounted.revision().generation().get();
                *self.network_config = ProductNetworkConfigState::Mounted(mounted);
                self.remember_network_config_mutation(principal, request, revision);
                Ok(NetworkConfigMutationOutcome::Applied {
                    revision,
                    reboot_required: true,
                })
            }
            Err(commit_error) => {
                let reconciled = mount_network_config_store(&mut access);
                match reconciled {
                    Ok(mounted) => {
                        let revision = mounted.revision().generation().get();
                        let intended_revision =
                            request.expected_revision().checked_add(1) == Some(revision);
                        let intended = intended_revision
                            && network_configurations_equal(
                                mounted.configuration(),
                                &configuration,
                            );
                        *self.network_config = ProductNetworkConfigState::Mounted(mounted);
                        if intended {
                            self.remember_network_config_mutation(principal, request, revision);
                            Ok(NetworkConfigMutationOutcome::Applied {
                                revision,
                                reboot_required: true,
                            })
                        } else {
                            Err(map_network_config_store_error(commit_error))
                        }
                    }
                    Err(NetworkConfigStoreError::Fault(
                        NetworkConfigStoreFault::UnformattedErased,
                    )) if predecessor.is_none() => {
                        *self.network_config = ProductNetworkConfigState::FreshErased { binding };
                        Err(map_network_config_store_error(commit_error))
                    }
                    Err(reconcile_error) => {
                        *self.network_config = ProductNetworkConfigState::Unavailable { binding };
                        Err(map_network_config_store_error(reconcile_error))
                    }
                }
            }
        }
    }
}

impl NodeDiagnosticsPort for ProductAuthenticatedApiPort<'_> {
    fn node_diagnostics(&mut self) -> Result<NodeDiagnosticsSnapshot, NodeDiagnosticsPortError> {
        let radio = self.radio_diagnostics.snapshot();
        let registry = self.supervisor.interface_registry();
        let mut interfaces = [None; reticulum_device_api::MAX_DIAGNOSTIC_INTERFACES];
        interfaces[0] = diagnostic_interface_record(
            DiagnosticInterfaceKind::LoRa,
            registry.descriptor(PacketInterfaceId::new(1)),
            matches!(radio.state, RadioRuntimeState::Faulted),
        );
        #[cfg(feature = "gateway")]
        {
            interfaces[1] = diagnostic_interface_record(
                DiagnosticInterfaceKind::Tcp,
                registry.descriptor(PacketInterfaceId::new(2)),
                matches!(self.wifi_tcp_status.snapshot().phase, WifiTcpPhase::Faulted),
            );
        }

        self.supervisor.snapshot_route_diagnostics(
            MonotonicSeconds::new(self.announce_now_seconds),
            self.route_diagnostics,
        );
        Ok(project_node_diagnostics(
            self.peer_discovery_now_ms,
            interfaces,
            radio,
            self.supervisor.node_metrics(),
            self.discovered_peers.len(),
            self.route_diagnostics,
        ))
    }

    fn route_diagnostics_page(
        &mut self,
        request: RouteDiagnosticsRequest,
    ) -> Result<RouteDiagnosticsPage, NodeDiagnosticsPortError> {
        project_route_diagnostics_page(
            self.route_diagnostics.generation(),
            request,
            self.route_diagnostics,
        )
        .map_err(|_| NodeDiagnosticsPortError::Invariant)
    }

    fn radio_trace_page(
        &mut self,
        request: reticulum_device_api::RadioTracePageRequest,
    ) -> Result<reticulum_device_api::RadioTracePage, NodeDiagnosticsPortError> {
        let cursor = request
            .after()
            .map(|cursor| RadioTraceCursor::new(cursor.boot_id(), cursor.after_sequence()));
        let trace = self.radio_diagnostics.radio_trace_page(cursor);
        let applied = self.radio_diagnostics.snapshot().applied;
        project_radio_trace_page(applied, trace).map_err(|_| NodeDiagnosticsPortError::Invariant)
    }
}

impl NetworkConfigPort for ProductAuthenticatedApiPort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        if self.network_config.available() {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    }

    fn configuration(&mut self) -> Result<NetworkConfigSnapshot, NetworkConfigPortError> {
        let revision = self.network_config_revision()?;
        api_network_config_snapshot(revision, self.network_config.redacted())
    }

    fn mutate(
        &mut self,
        principal: ApiPrincipalId,
        request: NetworkConfigMutationRequest<'_>,
    ) -> Result<NetworkConfigMutationOutcome, NetworkConfigPortError> {
        let current_revision = self.network_config_revision()?;
        if request.expected_revision() != current_revision {
            if self.replay_matches(principal, request, current_revision)? {
                return Ok(NetworkConfigMutationOutcome::Applied {
                    revision: current_revision,
                    reboot_required: current_revision != self.network_config_applied_revision(),
                });
            }
            return Ok(NetworkConfigMutationOutcome::RevisionConflict { current_revision });
        }
        if network_config_mutation_is_noop(self.network_config, request.mutation())? {
            return Ok(NetworkConfigMutationOutcome::Applied {
                revision: current_revision,
                reboot_required: current_revision != self.network_config_applied_revision(),
            });
        }
        if self.network_config_write_blocked() {
            return Err(NetworkConfigPortError::Busy);
        }
        self.commit_network_config_mutation(principal, request)
    }

    fn status(&mut self) -> Result<NetworkRuntimeStatus, NetworkConfigPortError> {
        let configured_revision = self.network_config_revision()?;
        let applied_revision = self.network_config_applied_revision();
        let rmap_status = self.rmap_status.snapshot(
            self.announce_now_seconds,
            configured_revision == applied_revision,
        );
        #[cfg(not(feature = "gateway"))]
        let redacted = self.network_config.redacted();
        #[cfg(feature = "gateway")]
        let tcp_status = self.wifi_tcp_status.snapshot();
        #[cfg(feature = "gateway")]
        let tcp_peer_state = match tcp_status.phase {
            WifiTcpPhase::Disabled => ReticulumTcpPeerState::Disabled,
            WifiTcpPhase::WaitingForNetwork => ReticulumTcpPeerState::WaitingForNetwork,
            WifiTcpPhase::Connecting => ReticulumTcpPeerState::Connecting,
            WifiTcpPhase::Connected => ReticulumTcpPeerState::Connected,
            WifiTcpPhase::Backoff => ReticulumTcpPeerState::Backoff,
            WifiTcpPhase::Faulted => ReticulumTcpPeerState::Faulted,
        };
        #[cfg(feature = "gateway")]
        let last_tcp_failure = tcp_status.last_failure.map(|failure| match failure {
            WifiTcpFailure::DnsTimeout => ReticulumTcpFailure::DnsTimeout,
            WifiTcpFailure::DnsLookupFailed => ReticulumTcpFailure::DnsLookupFailed,
            WifiTcpFailure::DnsNoIpv4Result => ReticulumTcpFailure::DnsNoIpv4Result,
            WifiTcpFailure::ConnectInvalidState => ReticulumTcpFailure::ConnectInvalidState,
            WifiTcpFailure::ConnectReset => ReticulumTcpFailure::ConnectReset,
            WifiTcpFailure::ConnectTimeout => ReticulumTcpFailure::ConnectTimeout,
            WifiTcpFailure::ConnectNoRoute => ReticulumTcpFailure::ConnectNoRoute,
            WifiTcpFailure::SocketClosed => ReticulumTcpFailure::SocketClosed,
            WifiTcpFailure::TransmitFailed => ReticulumTcpFailure::TransmitFailed,
        });
        #[cfg(feature = "gateway")]
        let dns_diagnostics = tcp_status.dns_diagnostics;
        #[cfg(not(feature = "gateway"))]
        let tcp_peer_state = if redacted.tcp_peer().is_some_and(|peer| peer.enabled()) {
            ReticulumTcpPeerState::WaitingForNetwork
        } else {
            ReticulumTcpPeerState::Disabled
        };
        #[cfg(not(feature = "gateway"))]
        let last_tcp_failure = None;
        #[cfg(not(feature = "gateway"))]
        let dns_diagnostics = None;

        #[cfg(feature = "gateway")]
        {
            let status = self.wifi_station_status.snapshot();
            let wifi_state = match status.phase {
                WifiStationPhase::Disabled => WifiStationState::Disabled,
                WifiStationPhase::Associating | WifiStationPhase::Dhcp => {
                    WifiStationState::Connecting
                }
                WifiStationPhase::Online => WifiStationState::Connected,
                WifiStationPhase::Backoff | WifiStationPhase::Faulted => {
                    WifiStationState::Disconnected
                }
            };
            let active_wifi_profile = status
                .profile_id
                .map(WifiNetworkProfileId::new)
                .transpose()
                .map_err(|_| NetworkConfigPortError::Invariant)?;
            let connected_ssid = status.connected_ssid();
            NetworkRuntimeStatus::new_with_tcp_diagnostics(
                configured_revision,
                applied_revision,
                wifi_state,
                active_wifi_profile,
                connected_ssid,
                status.ipv4,
                status.rssi_dbm.map(i16::from),
                tcp_peer_state,
                last_tcp_failure,
                dns_diagnostics,
            )
            .map(|status| status.with_rmap_status(rmap_status))
            .map_err(|_| NetworkConfigPortError::Invariant)
        }

        #[cfg(not(feature = "gateway"))]
        {
            let wifi_state = if redacted.wifi_profiles().any(|profile| profile.enabled()) {
                WifiStationState::Disconnected
            } else {
                WifiStationState::Disabled
            };
            NetworkRuntimeStatus::new_with_tcp_diagnostics(
                configured_revision,
                applied_revision,
                wifi_state,
                None,
                None,
                None,
                None,
                tcp_peer_state,
                last_tcp_failure,
                dns_diagnostics,
            )
            .map(|status| status.with_rmap_status(rmap_status))
            .map_err(|_| NetworkConfigPortError::Invariant)
        }
    }
}

impl ManualServiceAnnouncePort for ProductAuthenticatedApiPort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }

    fn queue_service_announce(&mut self) -> ManualServiceAnnounceDisposition {
        match self
            .manual_announce_schedule
            .request(self.announce_now_seconds)
        {
            ManualAnnounceRequestDisposition::Queued => ManualServiceAnnounceDisposition::Queued,
            ManualAnnounceRequestDisposition::AlreadyPending => {
                ManualServiceAnnounceDisposition::AlreadyPending
            }
        }
    }
}

impl SubmissionPort for ProductAuthenticatedApiPort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        ProductSubmissionPort {
            flash: &mut *self.flash,
            journal_binding: self.journal_binding,
            runtime: &mut *self.runtime,
            submission_service_enabled: self.submission_service_enabled,
            credential_physical_mutation_outstanding: self.credential_physical_mutation_outstanding,
            lxmf_mutation_pending: self.lxmf_mutation_pending,
        }
        .availability()
    }

    fn submission_state(
        &mut self,
        principal: PrincipalId,
        id: SubmissionId,
    ) -> Result<Option<LifecycleState>, SubmissionPortError> {
        ProductSubmissionPort {
            flash: &mut *self.flash,
            journal_binding: self.journal_binding,
            runtime: &mut *self.runtime,
            submission_service_enabled: self.submission_service_enabled,
            credential_physical_mutation_outstanding: self.credential_physical_mutation_outstanding,
            lxmf_mutation_pending: self.lxmf_mutation_pending,
        }
        .submission_state(principal, id)
    }

    fn accept(
        &mut self,
        candidate: AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError> {
        ProductSubmissionPort {
            flash: &mut *self.flash,
            journal_binding: self.journal_binding,
            runtime: &mut *self.runtime,
            submission_service_enabled: self.submission_service_enabled,
            credential_physical_mutation_outstanding: self.credential_physical_mutation_outstanding,
            lxmf_mutation_pending: self.lxmf_mutation_pending,
        }
        .accept(candidate)
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

impl LxmfInboxPort for ProductAuthenticatedApiPort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        if !self.lxmf_service_enabled {
            return CapabilityAvailability::Unavailable;
        }
        if self.lxmf.is_none() {
            return CapabilityAvailability::Unavailable;
        }
        if self.lxmf_mailbox.is_none() {
            return CapabilityAvailability::Unavailable;
        }
        // A retained physical mutation is transient ownership pressure, not a
        // missing capability. Operations which touch flash report `Busy` so
        // the authenticated client can preserve its session and retry.
        CapabilityAvailability::Available
    }

    fn next(
        &mut self,
        after: Option<ApiLxmfMessageHandle>,
    ) -> Result<Option<ApiLxmfMessageSummary>, LxmfInboxPortError> {
        if LxmfInboxPort::availability(self) != CapabilityAvailability::Available {
            return Err(LxmfInboxPortError::Unavailable);
        }
        let store = self.lxmf.as_ref().ok_or(LxmfInboxPortError::Unavailable)?;
        let mut after_seen = after.is_none();
        for receipt in store.receipts() {
            if !after_seen {
                after_seen = after.is_some_and(|cursor| cursor.get() == receipt.handle().get());
                continue;
            }
            let metadata = store
                .metadata(receipt.handle())
                .ok_or(LxmfInboxPortError::Faulted)?;
            let lengths = metadata.lengths();
            let handle = ApiLxmfMessageHandle::new(receipt.handle().get())
                .map_err(|_| LxmfInboxPortError::Faulted)?;
            let ingress = metadata.ingress_observation().map(|ingress| {
                ApiIngressObservation::new(
                    ingress.interface().get(),
                    ingress
                        .signal()
                        .map(|signal| ApiIngressSignal::new(signal.rssi_dbm(), signal.snr_db())),
                )
            });
            let summary = ApiLxmfMessageSummary::new(
                handle,
                *receipt.message_id().as_bytes(),
                ApiDestinationHash(*metadata.destination().as_bytes()),
                ApiDestinationHash(*metadata.source().as_bytes()),
                metadata.timestamp_bits(),
                lengths.normalized_wire(),
                lengths.title(),
                lengths.content(),
                lengths.fields_encoded(),
                *receipt.fingerprint().exact_wire_digest().as_bytes(),
            )
            .map(|summary| summary.with_ingress_observation(ingress))
            .map_err(|_| LxmfInboxPortError::Faulted)?;
            return Ok(Some(summary));
        }
        Ok(None)
    }

    fn read(
        &mut self,
        handle: ApiLxmfMessageHandle,
        offset: u32,
        max_bytes: ApiLxmfReadLength,
    ) -> Result<Option<ApiLxmfReadChunk>, LxmfInboxPortError> {
        if LxmfInboxPort::availability(self) != CapabilityAvailability::Available {
            return Err(LxmfInboxPortError::Unavailable);
        }
        if self.credential_physical_mutation_outstanding
            || self.journal_actor_mutation_pending
            || self.journal_projector_mutation_pending
            || self.lxmf_mutation_pending
        {
            return Err(LxmfInboxPortError::Busy);
        }
        let store = self.lxmf.as_ref().ok_or(LxmfInboxPortError::Unavailable)?;
        let model_handle =
            MessageHandle::new(handle.get()).map_err(|_| LxmfInboxPortError::InvalidRequest)?;
        let mut bytes = [0_u8; MAX_LXMF_READ_CHUNK_BYTES];
        let requested = usize::from(max_bytes.get());
        let region = PartitionNorFlash::new(&mut *self.flash, LXMF_STORE_OFFSET, LXMF_STORE_LEN);
        let mut access = BoundLxmfStore::new(region, store.binding());
        let chunk =
            match store.read_wire_chunk(&mut access, model_handle, offset, &mut bytes[..requested])
            {
                Ok(chunk) => chunk,
                Err(LxmfWireReadError::NotFound { .. }) => return Ok(None),
                Err(LxmfWireReadError::OffsetOutOfRange { .. }) => {
                    return Err(LxmfInboxPortError::InvalidRequest);
                }
                Err(LxmfWireReadError::Binding(_)) => return Err(LxmfInboxPortError::Binding),
                Err(LxmfWireReadError::Backend(_)) => return Err(LxmfInboxPortError::Backend),
                Err(LxmfWireReadError::Fault(_)) => return Err(LxmfInboxPortError::Faulted),
            };
        if chunk.bytes_read() == 0 {
            return Err(LxmfInboxPortError::InvalidRequest);
        }
        ApiLxmfReadChunk::new(
            handle,
            chunk.offset(),
            chunk.wire_length(),
            &bytes[..chunk.bytes_read()],
        )
        .map(Some)
        .map_err(|_| LxmfInboxPortError::Faulted)
    }

    fn mailbox_status(&mut self) -> Result<ApiLxmfMailboxStatus, LxmfInboxPortError> {
        if LxmfInboxPort::availability(self) != CapabilityAvailability::Available {
            return Err(LxmfInboxPortError::Unavailable);
        }
        project_lxmf_mailbox_status(
            self.lxmf.as_ref().ok_or(LxmfInboxPortError::Unavailable)?,
            self.lxmf_mailbox
                .as_ref()
                .ok_or(LxmfInboxPortError::Unavailable)?,
        )
        .ok_or(LxmfInboxPortError::Faulted)
    }

    fn acknowledge_mailbox_through(
        &mut self,
        through: ApiLxmfMessageHandle,
    ) -> Result<ApiLxmfMailboxStatus, LxmfInboxPortError> {
        if LxmfInboxPort::availability(self) != CapabilityAvailability::Available {
            return Err(LxmfInboxPortError::Unavailable);
        }
        if self.credential_physical_mutation_outstanding
            || self.journal_actor_mutation_pending
            || self.journal_projector_mutation_pending
            || self.lxmf_mutation_pending
        {
            return Err(LxmfInboxPortError::Busy);
        }
        let store = self.lxmf.as_ref().ok_or(LxmfInboxPortError::Unavailable)?;
        let latest = store
            .latest_handle()
            .ok_or(LxmfInboxPortError::InvalidRequest)?;
        let through_model =
            MessageHandle::new(through.get()).map_err(|_| LxmfInboxPortError::InvalidRequest)?;
        let receipt = store
            .receipt(through_model)
            .ok_or(LxmfInboxPortError::InvalidRequest)?;
        if through_model > latest {
            return Err(LxmfInboxPortError::InvalidRequest);
        }
        let acknowledged = mailbox_cursor_from_lxmf_receipt(receipt);
        let current = self
            .lxmf_mailbox
            .take()
            .ok_or(LxmfInboxPortError::Unavailable)?;
        if through.get() < current.acknowledged_through() {
            *self.lxmf_mailbox = Some(current);
            return Err(LxmfInboxPortError::InvalidRequest);
        }
        if through.get() == current.acknowledged_through() {
            *self.lxmf_mailbox = Some(current);
            return if current.acknowledged_cursor() == Some(acknowledged) {
                project_lxmf_mailbox_status(store, &current).ok_or(LxmfInboxPortError::Faulted)
            } else {
                Err(LxmfInboxPortError::Faulted)
            };
        }

        let binding = current.binding();
        let region = PartitionNorFlash::new(
            &mut *self.flash,
            LXMF_MAILBOX_STATE_OFFSET,
            LXMF_MAILBOX_STATE_LEN,
        );
        let mut access = BoundMailboxStore::new(region, binding);
        let mounted = match acknowledge_mailbox_through(current, &mut access, acknowledged) {
            Ok(mounted) => mounted,
            Err(error) => match mount_mailbox_store(&mut access) {
                Ok(MailboxStoreMount::Mounted(mounted))
                    if mounted.acknowledged_cursor() == Some(acknowledged) =>
                {
                    mounted
                }
                Ok(MailboxStoreMount::Mounted(mounted))
                    if mounted.acknowledged_through() <= latest.get() =>
                {
                    *self.lxmf_mailbox = Some(mounted);
                    return Err(map_mailbox_store_error(error));
                }
                _ => return Err(map_mailbox_store_error(error)),
            },
        };
        *self.lxmf_mailbox = Some(mounted);
        project_lxmf_mailbox_status(store, &mounted).ok_or(LxmfInboxPortError::Faulted)
    }
}

fn project_lxmf_mailbox_status(
    store: &MountedLxmfStore<'_>,
    mailbox: &MountedMailboxStore,
) -> Option<ApiLxmfMailboxStatus> {
    let latest = store
        .latest_handle()
        .and_then(|handle| ApiLxmfMessageHandle::new(handle.get()).ok());
    let acknowledged = if mailbox.acknowledged_through() == 0 {
        None
    } else {
        Some(ApiLxmfMessageHandle::new(mailbox.acknowledged_through()).ok()?)
    };
    ApiLxmfMailboxStatus::new(latest, acknowledged).ok()
}

fn map_mailbox_store_error(error: MailboxStoreError<ProductRegionError>) -> LxmfInboxPortError {
    match error {
        MailboxStoreError::Backend(_) => LxmfInboxPortError::Backend,
        MailboxStoreError::Fault(
            reticulum_lxmf_mailbox_store::MailboxStoreFault::InvalidBinding,
        ) => LxmfInboxPortError::Binding,
        MailboxStoreError::Fault(_) => LxmfInboxPortError::Faulted,
    }
}

impl LxmfComposePort for ProductAuthenticatedApiPort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        if !self.lxmf_compose_service_enabled {
            return CapabilityAvailability::Unavailable;
        }
        SubmissionPort::availability(self)
    }

    fn compose_and_accept(
        &mut self,
        request: LxmfComposeRequest<'_>,
    ) -> Result<LxmfComposeAcceptance, LxmfComposePortError> {
        if LxmfComposePort::availability(self) != CapabilityAvailability::Available {
            return Err(LxmfComposePortError::Unavailable);
        }

        let destination = NodeDestinationHash::new(*request.destination().as_bytes());
        let mut wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
        let prepared = match request.location() {
            Some(location) => self
                .supervisor
                .prepare_basic_direct_lxmf_with_location_into(
                    &destination,
                    request.timestamp_unix_ms(),
                    request.title(),
                    request.content(),
                    LxmfMessageLocation::new(
                        location.latitude_e6(),
                        location.longitude_e6(),
                        location.altitude_cm(),
                        location.speed_cm_per_second(),
                        location.bearing_centidegrees(),
                        location.accuracy_cm(),
                        location.updated_at_unix_seconds(),
                    ),
                    &mut wire,
                ),
            None => self.supervisor.prepare_basic_direct_lxmf_into(
                &destination,
                request.timestamp_unix_ms(),
                request.title(),
                request.content(),
                &mut wire,
            ),
        }
        .map_err(map_lxmf_compose_error)?;
        let wire_len = usize::from(prepared.wire_len());
        let intent = LxmfMessageIntent::new(&wire[..wire_len])
            .map_err(|_| LxmfComposePortError::InvalidRequest)?;
        let candidate = AcceptanceCandidate::new(
            request.principal(),
            request.idempotency_key(),
            intent,
            request.authorization(),
        );
        let acceptance = SubmissionPort::accept(self, candidate).map_err(map_lxmf_accept_error)?;
        Ok(LxmfComposeAcceptance::new(
            acceptance,
            prepared.message_id(),
        ))
    }
}

impl PeerDiscoveryPort for ProductAuthenticatedApiPort<'_> {
    fn availability(&mut self) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }

    fn max_app_data_bytes(&mut self) -> u16 {
        config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES as u16
    }

    fn next(
        &mut self,
        after: Option<ApiLxmfPeerDiscoveryCursor>,
    ) -> Result<ApiLxmfPeerDiscoveryPage, PeerDiscoveryPortError> {
        let mut history_gap = false;
        let cursor = match after {
            None => DiscoveryCursor::START,
            Some(cursor) if cursor.incarnation() == self.peer_discovery_incarnation => {
                DiscoveryCursor::new(cursor.after_generation())
            }
            Some(_) => {
                history_gap = true;
                DiscoveryCursor::START
            }
        };

        let page = match self.discovered_peers.page_after(cursor) {
            Ok(page) => page,
            Err(_) => {
                history_gap = true;
                self.discovered_peers
                    .page_after(DiscoveryCursor::START)
                    .map_err(|_| PeerDiscoveryPortError::Faulted)?
            }
        };
        history_gap |= page.history_gap();

        let peer = page
            .peer()
            .map(|peer| {
                let signal = peer.signal();
                let generation = ApiLxmfPeerGeneration::new(peer.generation().get())
                    .map_err(|_| PeerDiscoveryPortError::Faulted)?;
                ApiLxmfDiscoveredPeer::new(
                    ApiDestinationHash(*peer.destination().as_bytes()),
                    ApiIdentityHash::new(*peer.identity_hash().as_bytes()),
                    peer.app_data(),
                    peer.hops(),
                    peer.interface().get(),
                    signal.rssi_dbm(),
                    signal.snr_db(),
                    self.peer_discovery_now_ms
                        .saturating_sub(peer.observed_at().get()),
                    generation,
                )
                .map_err(|_| PeerDiscoveryPortError::Faulted)
            })
            .transpose()?;
        let latest_generation = self
            .discovered_peers
            .latest_generation()
            .map(|generation| ApiLxmfPeerGeneration::new(generation.get()))
            .transpose()
            .map_err(|_| PeerDiscoveryPortError::Faulted)?;
        let oldest_retained_generation = page
            .oldest_retained()
            .map(|generation| ApiLxmfPeerGeneration::new(generation.get()))
            .transpose()
            .map_err(|_| PeerDiscoveryPortError::Faulted)?;

        Ok(ApiLxmfPeerDiscoveryPage::new(
            ApiLxmfPeerDiscoveryCursor::new(
                self.peer_discovery_incarnation,
                page.next_cursor().get(),
            ),
            latest_generation,
            oldest_retained_generation,
            history_gap,
            peer,
        ))
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

const fn map_lxmf_compose_error(error: PrepareBasicLxmfError) -> LxmfComposePortError {
    match error {
        PrepareBasicLxmfError::InvalidTimestamp
        | PrepareBasicLxmfError::TimestampTooLarge { .. }
        | PrepareBasicLxmfError::PayloadTooLarge { .. } => LxmfComposePortError::InvalidRequest,
        PrepareBasicLxmfError::DeliveryDestinationUnavailable => LxmfComposePortError::Unavailable,
        PrepareBasicLxmfError::Signing
        | PrepareBasicLxmfError::OutputTooSmall { .. }
        | PrepareBasicLxmfError::Invariant => LxmfComposePortError::Invariant,
    }
}

const fn map_lxmf_accept_error(error: SubmissionPortError) -> LxmfComposePortError {
    match error {
        SubmissionPortError::Unavailable => LxmfComposePortError::Unavailable,
        SubmissionPortError::Busy => LxmfComposePortError::Busy,
        SubmissionPortError::Backend => LxmfComposePortError::Backend,
        SubmissionPortError::Binding => LxmfComposePortError::Binding,
        SubmissionPortError::Faulted => LxmfComposePortError::Faulted,
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

#[cfg(feature = "appliance")]
fn bond_store_fault(error: BondStoreError<ProductRegionError>) -> Option<BondStoreFault> {
    match error {
        BondStoreError::Backend(_) => None,
        BondStoreError::Fault(fault) => Some(fault),
    }
}

fn validate_partition_table(flash: &mut FlashStorage<'_>) -> Result<(), ProductFlashOpenError> {
    let mut bytes = [0_u8; PARTITION_TABLE_MAX_LEN];
    let table = read_partition_table(flash, &mut bytes)
        .map_err(|_| ProductFlashOpenError::PartitionTable)?;

    let mut identity_count = 0_u8;
    let mut clock_count = 0_u8;
    let mut credentials_count = 0_u8;
    let mut ble_bond_count = 0_u8;
    let mut config_count = 0_u8;
    let mut journal_count = 0_u8;
    let mut lxmf_store_count = 0_u8;
    let mut identity_valid = false;
    let mut clock_valid = false;
    let mut credentials_valid = false;
    let mut ble_bond_valid = false;
    let mut config_valid = false;
    let mut journal_valid = false;
    let mut lxmf_store_valid = false;

    for entry in table.iter() {
        let label = entry.label();
        let named_identity = label == NODE_IDENTITY_LABEL_BYTES;
        let named_clock = label == ANNOUNCE_CLOCK_LABEL_BYTES;
        let named_credentials = label == API_CREDENTIALS_LABEL_BYTES;
        let named_ble_bond = label == BLE_BOND_LABEL_BYTES;
        let named_config = label == DEVICE_CONFIG_LABEL_BYTES;
        let named_journal = label == NODE_JOURNAL_LABEL_BYTES;
        let named_lxmf_store = label == LXMF_STORE_LABEL_BYTES;
        if named_identity {
            identity_count = identity_count.saturating_add(1);
        }
        if named_clock {
            clock_count = clock_count.saturating_add(1);
        }
        if named_credentials {
            credentials_count = credentials_count.saturating_add(1);
        }
        if named_ble_bond {
            ble_bond_count = ble_bond_count.saturating_add(1);
        }
        if named_config {
            config_count = config_count.saturating_add(1);
        }
        if named_journal {
            journal_count = journal_count.saturating_add(1);
        }
        if named_lxmf_store {
            lxmf_store_count = lxmf_store_count.saturating_add(1);
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
            ("ble_bond", BLE_BOND_OFFSET, BLE_BOND_LEN, named_ble_bond),
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
                "lxmf_store",
                LXMF_STORE_OFFSET,
                LXMF_STORE_LEN,
                named_lxmf_store,
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
        if named_ble_bond {
            ble_bond_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
                && entry.offset() == BLE_BOND_OFFSET
                && entry.len() == BLE_BOND_LEN
                && plain_writable;
        }
        if named_config {
            config_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
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
        if named_lxmf_store {
            lxmf_store_valid = entry.raw_type() == DATA_PARTITION_TYPE
                && entry.raw_subtype() == UNDEFINED_DATA_SUBTYPE
                && entry.offset() == LXMF_STORE_OFFSET
                && entry.len() == LXMF_STORE_LEN
                && plain_writable;
        }
    }

    if identity_count != 1
        || clock_count != 1
        || credentials_count != 1
        || ble_bond_count != 1
        || config_count != 1
        || journal_count != 1
        || lxmf_store_count != 1
    {
        return Err(ProductFlashOpenError::PartitionCardinality {
            identity: identity_count,
            announce_clock: clock_count,
            api_credentials: credentials_count,
            ble_bond: ble_bond_count,
            device_config: config_count,
            node_journal: journal_count,
            lxmf_store: lxmf_store_count,
        });
    }
    for (valid, name) in [
        (identity_valid, "node_identity"),
        (clock_valid, "announce_clock"),
        (credentials_valid, "api_credentials"),
        (ble_bond_valid, "ble_bond"),
        (config_valid, "device_config"),
        (journal_valid, "node_journal"),
        (lxmf_store_valid, "lxmf_store"),
    ] {
        if !valid {
            return Err(ProductFlashOpenError::PartitionShape(name));
        }
    }
    Ok(())
}

const _: () = assert!(mem::size_of::<FlashStorage<'static>>() <= 32);
