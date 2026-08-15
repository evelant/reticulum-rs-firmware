//! Same-image two-board Reticulum semantic LoRa HIL for the E290.
//!
//! This hazardous image authorizes radio construction only for two exact
//! eFuse MACs. The initiator and responder exchange signed announces,
//! encrypted DATA, and its delivery proof through RNode framing. Every radio
//! operation and the complete exchange have finite deadlines; cancellation is
//! delegated to the fail-closed `SoleRnodeRadio` ownership contract.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave RF hardware active"
)]
#![deny(clippy::large_stack_frames)]

use core::{future::Future, num::NonZeroU64};

use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_hal::digital::{Error as DigitalError, ErrorKind, ErrorType};
use embedded_hal_async::digital::Wait;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig},
    rng::{Trng, TrngSource},
    spi::{
        Mode as SpiMode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use log::{error, info};
use rand_core::{CryptoRng, RngCore};
use reticulum_board_heltec_vision_master_e290_radio::{
    E290_NA915_DEV_CONFIGURATION, E290_NA915_DEV_PROFILE, E290_NA915_DEV_RAW_SET_TX_PARAMS_DBM,
    E290_NA915_DEV_REQUESTED_POWER_DBM, E290_PRIVATE_SYNC_WORD, E290_RX_SYMBOL_TIMEOUT, E290Radio,
};
use reticulum_heltec_vision_master_e290_semantic_hil::{
    HilRole, role_for_e290_base_mac, semantic_fixture_mac,
};
use reticulum_radio_interface::{
    BoundedRxOutcome, FrameSignal, RNODE_HW_MTU, SX1262_FRAME_MTU, SoleRadioFault, SoleRnodeRadio,
    TimedReceiveOutcome, TimedRnodeRx, frame_rns_packet,
};
use reticulum_radio_lora_phy::IrqTimestampCapture;
use reticulum_rns_rete::{
    ApplicationEvent, DestHash, IngressDisposition, InterfaceId, Packet, PacketType, RNS_MTU,
    ReceiptCandidate, ReceiptId, ReceiptKind, ReceiptReservationUnavailable, ReceiptTerminal,
    ReceiptTerminalReservation, ReceiptTerminalSink, TxTarget,
};
use reticulum_semantic_roundtrip_hil::{
    SEMANTIC_ROUNDTRIP_DATA_SEQUENCE, SEMANTIC_ROUNDTRIP_INITIATOR_ANNOUNCE_SEQUENCE,
    SEMANTIC_ROUNDTRIP_PAYLOAD_LEN, SEMANTIC_ROUNDTRIP_PROOF_SEQUENCE,
    SEMANTIC_ROUNDTRIP_RESPONDER_ANNOUNCE_SEQUENCE, SemanticRoundtripAction,
    SemanticRoundtripError, SemanticRoundtripNode, SemanticRoundtripPhase, SemanticRoundtripState,
    SemanticRoundtripStep, build_semantic_roundtrip_payload, prepare_semantic_roundtrip_announce,
    semantic_roundtrip_node_for_base_mac, semantic_roundtrip_peer_destination_for_base_mac,
    validate_semantic_roundtrip_frame, validate_semantic_roundtrip_payload,
};
use static_cell::StaticCell;

#[cfg(debug_assertions)]
compile_error!("the hazardous E290 semantic HIL must be built with --release");

const SPI_FREQUENCY_HZ: u32 = 1_000_000;
const BUSY_TIMEOUT_MS: u64 = 100;
const INITIATOR_STARTUP_DELAY_MS: u64 = 3_000;
const DATA_GUARD_DELAY_MS: u64 = 250;
const CAD_OPERATION_DEADLINE_MS: u64 = 500;
const RECEIVE_OPERATION_DEADLINE_MS: u64 = 1_750;
const TRANSMIT_OPERATION_DEADLINE_MS: u64 = 1_500;
const EXCHANGE_DEADLINE_SECONDS: u64 = 180;
const MAXIMUM_RX_WINDOWS: u8 = 48;
const INERT_HEARTBEAT_SECONDS: u64 = 30;
const HIL_INTERFACE: InterfaceId = InterfaceId(1);
const FRAGMENT_TIMEOUT_US: NonZeroU64 =
    NonZeroU64::new(1_500_000).expect("fragment timeout is non-zero");

static IRQ_TIMESTAMPS: IrqTimestampCapture = IrqTimestampCapture::new_monotonic_us(monotonic_us);
static SEMANTIC_NODE: StaticCell<Result<SemanticRoundtripNode, SemanticRoundtripError>> =
    StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

struct HexBytes<'a>(&'a [u8]);

