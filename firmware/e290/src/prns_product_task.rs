//! Sole product-application owner above the PRNS node.
//!
//! PRNS owns Reticulum protocol state and proof timing. This task owns only
//! copied management requests and copied LXMF carriers, and serializes their
//! access to the product application store. It deliberately has no router,
//! interface, Link, receipt, or proof state of its own.

use embassy_futures::select::{Either4, Either5, select4, select5};
#[cfg(feature = "display")]
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_bootloader_esp_idf::ota::OtaImageState;
use esp_hal::{gpio::Input, system::software_reset};
use log::{error, info, warn};
use personal_rns::bluetooth_auto::BluetoothAutoStatus;
use personal_rns::engine::{
    AllowRequesterFailure, AnnounceAppData, AnnounceNow, AnnounceTarget, CommandId,
    InterfaceCounts, MAX_SEND_SINGLE_PACKET_PLAINTEXT_LEN, PathRequestId, PrnsCommand, RequestPath,
    SendSinglePacket, SendSinglePacketFailure, SendSinglePacketPayload, SendSinglePacketRejection,
    SetResourceStrategy, SetResourceStrategyFailure,
};
use personal_rns::identity::{
    IDENTITY_SECRET_KEY_LEN, IdentitySigner, Zeroizing, in_memory::InMemoryNodeIdentity,
};
use personal_rns::interfaces::{ConnectionState, InterfaceId, InterfaceMode, InterfaceStatus};
use personal_rns::manifold::embassy::EmbassyInterfaceStatus;
use personal_rns::routing::links::resources::ResourceStrategy;
use personal_rns::routing::routes::NextHop;
use personal_rns::runtime::PrnsNodeApi;
use personal_rns::{InspectedRoute, InspectionError};
#[cfg(feature = "display")]
use reticulum_appliance_display_model::DisplayLabel;
#[cfg(feature = "gateway")]
use reticulum_device_api::WifiNetworkProfileId;
use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, CapabilityAvailability,
    DestinationHash as ApiDestinationHash, DeviceRequest, DeviceResponse,
    DiagnosticInterfaceFailureReason, DiagnosticInterfaceMode, DiagnosticInterfaceRecord,
    DiagnosticInterfaceState, IdentityHash as ApiIdentityHash, IdentitySummary,
    LxmfBasicSendAccepted, MAX_DIAGNOSTIC_INTERFACES, MAX_LXMF_PEER_APP_DATA_BYTES,
    MAX_MESSAGE_BYTES, MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES, NetworkConfigSnapshot,
    NetworkRuntimeStatus, NodeDiagnosticsSnapshot, OtaPhase, ResponseEnvelope,
    ReticulumInterfaceId, ReticulumTcpPeerState, RouteDiagnosticEntry, RouteDiagnosticNextHop,
    RouteDiagnosticsPage, RouteDiagnosticsRequest, SubmissionId, SubmissionState, SubmissionStatus,
    WifiStationState, decode_request, encode_response,
};
#[cfg(feature = "display")]
use reticulum_e290_firmware::display_handoff::{DisplayHomeTelemetry, DisplayTelemetryPublisher};
use reticulum_e290_firmware::lxmf_delivery::{
    LxmfOutboundRetryState, LxmfRetentionAction, carrier_retention_action, wire_limits,
};
use reticulum_e290_firmware::management_authorization::{EnrollmentWindow, ManagementIdentity};
use reticulum_e290_firmware::ota::{
    OTA_REBOOT_REQUEST_VALUE, OTA_STATUS_REQUEST_VALUE, OTA_STATUS_RESPONSE_MAX_BYTES,
    OtaCoordinator, OtaCoordinatorError, decode_next_request, decode_start_request,
    encode_status_response,
};
use reticulum_e290_firmware::prns_events::{OwnedLxmfSingleDelivery, OwnedOtaResource};
use reticulum_e290_firmware::prns_peer_discovery;
use reticulum_e290_firmware::prns_requests::{
    LxmfOutboundSettlement, MANAGEMENT_AUTHORIZED_PATHS, MANAGEMENT_ENROLLMENT_REQUEST_VALUE,
    MANAGEMENT_ENROLLMENT_SUCCESS_RESPONSE, ManagementDispatchError, ManagementRequest,
    ManagementRequestKind, ManagementResponse, allow_management_requester_for_path,
    dispatch_public_management_payload,
};
use reticulum_lxmf_durable_ingress::DurableCarrierOutcome;
use reticulum_lxmf_wire::{
    BasicLxmfSigner, MAX_BASIC_LXMF_WIRE_BYTES, MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES,
    SidebandLocationTelemetry, compose_basic_opportunistic_lxmf, encode_sideband_location_fields,
};

use super::E290PrnsPsramAlloc;
use crate::prns_target::{
    E290PrnsHandle, ManagementAuthorizationSettlementReceiver,
    OtaResourceStrategySettlementReceiver, StartedPrnsTarget, interface_counts,
};
use crate::product_store::{
    ProductApplianceSettingsError, ProductLxmfError, ProductNetworkConfigError, ProductOutboxError,
    ProductStore,
};
use reticulum_e290_firmware::product_outbox::{OutboxAcceptance, OutboxEntry};
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_station_profile::{WifiStationPhase, WifiStationStatusCell};

const APPLICATION_RETRY_DELAY: Duration = Duration::from_secs(1);
const RETAINED_FAULT_RECHECK_DELAY: Duration = Duration::from_secs(30);
const BUTTON_SAMPLE_MILLIS: u64 = 25;
const AUTHORIZATION_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(2);
const OTA_RESOURCE_STRATEGY_TIMEOUT: Duration = Duration::from_secs(2);
const OTA_BOOT_HEALTH_WINDOW: Duration = Duration::from_secs(30);
const SERVICE_ANNOUNCE_INTERVAL_MILLIS: u64 = 30 * 60 * 1_000;
const BLUETOOTH_ANNOUNCE_ATTACH_DELAY_MILLIS: u64 = 250;
const BLUETOOTH_ANNOUNCE_RETRY_DELAY_MILLIS: u64 = 250;
const OUTBOUND_RETRY_DELAY: Duration = Duration::from_secs(10);
const OUTBOUND_PATH_REQUEST_WAIT: Duration = Duration::from_secs(7);

#[cfg(feature = "display")]
fn publish_display_home_telemetry(
    storage: &ProductStore,
    publisher: &mut Option<DisplayTelemetryPublisher<CriticalSectionRawMutex>>,
) {
    let Some(publisher) = publisher.as_mut() else {
        return;
    };
    let appliance_label = storage
        .appliance_label()
        .and_then(|label| DisplayLabel::new(label.as_str()).ok());
    let uncollected_messages = storage
        .lxmf_mailbox_status()
        .map_or(0, |status| status.uncollected_count());
    publisher.publish_latest(DisplayHomeTelemetry::new(
        appliance_label,
        uncollected_messages,
    ));
}

struct ProductLxmfSigner(InMemoryNodeIdentity);

impl BasicLxmfSigner for ProductLxmfSigner {
    fn sign_lxmf(&self, input: &[u8]) -> [u8; 64] {
        self.0.sign(input).0
    }
}

#[allow(
    dead_code,
    reason = "the PRNS rejection payload is retained for target diagnostics"
)]
#[derive(Debug)]
enum AuthorizationApplyError {
    CommandPressure,
    Timeout,
    Rejected(AllowRequesterFailure),
}

#[allow(
    dead_code,
    reason = "the PRNS rejection payload is retained for target diagnostics"
)]
#[derive(Debug)]
enum OtaResourceStrategyError {
    CommandPressure,
    Timeout,
    Rejected(SetResourceStrategyFailure),
}

fn api_error(code: ApiErrorCode, operation: u16) -> DeviceResponse {
    DeviceResponse::Error(ApiErrorResponse {
        code,
        operation: Some(operation),
    })
}

fn lxmf_error(error: ProductLxmfError, operation: u16) -> DeviceResponse {
    let code = match error {
        ProductLxmfError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        ProductLxmfError::InvalidRequest => ApiErrorCode::InvalidRequest,
        ProductLxmfError::Backend | ProductLxmfError::Faulted => ApiErrorCode::Internal,
    };
    api_error(code, operation)
}

