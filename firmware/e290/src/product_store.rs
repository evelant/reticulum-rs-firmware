//! One product-state owner above PRNS.
//!
//! The bootloader exposes one `product_state` arena. This owner assigns typed,
//! bounded quotas within that arena while retaining the sole blocking view of
//! physical flash. Those quotas are a product-store format detail, not board
//! partitions and not a function of which Reticulum applications are enabled.

use core::{mem, num::NonZeroU64};

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::ota::OtaImageState;
use esp_bootloader_esp_idf::partitions::{
    AppPartitionSubType, Error as EspPartitionError, PARTITION_TABLE_MAX_LEN,
};
use esp_storage::FlashStorageError;
use rand_core::{CryptoRng, RngCore};
use reticulum_device_api::{
    ApplianceLabel as ApiApplianceLabel, ApplianceLabelMutationOutcome,
    ApplianceLabelMutationRequest, ApplianceLabelSnapshot, ApplianceLabelSummary,
    DestinationHash as ApiDestinationHash, ESP_IMAGE_MAGIC, GatewayPolicy,
    IngressObservation as ApiIngressObservation, IngressSignal as ApiIngressSignal,
    LoraRadioProfile as ApiLoraRadioProfile, LoraTransmitPowerDbm,
    LxmfMailboxStatus as ApiLxmfMailboxStatus, LxmfMessageHandle as ApiLxmfMessageHandle,
    LxmfMessageSummary as ApiLxmfMessageSummary, LxmfReadChunk as ApiLxmfReadChunk,
    LxmfReadLength as ApiLxmfReadLength, MAX_LXMF_READ_CHUNK_BYTES, NetworkConfigMutation,
    NetworkConfigMutationOutcome, NetworkConfigMutationRequest, NetworkConfigSnapshot, OtaSlot,
    ReticulumInterfaceId, ReticulumTcpPeerConfigSummary, ReticulumTcpPeerHostConfigSummary,
    ReticulumTcpPeerIpv4Address, RmapConfig, RmapLocation, WifiCredentialUpdate,
    WifiNetworkConfigSummary, WifiNetworkProfileId,
};
use reticulum_e290_firmware::appliance_settings::{
    ApplianceLabel as StoredApplianceLabel, ApplianceSettings, ApplianceSettingsError,
    ApplianceSettingsFault, mount_appliance_settings, set_appliance_label,
};
use reticulum_e290_firmware::management_authorization::{
    EnrollmentCommit, ManagementAllowlist, ManagementAuthorizationError,
    ManagementAuthorizationFault, ManagementIdentity, enroll_management_identity,
    mount_management_allowlist,
};
use reticulum_e290_firmware::ota::OtaBackend;
use reticulum_e290_firmware::partition_contract::{
    APP_PARTITION_TYPE, APPLIANCE_SETTINGS_LEN, APPLIANCE_SETTINGS_OFFSET, DATA_PARTITION_TYPE,
    LXMF_MAILBOX_STATE_LEN, LXMF_MAILBOX_STATE_OFFSET, LXMF_OUTBOX_LEN, LXMF_OUTBOX_OFFSET,
    LXMF_STORE_LEN, LXMF_STORE_OFFSET, MANAGEMENT_AUTHORIZATION_LEN,
    MANAGEMENT_AUTHORIZATION_OFFSET, NETWORK_CONFIG_LEN, NETWORK_CONFIG_OFFSET, NODE_IDENTITY_LEN,
    NODE_IDENTITY_OFFSET, OTA_0_LABEL_BYTES, OTA_0_LEN, OTA_0_OFFSET, OTA_0_SUBTYPE,
    OTA_1_LABEL_BYTES, OTA_1_LEN, OTA_1_OFFSET, OTA_1_SUBTYPE, OTA_DATA_LABEL_BYTES, OTA_DATA_LEN,
    OTA_DATA_OFFSET, OTA_DATA_SUBTYPE, PRNS_STATE_LABEL_BYTES, PRNS_STATE_LEN, PRNS_STATE_OFFSET,
    PRODUCT_STATE_LABEL_BYTES, PRODUCT_STATE_LEN, PRODUCT_STATE_OFFSET, REQUIRED_FLASH_BYTES,
    UNDEFINED_DATA_SUBTYPE,
};
use reticulum_e290_firmware::product_identity::{
    DurableIdentityMaterial, IdentityBootReport, IdentityStoreError, boot_load_or_provision,
};
use reticulum_e290_firmware::product_outbox::{
    MountedOutbox, OutboxAcceptance, OutboxBinding, OutboxCandidate, OutboxEntry, OutboxError,
    OutboxIndexSlot, OutboxSubmissionId, accept as accept_outbox, mark_delivered,
    mount as mount_outbox, read_wire as read_outbox_wire,
};
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_station_profile::WifiStationBootPlan;
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_tcp_profile::WifiTcpBootstrap;
use reticulum_lxmf_durable_ingress::{DurableCarrierOutcome, commit_parsed_carrier};
use reticulum_lxmf_ingress::{CarrierIngress, SourceIdentityResolver, WireLimits};
use reticulum_lxmf_mailbox_store::{
    BoundMailboxStore, MailboxStoreBinding, MailboxStoreCursor, MailboxStoreError,
    MailboxStoreFault, MailboxStoreMount, MountedMailboxStore,
    acknowledge_through as acknowledge_mailbox_through, mount as mount_mailbox_store,
    provision as provision_mailbox_store,
};
use reticulum_lxmf_model::{
    DurableMessageReceipt, InboundTransportObservation, MessageHandle, MessageId,
};
use reticulum_lxmf_store::{
    BoundLxmfStore, LxmfStoreBinding, LxmfStoreDeviceId, LxmfStoreIndexSlot, LxmfWireReadError,
    MountedLxmfStore, mount as mount_lxmf_store,
};
use reticulum_network_config_store::{
    BoundNetworkConfigStore, LoraRadioProfile as StoredLoraRadioProfile, LoraTxPower,
    MountedNetworkConfigStore, NetworkConfig, NetworkConfigStoreBinding,
    NetworkConfigStoreDeviceId, NetworkConfigStoreError, NetworkConfigStoreFault, OutboundTcpPeer,
    OutboundTcpPeerAddress, PhoneLocation, RedactedNetworkConfig, WifiProfile, WifiProfileId,
    commit_successor as commit_network_config_successor, mount as mount_network_config_store,
    provision_erased as provision_network_config_erased,
};
use reticulum_nor_flash_region::{PartitionNorFlash, RegionError};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::shared_flash::E290ProductFlash;

type ProductRegionError = RegionError<FlashStorageError>;

/// Failure before the generic product arena can be used.
#[allow(
    dead_code,
    reason = "payloads are retained for target Debug diagnostics on fatal startup paths"
)]
#[derive(Debug)]
pub(crate) enum ProductStoreBootError {
    /// This image has not qualified an encrypted raw-NOR product-store path.
    FlashEncryptionEnabled,
    /// Physical capacity differs from the board contract.
    FlashCapacity { expected: usize, actual: usize },
    /// The bootloader table failed parsing or validation.
    PartitionTable,
    /// A required physical partition is absent or duplicated.
    PartitionCardinality(&'static str),
    /// A required physical partition has the wrong type, range, or flags.
    PartitionShape(&'static str),
    /// Another table row overlaps a required state arena.
    PartitionOverlap(&'static str),
    /// The durable Reticulum identity could not be loaded or provisioned.
    Identity(IdentityStoreError<ProductRegionError>),
}

/// Non-secret network-config mount classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductNetworkState {
    /// Exactly erased media; defaults apply until a future management commit.
    FreshErased,
    /// A committed snapshot was mounted.
    Mounted {
        generation: u64,
        wifi_profiles: usize,
        tcp_peer_configured: bool,
    },
    /// Programmed media or I/O failed closed for this boot.
    Unavailable,
}

#[allow(
    clippy::large_enum_variant,
    reason = "the sole flash owner keeps the bounded mounted store inline instead of allocating during boot"
)]
enum NetworkAuthority {
    FreshErased,
    Mounted(MountedNetworkConfigStore),
    Unavailable,
}

