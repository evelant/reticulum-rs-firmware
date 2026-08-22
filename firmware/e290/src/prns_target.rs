//! Compile-checked PRNS ownership graph for the E290 cutover.
//!
//! This module constructs one unchanged-PRNS node and its sole LoRa owner from
//! product hardware, identity, application, and capacity inputs. It is not
//! started alongside the legacy node: the final cutover calls [`start`] only
//! after the old Reticulum and radio actors have been removed.

use allocator_api2::{boxed::Box, vec::Vec};
use embassy_executor::Spawner;
#[cfg(feature = "gateway")]
use embassy_net::{IpEndpoint, Ipv4Address, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver};
use embassy_sync::signal::Signal;
use embassy_time::Delay;
use esp_hal::gpio::{Input, Output};
use esp_hal::rng::Rng;
use personal_rns::EmbassyInspectionLane;
use personal_rns::bluetooth_auto::{BluetoothAutoShared, BluetoothAutoStatus};
use personal_rns::engine::{InterfaceCounts, IssuedCommand, Settlement};
use personal_rns::identity::{IDENTITY_SECRET_KEY_LEN, Zeroizing};
#[cfg(feature = "gateway")]
use personal_rns::interfaces::InterfaceMode;
use personal_rns::interfaces::bluetooth_auto::BLE_HW_MTU;
use personal_rns::interfaces::lora::{LORA_MAX_PAYLOAD, RadioProfile};
#[cfg(feature = "gateway")]
use personal_rns::interfaces::wifi_auto::WIFI_EMBEDDED_BITRATE_CEILING_BPS;
use personal_rns::interfaces::{ConnectionState, InterfaceId, InterfaceKind};
use personal_rns::lora::{
    LORA_TX_QUEUE_BYTES, LoRaConfigError, LoRaControl, LoRaInterface, LoRaInterfaceInput,
    LoRaSpectrumStatus,
};
use personal_rns::manifold::embassy::{
    EmbassyHost, EmbassyInterfaceSeam, EmbassyInterfaceStatus, InterfaceLifecycle,
};
use personal_rns::manifold::grant::FrameSlot;
#[cfg(feature = "gateway")]
use personal_rns::manifold::interface_seam::EMBEDDED_MAX_WIRE_FRAME_LEN;
use personal_rns::manifold::interface_seam::Interface;
#[cfg(feature = "gateway")]
use personal_rns::manifold::reconnect::ReconnectPolicy;
use personal_rns::radios::sx126x::Sx126x;
use personal_rns::runtime::{
    CompletionPool, Diagnostic, EmbassyInterfaceStore, Fleet, LaneClaimError, ManifoldLaneSet,
    PrnsEvent, PrnsNode, PrnsNodeHandle, StaticManifoldLane,
};
#[cfg(feature = "gateway")]
use personal_rns::tcp::{
    TCP_DNS_HOSTNAME_MAX_BYTES, TcpClient, TcpClientInput, TcpClientTarget, TcpSocketBuffers,
};
use personal_rns::wire::DestinationHash;
use static_cell::StaticCell;

use reticulum_e290_firmware::prns_applications::{
    ApplicationCatalogError, ApplicationProfile, application_catalog,
};
use reticulum_e290_firmware::prns_events::{
    ApplicationPayloadBudget, OwnedLxmfSingleDelivery, OwnedOtaResource, copy_lxmf_single_delivery,
    copy_ota_resource,
};
use reticulum_e290_firmware::prns_lora::{
    E290_PRNS_AIRTIME_POLICY, E290_SX126X_BOARD_CONFIG, prns_internal_lora_descriptor,
};
use reticulum_e290_firmware::prns_node::{
    APPLICATION_EVENT_CAPACITY, APPLICATION_EVENT_PAYLOAD_POOL_BYTES,
    APPLICATION_SETTLEMENT_CAPACITY, COMMAND_CAPACITY, COMPLETION_CAPACITY, EngineStorage,
    INTERFACE_CAPACITY, INTERFACE_INSPECTION_CAPACITY, INTERFACE_LIFECYCLE_CAPACITY,
    MANIFOLD_INGRESS_DEPTH, MANIFOLD_LANE_CAPACITY, MANIFOLD_NOTIFICATION_CAPACITY,
    MANIFOLD_OUTBOUND_DEPTH, PACKET_PHY_INDEX_BUCKETS, PACKET_PHY_RETENTION_CAPACITY,
};
use reticulum_e290_firmware::prns_persistence::{E290PrnsPersistence, e290_prns_persistence};
use reticulum_e290_firmware::prns_requests::{
    ApplicationRequestRoutes, LxmfOutboundSettlement, LxmfOutboundSettlementChannel,
    ManagementAuthorizationSettlement, ManagementAuthorizationSettlementChannel, ManagementRequest,
    ManagementRequestChannel, OtaResourceStrategySettlement, OtaResourceStrategySettlementChannel,
    PrnsApplicationState, application_recipe,
};
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_tcp_profile::{WifiTcpBootstrap, WifiTcpPeerAddress};