fn outbox_error(error: ProductOutboxError, operation: u16) -> DeviceResponse {
    let code = match error {
        ProductOutboxError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        ProductOutboxError::InvalidRequest => ApiErrorCode::InvalidRequest,
        ProductOutboxError::Backend | ProductOutboxError::Faulted => ApiErrorCode::Internal,
    };
    api_error(code, operation)
}

fn network_config_error(error: ProductNetworkConfigError, operation: u16) -> DeviceResponse {
    let code = match error {
        ProductNetworkConfigError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        ProductNetworkConfigError::InvalidRequest => ApiErrorCode::InvalidRequest,
        ProductNetworkConfigError::Backend
        | ProductNetworkConfigError::Faulted
        | ProductNetworkConfigError::Invariant => ApiErrorCode::Internal,
    };
    api_error(code, operation)
}

fn appliance_settings_error(
    error: ProductApplianceSettingsError,
    operation: u16,
) -> DeviceResponse {
    let code = match error {
        ProductApplianceSettingsError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        ProductApplianceSettingsError::InvalidRequest => ApiErrorCode::InvalidRequest,
        ProductApplianceSettingsError::Backend | ProductApplianceSettingsError::Faulted => {
            ApiErrorCode::Internal
        }
    };
    api_error(code, operation)
}

fn product_capabilities(
    storage: &ProductStore,
    lxmf: Option<personal_rns::wire::DestinationHash>,
) -> reticulum_device_api::CapabilitySnapshot {
    reticulum_device_api::CapabilitySnapshot::for_dispatch_with_lxmf_basic_send_and_peer_discovery(
        false,
        if lxmf.is_some() && storage.lxmf_available() {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        },
        if lxmf.is_some() && storage.lxmf_outbox_available() {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        },
        if lxmf.is_some() {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        },
        MAX_LXMF_PEER_APP_DATA_BYTES as u16,
    )
    .with_dispatch_network_config(if storage.network_config_available() {
        CapabilityAvailability::Available
    } else {
        CapabilityAvailability::Unavailable
    })
    .with_dispatch_route_diagnostics(CapabilityAvailability::Available)
}

fn configured_tcp_peer_enabled(snapshot: NetworkConfigSnapshot) -> bool {
    snapshot.tcp_peer().is_some_and(|peer| peer.enabled())
        || snapshot.tcp_host_peer().is_some_and(|peer| peer.enabled())
}

fn project_tcp_state(status: &EmbassyInterfaceStatus) -> ReticulumTcpPeerState {
    match status.connection() {
        ConnectionState::Initializing => ReticulumTcpPeerState::Connecting,
        ConnectionState::Connected | ConnectionState::Degraded => ReticulumTcpPeerState::Connected,
        ConnectionState::Reconnecting | ConnectionState::Disconnected => {
            ReticulumTcpPeerState::Backoff
        }
        ConnectionState::Failed | ConnectionState::Unknown => ReticulumTcpPeerState::Faulted,
        ConnectionState::Disabled => ReticulumTcpPeerState::Disabled,
    }
}

fn network_runtime_status(
    storage: &ProductStore,
    tcp_status: Option<&EmbassyInterfaceStatus>,
    #[cfg(feature = "gateway")] wifi_station_status: &WifiStationStatusCell,
) -> Result<NetworkRuntimeStatus, ProductNetworkConfigError> {
    let snapshot = storage.network_config_snapshot()?;
    let configured_revision = snapshot.revision;
    let boot_applied_revision = storage.network_applied_revision();
    let desired_tcp_peer_enabled = configured_tcp_peer_enabled(snapshot);

    #[cfg(feature = "gateway")]
    {
        let status = wifi_station_status.snapshot();
        let applied_revision = boot_applied_revision.min(status.applied_revision);
        let wifi_state = match status.phase {
            WifiStationPhase::Disabled => WifiStationState::Disabled,
            WifiStationPhase::Associating | WifiStationPhase::Dhcp => WifiStationState::Connecting,
            WifiStationPhase::Online => WifiStationState::Connected,
            WifiStationPhase::Backoff | WifiStationPhase::Faulted => WifiStationState::Disconnected,
        };
        let active_wifi_profile = status
            .profile_id
            .map(WifiNetworkProfileId::new)
            .transpose()
            .map_err(|_| ProductNetworkConfigError::Invariant)?;
        let tcp_peer_state = match tcp_status {
            Some(_) if wifi_state != WifiStationState::Connected => {
                ReticulumTcpPeerState::WaitingForNetwork
            }
            Some(status) => project_tcp_state(status),
            None if configured_revision == applied_revision && desired_tcp_peer_enabled => {
                ReticulumTcpPeerState::Faulted
            }
            None => ReticulumTcpPeerState::Disabled,
        };
        NetworkRuntimeStatus::new(
            configured_revision,
            applied_revision,
            wifi_state,
            active_wifi_profile,
            status.connected_ssid(),
            status.ipv4,
            status.rssi_dbm.map(i16::from),
            tcp_peer_state,
        )
        .map_err(|_| ProductNetworkConfigError::Invariant)
    }

    #[cfg(not(feature = "gateway"))]
    {
        let wifi_state = if snapshot.wifi_transport_enabled()
            && snapshot
                .wifi_profiles()
                .iter()
                .flatten()
                .any(|profile| profile.enabled())
        {
            WifiStationState::Disconnected
        } else {
            WifiStationState::Disabled
        };
        let tcp_peer_state = match tcp_status {
            Some(status) => project_tcp_state(status),
            None if desired_tcp_peer_enabled => ReticulumTcpPeerState::WaitingForNetwork,
            None => ReticulumTcpPeerState::Disabled,
        };
        NetworkRuntimeStatus::new(
            configured_revision,
            boot_applied_revision,
            wifi_state,
            None,
            None,
            None,
            None,
            tcp_peer_state,
        )
        .map_err(|_| ProductNetworkConfigError::Invariant)
    }
}

fn diagnostic_interface_state(state: ConnectionState) -> DiagnosticInterfaceState {
    match state {
        ConnectionState::Initializing => DiagnosticInterfaceState::Initializing,
        ConnectionState::Connected => DiagnosticInterfaceState::Connected,
        ConnectionState::Degraded => DiagnosticInterfaceState::Degraded,
        ConnectionState::Reconnecting => DiagnosticInterfaceState::Reconnecting,
        ConnectionState::Failed => DiagnosticInterfaceState::Failed,
        ConnectionState::Disconnected => DiagnosticInterfaceState::Disconnected,
        ConnectionState::Disabled => DiagnosticInterfaceState::Disabled,
        ConnectionState::Unknown => DiagnosticInterfaceState::Unknown,
    }
}

fn diagnostic_interface_mode(mode: InterfaceMode) -> DiagnosticInterfaceMode {
    match mode {
        InterfaceMode::Full => DiagnosticInterfaceMode::Full,
        InterfaceMode::PointToPoint => DiagnosticInterfaceMode::PointToPoint,
        InterfaceMode::AccessPoint => DiagnosticInterfaceMode::AccessPoint,
        InterfaceMode::Roaming => DiagnosticInterfaceMode::Roaming,
        InterfaceMode::Boundary => DiagnosticInterfaceMode::Boundary,
        InterfaceMode::Gateway => DiagnosticInterfaceMode::Gateway,
        InterfaceMode::Internal => DiagnosticInterfaceMode::Internal,
    }
}

fn diagnostic_interface(
    status: &impl InterfaceStatus,
    mode: InterfaceMode,
    counts: InterfaceCounts,
    supervisor: Option<InterfaceId>,
) -> DiagnosticInterfaceRecord {
    DiagnosticInterfaceRecord::new(
        ReticulumInterfaceId::new(*status.id().as_bytes()),
        diagnostic_interface_mode(mode),
        diagnostic_interface_state(status.connection()),
        status
            .failure_reason()
            .and_then(DiagnosticInterfaceFailureReason::try_from_str),
        status.rx_bytes(),
        status.tx_bytes(),
        counts.destinations,
        counts.links,
        counts.transported_links,
        supervisor.map(|id| ReticulumInterfaceId::new(*id.as_bytes())),
    )
}