impl core::fmt::Display for HexBytes<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

struct OptionalHexBytes<'a>(Option<&'a [u8]>);

impl core::fmt::Display for OptionalHexBytes<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(bytes) => HexBytes(bytes).fmt(formatter),
            None => formatter.write_str("none"),
        }
    }
}

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
    match with_timeout(Duration::from_millis(BUSY_TIMEOUT_MS), future).await {
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
    reason = "the one-shot entry point owns target peripherals and fixed radio buffers"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Establish the complete board-level RF interlock before logging, role
    // selection, timers, SPI, entropy, or radio construction.
    let radio_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let radio_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());

    esp_println::logger::init_logger_from_env();
    let base_mac = esp_hal::efuse::base_mac_address();
    let role = role_for_e290_base_mac(base_mac.as_bytes());
    info!(
        "e290-semantic-hil stage=mac-gate base_mac={} role={} exact_match={} radio_constructed=false spi_constructed=false rf_state=reset_low_nss_high",
        base_mac,
        role.label(),
        role != HilRole::Inert,
    );
    if role == HilRole::Inert {
        error!(
            "e290-semantic-hil stage=mac-gate status=DENIED reason=unknown-efuse-base-mac action=permanent-rf-inert"
        );
        loop {
            core::hint::spin_loop();
        }
    }

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    let timers = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);
    let _ = spawner;

    let _entropy_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    let mut rng = match Trng::try_new() {
        Ok(rng) => rng,
        Err(_) => {
            error!(
                "e290-semantic-hil stage=entropy status=FAIL action=permanent-rf-inert radio_constructed=false"
            );
            inert_forever().await
        }
    };

    info!(
        "e290-semantic-hil stage=profile status=PASS region=NA915 frequency_hz={} bandwidth_hz={} sf={} coding_rate_denominator={} preamble_symbols={} explicit_header={} crc={} iq_inverted={} sync_word=0x{:04x} sx1262_requested_output_dbm={} sx1262_raw_set_tx_params_dbm={} power_claim=configuration-target-not-calibration rx_symbol_timeout={} cad_operation_deadline_ms={} receive_operation_deadline_ms={} transmit_operation_deadline_ms={} exchange_deadline_seconds={}",
        E290_NA915_DEV_PROFILE.frequency_hz(),
        E290_NA915_DEV_PROFILE.bandwidth().hz(),
        E290_NA915_DEV_PROFILE.spreading_factor().factor(),
        E290_NA915_DEV_PROFILE.coding_rate().denom(),
        E290_NA915_DEV_PROFILE.preamble_symbols(),
        E290_NA915_DEV_PROFILE.explicit_header(),
        E290_NA915_DEV_PROFILE.crc(),
        E290_NA915_DEV_PROFILE.iq_inverted(),
        E290_PRIVATE_SYNC_WORD,
        E290_NA915_DEV_REQUESTED_POWER_DBM,
        E290_NA915_DEV_RAW_SET_TX_PARAMS_DBM,
        E290_RX_SYMBOL_TIMEOUT,
        CAD_OPERATION_DEADLINE_MS,
        RECEIVE_OPERATION_DEADLINE_MS,
        TRANSMIT_OPERATION_DEADLINE_MS,
        EXCHANGE_DEADLINE_SECONDS,
    );
    info!(
        "e290-semantic-hil stage=runtime-source esp_rtos_source={}",
        env!("RETICULUM_ESP_RTOS_UPSTREAM_IDENTITY"),
    );

    let spi = match Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(SPI_FREQUENCY_HZ))
            .with_mode(SpiMode::_0),
    ) {
        Ok(spi) => spi,
        Err(_) => {
            error!("e290-semantic-hil stage=spi status=FAIL action=permanent-rf-inert");
            inert_forever().await
        }
    }
    .with_sck(peripherals.GPIO9)
    .with_mosi(peripherals.GPIO10)
    .with_miso(peripherals.GPIO11)
    .into_async();
    let spi_device =
        match embedded_hal_bus::spi::ExclusiveDevice::new(spi, radio_nss, embassy_time::Delay) {
            Ok(device) => device,
            Err(_) => {
                error!("e290-semantic-hil stage=spi-device status=FAIL action=permanent-rf-inert");
                inert_forever().await
            }
        };
    let dio1 = Input::new(peripherals.GPIO14, InputConfig::default());
    let busy = BoundedBusy::new(Input::new(peripherals.GPIO13, InputConfig::default()));
    let mut radio = match E290Radio::new(
        spi_device,
        radio_reset,
        dio1,
        busy,
        embassy_time::Delay,
        &IRQ_TIMESTAMPS,
        E290_NA915_DEV_CONFIGURATION,
    )
    .await
    {
        Ok(radio) => radio,
        Err(fault) => {
            error!(
                "e290-semantic-hil stage=radio-init status=FAIL operation={:?} radio={:?} action=permanent-rf-inert",
                fault.operation, fault.radio,
            );
            inert_forever().await
        }
    };
    info!(
        "e290-semantic-hil stage=radio-init status=PASS role={} regulator=dcdc rf_switch=dio2 tcxo=dio3_1v8 tx_budget_packets=2",
        role.label(),
    );

    let fixture_mac = semantic_fixture_mac(role).expect("active role has a semantic fixture");
    let node = match SEMANTIC_NODE.init_with(|| semantic_roundtrip_node_for_base_mac(fixture_mac)) {
        Ok(node) => node,
        Err(fault) => {
            error!(
                "e290-semantic-hil stage=node status=FAIL role={} error={fault:?}",
                role.label(),
            );
            radio.shutdown();
            inert_forever().await
        }
    };

    let result = match with_timeout(
        Duration::from_secs(EXCHANGE_DEADLINE_SECONDS),
        run_exchange(role, fixture_mac, &mut radio, node, &mut rng),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            error!(
                "e290-semantic-hil stage=terminal status=FAIL role={} reason=exchange-deadline radio_active_after_cancel={}",
                role.label(),
                radio.is_active(),
            );
            Err(RunError::new("exchange-deadline"))
        }
    };

    match result {
        Ok(success) => info!(
            "e290-semantic-hil stage=terminal status=PASS role={} tx_done={} local_destination={} peer_destination={} data_receipt={} radio_shutdown=next",
            role.label(),
            success.tx_done,
            HexBytes(&success.local_destination),
            HexBytes(&success.peer_destination),
            HexBytes(&success.data_receipt),
        ),
        Err(failure) => error!(
            "e290-semantic-hil stage=terminal status=FAIL role={} reason={} radio_shutdown=next",
            role.label(),
            failure.reason,
        ),
    }
    radio.shutdown();
    info!(
        "e290-semantic-hil stage=complete role={} radio_active={} action=permanent-rf-inert",
        role.label(),
        radio.is_active(),
    );
    inert_forever().await
}