fn api_network_config_snapshot(
    revision: u64,
    redacted: RedactedNetworkConfig,
) -> Result<NetworkConfigSnapshot, ProductNetworkConfigError> {
    let mut wifi_profiles = [None; reticulum_device_api::MAX_WIFI_NETWORK_PROFILES];
    for (destination, source) in wifi_profiles.iter_mut().zip(redacted.wifi_profiles()) {
        let profile_id = WifiNetworkProfileId::new(*source.id().as_bytes())
            .map_err(|_| ProductNetworkConfigError::Faulted)?;
        *destination = Some(
            WifiNetworkConfigSummary::new(
                profile_id,
                source.enabled(),
                source.priority(),
                source.ssid(),
                source.password_configured(),
            )
            .map_err(|_| ProductNetworkConfigError::Faulted)?,
        );
    }
    let (tcp_peer, tcp_host_peer) = match redacted.tcp_peer() {
        Some(peer) => match peer.address() {
            OutboundTcpPeerAddress::Ipv4(ipv4) => {
                let address = ReticulumTcpPeerIpv4Address::new(ipv4)
                    .map_err(|_| ProductNetworkConfigError::Faulted)?;
                let peer = ReticulumTcpPeerConfigSummary::new(peer.enabled(), address, peer.port())
                    .map_err(|_| ProductNetworkConfigError::Faulted)?;
                (Some(peer), None)
            }
            OutboundTcpPeerAddress::Dns(hostname) => {
                let hostname = core::str::from_utf8(hostname.as_bytes())
                    .map_err(|_| ProductNetworkConfigError::Faulted)?;
                let peer =
                    ReticulumTcpPeerHostConfigSummary::new(peer.enabled(), hostname, peer.port())
                        .map_err(|_| ProductNetworkConfigError::Faulted)?;
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
        .map_err(|_| ProductNetworkConfigError::Faulted)?;
    let rmap_config = RmapConfig::new(
        redacted.rmap_discovery_enabled(),
        redacted.rmap_share_location(),
        phone_location,
    );
    let stored_lora_profile = redacted.lora_profile();
    let lora_tx_power_dbm =
        LoraTransmitPowerDbm::new(stored_lora_profile.tx_power().requested_dbm() as u8)
            .map_err(|_| ProductNetworkConfigError::Faulted)?;
    let lora_profile = ApiLoraRadioProfile::new(
        stored_lora_profile.frequency_hz(),
        stored_lora_profile.bandwidth_hz(),
        stored_lora_profile.spreading_factor(),
        stored_lora_profile.coding_rate_denominator(),
        lora_tx_power_dbm,
    )
    .map_err(|_| ProductNetworkConfigError::Faulted)?;
    NetworkConfigSnapshot::new(
        revision,
        wifi_profiles,
        tcp_peer,
        tcp_host_peer,
        gateway_policy,
        rmap_config,
        lora_profile,
    )
    .map_err(|_| ProductNetworkConfigError::Invariant)
}

fn store_wifi_profile_id(
    profile_id: WifiNetworkProfileId,
) -> Result<WifiProfileId, ProductNetworkConfigError> {
    WifiProfileId::new(*profile_id.as_bytes())
        .map_err(|_| ProductNetworkConfigError::InvalidRequest)
}

fn store_tcp_peer(
    peer: reticulum_device_api::ReticulumTcpPeerUpdate,
) -> Result<OutboundTcpPeer, ProductNetworkConfigError> {
    OutboundTcpPeer::new(peer.ipv4_address().octets(), peer.port(), peer.enabled())
        .map_err(|_| ProductNetworkConfigError::InvalidRequest)
}

fn store_tcp_host_peer(
    peer: reticulum_device_api::ReticulumTcpPeerHostUpdate<'_>,
) -> Result<OutboundTcpPeer, ProductNetworkConfigError> {
    OutboundTcpPeer::with_dns_hostname(
        peer.hostname().as_str().as_bytes(),
        peer.port(),
        peer.enabled(),
    )
    .map_err(|_| ProductNetworkConfigError::InvalidRequest)
}

fn store_rmap_location(location: RmapLocation) -> Result<PhoneLocation, ProductNetworkConfigError> {
    PhoneLocation::new(location.latitude_e6(), location.longitude_e6())
        .map_err(|_| ProductNetworkConfigError::InvalidRequest)
}

fn api_appliance_label_to_stored(
    label: ApiApplianceLabel<'_>,
) -> Result<StoredApplianceLabel, ProductApplianceSettingsError> {
    StoredApplianceLabel::new(label.as_str())
        .map_err(|_| ProductApplianceSettingsError::InvalidRequest)
}

fn map_appliance_settings_error<E>(
    error: &ApplianceSettingsError<E>,
) -> ProductApplianceSettingsError {
    match error {
        ApplianceSettingsError::Storage(_) => ProductApplianceSettingsError::Backend,
        ApplianceSettingsError::Fault(ApplianceSettingsFault::InvalidLabel) => {
            ProductApplianceSettingsError::InvalidRequest
        }
        ApplianceSettingsError::Fault(ApplianceSettingsFault::Unavailable) => {
            ProductApplianceSettingsError::Unavailable
        }
        ApplianceSettingsError::Fault(
            ApplianceSettingsFault::IncompatibleFlashGeometry
            | ApplianceSettingsFault::Corrupt
            | ApplianceSettingsFault::GenerationExhausted
            | ApplianceSettingsFault::Verification,
        ) => ProductApplianceSettingsError::Faulted,
    }
}

fn store_lora_profile(
    profile: ApiLoraRadioProfile,
) -> Result<StoredLoraRadioProfile, ProductNetworkConfigError> {
    let power = LoraTxPower::try_from_dbm(i32::from(profile.tx_power_dbm().get()))
        .map_err(|_| ProductNetworkConfigError::InvalidRequest)?;
    let stored = StoredLoraRadioProfile::new(
        profile.frequency_hz(),
        profile.bandwidth_hz(),
        profile.spreading_factor(),
        profile.coding_rate_denominator(),
        power,
    )
    .map_err(|_| ProductNetworkConfigError::InvalidRequest)?;
    reticulum_e290_firmware::prns_lora::prns_radio_profile(stored)
        .map_err(|_| ProductNetworkConfigError::InvalidRequest)?;
    Ok(stored)
}

fn network_config_mutation_is_noop(
    state: &NetworkAuthority,
    mutation: NetworkConfigMutation<'_>,
) -> Result<bool, ProductNetworkConfigError> {
    let configuration = match state {
        NetworkAuthority::Mounted(mounted) => Some(mounted.configuration()),
        NetworkAuthority::FreshErased => None,
        NetworkAuthority::Unavailable => return Err(ProductNetworkConfigError::Unavailable),
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
                    return Err(ProductNetworkConfigError::InvalidRequest);
                }
                if configuration.is_some_and(|configuration| {
                    configuration.wifi_profile_count()
                        >= reticulum_network_config_store::WIFI_PROFILE_CAPACITY
                }) {
                    return Err(ProductNetworkConfigError::InvalidRequest);
                }
                return Ok(false);
            };
            // Never compare replacement bytes with the stored secret: whether
            // a replacement matches must not become a revision/timing oracle.
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
                configuration.is_none_or(NetworkConfig::wifi_transport_enabled);
            let automatic_announces_enabled =
                configuration.is_none_or(NetworkConfig::automatic_announces_enabled);
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
    }
}

fn apply_network_config_mutation(
    configuration: &mut NetworkConfig,
    mutation: NetworkConfigMutation<'_>,
) -> Result<(), ProductNetworkConfigError> {
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
                        .ok_or(ProductNetworkConfigError::InvalidRequest)?;
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
            .map_err(|_| ProductNetworkConfigError::InvalidRequest)?;
            configuration
                .upsert_wifi_profile(profile)
                .map_err(|_| ProductNetworkConfigError::InvalidRequest)
        }
        NetworkConfigMutation::RemoveWifi { profile_id } => {
            configuration.remove_wifi_profile(store_wifi_profile_id(profile_id)?);
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
                .map_err(|_| ProductNetworkConfigError::InvalidRequest)?;
            configuration.set_lora_tx_power(power);
            Ok(())
        }
        NetworkConfigMutation::SetLoraProfile(profile) => {
            configuration.set_lora_profile(store_lora_profile(profile)?);
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
}

fn map_network_config_store_error(
    error: NetworkConfigStoreError<ProductRegionError>,
) -> ProductNetworkConfigError {
    match error {
        NetworkConfigStoreError::Binding(_) | NetworkConfigStoreError::Fault(_) => {
            ProductNetworkConfigError::Faulted
        }
        NetworkConfigStoreError::Backend(_) => ProductNetworkConfigError::Backend,
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "the sole flash owner keeps the bounded allow-list inline instead of allocating during boot"
)]
enum ManagementAuthority {
    Available(ManagementAllowlist),
    Unavailable,
}

enum ApplianceSettingsAuthority {
    Available(ApplianceSettings),
    Unavailable,
}

/// Non-secret management-authorization mount classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductManagementState {
    /// The typed quota mounted and can accept physical-presence enrollment.
    Available {
        generation: u64,
        identity_count: usize,
    },
    /// Programmed corruption or I/O disabled all privileged management.
    Unavailable,
}