fn add_interface(
    slots: &mut [Option<DiagnosticInterfaceRecord>; MAX_DIAGNOSTIC_INTERFACES],
    record: DiagnosticInterfaceRecord,
) {
    if let Some(slot) = slots.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(record);
    }
}

async fn node_diagnostics<const MEMBERS: usize>(
    handle: &E290PrnsHandle,
    now_millis: u64,
    lora_status: &EmbassyInterfaceStatus,
    tcp_status: Option<&EmbassyInterfaceStatus>,
    bluetooth_status: Option<BluetoothAutoStatus<MEMBERS>>,
) -> Result<NodeDiagnosticsSnapshot, InspectionError> {
    let route_count = handle.route_count().await?;
    let link_count = handle.link_count().await?;
    let mut interfaces = [None; MAX_DIAGNOSTIC_INTERFACES];
    add_interface(
        &mut interfaces,
        diagnostic_interface(
            lora_status,
            InterfaceMode::Internal,
            interface_counts(lora_status.id()),
            None,
        ),
    );
    if let Some(tcp_status) = tcp_status {
        add_interface(
            &mut interfaces,
            diagnostic_interface(
                tcp_status,
                InterfaceMode::Boundary,
                interface_counts(tcp_status.id()),
                None,
            ),
        );
    }
    if let Some(bluetooth_status) = bluetooth_status {
        let counts = bluetooth_status
            .members()
            .map(|member| interface_counts(member.id()))
            .fold(InterfaceCounts::default(), |aggregate, counts| {
                InterfaceCounts {
                    destinations: aggregate.destinations.saturating_add(counts.destinations),
                    links: aggregate.links.saturating_add(counts.links),
                    transported_links: aggregate
                        .transported_links
                        .saturating_add(counts.transported_links),
                }
            });
        add_interface(
            &mut interfaces,
            diagnostic_interface(&bluetooth_status, InterfaceMode::Full, counts, None),
        );
    }
    Ok(NodeDiagnosticsSnapshot::new(
        now_millis,
        interfaces,
        None,
        route_count,
        link_count,
    ))
}

fn route_diagnostic_entry(route: InspectedRoute) -> RouteDiagnosticEntry {
    let observed_at = route.observed_at.0;
    let route = route.route;
    let next_hop = match route.via {
        NextHop::Direct => RouteDiagnosticNextHop::Direct,
        NextHop::Via(identity) => {
            RouteDiagnosticNextHop::Via(ApiIdentityHash::new(*identity.as_bytes()))
        }
    };
    RouteDiagnosticEntry::new(
        ApiDestinationHash(*route.destination.as_bytes()),
        next_hop,
        route.hops,
        ReticulumInterfaceId::new(*route.interface.as_bytes()),
        observed_at.saturating_sub(route.learned_at.0),
        observed_at.saturating_sub(route.last_route_activity_at.0),
        route.expires_at.0.saturating_sub(observed_at),
    )
}

async fn route_diagnostics_page(
    handle: &E290PrnsHandle,
    request: RouteDiagnosticsRequest,
) -> Result<RouteDiagnosticsPage, InspectionError> {
    let mut entries = [None; MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES];
    let mut after = request
        .after()
        .map(|destination| personal_rns::wire::DestinationHash::new(destination.0));
    let mut populated = 0;
    for slot in &mut entries {
        let Some(route) = handle.next_route_after(after).await? else {
            break;
        };
        after = Some(route.route.destination);
        *slot = Some(route_diagnostic_entry(route));
        populated += 1;
    }
    let next_cursor = if populated == MAX_ROUTE_DIAGNOSTIC_PAGE_ENTRIES
        && handle.next_route_after(after).await?.is_some()
    {
        after.map(|destination| ApiDestinationHash(*destination.as_bytes()))
    } else {
        None
    };
    Ok(RouteDiagnosticsPage::new(entries, next_cursor)
        .expect("PRNS next-route inspection is strictly lexicographic"))
}

fn inspection_error(error: InspectionError, operation: u16) -> DeviceResponse {
    api_error(
        match error {
            InspectionError::Busy => ApiErrorCode::RetryLater,
            InspectionError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        },
        operation,
    )
}