fn monotonic_us() -> u64 {
    Instant::now().as_micros()
}

fn transport_seconds() -> u64 {
    Instant::now().as_secs()
}

#[derive(Clone, Copy, Debug)]
struct RunError {
    reason: &'static str,
}

impl RunError {
    const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

#[derive(Clone, Copy, Debug)]
struct RunSuccess {
    tx_done: u8,
    local_destination: [u8; 16],
    peer_destination: [u8; 16],
    data_receipt: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
struct ReceivedPacket {
    packet_len: usize,
    packet_hash: [u8; 32],
}

struct OneReceiptSink {
    expected: ReceiptId,
    attempts: u8,
    terminal: Option<ReceiptTerminal>,
}

impl OneReceiptSink {
    const fn new(expected: ReceiptId) -> Self {
        Self {
            expected,
            attempts: 0,
            terminal: None,
        }
    }
}

struct OneReceiptReservation<'a> {
    terminal: &'a mut Option<ReceiptTerminal>,
}

impl ReceiptTerminalSink for OneReceiptSink {
    type Reservation<'a>
        = OneReceiptReservation<'a>
    where
        Self: 'a;

    fn try_reserve(
        &mut self,
        candidate: ReceiptCandidate,
    ) -> Result<Self::Reservation<'_>, ReceiptReservationUnavailable> {
        self.attempts = self.attempts.saturating_add(1);
        if self.attempts != 1
            || candidate.kind() != ReceiptKind::Data
            || candidate.receipt() != self.expected
            || self.terminal.is_some()
        {
            return Err(ReceiptReservationUnavailable);
        }
        Ok(OneReceiptReservation {
            terminal: &mut self.terminal,
        })
    }
}