/// Stable failure vocabulary for the product-owned LXMF inbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductLxmfError {
    /// The inbox or its watermark did not mount for this boot.
    Unavailable,
    /// The requested handle or byte range is invalid.
    InvalidRequest,
    /// Raw storage access failed.
    Backend,
    /// Persisted binding or application data was contradictory.
    Faulted,
}

/// Stable failure vocabulary for the product-owned outbound-intent store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductOutboxError {
    /// The outbox did not mount for this boot.
    Unavailable,
    /// The supplied candidate or output buffer was invalid.
    InvalidRequest,
    /// Raw storage access failed.
    Backend,
    /// Persisted binding or application data was contradictory.
    Faulted,
}

/// Stable failure vocabulary for the product-owned network configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductNetworkConfigError {
    /// The network-config quota did not mount for this boot.
    Unavailable,
    /// The requested semantic update is not valid for this board or state.
    InvalidRequest,
    /// Raw storage access failed and completion may have been ambiguous.
    Backend,
    /// Persisted binding or media state was contradictory.
    Faulted,
    /// A store projection violated an internal representation invariant.
    Invariant,
}

/// Stable failure vocabulary for product-owned appliance settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductApplianceSettingsError {
    /// The typed appliance-settings quota did not mount for this boot.
    Unavailable,
    /// The requested label violates the bounded product label contract.
    InvalidRequest,
    /// Raw storage access failed and completion may have been ambiguous.
    Backend,
    /// Persisted media or a verified write was contradictory.
    Faulted,
}

/// Identity plus the sole resident product-state owner established at boot.
pub(crate) struct BootedProductStore {
    pub(crate) identity: DurableIdentityMaterial,
    pub(crate) identity_report: IdentityBootReport,
    pub(crate) identity_write_calls: u32,
    pub(crate) identity_erase_calls: u32,
    pub(crate) store: ProductStore,
}

/// Sole blocking owner of application state on the shared physical flash.
pub(crate) struct ProductStore {
    flash: &'static mut E290ProductFlash,
    network_binding: NetworkConfigStoreBinding,
    network: NetworkAuthority,
    network_applied_revision: u64,
    management: ManagementAuthority,
    appliance_settings: ApplianceSettingsAuthority,
    lxmf_binding: LxmfStoreBinding,
    lxmf: Option<MountedLxmfStore<'static>>,
    lxmf_mailbox_binding: MailboxStoreBinding,
    lxmf_mailbox: Option<MountedMailboxStore>,
    outbox: Option<MountedOutbox<'static>>,
}

impl ProductStore {
    /// Validate the fixed physical layout, provision/load identity, and mount
    /// the initial typed quotas. Network and LXMF corruption disable only those
    /// application capabilities; identity failure is fatal to node startup.
    pub(crate) fn boot<R>(
        flash: &'static mut E290ProductFlash,
        base_mac: [u8; 6],
        rng: &mut R,
        lxmf_index: &'static mut [LxmfStoreIndexSlot],
        outbox_index: &'static mut [OutboxIndexSlot],
    ) -> Result<BootedProductStore, ProductStoreBootError>
    where
        R: RngCore + CryptoRng,
    {
        if esp_hal::efuse::flash_encryption() {
            return Err(ProductStoreBootError::FlashEncryptionEnabled);
        }
        let actual = embedded_storage::nor_flash::ReadNorFlash::capacity(flash);
        if actual != REQUIRED_FLASH_BYTES {
            return Err(ProductStoreBootError::FlashCapacity {
                expected: REQUIRED_FLASH_BYTES,
                actual,
            });
        }
        validate_partition_table(flash)?;

        let (identity, identity_report, identity_write_calls, identity_erase_calls) = {
            let mut region =
                PartitionNorFlash::new(&mut *flash, NODE_IDENTITY_OFFSET, NODE_IDENTITY_LEN);
            let (identity, report) = boot_load_or_provision(&mut region, rng)
                .map_err(ProductStoreBootError::Identity)?;
            (identity, report, region.write_calls(), region.erase_calls())
        };

        let device_id = flash_device_id(base_mac);
        let network_binding = NetworkConfigStoreBinding::new(
            NetworkConfigStoreDeviceId::new(device_id),
            NETWORK_CONFIG_OFFSET as usize,
            NETWORK_CONFIG_LEN as usize,
            reticulum_network_config_store::PHYSICAL_FORMAT_VERSION,
        );
        let network = {
            let region =
                PartitionNorFlash::new(&mut *flash, NETWORK_CONFIG_OFFSET, NETWORK_CONFIG_LEN);
            let mut access = BoundNetworkConfigStore::new(region, network_binding);
            match mount_network_config_store(&mut access) {
                Ok(mounted) => NetworkAuthority::Mounted(mounted),
                Err(NetworkConfigStoreError::Fault(NetworkConfigStoreFault::UnformattedErased)) => {
                    NetworkAuthority::FreshErased
                }
                Err(_) => NetworkAuthority::Unavailable,
            }
        };
        let network_applied_revision = match &network {
            NetworkAuthority::Mounted(mounted) => mounted.revision().generation().get(),
            NetworkAuthority::FreshErased | NetworkAuthority::Unavailable => 0,
        };

        let management = {
            let mut region = PartitionNorFlash::new(
                &mut *flash,
                MANAGEMENT_AUTHORIZATION_OFFSET,
                MANAGEMENT_AUTHORIZATION_LEN,
            );
            match mount_management_allowlist(&mut region) {
                Ok(allowlist) => ManagementAuthority::Available(allowlist),
                Err(_) => ManagementAuthority::Unavailable,
            }
        };

        let appliance_settings = {
            let mut region = PartitionNorFlash::new(
                &mut *flash,
                APPLIANCE_SETTINGS_OFFSET,
                APPLIANCE_SETTINGS_LEN,
            );
            match mount_appliance_settings(&mut region) {
                Ok(settings) => ApplianceSettingsAuthority::Available(settings),
                Err(_) => ApplianceSettingsAuthority::Unavailable,
            }
        };

        let lxmf_binding = LxmfStoreBinding::new(
            LxmfStoreDeviceId::new(device_id),
            LXMF_STORE_OFFSET as usize,
            LXMF_STORE_LEN as usize,
            reticulum_lxmf_store::PHYSICAL_FORMAT_VERSION,
        );
        let lxmf = {
            let region = PartitionNorFlash::new(&mut *flash, LXMF_STORE_OFFSET, LXMF_STORE_LEN);
            let mut access = BoundLxmfStore::new(region, lxmf_binding);
            mount_lxmf_store(&mut access, lxmf_index).ok()
        };
        let lxmf_mailbox_binding = MailboxStoreBinding::new(
            reticulum_lxmf_mailbox_store::MailboxStoreDeviceId::new(device_id),
            LXMF_MAILBOX_STATE_OFFSET as usize,
            LXMF_MAILBOX_STATE_LEN as usize,
            reticulum_lxmf_mailbox_store::PHYSICAL_FORMAT_VERSION,
        );
        let lxmf_mailbox = lxmf.as_ref().and_then(|lxmf| {
            mount_or_provision_lxmf_mailbox(flash, lxmf_mailbox_binding, lxmf).ok()
        });
        let outbox_binding = OutboxBinding::new(
            device_id,
            LXMF_OUTBOX_OFFSET as usize,
            LXMF_OUTBOX_LEN as usize,
        );
        let outbox = {
            let mut region =
                PartitionNorFlash::new(&mut *flash, LXMF_OUTBOX_OFFSET, LXMF_OUTBOX_LEN);
            mount_outbox(&mut region, outbox_binding, outbox_index).ok()
        };

        Ok(BootedProductStore {
            identity,
            identity_report,
            identity_write_calls,
            identity_erase_calls,
            store: ProductStore {
                flash,
                network_binding,
                network,
                network_applied_revision,
                management,
                appliance_settings,
                lxmf_binding,
                lxmf,
                lxmf_mailbox_binding,
                lxmf_mailbox,
                outbox,
            },
        })
    }

