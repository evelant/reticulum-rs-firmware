//! Standalone Reticulum appliance firmware for the Vision Master E290.
//!
//! The executable composes one transport-neutral node task with one concrete
//! LoRa actor task. Before either is constructed, the sole flash owner safely
//! provisions or strictly mounts the submission journal and drives an explicit
//! bounded-history recovery gate to completion. The backend-independent durable
//! runtime then remains resident with the sole operation-scoped flash
//! coordinator. BLE provides the authenticated local device API and GPIO21
//! physical presence without becoming a Reticulum packet interface. The
//! `gateway` profile adds Wi-Fi station and Reticulum TCP actors. Both supported
//! profiles emit lifecycle and fault logs over USB Serial/JTAG without exposing
//! an API bearer there. The node retains transport-neutral admission and
//! authenticated dispatch lanes; inbound durable LXMF delivery and NomadNet
//! remain independent application services.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave RF hardware active"
)]
#![deny(clippy::large_stack_frames)]

#[cfg(not(feature = "appliance"))]
compile_error!("select the supported `appliance` or `gateway` firmware profile");
#[cfg(feature = "appliance")]
mod ble_api_task;
#[cfg(feature = "display")]
mod display_task;
mod node_task;
mod platform_storage;
mod radio_task;
#[cfg(feature = "gateway")]
mod wifi_station_task;
#[cfg(feature = "gateway")]
mod wifi_tcp_task;

use core::future::pending;
use core::{future::Future, mem};

use allocator_api2::{boxed::Box, vec::Vec};
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Delay, Duration, Instant, Timer, with_timeout};
use embedded_hal::digital::{Error as DigitalError, ErrorKind, ErrorType};
use embedded_hal_async::digital::Wait;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_alloc::ExternalMemory;
use esp_backtrace as _;
use esp_hal::gpio::Pull;
use esp_hal::{
    Async,
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig},
    psram::{FlashFreq, Psram, PsramConfig, PsramMode, PsramSize, SpiRamFreq},
    rng::{Trng, TrngSource},
    spi::{
        Mode as SpiMode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_storage::FlashStorage;
use log::{error, info, warn};
use rand_core::RngCore;
#[cfg(feature = "display")]
use reticulum_appliance_display_model::{
    DisplayCommand, DisplayCompositionState, DisplayHomeSnapshot, DisplayLabel, DisplaySetupState,
    DisplayViewKind,
};
#[cfg(feature = "display")]
use reticulum_board_e290::EINK_SPI_FREQUENCY_HZ;
use reticulum_board_e290_radio::{E290Na915TxPower, E290Radio, E290RadioConfiguration};
use reticulum_device_api::{LxmfPeerDiscoveryIncarnation, RmapDeferredReason};
use reticulum_device_api_handoff::DeviceApiHandoff;
use reticulum_device_api_pairing::DeviceId;
#[cfg(feature = "appliance")]
use reticulum_device_api_session::SessionSuite;
use reticulum_device_api_session::{
    AuthenticatedGrant, BearerBinding as SessionBearerBinding, DeviceId as SessionDeviceId,
    ServerParameters,
};
use reticulum_device_identity_store::IdentityMirrorCoverage;
#[cfg(feature = "appliance")]
use reticulum_e290_firmware::ble_api_profile::{
    BLE_BOND_BOOT_RECOVERY_HOLD_MS, BleAdvertisingParameters, BootBleBondRecoveryGesture,
    BootBleBondRecoveryProgress,
};
#[cfg(feature = "appliance")]
use reticulum_e290_firmware::ble_bond_handoff::BleBondHandoff;
#[cfg(feature = "display")]
use reticulum_e290_firmware::display_handoff::{
    DisplayBootClearOutcome, DisplayCompletionGateError, DisplayHandoff, DisplayTelemetryHandoff,
};
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_station_profile::WifiStationStatusCell;
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_tcp_profile::{
    LoRaAndTcpAuthorizationPolicy, TCP_INTERFACE, WifiTcpPhase, WifiTcpStatus, WifiTcpStatusCell,
};
use reticulum_e290_firmware::{
    config,
    credential_boot::CredentialBootState,
    device_api_id_from_eui48,
    durability_boot::{announce_clock_policy, journal_boot_policy},
    live_pairing_handoff::LivePairingHandoff,
    lxmf_delivery::{LxmfDeliveryActivation, activate_lxmf_delivery},
    nomad_responder::activate_nomad_responder,
    pairing_control_handoff::PairingControlHandoff,
    radio_diagnostics::RadioDiagnosticsCell,
    rmap_discovery::{
        RmapDiscoveryStatusCell, RmapPublicationPolicy, activate_rmap_discovery_destination,
    },
    session_admission_handoff::SessionAdmissionHandoff,
    storage_device_id_from_eui48,
};
#[cfg(feature = "display")]
use reticulum_eink_ssd1680::{E290FrameBuffer, Ssd1680};
use reticulum_interface_router::InterfaceFabric;
use reticulum_lxmf_store::LxmfStoreIndexSlot;
use reticulum_node_core::{
    ApplicationEventOwner, ApplicationEventSlot, DelayedProofOwner, DelayedProofSlot,
    InboundProofPolicy, NodeConfig, NodeIdentity, NodeInstanceId, OrdinaryActionOwner,
    OrdinaryBufferPool, OrdinaryPacketBuffer, PacketInterfaceId, TxPacketBuffer,
};
use reticulum_radio_lora_phy::IrqTimestampCapture;
use reticulum_radio_tx_dispatch::{ExactLoRaAirtimePolicy, SoleRadioTxDispatcher};
use reticulum_rns_interface_discovery::{
    DEFAULT_STAMP_COST, DiscoveryStampSearch, LocationE6, RnodeDiscoveryInfo, encode_rnode_info,
};
use reticulum_tx_handoff::{
    AuthorizedFrameHandoff, DataPairedPermitHandoff, DataPermitHandoff,
    OrdinaryPairedPermitHandoff, OrdinaryPermitHandoff,
};
use reticulum_tx_supervisor::{
    DataRouterCoordinator, NodeInterfaceActorPorts, NodeInterfaceSupervisor,
    NodeInterfaceSupervisorInit, OrdinaryRouterCoordinator, RouteDiagnosticsSnapshot,
};
use static_cell::StaticCell;

use crate::platform_storage::{
    BootCredentialStore, BootNetworkConfigStore, ProductCredentialInitializationPort,
    ProductFlashOwner, ProductStorageCoordinator, ProductSubmissionRuntime,
};

#[cfg(debug_assertions)]
compile_error!("the permanent E290 node must be built with --release");

const LORA_INTERFACE: PacketInterfaceId = PacketInterfaceId::new(1);
#[cfg(feature = "display")]
const DISPLAY_BOOT_CLEAR_DEADLINE_MS: u64 = 15_000;
#[cfg(feature = "display")]
const DISPLAY_HOME_DEADLINE_MS: u64 = 15_000;
#[cfg(feature = "appliance")]
const BLE_BOND_BOOT_RECOVERY_SAMPLE_MS: u64 = 20;

type E290SpiDevice = ExclusiveDevice<Spi<'static, Async>, Output<'static>, Delay>;
type ProductRadio =
    E290Radio<E290SpiDevice, Output<'static>, Input<'static>, BoundedBusy<Input<'static>>, Delay>;

#[cfg(feature = "appliance")]
async fn observe_boot_ble_bond_recovery(
    pairing_button: &Input<'_>,
    first_boot_sample_asserted: bool,
) -> bool {
    let started_at_ms = Instant::now().as_millis();
    let mut gesture = BootBleBondRecoveryGesture::new(started_at_ms, first_boot_sample_asserted);

    loop {
        match gesture.observe(Instant::now().as_millis(), pairing_button.is_low()) {
            BootBleBondRecoveryProgress::Pending => {
                Timer::after(Duration::from_millis(BLE_BOND_BOOT_RECOVERY_SAMPLE_MS)).await;
            }
            BootBleBondRecoveryProgress::Authorized => return true,
            BootBleBondRecoveryProgress::Rejected | BootBleBondRecoveryProgress::Consumed => {
                return false;
            }
        }
    }
}

#[cfg(feature = "appliance")]
const _: () = assert!(BLE_BOND_BOOT_RECOVERY_SAMPLE_MS < BLE_BOND_BOOT_RECOVERY_HOLD_MS);

pub(crate) type ProductDispatcher = SoleRadioTxDispatcher<
    ProductRadio,
    Trng,
    CriticalSectionRawMutex,
    { config::INTERFACE_QUEUE_DEPTH },
>;

pub(crate) type ProductSupervisor = NodeInterfaceSupervisor<
    CriticalSectionRawMutex,
    ProductTxAuthorizationPolicy,
    { config::PATHS },
    { config::ANNOUNCES },
    { config::DEDUPLICATION },
    { config::LINKS },
    { config::DATA_BUFFERS },
    { config::ORDINARY_BUFFERS },
    { config::INTERFACE_SLOTS },
    { config::INTERFACE_QUEUE_DEPTH },
>;

type ProductSupervisorInit<'a> = NodeInterfaceSupervisorInit<
    'a,
    CriticalSectionRawMutex,
    ProductTxAuthorizationPolicy,
    { config::PATHS },
    { config::ANNOUNCES },
    { config::DEDUPLICATION },
    { config::LINKS },
    { config::DATA_BUFFERS },
    { config::ORDINARY_BUFFERS },
    { config::INTERFACE_SLOTS },
    { config::INTERFACE_QUEUE_DEPTH },
>;

#[cfg(not(feature = "gateway"))]
type ProductTxAuthorizationPolicy = ExactLoRaAirtimePolicy;
#[cfg(feature = "gateway")]
type ProductTxAuthorizationPolicy = LoRaAndTcpAuthorizationPolicy;

#[cfg(feature = "display")]
#[inline(never)]
fn allocate_display_frame()
-> Result<Box<E290FrameBuffer, ExternalMemory>, allocator_api2::alloc::AllocError> {
    Box::try_new_in(E290FrameBuffer::new_white(), ExternalMemory)
}

#[cfg(feature = "display")]
fn display_device_suffix(base_mac: [u8; 6]) -> DisplayLabel {
    let local_name = reticulum_device_api_ble::local_name(base_mac);
    DisplayLabel::from_bytes(&local_name[reticulum_device_api_ble::LOCAL_NAME_PREFIX.len()..])
        .expect("the fixed BLE discovery suffix is valid display text")
}

const RMAP_DISCOVERY_NAME_PREFIX: &[u8] = b"Metalbeard E290 ";
const RMAP_DISCOVERY_NAME_BYTES: usize =
    RMAP_DISCOVERY_NAME_PREFIX.len() + reticulum_device_api_ble::LOCAL_NAME_SUFFIX_BYTES * 2;

fn rmap_discovery_name(base_mac: [u8; 6]) -> [u8; RMAP_DISCOVERY_NAME_BYTES] {
    let local_name = reticulum_device_api_ble::local_name(base_mac);
    let suffix = &local_name[reticulum_device_api_ble::LOCAL_NAME_PREFIX.len()..];
    let mut name = [0_u8; RMAP_DISCOVERY_NAME_BYTES];
    name[..RMAP_DISCOVERY_NAME_PREFIX.len()].copy_from_slice(RMAP_DISCOVERY_NAME_PREFIX);
    name[RMAP_DISCOVERY_NAME_PREFIX.len()..].copy_from_slice(suffix);
    name
}

fn rmap_discovery_location(location: reticulum_network_config_store::PhoneLocation) -> LocationE6 {
    LocationE6::new(location.latitude_e6(), location.longitude_e6())
        .expect("the durable network store validates public coordinates")
}

static IRQ_TIMESTAMPS: IrqTimestampCapture = IrqTimestampCapture::new(monotonic_us);
static INTERFACE_FABRIC: StaticCell<
    InterfaceFabric<
        CriticalSectionRawMutex,
        { config::INTERFACE_SLOTS },
        { config::INTERFACE_QUEUE_DEPTH },
    >,
> = StaticCell::new();
static DATA_PERMIT: StaticCell<DataPermitHandoff<CriticalSectionRawMutex>> = StaticCell::new();
static ORDINARY_PERMIT: StaticCell<OrdinaryPermitHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
#[cfg(feature = "gateway")]
static TCP_DATA_PERMIT: StaticCell<DataPermitHandoff<CriticalSectionRawMutex>> = StaticCell::new();
#[cfg(feature = "gateway")]
static TCP_ORDINARY_PERMIT: StaticCell<OrdinaryPermitHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
static AUTHORIZED_FRAME: StaticCell<AuthorizedFrameHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
static DATA_PACKET_STORAGE: StaticCell<[TxPacketBuffer; config::DATA_BUFFERS]> = StaticCell::new();
static ORDINARY_PACKET_STORAGE: StaticCell<[OrdinaryPacketBuffer; config::ORDINARY_BUFFERS]> =
    StaticCell::new();
static APPLICATION_EVENT_STORAGE: StaticCell<
    [ApplicationEventSlot; config::APPLICATION_EVENT_SLOTS],
> = StaticCell::new();
static DISPATCHER: StaticCell<ProductDispatcher> = StaticCell::new();
static RADIO_DIAGNOSTICS: StaticCell<RadioDiagnosticsCell> = StaticCell::new();
static ROUTE_DIAGNOSTICS: StaticCell<RouteDiagnosticsSnapshot<{ config::PATHS }>> =
    StaticCell::new();
static FLASH_STORAGE: StaticCell<FlashStorage<'static>> = StaticCell::new();
static STORAGE_COORDINATOR: StaticCell<ProductStorageCoordinator> = StaticCell::new();
static PAIRING_CONTROL: StaticCell<PairingControlHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
static LIVE_PAIRING: StaticCell<LivePairingHandoff<CriticalSectionRawMutex>> = StaticCell::new();
#[cfg(feature = "appliance")]
static BLE_BOND: StaticCell<BleBondHandoff<CriticalSectionRawMutex>> = StaticCell::new();
static SESSION_ADMISSION: StaticCell<SessionAdmissionHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
static AUTHENTICATED_API: StaticCell<
    DeviceApiHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
> = StaticCell::new();
#[cfg(feature = "gateway")]
static WIFI_STATION_STATUS: StaticCell<WifiStationStatusCell> = StaticCell::new();
#[cfg(feature = "gateway")]
static WIFI_TCP_STATUS: StaticCell<WifiTcpStatusCell> = StaticCell::new();
static RMAP_DISCOVERY_STATUS: StaticCell<RmapDiscoveryStatusCell> = StaticCell::new();
#[cfg(feature = "display")]
static DISPLAY_HANDOFF: StaticCell<DisplayHandoff<CriticalSectionRawMutex>> = StaticCell::new();
#[cfg(feature = "display")]
static DISPLAY_TELEMETRY_HANDOFF: StaticCell<DisplayTelemetryHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
const _: () = assert!(
    mem::size_of::<DeviceApiHandoff<CriticalSectionRawMutex, AuthenticatedGrant>>()
        <= config::MAXIMUM_AUTHENTICATED_API_HANDOFF_BYTES
);
static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();

#[cfg(feature = "appliance")]
pub(crate) struct UsbSerialJtagDiagnosticsOwner {
    _usb_device: esp_hal::peripherals::USB_DEVICE<'static>,
}

#[cfg(feature = "appliance")]
impl UsbSerialJtagDiagnosticsOwner {
    const fn new(usb_device: esp_hal::peripherals::USB_DEVICE<'static>) -> Self {
        Self {
            _usb_device: usb_device,
        }
    }
}

esp_bootloader_esp_idf::esp_app_desc!();

#[derive(Debug)]
enum BoundedBusyError<E> {
    Pin(E),
    Timeout,
}

impl<E: DigitalError> DigitalError for BoundedBusyError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Pin(error) => error.kind(),
            Self::Timeout => ErrorKind::Other,
        }
    }
}