impl ReceiptTerminalReservation for OneReceiptReservation<'_> {
    fn commit(self, terminal: ReceiptTerminal) {
        *self.terminal = Some(terminal);
    }
}

const fn packet_type(step: SemanticRoundtripStep) -> PacketType {
    match step {
        SemanticRoundtripStep::InitiatorAnnounce | SemanticRoundtripStep::ResponderAnnounce => {
            PacketType::Announce
        }
        SemanticRoundtripStep::EncryptedData => PacketType::Data,
        SemanticRoundtripStep::DeliveryProof => PacketType::Proof,
    }
}

fn advance(
    state: &mut SemanticRoundtripState,
    action: SemanticRoundtripAction,
) -> Result<(), RunError> {
    if state.expected() != Some(action) {
        return Err(RunError::new("state-order"));
    }
    state
        .advance(action)
        .map_err(|_| RunError::new("state-transition"))?;
    info!(
        "e290-semantic-hil stage=state status=ADVANCED completed={action:?} next_phase={:?}",
        state.phase(),
    );
    Ok(())
}

fn require_action(
    state: SemanticRoundtripState,
    action: SemanticRoundtripAction,
) -> Result<(), RunError> {
    if state.expected() == Some(action) {
        Ok(())
    } else {
        Err(RunError::new("state-order"))
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the HIL packet log deliberately cross-binds every independent evidence field"
)]
fn log_packet(
    direction: &'static str,
    status: &'static str,
    step: SemanticRoundtripStep,
    rns_len: usize,
    physical_len: usize,
    packet_hash: &[u8; 32],
    destination_hash: Option<&[u8]>,
    data_receipt: Option<&[u8]>,
    signal: Option<FrameSignal>,
) {
    let (rssi_dbm, snr_db) = signal
        .map(|signal| (signal.rssi_dbm, signal.snr_db))
        .unwrap_or((0, 0));
    info!(
        "e290-semantic-hil stage=packet direction={} status={} step={step:?} rns_len={} physical_len={} sequence={} packet_hash={} destination_hash={} data_receipt={} signal_present={} rssi_dbm={} snr_db={}",
        direction,
        status,
        rns_len,
        physical_len,
        step.sequence(),
        HexBytes(packet_hash),
        OptionalHexBytes(destination_hash),
        OptionalHexBytes(data_receipt),
        signal.is_some(),
        rssi_dbm,
        snr_db,
    );
}