use super::{BoundedBusy, E290PrnsPsramAlloc, E290SpiDevice};
use crate::shared_flash::E290PrnsFlash;

type Mtx = CriticalSectionRawMutex;
type PrnsRadio =
    Sx126x<E290SpiDevice, BoundedBusy<Input<'static>>, Input<'static>, Output<'static>, Delay>;
type PrnsLoRaInterface = LoRaInterface<'static, PrnsRadio>;
type PrnsLoRaSeam =
    EmbassyInterfaceSeam<'static, Mtx, MANIFOLD_NOTIFICATION_CAPACITY, LORA_MAX_PAYLOAD>;
#[cfg(feature = "gateway")]
type PrnsTcpInterface = TcpClient<'static>;
#[cfg(feature = "gateway")]
type PrnsTcpSeam =
    EmbassyInterfaceSeam<'static, Mtx, MANIFOLD_NOTIFICATION_CAPACITY, EMBEDDED_MAX_WIRE_FRAME_LEN>;
pub(crate) type E290BleFleet =
    Fleet<Mtx, BLE_HW_MTU, MANIFOLD_NOTIFICATION_CAPACITY, INTERFACE_LIFECYCLE_CAPACITY>;
type PrnsEventCallback = for<'a> fn(PrnsEvent<'a>, &PrnsApplicationState);
type E290Persistence = E290PrnsPersistence<E290PrnsFlash, E290PrnsPsramAlloc>;
type E290LxmfDelivery = OwnedLxmfSingleDelivery<E290PrnsPsramAlloc>;
type E290OtaResource = OwnedOtaResource<E290PrnsPsramAlloc>;
pub(crate) type ManagementAuthorizationSettlementReceiver =
    Receiver<'static, Mtx, ManagementAuthorizationSettlement, APPLICATION_SETTLEMENT_CAPACITY>;
pub(crate) type LxmfOutboundSettlementReceiver =
    Receiver<'static, Mtx, LxmfOutboundSettlement, APPLICATION_SETTLEMENT_CAPACITY>;
pub(crate) type OtaResourceStrategySettlementReceiver =
    Receiver<'static, Mtx, OtaResourceStrategySettlement, APPLICATION_SETTLEMENT_CAPACITY>;
type E290PrnsNode = PrnsNode<
    PrnsApplicationState,
    ApplicationRequestRoutes,
    PrnsEventCallback,
    EngineStorage<E290PrnsPsramAlloc>,
    EmbassyHost<fn(&mut [u8])>,
    Mtx,
    MANIFOLD_LANE_CAPACITY,
    INTERFACE_CAPACITY,
    MANIFOLD_NOTIFICATION_CAPACITY,
    COMMAND_CAPACITY,
    INTERFACE_LIFECYCLE_CAPACITY,
    COMPLETION_CAPACITY,
>;
pub(crate) type E290PrnsHandle =
    PrnsNodeHandle<'static, Mtx, COMMAND_CAPACITY, COMPLETION_CAPACITY>;
type E290ManifoldLanes =
    ManifoldLaneSet<Mtx, MANIFOLD_LANE_CAPACITY, MANIFOLD_NOTIFICATION_CAPACITY>;
