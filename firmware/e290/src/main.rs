//! Standalone PRNS appliance firmware for the Heltec Vision Master E290.
//!
//! PRNS owns Reticulum routing, proofs, Links, Resources, and packet-interface
//! behavior. This executable supplies board hardware, one generic product-state
//! arena, and ordinary application callbacks without a second protocol engine
//! or alpha-era admission policy.

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

#[cfg(feature = "display")]
mod display_task;
mod prns_bluetooth;
mod prns_product_task;
mod prns_target;
mod product_store;
mod shared_flash;
#[cfg(feature = "gateway")]
mod wifi_station_task;

use core::alloc::Layout;
use core::ptr::NonNull;
use core::{future::Future, mem};

use allocator_api2::alloc::{AllocError, Allocator};
use allocator_api2::{boxed::Box, vec::Vec};
use embassy_executor::Spawner;
use embassy_time::{Delay, Duration, Timer, with_timeout};
use embedded_hal::digital::{Error as DigitalError, ErrorKind, ErrorType};
use embedded_hal_async::digital::Wait;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_alloc::ExternalMemory;
use esp_backtrace as _;
use esp_hal::{
    Async,
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
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
#[cfg(feature = "gateway")]
use rand_core::RngCore;
#[cfg(feature = "display")]
use reticulum_appliance_display_model::{
    DisplayCommand, DisplayCompositionState, DisplayHomeSnapshot, DisplayLabel, DisplaySetupState,
    DisplayViewKind,
};
#[cfg(feature = "display")]
use reticulum_e290_firmware::board::EINK_SPI_FREQUENCY_HZ;
use reticulum_e290_firmware::prns_node::EngineStorage as PrnsEngineStorage;
use reticulum_e290_firmware::product_config;
#[cfg(not(feature = "display"))]
use reticulum_e290_firmware::product_config::MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES;
use reticulum_e290_firmware::product_identity::IdentityMirrorCoverage;
use reticulum_e290_firmware::product_outbox::OutboxIndexSlot;
#[cfg(feature = "gateway")]
use reticulum_e290_firmware::wifi_station_profile::WifiStationStatusCell;
#[cfg(feature = "display")]
use reticulum_e290_firmware::{
    display_handoff::{DisplayHandoff, DisplayTelemetryHandoff},
    product_config::MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES,
};
#[cfg(feature = "display")]
use reticulum_eink_ssd1680::{E290FrameBuffer, Ssd1680};
use reticulum_lxmf_store::LxmfStoreIndexSlot;
use static_cell::StaticCell;

use crate::product_store::ProductStore;

#[cfg(debug_assertions)]
compile_error!("the permanent E290 node must be built with --release");

type E290SpiDevice = ExclusiveDevice<Spi<'static, Async>, Output<'static>, Delay>;

/// Default-able PRNS allocator backed exclusively by mapped PSRAM.
#[derive(Clone, Copy, Debug, Default)]
struct E290PrnsPsramAlloc;

// SAFETY: allocation and deallocation are forwarded unchanged to esp-alloc's
// mapped external-memory allocator. This zero-sized wrapper owns no state.
unsafe impl Allocator for E290PrnsPsramAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        ExternalMemory.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the pointer and layout came from the forwarded allocator.
        unsafe { ExternalMemory.deallocate(ptr, layout) };
    }
}

const _: () = assert!(
    match <PrnsEngineStorage<E290PrnsPsramAlloc> as personal_rns::storage::StorageLayout>::LIMITS
        .resource_transfer_bytes
    {
        personal_rns::storage::StorageCapacity::Fixed(bytes) => bytes == 8_192,
        personal_rns::storage::StorageCapacity::Dynamic => false,
    }
);

#[cfg(feature = "display")]
static DISPLAY_HANDOFF: StaticCell<
    DisplayHandoff<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex>,
> = StaticCell::new();
#[cfg(feature = "display")]
static DISPLAY_TELEMETRY_HANDOFF: StaticCell<
    DisplayTelemetryHandoff<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex>,