async fn transmit_packet<R: SoleRnodeRadio>(
    radio: &mut R,
    step: SemanticRoundtripStep,
    raw: &[u8],
    covered_receipt: Option<&[u8; 32]>,
    tx_done: &mut u8,
) -> Result<[u8; 32], RunError> {
    if *tx_done >= 2 {
        return Err(RunError::new("tx-budget"));
    }
    let parsed = Packet::parse(raw).map_err(|_| RunError::new("tx-rns-parse"))?;
    if parsed.packet_type != packet_type(step) {
        return Err(RunError::new("tx-rns-type"));
    }
    let packet_hash = parsed.compute_hash();
    if step == SemanticRoundtripStep::EncryptedData && covered_receipt != Some(&packet_hash) {
        return Err(RunError::new("data-receipt-hash"));
    }
    if step == SemanticRoundtripStep::DeliveryProof && covered_receipt.is_none() {
        return Err(RunError::new("proof-missing-receipt"));
    }

    let mut first = [0_u8; SX1262_FRAME_MTU];
    let mut second = [0_u8; SX1262_FRAME_MTU];
    let frames = frame_rns_packet(raw, step.sequence(), &mut first, &mut second)
        .map_err(|_| RunError::new("tx-frame"))?;
    if frames.second().is_some() {
        return Err(RunError::new("tx-split-frame-disallowed"));
    }
    validate_semantic_roundtrip_frame(step, frames.first())
        .map_err(|_| RunError::new("tx-frame-validation"))?;

    let cad = match with_timeout(
        Duration::from_millis(CAD_OPERATION_DEADLINE_MS),
        radio.cad(),
    )
    .await
    {
        Ok(Ok(observation)) => observation,
        Ok(Err(fault)) => {
            error!(
                "e290-semantic-hil stage=cad status=FAIL step={step:?} reason=radio-fault phase={:?} class={:?}",
                fault.phase(),
                fault.class(),
            );
            return Err(RunError::new("radio-cad"));
        }
        Err(_) => return Err(RunError::new("radio-cad-deadline")),
    };
    info!(
        "e290-semantic-hil stage=cad status={} step={step:?} activity_detected={} observed_at_us={}",
        if cad.activity_detected() {
            "BUSY"
        } else {
            "CLEAR"
        },
        cad.activity_detected(),
        cad.observed_at_us(),
    );
    if cad.activity_detected() {
        return Err(RunError::new("radio-cad-busy"));
    }

    let observation = match with_timeout(
        Duration::from_millis(TRANSMIT_OPERATION_DEADLINE_MS),
        radio.transmit(frames),
    )
    .await
    {
        Ok(Ok(observation)) => observation,
        Ok(Err(fault)) => {
            error!(
                "e290-semantic-hil stage=tx status=FAIL step={step:?} reason=radio-fault phase={:?} class={:?} completed_frames={}",
                fault.fault().phase(),
                fault.fault().class(),
                fault.progress().completed_frame_count(),
            );
            return Err(RunError::new("radio-tx"));
        }
        Err(_) => return Err(RunError::new("radio-tx-deadline")),
    };
    if observation.progress().completed_frame_count() != 1
        || observation.progress().frame_completed_at_us(0).is_none()
    {
        return Err(RunError::new("tx-observation"));
    }
    *tx_done = tx_done.saturating_add(1);
    let destination = matches!(
        step,
        SemanticRoundtripStep::InitiatorAnnounce | SemanticRoundtripStep::ResponderAnnounce
    )
    .then_some(parsed.destination_hash);
    let data_receipt = match step {
        SemanticRoundtripStep::EncryptedData => Some(packet_hash.as_slice()),
        SemanticRoundtripStep::DeliveryProof => covered_receipt.map(|receipt| receipt.as_slice()),
        SemanticRoundtripStep::InitiatorAnnounce | SemanticRoundtripStep::ResponderAnnounce => None,
    };
    log_packet(
        "tx",
        "DRIVER_TX_DONE",
        step,
        raw.len(),
        frames.first().len(),
        &packet_hash,
        destination,
        data_receipt,
        None,
    );
    Ok(packet_hash)
}

async fn receive_packet<R: SoleRnodeRadio>(
    radio: &mut R,
    receiver: &mut TimedRnodeRx,
    role: HilRole,
    step: SemanticRoundtripStep,
    covered_data_receipt: Option<&[u8; 32]>,
    packet_output: &mut [u8; RNODE_HW_MTU],
) -> Result<ReceivedPacket, RunError> {
    let mut physical = [0_u8; SX1262_FRAME_MTU];
    info!(
        "e290-semantic-hil stage=rx status=ARMED role={} step={step:?} maximum_windows={MAXIMUM_RX_WINDOWS}",
        role.label(),
    );
    for window in 1..=MAXIMUM_RX_WINDOWS {
        let observation = match with_timeout(
            Duration::from_millis(RECEIVE_OPERATION_DEADLINE_MS),
            radio.receive_bounded(&mut physical),
        )
        .await
        {
            Ok(Ok(BoundedRxOutcome::Frame(observation))) => observation,
            Ok(Ok(BoundedRxOutcome::NoPreambleTimeout)) => continue,
            Ok(Err(fault)) => {
                error!(
                    "e290-semantic-hil stage=rx status=FAIL role={} step={step:?} window={} reason=radio-fault phase={:?} class={:?}",
                    role.label(),
                    window,
                    fault.phase(),
                    fault.class(),
                );
                return Err(RunError::new("radio-rx"));
            }
            Err(_) => return Err(RunError::new("radio-rx-deadline")),
        };
        let frame = observation
            .payload(&physical)
            .ok_or(RunError::new("rx-length"))?;
        validate_semantic_roundtrip_frame(step, frame)
            .map_err(|_| RunError::new("rx-frame-validation"))?;
        let outcome = receiver
            .feed(
                frame,
                observation.received_at_us(),
                observation.signal(),
                packet_output,
            )
            .map_err(|_| RunError::new("rx-rnode"))?;
        let TimedReceiveOutcome::Packet { packet_len, .. } = outcome else {
            return Err(RunError::new("rx-unexpected-fragment"));
        };
        let raw = &packet_output[..packet_len];
        let parsed = Packet::parse(raw).map_err(|_| RunError::new("rx-rns-parse"))?;
        if parsed.packet_type != packet_type(step) {
            return Err(RunError::new("rx-rns-type"));
        }
        let packet_hash = parsed.compute_hash();
        let destination = matches!(
            step,
            SemanticRoundtripStep::InitiatorAnnounce | SemanticRoundtripStep::ResponderAnnounce
        )
        .then_some(parsed.destination_hash);
        let data_receipt = match step {
            SemanticRoundtripStep::EncryptedData => Some(packet_hash.as_slice()),
            SemanticRoundtripStep::DeliveryProof => {
                covered_data_receipt.map(|receipt| receipt.as_slice())
            }
            SemanticRoundtripStep::InitiatorAnnounce | SemanticRoundtripStep::ResponderAnnounce => {
                None
            }
        };
        log_packet(
            "rx",
            "RNODE_PACKET",
            step,
            packet_len,
            frame.len(),
            &packet_hash,
            destination,
            data_receipt,
            Some(observation.signal()),
        );
        return Ok(ReceivedPacket {
            packet_len,
            packet_hash,
        });
    }
    Err(RunError::new("rx-window-exhausted"))
}