type E290InterfaceStore = EmbassyInterfaceStore<
    Mtx,
    INTERFACE_INSPECTION_CAPACITY,
    PACKET_PHY_RETENTION_CAPACITY,
    PACKET_PHY_INDEX_BUCKETS,
>;

static LORA_CONTROL: LoRaControl = LoRaControl::new();
const BLUETOOTH_SUPERVISOR_ID: InterfaceId =
    InterfaceId::new([InterfaceKind::BluetoothAuto as u8, 0, 0, 0, 0, 0, 0, 0]);
static BLUETOOTH_SHARED: BluetoothAutoShared<
    { reticulum_e290_firmware::prns_node::BLUETOOTH_PEER_CAPACITY },
> = BluetoothAutoShared::new(BLUETOOTH_SUPERVISOR_ID);
static NOTIFY: Channel<Mtx, InterfaceId, MANIFOLD_NOTIFICATION_CAPACITY> = Channel::new();
static COMMANDS: Channel<Mtx, IssuedCommand, COMMAND_CAPACITY> = Channel::new();
static LIFECYCLE: Channel<Mtx, InterfaceLifecycle, INTERFACE_LIFECYCLE_CAPACITY> = Channel::new();
static COMPLETION: CompletionPool<Mtx, COMPLETION_CAPACITY> = CompletionPool::new();
static INSPECTION: EmbassyInspectionLane<Mtx> = EmbassyInspectionLane::new();
static BLUETOOTH_OUTBOUND_WAKE: Signal<Mtx, ()> = Signal::new();
static INTERFACE_STORE: E290InterfaceStore = EmbassyInterfaceStore::new();
static MANAGEMENT_AUTHORIZATION_SETTLEMENTS: ManagementAuthorizationSettlementChannel =
    ManagementAuthorizationSettlementChannel::new();
static LXMF_OUTBOUND_SETTLEMENTS: LxmfOutboundSettlementChannel =
    LxmfOutboundSettlementChannel::new();
static OTA_RESOURCE_STRATEGY_SETTLEMENTS: OtaResourceStrategySettlementChannel =
    OtaResourceStrategySettlementChannel::new();
static APPLICATION_PAYLOADS: ApplicationPayloadBudget =
    ApplicationPayloadBudget::new(APPLICATION_EVENT_PAYLOAD_POOL_BYTES);
static LXMF_DELIVERIES: Channel<Mtx, E290LxmfDelivery, APPLICATION_EVENT_CAPACITY> = Channel::new();
static OTA_RESOURCES: Channel<Mtx, E290OtaResource, APPLICATION_EVENT_CAPACITY> = Channel::new();
static LORA_MANIFOLD_LANE: StaticManifoldLane<Mtx, LORA_MAX_PAYLOAD, MANIFOLD_INGRESS_DEPTH, 0> =
    StaticManifoldLane::new();
#[cfg(feature = "gateway")]
static TCP_MANIFOLD_LANE: StaticManifoldLane<
    Mtx,
    EMBEDDED_MAX_WIRE_FRAME_LEN,
    MANIFOLD_INGRESS_DEPTH,
    0,
> = StaticManifoldLane::new();
static BLUETOOTH_MANIFOLD_LANE: StaticManifoldLane<Mtx, BLE_HW_MTU, MANIFOLD_INGRESS_DEPTH, 0> =
    StaticManifoldLane::new();
static LORA_STATUS: StaticCell<EmbassyInterfaceStatus> = StaticCell::new();
static LORA_SPECTRUM: StaticCell<LoRaSpectrumStatus> = StaticCell::new();
#[cfg(feature = "gateway")]
static TCP_STATUS: StaticCell<EmbassyInterfaceStatus> = StaticCell::new();
static NODE: StaticCell<E290PrnsNode> = StaticCell::new();
static PERSISTENCE: StaticCell<E290Persistence> = StaticCell::new();
/// Failure to assemble the product inputs through unchanged PRNS APIs.
#[allow(
    dead_code,
    reason = "payloads are retained for target Debug diagnostics on fatal startup paths"
)]
#[derive(Debug)]
pub(crate) enum PrnsTargetStartError {
    /// A destination name or the shared application registry was invalid.
    ApplicationCatalog(ApplicationCatalogError),
    /// PRNS rejected the product's LoRa profile or airtime policy.
    RadioConfiguration(LoRaConfigError),
    /// The public Internal-mode interface policy rejected the profile.
    InterfacePolicy,
    /// Mapped PSRAM could not reserve a required bounded queue.
    ExternalAllocation,
    /// A validated product TCP hostname exceeded PRNS's public target type.
    TcpHostname,
    /// The statically owned LoRa lane could not be claimed.
    LaneClaim(LaneClaimError),
}