async fn dispatch_authorized_product_payload<const MEMBERS: usize>(
    handle: &E290PrnsHandle,
    data: &[u8],
    management: personal_rns::wire::DestinationHash,
    lxmf: Option<personal_rns::wire::DestinationHash>,
    storage: &mut ProductStore,
    principal: Option<personal_rns::identity::IdentityHash>,
    signer: &ProductLxmfSigner,
    now_millis: u64,
    lora_status: &EmbassyInterfaceStatus,
    tcp_status: Option<&EmbassyInterfaceStatus>,
    bluetooth_status: Option<BluetoothAutoStatus<MEMBERS>>,
    #[cfg(feature = "gateway")] wifi_station_status: &WifiStationStatusCell,
) -> Result<ManagementResponse, ManagementDispatchError> {
    let request = decode_request(data).map_err(|_| ManagementDispatchError::MalformedRequest)?;
    let operation = request.request.operation();
    let response = match request.request {
        DeviceRequest::SystemCapabilities => {
            DeviceResponse::SystemCapabilities(product_capabilities(storage, lxmf))
        }
        DeviceRequest::IdentitySummary => {
            let management = ApiDestinationHash(*management.as_bytes());
            let summary = match lxmf {
                Some(lxmf) => IdentitySummary::with_lxmf_delivery_destination(
                    management,
                    ApiDestinationHash(*lxmf.as_bytes()),
                ),
                None => IdentitySummary::new(management),
            };
            DeviceResponse::IdentitySummary(summary)
        }
        DeviceRequest::ApplianceLabelGet => {
            if principal.is_none() {
                api_error(ApiErrorCode::AuthenticationRequired, operation)
            } else {
                match storage.appliance_label_snapshot() {
                    Ok(snapshot) => DeviceResponse::ApplianceLabel(snapshot),
                    Err(error) => appliance_settings_error(error, operation),
                }
            }
        }
        DeviceRequest::ApplianceLabelMutate(label_request) => {
            if principal.is_none() {
                api_error(ApiErrorCode::AuthenticationRequired, operation)
            } else {
                match storage.mutate_appliance_label(label_request) {
                    Ok(outcome) => DeviceResponse::ApplianceLabelMutation(outcome),
                    Err(error) => appliance_settings_error(error, operation),
                }
            }
        }
        DeviceRequest::NetworkConfigGet => {
            if principal.is_none() {
                api_error(ApiErrorCode::AuthenticationRequired, operation)
            } else {
                match storage.network_config_snapshot() {
                    Ok(snapshot) => DeviceResponse::NetworkConfig(snapshot),
                    Err(error) => network_config_error(error, operation),
                }
            }
        }
        DeviceRequest::NetworkConfigMutate(config_request) => {
            if principal.is_none() {
                api_error(ApiErrorCode::AuthenticationRequired, operation)
            } else {
                match storage.mutate_network_config(config_request) {
                    Ok(outcome) => DeviceResponse::NetworkConfigMutation(outcome),
                    Err(error) => network_config_error(error, operation),
                }
            }
        }
        DeviceRequest::NetworkStatus => {
            if principal.is_none() {
                api_error(ApiErrorCode::AuthenticationRequired, operation)
            } else {
                match network_runtime_status(
                    storage,
                    tcp_status,
                    #[cfg(feature = "gateway")]
                    wifi_station_status,
                ) {
                    Ok(status) => DeviceResponse::NetworkStatus(status),
                    Err(error) => network_config_error(error, operation),
                }
            }
        }
        DeviceRequest::NodeDiagnostics => {
            if principal.is_none() {
                api_error(ApiErrorCode::AuthenticationRequired, operation)
            } else {
                match node_diagnostics(
                    handle,
                    now_millis,
                    lora_status,
                    tcp_status,
                    bluetooth_status,
                )
                .await
                {
                    Ok(snapshot) => DeviceResponse::NodeDiagnostics(snapshot),
                    Err(error) => inspection_error(error, operation),
                }
            }
        }
        DeviceRequest::RouteDiagnosticsPage(request) => {
            if principal.is_none() {
                api_error(ApiErrorCode::AuthenticationRequired, operation)
            } else {
                match route_diagnostics_page(handle, request).await {
                    Ok(page) => DeviceResponse::RouteDiagnosticsPage(page),
                    Err(error) => inspection_error(error, operation),
                }
            }
        }
        DeviceRequest::LxmfPeerNext { after } => {
            if principal.is_none() {
                api_error(ApiErrorCode::AuthenticationRequired, operation)
            } else {
                DeviceResponse::LxmfPeerNext(prns_peer_discovery::page(after, now_millis))
            }
        }
        DeviceRequest::LxmfNext { after } => match storage.lxmf_next(after) {
            Ok(Some(summary)) => DeviceResponse::LxmfNext(summary),
            Ok(None) => api_error(ApiErrorCode::NotFound, operation),
            Err(error) => lxmf_error(error, operation),
        },
        DeviceRequest::LxmfRead {
            handle,
            offset,
            max_bytes,
        } => match storage.lxmf_read(handle, offset, max_bytes) {
            Ok(Some(chunk)) => DeviceResponse::LxmfRead(chunk),
            Ok(None) => api_error(ApiErrorCode::NotFound, operation),
            Err(error) => lxmf_error(error, operation),
        },
        DeviceRequest::LxmfMailboxStatus => match storage.lxmf_mailbox_status() {
            Ok(status) => DeviceResponse::LxmfMailboxStatus(status),
            Err(error) => lxmf_error(error, operation),
        },
        DeviceRequest::LxmfMailboxAcknowledge { through } => {
            match storage.acknowledge_lxmf_mailbox_through(through) {
                Ok(status) => DeviceResponse::LxmfMailboxAcknowledged(status),
                Err(error) => lxmf_error(error, operation),
            }
        }
        DeviceRequest::LxmfBasicSend {
            destination,
            timestamp_unix_ms,
            title,
            content,
            location,
            idempotency_key,
        } => {
            let Some(principal) = principal else {
                return encode_single_response(
                    request.request_id,
                    api_error(ApiErrorCode::AuthenticationRequired, operation),
                );
            };
            let Some(source) = lxmf else {
                return encode_single_response(
                    request.request_id,
                    api_error(ApiErrorCode::CapabilityUnavailable, operation),
                );
            };
            let mut fields = [0_u8; MAX_ENCODED_SIDEBAND_LOCATION_FIELDS_BYTES];
            let fields = match location {
                Some(location) => {
                    let telemetry = SidebandLocationTelemetry::new(
                        location.latitude_e6(),
                        location.longitude_e6(),
                        location.altitude_cm(),
                        location.speed_cm_per_second(),
                        location.bearing_centidegrees(),
                        location.accuracy_cm(),
                        location.updated_at_unix_seconds(),
                    );
                    match encode_sideband_location_fields(telemetry, &mut fields) {
                        Ok(written) => Some(&fields[..written]),
                        Err(_) => {
                            return encode_single_response(
                                request.request_id,
                                api_error(ApiErrorCode::InvalidRequest, operation),
                            );
                        }
                    }
                }
                None => None,
            };
            let mut wire = [0_u8; MAX_BASIC_LXMF_WIRE_BYTES];
            let prepared = match compose_basic_opportunistic_lxmf(
                destination.0,
                *source.as_bytes(),
                timestamp_unix_ms,
                title,
                content,
                fields,
                MAX_SEND_SINGLE_PACKET_PLAINTEXT_LEN,
                signer,
                &mut wire,
            ) {
                Ok(prepared) => prepared,
                Err(_) => {
                    return encode_single_response(
                        request.request_id,
                        api_error(ApiErrorCode::InvalidRequest, operation),
                    );
                }
            };
            match storage.accept_lxmf_outbound(
                *principal.as_bytes(),
                idempotency_key.0,
                destination.0,
                prepared.message_id(),
                &wire[..usize::from(prepared.wire_len())],
            ) {
                Ok(OutboxAcceptance::Accepted(entry) | OutboxAcceptance::Replay(entry)) => {
                    DeviceResponse::LxmfBasicSendAccepted(LxmfBasicSendAccepted::new(
                        SubmissionId(entry.submission_id().get()),
                        entry.message_id(),
                    ))
                }
                Ok(OutboxAcceptance::IdempotencyConflict) => {
                    api_error(ApiErrorCode::IdempotencyConflict, operation)
                }
                Ok(OutboxAcceptance::CapacityExhausted) => {
                    api_error(ApiErrorCode::CapacityExhausted, operation)
                }
                Err(error) => outbox_error(error, operation),
            }
        }
        DeviceRequest::SubmissionStatus { id } => {
            let Some(principal) = principal else {
                return encode_single_response(
                    request.request_id,
                    api_error(ApiErrorCode::AuthenticationRequired, operation),
                );
            };
            match storage.lxmf_outbound_submission(*principal.as_bytes(), id.0) {
                Some(entry) => DeviceResponse::SubmissionStatus(SubmissionStatus {
                    id,
                    state: if entry.delivered() {
                        SubmissionState::ApplicationDelivered
                    } else {
                        SubmissionState::Queued
                    },
                }),
                None => api_error(ApiErrorCode::NotFound, operation),
            }
        }
        _ => api_error(ApiErrorCode::CapabilityUnavailable, operation),
    };
    encode_single_response(request.request_id, response)
}

fn encode_single_response(
    request_id: reticulum_device_api::RequestId,
    response: DeviceResponse,
) -> Result<ManagementResponse, ManagementDispatchError> {
    let envelope = ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id,
        response,
    };
    let mut encoded = [0_u8; MAX_MESSAGE_BYTES];
    let written = encode_response(&envelope, &mut encoded)
        .map_err(|_| ManagementDispatchError::ResponseEncoding)?;
    ManagementResponse::from_slice(&encoded[..written])
        .map_err(|_| ManagementDispatchError::ResponseCapacity)
}

async fn apply_management_authorization<H: PrnsNodeApi>(
    handle: &H,
    settlements: &mut ManagementAuthorizationSettlementReceiver,
    command: PrnsCommand,
) -> Result<(), AuthorizationApplyError> {
    let id = handle
        .issue(command)
        .ok_or(AuthorizationApplyError::CommandPressure)?;
    let settled = with_timeout(AUTHORIZATION_SETTLEMENT_TIMEOUT, async {
        loop {
            let settlement = settlements.receive().await;
            if settlement.id == id {
                return settlement.result;
            }
            warn!(
                "e290-prns management-authorization status=STALE-SETTLEMENT expected={id:?} actual={:?}",
                settlement.id,
            );
        }
    })
    .await
    .map_err(|_| AuthorizationApplyError::Timeout)?;
    settled.map_err(AuthorizationApplyError::Rejected)
}

async fn apply_management_identity<H: PrnsNodeApi>(
    handle: &H,
    settlements: &mut ManagementAuthorizationSettlementReceiver,
    management: personal_rns::wire::DestinationHash,
    identity: personal_rns::identity::IdentityHash,
) -> Result<(), AuthorizationApplyError> {
    for path in MANAGEMENT_AUTHORIZED_PATHS {
        apply_management_authorization(
            handle,
            settlements,
            allow_management_requester_for_path(management, path, identity),
        )
        .await?;
    }
    Ok(())
}

async fn set_ota_resource_strategy<H: PrnsNodeApi>(
    handle: &H,
    settlements: &mut OtaResourceStrategySettlementReceiver,
    link_id: personal_rns::routing::links::LinkId,
    strategy: ResourceStrategy,
) -> Result<(), OtaResourceStrategyError> {
    let id = handle
        .issue(PrnsCommand::SetResourceStrategy(SetResourceStrategy {
            link_id,
            strategy,
        }))
        .ok_or(OtaResourceStrategyError::CommandPressure)?;
    let settled = with_timeout(OTA_RESOURCE_STRATEGY_TIMEOUT, async {
        loop {
            let settlement = settlements.receive().await;
            if settlement.id == id {
                return settlement.result;
            }
            warn!(
                "e290-prns ota status=STALE-RESOURCE-STRATEGY-SETTLEMENT expected={id:?} actual={:?}",
                settlement.id,
            );
        }
    })
    .await
    .map_err(|_| OtaResourceStrategyError::Timeout)?;
    settled.map_err(OtaResourceStrategyError::Rejected)
}

