//! First permanent LoRa-first Reticulum node image for the Vision Master E290.
//!
//! The executable composes one transport-neutral node task with one concrete
//! LoRa actor task. Before either is constructed, the sole flash owner safely
//! provisions or strictly mounts the submission journal and drives an explicit
//! bounded-history recovery gate to completion. The backend-independent durable
//! runtime then remains resident with the sole operation-scoped flash
//! coordinator. USB, BLE, Wi-Fi, LXMF client, NomadNet client and UI actors are
//! still deferred; they will attach as independent interface or client
//! capabilities rather than changing the LoRa ownership graph.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave RF hardware active"
)]
#![deny(clippy::large_stack_frames)]

mod node_task;
mod platform_storage;
mod radio_task;

use core::future::{Future, pending};

use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Delay, Duration, Instant, Timer, with_timeout};
use embedded_hal::digital::{Error as DigitalError, ErrorKind, ErrorType};
use embedded_hal_async::digital::Wait;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
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
use reticulum_device_identity_store::IdentityMirrorCoverage;
use reticulum_heltec_vision_master_e290_node::{
    config,
    credential_boot::CredentialBootState,
    durability_boot::{
        BUILD_JOURNAL_REPROVISION_POLICY, JournalReprovisionPolicy, announce_clock_policy,
        journal_boot_policy,
    },
    storage_device_id_from_eui48,
};
use reticulum_interface_router::InterfaceFabric;
use reticulum_node_core::{
    InboundProofPolicy, NodeConfig, NodeCore, NodeIdentity, NodeInstanceId, OrdinaryBufferPool,
    OrdinaryPacketBuffer, PacketInterfaceId, TxPacketBuffer,
};
use reticulum_radio_lora_phy::IrqTimestampCapture;
use reticulum_radio_tx_dispatch::{ExactLoRaAirtimePolicy, SoleRadioTxDispatcher};
use reticulum_tx_handoff::{AuthorizedFrameHandoff, DataPermitHandoff, OrdinaryPermitHandoff};
use reticulum_tx_supervisor::{
    DataRouterCoordinator, NodeInterfaceSupervisor, OrdinaryRouterCoordinator,
};
use static_cell::StaticCell;