> = StaticCell::new();
static PRODUCT_FLASH: StaticCell<shared_flash::E290ProductFlash> = StaticCell::new();
static PRODUCT_STORE: StaticCell<ProductStore> = StaticCell::new();
static LXMF_ANNOUNCE_DATA: StaticCell<[u8; MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES]> =
    StaticCell::new();
#[cfg(feature = "gateway")]
static WIFI_STATION_STATUS: StaticCell<WifiStationStatusCell> = StaticCell::new();
static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();

#[cfg(feature = "display")]
const DISPLAY_HOME_DEADLINE_MS: u64 = 15_000;

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
    match with_timeout(
        Duration::from_millis(product_config::BUSY_PIN_WATCHDOG_MS),
        future,
    )
    .await
    {
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

#[cfg(feature = "display")]
#[inline(never)]
fn allocate_display_frame() -> Option<Box<E290FrameBuffer, ExternalMemory>> {
    Box::try_new_in(E290FrameBuffer::new_white(), ExternalMemory).ok()
}

#[inline(never)]
fn allocate_lxmf_index_or_inert(
    psram_start: usize,
    psram_end: usize,
) -> &'static mut [LxmfStoreIndexSlot] {
    let mut index = Vec::new_in(ExternalMemory);
    if index
        .try_reserve_exact(product_config::LXMF_INDEX_SLOTS)
        .is_err()
    {
        error!(
            "e290-node stage=lxmf-index status=FAIL reason=external-allocation slots={} bytes={}",
            product_config::LXMF_INDEX_SLOTS,
            product_config::LXMF_INDEX_STORAGE_BYTES,
        );
        inert_large_construction()
    }
    for _ in 0..product_config::LXMF_INDEX_SLOTS {
        index.push(LxmfStoreIndexSlot::new());
    }
    let bytes = mem::size_of_val(index.as_slice());
    let start = index.as_ptr() as usize;
    let Some(end) = start.checked_add(bytes) else {
        error!("e290-node stage=lxmf-index status=FAIL reason=address-overflow");
        inert_large_construction()
    };
    if start < psram_start
        || end > psram_end
        || !start.is_multiple_of(mem::align_of::<LxmfStoreIndexSlot>())
    {
        error!(
            "e290-node stage=lxmf-index status=FAIL reason=external-address start=0x{start:08x} end=0x{end:08x}"
        );
        inert_large_construction()
    }
    info!(
        "e290-node stage=lxmf-index status=PASS slots={} bytes={bytes} start=0x{start:08x} end=0x{end:08x}",
        index.len(),
    );
    index.leak()
}

#[inline(never)]
fn allocate_lxmf_outbox_index_or_inert(
    psram_start: usize,
    psram_end: usize,
) -> &'static mut [OutboxIndexSlot] {
    let mut index = Vec::new_in(ExternalMemory);
    if index
        .try_reserve_exact(product_config::LXMF_OUTBOX_INDEX_SLOTS)
        .is_err()
    {
        error!(
            "e290-node stage=lxmf-outbox-index status=FAIL reason=external-allocation slots={} bytes={}",
            product_config::LXMF_OUTBOX_INDEX_SLOTS,
            product_config::LXMF_OUTBOX_INDEX_STORAGE_BYTES,
        );
        inert_large_construction()
    }
    for _ in 0..product_config::LXMF_OUTBOX_INDEX_SLOTS {
        index.push(OutboxIndexSlot::EMPTY);
    }
    let bytes = mem::size_of_val(index.as_slice());
    let start = index.as_ptr() as usize;
    let Some(end) = start.checked_add(bytes) else {
        error!("e290-node stage=lxmf-outbox-index status=FAIL reason=address-overflow");
        inert_large_construction()
    };
    if start < psram_start
        || end > psram_end
        || !start.is_multiple_of(mem::align_of::<OutboxIndexSlot>())
    {
        error!(
            "e290-node stage=lxmf-outbox-index status=FAIL reason=external-address start=0x{start:08x} end=0x{end:08x}"
        );
        inert_large_construction()
    }
    info!(
        "e290-node stage=lxmf-outbox-index status=PASS slots={} bytes={bytes} start=0x{start:08x} end=0x{end:08x}",
        index.len(),
    );
    index.leak()
}

