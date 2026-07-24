//! First permanent LoRa-first Reticulum node image for the Vision Master E290.
//!
//! The executable composes one transport-neutral node task with one concrete
//! LoRa actor task. Before either is constructed, the sole flash owner safely
//! provisions or strictly mounts the submission journal and drives an explicit
//! bounded-history recovery gate to completion. The backend-independent durable
//! runtime then remains resident with the sole operation-scoped flash
//! coordinator. The ordinary profile's third task owns USB Serial/JTAG
//! initialization, live pairing, and the single-flight authenticated local API
//! plus GPIO21 physical presence without becoming a Reticulum packet
//! interface. The mutually exclusive `wifi-api-proof` and `ble-api-proof`
//! profiles replace that task with one authenticated wireless RDA1 bearer.
//! Neither profile composes USB concurrently. The node retains
//! transport-neutral admission and authenticated dispatch lanes. Inbound
//! durable LXMF delivery is composed without becoming an interface; NomadNet
//! and UI actors remain independent later capabilities.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave RF hardware active"
)]
#![deny(clippy::large_stack_frames)]

#[cfg(all(feature = "ble-api-proof", feature = "wifi-api-proof"))]
compile_error!("ble-api-proof and wifi-api-proof are mutually exclusive local API bearers");
#[cfg(all(reticulum_e290_ble_startup_diagnostic, not(feature = "ble-api-proof")))]
compile_error!("the E290 BLE startup diagnostic requires ble-api-proof");

#[cfg(feature = "ble-api-proof")]
mod ble_api_task;
mod node_task;
mod platform_storage;
mod radio_task;
#[cfg(feature = "runtime-measurement-hil")]
mod runtime_measurement_stack_hil;
#[cfg_attr(
    any(feature = "ble-api-proof", feature = "wifi-api-proof"),
    allow(
        dead_code,
        unused_imports,
        reason = "wireless profiles retain only the earliest USB quarantine boundary"
    )
)]
mod usb_pairing_task;
#[cfg(feature = "wifi-api-proof")]
mod wifi_api_task;

#[cfg(not(feature = "runtime-measurement-hil"))]
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
#[cfg(feature = "runtime-measurement-hil")]
use esp_alloc::{EspHeap, MemoryCapability};
use esp_backtrace as _;
#[cfg(not(any(feature = "ble-api-proof", feature = "wifi-api-proof")))]
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
use reticulum_board_heltec_vision_master_e290_radio::{
    E290_NA915_DEV_CONFIGURATION, E290_NA915_DEV_CONFIGURATION_FINGERPRINT, E290_NA915_DEV_PROFILE,
    E290Radio,
};
use reticulum_device_api::LxmfPeerDiscoveryIncarnation;
use reticulum_device_api_handoff::DeviceApiHandoff;
use reticulum_device_api_pairing::DeviceId;
#[cfg(any(feature = "ble-api-proof", feature = "wifi-api-proof"))]
use reticulum_device_api_session::SessionSuite;
use reticulum_device_api_session::{
    AuthenticatedGrant, BearerBinding as SessionBearerBinding, DeviceId as SessionDeviceId,
    ServerParameters,
};
use reticulum_device_identity_store::IdentityMirrorCoverage;
#[cfg(feature = "runtime-measurement-hil")]
use reticulum_heltec_vision_master_e290_node::runtime_measurement::{
    BootPhase as RuntimeBootPhase, HeapSnapshot as RuntimeHeapSnapshot,
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE, StackSnapshot as RuntimeStackSnapshot,
};
#[cfg(feature = "wifi-api-proof")]
use reticulum_heltec_vision_master_e290_node::wifi_api_profile;
use reticulum_heltec_vision_master_e290_node::{
    config,
    credential_boot::CredentialBootState,
    device_api_id_from_eui48,
    durability_boot::{
        BUILD_JOURNAL_REPROVISION_POLICY, JournalReprovisionPolicy, announce_clock_policy,
        journal_boot_policy,
    },
    live_pairing_handoff::LivePairingHandoff,
    lxmf_delivery::{LxmfDeliveryActivation, activate_lxmf_delivery},
    pairing_control_handoff::PairingControlHandoff,
    session_admission_handoff::SessionAdmissionHandoff,
    storage_device_id_from_eui48,
};
use reticulum_interface_router::InterfaceFabric;
use reticulum_lxmf_store::LxmfStoreIndexSlot;
use reticulum_node_core::{
    ApplicationEventOwner, ApplicationEventSlot, DelayedProofOwner, DelayedProofSlot,
    InboundProofPolicy, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId, OrdinaryBufferPool,
    OrdinaryPacketBuffer, PacketInterfaceId, TxPacketBuffer,
};
use reticulum_radio_lora_phy::IrqTimestampCapture;
use reticulum_radio_tx_dispatch::{ExactLoRaAirtimePolicy, SoleRadioTxDispatcher};
use reticulum_rns_inbox_store::InboxStoreState;
use reticulum_tx_handoff::{AuthorizedFrameHandoff, DataPermitHandoff, OrdinaryPermitHandoff};
use reticulum_tx_supervisor::{
    DataRouterCoordinator, NodeInterfaceSupervisor, OrdinaryRouterCoordinator,
};
use static_cell::StaticCell;