fn ingest_announce<G: RngCore + CryptoRng>(
    node: &mut SemanticRoundtripNode,
    expected_peer: &DestHash,
    raw: &[u8],
    rng: &mut G,
) -> Result<(), RunError> {
    let parsed = Packet::parse(raw).map_err(|_| RunError::new("announce-parse"))?;
    if parsed.packet_type != PacketType::Announce
        || parsed.destination_hash != expected_peer.as_ref()
    {
        return Err(RunError::new("announce-peer-mismatch"));
    }
    let report = node.ingest(raw, transport_seconds(), HIL_INTERFACE, rng);
    let exact_event = matches!(
        report.actions.events.as_slice(),
        [ApplicationEvent::AnnounceReceived { destination, .. }]
            if *destination == *expected_peer.as_bytes()
    );
    if report.disposition != IngressDisposition::Processed
        || !exact_event
        || !report.actions.packets.is_empty()
        || report.actions.unroutable_packets != 0
        || node.route(expected_peer).is_none()
    {
        return Err(RunError::new("announce-ingress"));
    }
    info!(
        "e290-semantic-hil stage=announce-ingress status=SEMANTIC_VALIDATED peer_destination={} route_learned=true",
        HexBytes(expected_peer.as_bytes()),
    );
    Ok(())
}