fn respond_ota_status<H: PrnsNodeApi>(
    handle: &H,
    request: &ManagementRequest,
    coordinator: &OtaCoordinator,
) -> bool {
    let mut response = [0_u8; OTA_STATUS_RESPONSE_MAX_BYTES];
    let Ok(written) = encode_status_response(coordinator.status(), &mut response) else {
        return false;
    };
    handle.respond_packed(request.respond_token(), &response[..written])
}

async fn answer_ota_control<H: PrnsNodeApi>(
    handle: &H,
    settlements: &mut OtaResourceStrategySettlementReceiver,
    storage: &mut ProductStore,
    coordinator: &mut OtaCoordinator,
    now_millis: u64,
    request: &ManagementRequest,
) {
    let Some(requester) = request.requester() else {
        warn!(
            "e290-prns ota=control status=REJECTED reason=unidentified-link link={:?}",
            request.link_id(),
        );
        let _ = handle.close_link(request.link_id());
        return;
    };
    let link = *request.link_id().as_bytes();
    match request.kind() {
        ManagementRequestKind::OtaStart => {
            let manifest = match decode_start_request(request.data()) {
                Ok(manifest) => manifest,
                Err(reason) => {
                    warn!(
                        "e290-prns ota=start status=REJECTED reason={reason:?} identity={:02x?}",
                        requester.as_bytes(),
                    );
                    let _ = handle.close_link(request.link_id());
                    return;
                }
            };
            let session = match coordinator.begin(storage, link, now_millis, manifest) {
                Ok(session) => session,
                Err(OtaCoordinatorError::Busy) => {
                    if !respond_ota_status(handle, request, coordinator) {
                        warn!("e290-prns ota=start status=RESPONSE-PRESSURE");
                    }
                    return;
                }
                Err(reason) => {
                    error!("e290-prns ota=start status=FAILED reason={reason:?}");
                    let _ = respond_ota_status(handle, request, coordinator);
                    return;
                }
            };
            if coordinator.arm_next::<()>(link, session, 0).is_err() {
                error!("e290-prns ota=start status=FAILED reason=coordinator-arm");
                let _ = handle.close_link(request.link_id());
                return;
            }
        }
        ManagementRequestKind::OtaNext => {
            let (session, index) = match decode_next_request(request.data()) {
                Ok(next) => next,
                Err(reason) => {
                    warn!("e290-prns ota=next status=REJECTED reason={reason:?}");
                    let _ = handle.close_link(request.link_id());
                    return;
                }
            };
            if coordinator.arm_next::<()>(link, session, index).is_err() {
                warn!(
                    "e290-prns ota=next status=REJECTED reason=unexpected-session-or-index index={index}"
                );
                let _ = respond_ota_status(handle, request, coordinator);
                return;
            }
        }
        ManagementRequestKind::OtaStatus => {
            if request.data() != OTA_STATUS_REQUEST_VALUE {
                warn!("e290-prns ota=status status=REJECTED reason=payload");
                let _ = handle.close_link(request.link_id());
                return;
            }
            if !respond_ota_status(handle, request, coordinator) {
                warn!("e290-prns ota=status status=RESPONSE-PRESSURE");
            }
            return;
        }
        ManagementRequestKind::OtaReboot => {
            if request.data() != OTA_REBOOT_REQUEST_VALUE {
                warn!("e290-prns ota=reboot status=REJECTED reason=payload");
                let _ = handle.close_link(request.link_id());
                return;
            }
            if coordinator.status().phase != OtaPhase::Activated {
                warn!("e290-prns ota=reboot status=REJECTED reason=no-activated-image");
                let _ = respond_ota_status(handle, request, coordinator);
                return;
            }
            if !respond_ota_status(handle, request, coordinator) {
                error!("e290-prns ota=reboot status=FAILED reason=response-pressure");
                let _ = handle.close_link(request.link_id());
                return;
            }
            info!("e290-prns ota=reboot status=ARMED delay_ms=2000");
            Timer::after(Duration::from_secs(2)).await;
            info!("e290-prns ota=reboot status=EXECUTING");
            software_reset();
        }
        ManagementRequestKind::Public
        | ManagementRequestKind::Authorized
        | ManagementRequestKind::Enrollment => unreachable!(),
    }

    let strategy = ResourceStrategy::Accept {
        max_uncompressed_bytes: reticulum_e290_firmware::prns_node::INCOMING_RESOURCE_TRANSFER_BYTES
            as u64,
        accept_compressed: false,
    };
    match set_ota_resource_strategy(handle, settlements, request.link_id(), strategy).await {
        Ok(()) => {
            let status = coordinator.status();
            info!(
                "e290-prns ota=resource-gate status=ARMED identity={:02x?} session={:02x?} chunk={} verified_bytes={}",
                requester.as_bytes(),
                status.session.map(|session| *session.as_bytes()),
                status.next_chunk,
                status.verified_bytes,
            );
            if !respond_ota_status(handle, request, coordinator) {
                warn!("e290-prns ota=control status=RESPONSE-PRESSURE");
            }
        }
        Err(reason) => {
            coordinator.disarm();
            error!("e290-prns ota=resource-gate status=FAILED reason={reason:?}");
            let _ = handle.close_link(request.link_id());
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the sole product task passes its independently owned PRNS, storage, enrollment, signing, and OTA authorities explicitly"
)]
async fn answer_management<const MEMBERS: usize>(
    handle: &E290PrnsHandle,
    settlements: &mut ManagementAuthorizationSettlementReceiver,
    storage: &mut ProductStore,
    enrollment: &mut EnrollmentWindow,
    now_millis: u64,
    management: personal_rns::wire::DestinationHash,
    lxmf: Option<personal_rns::wire::DestinationHash>,
    request: ManagementRequest,
    signer: &ProductLxmfSigner,
    ota: &mut OtaCoordinator,
    ota_resource_strategy_settlements: &mut OtaResourceStrategySettlementReceiver,
    lora_status: &EmbassyInterfaceStatus,
    tcp_status: Option<&EmbassyInterfaceStatus>,
    bluetooth_status: Option<BluetoothAutoStatus<MEMBERS>>,
    #[cfg(feature = "gateway")] wifi_station_status: &WifiStationStatusCell,
) {
    match request.kind() {
        ManagementRequestKind::Enrollment => {
            let Some(requester) = request.requester() else {
                warn!(
                    "e290-prns management=enrollment status=REJECTED reason=unidentified-link link={:?}",
                    request.link_id(),
                );
                let _ = handle.close_link(request.link_id());
                return;
            };
            if request.data() != MANAGEMENT_ENROLLMENT_REQUEST_VALUE {
                warn!(
                    "e290-prns management=enrollment status=REJECTED reason=payload link={:?}",
                    request.link_id(),
                );
                let _ = handle.close_link(request.link_id());
                return;
            }
            if !enrollment.is_open(now_millis) {
                warn!(
                    "e290-prns management=enrollment status=REJECTED reason=physical-presence link={:?}",
                    request.link_id(),
                );
                let _ = handle.close_link(request.link_id());
                return;
            }
            let identity = ManagementIdentity::new(*requester.as_bytes());
            let committed = match storage.enroll_management_identity(identity) {
                Ok(committed) => committed,
                Err(reason) => {
                    error!(
                        "e290-prns management=enrollment status=REJECTED reason=durability detail={reason:?} link={:?}",
                        request.link_id(),
                    );
                    let _ = handle.close_link(request.link_id());
                    return;
                }
            };
            let consumed = enrollment.consume(now_millis);
            debug_assert!(consumed, "the checked physical-presence window is live");
            match apply_management_identity(handle, settlements, management, requester).await {
                Ok(()) => {
                    info!(
                        "e290-prns management=enrollment status=AUTHORIZED identity={:02x?} durable={committed:?}",
                        requester.as_bytes(),
                    );
                    if !handle.respond_packed(
                        request.respond_token(),
                        &MANAGEMENT_ENROLLMENT_SUCCESS_RESPONSE,
                    ) {
                        warn!(
                            "e290-prns management=enrollment status=RESPONSE-PRESSURE link={:?}",
                            request.link_id(),
                        );
                    }
                }
                Err(reason) => {
                    error!(
                        "e290-prns management=enrollment status=DURABLE-NOT-LIVE reason={reason:?} identity={:02x?}",
                        requester.as_bytes(),
                    );
                    let _ = handle.close_link(request.link_id());
                }
            }
        }
        ManagementRequestKind::OtaStart
        | ManagementRequestKind::OtaNext
        | ManagementRequestKind::OtaStatus
        | ManagementRequestKind::OtaReboot => {
            answer_ota_control(
                handle,
                ota_resource_strategy_settlements,
                storage,
                ota,
                now_millis,
                &request,
            )
            .await;
        }
        ManagementRequestKind::Public | ManagementRequestKind::Authorized => {
            let response = match request.kind() {
                ManagementRequestKind::Public => dispatch_public_management_payload(
                    request.data(),
                    management,
                    lxmf,
                    product_capabilities(storage, lxmf),
                ),
                ManagementRequestKind::Authorized => {
                    dispatch_authorized_product_payload(
                        handle,
                        request.data(),
                        management,
                        lxmf,
                        storage,
                        request.requester(),
                        signer,
                        now_millis,
                        lora_status,
                        tcp_status,
                        bluetooth_status,
                        #[cfg(feature = "gateway")]
                        wifi_station_status,
                    )
                    .await
                }
                ManagementRequestKind::Enrollment
                | ManagementRequestKind::OtaStart
                | ManagementRequestKind::OtaNext
                | ManagementRequestKind::OtaStatus
                | ManagementRequestKind::OtaReboot => unreachable!(),
            };
            match response {
                Ok(response) => {
                    if !handle.respond_packed(request.respond_token(), response.as_slice()) {
                        warn!(
                            "e290-prns management=response status=PRESSURE request={:?} link={:?}",
                            request.request_id(),
                            request.link_id(),
                        );
                    }
                }
                Err(reason) => {
                    warn!(
                        "e290-prns management=request status=REJECTED reason={reason:?} request={:?} link={:?}",
                        request.request_id(),
                        request.link_id(),
                    );
                    let _ = handle.close_link(request.link_id());
                }
            }
        }
    }
}

async fn consume_ota_resource<H: PrnsNodeApi>(
    handle: &H,
    settlements: &mut OtaResourceStrategySettlementReceiver,
    storage: &mut ProductStore,
    coordinator: &mut OtaCoordinator,
    resource: OwnedOtaResource<E290PrnsPsramAlloc>,
) {
    let link_id = resource.link_id();
    if let Err(reason) =
        set_ota_resource_strategy(handle, settlements, link_id, ResourceStrategy::AcceptNone).await
    {
        error!("e290-prns ota=resource-gate status=CLOSE-FAILED reason={reason:?}");
        coordinator.link_closed(*link_id.as_bytes());
        let _ = handle.close_link(link_id);
        return;
    }
    let metadata = resource.metadata().unwrap_or(&[]);
    match coordinator.ingest_resource(storage, *link_id.as_bytes(), metadata, resource.data()) {
        Ok(status) => {
            info!(
                "e290-prns ota=chunk status=VERIFIED hash={:02x?} chunk={} verified_bytes={} image_bytes={} phase={:?}",
                resource.hash().as_bytes(),
                status.next_chunk.saturating_sub(1),
                status.verified_bytes,
                status.image_bytes,
                status.phase,
            );
            if status.phase == OtaPhase::Activated {
                info!(
                    "e290-prns ota=activation status=SELECTED slot={:?} version={:?} action=await-explicit-reboot",
                    status.slot,
                    status.version.as_ref().map(|version| version.as_bytes()),
                );
            }
        }
        Err(reason) => {
            error!(
                "e290-prns ota=chunk status=FAILED hash={:02x?} reason={reason:?} retained={:?}",
                resource.hash().as_bytes(),
                coordinator.status().failure,
            );
            let _ = handle.close_link(link_id);
        }
    }
}

async fn seed_management_authorization<H: PrnsNodeApi>(
    handle: &H,
    settlements: &mut ManagementAuthorizationSettlementReceiver,
    storage: &ProductStore,
    management: personal_rns::wire::DestinationHash,
) {
    let Some(allowlist) = storage.management_allowlist() else {
        error!("e290-prns management-authorization status=DISABLED reason=store-unavailable");
        return;
    };
    for identity in allowlist.identities() {
        let identity = personal_rns::identity::IdentityHash::new(*identity.as_bytes());
        match apply_management_identity(handle, settlements, management, identity).await {
            Ok(()) => info!(
                "e290-prns management-authorization status=SEEDED identity={:02x?}",
                identity.as_bytes(),
            ),
            Err(reason) => error!(
                "e290-prns management-authorization status=SEED-FAILED reason={reason:?} identity={:02x?}",
                identity.as_bytes(),
            ),
        }
    }
}

fn announce_product_services<H: PrnsNodeApi>(
    handle: &H,
    management: personal_rns::wire::DestinationHash,
    lxmf: Option<personal_rns::wire::DestinationHash>,
    nomad: personal_rns::wire::DestinationHash,
    rmap: Option<personal_rns::wire::DestinationHash>,
) {
    for destination in [Some(management), lxmf, Some(nomad), rmap]
        .into_iter()
        .flatten()
    {
        if handle.announce(destination).is_none() {
            warn!(
                "e290-prns application-announce status=PRESSURE destination={:02x?}",
                destination.as_bytes(),
            );
        }
    }
}

fn announce_product_services_on_interface<H: PrnsNodeApi>(
    handle: &H,
    interface: personal_rns::interfaces::InterfaceId,
    management: personal_rns::wire::DestinationHash,
    lxmf: Option<personal_rns::wire::DestinationHash>,
    nomad: personal_rns::wire::DestinationHash,
    rmap: Option<personal_rns::wire::DestinationHash>,
) -> bool {
    let mut queued_all = true;
    for destination in [Some(management), lxmf, Some(nomad), rmap]
        .into_iter()
        .flatten()
    {
        if handle
            .issue(PrnsCommand::AnnounceNow(AnnounceNow {
                destination,
                target: AnnounceTarget::Interface(interface),
                app_data: AnnounceAppData::Registered,
            }))
            .is_none()
        {
            queued_all = false;
            warn!(
                "e290-prns application-announce status=PRESSURE target=bluetooth-interface interface={:02x?} destination={:02x?}",
                interface.as_bytes(),
                destination.as_bytes(),
            );
        }
    }
    queued_all
}

fn active_bluetooth_interfaces<const MEMBERS: usize>(
    status: Option<BluetoothAutoStatus<MEMBERS>>,
) -> [Option<personal_rns::interfaces::InterfaceId>; MEMBERS] {
    let mut active = [None; MEMBERS];
    if let Some(status) = status {
        for (slot, member) in status.members().enumerate() {
            active[slot] = Some(member.id());
        }
    }
    active
}

#[allow(
    clippy::large_enum_variant,
    reason = "the bounded task select owns each channel value directly and must not allocate while dispatching product work"
)]
enum ProductInput {
    Management(ManagementRequest),
    Lxmf(OwnedLxmfSingleDelivery<E290PrnsPsramAlloc>),
    Ota(OwnedOtaResource<E290PrnsPsramAlloc>),
    OutboundSettlement(LxmfOutboundSettlement),
    Poll,
}