/// PRNS's dynamic Bluetooth Auto lane and shared supervisor state.
pub(crate) struct E290PrnsBluetooth {
    pub(crate) fleet: E290BleFleet,
    pub(crate) shared: &'static BluetoothAutoShared<
        { reticulum_e290_firmware::prns_node::BLUETOOTH_PEER_CAPACITY },
    >,
}

/// PRNS TCP client plus its live interface status projection.
#[cfg(feature = "gateway")]
pub(crate) struct E290PrnsTcp {
    interface: PrnsTcpInterface,
    status: &'static EmbassyInterfaceStatus,
}

/// Command handle and application hashes produced by the PRNS recipe.
pub(crate) struct StartedPrnsTarget {
    /// Ordinary PRNS command/settlement owner used by product applications.
    pub handle: E290PrnsHandle,
    /// Shared management and OTA destination.
    pub management: DestinationHash,
    /// Nomad Network node destination.
    pub nomad: DestinationHash,
    /// LXMF delivery destination when the application is enabled.
    pub lxmf: Option<DestinationHash>,
    /// Optional RMAP discovery destination.
    pub rmap: Option<DestinationHash>,
    /// Optional PRNS TCP client interface identity.
    pub tcp: Option<InterfaceId>,
    /// Optional live PRNS TCP interface status.
    pub tcp_status: Option<&'static EmbassyInterfaceStatus>,
    /// Live PRNS LoRa interface status.
    pub lora_status: &'static EmbassyInterfaceStatus,
    /// Optional Bluetooth Auto supervisor lane for the sole BLE controller.
    pub bluetooth: Option<E290PrnsBluetooth>,
    /// Live Bluetooth member status retained after the controller takes its fleet.
    pub bluetooth_status: Option<
        BluetoothAutoStatus<{ reticulum_e290_firmware::prns_node::BLUETOOTH_PEER_CAPACITY }>,
    >,
    /// Product-owned management requests copied from PRNS request callbacks.
    pub management_requests: Receiver<'static, Mtx, ManagementRequest, APPLICATION_EVENT_CAPACITY>,
    /// Settlements for product-issued PRNS management authorization commands.
    pub management_authorization_settlements: ManagementAuthorizationSettlementReceiver,
    /// Settlements for product-issued ordinary PRNS LXMF Single sends.
    pub lxmf_outbound_settlements: LxmfOutboundSettlementReceiver,
    /// Settlements for product-issued PRNS per-Link OTA Resource gates.
    pub ota_resource_strategy_settlements: OtaResourceStrategySettlementReceiver,
    /// Opportunistic LXMF deliveries copied into the bounded PSRAM budget.
    pub lxmf_deliveries: Receiver<'static, Mtx, E290LxmfDelivery, APPLICATION_EVENT_CAPACITY>,
    /// Complete PRNS-verified Resources copied into the product OTA lane.
    pub ota_resources: Receiver<'static, Mtx, E290OtaResource, APPLICATION_EVENT_CAPACITY>,
}

/// Construct PRNS's native SX126x radio around the already-owned E290 pins.
pub(crate) fn radio(
    spi: E290SpiDevice,
    busy: BoundedBusy<Input<'static>>,
    dio1: Input<'static>,
    reset: Output<'static>,
) -> PrnsRadio {
    Sx126x::new(spi, busy, dio1, reset, Delay, E290_SX126X_BOARD_CONFIG)
}