struct BoundedBusy<Pin> {
    pin: Pin,
}

impl<Pin> BoundedBusy<Pin> {
    const fn new(pin: Pin) -> Self {
        Self { pin }
    }
}

impl<Pin> ErrorType for BoundedBusy<Pin>
where
    Pin: ErrorType,
    Pin::Error: DigitalError,
{
    type Error = BoundedBusyError<Pin::Error>;
}

async fn wait_for_busy<F, E>(future: F) -> Result<(), BoundedBusyError<E>>
where
    F: Future<Output = Result<(), E>>,
    E: DigitalError,
{
    match with_timeout(Duration::from_millis(config::BUSY_PIN_WATCHDOG_MS), future).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(BoundedBusyError::Pin(error)),
        Err(_) => Err(BoundedBusyError::Timeout),
    }
}

impl<Pin> Wait for BoundedBusy<Pin>
where
    Pin: Wait,
    Pin::Error: DigitalError,
{
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        wait_for_busy(self.pin.wait_for_high()).await
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        wait_for_busy(self.pin.wait_for_low()).await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_busy(self.pin.wait_for_rising_edge()).await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_busy(self.pin.wait_for_falling_edge()).await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_busy(self.pin.wait_for_any_edge()).await
    }
}

#[inline(never)]
fn construct_node_or_inert(
    identity: NodeIdentity,
    instance: NodeInstanceId,
    psram_start_address: usize,
    psram_end_address: usize,
) -> ProductSupervisorInit<'static> {
    let supervisor_storage = match Box::<ProductSupervisor, ExternalMemory>::try_new_uninit_in(
        ExternalMemory,
    ) {
        Ok(storage) => storage,
        Err(_) => {
            error!(
                "e290-node stage=supervisor-placement status=FAIL reason=external-allocation expected_bytes={}",
                mem::size_of::<ProductSupervisor>(),
            );
            inert_large_construction()
        }
    };
    let supervisor_bytes = mem::size_of::<ProductSupervisor>();
    let supervisor_start = supervisor_storage.as_ptr() as usize;
    let supervisor_end = match supervisor_start.checked_add(supervisor_bytes) {
        Some(end) => end,
        None => {
            error!(
                "e290-node stage=supervisor-placement status=FAIL reason=allocation-range-overflow"
            );
            inert_large_construction()
        }
    };
    if !supervisor_start.is_multiple_of(mem::align_of::<ProductSupervisor>())
        || supervisor_start < psram_start_address
        || supervisor_end > psram_end_address
    {
        error!(
            "e290-node stage=supervisor-placement status=FAIL reason=external-address allocation_start=0x{supervisor_start:08x} allocation_end=0x{supervisor_end:08x} psram_start=0x{psram_start_address:08x} psram_end=0x{psram_end_address:08x} expected_bytes={supervisor_bytes} alignment={}",
            mem::align_of::<ProductSupervisor>(),
        );
        inert_large_construction()
    }
    let supervisor_storage = Box::leak(supervisor_storage);
    let stage = match ProductSupervisor::begin_node_in(
        supervisor_storage,
        identity,
        config::RNS_APPLICATION_NAME,
        &config::RNS_PRIMARY_ASPECTS,
        instance,
        NodeConfig::transport(),
    ) {
        Ok(stage) => stage,
        Err(reason) => {
            error!("e290-node stage=node status=FAIL reason={reason:?}");
            inert_large_construction()
        }
    };
    info!(
        "e290-node stage=supervisor-placement status=PASS ownership=boot-lifetime-external bytes={supervisor_bytes} start=0x{supervisor_start:08x} end=0x{supervisor_end:08x} construction=in-place"
    );
    stage
}