fn request_outbound_path<H: PrnsNodeApi>(handle: &H, entry: OutboxEntry, now_millis: u64) {
    let mut request_id = [0_u8; 16];
    request_id.copy_from_slice(&entry.message_id()[..16]);
    for (target, source) in request_id[..8].iter_mut().zip(now_millis.to_le_bytes()) {
        *target ^= source;
    }
    if handle
        .issue(PrnsCommand::RequestPath(RequestPath {
            destination: personal_rns::wire::DestinationHash::new(entry.destination()),
            id: PathRequestId::new(request_id),
        }))
        .is_none()
    {
        warn!(
            "e290-prns application=lxmf-outbound status=PATH-PRESSURE submission={}",
            entry.submission_id().get(),
        );
    }
}

fn issue_pending_outbound<H: PrnsNodeApi>(
    handle: &H,
    storage: &mut ProductStore,
) -> Option<(CommandId, OutboxEntry)> {
    let entry = storage.next_pending_lxmf_outbound()?;
    let mut wire = [0_u8; MAX_BASIC_LXMF_WIRE_BYTES];
    let len = match storage.read_lxmf_outbound_wire(entry, &mut wire) {
        Ok(len) => len,
        Err(reason) => {
            error!(
                "e290-prns application=lxmf-outbound status=READ-FAILED submission={} reason={reason:?}",
                entry.submission_id().get(),
            );
            return None;
        }
    };
    let payload = match SendSinglePacketPayload::from_slice(&wire[16..len]) {
        Ok(payload) => payload,
        Err(()) => {
            error!(
                "e290-prns application=lxmf-outbound status=INVALID-DURABLE-CARRIER submission={} len={len}",
                entry.submission_id().get(),
            );
            return None;
        }
    };
    let id = handle.issue(PrnsCommand::SendSinglePacket(SendSinglePacket {
        destination: personal_rns::wire::DestinationHash::new(entry.destination()),
        payload,
    }))?;
    info!(
        "e290-prns application=lxmf-outbound status=ISSUED submission={} command={id:?} message={:02x?}",
        entry.submission_id().get(),
        entry.message_id(),
    );
    Some((id, entry))
}