    /// Non-secret management authorization state recovered from product flash.
    pub(crate) fn management_state(&self) -> ProductManagementState {
        match &self.management {
            ManagementAuthority::Available(allowlist) => ProductManagementState::Available {
                generation: allowlist.generation(),
                identity_count: allowlist.len(),
            },
            ManagementAuthority::Unavailable => ProductManagementState::Unavailable,
        }
    }

    /// Read the ESP bootloader's state for the currently selected OTA image.
    ///
    /// Erased `otadata` is the expected state after initial provisioning: the
    /// bootloader selects `ota_0` without creating an OTA selection record, so
    /// that case is reported as `None` rather than as a product-store fault.
    pub(crate) fn current_ota_image_state(
        &mut self,
    ) -> Result<Option<OtaImageState>, ProductOtaError> {
        match self.flash.current_ota_state() {
            Ok(state) => Ok(Some(state)),
            Err(EspPartitionError::InvalidState) => Ok(None),
            Err(_) => Err(ProductOtaError::Partition),
        }
    }

    /// Confirm that the currently running rollback candidate is healthy.
    ///
    /// This deliberately updates only ESP-IDF's existing `otadata` state. The
    /// product does not maintain a parallel last-known-good record. A caller
    /// must invoke this only after its board/application health window passes.
    pub(crate) fn confirm_pending_ota_image(&mut self) -> Result<bool, ProductOtaError> {
        match self
            .flash
            .current_ota_state()
            .map_err(|_| ProductOtaError::Partition)?
        {
            OtaImageState::PendingVerify => {}
            _ => return Ok(false),
        }
        self.flash
            .set_current_ota_state(OtaImageState::Valid)
            .map_err(|_| ProductOtaError::Partition)?;
        match self.flash.current_ota_state() {
            Ok(OtaImageState::Valid) => Ok(true),
            Ok(_) | Err(_) => Err(ProductOtaError::Readback),
        }
    }

    /// Copy the current durable identity set for seeding PRNS's live allow-list.
    pub(crate) fn management_allowlist(&self) -> Option<ManagementAllowlist> {
        match &self.management {
            ManagementAuthority::Available(allowlist) => Some(*allowlist),
            ManagementAuthority::Unavailable => None,
        }
    }

    /// Current product-owned appliance label, if configured and available.
    pub(crate) fn appliance_label(&self) -> Option<StoredApplianceLabel> {
        match &self.appliance_settings {
            ApplianceSettingsAuthority::Available(settings) => settings.label(),
            ApplianceSettingsAuthority::Unavailable => None,
        }
    }

    /// Read the current durable product-owned appliance-label snapshot.
    pub(crate) fn appliance_label_snapshot(
        &self,
    ) -> Result<ApplianceLabelSnapshot, ProductApplianceSettingsError> {
        let ApplianceSettingsAuthority::Available(settings) = &self.appliance_settings else {
            return Err(ProductApplianceSettingsError::Unavailable);
        };
        let label = settings
            .label()
            .map(|label| ApplianceLabelSummary::new(label.as_str()))
            .transpose()
            .map_err(|_| ProductApplianceSettingsError::Faulted)?;
        Ok(ApplianceLabelSnapshot::new(settings.generation(), label))
    }

    /// Compare-and-swap the product-owned appliance label.
    pub(crate) fn mutate_appliance_label(
        &mut self,
        request: ApplianceLabelMutationRequest<'_>,
    ) -> Result<ApplianceLabelMutationOutcome, ProductApplianceSettingsError> {
        let ApplianceSettingsAuthority::Available(settings) = &mut self.appliance_settings else {
            return Err(ProductApplianceSettingsError::Unavailable);
        };
        if request.expected_revision != settings.generation() {
            return Ok(ApplianceLabelMutationOutcome::RevisionConflict {
                current_revision: settings.generation(),
            });
        }
        let label = request
            .label
            .map(api_appliance_label_to_stored)
            .transpose()?;
        let mut region = PartitionNorFlash::new(
            &mut *self.flash,
            APPLIANCE_SETTINGS_OFFSET,
            APPLIANCE_SETTINGS_LEN,
        );
        let result = set_appliance_label(&mut region, settings, label);
        match result {
            Ok(commit) => {
                let revision = match commit {
                    reticulum_e290_firmware::appliance_settings::ApplianceSettingsCommit::Unchanged {
                        revision,
                    }
                    | reticulum_e290_firmware::appliance_settings::ApplianceSettingsCommit::Updated {
                        revision,
                    } => revision,
                };
                Ok(ApplianceLabelMutationOutcome::Applied { revision })
            }
            Err(error) => {
                let mapped = map_appliance_settings_error(&error);
                if matches!(
                    error,
                    ApplianceSettingsError::Storage(_)
                        | ApplianceSettingsError::Fault(
                            ApplianceSettingsFault::Corrupt
                                | ApplianceSettingsFault::IncompatibleFlashGeometry
                                | ApplianceSettingsFault::Verification
                        )
                ) {
                    self.appliance_settings = ApplianceSettingsAuthority::Unavailable;
                }
                Err(mapped)
            }
        }
    }

    /// Commit an identified Link peer before asking PRNS to admit it live.
    ///
    /// Any ambiguous flash failure disables further management authorization
    /// for this boot. A clean remount on the next boot decides whether the new
    /// generation committed.
    pub(crate) fn enroll_management_identity(
        &mut self,
        identity: ManagementIdentity,
    ) -> Result<EnrollmentCommit, ManagementAuthorizationError<ProductRegionError>> {
        let result = match &mut self.management {
            ManagementAuthority::Available(allowlist) => {
                let mut region = PartitionNorFlash::new(
                    &mut *self.flash,
                    MANAGEMENT_AUTHORIZATION_OFFSET,
                    MANAGEMENT_AUTHORIZATION_LEN,
                );
                enroll_management_identity(&mut region, allowlist, identity)
            }
            ManagementAuthority::Unavailable => Err(ManagementAuthorizationError::Fault(
                ManagementAuthorizationFault::Unavailable,
            )),
        };
        if matches!(
            &result,
            Err(ManagementAuthorizationError::Storage(_))
                | Err(ManagementAuthorizationError::Fault(
                    ManagementAuthorizationFault::Corrupt
                        | ManagementAuthorizationFault::IncompatibleFlashGeometry
                        | ManagementAuthorizationFault::Verification
                ))
        ) {
            self.management = ManagementAuthority::Unavailable;
        }
        result
    }

    /// Non-secret boot classification of the network-config quota.
    pub(crate) fn network_state(&self) -> ProductNetworkState {
        match &self.network {
            NetworkAuthority::FreshErased => ProductNetworkState::FreshErased,
            NetworkAuthority::Mounted(mounted) => ProductNetworkState::Mounted {
                generation: mounted.revision().generation().get(),
                wifi_profiles: mounted.configuration().wifi_profile_count(),
                tcp_peer_configured: mounted.configuration().tcp_peer().is_some(),
            },
            NetworkAuthority::Unavailable => ProductNetworkState::Unavailable,
        }
    }