const DEFAULT_LXMF_DISPLAY_NAME_PREFIX: &[u8] = b"Metalbeard E290 ";
const DEFAULT_LXMF_DISPLAY_NAME_BYTES: usize = DEFAULT_LXMF_DISPLAY_NAME_PREFIX.len() + 12;

fn default_lxmf_display_name(mac: [u8; 6]) -> [u8; DEFAULT_LXMF_DISPLAY_NAME_BYTES] {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut name = [0_u8; DEFAULT_LXMF_DISPLAY_NAME_BYTES];
    name[..DEFAULT_LXMF_DISPLAY_NAME_PREFIX.len()]
        .copy_from_slice(DEFAULT_LXMF_DISPLAY_NAME_PREFIX);
    for (index, byte) in mac.into_iter().enumerate() {
        let offset = DEFAULT_LXMF_DISPLAY_NAME_PREFIX.len() + index * 2;
        name[offset] = HEX[usize::from(byte >> 4)];
        name[offset + 1] = HEX[usize::from(byte & 0x0f)];
    }
    name
}

fn lxmf_announce_app_data(mac: [u8; 6]) -> &'static [u8] {
    let name = default_lxmf_display_name(mac);
    let name = core::str::from_utf8(&name).expect("the fixed LXMF display name is UTF-8");
    let mut encoded = [0_u8; MAX_LXMF_DELIVERY_ANNOUNCE_APP_DATA_BYTES];
    let len = product_config::encode_lxmf_delivery_announce_app_data(name, &mut encoded);
    &LXMF_ANNOUNCE_DATA.init(encoded)[..len]
}