fn hardware_entropy(bytes: &mut [u8]) {
    Rng::new().read(bytes);
}

fn observe_persistence(diagnostic: personal_rns::runtime::EmbeddedPersistenceDiagnostic) {
    log::debug!("e290-prns persistence={diagnostic:?}");
}

fn observe_prns_event(event: PrnsEvent<'_>, state: &PrnsApplicationState) {
    if let PrnsEvent::Diagnostic(Diagnostic::CommandSettled {
        id,
        settlement: Settlement::AllowRequester(result),
    }) = &event
        && MANAGEMENT_AUTHORIZATION_SETTLEMENTS
            .try_send(ManagementAuthorizationSettlement {
                id: *id,
                result: *result,
            })
            .is_err()
    {
        log::error!("e290-prns management-authorization pressure=settlement-lane-full");
    }
    if let PrnsEvent::Diagnostic(Diagnostic::CommandSettled {
        id,
        settlement: Settlement::SetResourceStrategy(result),
    }) = &event
        && OTA_RESOURCE_STRATEGY_SETTLEMENTS
            .try_send(OtaResourceStrategySettlement {
                id: *id,
                result: *result,
            })
            .is_err()
    {
        log::error!("e290-prns ota pressure=resource-strategy-settlement-lane-full");
    }
    if let PrnsEvent::Diagnostic(Diagnostic::CommandSettled {
        id,
        settlement: Settlement::SendSinglePacket(result),
    }) = &event
        && LXMF_OUTBOUND_SETTLEMENTS
            .try_send(LxmfOutboundSettlement {
                id: *id,
                result: *result,
            })
            .is_err()
    {
        log::error!("e290-prns lxmf-outbound pressure=settlement-lane-full");
    }
    match copy_lxmf_single_delivery::<E290PrnsPsramAlloc>(&event, state, &APPLICATION_PAYLOADS) {
        Ok(Some(delivery)) => {
            if LXMF_DELIVERIES.try_send(delivery).is_err() {
                log::warn!("e290-prns application-event pressure=lxmf-delivery-lane-full");
            }
        }
        Ok(None) => {}
        Err(error) => {
            log::warn!("e290-prns application-event pressure={error:?}");
        }
    }
    match copy_ota_resource::<E290PrnsPsramAlloc>(&event, &APPLICATION_PAYLOADS) {
        Ok(Some(resource)) => {
            if OTA_RESOURCES.try_send(resource).is_err() {
                log::warn!("e290-prns application-event pressure=ota-resource-lane-full");
            }
        }
        Ok(None) => {}
        Err(error) => {
            log::warn!("e290-prns ota-resource pressure={error:?}");
        }
    }
}

fn allocate_tx_queue() -> Result<&'static mut [u8], PrnsTargetStartError> {
    allocate_bytes(LORA_TX_QUEUE_BYTES)
}

fn allocate_bytes(bytes: usize) -> Result<&'static mut [u8], PrnsTargetStartError> {
    let mut queue = Vec::new_in(E290PrnsPsramAlloc);
    queue
        .try_reserve_exact(bytes)
        .map_err(|_| PrnsTargetStartError::ExternalAllocation)?;
    queue.resize(bytes, 0);
    Ok(queue.leak())
}

struct InitializedPrnsNode {
    handle: E290PrnsHandle,
    node: &'static mut E290PrnsNode,
    persistence: &'static mut E290Persistence,
    management: DestinationHash,
    nomad: DestinationHash,
    lxmf: Option<DestinationHash>,
    rmap: Option<DestinationHash>,
    management_requests: Receiver<'static, Mtx, ManagementRequest, APPLICATION_EVENT_CAPACITY>,
}

/// Place the copied application-request lane in mapped PSRAM. Its payload
/// slots are ordinary product work, not interrupt or radio state, and keeping
/// them out of internal DRAM preserves the reviewed CPU0 startup stack.
#[inline(never)]
fn allocate_management_requests() -> Result<&'static ManagementRequestChannel, PrnsTargetStartError>
{
    let slot =
        Box::<ManagementRequestChannel, E290PrnsPsramAlloc>::try_new_uninit_in(E290PrnsPsramAlloc)
            .map_err(|_| PrnsTargetStartError::ExternalAllocation)?;
    Ok(Box::leak(Box::write(slot, ManagementRequestChannel::new())))
}