#[inline(never)]
fn construct_ordinary_coordinator_or_inert(
    owner: OrdinaryActionOwner<{ config::ORDINARY_BUFFERS }>,
    pool: OrdinaryBufferPool<'static, { config::ORDINARY_BUFFERS }>,
) -> OrdinaryRouterCoordinator<{ config::ORDINARY_BUFFERS }> {
    match OrdinaryRouterCoordinator::try_new(owner, pool, config::ordinary_router_config()) {
        Ok(coordinator) => coordinator,
        Err(failure) => {
            error!(
                "e290-node stage=ordinary-coordinator status=FAIL reason={:?}",
                failure.reason()
            );
            inert_large_construction()
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
// Finish the resident supervisor at its already-validated PSRAM address in
// this isolated frame. No complete node or supervisor crosses this boundary.
fn construct_supervisor_or_inert(
    stage: ProductSupervisorInit<'static>,
    fabric: &'static mut InterfaceFabric<
        CriticalSectionRawMutex,
        { config::INTERFACE_SLOTS },
        { config::INTERFACE_QUEUE_DEPTH },
    >,
    data: DataRouterCoordinator<{ config::DATA_BUFFERS }>,
    ordinary: OrdinaryRouterCoordinator<{ config::ORDINARY_BUFFERS }>,
    data_permits: [DataPairedPermitHandoff<CriticalSectionRawMutex>; config::INTERFACE_SLOTS],
    ordinary_permits: [OrdinaryPairedPermitHandoff<CriticalSectionRawMutex>;
        config::INTERFACE_SLOTS],
    policy: ProductTxAuthorizationPolicy,
) -> (
    &'static mut ProductSupervisor,
    [NodeInterfaceActorPorts<CriticalSectionRawMutex, { config::INTERFACE_QUEUE_DEPTH }>;
        config::INTERFACE_SLOTS],
) {
    let (supervisor, actors) = match stage.finish(
        fabric,
        data,
        ordinary,
        data_permits,
        ordinary_permits,
        policy,
    ) {
        Ok(success) => success,
        Err(reason) => {
            error!("e290-node stage=supervisor status=FAIL reason={:?}", reason);
            inert_large_construction()
        }
    };
    (supervisor, actors)
}

#[esp_hal::main]
fn main() -> ! {
    let executor = EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run(|spawner| {
        spawner.spawn(
            product_main(spawner)
                .expect("the sole product composition task pool must be available"),
        );
    })
}

#[allow(
    clippy::large_stack_frames,
    reason = "the linked ELF audits both large startup call paths before this image may be flashed"
)]
#[embassy_executor::task]
async fn product_main(spawner: Spawner) -> ! {
    let hal = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(hal);
    #[cfg(feature = "appliance")]
    let usb_serial_jtag_diagnostics_owner =
        UsbSerialJtagDiagnosticsOwner::new(peripherals.USB_DEVICE);

    // Establish the complete E290 RF interlock at the composition task's first
    // poll, before logging, entropy, PSRAM initialization or radio construction.
    let radio_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let radio_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    // USB Serial/JTAG is reserved for diagnostics. It is not a framed
    // device-API bearer.
    esp_println::logger::init_logger(log::LevelFilter::Info);
    info!(
        "e290-node stage=usb-logging status=READY backend=usb-serial-jtag level=info secrets=redacted"
    );
    let pairing_button = Input::new(
        peripherals.GPIO21,
        InputConfig::default().with_pull(Pull::Up),
    );
    #[cfg(feature = "appliance")]
    let boot_ble_bond_first_sample_asserted = pairing_button.is_low();
    {
        let base_mac = esp_hal::efuse::base_mac_address();
        let base_mac_bytes = base_mac.as_bytes();
        let base_mac_eui48 = [
            base_mac_bytes[0],
            base_mac_bytes[1],
            base_mac_bytes[2],
            base_mac_bytes[3],
            base_mac_bytes[4],
            base_mac_bytes[5],
        ];
        let storage_device_id = storage_device_id_from_eui48(base_mac_eui48);
        let device_api_id = match DeviceId::new(device_api_id_from_eui48(base_mac_eui48)) {
            Ok(device_api_id) => device_api_id,
            Err(reason) => {
                error!("e290-node stage=device-api-id status=FAIL reason={reason:?}");
                inert_before_rtos()
            }
        };
        #[cfg(feature = "appliance")]
        let local_api_session_parameters = ServerParameters::new_for_suite(
            SessionDeviceId::new(device_api_id_from_eui48(base_mac_eui48)),
            SessionBearerBinding::BleGatt,
            SessionSuite::BleGatt,
        );
        info!(
            "e290-node stage=boot base_mac={} identity=pending-durable radio_constructed=false rf_state=reset_low_nss_high",
            base_mac
        );

        let psram_config = PsramConfig {
            mode: PsramMode::Auto,
            size: PsramSize::AutoDetect,
            core_clock: None,
            flash_frequency: FlashFreq::FlashFreq40m,
            ram_frequency: SpiRamFreq::Freq40m,
        };
        let psram = Psram::new(peripherals.PSRAM, psram_config);
        let (psram_start, psram_bytes) = psram.raw_parts();
        let psram_start_address = psram_start as usize;
        let psram_end_address = match psram_start_address.checked_add(psram_bytes) {
            Some(end) => end,
            None => {
                error!("e290-node stage=psram status=FAIL reason=mapped-range-overflow");
                inert_before_rtos()
            }
        };
        if !(config::MINIMUM_PSRAM_BYTES..=config::MAXIMUM_PSRAM_BYTES).contains(&psram_bytes) {
            error!(
                "e290-node stage=psram status=FAIL expected_bytes={}..={} actual_bytes={psram_bytes}",
                config::MINIMUM_PSRAM_BYTES,
                config::MAXIMUM_PSRAM_BYTES,
            );
            inert_before_rtos()
        }
        esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: config::INTERNAL_HEAP_BYTES);
        #[cfg(feature = "gateway")]
        esp_alloc::heap_allocator!(size: config::WIFI_INTERNAL_HEAP_BYTES);
        esp_alloc::psram_allocator!(&psram);
        info!(
            "e290-node stage=psram status=PASS bytes={psram_bytes} mode=auto minimum_qualified_bytes={} internal_heap_bytes={} wifi_internal_heap_bytes={} ownership_state=external-allocator-ready",
            config::MINIMUM_PSRAM_BYTES,
            config::INTERNAL_HEAP_BYTES,
            config::WIFI_INTERNAL_HEAP_BYTES,
        );

        let timers = TimerGroup::new(peripherals.TIMG0);
        let software_interrupts =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);
        #[cfg(feature = "appliance")]
        let boot_ble_bond_recovery_authorized =
            observe_boot_ble_bond_recovery(&pairing_button, boot_ble_bond_first_sample_asserted)
                .await;

        #[cfg(feature = "appliance")]
        let ble_connector = match ble_api_task::initialize_controller(peripherals.BT) {
            Ok(connector) => Some(connector),
            Err(reason) => {
                error!(
                    "e290-node stage=ble-api status=DISABLED reason=controller-init-{reason:?} lora_routing=continue"
                );
                None
            }
        };

        #[cfg(feature = "display")]
        let (display_publisher, display_telemetry_publisher, display_task_spawned) = {
            let (mut publisher, receiver) = DISPLAY_HANDOFF.init(DisplayHandoff::new()).split();
            let (telemetry_publisher, telemetry_receiver) = DISPLAY_TELEMETRY_HANDOFF
                .init(DisplayTelemetryHandoff::new())
                .split();
            let display_task = 'display: {
                let frame = match allocate_display_frame() {
                    Ok(frame) => frame,
                    Err(_) => {
                        error!(
                            "e290-node stage=display-placement status=FAIL reason=external-allocation expected_bytes={}",
                            mem::size_of::<E290FrameBuffer>(),
                        );
                        break 'display None;
                    }
                };
                let frame_bytes = mem::size_of_val(&*frame);
                let frame_start = (&*frame as *const E290FrameBuffer) as usize;
                let frame_end = match frame_start.checked_add(frame_bytes) {
                    Some(end) => end,
                    None => {
                        error!(
                            "e290-node stage=display-placement status=FAIL reason=allocation-range-overflow"
                        );
                        break 'display None;
                    }
                };
                if frame_bytes != mem::size_of::<E290FrameBuffer>()
                    || !frame_start.is_multiple_of(mem::align_of::<E290FrameBuffer>())
                    || frame_start < psram_start_address
                    || frame_end > psram_end_address
                {
                    error!(
                        "e290-node stage=display-placement status=FAIL reason=external-address allocation_start=0x{frame_start:08x} allocation_end=0x{frame_end:08x} psram_start=0x{psram_start_address:08x} psram_end=0x{psram_end_address:08x} expected_bytes={} actual_bytes={frame_bytes} alignment={}",
                        mem::size_of::<E290FrameBuffer>(),
                        mem::align_of::<E290FrameBuffer>(),
                    );
                    break 'display None;
                }
                info!(
                    "e290-node stage=display-placement status=PASS ownership=boot-lifetime-external bytes={frame_bytes} start=0x{frame_start:08x} end=0x{frame_end:08x}"
                );

                let display_power =
                    Output::new(peripherals.GPIO18, Level::Low, OutputConfig::default());
                let chip_select =
                    Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
                let data_command =
                    Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
                let reset = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
                let busy = display_task::DisplayBusy::new(Input::new(
                    peripherals.GPIO6,
                    InputConfig::default(),
                ));
                let spi = match Spi::new(
                    peripherals.SPI3,
                    SpiConfig::default()
                        .with_frequency(Rate::from_hz(EINK_SPI_FREQUENCY_HZ))
                        .with_mode(SpiMode::_0),
                ) {
                    Ok(spi) => spi,
                    Err(_) => {
                        error!("e290-node stage=display-spi status=FAIL lora_routing=continue");
                        break 'display None;
                    }
                }
                .with_sck(peripherals.GPIO2)
                .with_mosi(peripherals.GPIO1)
                .into_async();
                let spi_device = match ExclusiveDevice::new(spi, chip_select, Delay) {
                    Ok(device) => device,
                    Err(_) => {
                        error!(
                            "e290-node stage=display-spi-device status=FAIL lora_routing=continue"
                        );
                        break 'display None;
                    }
                };
                let display = Ssd1680::new(spi_device, data_command, reset, busy, Delay);
                let task = match display_task::run(
                    display,
                    display_power,
                    frame,
                    receiver,
                    telemetry_receiver,
                ) {
                    Ok(task) => task,
                    Err(_) => {
                        error!(
                            "e290-node stage=display-spawn status=FAIL reason=task-pool lora_routing=continue"
                        );
                        break 'display None;
                    }
                };
                Some(task)
            };

            let display_task_spawned = display_task.is_some();
            let display_publisher = match display_task {
                Some(task) => {
                    let booting = DisplayCommand::ShowBooting {
                        label: DisplayLabel::new("Reticulum E290")
                            .expect("the fixed product label fits"),
                    };
                    if publisher.publish_latest(booting).is_err() {
                        error!(
                            "e290-node stage=display-request status=FAIL reason=request-id-exhausted lora_routing=continue"
                        );
                        None
                    } else {
                        spawner.spawn(task);
                        match with_timeout(
                            Duration::from_millis(DISPLAY_BOOT_CLEAR_DEADLINE_MS),
                            publisher.wait_for_boot_clear(),
                        )
                        .await
                        {
                            Ok(DisplayBootClearOutcome::Ready) => Some(publisher),
                            Ok(DisplayBootClearOutcome::Faulted) => {
                                error!(
                                    "e290-node stage=display-readiness status=DISABLED reason=boot-clear-fault fresh_pairing=false lora_routing=continue"
                                );
                                None
                            }
                            Err(_) => {
                                error!(
                                    "e290-node stage=display-readiness status=DISABLED reason=boot-clear-deadline fresh_pairing=false lora_routing=continue"
                                );
                                None
                            }
                        }
                    }
                }
                None => None,
            };
            (
                display_publisher,
                display_task_spawned.then_some(telemetry_publisher),
                display_task_spawned,
            )
        };
        #[cfg(not(feature = "display"))]
        let display_task_spawned = false;

        let application_volatile = match Box::try_new_in(
            node_task::ApplicationVolatileState::new(),
            ExternalMemory,
        ) {
            Ok(state) => state,
            Err(_) => {
                error!(
                    "e290-node stage=application-volatile status=FAIL reason=external-allocation expected_bytes={}",
                    mem::size_of::<node_task::ApplicationVolatileState>(),
                );
                inert_forever().await
            }
        };
        let application_volatile_bytes = mem::size_of_val(&*application_volatile);
        let application_volatile_start =
            (&*application_volatile as *const node_task::ApplicationVolatileState) as usize;
        let application_volatile_end = match application_volatile_start
            .checked_add(application_volatile_bytes)
        {
            Some(end) => end,
            None => {
                error!(
                    "e290-node stage=application-volatile status=FAIL reason=allocation-range-overflow"
                );
                inert_forever().await
            }
        };
        if application_volatile_bytes != mem::size_of::<node_task::ApplicationVolatileState>()
            || !application_volatile_start
                .is_multiple_of(mem::align_of::<node_task::ApplicationVolatileState>())
            || application_volatile_start < psram_start_address
            || application_volatile_end > psram_end_address
        {
            error!(
                "e290-node stage=application-volatile status=FAIL reason=external-address allocation_start=0x{application_volatile_start:08x} allocation_end=0x{application_volatile_end:08x} psram_start=0x{psram_start_address:08x} psram_end=0x{psram_end_address:08x} expected_bytes={} actual_bytes={application_volatile_bytes} alignment={}",
                mem::size_of::<node_task::ApplicationVolatileState>(),
                mem::align_of::<node_task::ApplicationVolatileState>(),
            );
            inert_forever().await
        }
        info!(
            "e290-node stage=application-volatile status=PASS ownership=boot-lifetime-external bytes={application_volatile_bytes} start=0x{application_volatile_start:08x} end=0x{application_volatile_end:08x} nomad=resident"
        );
        let application_volatile: &'static mut node_task::ApplicationVolatileState =
            Box::leak(application_volatile);

        let mut delayed_proof_storage = Vec::new_in(ExternalMemory);
        if delayed_proof_storage
            .try_reserve_exact(config::LXMF_DELAYED_PROOF_SLOTS)
            .is_err()
        {
            error!(
                "e290-node stage=lxmf-delayed-proofs status=FAIL reason=external-allocation slots={} expected_bytes={}",
                config::LXMF_DELAYED_PROOF_SLOTS,
                config::LXMF_DELAYED_PROOF_STORAGE_BYTES,
            );
            inert_forever().await
        }
        if delayed_proof_storage.capacity() < config::LXMF_DELAYED_PROOF_SLOTS {
            error!(
                "e290-node stage=lxmf-delayed-proofs status=FAIL reason=allocation-capacity expected_slots={} actual_slots={}",
                config::LXMF_DELAYED_PROOF_SLOTS,
                delayed_proof_storage.capacity(),
            );
            inert_forever().await
        }
        for _ in 0..config::LXMF_DELAYED_PROOF_SLOTS {
            if delayed_proof_storage
                .push_within_capacity(DelayedProofSlot::new())
                .is_err()
            {
                error!(
                    "e290-node stage=lxmf-delayed-proofs status=FAIL reason=initialization-capacity expected_slots={} initialized_slots={}",
                    config::LXMF_DELAYED_PROOF_SLOTS,
                    delayed_proof_storage.len(),
                );
                inert_forever().await
            }
        }
        let delayed_proof_storage_bytes = mem::size_of_val(delayed_proof_storage.as_slice());
        let delayed_proof_storage_start = delayed_proof_storage.as_ptr() as usize;
        let delayed_proof_storage_end = match delayed_proof_storage_start
            .checked_add(delayed_proof_storage_bytes)
        {
            Some(end) => end,
            None => {
                error!(
                    "e290-node stage=lxmf-delayed-proofs status=FAIL reason=allocation-range-overflow"
                );
                inert_forever().await
            }
        };
        if delayed_proof_storage.len() != config::LXMF_DELAYED_PROOF_SLOTS
            || delayed_proof_storage_bytes != config::LXMF_DELAYED_PROOF_STORAGE_BYTES
        {
            error!(
                "e290-node stage=lxmf-delayed-proofs status=FAIL reason=initialized-size expected_slots={} actual_slots={} expected_bytes={} actual_bytes={delayed_proof_storage_bytes}",
                config::LXMF_DELAYED_PROOF_SLOTS,
                delayed_proof_storage.len(),
                config::LXMF_DELAYED_PROOF_STORAGE_BYTES,
            );
            inert_forever().await
        }
        if !delayed_proof_storage_start.is_multiple_of(mem::align_of::<DelayedProofSlot>())
            || delayed_proof_storage_start < psram_start_address
            || delayed_proof_storage_end > psram_end_address
        {
            error!(
                "e290-node stage=lxmf-delayed-proofs status=FAIL reason=external-address allocation_start=0x{delayed_proof_storage_start:08x} allocation_end=0x{delayed_proof_storage_end:08x} psram_start=0x{psram_start_address:08x} psram_end=0x{psram_end_address:08x} alignment={}",
                mem::align_of::<DelayedProofSlot>(),
            );
            inert_forever().await
        }
        info!(
            "e290-node stage=lxmf-delayed-proofs status=PASS ownership=boot-lifetime-external slots={} reserved_slots={} bytes={delayed_proof_storage_bytes} start=0x{delayed_proof_storage_start:08x} end=0x{delayed_proof_storage_end:08x}",
            config::LXMF_DELAYED_PROOF_SLOTS,
            delayed_proof_storage.capacity(),
        );
        let delayed_proof_storage: &'static mut [DelayedProofSlot] = delayed_proof_storage.leak();

        let mut lxmf_index = Vec::new_in(ExternalMemory);
        if lxmf_index
            .try_reserve_exact(config::LXMF_INDEX_SLOTS)
            .is_err()
        {
            error!(
                "e290-node stage=lxmf-index status=FAIL reason=external-allocation slots={} expected_bytes={}",
                config::LXMF_INDEX_SLOTS,
                config::LXMF_INDEX_STORAGE_BYTES,
            );
            inert_forever().await
        }
        if lxmf_index.capacity() < config::LXMF_INDEX_SLOTS {
            error!(
                "e290-node stage=lxmf-index status=FAIL reason=allocation-capacity expected_slots={} actual_slots={}",
                config::LXMF_INDEX_SLOTS,
                lxmf_index.capacity(),
            );
            inert_forever().await
        }
        for _ in 0..config::LXMF_INDEX_SLOTS {
            if lxmf_index
                .push_within_capacity(LxmfStoreIndexSlot::new())
                .is_err()
            {
                error!(
                    "e290-node stage=lxmf-index status=FAIL reason=initialization-capacity expected_slots={} initialized_slots={}",
                    config::LXMF_INDEX_SLOTS,
                    lxmf_index.len(),
                );
                inert_forever().await
            }
        }
        let lxmf_index_bytes = mem::size_of_val(lxmf_index.as_slice());
        let lxmf_index_start = lxmf_index.as_ptr() as usize;
        let lxmf_index_end = match lxmf_index_start.checked_add(lxmf_index_bytes) {
            Some(end) => end,
            None => {
                error!("e290-node stage=lxmf-index status=FAIL reason=allocation-range-overflow");
                inert_forever().await
            }
        };
        if lxmf_index.len() != config::LXMF_INDEX_SLOTS
            || lxmf_index_bytes != config::LXMF_INDEX_STORAGE_BYTES
        {
            error!(
                "e290-node stage=lxmf-index status=FAIL reason=initialized-size expected_slots={} actual_slots={} expected_bytes={} actual_bytes={lxmf_index_bytes}",
                config::LXMF_INDEX_SLOTS,
                lxmf_index.len(),
                config::LXMF_INDEX_STORAGE_BYTES,
            );
            inert_forever().await
        }
        if !lxmf_index_start.is_multiple_of(mem::align_of::<LxmfStoreIndexSlot>())
            || lxmf_index_start < psram_start_address
            || lxmf_index_end > psram_end_address
        {
            error!(
                "e290-node stage=lxmf-index status=FAIL reason=external-address allocation_start=0x{lxmf_index_start:08x} allocation_end=0x{lxmf_index_end:08x} psram_start=0x{psram_start_address:08x} psram_end=0x{psram_end_address:08x} alignment={}",
                mem::align_of::<LxmfStoreIndexSlot>(),
            );
            inert_forever().await
        }
        info!(
            "e290-node stage=lxmf-index status=PASS ownership=boot-lifetime-external slots={} reserved_slots={} bytes={lxmf_index_bytes} start=0x{lxmf_index_start:08x} end=0x{lxmf_index_end:08x}",
            config::LXMF_INDEX_SLOTS,
            lxmf_index.capacity(),
        );
        let lxmf_index: &'static mut [LxmfStoreIndexSlot] = lxmf_index.leak();
        let _entropy_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
        let mut bootstrap_rng = match Trng::try_new() {
            Ok(rng) => rng,
            Err(reason) => {
                error!("e290-node stage=entropy status=FAIL reason={reason:?}");
                inert_forever().await
            }
        };
        let flash = FLASH_STORAGE.init(FlashStorage::new(peripherals.FLASH));
        let mut flash_owner = match ProductFlashOwner::open(flash, storage_device_id) {
            Ok(owner) => owner,
            Err(reason) => {
                error!("e290-node stage=flash-owner status=FAIL reason={reason}");
                inert_forever().await
            }
        };
        info!(
            "e290-node stage=flash-owner status=PASS partition_contract=validated api_credentials=0x614000..0x616000 credential_store=bound credential_media=plaintext"
        );
        let credential_boot = flash_owner.boot_credentials();
        log_credential_boot(&credential_boot);
        #[cfg(feature = "appliance")]
        let boot_ble_bond = match flash_owner.boot_ble_bond() {
            Ok(boot) if boot_ble_bond_recovery_authorized => {
                match flash_owner.clear_boot_ble_bond_for_physical_recovery(boot) {
                    Ok(recovered) => {
                        let generation = recovered.generation();
                        let cleanup = recovered.cleanup();
                        let raw_write_calls = recovered.raw_write_calls();
                        let raw_erase_calls = recovered.raw_erase_calls();
                        info!(
                            "e290-node stage=ble-bond-physical-recovery status=PASS generation={generation:?} bond_present=false cleanup={cleanup:?} raw_writes={raw_write_calls} raw_erases={raw_erase_calls} preserved=identity,api-credentials,network-config,messages,journal"
                        );
                        Ok(recovered)
                    }
                    Err(reason) => {
                        error!(
                            "e290-node stage=ble-bond-physical-recovery status=DISABLED reason={reason:?} ble_bearer=closed lora_routing=continue"
                        );
                        Err(reason)
                    }
                }
            }
            outcome => outcome,
        };
        #[cfg(feature = "appliance")]
        let (restored_ble_bond, boot_ble_bond_store_available) = match boot_ble_bond {
            Ok(boot) => {
                let generation = boot.generation();
                let bond_present = boot.bond_present();
                let cleanup = boot.cleanup();
                let raw_write_calls = boot.raw_write_calls();
                let raw_erase_calls = boot.raw_erase_calls();
                info!(
                    "e290-node stage=ble-bond-mount status=PASS generation={generation:?} bond_present={bond_present} cleanup={cleanup:?} raw_writes={raw_write_calls} raw_erases={raw_erase_calls}"
                );
                (boot.into_bond(), true)
            }
            Err(reason) => {
                error!(
                    "e290-node stage=ble-bond-mount status=DISABLED reason={reason:?} ble_bearer=closed lora_routing=continue"
                );
                (None, false)
            }
        };
        let network_config_binding = flash_owner.network_config_binding();
        let network_config_boot = match flash_owner.boot_network_config() {
            Ok(boot) => {
                let fresh_erased = boot.is_fresh_erased();
                let generation = boot.generation();
                let wifi_profiles = boot.wifi_profile_count();
                let tcp_peer_configured = boot.tcp_peer_configured();
                let raw_write_calls = boot.raw_write_calls();
                let raw_erase_calls = boot.raw_erase_calls();
                info!(
                    "e290-node stage=network-config-mount status=PASS fresh_erased={fresh_erased} generation={generation:?} wifi_profiles={wifi_profiles} tcp_peer_configured={tcp_peer_configured} raw_writes={raw_write_calls} raw_erases={raw_erase_calls}"
                );
                boot
            }
            Err(reason) => {
                error!(
                    "e290-node stage=network-config-mount status=DISABLED reason={reason:?} wifi_station=closed lora_routing=continue ble_management=continue"
                );
                BootNetworkConfigStore::unavailable(network_config_binding)
            }
        };
        let identity_preflight = match flash_owner.inspect_identity() {
            Ok(preflight) => preflight,
            Err(reason) => {
                error!("e290-node stage=identity-preflight status=FAIL reason={reason:?}");
                inert_forever().await
            }
        };
        let fresh_clock_policy = announce_clock_policy(identity_preflight);
        let journal_policy = journal_boot_policy(identity_preflight);
        info!(
            "e290-node stage=identity-preflight status=PASS state={identity_preflight:?} announce_clock_policy={fresh_clock_policy:?} journal_policy={journal_policy:?} writes=0 erases=0"
        );
        let journal_provision = flash_owner.provision_node_journal(journal_policy);
        match journal_provision {
            Ok(Some(report)) => info!(
                "e290-node stage=node-journal-provision status=PASS policy={journal_policy:?} bank={:?} generation={} records={} accepted={} writes={} erases={}",
                report.state.bank(),
                report.state.generation(),
                report.state.committed_records(),
                report.state.accepted_submissions(),
                report.raw_write_calls,
                report.raw_erase_calls,
            ),
            Ok(None) => info!(
                "e290-node stage=node-journal-provision status=SKIPPED policy={journal_policy:?} action=strict-mount-only writes=0 erases=0"
            ),
            Err(reason) => {
                error!(
                    "e290-node stage=node-journal-provision status=FAIL policy={journal_policy:?} reason={reason:?}"
                );
                inert_forever().await
            }
        }
        let boot_epoch_result = flash_owner.reserve_announce_epoch(fresh_clock_policy);
        let boot_epoch = match boot_epoch_result {
            Ok(epoch) => epoch,
            Err(reason) => {
                error!("e290-node stage=announce-clock status=FAIL reason={reason:?}");
                inert_forever().await
            }
        };
        let announce_epoch = boot_epoch.reservation.epoch();
        let clock_report = boot_epoch.reservation.report();
        let previous_epoch = clock_report.previous_high_water().map(|epoch| epoch.get());
        info!(
            "e290-node stage=announce-clock status=PASS epoch={} previous_epoch={previous_epoch:?} sector_a={:?} sector_b={:?} writes={} erases={}",
            announce_epoch.get(),
            clock_report.sector_a(),
            clock_report.sector_b(),
            boot_epoch.raw_write_calls,
            boot_epoch.raw_erase_calls,
        );
        let boot_identity_result = flash_owner.boot_identity(&mut bootstrap_rng);
        let boot_identity = match boot_identity_result {
            Ok(identity) => identity,
            Err(reason) => {
                error!("e290-node stage=identity-store status=FAIL reason={reason:?}");
                inert_forever().await
            }
        };
        if boot_identity.report.coverage() != IdentityMirrorCoverage::Redundant {
            error!(
                "e290-node stage=identity-store status=FAIL reason=identity-repair-deferred source={:?} coverage={:?} writes={} erases={}",
                boot_identity.report.source(),
                boot_identity.report.coverage(),
                boot_identity.raw_write_calls,
                boot_identity.raw_erase_calls,
            );
            inert_forever().await
        }
        let identity = match NodeIdentity::from_private_key(boot_identity.material.as_bytes()) {
            Ok(identity) => identity,
            Err(reason) => {
                error!("e290-node stage=identity-import status=FAIL reason={reason:?}");
                inert_forever().await
            }
        };
        let identity_hash = identity.identity_hash();
        info!(
            "e290-node stage=identity-store status=PASS source={:?} coverage={:?} repair_deferred={} writes={} erases={} identity_hash={identity_hash:02x?} plaintext=true",
            boot_identity.report.source(),
            boot_identity.report.coverage(),
            boot_identity.report.repair_deferred(),
            boot_identity.raw_write_calls,
            boot_identity.raw_erase_calls,
        );
        drop(boot_identity);

        let submission_runtime_storage = match Box::<ProductSubmissionRuntime, _>::try_new_uninit_in(
            ExternalMemory,
        ) {
            Ok(storage) => Some(storage),
            Err(_) => {
                error!(
                    "e290-node stage=submission-runtime-placement status=DISABLED reason=external-allocation expected_bytes={} lora_routing=continue local_submission_admission=closed",
                    mem::size_of::<ProductSubmissionRuntime>(),
                );
                None
            }
        };
        #[allow(
            clippy::manual_map,
            reason = "Option::map merges the external-runtime mount into this async frame and regresses the reviewed compiler-emitted stack ceiling"
        )]
        let journal_mount_result = match submission_runtime_storage {
            Some(storage) => {
                Some(flash_owner.mount_node_runtime(u64::from(announce_epoch.get()), storage))
            }
            None => None,
        };
        let submission_runtime = match journal_mount_result {
            Some(Ok((runtime, journal_report))) => {
                info!(
                    "e290-node stage=node-journal-recovery status=PASS boot_sequence={} profile=bounded-live-admission accepted_limit={} bank={:?} generation={} records={} accepted={} replayed={} queued={} pending_lxmf={} already_final={} finalized={} writes={} erases={} service_gate=open flash_owner=resident",
                    announce_epoch.get(),
                    config::DURABLE_ACCEPTED_SUBMISSION_LIMIT,
                    journal_report.state.bank(),
                    journal_report.state.generation(),
                    journal_report.state.committed_records(),
                    journal_report.state.accepted_submissions(),
                    journal_report.replayed_submissions,
                    journal_report.queued_submissions,
                    journal_report.pending_lxmf_submissions,
                    journal_report.already_final_submissions,
                    journal_report.finalized_submissions,
                    journal_report.raw_write_calls,
                    journal_report.raw_erase_calls,
                );
                Some(runtime)
            }
            Some(Err(reason)) => {
                error!(
                    "e290-node stage=node-journal-recovery status=DISABLED boot_sequence={} phase={} reason={reason:?} lora_routing=continue local_submission_admission=closed",
                    announce_epoch.get(),
                    reason.stage(),
                );
                None
            }
            None => None,
        };
        let submission_runtime = match submission_runtime {
            Some(runtime) => {
                let runtime_bytes = mem::size_of_val(&*runtime);
                let runtime_start = (&*runtime as *const ProductSubmissionRuntime) as usize;
                let runtime_end = match runtime_start.checked_add(runtime_bytes) {
                    Some(end) => end,
                    None => {
                        error!(
                            "e290-node stage=submission-runtime-placement status=FAIL reason=allocation-range-overflow"
                        );
                        inert_forever().await
                    }
                };
                if runtime_bytes != mem::size_of::<ProductSubmissionRuntime>()
                    || !runtime_start.is_multiple_of(mem::align_of::<ProductSubmissionRuntime>())
                    || runtime_start < psram_start_address
                    || runtime_end > psram_end_address
                {
                    error!(
                        "e290-node stage=submission-runtime-placement status=FAIL reason=external-address allocation_start=0x{runtime_start:08x} allocation_end=0x{runtime_end:08x} psram_start=0x{psram_start_address:08x} psram_end=0x{psram_end_address:08x} expected_bytes={} actual_bytes={runtime_bytes} alignment={}",
                        mem::size_of::<ProductSubmissionRuntime>(),
                        mem::align_of::<ProductSubmissionRuntime>(),
                    );
                    inert_forever().await
                }
                info!(
                    "e290-node stage=submission-runtime-placement status=PASS ownership=boot-lifetime-external bytes={runtime_bytes} start=0x{runtime_start:08x} end=0x{runtime_end:08x}"
                );
                Some(Box::leak(runtime))
            }
            None => None,
        };
        let lxmf = match flash_owner.mount_lxmf(lxmf_index) {
            Ok(lxmf) => {
                info!(
                    "e290-node stage=lxmf-store-mount status=PASS profile=mounted index_slots={} messages={} consumed_extents={} partition=0x730000..0xb30000 plaintext=true writes=0 erases=0 destination_activation=pending-node-construction lora_routing=continue",
                    config::LXMF_INDEX_SLOTS,
                    lxmf.message_count(),
                    lxmf.consumed_extents(),
                );
                Some(lxmf)
            }
            Err(reason) => {
                error!(
                    "e290-node stage=lxmf-store-mount status=DISABLED reason={reason:?} lora_routing=continue destination_activation=disabled"
                );
                None
            }
        };
        #[cfg(feature = "gateway")]
        let wifi_station_status: &'static WifiStationStatusCell =
            WIFI_STATION_STATUS.init(WifiStationStatusCell::new());
        #[cfg(feature = "gateway")]
        let wifi_tcp_status: &'static WifiTcpStatusCell =
            WIFI_TCP_STATUS.init(WifiTcpStatusCell::new());
        let rmap_discovery_status: &'static RmapDiscoveryStatusCell =
            RMAP_DISCOVERY_STATUS.init(RmapDiscoveryStatusCell::new());
        let storage_coordinator = flash_owner.into_storage_coordinator(
            submission_runtime,
            lxmf,
            credential_boot,
            network_config_boot,
            device_api_id,
            #[cfg(feature = "gateway")]
            wifi_station_status,
            #[cfg(feature = "gateway")]
            wifi_tcp_status,
            rmap_discovery_status,
        );
        let storage_service_available = storage_coordinator.submission_service_available();
        let lxmf_service_available = storage_coordinator.lxmf_service_available();
        let network_config_store_available = storage_coordinator.network_config_store_available();
        let network_config_generation = storage_coordinator.network_config_generation();
        let lora_profile = storage_coordinator.lora_radio_profile();
        let radio_configuration = E290RadioConfiguration::try_from_profile(
            lora_profile.frequency_hz(),
            lora_profile.bandwidth_hz(),
            lora_profile.spreading_factor(),
            lora_profile.coding_rate_denominator(),
            E290Na915TxPower::try_from_dbm(lora_profile.tx_power().requested_dbm())
                .expect("the persisted LoRa power set is identical to the E290 board policy"),
        )
        .expect("the product adapter commits only board-valid LoRa profiles");
        let radio_task_timing = config::RadioTaskTiming::for_configuration(radio_configuration);
        let radio_diagnostics =
            RADIO_DIAGNOSTICS.init_with(|| RadioDiagnosticsCell::new(radio_configuration));
        radio_diagnostics.set_trace_boot_sequence(u64::from(announce_epoch.get()));
        let route_diagnostics = ROUTE_DIAGNOSTICS.init(RouteDiagnosticsSnapshot::empty());
        let automatic_announces_enabled = storage_coordinator.automatic_announces_enabled();
        let rmap_discovery_enabled = storage_coordinator.rmap_discovery_enabled();
        let rmap_public_location = storage_coordinator.rmap_public_location();
        #[cfg(feature = "gateway")]
        let wifi_tcp_bootstrap = storage_coordinator.wifi_tcp_bootstrap();
        let rmap_publication_policy = {
            #[cfg(feature = "gateway")]
            {
                if wifi_tcp_bootstrap.is_some() {
                    RmapPublicationPolicy::require_interface(TCP_INTERFACE)
                } else {
                    RmapPublicationPolicy::immediate()
                }
            }
            #[cfg(not(feature = "gateway"))]
            {
                RmapPublicationPolicy::immediate()
            }
        };
        #[cfg(feature = "gateway")]
        wifi_tcp_status.publish(match wifi_tcp_bootstrap {
            Some(bootstrap) => {
                WifiTcpStatus::for_bootstrap(bootstrap, WifiTcpPhase::WaitingForNetwork)
            }
            None => WifiTcpStatus {
                applied_revision: network_config_generation.unwrap_or(0),
                phase: WifiTcpPhase::Disabled,
                last_failure: None,
                dns_diagnostics: None,
            },
        });
        let network_config_wifi_profiles = storage_coordinator.network_config_wifi_profile_count();
        let network_config_tcp_peer_configured =
            storage_coordinator.network_config_tcp_peer_configured();
        #[cfg(feature = "appliance")]
        let durable_ble_bond_store_available = storage_coordinator.ble_bond_store_available();
        #[cfg(feature = "appliance")]
        let durable_ble_bond_present = storage_coordinator.ble_bond_present();
        #[cfg(feature = "appliance")]
        let durable_ble_bond_generation = storage_coordinator.ble_bond_generation();
        let credential_boot_state = storage_coordinator.credential_boot_state();
        let credential_binding = storage_coordinator.credential_binding();
        let credential_revision = storage_coordinator.credential_revision();
        #[cfg(feature = "display")]
        let active_credential_count = storage_coordinator.active_credential_count();
        let credential_authority_publishable =
            storage_coordinator.credential_authority_publishable();
        let credential_mutation_eligible = storage_coordinator.credential_mutation_eligible();
        let credential_pairing_policy_available =
            storage_coordinator.credential_pairing_policy_available();
        let credential_initialization_status = storage_coordinator.initialization_status();
        info!(
            "e290-node stage=network-config status=READY store_available={network_config_store_available} generation={network_config_generation:?} wifi_profiles={network_config_wifi_profiles} tcp_peer_configured={network_config_tcp_peer_configured} automatic_announces={automatic_announces_enabled} rmap_discovery={rmap_discovery_enabled} rmap_public_location={} lora_frequency_hz={} lora_bandwidth_hz={} lora_sf={} lora_cr=4/{} lora_tx_power_dbm={} apply_policy=reboot-required",
            rmap_public_location.is_some(),
            radio_configuration.profile().frequency_hz(),
            radio_configuration.profile().bandwidth().hz(),
            radio_configuration.profile().spreading_factor().factor(),
            radio_configuration.profile().coding_rate().denom(),
            radio_configuration.requested_power_dbm(),
        );
        #[cfg(feature = "gateway")]
        let wifi_station_boot_plan = storage_coordinator.wifi_station_boot_plan();
        let storage_coordinator = STORAGE_COORDINATOR.init(storage_coordinator);

        let node_rng = bootstrap_rng.clone();
        let radio_rng = bootstrap_rng.clone();
        let local_api_session_rng = bootstrap_rng.clone();
        #[cfg(feature = "gateway")]
        let wifi_network_seed =
            (u64::from(bootstrap_rng.next_u32()) << 32) | u64::from(bootstrap_rng.next_u32());
        let mut instance_bytes = [0_u8; 16];
        bootstrap_rng.fill_bytes(&mut instance_bytes);
        let mut peer_discovery_incarnation_bytes = [0_u8; 8];
        peer_discovery_incarnation_bytes.copy_from_slice(&instance_bytes[..8]);
        let peer_discovery_incarnation =
            LxmfPeerDiscoveryIncarnation::new(peer_discovery_incarnation_bytes);
        peer_discovery_incarnation_bytes.fill(0);
        let instance = NodeInstanceId::new(instance_bytes);
        instance_bytes.fill(0);

        // Complete the only fallible asynchronous physical-radio construction
        // before beginning the transport-neutral node graph in final PSRAM.
        // The in-place stage remains pointer-sized across the later display
        // completion await and never enters product_main's Embassy task frame
        // as a by-value NodeCore or supervisor.
        let spi = match Spi::new(
            peripherals.SPI2,
            SpiConfig::default()
                .with_frequency(Rate::from_hz(config::SPI_FREQUENCY_HZ))
                .with_mode(SpiMode::_0),
        ) {
            Ok(spi) => spi,
            Err(_) => {
                radio_diagnostics.mark_faulted();
                error!("e290-node stage=spi status=FAIL");
                inert_forever().await
            }
        }
        .with_sck(peripherals.GPIO9)
        .with_mosi(peripherals.GPIO10)
        .with_miso(peripherals.GPIO11)
        .into_async();
        let spi_device = match ExclusiveDevice::new(spi, radio_nss, Delay) {
            Ok(device) => device,
            Err(_) => {
                radio_diagnostics.mark_faulted();
                error!("e290-node stage=spi-device status=FAIL");
                inert_forever().await
            }
        };
        let dio1 = Input::new(peripherals.GPIO14, InputConfig::default());
        let busy = BoundedBusy::new(Input::new(peripherals.GPIO13, InputConfig::default()));
        let radio = match E290Radio::new(
            spi_device,
            radio_reset,
            dio1,
            busy,
            Delay,
            &IRQ_TIMESTAMPS,
            radio_configuration,
        )
        .await
        {
            Ok(radio) => radio,
            Err(fault) => {
                radio_diagnostics.mark_faulted();
                error!(
                    "e290-node stage=radio-init status=FAIL operation={:?} radio={:?}",
                    fault.operation, fault.radio,
                );
                inert_forever().await
            }
        };

        let mut supervisor_stage =
            construct_node_or_inert(identity, instance, psram_start_address, psram_end_address);
        let node = supervisor_stage.node_mut();
        node.set_inbound_proof_policy(InboundProofPolicy::Always);
        let primary_destination = node.destination_hash();
        if let Err(reason) = node.set_destination_accepts_links(&primary_destination, false) {
            error!(
                "e290-node stage=node-link-policy status=FAIL destination=primary reason={reason:?} action=pre-task-construction-fail-stop"
            );
            inert_forever().await
        }
        let lxmf_destination = match activate_lxmf_delivery(node, lxmf_service_available) {
            Ok(LxmfDeliveryActivation::Active(destination)) => {
                info!(
                    "e290-node stage=lxmf-delivery status=ENABLED destination={:02x?} proof_policy=retain durability=required-for-all-carriers accepts_links=true data_profile=opportunistic+responder-direct-link resource_ingress=disabled discovery_announce=periodic interfaces=transport-neutral",
                    destination.as_bytes(),
                );
                Some(destination)
            }
            Ok(LxmfDeliveryActivation::Disabled) => {
                warn!(
                    "e290-node stage=lxmf-delivery status=DISABLED reason=store-unavailable destination_registered=false proof_policy=none lora_routing=continue"
                );
                None
            }
            Err(reason) => {
                error!(
                    "e290-node stage=lxmf-delivery status=FAIL reason={reason:?} action=pre-task-construction-fail-stop"
                );
                inert_forever().await
            }
        };
        let lxmf_delivery_admission = if lxmf_destination.is_some() {
            "enabled"
        } else {
            "disabled"
        };
        let nomad_destination = match activate_nomad_responder(node) {
            Ok(destination) => {
                info!(
                    "e290-node stage=nomad-responder status=ENABLED destination={:02x?} accepts_links=true proof_policy=never page=/page/index.mu response_profile=single-packet-static-micron resource_ingress=disabled discovery_announce=periodic interfaces=transport-neutral",
                    destination.as_bytes(),
                );
                destination
            }
            Err(reason) => {
                error!(
                    "e290-node stage=nomad-responder status=FAIL reason={reason:?} action=pre-task-construction-fail-stop"
                );
                inert_forever().await
            }
        };
        let proof_probe_destination = match node.register_proof_probe_destination() {
            Ok(destination) => {
                info!(
                    "e290-node stage=reticulum-proof-probe status=ENABLED destination={:02x?} expanded_name=rnstransport.probe accepts_links=false proof_policy=always state=volatile-one-in-flight interfaces=transport-neutral",
                    destination.as_bytes(),
                );
                destination
            }
            Err(reason) => {
                error!(
                    "e290-node stage=reticulum-proof-probe status=FAIL reason={reason:?} action=pre-task-construction-fail-stop"
                );
                inert_forever().await
            }
        };
        let rmap_destination = if rmap_discovery_enabled {
            match activate_rmap_discovery_destination(node) {
                Ok(destination) => {
                    if let Some(interface) = rmap_publication_policy.initial_interface() {
                        node.set_local_announce_interface_target(&destination, interface)
                            .expect(
                                "the newly registered RMAP destination remains available for exact targeting",
                            );
                    }
                    let name_bytes = rmap_discovery_name(base_mac_eui48);
                    let name = core::str::from_utf8(&name_bytes)
                        .expect("the fixed RMAP interface name is ASCII");
                    let location = rmap_public_location.map(rmap_discovery_location);
                    match RnodeDiscoveryInfo::new(
                        identity_hash,
                        name,
                        true,
                        location,
                        radio_configuration.profile().frequency_hz(),
                        radio_configuration.profile().bandwidth().hz(),
                        u8::try_from(radio_configuration.profile().spreading_factor().factor())
                            .expect("the validated E290 spreading factor fits u8"),
                        u8::try_from(radio_configuration.profile().coding_rate().denom())
                            .expect("the validated E290 coding-rate denominator fits u8"),
                    ) {
                        Ok(info) => match encode_rnode_info(info) {
                            Ok(packed) => {
                                match DiscoveryStampSearch::new(&packed, DEFAULT_STAMP_COST) {
                                    Ok(search) => {
                                        application_volatile.configure_rmap(
                                            packed,
                                            search,
                                            rmap_discovery_status,
                                            rmap_publication_policy,
                                        );
                                        info!(
                                            "e290-node stage=rmap-discovery status=ENABLED destination={:02x?} transport_id={identity_hash:02x?} name={name} public_location={} stamp_cost={DEFAULT_STAMP_COST} interval_hours=6",
                                            destination.as_bytes(),
                                            location.is_some(),
                                        );
                                        Some(destination)
                                    }
                                    Err(reason) => {
                                        rmap_discovery_status.publish_activation_failure(
                                            rmap_publication_policy,
                                            RmapDeferredReason::StampInitializationFailed,
                                        );
                                        warn!(
                                            "e290-node stage=rmap-discovery status=DISABLED reason=stamp-initialization-{reason:?} lora_routing=continue"
                                        );
                                        None
                                    }
                                }
                            }
                            Err(reason) => {
                                rmap_discovery_status.publish_activation_failure(
                                    rmap_publication_policy,
                                    RmapDeferredReason::PayloadEncodingFailed,
                                );
                                warn!(
                                    "e290-node stage=rmap-discovery status=DISABLED reason=payload-encoding-{reason:?} lora_routing=continue"
                                );
                                None
                            }
                        },
                        Err(reason) => {
                            rmap_discovery_status.publish_activation_failure(
                                rmap_publication_policy,
                                RmapDeferredReason::DiscoveryModelInvalid,
                            );
                            warn!(
                                "e290-node stage=rmap-discovery status=DISABLED reason=model-{reason:?} lora_routing=continue"
                            );
                            None
                        }
                    }
                }
                Err(reason) => {
                    rmap_discovery_status.publish_activation_failure(
                        rmap_publication_policy,
                        RmapDeferredReason::DestinationActivationFailed,
                    );
                    warn!(
                        "e290-node stage=rmap-discovery status=DISABLED reason=destination-{reason:?} lora_routing=continue"
                    );
                    None
                }
            }
        } else {
            info!(
                "e290-node stage=rmap-discovery status=DISABLED reason=durable-policy lora_routing=continue"
            );
            None
        };

        #[cfg(all(feature = "display", feature = "appliance"))]
        let ble_display_available = ble_connector.is_some() && boot_ble_bond_store_available;
        #[cfg(feature = "display")]
        let display_home = {
            let application_setup = DisplaySetupState::from_application_state(
                active_credential_count,
                credential_pairing_policy_available,
            );
            #[cfg(feature = "appliance")]
            let ble = if ble_display_available {
                DisplayCompositionState::Configured
            } else {
                DisplayCompositionState::Unavailable
            };
            #[cfg(not(feature = "appliance"))]
            let ble = DisplayCompositionState::Unavailable;
            #[cfg(feature = "appliance")]
            let setup =
                application_setup.with_ble_bond(ble_display_available, durable_ble_bond_present);
            #[cfg(not(feature = "appliance"))]
            let setup = application_setup;
            DisplayHomeSnapshot::new(
                DisplayLabel::new("Reticulum E290").expect("the fixed product label fits"),
                display_device_suffix(base_mac_eui48),
                setup,
                DisplayCompositionState::Configured,
                ble,
                if lxmf_destination.is_some() {
                    DisplayCompositionState::Configured
                } else {
                    DisplayCompositionState::Unavailable
                },
                DisplayCompositionState::Configured,
            )
            .with_uncollected_messages(
                storage_coordinator
                    .lxmf_mailbox_status()
                    .map_or(0, |status| status.uncollected_count()),
            )
        };
        // Finish the initial Home render after the physical
        // radio and application destinations are ready, but before constructing
        // the larger supervisor graph. Secure BLE onboarding then receives the
        // sole verified publisher and a Copy of this non-secret snapshot without
        // racing the startup completion gate.
        #[cfg(feature = "display")]
        let display_publisher = match display_publisher {
            Some(mut publisher) => {
                let home_request = publisher.publish_latest(DisplayCommand::ShowHome {
                    snapshot: display_home,
                });
                match home_request {
                    Ok(request_id) => {
                        info!(
                            "e290-node stage=display-request status=QUEUED request={} view=Home",
                            request_id.sequence(),
                        );
                        match with_timeout(
                            Duration::from_millis(DISPLAY_HOME_DEADLINE_MS),
                            publisher
                                .wait_for_rendered_completion(request_id, DisplayViewKind::Home),
                        )
                        .await
                        {
                            Ok(Ok(completion)) => {
                                info!(
                                    "e290-node stage=display-home status=PASS request={} view={:?} outcome={:?}",
                                    completion.request_id().sequence(),
                                    completion.view(),
                                    completion.outcome(),
                                );
                                Some(publisher)
                            }
                            Ok(Err(error)) => {
                                match error {
                                    DisplayCompletionGateError::Superseded { .. } => {}
                                    DisplayCompletionGateError::ViewMismatch { .. } => {}
                                    DisplayCompletionGateError::Faulted { .. } => {}
                                }
                                error!(
                                    "e290-node stage=display-home status=DISABLED reason=completion-gate error={error:?} fresh_pairing=false lora_routing=continue"
                                );
                                None
                            }
                            Err(_) => {
                                error!(
                                    "e290-node stage=display-home status=DISABLED reason=completion-timeout request={} deadline_ms={} fresh_pairing=false lora_routing=continue",
                                    request_id.sequence(),
                                    DISPLAY_HOME_DEADLINE_MS,
                                );
                                None
                            }
                        }
                    }
                    Err(_) => {
                        error!(
                            "e290-node stage=display-request status=FAIL reason=request-id-exhausted fresh_pairing=false lora_routing=continue"
                        );
                        None
                    }
                }
            }
            None => None,
        };
        #[cfg(feature = "display")]
        let display_available = display_publisher.is_some();
        #[cfg(not(feature = "display"))]
        let display_available = false;

        let mut data_buffers = DATA_PACKET_STORAGE
            .init([const { TxPacketBuffer::new() }; config::DATA_BUFFERS])
            .each_mut();
        for buffer in data_buffers.iter_mut() {
            if let Err(reason) = node.register_packet_buffer(buffer) {
                error!("e290-node stage=data-buffer status=FAIL reason={reason:?}");
                inert_large_construction()
            }
        }
        let data = match DataRouterCoordinator::try_new(
            node,
            data_buffers,
            config::data_router_config(),
        ) {
            Ok(coordinator) => coordinator,
            Err(failure) => {
                error!(
                    "e290-node stage=data-coordinator status=FAIL reason={:?}",
                    failure.reason()
                );
                inert_large_construction()
            }
        };

        let mut ordinary_owner =
            match node.take_ordinary_action_owner::<{ config::ORDINARY_BUFFERS }>() {
                Ok(owner) => owner,
                Err(reason) => {
                    error!("e290-node stage=ordinary-owner status=FAIL reason={reason:?}");
                    inert_large_construction()
                }
            };
        let ordinary_buffers = ORDINARY_PACKET_STORAGE
            .init([const { OrdinaryPacketBuffer::new() }; config::ORDINARY_BUFFERS])
            .each_mut();
        let mut ordinary_pool = OrdinaryBufferPool::new();
        for buffer in ordinary_buffers {
            if let Err(failure) = ordinary_owner.register_and_park(&mut ordinary_pool, buffer) {
                error!(
                    "e290-node stage=ordinary-buffer status=FAIL reason={:?}",
                    failure.reason()
                );
                inert_large_construction()
            }
        }
        let ordinary = construct_ordinary_coordinator_or_inert(ordinary_owner, ordinary_pool);

        let fabric = INTERFACE_FABRIC.init(InterfaceFabric::new());
        let lora_data_pair = DATA_PERMIT.init(DataPermitHandoff::new()).split_paired();
        let lora_ordinary_pair = ORDINARY_PERMIT
            .init(OrdinaryPermitHandoff::new())
            .split_paired();
        #[cfg(feature = "gateway")]
        let tcp_data_pair = TCP_DATA_PERMIT
            .init(DataPermitHandoff::new())
            .split_paired();
        #[cfg(feature = "gateway")]
        let tcp_ordinary_pair = TCP_ORDINARY_PERMIT
            .init(OrdinaryPermitHandoff::new())
            .split_paired();
        let (frame_node, frame_dispatcher) =
            AUTHORIZED_FRAME.init(AuthorizedFrameHandoff::new()).split();
        let lora_policy = ExactLoRaAirtimePolicy::new(
            LORA_INTERFACE,
            radio_configuration.profile(),
            radio_configuration.fingerprint(),
        );
        #[cfg(not(feature = "gateway"))]
        let policy = lora_policy;
        #[cfg(feature = "gateway")]
        let policy = LoRaAndTcpAuthorizationPolicy::new(lora_policy);
        #[cfg(not(feature = "gateway"))]
        let data_permits = [lora_data_pair];
        #[cfg(feature = "gateway")]
        let data_permits = [lora_data_pair, tcp_data_pair];
        #[cfg(not(feature = "gateway"))]
        let ordinary_permits = [lora_ordinary_pair];
        #[cfg(feature = "gateway")]
        let ordinary_permits = [lora_ordinary_pair, tcp_ordinary_pair];
        let (supervisor, actors) = construct_supervisor_or_inert(
            supervisor_stage,
            fabric,
            data,
            ordinary,
            data_permits,
            ordinary_permits,
            policy,
        );
        #[cfg(not(feature = "gateway"))]
        let [lora_actor] = actors;
        #[cfg(feature = "gateway")]
        let [lora_actor, tcp_actor] = actors;
        let offline_descriptor = match supervisor.register_interface(
            lora_actor.queue_id(),
            LORA_INTERFACE,
            config::interface_properties(radio_configuration.profile()),
        ) {
            Ok(descriptor) => descriptor,
            Err(reason) => {
                error!("e290-node stage=interface-register status=FAIL reason={reason:?}");
                inert_large_construction()
            }
        };
        let (interface, data_permit, ordinary_permit) = lora_actor.into_parts();
        let (tx_interface, ingress, lifecycle) = interface.into_parts();
        let ingress_authority = match ingress.bind_ingress(offline_descriptor) {
            Ok(authority) => authority,
            Err(reason) => {
                error!("e290-node stage=interface-bind status=FAIL reason={reason:?}");
                inert_large_construction()
            }
        };
        #[cfg(feature = "gateway")]
        let tcp_offline_descriptor = match supervisor.register_interface(
            tcp_actor.queue_id(),
            TCP_INTERFACE,
            config::tcp_interface_properties(),
        ) {
            Ok(descriptor) => descriptor,
            Err(reason) => {
                error!(
                    "e290-node stage=interface-register status=FAIL interface=2 reason={reason:?}"
                );
                inert_large_construction()
            }
        };
        #[cfg(feature = "gateway")]
        let wifi_tcp_actor_ports =
            match wifi_tcp_task::WifiTcpActorPorts::new(tcp_actor, tcp_offline_descriptor) {
                Ok(ports) => ports,
                Err(reason) => {
                    error!(
                        "e290-node stage=interface-bind status=FAIL interface=2 reason={reason:?}"
                    );
                    inert_large_construction()
                }
            };
        let dispatcher = DISPATCHER.init(SoleRadioTxDispatcher::new(
            radio,
            radio_rng,
            tx_interface,
            data_permit,
            ordinary_permit,
            frame_dispatcher,
            config::dispatcher_config(radio_configuration),
        ));
        let application_event_storage = APPLICATION_EVENT_STORAGE
            .init([const { ApplicationEventSlot::new() }; config::APPLICATION_EVENT_SLOTS]);
        let application_events = ApplicationEventOwner::new(application_event_storage);
        let delayed_proofs = DelayedProofOwner::new(delayed_proof_storage);
        let (bearer_pairing_handoff, node_pairing_handoff) =
            PAIRING_CONTROL.init(PairingControlHandoff::new()).split();
        let (bearer_live_pairing_handoff, node_live_pairing_handoff) =
            LIVE_PAIRING.init(LivePairingHandoff::new()).split();
        #[cfg(feature = "appliance")]
        let (ble_bond_handoff, node_ble_bond_handoff) =
            BLE_BOND.init(BleBondHandoff::new()).split();
        let (bearer_session_admission, node_session_admission) = SESSION_ADMISSION
            .init(SessionAdmissionHandoff::new())
            .split();
        let (bearer_authenticated_api, node_authenticated_api) =
            AUTHENTICATED_API.init(DeviceApiHandoff::new()).split();
        let radio_task = match radio_task::run(
            dispatcher,
            radio_task_timing,
            radio_diagnostics,
            ingress,
            lifecycle,
            ingress_authority,
        ) {
            Ok(task) => task,
            Err(_) => {
                error!("e290-node stage=spawn status=FAIL task=lora");
                inert_large_construction()
            }
        };
        let node_task = match node_task::run(
            supervisor,
            storage_coordinator,
            node_task::NodeDiagnosticsOwners::new(
                radio_diagnostics,
                route_diagnostics,
                radio_configuration.profile(),
            ),
            application_events,
            delayed_proofs,
            application_volatile,
            node_task::NodeApplicationDestinations::new(
                lxmf_destination,
                nomad_destination,
                proof_probe_destination,
                rmap_destination,
            ),
            rmap_publication_policy,
            automatic_announces_enabled,
            peer_discovery_incarnation,
            node_task::NodeHandoffs::new(
                node_pairing_handoff,
                node_live_pairing_handoff,
                #[cfg(feature = "appliance")]
                node_ble_bond_handoff,
                node_session_admission,
                node_authenticated_api,
                frame_node,
                #[cfg(feature = "display")]
                display_telemetry_publisher,
            ),
            offline_descriptor,
            announce_epoch,
            node_rng,
        ) {
            Ok(task) => task,
            Err(_) => {
                error!("e290-node stage=spawn status=FAIL task=node");
                inert_large_construction()
            }
        };
        #[cfg(feature = "gateway")]
        let wifi_station_stack = wifi_station_task::start(
            spawner,
            peripherals.WIFI,
            wifi_network_seed,
            wifi_station_boot_plan,
            wifi_station_status,
        );
        #[cfg(feature = "gateway")]
        let wifi_station_task_available = wifi_station_stack.is_some();
        #[cfg(feature = "gateway")]
        let wifi_tcp_task = match wifi_tcp_task::run(
            wifi_station_stack,
            wifi_tcp_bootstrap,
            wifi_tcp_status,
            wifi_tcp_actor_ports,
        ) {
            Ok(task) => Some(task),
            Err(_) => {
                if let Some(bootstrap) = wifi_tcp_bootstrap {
                    wifi_tcp_status.publish(WifiTcpStatus::for_bootstrap(
                        bootstrap,
                        WifiTcpPhase::Faulted,
                    ));
                }
                error!(
                    "e290-node stage=spawn status=DISABLED task=wifi-tcp reason=task-pool interface=2 lora_routing=continue ble_management=continue"
                );
                None
            }
        };
        #[cfg(feature = "gateway")]
        let wifi_tcp_task_available = wifi_tcp_task.is_some();
        #[cfg(feature = "appliance")]
        let ble_api_task = {
            // Retain the USB peripheral token for Serial/JTAG diagnostics over
            // the complete BLE task lifetime; USB exposes no device-API surface.
            match ble_connector {
                Some(connector) => match ble_api_task::run(
                    connector,
                    BleAdvertisingParameters::new(identity_hash, base_mac_eui48),
                    ble_api_task::BleHandoffs::new(
                        bearer_pairing_handoff,
                        bearer_live_pairing_handoff,
                        ble_bond_handoff,
                        bearer_session_admission,
                        bearer_authenticated_api,
                    ),
                    local_api_session_parameters,
                    local_api_session_rng,
                    ble_api_task::BlePhysicalOwners::new(
                        pairing_button,
                        display_publisher,
                        display_home,
                        restored_ble_bond,
                        boot_ble_bond_store_available,
                    ),
                    usb_serial_jtag_diagnostics_owner,
                ) {
                    Ok(task) => Some(task),
                    Err(_) => {
                        error!(
                            "e290-node stage=spawn status=DISABLED task=ble-api lora_routing=continue"
                        );
                        None
                    }
                },
                None => {
                    error!(
                        "e290-node stage=spawn status=DISABLED task=ble-api reason=controller-unavailable lora_routing=continue"
                    );
                    None
                }
            }
        };
        #[cfg(feature = "appliance")]
        let ble_api_task_available = ble_api_task.is_some();
        // This Embassy version reports pool exhaustion while constructing each
        // SpawnToken above; `Spawner::spawn` is infallible and returns unit.
        spawner.spawn(radio_task);
        spawner.spawn(node_task);
        #[cfg(feature = "gateway")]
        if let Some(wifi_tcp_task) = wifi_tcp_task {
            spawner.spawn(wifi_tcp_task);
        }
        #[cfg(feature = "appliance")]
        if let Some(ble_api_task) = ble_api_task {
            spawner.spawn(ble_api_task);
        }
        #[cfg(feature = "appliance")]
        info!(
            "e290-node stage=composition status=PASS tasks={} interfaces={} primary_transport=lora local_api_profile=appliance ble_api_task_available={ble_api_task_available} wifi_station_compiled={} wifi_station_task_available={} wifi_station_phase={:?} wifi_tcp_compiled={} wifi_tcp_task_available={} wifi_tcp_phase={:?} usb=serial-jtag-logging display_task_spawned={display_task_spawned} display_available={display_available} ble_max_centrals=1 ble_pairing=display-passkey-physical-presence ble_bond_capacity=1 ble_bond_store_available={durable_ble_bond_store_available} ble_bond_present={durable_ble_bond_present} ble_bond_generation={durable_ble_bond_generation:?} ble_tx=indicate ble_rx=authenticated-write-with-response authenticated_local_api=node-dispatch bearer_session=ble-authenticated-single-flight session_suite=ble-gatt node_journal=mounted resident_storage_available={storage_service_available} lxmf_store_available={lxmf_service_available} lxmf_delivery_admission={lxmf_delivery_admission} credential_state={credential_boot_state:?} credential_revision={credential_revision:?} credential_authority_publishable={credential_authority_publishable} credential_mutation_eligible={credential_mutation_eligible} credential_pairing_policy_resident={credential_pairing_policy_available} credential_initialization={credential_initialization_status:?} credential_offset=0x{:x} credential_len=0x{:x} durable_runtime_bytes={} esp_rtos_source={}",
            (if ble_api_task_available { 3 } else { 2 })
                + {
                    #[cfg(feature = "gateway")]
                    {
                        usize::from(wifi_station_task_available)
                    }
                    #[cfg(not(feature = "gateway"))]
                    {
                        0
                    }
                }
                + {
                    #[cfg(feature = "gateway")]
                    {
                        usize::from(wifi_tcp_task_available)
                    }
                    #[cfg(not(feature = "gateway"))]
                    {
                        0
                    }
                }
                + if display_task_spawned { 1 } else { 0 },
            config::INTERFACE_SLOTS,
            cfg!(feature = "gateway"),
            {
                #[cfg(feature = "gateway")]
                {
                    wifi_station_task_available
                }
                #[cfg(not(feature = "gateway"))]
                {
                    false
                }
            },
            {
                #[cfg(feature = "gateway")]
                {
                    wifi_station_status.snapshot().phase
                }
                #[cfg(not(feature = "gateway"))]
                {
                    "not-compiled"
                }
            },
            cfg!(feature = "gateway"),
            {
                #[cfg(feature = "gateway")]
                {
                    wifi_tcp_task_available
                }
                #[cfg(not(feature = "gateway"))]
                {
                    false
                }
            },
            {
                #[cfg(feature = "gateway")]
                {
                    wifi_tcp_status.snapshot().phase
                }
                #[cfg(not(feature = "gateway"))]
                {
                    "not-compiled"
                }
            },
            credential_binding.absolute_offset(),
            credential_binding.length(),
            config::DURABLE_RUNTIME_BYTES,
            env!("RETICULUM_ESP_RTOS_UPSTREAM_IDENTITY"),
        );
    }

    pending().await
}