use crate::platform_storage::{
    BootCredentialStore, ProductCredentialInitializationPort, ProductFlashOwner,
    ProductStorageCoordinator, ProductSubmissionRuntime,
};

#[cfg(debug_assertions)]
compile_error!("the permanent E290 node must be built with --release");

const LORA_INTERFACE: PacketInterfaceId = PacketInterfaceId::new(1);

type E290SpiDevice = ExclusiveDevice<Spi<'static, Async>, Output<'static>, Delay>;
type ProductRadio =
    E290Radio<E290SpiDevice, Output<'static>, Input<'static>, BoundedBusy<Input<'static>>, Delay>;

pub(crate) type ProductDispatcher = SoleRadioTxDispatcher<
    ProductRadio,
    Trng,
    CriticalSectionRawMutex,
    { config::INTERFACE_QUEUE_DEPTH },
>;

pub(crate) type ProductSupervisor = NodeInterfaceSupervisor<
    CriticalSectionRawMutex,
    ExactLoRaAirtimePolicy,
    { config::PATHS },
    { config::ANNOUNCES },
    { config::DEDUPLICATION },
    { config::LINKS },
    { config::DATA_BUFFERS },
    { config::ORDINARY_BUFFERS },
    { config::INTERFACE_SLOTS },
    { config::INTERFACE_QUEUE_DEPTH },
>;

static IRQ_TIMESTAMPS: IrqTimestampCapture = IrqTimestampCapture::new_monotonic_us(monotonic_us);
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
static AUTHORIZED_FRAME: StaticCell<AuthorizedFrameHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
static DATA_PACKET_STORAGE: StaticCell<[TxPacketBuffer; config::DATA_BUFFERS]> = StaticCell::new();
static ORDINARY_PACKET_STORAGE: StaticCell<[OrdinaryPacketBuffer; config::ORDINARY_BUFFERS]> =
    StaticCell::new();
static APPLICATION_EVENT_STORAGE: StaticCell<
    [ApplicationEventSlot; config::APPLICATION_EVENT_SLOTS],
> = StaticCell::new();
static SUPERVISOR: StaticCell<ProductSupervisor> = StaticCell::new();
static DISPATCHER: StaticCell<ProductDispatcher> = StaticCell::new();
static FLASH_STORAGE: StaticCell<FlashStorage<'static>> = StaticCell::new();
static STORAGE_COORDINATOR: StaticCell<ProductStorageCoordinator> = StaticCell::new();
static PAIRING_CONTROL: StaticCell<PairingControlHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
static LIVE_PAIRING: StaticCell<LivePairingHandoff<CriticalSectionRawMutex>> = StaticCell::new();
static SESSION_ADMISSION: StaticCell<SessionAdmissionHandoff<CriticalSectionRawMutex>> =
    StaticCell::new();
static AUTHENTICATED_API: StaticCell<
    DeviceApiHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
> = StaticCell::new();
const _: () = assert!(
    mem::size_of::<DeviceApiHandoff<CriticalSectionRawMutex, AuthenticatedGrant>>()
        <= config::MAXIMUM_AUTHENTICATED_API_HANDOFF_BYTES
);
static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();

#[cfg(reticulum_e290_ble_startup_diagnostic)]
pub(crate) struct DiagnosticUsbSerialJtagOwner {
    _usb_device: Option<esp_hal::peripherals::USB_DEVICE<'static>>,
}

#[cfg(reticulum_e290_ble_startup_diagnostic)]
impl DiagnosticUsbSerialJtagOwner {
    const fn before_hal_initialization() -> Self {
        Self { _usb_device: None }
    }

    fn retain_native_usb(mut self, usb_device: esp_hal::peripherals::USB_DEVICE<'static>) -> Self {
        debug_assert!(self._usb_device.is_none());
        self._usb_device = Some(usb_device);
        self
    }
}

#[cfg(reticulum_e290_ble_startup_diagnostic)]
type ProductUsbBootBoundary = DiagnosticUsbSerialJtagOwner;
#[cfg(not(reticulum_e290_ble_startup_diagnostic))]
type ProductUsbBootBoundary = usb_pairing_task::BootUsbQuarantine;

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

#[esp_hal::main]
fn main() -> ! {
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    let usb_boot_boundary = DiagnosticUsbSerialJtagOwner::before_hal_initialization();
    #[cfg(not(reticulum_e290_ble_startup_diagnostic))]
    let usb_boot_boundary = usb_pairing_task::quarantine_usb_at_boot();
    let executor = EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run(|spawner| {
        spawner.spawn(
            product_main(spawner, usb_boot_boundary)
                .expect("the sole product composition task pool must be available"),
        );
    })
}