/// Keep PRNS recipe materialization out of the board/interface construction
/// frame. The runtime already initializes its node in static storage; this
/// product helper only prevents unrelated board values from sharing that
/// synchronous compiler frame.
#[inline(never)]
fn initialize_prns_node(
    lanes: E290ManifoldLanes,
    shared_flash: E290PrnsFlash,
    transport_identity: Zeroizing<[u8; IDENTITY_SECRET_KEY_LEN]>,
    destination_identity: &[u8; IDENTITY_SECRET_KEY_LEN],
    lxmf_announce_app_data: &[u8],
    application_profile: ApplicationProfile,
) -> Result<InitializedPrnsNode, PrnsTargetStartError> {
    let mut peer_discovery_incarnation = [0_u8; 8];
    hardware_entropy(&mut peer_discovery_incarnation);
    reticulum_e290_firmware::prns_peer_discovery::initialize_in(
        peer_discovery_incarnation,
        E290PrnsPsramAlloc,
    )
    .map_err(|_| PrnsTargetStartError::ExternalAllocation)?;
    let handle = PrnsNodeHandle::new_with_inspection(COMMANDS.sender(), &COMPLETION, &INSPECTION);
    let wiring = lanes.into_manifold_wiring(
        NOTIFY.receiver(),
        COMMANDS.receiver(),
        LIFECYCLE.receiver(),
        handle,
    );
    let persistence =
        e290_prns_persistence::<_, E290PrnsPsramAlloc>(shared_flash, observe_persistence);
    let management_requests = allocate_management_requests()?;
    let catalog = application_catalog(
        destination_identity,
        lxmf_announce_app_data,
        application_profile,
    )
    .map_err(PrnsTargetStartError::ApplicationCatalog)?;
    let management = catalog.management;
    let nomad = catalog.nomad;
    let lxmf = catalog.lxmf;
    let rmap = catalog.rmap;
    let recipe = application_recipe::<E290PrnsPsramAlloc, _, _>(
        Some(transport_identity),
        catalog,
        management_requests,
        persistence,
        observe_prns_event as PrnsEventCallback,
    );
    let (node, persistence) = PrnsNode::init_static_with_persistence(
        &NODE,
        recipe,
        wiring,
        EmbassyHost::new(hardware_entropy as fn(&mut [u8])),
    );
    node.set_accepted_announce_observer(
        reticulum_e290_firmware::prns_peer_discovery::observe_accepted_announce,
    );
    let persistence = PERSISTENCE.init(persistence);
    Ok(InitializedPrnsNode {
        handle,
        node,
        persistence,
        management,
        nomad,
        lxmf,
        rmap,
        management_requests: management_requests.receiver(),
    })
}

fn allocate_outbound<const MTU: usize>()
-> Result<&'static mut [FrameSlot<MTU>], PrnsTargetStartError> {
    let mut slots = Vec::new_in(E290PrnsPsramAlloc);
    slots
        .try_reserve_exact(MANIFOLD_OUTBOUND_DEPTH)
        .map_err(|_| PrnsTargetStartError::ExternalAllocation)?;
    for _ in 0..MANIFOLD_OUTBOUND_DEPTH {
        slots.push(FrameSlot::empty());
    }
    Ok(slots.leak())
}