    /// Whether the revisioned network-config quota is usable for this boot.
    pub(crate) fn network_config_available(&self) -> bool {
        !matches!(self.network, NetworkAuthority::Unavailable)
    }

    /// Configuration generation selected by the immutable boot-time actors.
    ///
    /// A later management commit changes desired state but cannot silently
    /// reconfigure the radio, Wi-Fi controller, or TCP actor in place.
    pub(crate) const fn network_applied_revision(&self) -> u64 {
        self.network_applied_revision
    }

    fn network_config_revision(&self) -> Result<u64, ProductNetworkConfigError> {
        match &self.network {
            NetworkAuthority::FreshErased => Ok(0),
            NetworkAuthority::Mounted(mounted) => Ok(mounted.revision().generation().get()),
            NetworkAuthority::Unavailable => Err(ProductNetworkConfigError::Unavailable),
        }
    }

    /// Read the complete desired configuration without returning credentials.
    pub(crate) fn network_config_snapshot(
        &self,
    ) -> Result<NetworkConfigSnapshot, ProductNetworkConfigError> {
        match &self.network {
            NetworkAuthority::FreshErased => {
                api_network_config_snapshot(0, NetworkConfig::empty().redacted())
            }
            NetworkAuthority::Mounted(mounted) => api_network_config_snapshot(
                mounted.revision().generation().get(),
                mounted.redacted(),
            ),
            NetworkAuthority::Unavailable => Err(ProductNetworkConfigError::Unavailable),
        }
    }

    /// Apply one compare-and-swap desired-network update to the typed quota.
    ///
    /// The idempotency key remains correlation metadata, matching the API
    /// contract; this owner does not restore the alpha-only boot-scoped replay
    /// receipt as an additional protocol invariant.
    pub(crate) fn mutate_network_config(
        &mut self,
        request: NetworkConfigMutationRequest<'_>,
    ) -> Result<NetworkConfigMutationOutcome, ProductNetworkConfigError> {
        let current_revision = self.network_config_revision()?;
        if request.expected_revision() != current_revision {
            return Ok(NetworkConfigMutationOutcome::RevisionConflict { current_revision });
        }
        if network_config_mutation_is_noop(&self.network, request.mutation())? {
            return Ok(NetworkConfigMutationOutcome::Applied {
                revision: current_revision,
                reboot_required: current_revision != self.network_applied_revision,
            });
        }
        self.commit_network_config_mutation(request)
    }

    fn remount_network_config(&mut self, allow_fresh_erased: bool) {
        let region =
            PartitionNorFlash::new(&mut *self.flash, NETWORK_CONFIG_OFFSET, NETWORK_CONFIG_LEN);
        let mut access = BoundNetworkConfigStore::new(region, self.network_binding);
        self.network = match mount_network_config_store(&mut access) {
            Ok(mounted) => NetworkAuthority::Mounted(mounted),
            Err(NetworkConfigStoreError::Fault(NetworkConfigStoreFault::UnformattedErased))
                if allow_fresh_erased =>
            {
                NetworkAuthority::FreshErased
            }
            Err(_) => NetworkAuthority::Unavailable,
        };
    }

    fn commit_network_config_mutation(
        &mut self,
        request: NetworkConfigMutationRequest<'_>,
    ) -> Result<NetworkConfigMutationOutcome, ProductNetworkConfigError> {
        let prior = mem::replace(&mut self.network, NetworkAuthority::Unavailable);
        let (predecessor, mut configuration) = match prior {
            NetworkAuthority::FreshErased => (None, NetworkConfig::empty()),
            NetworkAuthority::Mounted(mounted) => {
                let revision = mounted.revision();
                (Some(revision), mounted.into_configuration())
            }
            NetworkAuthority::Unavailable => {
                return Err(ProductNetworkConfigError::Unavailable);
            }
        };
        if let Err(error) = apply_network_config_mutation(&mut configuration, request.mutation()) {
            self.remount_network_config(predecessor.is_none());
            return Err(error);
        }

        let region =
            PartitionNorFlash::new(&mut *self.flash, NETWORK_CONFIG_OFFSET, NETWORK_CONFIG_LEN);
        let mut access = BoundNetworkConfigStore::new(region, self.network_binding);
        let commit = match predecessor {
            Some(predecessor) => {
                commit_network_config_successor(&mut access, predecessor, &configuration)
            }
            None => provision_network_config_erased(&mut access, &configuration),
        };
        match commit {
            Ok(mounted) => {
                let revision = mounted.revision().generation().get();
                self.network = NetworkAuthority::Mounted(mounted);
                Ok(NetworkConfigMutationOutcome::Applied {
                    revision,
                    reboot_required: true,
                })
            }
            Err(commit_error) => match mount_network_config_store(&mut access) {
                Ok(mounted) => {
                    let revision = mounted.revision().generation().get();
                    let intended_revision =
                        request.expected_revision().checked_add(1) == Some(revision);
                    let intended = intended_revision
                        && network_configurations_equal(mounted.configuration(), &configuration);
                    self.network = NetworkAuthority::Mounted(mounted);
                    if intended {
                        Ok(NetworkConfigMutationOutcome::Applied {
                            revision,
                            reboot_required: true,
                        })
                    } else {
                        Err(map_network_config_store_error(commit_error))
                    }
                }
                Err(NetworkConfigStoreError::Fault(NetworkConfigStoreFault::UnformattedErased))
                    if predecessor.is_none() =>
                {
                    self.network = NetworkAuthority::FreshErased;
                    Err(map_network_config_store_error(commit_error))
                }
                Err(reconcile_error) => {
                    self.network = NetworkAuthority::Unavailable;
                    Err(map_network_config_store_error(reconcile_error))
                }
            },
        }
    }

    /// Boot-selected LoRa profile, or the board default when config is absent.
    pub(crate) fn lora_profile(&self) -> StoredLoraRadioProfile {
        match &self.network {
            NetworkAuthority::Mounted(mounted) => mounted.configuration().lora_profile(),
            NetworkAuthority::FreshErased | NetworkAuthority::Unavailable => {
                StoredLoraRadioProfile::DEFAULT
            }
        }
    }

    /// Whether optional RMAP publication is enabled for this boot.
    pub(crate) fn rmap_discovery_enabled(&self) -> bool {
        match &self.network {
            NetworkAuthority::Mounted(mounted) => mounted.configuration().rmap_discovery_enabled(),
            NetworkAuthority::FreshErased | NetworkAuthority::Unavailable => false,
        }
    }

    /// Immutable Wi-Fi station plan copied from the mounted config.
    #[cfg(feature = "gateway")]
    pub(crate) fn wifi_station_boot_plan(&self) -> Option<WifiStationBootPlan> {
        match &self.network {
            NetworkAuthority::Mounted(mounted) => Some(WifiStationBootPlan::select(
                mounted.revision().generation().get(),
                mounted.configuration(),
            )),
            NetworkAuthority::FreshErased => Some(WifiStationBootPlan::Disabled {
                applied_revision: 0,
            }),
            NetworkAuthority::Unavailable => None,
        }
    }

    /// Enabled immutable outbound TCP endpoint copied from durable config.
    #[cfg(feature = "gateway")]
    pub(crate) fn wifi_tcp_bootstrap(&self) -> Option<WifiTcpBootstrap> {
        let NetworkAuthority::Mounted(mounted) = &self.network else {
            return None;
        };
        if !mounted.configuration().wifi_transport_enabled() {
            return None;
        }
        let peer = mounted
            .configuration()
            .tcp_peer()
            .filter(|peer| peer.enabled())?;
        let generation = mounted.revision().generation().get();
        match peer.address() {
            OutboundTcpPeerAddress::Ipv4(ipv4) => {
                WifiTcpBootstrap::new(generation, ipv4, peer.port()).ok()
            }
            OutboundTcpPeerAddress::Dns(hostname) => {
                WifiTcpBootstrap::with_dns(generation, hostname.as_bytes(), peer.port()).ok()
            }
        }
    }

    /// Whether the LXMF app quota mounted cleanly.
    pub(crate) fn lxmf_available(&self) -> bool {
        self.lxmf.is_some() && self.lxmf_mailbox.is_some()
    }