async fn run_exchange<R, G>(
    role: HilRole,
    fixture_mac: &[u8],
    radio: &mut R,
    node: &mut SemanticRoundtripNode,
    rng: &mut G,
) -> Result<RunSuccess, RunError>
where
    R: SoleRnodeRadio,
    G: RngCore + CryptoRng,
{
    let local_destination = node.destination_hash();
    let peer_destination = semantic_roundtrip_peer_destination_for_base_mac(fixture_mac)
        .map_err(|_| RunError::new("peer-destination"))?;
    let (initiator_destination, responder_destination) = match role {
        HilRole::Initiator => (local_destination, peer_destination),
        HilRole::Responder => (peer_destination, local_destination),
        HilRole::Inert => return Err(RunError::new("inert-role")),
    };
    let payload = build_semantic_roundtrip_payload(
        initiator_destination.as_bytes(),
        responder_destination.as_bytes(),
    );
    if payload.len() != SEMANTIC_ROUNDTRIP_PAYLOAD_LEN
        || !validate_semantic_roundtrip_payload(
            &payload,
            initiator_destination.as_bytes(),
            responder_destination.as_bytes(),
        )
    {
        return Err(RunError::new("payload-fixture"));
    }
    let mut state =
        SemanticRoundtripState::new(role).map_err(|_| RunError::new("state-construction"))?;
    info!(
        "e290-semantic-hil stage=exchange status=ARMED role={} phase={:?} local_destination={} peer_destination={} payload_len={} tx_budget=2 maximum_rx_windows={}",
        role.label(),
        state.phase(),
        HexBytes(local_destination.as_bytes()),
        HexBytes(peer_destination.as_bytes()),
        payload.len(),
        MAXIMUM_RX_WINDOWS,
    );
    let mut receiver = TimedRnodeRx::new(FRAGMENT_TIMEOUT_US);
    let mut packet = [0_u8; RNODE_HW_MTU];
    let mut tx_done = 0_u8;

    let data_receipt = match role {
        HilRole::Initiator => {
            let announce_action =
                SemanticRoundtripAction::Transmit(SemanticRoundtripStep::InitiatorAnnounce);
            require_action(state, announce_action)?;
            Timer::after(Duration::from_millis(INITIATOR_STARTUP_DELAY_MS)).await;
            let mut announce = [0_u8; RNS_MTU];
            let announce_len =
                prepare_semantic_roundtrip_announce(node, transport_seconds(), rng, &mut announce)
                    .map_err(|_| RunError::new("initiator-announce-build"))?;
            transmit_packet(
                radio,
                SemanticRoundtripStep::InitiatorAnnounce,
                &announce[..announce_len],
                None,
                &mut tx_done,
            )
            .await?;
            advance(&mut state, announce_action)?;

            let peer_announce_action =
                SemanticRoundtripAction::Receive(SemanticRoundtripStep::ResponderAnnounce);
            require_action(state, peer_announce_action)?;
            let received = receive_packet(
                radio,
                &mut receiver,
                role,
                SemanticRoundtripStep::ResponderAnnounce,
                None,
                &mut packet,
            )
            .await?;
            ingest_announce(node, &peer_destination, &packet[..received.packet_len], rng)?;
            advance(&mut state, peer_announce_action)?;

            Timer::after(Duration::from_millis(DATA_GUARD_DELAY_MS)).await;
            let data_action =
                SemanticRoundtripAction::Transmit(SemanticRoundtripStep::EncryptedData);
            require_action(state, data_action)?;
            let mut encrypted = [0_u8; RNS_MTU];
            let prepared = node
                .prepare_data_into(
                    &peer_destination,
                    &payload,
                    transport_seconds(),
                    rng,
                    &mut encrypted,
                )
                .map_err(|_| RunError::new("data-encrypt"))?;
            if prepared.target() != TxTarget::Only(HIL_INTERFACE) {
                return Err(RunError::new("data-interface-selection"));
            }
            let receipt = *prepared.receipt().as_bytes();
            transmit_packet(
                radio,
                SemanticRoundtripStep::EncryptedData,
                &encrypted[..usize::from(prepared.packet_len())],
                Some(&receipt),
                &mut tx_done,
            )
            .await?;
            advance(&mut state, data_action)?;

            let proof_action =
                SemanticRoundtripAction::Receive(SemanticRoundtripStep::DeliveryProof);
            require_action(state, proof_action)?;
            let received = receive_packet(
                radio,
                &mut receiver,
                role,
                SemanticRoundtripStep::DeliveryProof,
                Some(&receipt),
                &mut packet,
            )
            .await?;
            let mut sink = OneReceiptSink::new(prepared.receipt());
            let report = node
                .ingest_with_receipt_sink(
                    &packet[..received.packet_len],
                    transport_seconds(),
                    HIL_INTERFACE,
                    rng,
                    &mut sink,
                )
                .map_err(|_| RunError::new("proof-receipt-backpressure"))?;
            let delivered = matches!(
                sink.terminal,
                Some(ReceiptTerminal::Delivered(candidate))
                    if candidate.kind() == ReceiptKind::Data
                        && candidate.receipt() == prepared.receipt()
            );
            if report.disposition != IngressDisposition::Processed
                || !report.actions.events.is_empty()
                || !report.actions.packets.is_empty()
                || report.actions.unroutable_packets != 0
                || sink.attempts != 1
                || !delivered
                || node.metrics().capacity.receipts.used != 0
            {
                return Err(RunError::new("proof-ingress"));
            }
            info!(
                "e290-semantic-hil stage=proof-ingress status=SEMANTIC_VALIDATED receipt={} terminal=Delivered receipt_slots_used=0 proof_packet_hash={}",
                HexBytes(&receipt),
                HexBytes(&received.packet_hash),
            );
            advance(&mut state, proof_action)?;
            receipt
        }
        HilRole::Responder => {
            let peer_announce_action =
                SemanticRoundtripAction::Receive(SemanticRoundtripStep::InitiatorAnnounce);
            require_action(state, peer_announce_action)?;
            let received = receive_packet(
                radio,
                &mut receiver,
                role,
                SemanticRoundtripStep::InitiatorAnnounce,
                None,
                &mut packet,
            )
            .await?;
            ingest_announce(node, &peer_destination, &packet[..received.packet_len], rng)?;
            advance(&mut state, peer_announce_action)?;

            let announce_action =
                SemanticRoundtripAction::Transmit(SemanticRoundtripStep::ResponderAnnounce);
            require_action(state, announce_action)?;
            let mut announce = [0_u8; RNS_MTU];
            let announce_len =
                prepare_semantic_roundtrip_announce(node, transport_seconds(), rng, &mut announce)
                    .map_err(|_| RunError::new("responder-announce-build"))?;
            transmit_packet(
                radio,
                SemanticRoundtripStep::ResponderAnnounce,
                &announce[..announce_len],
                None,
                &mut tx_done,
            )
            .await?;
            advance(&mut state, announce_action)?;

            let data_action =
                SemanticRoundtripAction::Receive(SemanticRoundtripStep::EncryptedData);
            require_action(state, data_action)?;
            let received = receive_packet(
                radio,
                &mut receiver,
                role,
                SemanticRoundtripStep::EncryptedData,
                None,
                &mut packet,
            )
            .await?;
            let receipt = received.packet_hash;
            let report = node.ingest(
                &packet[..received.packet_len],
                transport_seconds(),
                HIL_INTERFACE,
                rng,
            );
            let payload_is_exact = matches!(
                report.actions.events.as_slice(),
                [ApplicationEvent::DataReceived {
                    destination,
                    payload: observed,
                }]
                    if *destination == *local_destination.as_bytes()
                        && validate_semantic_roundtrip_payload(
                            observed,
                            initiator_destination.as_bytes(),
                            responder_destination.as_bytes(),
                        )
            );
            if report.disposition != IngressDisposition::Processed
                || !payload_is_exact
                || report.actions.packets.len() != 1
                || report.actions.unroutable_packets != 0
                || report.actions.packets[0].target() != TxTarget::Only(HIL_INTERFACE)
            {
                return Err(RunError::new("data-ingress"));
            }
            info!(
                "e290-semantic-hil stage=data-ingress status=SEMANTIC_VALIDATED role=responder payload_len={} destination={} data_receipt={} proof_actions=1 extra_actions=0",
                payload.len(),
                HexBytes(local_destination.as_bytes()),
                HexBytes(&receipt),
            );
            advance(&mut state, data_action)?;
            let proof = &report.actions.packets[0];
            if Packet::parse(proof.bytes())
                .map_err(|_| RunError::new("proof-parse"))?
                .packet_type
                != PacketType::Proof
            {
                return Err(RunError::new("generated-action-not-proof"));
            }
            let proof_action =
                SemanticRoundtripAction::Transmit(SemanticRoundtripStep::DeliveryProof);
            require_action(state, proof_action)?;
            transmit_packet(
                radio,
                SemanticRoundtripStep::DeliveryProof,
                proof.bytes(),
                Some(&receipt),
                &mut tx_done,
            )
            .await?;
            advance(&mut state, proof_action)?;
            receipt
        }
        HilRole::Inert => return Err(RunError::new("inert-role")),
    };

    if tx_done != 2 || !state.is_complete() || state.phase() != SemanticRoundtripPhase::Complete {
        return Err(RunError::new("terminal-invariant"));
    }
    Ok(RunSuccess {
        tx_done,
        local_destination: *local_destination.as_bytes(),
        peer_destination: *peer_destination.as_bytes(),
        data_receipt,
    })
}

async fn inert_forever() -> ! {
    loop {
        Timer::after(Duration::from_secs(INERT_HEARTBEAT_SECONDS)).await;
        info!("e290-semantic-hil stage=inert-heartbeat rf_state=reset_low");
    }
}

const _: () = assert!(SEMANTIC_ROUNDTRIP_INITIATOR_ANNOUNCE_SEQUENCE == 9);
const _: () = assert!(SEMANTIC_ROUNDTRIP_RESPONDER_ANNOUNCE_SEQUENCE == 10);
const _: () = assert!(SEMANTIC_ROUNDTRIP_DATA_SEQUENCE == 11);
const _: () = assert!(SEMANTIC_ROUNDTRIP_PROOF_SEQUENCE == 12);