/// Construct PRNS's standard TCP client around the station-owned network stack.
#[cfg(feature = "gateway")]
pub(crate) fn tcp(
    stack: Stack<'static>,
    bootstrap: WifiTcpBootstrap,
) -> Result<E290PrnsTcp, PrnsTargetStartError> {
    let mut channel_tag = Vec::new_in(E290PrnsPsramAlloc);
    channel_tag
        .try_reserve_exact(TCP_DNS_HOSTNAME_MAX_BYTES + 3)
        .map_err(|_| PrnsTargetStartError::ExternalAllocation)?;
    let target = match bootstrap.address() {
        WifiTcpPeerAddress::Ipv4(address) => {
            channel_tag.push(1);
            channel_tag.extend_from_slice(&address);
            TcpClientTarget::endpoint(IpEndpoint::new(
                Ipv4Address::new(address[0], address[1], address[2], address[3]).into(),
                bootstrap.port(),
            ))
        }
        WifiTcpPeerAddress::Dns { .. } => {
            let address = bootstrap.address();
            let hostname = address
                .dns_hostname()
                .expect("the DNS address variant has a hostname");
            let hostname = heapless::String::<TCP_DNS_HOSTNAME_MAX_BYTES>::try_from(hostname)
                .map_err(|_| PrnsTargetStartError::TcpHostname)?;
            channel_tag.push(2);
            channel_tag.extend_from_slice(hostname.as_bytes());
            TcpClientTarget::dns(hostname, bootstrap.port())
        }
    };
    channel_tag.extend_from_slice(&bootstrap.port().to_be_bytes());
    let channel_tag = channel_tag.leak();
    let id = TcpClient::interface_id(channel_tag);
    let status = TCP_STATUS.init(EmbassyInterfaceStatus::new(
        id,
        ConnectionState::Initializing,
    ));
    Ok(E290PrnsTcp {
        interface: TcpClient::new(TcpClientInput {
            stack,
            target,
            channel_tag,
            bitrate: WIFI_EMBEDDED_BITRATE_CEILING_BPS,
            reconnect_policy: ReconnectPolicy::STANDARD,
            socket_buffers: TcpSocketBuffers {
                rx: allocate_bytes(4 * 1024)?,
                tx: allocate_bytes(4 * 1024)?,
            },
            status,
        }),
        status,
    })
}