use crate::platform_storage::{
    BootCredentialStore, ProductCredentialInitializationPort, ProductFlashOwner,
    ProductStorageCoordinator,
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
static SUPERVISOR: StaticCell<ProductSupervisor> = StaticCell::new();
static DISPATCHER: StaticCell<ProductDispatcher> = StaticCell::new();
static FLASH_STORAGE: StaticCell<FlashStorage<'static>> = StaticCell::new();
static STORAGE_COORDINATOR: StaticCell<ProductStorageCoordinator> = StaticCell::new();

pub(crate) static RADIO_READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub(crate) static LORA_ONLINE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

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

#[allow(
    clippy::large_stack_frames,
    reason = "one-shot composition moves every fixed owner into static task storage"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let hal = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(hal);

    // Establish the complete E290 RF interlock before logging, entropy,
    // executor startup, allocation or radio construction.
    let radio_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let radio_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());

    esp_println::logger::init_logger_from_env();
    let base_mac = esp_hal::efuse::base_mac_address();
    let base_mac_bytes = base_mac.as_bytes();
    let storage_device_id = storage_device_id_from_eui48([
        base_mac_bytes[0],
        base_mac_bytes[1],
        base_mac_bytes[2],
        base_mac_bytes[3],
        base_mac_bytes[4],
        base_mac_bytes[5],
    ]);
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
    let (_, psram_bytes) = psram.raw_parts();
    if !(config::MINIMUM_PSRAM_BYTES..=config::MAXIMUM_PSRAM_BYTES).contains(&psram_bytes) {
        error!(
            "e290-node stage=psram status=FAIL expected_bytes={}..={} actual_bytes={psram_bytes}",
            config::MINIMUM_PSRAM_BYTES,
            config::MAXIMUM_PSRAM_BYTES,
        );
        inert_forever().await
    }
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: config::INTERNAL_HEAP_BYTES);
    esp_alloc::psram_allocator!(&psram);
    info!(
        "e290-node stage=psram status=PASS bytes={psram_bytes} mode=auto minimum_qualified_bytes={} ownership_state=internal-static",
        config::MINIMUM_PSRAM_BYTES,
    );

    let timers = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);

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
    let identity_preflight = match flash_owner.inspect_identity() {
        Ok(preflight) => preflight,
        Err(reason) => {
            error!("e290-node stage=identity-preflight status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };
    let fresh_clock_policy = announce_clock_policy(identity_preflight);
    let journal_reprovision_policy = BUILD_JOURNAL_REPROVISION_POLICY;
    let journal_policy = journal_boot_policy(identity_preflight, journal_reprovision_policy);
    info!(
        "e290-node stage=journal-reprovision-policy status=SELECTED build_policy={} explicit={} migration_mutation_scope=node_journal erased_media_only={} automatic_erase=false identity_config_preserved=true normal_boot_clock_reservation=unchanged",
        journal_reprovision_policy.log_label(),
        matches!(
            journal_reprovision_policy,
            JournalReprovisionPolicy::ExplicitErasedSchema2Development
        ),
        matches!(
            journal_policy,
            reticulum_heltec_vision_master_e290_node::durability_boot::JournalBootPolicy::ProvisionErasedSchema2Development
        ),
    );
    info!(
        "e290-node stage=identity-preflight status=PASS state={identity_preflight:?} announce_clock_policy={fresh_clock_policy:?} journal_reprovision_policy={journal_reprovision_policy:?} journal_policy={journal_policy:?} writes=0 erases=0"
    );
    match flash_owner.provision_node_journal(journal_policy) {
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
    let boot_epoch = match flash_owner.reserve_announce_epoch(fresh_clock_policy) {
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
    let boot_identity = match flash_owner.boot_identity(&mut bootstrap_rng) {
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

    let submission_runtime = match flash_owner.mount_node_runtime(u64::from(announce_epoch.get())) {
        Ok((runtime, journal_report)) => {
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
        Err(reason) => {
            error!(
                "e290-node stage=node-journal-recovery status=DISABLED boot_sequence={} phase={} reason={reason:?} lora_routing=continue local_submission_admission=closed",
                announce_epoch.get(),
                reason.stage(),
            );
            None
        }
    };
    let storage_coordinator =
        flash_owner.into_storage_coordinator(submission_runtime, credential_boot);
    let storage_service_available = storage_coordinator.submission_service_available();
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
    let mut instance_bytes = [0_u8; 16];
    bootstrap_rng.fill_bytes(&mut instance_bytes);
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
        false,
    ) {
        Ok(descriptor) => descriptor,
        Err(reason) => {
            error!("e290-node stage=interface-register status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };
    let (interface, data_permit, ordinary_permit) = actor.into_parts();
    let (tx_interface, ingress) = interface.into_parts();
    let ingress_authority = match ingress.bind_ingress(offline_descriptor) {
        Ok(authority) => authority,
        Err(reason) => {
            error!("e290-node stage=interface-bind status=FAIL reason={reason:?}");
            inert_forever().await
        }
    };

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

    let radio_task = match radio_task::run(dispatcher, ingress, ingress_authority) {
        Ok(task) => task,
        Err(_) => {
            error!("e290-node stage=spawn status=FAIL task=lora");
            inert_forever().await
        }
    };
    let node_task = match node_task::run(
        supervisor,
        storage_coordinator,
        frame_node,
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
    // This Embassy version reports pool exhaustion while constructing each
    // SpawnToken above; `Spawner::spawn` is infallible and returns unit.
    spawner.spawn(radio_task);
    spawner.spawn(node_task);
    info!(
        "e290-node stage=composition status=PASS tasks=2 interfaces=1 primary_transport=lora future_transport_actors=deferred node_journal=mounted resident_storage_available={storage_service_available} credential_state={credential_boot_state:?} credential_revision={credential_revision:?} credential_authority_publishable={credential_authority_publishable} credential_mutation_eligible={credential_mutation_eligible} credential_pairing_policy_resident={credential_pairing_policy_available} credential_initialization={credential_initialization_status:?} external_local_api=closed local_api_bearer=absent local_api_session=absent credential_offset=0x{:x} credential_len=0x{:x} durable_runtime_bytes={} admission=deferred runtime_patch={} flash_assumption_bytes=16777216",
        credential_binding.absolute_offset(),
        credential_binding.length(),
        config::DURABLE_RUNTIME_BYTES,
        env!("RETICULUM_ESP_RTOS_MAIN_STACK_PATCH"),
    );

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

async fn inert_forever() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
    }
}