#[allow(
    clippy::large_stack_frames,
    reason = "one-shot composition moves every fixed owner into static task storage"
)]
#[embassy_executor::task]
async fn product_main(spawner: Spawner, usb_boot_boundary: ProductUsbBootBoundary) -> ! {
    let hal = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(hal);
    #[cfg(reticulum_e290_ble_startup_diagnostic)]
    let diagnostic_usb_serial_jtag_owner =
        usb_boot_boundary.retain_native_usb(peripherals.USB_DEVICE);
    #[cfg(not(reticulum_e290_ble_startup_diagnostic))]
    let usb_boot_quarantine: usb_pairing_task::BootUsbQuarantine = usb_boot_boundary;

    // Establish the complete E290 RF interlock at the composition task's first
    // poll, before logging, entropy, PSRAM initialization or radio construction.
    let radio_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let radio_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    #[cfg(feature = "runtime-measurement-hil")]
    let mut runtime_stack = match runtime_measurement_stack_hil::StackWatermarkMonitor::initialize()
    {
        Ok(monitor) => monitor,
        Err(reason) => {
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE
                .record_initialization_error(reason.evidence_code());
            loop {
                core::hint::spin_loop();
            }
        }
    };

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
    #[cfg(not(any(feature = "ble-api-proof", feature = "wifi-api-proof")))]
    let local_api_session_parameters = ServerParameters::new(
        SessionDeviceId::new(device_api_id_from_eui48(base_mac_eui48)),
        SessionBearerBinding::UsbSerialJtag,
    );
    #[cfg(feature = "wifi-api-proof")]
    let local_api_session_parameters = ServerParameters::new_for_suite(
        SessionDeviceId::new(device_api_id_from_eui48(base_mac_eui48)),
        SessionBearerBinding::Wifi,
        SessionSuite::WifiQualification,
    );
    #[cfg(feature = "ble-api-proof")]
    let local_api_session_parameters = ServerParameters::new_for_suite(
        SessionDeviceId::new(device_api_id_from_eui48(base_mac_eui48)),
        SessionBearerBinding::BleGatt,
        SessionSuite::BleGattQualification,
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
    esp_alloc::psram_allocator!(&psram);
    info!(
        "e290-node stage=psram status=PASS bytes={psram_bytes} mode=auto minimum_qualified_bytes={} ownership_state=external-allocator-ready",
        config::MINIMUM_PSRAM_BYTES,
    );

    let timers = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);

    let lxmf_volatile = match Box::try_new_in(node_task::LxmfVolatileState::new(), ExternalMemory) {
        Ok(state) => state,
        Err(_) => {
            error!(
                "e290-node stage=lxmf-volatile status=FAIL reason=external-allocation expected_bytes={}",
                mem::size_of::<node_task::LxmfVolatileState>(),
            );
            inert_forever().await
        }
    };
    let lxmf_volatile_bytes = mem::size_of_val(&*lxmf_volatile);
    let lxmf_volatile_start = (&*lxmf_volatile as *const node_task::LxmfVolatileState) as usize;
    let lxmf_volatile_end = match lxmf_volatile_start.checked_add(lxmf_volatile_bytes) {
        Some(end) => end,
        None => {
            error!("e290-node stage=lxmf-volatile status=FAIL reason=allocation-range-overflow");
            inert_forever().await
        }
    };
    if lxmf_volatile_bytes != mem::size_of::<node_task::LxmfVolatileState>()
        || !lxmf_volatile_start.is_multiple_of(mem::align_of::<node_task::LxmfVolatileState>())
        || lxmf_volatile_start < psram_start_address
        || lxmf_volatile_end > psram_end_address
    {
        error!(
            "e290-node stage=lxmf-volatile status=FAIL reason=external-address allocation_start=0x{lxmf_volatile_start:08x} allocation_end=0x{lxmf_volatile_end:08x} psram_start=0x{psram_start_address:08x} psram_end=0x{psram_end_address:08x} expected_bytes={} actual_bytes={lxmf_volatile_bytes} alignment={}",
            mem::size_of::<node_task::LxmfVolatileState>(),
            mem::align_of::<node_task::LxmfVolatileState>(),
        );
        inert_forever().await
    }
    info!(
        "e290-node stage=lxmf-volatile status=PASS ownership=boot-lifetime-external bytes={lxmf_volatile_bytes} start=0x{lxmf_volatile_start:08x} end=0x{lxmf_volatile_end:08x}"
    );
    let lxmf_volatile: &'static mut node_task::LxmfVolatileState = Box::leak(lxmf_volatile);

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
    #[cfg(feature = "runtime-measurement-hil")]
    let runtime_measurement_started_us = monotonic_us();
    #[cfg(feature = "runtime-measurement-hil")]
    {
        let measurement_work_started_us = monotonic_us();
        RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_psram_bytes(psram_bytes as u64);
        record_runtime_heap_snapshot(&esp_alloc::HEAP);
        record_runtime_stack_snapshot(&mut runtime_stack);
        RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE
            .record_measurement_work(monotonic_us().saturating_sub(measurement_work_started_us));
    }

    let _entropy_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    let mut bootstrap_rng = match Trng::try_new() {
        Ok(rng) => rng,
        Err(reason) => {
            error!("e290-node stage=entropy status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };
    #[cfg(feature = "runtime-measurement-hil")]
    let credential_boot_started_us = monotonic_us();
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
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_boot_phase(
        RuntimeBootPhase::CredentialBoot,
        monotonic_us().saturating_sub(credential_boot_started_us),
    );
    #[cfg(feature = "runtime-measurement-hil")]
    let identity_preflight_started_us = monotonic_us();
    let identity_preflight = match flash_owner.inspect_identity() {
        Ok(preflight) => preflight,
        Err(reason) => {
            error!("e290-node stage=identity-preflight status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_boot_phase(
        RuntimeBootPhase::IdentityPreflight,
        monotonic_us().saturating_sub(identity_preflight_started_us),
    );
    let fresh_clock_policy = announce_clock_policy(identity_preflight);
    let journal_reprovision_policy = BUILD_JOURNAL_REPROVISION_POLICY;
    let journal_policy = journal_boot_policy(identity_preflight, journal_reprovision_policy);
    info!(
        "e290-node stage=journal-reprovision-policy status=SELECTED build_policy={} explicit={} migration_mutation_scope=node_journal erased_media_only={} automatic_erase=false identity_config_preserved=true normal_boot_clock_reservation=unchanged",
        journal_reprovision_policy.log_label(),
        matches!(
            journal_reprovision_policy,
            JournalReprovisionPolicy::ExplicitErasedSchema3Development
        ),
        matches!(
            journal_policy,
            reticulum_heltec_vision_master_e290_node::durability_boot::JournalBootPolicy::ProvisionErasedSchema3Development
        ),
    );
    info!(
        "e290-node stage=identity-preflight status=PASS state={identity_preflight:?} announce_clock_policy={fresh_clock_policy:?} journal_reprovision_policy={journal_reprovision_policy:?} journal_policy={journal_policy:?} writes=0 erases=0"
    );
    #[cfg(feature = "runtime-measurement-hil")]
    let journal_provision_started_us = monotonic_us();
    let journal_provision = flash_owner.provision_node_journal(journal_policy);
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_boot_phase(
        RuntimeBootPhase::JournalProvision,
        monotonic_us().saturating_sub(journal_provision_started_us),
    );
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
    #[cfg(feature = "runtime-measurement-hil")]
    let announce_epoch_started_us = monotonic_us();
    let boot_epoch_result = flash_owner.reserve_announce_epoch(fresh_clock_policy);
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_boot_phase(
        RuntimeBootPhase::AnnounceEpoch,
        monotonic_us().saturating_sub(announce_epoch_started_us),
    );
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
    #[cfg(feature = "runtime-measurement-hil")]
    let identity_boot_started_us = monotonic_us();
    let boot_identity_result = flash_owner.boot_identity(&mut bootstrap_rng);
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_boot_phase(
        RuntimeBootPhase::IdentityBoot,
        monotonic_us().saturating_sub(identity_boot_started_us),
    );
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
    #[cfg(feature = "runtime-measurement-hil")]
    let journal_mount_started_us = monotonic_us();
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
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_boot_phase(
        RuntimeBootPhase::JournalMount,
        monotonic_us().saturating_sub(journal_mount_started_us),
    );
    let submission_runtime = match journal_mount_result {
        Some(Ok((runtime, journal_report))) => {
            info!(
                "e290-node stage=node-journal-recovery status=PASS boot_sequence={} profile=bounded-live-admission accepted_limit={} bank={:?} generation={} records={} accepted={} replayed={} queued={} already_final={} finalized={} writes={} erases={} service_gate=open flash_owner=resident",
                announce_epoch.get(),
                config::DURABLE_ACCEPTED_SUBMISSION_LIMIT,
                journal_report.state.bank(),
                journal_report.state.generation(),
                journal_report.state.committed_records(),
                journal_report.state.accepted_submissions(),
                journal_report.replayed_submissions,
                journal_report.queued_submissions,
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
    #[cfg(feature = "runtime-measurement-hil")]
    let inbox_mount_started_us = monotonic_us();
    let inbox_mount_result = flash_owner.mount_inbox();
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_boot_phase(
        RuntimeBootPhase::InboxMount,
        monotonic_us().saturating_sub(inbox_mount_started_us),
    );
    let inbox = match inbox_mount_result {
        Ok(inbox) => {
            let depth = match inbox.state() {
                InboxStoreState::Empty => 0,
                InboxStoreState::Occupied(_) => 1,
            };
            info!(
                "e290-node stage=rns-inbox-mount status=PASS profile=durable-qualification capacity=1 depth={depth} max_payload_bytes={} plaintext=true writes=0 erases=0 lora_routing=continue",
                reticulum_rns_inbox_store::MAX_PAYLOAD_SIZE,
            );
            Some(inbox)
        }
        Err(reason) => {
            error!(
                "e290-node stage=rns-inbox-mount status=DISABLED reason={reason:?} lora_routing=continue inbound_client_delivery=closed"
            );
            None
        }
    };
    let lxmf = match flash_owner.mount_lxmf(lxmf_index) {
        Ok(lxmf) => {
            info!(
                "e290-node stage=lxmf-store-mount status=PASS profile=mounted index_slots={} messages={} consumed_extents={} partition=0x930000..0xb30000 plaintext=true writes=0 erases=0 destination_activation=pending-node-construction lora_routing=continue",
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
    let storage_coordinator = flash_owner.into_storage_coordinator(
        submission_runtime,
        inbox,
        lxmf,
        credential_boot,
        device_api_id,
    );
    let storage_service_available = storage_coordinator.submission_service_available();
    let inbox_service_available = storage_coordinator.inbox_service_available();
    let lxmf_service_available = storage_coordinator.lxmf_service_available();
    let credential_boot_state = storage_coordinator.credential_boot_state();
    let credential_binding = storage_coordinator.credential_binding();
    let credential_revision = storage_coordinator.credential_revision();
    let credential_authority_publishable = storage_coordinator.credential_authority_publishable();
    let credential_mutation_eligible = storage_coordinator.credential_mutation_eligible();
    let credential_pairing_policy_available =
        storage_coordinator.credential_pairing_policy_available();
    let credential_initialization_status = storage_coordinator.initialization_status();
    let storage_coordinator = STORAGE_COORDINATOR.init(storage_coordinator);

    let node_rng = bootstrap_rng.clone();
    let radio_rng = bootstrap_rng.clone();
    let local_api_session_rng = bootstrap_rng.clone();
    #[cfg(feature = "wifi-api-proof")]
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

    let mut node = match NodeCore::<
        { config::PATHS },
        { config::ANNOUNCES },
        { config::DEDUPLICATION },
        { config::LINKS },
        { config::DATA_BUFFERS },
    >::new(
        identity,
        config::RNS_APPLICATION_NAME,
        &config::RNS_PRIMARY_ASPECTS,
        instance,
        NodeConfig::transport(),
    ) {
        Ok(node) => node,
        Err(reason) => {
            error!("e290-node stage=node status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };
    node.set_inbound_proof_policy(InboundProofPolicy::Always);
    let primary_destination = node.destination_hash();
    if let Err(reason) = node.set_destination_accepts_links(&primary_destination, false) {
        error!(
            "e290-node stage=node-link-policy status=FAIL destination=primary reason={reason:?} action=pre-task-construction-fail-stop"
        );
        inert_forever().await
    }
    let lxmf_destination = match activate_lxmf_delivery(&mut node, lxmf_service_available) {
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

    let mut data_buffers = DATA_PACKET_STORAGE
        .init([const { TxPacketBuffer::new() }; config::DATA_BUFFERS])
        .each_mut();
    for buffer in data_buffers.iter_mut() {
        if let Err(reason) = node.register_packet_buffer(buffer) {
            error!("e290-node stage=data-buffer status=FAIL reason={reason:?}");
            inert_forever().await
        }
    }
    let data =
        match DataRouterCoordinator::try_new(&node, data_buffers, config::data_router_config()) {
            Ok(coordinator) => coordinator,
            Err(failure) => {
                error!(
                    "e290-node stage=data-coordinator status=FAIL reason={:?}",
                    failure.reason()
                );
                inert_forever().await
            }
        };

    let mut ordinary_owner = match node.take_ordinary_action_owner::<{ config::ORDINARY_BUFFERS }>()
    {
        Ok(owner) => owner,
        Err(reason) => {
            error!("e290-node stage=ordinary-owner status=FAIL reason={reason:?}");
            inert_forever().await
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
            inert_forever().await
        }
    }
    let ordinary = match OrdinaryRouterCoordinator::try_new(
        ordinary_owner,
        ordinary_pool,
        config::ordinary_router_config(),
    ) {
        Ok(coordinator) => coordinator,
        Err(failure) => {
            error!(
                "e290-node stage=ordinary-coordinator status=FAIL reason={:?}",
                failure.reason()
            );
            inert_forever().await
        }
    };

    let fabric = INTERFACE_FABRIC.init(InterfaceFabric::new());
    let data_pair = DATA_PERMIT.init(DataPermitHandoff::new()).split_paired();
    let ordinary_pair = ORDINARY_PERMIT
        .init(OrdinaryPermitHandoff::new())
        .split_paired();
    let (frame_node, frame_dispatcher) =
        AUTHORIZED_FRAME.init(AuthorizedFrameHandoff::new()).split();
    let policy = ExactLoRaAirtimePolicy::new(
        LORA_INTERFACE,
        E290_NA915_DEV_PROFILE,
        E290_NA915_DEV_CONFIGURATION_FINGERPRINT,
    );
    let (mut supervisor, [actor]) = match ProductSupervisor::try_new(
        node,
        fabric,
        data,
        ordinary,
        [data_pair],
        [ordinary_pair],
        policy,
    ) {
        Ok(success) => success.into_parts(),
        Err(failure) => {
            error!(
                "e290-node stage=supervisor status=FAIL reason={:?}",
                failure.reason()
            );
            inert_forever().await
        }
    };
    let offline_descriptor = match supervisor.register_interface(
        actor.queue_id(),
        LORA_INTERFACE,
        config::interface_properties(),
    ) {
        Ok(descriptor) => descriptor,
        Err(reason) => {
            error!("e290-node stage=interface-register status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };
    let (interface, data_permit, ordinary_permit) = actor.into_parts();
    let (tx_interface, ingress, lifecycle) = interface.into_parts();
    let ingress_authority = match ingress.bind_ingress(offline_descriptor) {
        Ok(authority) => authority,
        Err(reason) => {
            error!("e290-node stage=interface-bind status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };

    #[cfg(feature = "runtime-measurement-hil")]
    let radio_init_started_us = monotonic_us();
    let spi = match Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(config::SPI_FREQUENCY_HZ))
            .with_mode(SpiMode::_0),
    ) {
        Ok(spi) => spi,
        Err(_) => {
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
        E290_NA915_DEV_CONFIGURATION,
    )
    .await
    {
        Ok(radio) => radio,
        Err(fault) => {
            error!(
                "e290-node stage=radio-init status=FAIL operation={:?} radio={:?}",
                fault.operation, fault.radio,
            );
            inert_forever().await
        }
    };
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_boot_phase(
        RuntimeBootPhase::RadioInit,
        monotonic_us().saturating_sub(radio_init_started_us),
    );
    let dispatcher = DISPATCHER.init(SoleRadioTxDispatcher::new(
        radio,
        radio_rng,
        tx_interface,
        data_permit,
        ordinary_permit,
        frame_dispatcher,
        config::dispatcher_config(),
    ));
    let supervisor = SUPERVISOR.init(supervisor);
    let application_event_storage = APPLICATION_EVENT_STORAGE
        .init([const { ApplicationEventSlot::new() }; config::APPLICATION_EVENT_SLOTS]);
    let application_events = ApplicationEventOwner::new(application_event_storage);
    let delayed_proofs = DelayedProofOwner::new(delayed_proof_storage);
    let (usb_pairing_handoff, node_pairing_handoff) =
        PAIRING_CONTROL.init(PairingControlHandoff::new()).split();
    let (usb_live_pairing_handoff, node_live_pairing_handoff) =
        LIVE_PAIRING.init(LivePairingHandoff::new()).split();
    let (usb_session_admission, node_session_admission) = SESSION_ADMISSION
        .init(SessionAdmissionHandoff::new())
        .split();
    let (usb_authenticated_api, node_authenticated_api) =
        AUTHENTICATED_API.init(DeviceApiHandoff::new()).split();
    #[cfg(not(any(feature = "ble-api-proof", feature = "wifi-api-proof")))]
    let pairing_button = Input::new(
        peripherals.GPIO21,
        InputConfig::default().with_pull(Pull::Up),
    );

    let radio_task = match radio_task::run(dispatcher, ingress, lifecycle, ingress_authority) {
        Ok(task) => task,
        Err(_) => {
            error!("e290-node stage=spawn status=FAIL task=lora");
            inert_forever().await
        }
    };
    let node_task = match node_task::run(
        supervisor,
        storage_coordinator,
        application_events,
        delayed_proofs,
        lxmf_volatile,
        lxmf_destination,
        peer_discovery_incarnation,
        node_task::NodeHandoffs::new(
            node_pairing_handoff,
            node_live_pairing_handoff,
            node_session_admission,
            node_authenticated_api,
            frame_node,
        ),
        offline_descriptor,
        announce_epoch,
        node_rng,
    ) {
        Ok(task) => task,
        Err(_) => {
            error!("e290-node stage=spawn status=FAIL task=node");
            inert_forever().await
        }
    };
    #[cfg(not(any(feature = "ble-api-proof", feature = "wifi-api-proof")))]
    let usb_pairing_task = match usb_pairing_task::run(
        usb_boot_quarantine,
        peripherals.USB_DEVICE,
        pairing_button,
        usb_pairing_task::UsbHandoffs::new(
            usb_pairing_handoff,
            usb_live_pairing_handoff,
            usb_session_admission,
            usb_authenticated_api,
        ),
        local_api_session_parameters,
        local_api_session_rng,
    ) {
        Ok(task) => task,
        Err(_) => {
            error!("e290-node stage=spawn status=FAIL task=usb-pairing");
            inert_forever().await
        }
    };
    #[cfg(feature = "wifi-api-proof")]
    let wifi_api_task = {
        // The earliest entrypoint deliberately leaves USB quarantined for the
        // complete Wi-Fi proof boot. Moving the opaque token here makes that
        // one-bearer choice explicit even though the Wi-Fi task has no USB
        // capability.
        let _usb_boot_quarantine = usb_boot_quarantine;
        match wifi_api_task::run(
            spawner,
            peripherals.WIFI,
            base_mac_eui48,
            wifi_network_seed,
            wifi_api_task::WifiHandoffs::new(
                usb_pairing_handoff,
                usb_live_pairing_handoff,
                usb_session_admission,
                usb_authenticated_api,
            ),
            local_api_session_parameters,
            local_api_session_rng,
        ) {
            Ok(task) => task,
            Err(_) => {
                error!("e290-node stage=spawn status=FAIL task=wifi-api");
                inert_forever().await
            }
        }
    };
    #[cfg(feature = "ble-api-proof")]
    let ble_api_task = {
        // The earliest entrypoint deliberately leaves USB quarantined for the
        // complete BLE proof boot. Moving the opaque token here makes the
        // one-bearer choice explicit even though the BLE task has no USB
        // capability or pairing surface.
        #[cfg(not(reticulum_e290_ble_startup_diagnostic))]
        let _usb_boot_quarantine = usb_boot_quarantine;
        match ble_api_task::run(
            peripherals.BT,
            base_mac_eui48,
            ble_api_task::BleHandoffs::new(
                usb_pairing_handoff,
                usb_live_pairing_handoff,
                usb_session_admission,
                usb_authenticated_api,
            ),
            local_api_session_parameters,
            local_api_session_rng,
            #[cfg(reticulum_e290_ble_startup_diagnostic)]
            diagnostic_usb_serial_jtag_owner,
        ) {
            Ok(task) => Some(task),
            Err(_) => {
                error!("e290-node stage=spawn status=DISABLED task=ble-api lora_routing=continue");
                None
            }
        }
    };
    #[cfg(feature = "ble-api-proof")]
    let ble_api_task_available = ble_api_task.is_some();
    // This Embassy version reports pool exhaustion while constructing each
    // SpawnToken above; `Spawner::spawn` is infallible and returns unit.
    spawner.spawn(radio_task);
    spawner.spawn(node_task);
    #[cfg(not(any(feature = "ble-api-proof", feature = "wifi-api-proof")))]
    spawner.spawn(usb_pairing_task);
    #[cfg(feature = "wifi-api-proof")]
    spawner.spawn(wifi_api_task);
    #[cfg(feature = "ble-api-proof")]
    if let Some(ble_api_task) = ble_api_task {
        spawner.spawn(ble_api_task);
    }
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE
        .record_composition_ready(monotonic_us().saturating_sub(runtime_measurement_started_us));
    #[cfg(not(any(feature = "ble-api-proof", feature = "wifi-api-proof")))]
    info!(
        "e290-node stage=composition status=PASS tasks=3 interfaces=1 primary_transport=lora future_transport_actors=deferred node_journal=mounted resident_storage_available={storage_service_available} durable_rns_inbox_available={inbox_service_available} rns_inbox_capacity=1 lxmf_store_available={lxmf_service_available} lxmf_index_slots={} lxmf_delivery_admission={lxmf_delivery_admission} lxmf_volatile_placement=external-psram lxmf_volatile_bytes={lxmf_volatile_bytes} lxmf_delayed_proof_placement=external-psram lxmf_delayed_proof_slots={} lxmf_delayed_proof_bytes={delayed_proof_storage_bytes} application_event_placement=internal-static credential_state={credential_boot_state:?} credential_revision={credential_revision:?} credential_authority_publishable={credential_authority_publishable} credential_mutation_eligible={credential_mutation_eligible} credential_pairing_policy_resident={credential_pairing_policy_available} credential_initialization={credential_initialization_status:?} preauth_initialization_bearer=usb-serial-jtag preauth_live_pairing_bearer=usb-serial-jtag pairing_button_gpio=21 authenticated_local_api=node-dispatch bearer_session=usb-authenticated-single-flight credential_offset=0x{:x} credential_len=0x{:x} durable_runtime_bytes={} admission=session-selected runtime_patch={} flash_assumption_bytes=16777216",
        config::LXMF_INDEX_SLOTS,
        config::LXMF_DELAYED_PROOF_SLOTS,
        credential_binding.absolute_offset(),
        credential_binding.length(),
        config::DURABLE_RUNTIME_BYTES,
        env!("RETICULUM_ESP_RTOS_MAIN_STACK_PATCH"),
    );
    #[cfg(feature = "ble-api-proof")]
    info!(
        "e290-node stage=composition status=PASS tasks={} interfaces=1 primary_transport=lora local_api_profile=ble-api-proof ble_api_task_available={ble_api_task_available} usb=boot-quarantined ble_max_centrals=1 ble_pairing=disabled ble_tx=indicate ble_rx=write-with-response authenticated_local_api=node-dispatch bearer_session=ble-authenticated-single-flight session_suite=ble-gatt-qualification node_journal=mounted resident_storage_available={storage_service_available} durable_rns_inbox_available={inbox_service_available} lxmf_store_available={lxmf_service_available} lxmf_delivery_admission={lxmf_delivery_admission} credential_state={credential_boot_state:?} credential_revision={credential_revision:?} credential_authority_publishable={credential_authority_publishable} credential_mutation_eligible={credential_mutation_eligible} credential_pairing_policy_resident={credential_pairing_policy_available} credential_initialization={credential_initialization_status:?} credential_offset=0x{:x} credential_len=0x{:x} durable_runtime_bytes={} runtime_patch={}",
        if ble_api_task_available { 3 } else { 2 },
        credential_binding.absolute_offset(),
        credential_binding.length(),
        config::DURABLE_RUNTIME_BYTES,
        env!("RETICULUM_ESP_RTOS_MAIN_STACK_PATCH"),
    );
    #[cfg(feature = "wifi-api-proof")]
    info!(
        "e290-node stage=composition status=PASS tasks=5 interfaces=1 primary_transport=lora local_api_profile=wifi-api-proof usb=boot-quarantined wifi_softap_ssid={:?} wifi_gateway={}.{}.{}.{} wifi_prefix={} wifi_tcp_port={} wifi_dhcp=enabled wifi_max_clients=1 wifi_pairing=disabled authenticated_local_api=node-dispatch bearer_session=wifi-authenticated-single-flight session_suite=wifi-qualification node_journal=mounted resident_storage_available={storage_service_available} durable_rns_inbox_available={inbox_service_available} lxmf_store_available={lxmf_service_available} lxmf_delivery_admission={lxmf_delivery_admission} credential_state={credential_boot_state:?} credential_revision={credential_revision:?} credential_authority_publishable={credential_authority_publishable} credential_mutation_eligible={credential_mutation_eligible} credential_pairing_policy_resident={credential_pairing_policy_available} credential_initialization={credential_initialization_status:?} credential_offset=0x{:x} credential_len=0x{:x} durable_runtime_bytes={} runtime_patch={}",
        wifi_api_profile::softap_ssid(base_mac_eui48),
        wifi_api_profile::GATEWAY_IPV4[0],
        wifi_api_profile::GATEWAY_IPV4[1],
        wifi_api_profile::GATEWAY_IPV4[2],
        wifi_api_profile::GATEWAY_IPV4[3],
        wifi_api_profile::GATEWAY_PREFIX_LEN,
        wifi_api_profile::RDA1_TCP_PORT,
        credential_binding.absolute_offset(),
        credential_binding.length(),
        config::DURABLE_RUNTIME_BYTES,
        env!("RETICULUM_ESP_RTOS_MAIN_STACK_PATCH"),
    );

    #[cfg(feature = "runtime-measurement-hil")]
    {
        const SAMPLE_INTERVAL_US: u64 = 1_000_000;
        let mut next_sample_us = monotonic_us().saturating_add(SAMPLE_INTERVAL_US);
        loop {
            Timer::at(Instant::from_micros(next_sample_us)).await;
            let sampled_at_us = monotonic_us();
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE
                .record_measurement_lateness(sampled_at_us.saturating_sub(next_sample_us));
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_uptime_ms(
                sampled_at_us.saturating_sub(runtime_measurement_started_us) / 1_000,
            );
            let measurement_work_started_us = monotonic_us();
            record_runtime_heap_snapshot(&esp_alloc::HEAP);
            record_runtime_stack_snapshot(&mut runtime_stack);
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_measurement_work(
                monotonic_us().saturating_sub(measurement_work_started_us),
            );
            next_sample_us = sampled_at_us.saturating_add(SAMPLE_INTERVAL_US);
        }
    }

    #[cfg(not(feature = "runtime-measurement-hil"))]
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

#[cfg(feature = "runtime-measurement-hil")]
fn record_runtime_heap_snapshot(heap: &EspHeap) {
    let stats = heap.stats();
    let mut internal_current_bytes = 0_u64;
    let mut internal_free_bytes = 0_u64;
    let mut external_current_bytes = 0_u64;
    let mut external_free_bytes = 0_u64;
    for region in stats.region_stats.iter().flatten() {
        if region.capabilities.contains(MemoryCapability::Internal) {
            internal_current_bytes = internal_current_bytes.saturating_add(region.used as u64);
            internal_free_bytes = internal_free_bytes.saturating_add(region.free as u64);
        }
        if region.capabilities.contains(MemoryCapability::External) {
            external_current_bytes = external_current_bytes.saturating_add(region.used as u64);
            external_free_bytes = external_free_bytes.saturating_add(region.free as u64);
        }
    }
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_heap_snapshot(RuntimeHeapSnapshot {
        total_bytes: stats.size as u64,
        current_bytes: stats.current_usage as u64,
        maximum_bytes: stats.max_usage as u64,
        free_bytes: stats.size.saturating_sub(stats.current_usage) as u64,
        internal_current_bytes,
        internal_free_bytes,
        external_current_bytes,
        external_free_bytes,
    });
}

#[cfg(feature = "runtime-measurement-hil")]
fn record_runtime_stack_snapshot(
    monitor: &mut runtime_measurement_stack_hil::StackWatermarkMonitor,
) {
    let metrics = monitor.sample();
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_stack_snapshot(RuntimeStackSnapshot {
        reserved_bytes: u64::from(metrics.stack_reserved_bytes),
        usable_bytes: u64::from(metrics.stack_usable_above_guard_bytes),
        painted_bytes: u64::from(metrics.startup_painted_bytes),
        high_water_bytes: u64::from(metrics.high_water_used_bytes),
        remaining_bytes: u64::from(metrics.minimum_remaining_above_guard_bytes),
        guard_offset_bytes: u64::from(metrics.stack_guard_offset_bytes),
        scan_valid: metrics.scan_valid,
        guard_intact: metrics.stack_guard_intact,
    });
}

#[cfg(feature = "runtime-measurement-hil")]
#[unsafe(no_mangle)]
fn _esp_alloc_alloc(
    heap: &::esp_alloc::EspHeap,
    _capabilities: ::esp_alloc::export::enumset::EnumSet<::esp_alloc::MemoryCapability>,
    pointer: usize,
    _size: usize,
) {
    let success = pointer != 0;
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_allocation(success);
    if success {
        // The allocator invokes this hook after releasing its lock. The stats
        // path is allocation-free, so it can safely capture exact regional
        // post-allocation minima without recursion.
        record_runtime_heap_snapshot(heap);
    }
}

#[cfg(feature = "runtime-measurement-hil")]
#[unsafe(no_mangle)]
fn _esp_alloc_dealloc(_heap: &::esp_alloc::EspHeap, _pointer: usize, _size: usize) {
    // esp-alloc calls this hook before deallocation. A pre-free snapshot cannot
    // improve a high-water/minimum-free bound, and the periodic sampler will
    // refresh current values after the free completes.
}

fn monotonic_us() -> u64 {
    Instant::now().as_micros()
}

fn inert_before_rtos() -> ! {
    // Embassy timers are unavailable until `esp_rtos::start`. These early
    // fail-stop paths run with RF held in reset and USB still quarantined.
    loop {
        core::hint::spin_loop();
    }
}

async fn inert_forever() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
    }
}