/// Run all product application work above one already-started PRNS node.
pub(crate) async fn run(
    target: StartedPrnsTarget,
    storage: &'static mut ProductStore,
    enrollment_button: Input<'static>,
    identity_secret: Zeroizing<[u8; IDENTITY_SECRET_KEY_LEN]>,
    #[cfg(feature = "display")] mut display_telemetry_publisher: Option<
        DisplayTelemetryPublisher<CriticalSectionRawMutex>,
    >,
    #[cfg(feature = "gateway")] wifi_station_status: &'static WifiStationStatusCell,
) -> ! {
    let StartedPrnsTarget {
        handle,
        management,
        nomad,
        lxmf,
        rmap,
        lora_status,
        tcp_status,
        bluetooth_status,
        management_requests,
        mut management_authorization_settlements,
        lxmf_outbound_settlements,
        mut ota_resource_strategy_settlements,
        lxmf_deliveries,
        ota_resources,
        ..
    } = target;
    let signer = ProductLxmfSigner(InMemoryNodeIdentity::from_secret_key_bytes(
        &identity_secret,
    ));
    let no_source_identity = |_: &[u8; 16]| None::<[u8; 64]>;
    seed_management_authorization(
        &handle,
        &mut management_authorization_settlements,
        storage,
        management,
    )
    .await;
    announce_product_services(&handle, management, lxmf, nomad, rmap);
    #[cfg(feature = "display")]
    publish_display_home_telemetry(storage, &mut display_telemetry_publisher);
    info!(
        "e290-prns application-owner=READY management={:?} lxmf={} outbox={} signature_policy=python-lxmf proof_policy=prns-immediate",
        storage.management_state(),
        lxmf.is_some(),
        storage.lxmf_outbox_count().is_some(),
    );

    let mut ota_boot_confirmation_at = match storage.current_ota_image_state() {
        Ok(Some(OtaImageState::PendingVerify)) => {
            let deadline = Instant::now()
                .as_millis()
                .saturating_add(OTA_BOOT_HEALTH_WINDOW.as_millis());
            info!(
                "e290-prns ota=boot-health status=PENDING window_ms={} action=continue-application-loop",
                OTA_BOOT_HEALTH_WINDOW.as_millis(),
            );
            Some(deadline)
        }
        Ok(Some(state)) => {
            info!("e290-prns ota=boot-health status={state:?} action=none");
            None
        }
        Ok(None) => {
            info!("e290-prns ota=boot-health status=UNSELECTED action=none");
            None
        }
        Err(reason) => {
            error!(
                "e290-prns ota=boot-health status=FAILED reason={reason:?} action=software-reset"
            );
            Timer::after(Duration::from_millis(200)).await;
            software_reset();
        }
    };

    let mut enrollment = EnrollmentWindow::new();
    let mut ota = OtaCoordinator::new();
    let mut next_button_sample = Instant::now().as_millis();
    let mut next_service_announce =
        next_button_sample.saturating_add(SERVICE_ANNOUNCE_INTERVAL_MILLIS);
    let mut announced_bluetooth_interfaces =
        [None; reticulum_e290_firmware::prns_node::BLUETOOTH_PEER_CAPACITY];
    let mut pending_bluetooth_announces =
        [None; reticulum_e290_firmware::prns_node::BLUETOOTH_PEER_CAPACITY];
    let mut pending_lxmf: Option<OwnedLxmfSingleDelivery<E290PrnsPsramAlloc>> = None;
    let mut retry_lxmf_at: Option<u64> = None;
    let mut outbound_inflight: Option<(CommandId, OutboxEntry)> = None;
    let mut outbound_retry = LxmfOutboundRetryState::default();
    let mut retry_outbound_at: Option<u64> = None;
    loop {
        let now_millis = Instant::now().as_millis();
        if ota_boot_confirmation_at.is_some_and(|deadline| now_millis >= deadline) {
            match storage.confirm_pending_ota_image() {
                Ok(true) => {
                    info!(
                        "e290-prns ota=boot-health status=VALID window_ms={} confirmation=read-back",
                        OTA_BOOT_HEALTH_WINDOW.as_millis(),
                    );
                    ota_boot_confirmation_at = None;
                }
                Ok(false) => {
                    error!(
                        "e290-prns ota=boot-health status=FAILED reason=state-changed action=software-reset"
                    );
                    Timer::after(Duration::from_millis(200)).await;
                    software_reset();
                }
                Err(reason) => {
                    error!(
                        "e290-prns ota=boot-health status=FAILED reason={reason:?} action=software-reset"
                    );
                    Timer::after(Duration::from_millis(200)).await;
                    software_reset();
                }
            }
        }
        if now_millis >= next_button_sample {
            if enrollment.observe(now_millis, enrollment_button.is_low()) {
                info!(
                    "e290-prns management=enrollment-window status=OPEN duration_ms={}",
                    reticulum_e290_firmware::management_authorization::ENROLLMENT_WINDOW_MILLIS,
                );
            }
            next_button_sample = now_millis.saturating_add(BUTTON_SAMPLE_MILLIS);
        }
        if now_millis >= next_service_announce {
            announce_product_services(&handle, management, lxmf, nomad, rmap);
            next_service_announce = now_millis.saturating_add(SERVICE_ANNOUNCE_INTERVAL_MILLIS);
        }
        let active_bluetooth = active_bluetooth_interfaces(bluetooth_status);
        for announced in &mut announced_bluetooth_interfaces {
            if announced.is_some_and(|id| !active_bluetooth.contains(&Some(id))) {
                *announced = None;
            }
        }
        for pending in &mut pending_bluetooth_announces {
            if pending.is_some_and(|(id, _)| !active_bluetooth.contains(&Some(id))) {
                *pending = None;
            }
        }
        for interface in active_bluetooth.into_iter().flatten() {
            if announced_bluetooth_interfaces.contains(&Some(interface))
                || pending_bluetooth_announces
                    .iter()
                    .flatten()
                    .any(|(id, _)| *id == interface)
            {
                continue;
            }
            if let Some(slot) = pending_bluetooth_announces
                .iter_mut()
                .find(|slot| slot.is_none())
            {
                *slot = Some((
                    interface,
                    now_millis.saturating_add(BLUETOOTH_ANNOUNCE_ATTACH_DELAY_MILLIS),
                ));
            }
        }
        for pending in &mut pending_bluetooth_announces {
            let Some((interface, deadline)) = *pending else {
                continue;
            };
            if now_millis < deadline {
                continue;
            }
            if announce_product_services_on_interface(
                &handle, interface, management, lxmf, nomad, rmap,
            ) {
                if let Some(slot) = announced_bluetooth_interfaces
                    .iter_mut()
                    .find(|slot| slot.is_none())
                {
                    *slot = Some(interface);
                }
                *pending = None;
                info!(
                    "e290-prns application-announce status=QUEUED target=bluetooth-interface interface={:02x?}",
                    interface.as_bytes(),
                );
            } else {
                *pending = Some((
                    interface,
                    now_millis.saturating_add(BLUETOOTH_ANNOUNCE_RETRY_DELAY_MILLIS),
                ));
            }
        }

        if outbound_inflight.is_none()
            && retry_outbound_at.is_none_or(|deadline| now_millis >= deadline)
        {
            match issue_pending_outbound(&handle, storage) {
                Some(inflight) => {
                    outbound_inflight = Some(inflight);
                    retry_outbound_at = None;
                }
                None if storage.next_pending_lxmf_outbound().is_some() => {
                    retry_outbound_at =
                        Some(now_millis.saturating_add(OUTBOUND_RETRY_DELAY.as_millis()));
                }
                None => retry_outbound_at = None,
            }
        }

        if let Some(delivery) = pending_lxmf.as_ref()
            && retry_lxmf_at.is_none_or(|deadline| now_millis >= deadline)
        {
            let Some(outcome) = storage.commit_lxmf_carrier(
                delivery.carrier(),
                Some(delivery.ingress_observation()),
                wire_limits(),
                &no_source_identity,
            ) else {
                warn!("e290-prns application=lxmf status=DISCARDED reason=store-unavailable");
                pending_lxmf = None;
                retry_lxmf_at = None;
                continue;
            };
            match outcome {
                DurableCarrierOutcome::Durable(success) => {
                    info!(
                        "e290-prns application=lxmf status=DURABLE kind={:?} message={:?}",
                        success.kind(),
                        success.receipt().message_id(),
                    );
                    pending_lxmf = None;
                    retry_lxmf_at = None;
                    continue;
                }
                DurableCarrierOutcome::Retained(reason) => {
                    match carrier_retention_action(&reason, storage.lxmf_pending_message_id()) {
                        LxmfRetentionAction::Terminal(terminal) => {
                            warn!(
                                "e290-prns application=lxmf status=DISCARDED reason={terminal:?} detail={reason:?}"
                            );
                            pending_lxmf = None;
                            retry_lxmf_at = None;
                            continue;
                        }
                        LxmfRetentionAction::Retry(class) => {
                            warn!(
                                "e290-prns application=lxmf status=RETAINED class={class:?} reason={reason:?}"
                            );
                            retry_lxmf_at = Some(
                                now_millis.saturating_add(APPLICATION_RETRY_DELAY.as_millis()),
                            );
                        }
                        LxmfRetentionAction::HoldPendingFault => {
                            error!(
                                "e290-prns application=lxmf status=HELD reason=pending-store-fault detail={reason:?}"
                            );
                            retry_lxmf_at = Some(
                                now_millis.saturating_add(RETAINED_FAULT_RECHECK_DELAY.as_millis()),
                            );
                        }
                    }
                }
            }
        }

        let wake_at = retry_lxmf_at
            .map(|retry| retry.min(next_button_sample))
            .unwrap_or(next_button_sample)
            .min(next_service_announce)
            .min(retry_outbound_at.unwrap_or(u64::MAX))
            .min(ota_boot_confirmation_at.unwrap_or(u64::MAX));
        let input = if pending_lxmf.is_some() {
            match select4(
                management_requests.receive(),
                ota_resources.receive(),
                lxmf_outbound_settlements.receive(),
                Timer::at(Instant::from_millis(wake_at)),
            )
            .await
            {
                Either4::First(request) => ProductInput::Management(request),
                Either4::Second(resource) => ProductInput::Ota(resource),
                Either4::Third(settlement) => ProductInput::OutboundSettlement(settlement),
                Either4::Fourth(()) => ProductInput::Poll,
            }
        } else {
            match select5(
                management_requests.receive(),
                lxmf_deliveries.receive(),
                ota_resources.receive(),
                lxmf_outbound_settlements.receive(),
                Timer::at(Instant::from_millis(wake_at)),
            )
            .await
            {
                Either5::First(request) => ProductInput::Management(request),
                Either5::Second(delivery) => ProductInput::Lxmf(delivery),
                Either5::Third(resource) => ProductInput::Ota(resource),
                Either5::Fourth(settlement) => ProductInput::OutboundSettlement(settlement),
                Either5::Fifth(()) => ProductInput::Poll,
            }
        };
        match input {
            ProductInput::Management(request) => {
                answer_management(
                    &handle,
                    &mut management_authorization_settlements,
                    storage,
                    &mut enrollment,
                    Instant::now().as_millis(),
                    management,
                    lxmf,
                    request,
                    &signer,
                    &mut ota,
                    &mut ota_resource_strategy_settlements,
                    lora_status,
                    tcp_status,
                    bluetooth_status,
                    #[cfg(feature = "gateway")]
                    wifi_station_status,
                )
                .await;
            }
            ProductInput::Lxmf(delivery) => {
                pending_lxmf = Some(delivery);
                retry_lxmf_at = None;
            }
            ProductInput::Ota(resource) => {
                consume_ota_resource(
                    &handle,
                    &mut ota_resource_strategy_settlements,
                    storage,
                    &mut ota,
                    resource,
                )
                .await;
            }
            ProductInput::OutboundSettlement(settlement) => {
                let Some((expected, entry)) = outbound_inflight else {
                    warn!(
                        "e290-prns application=lxmf-outbound status=STALE-SETTLEMENT command={:?}",
                        settlement.id,
                    );
                    continue;
                };
                if settlement.id != expected {
                    warn!(
                        "e290-prns application=lxmf-outbound status=STALE-SETTLEMENT expected={expected:?} actual={:?}",
                        settlement.id,
                    );
                    continue;
                }
                outbound_inflight = None;
                match settlement.result {
                    Ok(receipt) => match storage.mark_lxmf_outbound_delivered(entry) {
                        Ok(_) => {
                            outbound_retry.reset();
                            info!(
                                "e290-prns application=lxmf-outbound status=DELIVERED submission={} evidence={:?} rtt={:?}",
                                entry.submission_id().get(),
                                receipt.evidence,
                                receipt.rtt,
                            );
                            retry_outbound_at = None;
                        }
                        Err(reason) => {
                            error!(
                                "e290-prns application=lxmf-outbound status=DELIVERY-MARK-FAILED submission={} reason={reason:?}",
                                entry.submission_id().get(),
                            );
                            retry_outbound_at = Some(
                                Instant::now()
                                    .as_millis()
                                    .saturating_add(OUTBOUND_RETRY_DELAY.as_millis()),
                            );
                        }
                    },
                    Err(reason) => {
                        let rediscover_path = if matches!(reason, SendSinglePacketFailure::Timeout)
                        {
                            outbound_retry.note_timeout()
                        } else {
                            outbound_retry.reset();
                            false
                        };
                        if matches!(
                            reason,
                            SendSinglePacketFailure::Rejected(
                                SendSinglePacketRejection::NoRouteToDestination
                                    | SendSinglePacketRejection::NotDirectlyReachable
                            )
                        ) {
                            request_outbound_path(&handle, entry, Instant::now().as_millis());
                        }
                        if rediscover_path {
                            let destination =
                                personal_rns::wire::DestinationHash::new(entry.destination());
                            match handle.route(destination).await {
                                Ok(Some(route)) => info!(
                                    "e290-prns application=lxmf-outbound status=STALE-PATH submission={} hops={} interface={:02x?} via={:?}",
                                    entry.submission_id().get(),
                                    route.route.hops,
                                    route.route.interface.as_bytes(),
                                    route.route.via,
                                ),
                                Ok(None) => info!(
                                    "e290-prns application=lxmf-outbound status=STALE-PATH submission={} route=absent",
                                    entry.submission_id().get(),
                                ),
                                Err(error) => warn!(
                                    "e290-prns application=lxmf-outbound status=STALE-PATH-INSPECTION-FAILED submission={} reason={error:?}",
                                    entry.submission_id().get(),
                                ),
                            }
                            request_outbound_path(&handle, entry, Instant::now().as_millis());
                        }
                        warn!(
                            "e290-prns application=lxmf-outbound status=RETRY submission={} reason={reason:?}",
                            entry.submission_id().get(),
                        );
                        let retry_delay = if rediscover_path {
                            OUTBOUND_PATH_REQUEST_WAIT
                        } else {
                            OUTBOUND_RETRY_DELAY
                        };
                        retry_outbound_at = Some(
                            Instant::now()
                                .as_millis()
                                .saturating_add(retry_delay.as_millis()),
                        );
                    }
                }
            }
            ProductInput::Poll => {}
        }
        #[cfg(feature = "display")]
        publish_display_home_telemetry(storage, &mut display_telemetry_publisher);
    }
}