/// Start the first PRNS-native E290 topology.
///
/// This composes LoRa plus the configured TCP and Bluetooth Auto lanes in one
/// manifold. PRNS's default engine protocol policy remains unchanged.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start(
    spawner: Spawner,
    radio: PrnsRadio,
    #[cfg(feature = "gateway")] tcp: Option<E290PrnsTcp>,
    enable_bluetooth_auto: bool,
    shared_flash: E290PrnsFlash,
    transport_identity: Zeroizing<[u8; IDENTITY_SECRET_KEY_LEN]>,
    destination_identity: &[u8; IDENTITY_SECRET_KEY_LEN],
    lora_profile: RadioProfile,
    lxmf_announce_app_data: &[u8],
    application_profile: ApplicationProfile,
) -> Result<StartedPrnsTarget, PrnsTargetStartError> {
    let id = LoRaInterface::<PrnsRadio>::interface_id(&lora_profile);
    let status = LORA_STATUS.init(EmbassyInterfaceStatus::new(
        id,
        ConnectionState::Initializing,
    ));
    let spectrum = LORA_SPECTRUM.init(LoRaSpectrumStatus::new());
    let lora = LoRaInterface::new(LoRaInterfaceInput {
        radio,
        profile: lora_profile,
        airtime_policy: E290_PRNS_AIRTIME_POLICY,
        tx_queue: allocate_tx_queue()?,
        control: &LORA_CONTROL,
        status,
        spectrum,
        lifecycle: LIFECYCLE.dyn_sender(),
    })
    .map_err(PrnsTargetStartError::RadioConfiguration)?;

    let descriptor = prns_internal_lora_descriptor(&lora_profile, E290_PRNS_AIRTIME_POLICY)
        .map_err(|_| PrnsTargetStartError::InterfacePolicy)?;
    debug_assert_eq!(descriptor.id, lora.id());
    let mut lanes = E290ManifoldLanes::new();
    let lora_lane = lanes
        .claim_interface_with_outbound_buffer(&LORA_MANIFOLD_LANE, descriptor, allocate_outbound()?)
        .map_err(PrnsTargetStartError::LaneClaim)?;
    #[cfg(feature = "gateway")]
    let tcp_status = tcp.as_ref().map(|tcp| tcp.status);
    #[cfg(not(feature = "gateway"))]
    let tcp_status = None;
    #[cfg(feature = "gateway")]
    let tcp = match tcp {
        Some(tcp) => {
            let mut descriptor = tcp.interface.descriptor();
            descriptor.mode = InterfaceMode::Boundary;
            let lane = lanes
                .claim_interface_with_outbound_buffer(
                    &TCP_MANIFOLD_LANE,
                    descriptor,
                    allocate_outbound()?,
                )
                .map_err(PrnsTargetStartError::LaneClaim)?;
            Some((tcp.interface, lane))
        }
        None => None,
    };
    let bluetooth_lane = if enable_bluetooth_auto {
        Some(
            lanes
                .claim_supervisor_with_outbound_buffer(
                    &BLUETOOTH_MANIFOLD_LANE,
                    BLUETOOTH_SUPERVISOR_ID,
                    &BLUETOOTH_OUTBOUND_WAKE,
                    allocate_outbound()?,
                )
                .map_err(PrnsTargetStartError::LaneClaim)?,
        )
    } else {
        None
    };

    let InitializedPrnsNode {
        handle,
        node,
        persistence,
        management,
        nomad,
        lxmf,
        rmap,
        management_requests,
    } = initialize_prns_node(
        lanes,
        shared_flash,
        transport_identity,
        destination_identity,
        lxmf_announce_app_data,
        application_profile,
    )?;

    let seam = lora_lane.into_seam(NOTIFY.sender(), hardware_entropy);
    spawner.spawn(
        manifold_task(node, persistence).expect("the single PRNS manifold task is available"),
    );
    spawner.spawn(lora_task(lora, seam).expect("the single PRNS LoRa task is available"));
    #[cfg(feature = "gateway")]
    let tcp_id = tcp.as_ref().map(|(interface, _)| interface.id());
    #[cfg(not(feature = "gateway"))]
    let tcp_id = None;
    #[cfg(feature = "gateway")]
    if let Some((interface, lane)) = tcp {
        let seam = lane.into_seam(NOTIFY.sender(), hardware_entropy);
        spawner.spawn(tcp_task(interface, seam).expect("the single PRNS TCP task is available"));
    }
    let bluetooth = bluetooth_lane.map(|lane| E290PrnsBluetooth {
        fleet: lane.into_fleet(NOTIFY.sender(), LIFECYCLE.sender()),
        shared: &BLUETOOTH_SHARED,
    });
    let bluetooth_status = bluetooth
        .as_ref()
        .map(|bluetooth| BluetoothAutoStatus::new(bluetooth.shared));

    Ok(StartedPrnsTarget {
        handle,
        management,
        nomad,
        lxmf,
        rmap,
        tcp: tcp_id,
        tcp_status,
        lora_status: status,
        bluetooth,
        bluetooth_status,
        management_requests,
        management_authorization_settlements: MANAGEMENT_AUTHORIZATION_SETTLEMENTS.receiver(),
        lxmf_outbound_settlements: LXMF_OUTBOUND_SETTLEMENTS.receiver(),
        ota_resource_strategy_settlements: OTA_RESOURCE_STRATEGY_SETTLEMENTS.receiver(),
        lxmf_deliveries: LXMF_DELIVERIES.receiver(),
        ota_resources: OTA_RESOURCES.receiver(),
    })
}

/// Read PRNS's latest engine-owned counts for one interface.
pub(crate) fn interface_counts(interface: InterfaceId) -> InterfaceCounts {
    INTERFACE_STORE.counts(interface)
}

#[embassy_executor::task]
async fn manifold_task(node: &'static mut E290PrnsNode, persistence: &'static mut E290Persistence) {
    let _ = node.restore_embedded_persistence(persistence).await;
    node.run_manifold_with_persistence_and_interface_store(&INTERFACE_STORE, persistence)
        .await;
}

#[embassy_executor::task]
async fn lora_task(interface: PrnsLoRaInterface, seam: PrnsLoRaSeam) {
    interface.run(seam).await;
}

#[cfg(feature = "gateway")]
#[embassy_executor::task]
async fn tcp_task(interface: PrnsTcpInterface, seam: PrnsTcpSeam) {
    interface.run(seam).await;
}