    /// Committed LXMF messages recovered at boot.
    pub(crate) fn lxmf_message_count(&self) -> Option<usize> {
        self.lxmf.as_ref().map(MountedLxmfStore::message_count)
    }

    /// Consumed LXMF extents recovered at boot.
    pub(crate) fn lxmf_consumed_extents(&self) -> Option<usize> {
        self.lxmf.as_ref().map(MountedLxmfStore::consumed_extents)
    }

    /// Message owning reconciliation after an ambiguous store operation.
    pub(crate) fn lxmf_pending_message_id(&self) -> Option<MessageId> {
        self.lxmf
            .as_ref()
            .and_then(MountedLxmfStore::pending_message_id)
    }

    /// Validate and commit one PRNS-delivered LXMF carrier.
    pub(crate) fn commit_lxmf_carrier<R>(
        &mut self,
        carrier: CarrierIngress<'_>,
        ingress: Option<InboundTransportObservation>,
        limits: WireLimits,
        identities: &R,
    ) -> Option<DurableCarrierOutcome<ProductRegionError>>
    where
        R: SourceIdentityResolver + ?Sized,
    {
        let lxmf = self.lxmf.as_mut()?;
        let region = PartitionNorFlash::new(&mut *self.flash, LXMF_STORE_OFFSET, LXMF_STORE_LEN);
        let mut access = BoundLxmfStore::new(region, self.lxmf_binding);
        Some(commit_parsed_carrier(
            carrier,
            ingress,
            limits,
            identities,
            lxmf,
            &mut access,
        ))
    }

    /// Return the next committed inbox entry after an optional stable handle.
    pub(crate) fn lxmf_next(
        &self,
        after: Option<ApiLxmfMessageHandle>,
    ) -> Result<Option<ApiLxmfMessageSummary>, ProductLxmfError> {
        let store = self.lxmf.as_ref().ok_or(ProductLxmfError::Unavailable)?;
        self.lxmf_mailbox
            .as_ref()
            .ok_or(ProductLxmfError::Unavailable)?;
        let mut after_seen = after.is_none();
        for receipt in store.receipts() {
            if !after_seen {
                after_seen = after.is_some_and(|cursor| cursor.get() == receipt.handle().get());
                continue;
            }
            let metadata = store
                .metadata(receipt.handle())
                .ok_or(ProductLxmfError::Faulted)?;
            let lengths = metadata.lengths();
            let handle = ApiLxmfMessageHandle::new(receipt.handle().get())
                .map_err(|_| ProductLxmfError::Faulted)?;
            let ingress = metadata.ingress_observation().map(|ingress| {
                ApiIngressObservation::new(
                    ReticulumInterfaceId::new(*ingress.interface().as_bytes()),
                    ingress
                        .signal()
                        .map(|signal| ApiIngressSignal::new(signal.rssi_dbm(), signal.snr_db())),
                )
            });
            return ApiLxmfMessageSummary::new(
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
            .map(|summary| Some(summary.with_ingress_observation(ingress)))
            .map_err(|_| ProductLxmfError::Faulted);
        }
        Ok(None)
    }

    /// Read one bounded exact-wire chunk from a committed inbox entry.
    pub(crate) fn lxmf_read(
        &mut self,
        handle: ApiLxmfMessageHandle,
        offset: u32,
        max_bytes: ApiLxmfReadLength,
    ) -> Result<Option<ApiLxmfReadChunk>, ProductLxmfError> {
        let store = self.lxmf.as_ref().ok_or(ProductLxmfError::Unavailable)?;
        self.lxmf_mailbox
            .as_ref()
            .ok_or(ProductLxmfError::Unavailable)?;
        let model_handle =
            MessageHandle::new(handle.get()).map_err(|_| ProductLxmfError::InvalidRequest)?;
        let mut bytes = [0_u8; MAX_LXMF_READ_CHUNK_BYTES];
        let requested = usize::from(max_bytes.get());
        let region = PartitionNorFlash::new(&mut *self.flash, LXMF_STORE_OFFSET, LXMF_STORE_LEN);
        let mut access = BoundLxmfStore::new(region, self.lxmf_binding);
        let chunk =
            match store.read_wire_chunk(&mut access, model_handle, offset, &mut bytes[..requested])
            {
                Ok(chunk) => chunk,
                Err(LxmfWireReadError::NotFound { .. }) => return Ok(None),
                Err(LxmfWireReadError::OffsetOutOfRange { .. }) => {
                    return Err(ProductLxmfError::InvalidRequest);
                }
                Err(LxmfWireReadError::Backend(_)) => return Err(ProductLxmfError::Backend),
                Err(LxmfWireReadError::Binding(_) | LxmfWireReadError::Fault(_)) => {
                    return Err(ProductLxmfError::Faulted);
                }
            };
        if chunk.bytes_read() == 0 {
            return Err(ProductLxmfError::InvalidRequest);
        }
        ApiLxmfReadChunk::new(
            handle,
            chunk.offset(),
            chunk.wire_length(),
            &bytes[..chunk.bytes_read()],
        )
        .map(Some)
        .map_err(|_| ProductLxmfError::Faulted)
    }

    /// Read the durable client collection watermark and current inbox tail.
    pub(crate) fn lxmf_mailbox_status(&self) -> Result<ApiLxmfMailboxStatus, ProductLxmfError> {
        project_lxmf_mailbox_status(
            self.lxmf.as_ref().ok_or(ProductLxmfError::Unavailable)?,
            self.lxmf_mailbox
                .as_ref()
                .ok_or(ProductLxmfError::Unavailable)?,
        )
        .ok_or(ProductLxmfError::Faulted)
    }

    /// Monotonically acknowledge all committed messages through `through`.
    pub(crate) fn acknowledge_lxmf_mailbox_through(
        &mut self,
        through: ApiLxmfMessageHandle,
    ) -> Result<ApiLxmfMailboxStatus, ProductLxmfError> {
        let store = self.lxmf.as_ref().ok_or(ProductLxmfError::Unavailable)?;
        let through_model =
            MessageHandle::new(through.get()).map_err(|_| ProductLxmfError::InvalidRequest)?;
        let receipt = store
            .receipt(through_model)
            .ok_or(ProductLxmfError::InvalidRequest)?;
        let acknowledged = mailbox_cursor_from_lxmf_receipt(receipt);
        let current = self
            .lxmf_mailbox
            .take()
            .ok_or(ProductLxmfError::Unavailable)?;
        let region = PartitionNorFlash::new(
            &mut *self.flash,
            LXMF_MAILBOX_STATE_OFFSET,
            LXMF_MAILBOX_STATE_LEN,
        );
        let mut access = BoundMailboxStore::new(region, self.lxmf_mailbox_binding);
        let mounted = match acknowledge_mailbox_through(current, &mut access, acknowledged) {
            Ok(mounted) => mounted,
            Err(error) => match mount_mailbox_store(&mut access) {
                Ok(MailboxStoreMount::Mounted(mounted))
                    if mounted.acknowledged_cursor() == Some(acknowledged) =>
                {
                    mounted
                }
                Ok(MailboxStoreMount::Mounted(mounted)) => {
                    self.lxmf_mailbox = Some(mounted);
                    return Err(map_mailbox_store_error(error));
                }
                _ => return Err(map_mailbox_store_error(error)),
            },
        };
        self.lxmf_mailbox = Some(mounted);
        project_lxmf_mailbox_status(store, &mounted).ok_or(ProductLxmfError::Faulted)
    }

    /// Whether the product-owned outbound-intent quota mounted cleanly.
    pub(crate) fn lxmf_outbox_available(&self) -> bool {
        self.outbox.is_some()
    }

    /// Number of durable outbound intents recovered at boot.
    pub(crate) fn lxmf_outbox_count(&self) -> Option<usize> {
        self.outbox.as_ref().map(MountedOutbox::len)
    }

    /// Commit or replay one exact signed LXMF message before API acceptance.
    pub(crate) fn accept_lxmf_outbound(
        &mut self,
        principal: [u8; 16],
        idempotency_key: [u8; 16],
        destination: [u8; 16],
        message_id: [u8; 32],
        wire: &[u8],
    ) -> Result<OutboxAcceptance, ProductOutboxError> {
        let outbox = self
            .outbox
            .as_mut()
            .ok_or(ProductOutboxError::Unavailable)?;
        let mut region =
            PartitionNorFlash::new(&mut *self.flash, LXMF_OUTBOX_OFFSET, LXMF_OUTBOX_LEN);
        accept_outbox(
            outbox,
            &mut region,
            OutboxCandidate {
                principal,
                idempotency_key,
                destination,
                message_id,
                wire,
            },
        )
        .map_err(map_outbox_error)
    }

    /// Principal-owned durable submission metadata.
    pub(crate) fn lxmf_outbound_submission(
        &self,
        principal: [u8; 16],
        id: u64,
    ) -> Option<OutboxEntry> {
        let id = OutboxSubmissionId::new(id)?;
        self.outbox.as_ref()?.submission(principal, id)
    }

    /// First durable intent still awaiting a PRNS delivery receipt.
    pub(crate) fn next_pending_lxmf_outbound(&self) -> Option<OutboxEntry> {
        self.outbox.as_ref()?.next_pending()
    }

    /// Copy one pending intent's exact signed wire into caller storage.
    pub(crate) fn read_lxmf_outbound_wire(
        &mut self,
        entry: OutboxEntry,
        output: &mut [u8],
    ) -> Result<usize, ProductOutboxError> {
        let outbox = self
            .outbox
            .as_ref()
            .ok_or(ProductOutboxError::Unavailable)?;
        let mut region =
            PartitionNorFlash::new(&mut *self.flash, LXMF_OUTBOX_OFFSET, LXMF_OUTBOX_LEN);
        read_outbox_wire(outbox, &mut region, entry, output).map_err(map_outbox_error)
    }

    /// Record the matching ordinary PRNS delivery receipt.
    pub(crate) fn mark_lxmf_outbound_delivered(
        &mut self,
        entry: OutboxEntry,
    ) -> Result<OutboxEntry, ProductOutboxError> {
        let outbox = self
            .outbox
            .as_mut()
            .ok_or(ProductOutboxError::Unavailable)?;
        let mut region =
            PartitionNorFlash::new(&mut *self.flash, LXMF_OUTBOX_OFFSET, LXMF_OUTBOX_LEN);
        mark_delivered(outbox, &mut region, entry).map_err(map_outbox_error)
    }
}

/// Target-side flash or ESP-image failure retained for OTA diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProductOtaError {
    /// The partition table or boot-selection record was invalid.
    Partition,
    /// The requested image or write exceeded the fixed inactive slot.
    Bounds,
    /// Raw flash erase, write, or read failed.
    Flash,
    /// Programmed bytes did not match immediate readback.
    Readback,
    /// The complete staged bytes were not an ESP32-S3 application image.
    InvalidImage,
}