fn log_credential_boot(report: &BootCredentialStore) {
    let state = report.state();
    let binding = report.binding();
    let revision = report.revision();
    let recovery = report.recovery();
    let steps = report.completed_recovery_steps();
    let writes = report.raw_write_calls();
    let erases = report.raw_erase_calls();
    match state {
        CredentialBootState::Ready => info!(
            "e290-node stage=credential-store status=READY state={state:?} revision={revision:?} recovery={recovery:?} recovery_steps={steps} writes={writes} erases={erases} binding_offset=0x{:x} binding_len=0x{:x} authority_publishable=true credential_mutation_eligible=true external_local_api=closed local_api_bearer=absent local_api_session=absent lora_routing=continue submission_policy=unchanged",
            binding.absolute_offset(),
            binding.length(),
        ),
        CredentialBootState::AuthenticationOnly { .. } => warn!(
            "e290-node stage=credential-store status=AUTHENTICATION-ONLY state={state:?} revision={revision:?} recovery={recovery:?} recovery_steps={steps} writes={writes} erases={erases} binding_offset=0x{:x} binding_len=0x{:x} authority_publishable=true credential_mutation_eligible=false external_local_api=closed local_api_bearer=absent local_api_session=absent lora_routing=continue submission_policy=unchanged",
            binding.absolute_offset(),
            binding.length(),
        ),
        CredentialBootState::UninitializedErased => info!(
            "e290-node stage=credential-store status=UNINITIALIZED-ERASED state={state:?} revision=none recovery=none recovery_steps={steps} writes={writes} erases={erases} binding_offset=0x{:x} binding_len=0x{:x} authority_publishable=false credential_mutation_eligible=false external_local_api=closed explicit_initialization_required=true automatic_provision=false lora_routing=continue submission_policy=unchanged",
            binding.absolute_offset(),
            binding.length(),
        ),
        CredentialBootState::InitializationInterrupted => warn!(
            "e290-node stage=credential-store status=INITIALIZATION-INTERRUPTED state={state:?} revision=none recovery=none recovery_steps={steps} writes={writes} erases={erases} binding_offset=0x{:x} binding_len=0x{:x} authority_publishable=false credential_mutation_eligible=false external_local_api=closed explicit_recovery_required=true automatic_recovery=false lora_routing=continue submission_policy=unchanged",
            binding.absolute_offset(),
            binding.length(),
        ),
        CredentialBootState::Blocked { .. }
        | CredentialBootState::Corrupt { .. }
        | CredentialBootState::Backend { .. } => error!(
            "e290-node stage=credential-store status=DISABLED state={state:?} revision={revision:?} recovery={recovery:?} recovery_steps={steps} writes={writes} erases={erases} binding_offset=0x{:x} binding_len=0x{:x} authority_publishable=false credential_mutation_eligible=false external_local_api=closed lora_routing=continue submission_policy=unchanged",
            binding.absolute_offset(),
            binding.length(),
        ),
    }
}

fn monotonic_us() -> u64 {
    Instant::now().as_micros()
}

fn inert_before_rtos() -> ! {
    // Embassy timers are unavailable until `esp_rtos::start`. These early
    // fail-stop paths run with RF held in reset. The immediately preceding
    // error is visible through the production USB logger.
    loop {
        core::hint::spin_loop();
    }
}

#[inline(never)]
fn inert_large_construction() -> ! {
    // These post-RTOS callers can hold construction-sized protocol values.
    // Suspending there would copy one-shot scratch into product_main's
    // permanent Embassy task storage and steal the linker-generated CPU0 stack
    // reservation. A terminal synchronous stop keeps that scratch transient.
    loop {
        core::hint::spin_loop();
    }
}

async fn inert_forever() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
    }
}