#[cfg(feature = "display")]
fn display_device_suffix(base_mac: [u8; 6]) -> DisplayLabel {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut suffix = [0_u8; 6];
    for (index, byte) in base_mac[3..].iter().copied().enumerate() {
        suffix[index * 2] = HEX[usize::from(byte >> 4)];
        suffix[index * 2 + 1] = HEX[usize::from(byte & 0x0f)];
    }
    DisplayLabel::from_bytes(&suffix).expect("the fixed hexadecimal device suffix fits")
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
    reason = "the linked ELF audits the complete bounded startup call path"
)]
#[embassy_executor::task]
async fn product_main(spawner: Spawner) -> ! {
    let hal = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(hal);
    let _usb_serial_jtag_diagnostics_owner = peripherals.USB_DEVICE;

    // Hold the radio in reset before any fallible startup work.
    let radio_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let radio_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    esp_println::logger::init_logger(log::LevelFilter::Info);

    let base_mac = esp_hal::efuse::base_mac_address();
    let bytes = base_mac.as_bytes();
    let base_mac_eui48 = [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]];
    info!("e290-node stage=boot status=START runtime=prns base_mac={base_mac} rf_state=reset-low");

    let psram = Psram::new(
        peripherals.PSRAM,
        PsramConfig {
            mode: PsramMode::Auto,
            size: PsramSize::AutoDetect,
            core_clock: None,
            flash_frequency: FlashFreq::FlashFreq40m,
            ram_frequency: SpiRamFreq::Freq40m,
        },
    );
    let (psram_start, psram_bytes) = psram.raw_parts();
    let psram_start = psram_start as usize;
    let Some(psram_end) = psram_start.checked_add(psram_bytes) else {
        error!("e290-node stage=psram status=FAIL reason=range-overflow");
        inert_before_rtos()
    };
    if !(product_config::MINIMUM_PSRAM_BYTES..=product_config::MAXIMUM_PSRAM_BYTES)
        .contains(&psram_bytes)
    {
        error!(
            "e290-node stage=psram status=FAIL expected={}..={} actual={psram_bytes}",
            product_config::MINIMUM_PSRAM_BYTES,
            product_config::MAXIMUM_PSRAM_BYTES,
        );
        inert_before_rtos()
    }
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: product_config::INTERNAL_HEAP_BYTES);
    #[cfg(feature = "gateway")]
    esp_alloc::heap_allocator!(size: product_config::WIFI_INTERNAL_HEAP_BYTES);
    esp_alloc::psram_allocator!(&psram);
    info!(
        "e290-node stage=psram status=PASS bytes={psram_bytes} internal_heap={} wifi_heap={}",
        product_config::INTERNAL_HEAP_BYTES,
        product_config::WIFI_INTERNAL_HEAP_BYTES,
    );

    let timers = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);

    #[cfg(feature = "display")]
    let mut display_publisher = None;
    #[cfg(feature = "display")]
    let mut display_telemetry_publisher = None;
    #[cfg(feature = "display")]
    {
        let (mut publisher, receiver) = DISPLAY_HANDOFF.init(DisplayHandoff::new()).split();
        let (telemetry_publisher, telemetry_receiver) = DISPLAY_TELEMETRY_HANDOFF
            .init(DisplayTelemetryHandoff::new())
            .split();
        if let Some(frame) = allocate_display_frame() {
            let display_power =
                Output::new(peripherals.GPIO18, Level::Low, OutputConfig::default());
            let chip_select = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
            let data_command = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
            let reset = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
            let busy = display_task::DisplayBusy::new(Input::new(
                peripherals.GPIO6,
                InputConfig::default(),
            ));
            let spi = Spi::new(
                peripherals.SPI3,
                SpiConfig::default()
                    .with_frequency(Rate::from_hz(EINK_SPI_FREQUENCY_HZ))
                    .with_mode(SpiMode::_0),
            )
            .ok()
            .map(|spi| {
                spi.with_sck(peripherals.GPIO2)
                    .with_mosi(peripherals.GPIO1)
                    .into_async()
            });
            match spi.and_then(|spi| ExclusiveDevice::new(spi, chip_select, Delay).ok()) {
                Some(spi) => {
                    let display = Ssd1680::new(spi, data_command, reset, busy, Delay);
                    let command = DisplayCommand::ShowBooting {
                        label: DisplayLabel::new("Reticulum E290")
                            .expect("the fixed display label fits"),
                    };
                    let _ = publisher.publish_latest(command);
                    match display_task::run(
                        display,
                        display_power,
                        frame,
                        receiver,
                        telemetry_receiver,
                    ) {
                        Ok(task) => {
                            spawner.spawn(task);
                            display_publisher = Some(publisher);
                            display_telemetry_publisher = Some(telemetry_publisher);
                        }
                        Err(_) => {
                            error!("e290-node stage=display status=DISABLED reason=task-capacity")
                        }
                    }
                }
                None => {
                    error!("e290-node stage=display status=DISABLED reason=spi-initialization")
                }
            }
        } else {
            error!("e290-node stage=display status=DISABLED reason=external-allocation");
        }
    }

    let lxmf_index = allocate_lxmf_index_or_inert(psram_start, psram_end);
    let lxmf_outbox_index = allocate_lxmf_outbox_index_or_inert(psram_start, psram_end);
    let _entropy_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    let mut bootstrap_rng = match Trng::try_new() {
        Ok(rng) => rng,
        Err(reason) => {
            error!("e290-node stage=entropy status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };

    let shared_flash::E290FlashClients {
        product,
        prns: prns_flash,
    } = shared_flash::share_flash(FlashStorage::new(peripherals.FLASH));
    let product_flash = PRODUCT_FLASH.init(product);
    let boot = match ProductStore::boot(
        product_flash,
        base_mac_eui48,
        &mut bootstrap_rng,
        lxmf_index,
        lxmf_outbox_index,
    ) {
        Ok(boot) => boot,
        Err(reason) => {
            error!("e290-node stage=product-store status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };
    if boot.identity_report.coverage() != IdentityMirrorCoverage::Redundant {
        warn!(
            "e290-node stage=identity status=DEGRADED coverage={:?} repair_deferred={} action=continue-with-valid-mirror",
            boot.identity_report.coverage(),
            boot.identity_report.repair_deferred(),
        );
    }
    let prns_identity = personal_rns::identity::Zeroizing::new(*boot.identity.as_bytes());
    let identity_hash =
        personal_rns::identity::PrivateIdentityMaterial::from_slice(&prns_identity[..])
            .expect("the product identity and PRNS use the same key length")
            .identity_hash();
    info!(
        "e290-node stage=identity status=PASS source={:?} coverage={:?} writes={} erases={} identity_hash={identity_hash:02x?}",
        boot.identity_report.source(),
        boot.identity_report.coverage(),
        boot.identity_write_calls,
        boot.identity_erase_calls,
    );

    let network_state = boot.store.network_state();
    let lora_profile = boot.store.lora_profile();
    let lxmf_available = boot.store.lxmf_available();
    let lxmf_messages = boot.store.lxmf_message_count();
    let lxmf_extents = boot.store.lxmf_consumed_extents();
    let rmap_enabled = boot.store.rmap_discovery_enabled();
    let announce_app_data = lxmf_announce_app_data(base_mac_eui48);
    #[cfg(feature = "display")]
    let display_setup = match boot.store.management_state() {
        product_store::ProductManagementState::Available {
            identity_count: 0, ..
        } => DisplaySetupState::EnrollmentRequired,
        product_store::ProductManagementState::Available { .. } => DisplaySetupState::Enrolled,
        product_store::ProductManagementState::Unavailable => DisplaySetupState::Unavailable,
    };
    #[cfg(feature = "display")]
    let display_appliance_label = boot
        .store
        .appliance_label()
        .and_then(|name| DisplayLabel::new(name.as_str()).ok());
    #[cfg(feature = "display")]
    let display_uncollected_messages = boot
        .store
        .lxmf_mailbox_status()
        .map_or(0, |status| status.uncollected_count());
    #[cfg(feature = "gateway")]
    let wifi_plan = boot.store.wifi_station_boot_plan();
    #[cfg(feature = "gateway")]
    let tcp_bootstrap = boot.store.wifi_tcp_bootstrap();
    let product_store = PRODUCT_STORE.init(boot.store);
    info!(
        "e290-node stage=product-store status=PASS arena=product_state network={network_state:?} lxmf_available={lxmf_available} lxmf_messages={lxmf_messages:?} lxmf_extents={lxmf_extents:?} rmap={rmap_enabled} reset_policy=fresh-provisioning"
    );

    #[cfg(feature = "gateway")]
    let wifi_seed =
        (u64::from(bootstrap_rng.next_u32()) << 32) | u64::from(bootstrap_rng.next_u32());
    let spi = match Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(product_config::SPI_FREQUENCY_HZ))
            .with_mode(SpiMode::_0),
    ) {
        Ok(spi) => spi,
        Err(_) => {
            error!("e290-node stage=radio-spi status=FAIL");
            inert_forever().await
        }
    }
    .with_sck(peripherals.GPIO9)
    .with_mosi(peripherals.GPIO10)
    .with_miso(peripherals.GPIO11)
    .into_async();
    let spi = match ExclusiveDevice::new(spi, radio_nss, Delay) {
        Ok(spi) => spi,
        Err(_) => {
            error!("e290-node stage=radio-spi-device status=FAIL");
            inert_forever().await
        }
    };
    let radio = prns_target::radio(
        spi,
        BoundedBusy::new(Input::new(peripherals.GPIO13, InputConfig::default())),
        Input::new(peripherals.GPIO14, InputConfig::default()),
        radio_reset,
    );
    let lora_profile = match reticulum_e290_firmware::prns_lora::prns_radio_profile(lora_profile) {
        Ok(profile) => profile,
        Err(reason) => {
            error!("e290-node stage=radio-profile status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };

    #[cfg(feature = "gateway")]
    let wifi_station_status = WIFI_STATION_STATUS.init(WifiStationStatusCell::new());
    #[cfg(feature = "gateway")]
    let prns_tcp = {
        let stack = wifi_station_task::start(
            spawner,
            peripherals.WIFI,
            wifi_seed,
            wifi_plan,
            wifi_station_status,
        );
        match (stack, tcp_bootstrap) {
            (Some(stack), Some(bootstrap)) => match prns_target::tcp(stack, bootstrap) {
                Ok(tcp) => Some(tcp),
                Err(reason) => {
                    warn!("e290-node stage=prns-tcp status=DISABLED reason={reason:?}");
                    None
                }
            },
            _ => None,
        }
    };
    let enrollment_button = Input::new(
        peripherals.GPIO21,
        InputConfig::default().with_pull(Pull::Up),
    );
    let bluetooth_identity = prns_bluetooth::identity_from_node_secret(&prns_identity);
    let mut target = match prns_target::start(
        spawner,
        radio,
        #[cfg(feature = "gateway")]
        prns_tcp,
        true,
        prns_flash,
        prns_identity.clone(),
        &prns_identity,
        lora_profile,
        announce_app_data,
        reticulum_e290_firmware::prns_applications::ApplicationProfile::new(
            lxmf_available,
            rmap_enabled,
        ),
    ) {
        Ok(target) => target,
        Err(reason) => {
            error!("e290-node stage=prns-composition status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };
    let bluetooth_started = match target.bluetooth.take() {
        Some(bluetooth) => match esp_radio::ble::controller::BleConnector::new(
            peripherals.BT,
            prns_bluetooth::controller_config(),
        ) {
            Ok(connector) => prns_bluetooth::spawn(
                spawner,
                connector,
                base_mac_eui48,
                bluetooth_identity,
                bluetooth.fleet,
                bluetooth.shared,
            ),
            Err(reason) => {
                warn!("e290-node stage=prns-bluetooth status=DISABLED reason={reason:?}");
                false
            }
        },
        None => false,
    };
    info!(
        "e290-node stage=prns-composition status=PASS lora=internal tcp={:?} bluetooth_auto={} management={:02x?} lxmf={:?} nomad={:02x?} rmap={:?} proof_policy=prns-default",
        target.tcp,
        bluetooth_started,
        target.management.as_bytes(),
        target.lxmf.map(|destination| *destination.as_bytes()),
        target.nomad.as_bytes(),
        target.rmap.map(|destination| *destination.as_bytes()),
    );

    #[cfg(feature = "display")]
    if let Some(publisher) = display_publisher.as_mut() {
        let snapshot = DisplayHomeSnapshot::new(
            DisplayLabel::new("Reticulum E290").expect("the fixed product label fits"),
            display_device_suffix(base_mac_eui48),
            display_setup,
            DisplayCompositionState::Configured,
            if bluetooth_started {
                DisplayCompositionState::Configured
            } else {
                DisplayCompositionState::Unavailable
            },
            if target.lxmf.is_some() {
                DisplayCompositionState::Configured
            } else {
                DisplayCompositionState::Unavailable
            },
            DisplayCompositionState::Configured,
        )
        .with_appliance_label(display_appliance_label)
        .with_uncollected_messages(display_uncollected_messages);
        match publisher.publish_latest(DisplayCommand::ShowHome { snapshot }) {
            Ok(request_id) => {
                info!(
                    "e290-node stage=display-request status=QUEUED request={} view=Home",
                    request_id.sequence(),
                );
                match with_timeout(
                    Duration::from_millis(DISPLAY_HOME_DEADLINE_MS),
                    publisher.wait_for_rendered_completion(request_id, DisplayViewKind::Home),
                )
                .await
                {
                    Ok(Ok(_)) => info!(
                        "e290-node stage=display-home status=READY request={} view=Home",
                        request_id.sequence(),
                    ),
                    Ok(Err(reason)) => error!(
                        "e290-node stage=display-home status=DISABLED request={} reason={reason:?}",
                        request_id.sequence(),
                    ),
                    Err(_) => error!(
                        "e290-node stage=display-home status=DISABLED request={} reason=completion-timeout deadline_ms={DISPLAY_HOME_DEADLINE_MS}",
                        request_id.sequence(),
                    ),
                }
            }
            Err(_) => error!(
                "e290-node stage=display-request status=FAIL reason=request-id-exhausted view=Home"
            ),
        }
    }

    prns_product_task::run(
        target,
        product_store,
        enrollment_button,
        prns_identity,
        #[cfg(feature = "display")]
        display_telemetry_publisher,
        #[cfg(feature = "gateway")]
        wifi_station_status,
    )
    .await
}

fn inert_before_rtos() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[inline(never)]
fn inert_large_construction() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

async fn inert_forever() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
    }
}