impl OtaBackend for ProductStore {
    type Error = ProductOtaError;

    fn prepare_inactive(&mut self, image_bytes: u32) -> Result<OtaSlot, Self::Error> {
        let slot = next_ota_slot(self.flash)?;
        let (offset, len) = ota_slot_bounds(slot);
        if image_bytes == 0 || image_bytes > len {
            return Err(ProductOtaError::Bounds);
        }
        NorFlash::erase(&mut *self.flash, offset, offset + len)
            .map_err(|_| ProductOtaError::Flash)?;
        Ok(slot)
    }

    fn write_verified(
        &mut self,
        slot: OtaSlot,
        offset: u32,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        const WRITE_SIZE: usize = <E290ProductFlash as NorFlash>::WRITE_SIZE;
        const _: () = assert!(WRITE_SIZE == 4);
        let (slot_offset, slot_len) = ota_slot_bounds(slot);
        let len = u32::try_from(data.len()).map_err(|_| ProductOtaError::Bounds)?;
        if !offset.is_multiple_of(WRITE_SIZE as u32)
            || data.is_empty()
            || offset.checked_add(len).is_none_or(|end| end > slot_len)
        {
            return Err(ProductOtaError::Bounds);
        }
        let absolute = slot_offset + offset;
        let aligned = data.len() / WRITE_SIZE * WRITE_SIZE;
        if aligned != 0 {
            NorFlash::write(&mut *self.flash, absolute, &data[..aligned])
                .map_err(|_| ProductOtaError::Flash)?;
        }
        if aligned != data.len() {
            let mut tail = [0xff_u8; WRITE_SIZE];
            tail[..data.len() - aligned].copy_from_slice(&data[aligned..]);
            NorFlash::write(&mut *self.flash, absolute + aligned as u32, &tail)
                .map_err(|_| ProductOtaError::Flash)?;
        }

        let mut verified = 0_usize;
        let mut readback = [0_u8; 256];
        while verified < data.len() {
            let read = (data.len() - verified).min(readback.len());
            ReadNorFlash::read(
                &mut *self.flash,
                absolute + verified as u32,
                &mut readback[..read],
            )
            .map_err(|_| ProductOtaError::Flash)?;
            if readback[..read] != data[verified..verified + read] {
                return Err(ProductOtaError::Readback);
            }
            verified += read;
        }
        Ok(())
    }

    fn validate_staged_image(
        &mut self,
        slot: OtaSlot,
        image_bytes: u32,
    ) -> Result<(), Self::Error> {
        validate_esp32s3_image(self.flash, slot, image_bytes)
    }

    fn activate(&mut self, slot: OtaSlot) -> Result<(), Self::Error> {
        let next = self
            .flash
            .next_ota_partition()
            .map_err(|_| ProductOtaError::Partition)?;
        if ota_slot(next)? != slot {
            return Err(ProductOtaError::Partition);
        }
        self.flash
            .activate_next_ota_partition()
            .map_err(|_| ProductOtaError::Partition)?;
        self.flash
            .set_current_ota_state(OtaImageState::New)
            .map_err(|_| ProductOtaError::Partition)
    }
}

fn next_ota_slot(flash: &mut E290ProductFlash) -> Result<OtaSlot, ProductOtaError> {
    let next = flash
        .next_ota_partition()
        .map_err(|_| ProductOtaError::Partition)?;
    ota_slot(next)
}

fn ota_slot(subtype: AppPartitionSubType) -> Result<OtaSlot, ProductOtaError> {
    match subtype {
        AppPartitionSubType::Ota0 => Ok(OtaSlot::Ota0),
        AppPartitionSubType::Ota1 => Ok(OtaSlot::Ota1),
        _ => Err(ProductOtaError::Partition),
    }
}

const fn ota_slot_bounds(slot: OtaSlot) -> (u32, u32) {
    match slot {
        OtaSlot::Ota0 => (OTA_0_OFFSET, OTA_0_LEN),
        OtaSlot::Ota1 => (OTA_1_OFFSET, OTA_1_LEN),
    }
}

fn validate_esp32s3_image(
    flash: &mut E290ProductFlash,
    slot: OtaSlot,
    image_bytes: u32,
) -> Result<(), ProductOtaError> {
    const IMAGE_HEADER_BYTES: u32 = 24;
    const SEGMENT_HEADER_BYTES: u32 = 8;
    const MAX_SEGMENTS: u8 = 16;
    const ESP32S3_CHIP_ID: u16 = 9;
    const CHECKSUM_INITIAL: u8 = 0xef;

    let (slot_offset, slot_len) = ota_slot_bounds(slot);
    if image_bytes < IMAGE_HEADER_BYTES || image_bytes > slot_len {
        return Err(ProductOtaError::Bounds);
    }
    let mut header = [0_u8; IMAGE_HEADER_BYTES as usize];
    ReadNorFlash::read(flash, slot_offset, &mut header).map_err(|_| ProductOtaError::Flash)?;
    let segment_count = header[1];
    let chip_id = u16::from_le_bytes([header[12], header[13]]);
    let hash_appended = header[23];
    if header[0] != ESP_IMAGE_MAGIC
        || segment_count == 0
        || segment_count > MAX_SEGMENTS
        || chip_id != ESP32S3_CHIP_ID
        || hash_appended != 1
    {
        return Err(ProductOtaError::InvalidImage);
    }

    let mut cursor = IMAGE_HEADER_BYTES;
    let mut checksum = CHECKSUM_INITIAL;
    let mut block = [0_u8; 256];
    for _ in 0..segment_count {
        let header_end = cursor
            .checked_add(SEGMENT_HEADER_BYTES)
            .ok_or(ProductOtaError::InvalidImage)?;
        if header_end > image_bytes {
            return Err(ProductOtaError::InvalidImage);
        }
        let mut segment = [0_u8; SEGMENT_HEADER_BYTES as usize];
        ReadNorFlash::read(flash, slot_offset + cursor, &mut segment)
            .map_err(|_| ProductOtaError::Flash)?;
        let segment_bytes = u32::from_le_bytes(segment[4..8].try_into().expect("fixed length"));
        cursor = header_end;
        let segment_end = cursor
            .checked_add(segment_bytes)
            .ok_or(ProductOtaError::InvalidImage)?;
        if segment_bytes == 0 || segment_end > image_bytes {
            return Err(ProductOtaError::InvalidImage);
        }
        while cursor < segment_end {
            let read = (segment_end - cursor).min(block.len() as u32) as usize;
            ReadNorFlash::read(flash, slot_offset + cursor, &mut block[..read])
                .map_err(|_| ProductOtaError::Flash)?;
            checksum = block[..read]
                .iter()
                .fold(checksum, |checksum, byte| checksum ^ byte);
            cursor += read as u32;
        }
    }

    let mut stored_checksum = [0_u8; 1];
    ReadNorFlash::read(flash, slot_offset + cursor, &mut stored_checksum)
        .map_err(|_| ProductOtaError::Flash)?;
    if stored_checksum[0] != checksum {
        return Err(ProductOtaError::InvalidImage);
    }
    let hash_offset = cursor
        .checked_add(1)
        .and_then(|after_checksum| after_checksum.checked_add(15))
        .map(|aligned| aligned & !15)
        .ok_or(ProductOtaError::InvalidImage)?;
    if hash_offset.checked_add(32) != Some(image_bytes) {
        return Err(ProductOtaError::InvalidImage);
    }

    let mut digest = Sha256::new();
    let mut hashed = 0_u32;
    while hashed < hash_offset {
        let read = (hash_offset - hashed).min(block.len() as u32) as usize;
        ReadNorFlash::read(flash, slot_offset + hashed, &mut block[..read])
            .map_err(|_| ProductOtaError::Flash)?;
        digest.update(&block[..read]);
        hashed += read as u32;
    }
    let actual: [u8; 32] = digest.finalize().into();
    let mut embedded = [0_u8; 32];
    ReadNorFlash::read(flash, slot_offset + hash_offset, &mut embedded)
        .map_err(|_| ProductOtaError::Flash)?;
    if embedded != actual {
        return Err(ProductOtaError::InvalidImage);
    }
    Ok(())
}

fn mount_or_provision_lxmf_mailbox(
    flash: &mut E290ProductFlash,
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

fn map_mailbox_store_error(error: MailboxStoreError<ProductRegionError>) -> ProductLxmfError {
    match error {
        MailboxStoreError::Backend(_) => ProductLxmfError::Backend,
        MailboxStoreError::Fault(_) => ProductLxmfError::Faulted,
    }
}

fn map_outbox_error(error: OutboxError<ProductRegionError>) -> ProductOutboxError {
    match error {
        OutboxError::Backend(_) => ProductOutboxError::Backend,
        OutboxError::InvalidBinding
        | OutboxError::Corrupt { .. }
        | OutboxError::Verification { .. } => ProductOutboxError::Faulted,
        OutboxError::InvalidCandidate => ProductOutboxError::InvalidRequest,
    }
}

const fn flash_device_id(mac: [u8; 6]) -> [u8; 16] {
    [
        b'e', b'2', b'9', b'0', b'-', b'f', b'l', b'a', b's', b'h', mac[0], mac[1], mac[2], mac[3],
        mac[4], mac[5],
    ]
}

#[derive(Clone, Copy)]
struct ExpectedPartition {
    name: &'static str,
    label: [u8; 16],
    kind: u8,
    subtype: u8,
    offset: u32,
    len: u32,
}

const EXPECTED_PARTITIONS: [ExpectedPartition; 5] = [
    ExpectedPartition {
        name: "ota_0",
        label: OTA_0_LABEL_BYTES,
        kind: APP_PARTITION_TYPE,
        subtype: OTA_0_SUBTYPE,
        offset: OTA_0_OFFSET,
        len: OTA_0_LEN,
    },
    ExpectedPartition {
        name: "ota_1",
        label: OTA_1_LABEL_BYTES,
        kind: APP_PARTITION_TYPE,
        subtype: OTA_1_SUBTYPE,
        offset: OTA_1_OFFSET,
        len: OTA_1_LEN,
    },
    ExpectedPartition {
        name: "otadata",
        label: OTA_DATA_LABEL_BYTES,
        kind: DATA_PARTITION_TYPE,
        subtype: OTA_DATA_SUBTYPE,
        offset: OTA_DATA_OFFSET,
        len: OTA_DATA_LEN,
    },
    ExpectedPartition {
        name: "product_state",
        label: PRODUCT_STATE_LABEL_BYTES,
        kind: DATA_PARTITION_TYPE,
        subtype: UNDEFINED_DATA_SUBTYPE,
        offset: PRODUCT_STATE_OFFSET,
        len: PRODUCT_STATE_LEN,
    },
    ExpectedPartition {
        name: "prns_state",
        label: PRNS_STATE_LABEL_BYTES,
        kind: DATA_PARTITION_TYPE,
        subtype: UNDEFINED_DATA_SUBTYPE,
        offset: PRNS_STATE_OFFSET,
        len: PRNS_STATE_LEN,
    },
];

fn validate_partition_table(flash: &mut E290ProductFlash) -> Result<(), ProductStoreBootError> {
    let mut bytes = [0_u8; PARTITION_TABLE_MAX_LEN];
    let table = flash
        .read_partition_table(&mut bytes)
        .map_err(|_| ProductStoreBootError::PartitionTable)?;

    for expected in EXPECTED_PARTITIONS {
        let mut count = 0_u8;
        let mut valid = false;
        for entry in table.iter() {
            if entry.label() != expected.label {
                continue;
            }
            count = count.saturating_add(1);
            valid = entry.raw_type() == expected.kind
                && entry.raw_subtype() == expected.subtype
                && entry.offset() == expected.offset
                && entry.len() == expected.len
                && !entry.is_read_only()
                && !entry.is_encrypted()
                && entry.flags() == 0;
        }
        if count != 1 {
            return Err(ProductStoreBootError::PartitionCardinality(expected.name));
        }
        if !valid {
            return Err(ProductStoreBootError::PartitionShape(expected.name));
        }
    }

    for entry in table.iter() {
        let end = entry
            .offset()
            .checked_add(entry.len())
            .ok_or(ProductStoreBootError::PartitionTable)?;
        for expected in &EXPECTED_PARTITIONS[3..] {
            let expected_end = expected.offset + expected.len;
            let named = entry.label() == expected.label;
            if !named && entry.offset() < expected_end && expected.offset < end {
                return Err(ProductStoreBootError::PartitionOverlap(expected.name));
            }
        }
    }
    Ok(())
}

const _: () = assert!(
    NODE_IDENTITY_LEN as usize == reticulum_e290_firmware::product_identity::PARTITION_SIZE
);
const _: () =
    assert!(NETWORK_CONFIG_LEN as usize == reticulum_network_config_store::PARTITION_SIZE);
const _: () = assert!((LXMF_STORE_LEN as usize).is_multiple_of(reticulum_lxmf_store::EXTENT_SIZE));
const _: () =
    assert!(LXMF_MAILBOX_STATE_LEN as usize == reticulum_lxmf_mailbox_store::PARTITION_SIZE);
const _: () =
    assert!(LXMF_OUTBOX_LEN as usize == reticulum_e290_firmware::product_outbox::OUTBOX_STORE_SIZE);
