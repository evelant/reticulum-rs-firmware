//! Dedicated two-board LoRa/RNode-framing and semantic Reticulum HIL.
//!
//! This is not product firmware. Its default mode emits only invalid short
//! sentinels. The explicit `semantic-announce-hil` mode instead emits one
//! fixed signed Reticulum conformance fixture. The explicit
//! `semantic-roundtrip-hil` mode runs signed announces, encrypted DATA and its
//! delivery proof in both directions across the two boards. Radio construction
//! is allowed only for two exact factory eFuse base MACs.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

use core::future::Future;
#[cfg(feature = "semantic-roundtrip-hil")]
use core::num::NonZeroU64;

use embassy_executor::Spawner;
#[cfg(feature = "semantic-roundtrip-hil")]
use embassy_time::Instant;
use embassy_time::{Delay, Duration, Timer, with_timeout};
use embedded_hal::digital::{Error as DigitalError, ErrorKind as DigitalErrorKind, ErrorType};
use embedded_hal_async::digital::Wait;
use esp_backtrace as _;
#[cfg(feature = "semantic-roundtrip-hil")]
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::{
    clock::CpuClock,
    delay::Delay as BlockingDelay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig},
    spi::{
        Mode as SpiMode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use log::{error, info};
use reticulum_board_heltec_tracker_v2_tx_hil::{
    TRACKER_TX_HIL_CTX_ASSERTION, TRACKER_TX_HIL_FEM_POWER_POLICY, TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
    TRACKER_TX_HIL_POWER_PROFILE, TRACKER_TX_HIL_PROFILE, TRACKER_TX_HIL_RX_SYMBOL_TIMEOUT,
    TRACKER_TX_HIL_STANDBY_CLOCK, TRACKER_TX_HIL_TARGET_POWER_DBM, TrackerTxArm, TrackerTxHilRadio,
};
#[cfg(not(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil")))]
use reticulum_heltec_tracker_v2_tx_hil::{
    HIL_PING_PAYLOAD, HIL_PING_SEQUENCE, HIL_REPLY_PAYLOAD, HIL_REPLY_SEQUENCE, HilFrameKind,
    RNS_MINIMUM_PACKET_LEN, classify_hil_frame,
};
#[cfg(all(
    feature = "semantic-announce-hil",
    not(feature = "semantic-roundtrip-hil")
))]
use reticulum_heltec_tracker_v2_tx_hil::{
    HIL_PING_SEQUENCE, SEMANTIC_ANNOUNCE_DESTINATION_HASH, SEMANTIC_ANNOUNCE_PACKET_HASH,
    build_semantic_announce_packet,
};
use reticulum_heltec_tracker_v2_tx_hil::{HilRole, role_for_base_mac};
#[cfg(feature = "semantic-roundtrip-hil")]
use reticulum_heltec_tracker_v2_tx_hil::{
    INITIATOR_BASE_MAC, RESPONDER_BASE_MAC, SEMANTIC_ROUNDTRIP_DATA_SEQUENCE,
    SEMANTIC_ROUNDTRIP_INITIATOR_ANNOUNCE_SEQUENCE, SEMANTIC_ROUNDTRIP_PAYLOAD_LEN,
    SEMANTIC_ROUNDTRIP_PROOF_SEQUENCE, SEMANTIC_ROUNDTRIP_RESPONDER_ANNOUNCE_SEQUENCE,
    SemanticRoundtripAction, SemanticRoundtripError, SemanticRoundtripNode, SemanticRoundtripPhase,
    SemanticRoundtripState, SemanticRoundtripStep, build_semantic_roundtrip_payload,
    prepare_semantic_roundtrip_announce, semantic_roundtrip_node_for_base_mac,
    semantic_roundtrip_peer_destination_for_base_mac, validate_semantic_roundtrip_frame,
    validate_semantic_roundtrip_payload,
};
#[cfg(feature = "semantic-roundtrip-hil")]
use reticulum_radio_interface::{FrameSignal, RNODE_HW_MTU, TimedReceiveOutcome, TimedRnodeRx};
use reticulum_radio_interface::{SX1262_FRAME_MTU, frame_rns_packet};
#[cfg(any(
    all(
        feature = "semantic-announce-hil",
        not(feature = "semantic-roundtrip-hil")
    ),
    feature = "semantic-roundtrip-hil"
))]
use reticulum_rns_rete::RNS_MTU;
#[cfg(feature = "semantic-roundtrip-hil")]
use reticulum_rns_rete::{
    DestHash, IngressDisposition, InterfaceId, NodeEvent, Packet, PacketType, ReceiptCandidate,
    ReceiptId, ReceiptKind, ReceiptReservationUnavailable, ReceiptTerminal,
    ReceiptTerminalReservation, ReceiptTerminalSink, TxTarget,
};
#[cfg(feature = "semantic-roundtrip-hil")]
use static_cell::StaticCell;

const TRACKER_SPI_FREQUENCY_HZ: u32 = 1_000_000;
const TRACKER_BUSY_TIMEOUT_MS: u64 = 100;
const INITIATOR_STARTUP_DELAY_MS: u32 = 3_000;
#[cfg(not(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil")))]
const RESPONDER_RX_WINDOWS: u8 = 48;
#[cfg(not(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil")))]
const INITIATOR_REPLY_RX_WINDOWS: u8 = 24;
#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_RX_WINDOWS: u8 = 48;
#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_DATA_GUARD_DELAY_MS: u32 = 250;
#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_INTERFACE: InterfaceId = InterfaceId(1);
#[cfg(feature = "semantic-roundtrip-hil")]
const SEMANTIC_ROUNDTRIP_FRAGMENT_TIMEOUT_TICKS: NonZeroU64 =
    NonZeroU64::new(1).expect("one is non-zero");
const RECEIVE_SOFTWARE_DEADLINE_MS: u64 = 1_500;
const TRANSMIT_SOFTWARE_DEADLINE_MS: u64 = 1_500;
const INERT_HEARTBEAT_SECONDS: u64 = 30;

#[cfg(not(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil")))]
const HIL_MODE: &str = "sentinel";
#[cfg(all(
    feature = "semantic-announce-hil",
    not(feature = "semantic-roundtrip-hil")
))]
const HIL_MODE: &str = "semantic-announce";
#[cfg(feature = "semantic-roundtrip-hil")]
const HIL_MODE: &str = "semantic-roundtrip";

#[cfg(not(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil")))]
const HIL_RNS_POLICY: &str = "invalid-short-sentinel";
#[cfg(all(
    feature = "semantic-announce-hil",
    not(feature = "semantic-roundtrip-hil")
))]
const HIL_RNS_POLICY: &str = "cryptographic-validation-required-before-transmit";
#[cfg(feature = "semantic-roundtrip-hil")]
const HIL_RNS_POLICY: &str = "signed-announce+encrypted-data+delivery-proof";

#[cfg(not(feature = "near-field-attenuation-hil"))]
const _: () =
    assert!(TRACKER_TX_HIL_TARGET_POWER_DBM == 14 && TRACKER_TX_HIL_MODEM_OUTPUT_DBM == 0);
#[cfg(feature = "near-field-attenuation-hil")]
const _: () =
    assert!(TRACKER_TX_HIL_TARGET_POWER_DBM == 5 && TRACKER_TX_HIL_MODEM_OUTPUT_DBM == -9);

static TX_ARM: TrackerTxArm = TrackerTxArm::new();
#[cfg(feature = "semantic-roundtrip-hil")]
static SEMANTIC_ROUNDTRIP_NODE: StaticCell<Result<SemanticRoundtripNode, SemanticRoundtripError>> =
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

#[cfg(feature = "semantic-roundtrip-hil")]
struct OptionalHexBytes<'a>(Option<&'a [u8]>);

#[cfg(feature = "semantic-roundtrip-hil")]
impl core::fmt::Display for OptionalHexBytes<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(bytes) => HexBytes(bytes).fmt(formatter),
            None => formatter.write_str("none"),
        }
    }
}

#[derive(Debug)]
enum BoundedBusyError<PinError> {
    Pin(PinError),
    Timeout,
}

impl<PinError: DigitalError> DigitalError for BoundedBusyError<PinError> {
    fn kind(&self) -> DigitalErrorKind {
        match self {
            Self::Pin(error) => error.kind(),
            Self::Timeout => DigitalErrorKind::Other,
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

impl<Pin: ErrorType> ErrorType for BoundedBusy<Pin>
where
    Pin::Error: DigitalError,
{
    type Error = BoundedBusyError<Pin::Error>;
}

async fn wait_for_busy_pin<F, PinError>(future: F) -> Result<(), BoundedBusyError<PinError>>
where
    F: Future<Output = Result<(), PinError>>,
    PinError: DigitalError,
{
    match with_timeout(Duration::from_millis(TRACKER_BUSY_TIMEOUT_MS), future).await {
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
        wait_for_busy_pin(self.pin.wait_for_high()).await
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        wait_for_busy_pin(self.pin.wait_for_low()).await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_busy_pin(self.pin.wait_for_rising_edge()).await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_busy_pin(self.pin.wait_for_falling_edge()).await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_busy_pin(self.pin.wait_for_any_edge()).await
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "the firmware entry point initializes target-owned resources and fixed radio buffers"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Own every RF control at its inert level before logging, eFuse role
    // selection, timer startup, SPI construction, or any radio operation.
    let sx1262_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let fem_power = Output::new(peripherals.GPIO7, Level::Low, OutputConfig::default());
    let fem_csd = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
    let fem_ctx = Output::new(peripherals.GPIO5, Level::Low, OutputConfig::default());
    let sx1262_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    let _vext = Output::new(peripherals.GPIO3, Level::Low, OutputConfig::default());
    let _battery_divider = Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default());

    esp_println::logger::init_logger_from_env();

    let base_mac = esp_hal::efuse::base_mac_address();
    let role = role_for_base_mac(base_mac.as_bytes());
    info!(
        "tx-hil stage=mac-gate base_mac={} role={} exact_match={} radio_constructed=false spi_constructed=false rf_state=reset_low_fem_low",
        base_mac,
        role.label(),
        role != HilRole::Inert,
    );

    if role == HilRole::Inert {
        error!(
            "tx-hil stage=mac-gate status=DENIED action=permanent-rf-inert reason=unknown-efuse-base-mac"
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

    #[cfg(feature = "semantic-roundtrip-hil")]
    let _semantic_entropy_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    #[cfg(feature = "semantic-roundtrip-hil")]
    let mut semantic_crypto_rng = match Trng::try_new() {
        Ok(rng) => rng,
        Err(_) => {
            error!(
                "tx-hil stage=semantic-roundtrip-terminal status=FAIL role={} tx_done=0 reason=adc-backed-trng-unavailable radio_constructed=false action=permanent-rf-inert-hold",
                role.label(),
            );
            inert_forever().await
        }
    };

    info!(
        "tx-hil stage=profile status=PASS source=tx-bsp-fixed region=NA915 frequency_hz={} bandwidth_hz={} sf={} coding_rate_denominator={} preamble_symbols={} explicit_header={} crc={} iq_inverted={} sync_word=private standby_clock={} fem_power_policy={} fem_ctx_assertion={} power_profile={} target_antenna_path_dbm={} sx1262_output_dbm={} hil_mode={} rns_policy={}",
        TRACKER_TX_HIL_PROFILE.frequency_hz(),
        TRACKER_TX_HIL_PROFILE.bandwidth().hz(),
        TRACKER_TX_HIL_PROFILE.spreading_factor().factor(),
        TRACKER_TX_HIL_PROFILE.coding_rate().denom(),
        TRACKER_TX_HIL_PROFILE.preamble_symbols(),
        TRACKER_TX_HIL_PROFILE.explicit_header(),
        TRACKER_TX_HIL_PROFILE.crc(),
        TRACKER_TX_HIL_PROFILE.iq_inverted(),
        TRACKER_TX_HIL_STANDBY_CLOCK,
        TRACKER_TX_HIL_FEM_POWER_POLICY,
        TRACKER_TX_HIL_CTX_ASSERTION,
        TRACKER_TX_HIL_POWER_PROFILE,
        TRACKER_TX_HIL_TARGET_POWER_DBM,
        TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
        HIL_MODE,
        HIL_RNS_POLICY,
    );
    info!(
        "tx-hil stage=runtime-patch esp_rtos_main_stack_slice={}",
        env!("RETICULUM_ESP_RTOS_MAIN_STACK_PATCH"),
    );

    let spi = match Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(TRACKER_SPI_FREQUENCY_HZ))
            .with_mode(SpiMode::_0),
    ) {
        Ok(spi) => spi,
        Err(_) => {
            error!("tx-hil stage=spi status=FAIL action=rf-inert-hold");
            inert_forever().await
        }
    }
    .with_sck(peripherals.GPIO9)
    .with_mosi(peripherals.GPIO10)
    .with_miso(peripherals.GPIO11)
    .into_async();
    let spi_device = match embedded_hal_bus::spi::ExclusiveDevice::new(spi, sx1262_nss, Delay) {
        Ok(device) => device,
        Err(_) => {
            error!("tx-hil stage=spi-device status=FAIL action=rf-inert-hold");
            inert_forever().await
        }
    };
    let dio1 = Input::new(peripherals.GPIO14, InputConfig::default());
    let busy = BoundedBusy::new(Input::new(peripherals.GPIO13, InputConfig::default()));
    let mut radio = match TrackerTxHilRadio::new(
        spi_device,
        sx1262_reset,
        dio1,
        busy,
        fem_power,
        fem_csd,
        fem_ctx,
        Delay,
        Delay,
        &TX_ARM,
    )
    .await
    {
        Ok(radio) => radio,
        Err(fault) => {
            error!(
                "tx-hil stage=radio-init status=FAIL operation={:?} radio={:?} action=rf-inert-hold",
                fault.operation, fault.radio,
            );
            inert_forever().await
        }
    };

    info!(
        "tx-hil stage=radio-init status=PASS role={} fem_state=powered_settled_ctx_rx sx1262_preamble_symbol_timeout={} receive_whole_operation_outer_deadline_ms={} transmit_whole_operation_outer_deadline_ms={} tx_budget_frames={}",
        role.label(),
        TRACKER_TX_HIL_RX_SYMBOL_TIMEOUT,
        RECEIVE_SOFTWARE_DEADLINE_MS,
        TRANSMIT_SOFTWARE_DEADLINE_MS,
        if cfg!(feature = "semantic-roundtrip-hil") {
            2
        } else {
            1
        },
    );

    #[cfg(not(feature = "semantic-roundtrip-hil"))]
    match role {
        HilRole::Initiator => {
            #[cfg(not(any(
                feature = "semantic-announce-hil",
                feature = "semantic-roundtrip-hil"
            )))]
            run_initiator_inline(&mut radio).await;
            #[cfg(all(
                feature = "semantic-announce-hil",
                not(feature = "semantic-roundtrip-hil")
            ))]
            run_semantic_announce_initiator_inline(&mut radio).await;
        }
        HilRole::Responder => {
            #[cfg(not(any(
                feature = "semantic-announce-hil",
                feature = "semantic-roundtrip-hil"
            )))]
            run_responder_inline(&mut radio).await;
            #[cfg(all(
                feature = "semantic-announce-hil",
                not(feature = "semantic-roundtrip-hil")
            ))]
            info!(
                "tx-hil stage=semantic-responder status=TX_DISABLED tx_sent=0 tx_budget_frames=0 reason=semantic-mode-has-one-initiator-only"
            );
        }
        HilRole::Inert => {}
    }

    #[cfg(feature = "semantic-roundtrip-hil")]
    {
        log_semantic_roundtrip_heap("before-node-construction", role);
        let node_result = SEMANTIC_ROUNDTRIP_NODE
            .init_with(|| semantic_roundtrip_node_for_base_mac(base_mac.as_bytes()));
        let result = match node_result {
            Ok(node) => {
                log_semantic_roundtrip_heap("after-node-construction", role);
                run_semantic_roundtrip_inline(
                    role,
                    base_mac.as_bytes(),
                    &mut radio,
                    node,
                    &mut semantic_crypto_rng,
                )
                .await
            }
            Err(error) => {
                error!(
                    "tx-hil stage=semantic-roundtrip-node status=FAIL role={} error={error:?}",
                    role.label(),
                );
                Err(SemanticRoundtripRunError::new("node-construction"))
            }
        };
        log_semantic_roundtrip_heap("terminal", role);
        match result {
            Ok(success) => info!(
                "tx-hil stage=semantic-roundtrip-terminal status=PASS role={} tx_done={} local_destination={} peer_destination={} data_receipt={} radio_shutdown=next",
                role.label(),
                success.tx_done,
                HexBytes(&success.local_destination),
                HexBytes(&success.peer_destination),
                HexBytes(&success.data_receipt),
            ),
            Err(failure) => error!(
                "tx-hil stage=semantic-roundtrip-terminal status=FAIL role={} reason={} radio_shutdown=next",
                role.label(),
                failure.reason,
            ),
        }
    }

    radio.shutdown();
    info!(
        "tx-hil stage=complete role={} radio_active={} action=permanent-rf-inert-hold",
        role.label(),
        radio.is_active(),
    );
    inert_forever().await
}

#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug)]
struct SemanticRoundtripRunError {
    reason: &'static str,
}

#[cfg(feature = "semantic-roundtrip-hil")]
impl SemanticRoundtripRunError {
    const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug)]
struct SemanticRoundtripSuccess {
    tx_done: u8,
    local_destination: [u8; 16],
    peer_destination: [u8; 16],
    data_receipt: [u8; 32],
}

#[cfg(feature = "semantic-roundtrip-hil")]
#[derive(Clone, Copy, Debug)]
struct SemanticReceivedPacket {
    packet_len: usize,
    packet_hash: [u8; 32],
}

#[cfg(feature = "semantic-roundtrip-hil")]
struct SemanticReceiptSink {
    expected: ReceiptId,
    reservation_attempts: u8,
    terminal: Option<ReceiptTerminal>,
}

#[cfg(feature = "semantic-roundtrip-hil")]
impl SemanticReceiptSink {
    const fn new(expected: ReceiptId) -> Self {
        Self {
            expected,
            reservation_attempts: 0,
            terminal: None,
        }
    }
}

#[cfg(feature = "semantic-roundtrip-hil")]
struct SemanticReceiptReservation<'a> {
    terminal: &'a mut Option<ReceiptTerminal>,
}

#[cfg(feature = "semantic-roundtrip-hil")]
impl ReceiptTerminalSink for SemanticReceiptSink {
    type Reservation<'a>
        = SemanticReceiptReservation<'a>
    where
        Self: 'a;

    fn try_reserve(
        &mut self,
        candidate: ReceiptCandidate,
    ) -> Result<Self::Reservation<'_>, ReceiptReservationUnavailable> {
        self.reservation_attempts = self.reservation_attempts.saturating_add(1);
        if self.reservation_attempts != 1
            || candidate.kind() != ReceiptKind::Data
            || candidate.receipt() != self.expected
            || self.terminal.is_some()
        {
            return Err(ReceiptReservationUnavailable);
        }
        Ok(SemanticReceiptReservation {
            terminal: &mut self.terminal,
        })
    }
}

#[cfg(feature = "semantic-roundtrip-hil")]
impl ReceiptTerminalReservation for SemanticReceiptReservation<'_> {
    fn commit(self, terminal: ReceiptTerminal) {
        *self.terminal = Some(terminal);
    }
}

#[cfg(feature = "semantic-roundtrip-hil")]
fn log_semantic_roundtrip_heap(stage: &'static str, role: HilRole) {
    let heap = esp_alloc::HEAP.stats();
    info!(
        "tx-hil stage=semantic-roundtrip-heap checkpoint={} role={} heap_size={} heap_used={} heap_free={} heap_max_used={}",
        stage,
        role.label(),
        heap.size,
        heap.current_usage,
        heap.size.saturating_sub(heap.current_usage),
        heap.max_usage,
    );
}

#[cfg(feature = "semantic-roundtrip-hil")]
fn semantic_rnode_ticks() -> u64 {
    Instant::now().as_ticks()
}

#[cfg(feature = "semantic-roundtrip-hil")]
fn semantic_transport_seconds() -> u64 {
    Instant::now().as_secs()
}

#[cfg(feature = "semantic-roundtrip-hil")]
fn ensure_semantic_action(
    state: SemanticRoundtripState,
    action: SemanticRoundtripAction,
) -> Result<(), SemanticRoundtripRunError> {
    if state.expected() != Some(action) {
        error!(
            "tx-hil stage=semantic-roundtrip-state status=FAIL phase={:?} expected={:?} observed={action:?}",
            state.phase(),
            state.expected(),
        );
        return Err(SemanticRoundtripRunError::new("state-order"));
    }
    Ok(())
}

#[cfg(feature = "semantic-roundtrip-hil")]
fn advance_semantic_action(
    state: &mut SemanticRoundtripState,
    action: SemanticRoundtripAction,
) -> Result<(), SemanticRoundtripRunError> {
    state.advance(action).map_err(|error| {
        error!(
            "tx-hil stage=semantic-roundtrip-state status=FAIL action={action:?} error={error:?}"
        );
        SemanticRoundtripRunError::new("state-transition")
    })?;
    let phase: SemanticRoundtripPhase = state.phase();
    info!(
        "tx-hil stage=semantic-roundtrip-state status=ADVANCED completed={action:?} next_phase={phase:?}"
    );
    Ok(())
}

#[cfg(feature = "semantic-roundtrip-hil")]
const fn semantic_step_packet_type(step: SemanticRoundtripStep) -> PacketType {
    match step {
        SemanticRoundtripStep::InitiatorAnnounce | SemanticRoundtripStep::ResponderAnnounce => {
            PacketType::Announce
        }
        SemanticRoundtripStep::EncryptedData => PacketType::Data,
        SemanticRoundtripStep::DeliveryProof => PacketType::Proof,
    }
}

#[cfg(feature = "semantic-roundtrip-hil")]
#[allow(
    clippy::too_many_arguments,
    reason = "the HIL packet log deliberately cross-binds every independent evidence field"
)]
fn log_semantic_packet(
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
        "tx-hil stage=semantic-roundtrip-packet direction={} status={} step={step:?} rns_len={} physical_len={} sequence={} packet_hash={} destination_hash={} data_receipt={} signal_present={} rssi_dbm={} snr_db={}",
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

#[cfg(feature = "semantic-roundtrip-hil")]
#[allow(
    clippy::too_many_arguments,
    reason = "the target radio owner has explicit pin and delay type parameters"
)]
async fn transmit_semantic_packet<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>(
    radio: &mut TrackerTxHilRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
    step: SemanticRoundtripStep,
    sequence: u8,
    raw: &[u8],
    covered_data_receipt: Option<&[u8; 32]>,
    tx_done: &mut u8,
) -> Result<[u8; 32], SemanticRoundtripRunError>
where
    Spi: embedded_hal_async::spi::SpiDevice<u8>,
    Reset: embedded_hal::digital::OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: embedded_hal::digital::OutputPin,
    Csd: embedded_hal::digital::OutputPin,
    Ctx: embedded_hal::digital::StatefulOutputPin,
    FemDelay: embedded_hal_async::delay::DelayNs,
    RadioDelay: embedded_hal_async::delay::DelayNs,
{
    if sequence != step.sequence() || *tx_done >= 2 {
        error!(
            "tx-hil stage=semantic-roundtrip-tx status=FAIL step={step:?} sequence={} expected_sequence={} tx_done={} reason=local-budget-or-sequence",
            sequence,
            step.sequence(),
            *tx_done,
        );
        return Err(SemanticRoundtripRunError::new("tx-local-policy"));
    }

    let parsed = Packet::parse(raw).map_err(|error| {
        error!(
            "tx-hil stage=semantic-roundtrip-tx status=FAIL step={step:?} reason=rns-parse error={error:?}"
        );
        SemanticRoundtripRunError::new("tx-rns-parse")
    })?;
    if parsed.packet_type != semantic_step_packet_type(step) {
        error!(
            "tx-hil stage=semantic-roundtrip-tx status=FAIL step={step:?} packet_type={:?} reason=wrong-rns-type",
            parsed.packet_type,
        );
        return Err(SemanticRoundtripRunError::new("tx-rns-type"));
    }
    let packet_hash = parsed.compute_hash();
    if step == SemanticRoundtripStep::EncryptedData && covered_data_receipt != Some(&packet_hash) {
        error!(
            "tx-hil stage=semantic-roundtrip-tx status=FAIL step={step:?} packet_hash={} data_receipt={} reason=receipt-hash-mismatch",
            HexBytes(&packet_hash),
            OptionalHexBytes(covered_data_receipt.map(|receipt| receipt.as_slice())),
        );
        return Err(SemanticRoundtripRunError::new("data-receipt-hash"));
    }
    if step == SemanticRoundtripStep::DeliveryProof && covered_data_receipt.is_none() {
        return Err(SemanticRoundtripRunError::new("proof-missing-data-receipt"));
    }

    let mut first = [0_u8; SX1262_FRAME_MTU];
    let mut second = [0_u8; SX1262_FRAME_MTU];
    let frames = frame_rns_packet(raw, sequence, &mut first, &mut second).map_err(|error| {
        error!(
            "tx-hil stage=semantic-roundtrip-frame status=FAIL direction=tx step={step:?} reason=frame error={error:?}"
        );
        SemanticRoundtripRunError::new("tx-frame")
    })?;
    if frames.second().is_some() {
        error!(
            "tx-hil stage=semantic-roundtrip-frame status=FAIL direction=tx step={step:?} reason=split-frame-disallowed"
        );
        return Err(SemanticRoundtripRunError::new("tx-split-frame"));
    }
    validate_semantic_roundtrip_frame(step, frames.first()).map_err(|error| {
        error!(
            "tx-hil stage=semantic-roundtrip-frame status=FAIL direction=tx step={step:?} error={error:?}"
        );
        SemanticRoundtripRunError::new("tx-frame-validation")
    })?;

    match with_timeout(
        Duration::from_millis(TRANSMIT_SOFTWARE_DEADLINE_MS),
        radio.transmit_frame(frames.first()),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(fault)) => {
            error!(
                "tx-hil stage=semantic-roundtrip-tx status=FAIL step={step:?} no_retry=true operation={:?} radio={:?}",
                fault.operation, fault.radio,
            );
            return Err(SemanticRoundtripRunError::new("radio-tx"));
        }
        Err(_) => {
            error!(
                "tx-hil stage=semantic-roundtrip-tx status=FAIL step={step:?} no_retry=true reason=software-deadline action=radio-owner-fail-closed"
            );
            return Err(SemanticRoundtripRunError::new("radio-tx-deadline"));
        }
    }

    *tx_done = tx_done.saturating_add(1);
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
        SemanticRoundtripStep::InitiatorAnnounce | SemanticRoundtripStep::ResponderAnnounce => None,
    };
    log_semantic_packet(
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

#[cfg(feature = "semantic-roundtrip-hil")]
#[allow(
    clippy::too_many_arguments,
    reason = "the target radio owner has explicit pin and delay type parameters"
)]
async fn receive_semantic_packet<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>(
    radio: &mut TrackerTxHilRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
    receiver: &mut TimedRnodeRx,
    role: HilRole,
    step: SemanticRoundtripStep,
    covered_data_receipt: Option<&[u8; 32]>,
    tx_done: u8,
    packet_output: &mut [u8; RNODE_HW_MTU],
) -> Result<SemanticReceivedPacket, SemanticRoundtripRunError>
where
    Spi: embedded_hal_async::spi::SpiDevice<u8>,
    Reset: embedded_hal::digital::OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: embedded_hal::digital::OutputPin,
    Csd: embedded_hal::digital::OutputPin,
    Ctx: embedded_hal::digital::StatefulOutputPin,
    FemDelay: embedded_hal_async::delay::DelayNs,
    RadioDelay: embedded_hal_async::delay::DelayNs,
{
    let mut physical = [0_u8; SX1262_FRAME_MTU];
    info!(
        "tx-hil stage=semantic-roundtrip-rx status=ARMED role={} step={step:?} maximum_windows={} tx_done={}",
        role.label(),
        SEMANTIC_ROUNDTRIP_RX_WINDOWS,
        tx_done,
    );
    for window in 1..=SEMANTIC_ROUNDTRIP_RX_WINDOWS {
        let received = match with_timeout(
            Duration::from_millis(RECEIVE_SOFTWARE_DEADLINE_MS),
            radio.receive_frame(&mut physical),
        )
        .await
        {
            Ok(Ok(Some(received))) => received,
            Ok(Ok(None)) => continue,
            Ok(Err(fault)) => {
                error!(
                    "tx-hil stage=semantic-roundtrip-rx status=FAIL step={step:?} window={} operation={:?} radio={:?}",
                    window, fault.operation, fault.radio,
                );
                return Err(SemanticRoundtripRunError::new("radio-rx"));
            }
            Err(_) => {
                error!(
                    "tx-hil stage=semantic-roundtrip-rx status=FAIL step={step:?} window={} reason=software-deadline action=radio-owner-fail-closed",
                    window,
                );
                return Err(SemanticRoundtripRunError::new("radio-rx-deadline"));
            }
        };

        let physical_len = usize::from(received.len);
        let frame = &physical[..physical_len];
        validate_semantic_roundtrip_frame(step, frame).map_err(|error| {
            error!(
                "tx-hil stage=semantic-roundtrip-frame status=FAIL direction=rx step={step:?} window={} physical_len={} error={error:?}",
                window, physical_len,
            );
            SemanticRoundtripRunError::new("rx-frame-validation")
        })?;
        let signal = FrameSignal::new(received.rssi_dbm, received.snr_db);
        let outcome = receiver
            .feed(frame, semantic_rnode_ticks(), signal, packet_output)
            .map_err(|error| {
                error!(
                    "tx-hil stage=semantic-roundtrip-rnode-rx status=FAIL step={step:?} window={} error={error:?}",
                    window,
                );
                SemanticRoundtripRunError::new("rx-rnode")
            })?;
        let TimedReceiveOutcome::Packet { packet_len, .. } = outcome else {
            error!(
                "tx-hil stage=semantic-roundtrip-rnode-rx status=FAIL step={step:?} window={} reason=canonical-unsplit-frame-was-incomplete",
                window,
            );
            return Err(SemanticRoundtripRunError::new("rx-unexpected-fragment"));
        };

        let raw = &packet_output[..packet_len];
        let parsed = Packet::parse(raw).map_err(|error| {
            error!(
                "tx-hil stage=semantic-roundtrip-rx status=FAIL step={step:?} reason=rns-parse error={error:?}"
            );
            SemanticRoundtripRunError::new("rx-rns-parse")
        })?;
        if parsed.packet_type != semantic_step_packet_type(step) {
            error!(
                "tx-hil stage=semantic-roundtrip-rx status=FAIL step={step:?} packet_type={:?} reason=wrong-rns-type",
                parsed.packet_type,
            );
            return Err(SemanticRoundtripRunError::new("rx-rns-type"));
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
        log_semantic_packet(
            "rx",
            "RNODE_PACKET",
            step,
            packet_len,
            physical_len,
            &packet_hash,
            destination,
            data_receipt,
            Some(signal),
        );
        return Ok(SemanticReceivedPacket {
            packet_len,
            packet_hash,
        });
    }

    error!(
        "tx-hil stage=semantic-roundtrip-rx status=FAIL step={step:?} reason=receive-window-exhausted maximum_windows={SEMANTIC_ROUNDTRIP_RX_WINDOWS}"
    );
    Err(SemanticRoundtripRunError::new("rx-window-exhausted"))
}

#[cfg(feature = "semantic-roundtrip-hil")]
fn ingest_semantic_announce(
    role: HilRole,
    node: &mut SemanticRoundtripNode,
    peer_destination: &DestHash,
    raw: &[u8],
    rng: &mut Trng,
    step: SemanticRoundtripStep,
) -> Result<(), SemanticRoundtripRunError> {
    let parsed =
        Packet::parse(raw).map_err(|_| SemanticRoundtripRunError::new("announce-parse"))?;
    if parsed.packet_type != PacketType::Announce
        || parsed.destination_hash != peer_destination.as_ref()
    {
        error!(
            "tx-hil stage=semantic-roundtrip-announce-ingress status=FAIL role={} step={step:?} observed_destination={} expected_destination={}",
            role.label(),
            HexBytes(parsed.destination_hash),
            HexBytes(peer_destination.as_bytes()),
        );
        return Err(SemanticRoundtripRunError::new("announce-peer-mismatch"));
    }

    log_semantic_roundtrip_heap("before-announce-validation", role);
    let report = node.ingest(
        raw,
        semantic_transport_seconds(),
        SEMANTIC_ROUNDTRIP_INTERFACE,
        rng,
    );
    log_semantic_roundtrip_heap("after-announce-validation", role);
    let event_is_exact = matches!(
        report.actions.events.as_slice(),
        [NodeEvent::AnnounceReceived { dest_hash, .. }] if dest_hash == peer_destination
    );
    if report.disposition != IngressDisposition::Processed
        || !event_is_exact
        || !report.actions.packets.is_empty()
        || report.actions.unroutable_packets != 0
        || node.route(peer_destination).is_none()
    {
        error!(
            "tx-hil stage=semantic-roundtrip-announce-ingress status=FAIL role={} step={step:?} disposition={:?} events={} packets={} unroutable={} route_learned={}",
            role.label(),
            report.disposition,
            report.actions.events.len(),
            report.actions.packets.len(),
            report.actions.unroutable_packets,
            node.route(peer_destination).is_some(),
        );
        return Err(SemanticRoundtripRunError::new("announce-ingress"));
    }
    info!(
        "tx-hil stage=semantic-roundtrip-announce-ingress status=SEMANTIC_VALIDATED role={} step={step:?} peer_destination={} route_learned=true extra_actions=0",
        role.label(),
        HexBytes(peer_destination.as_bytes()),
    );
    Ok(())
}

#[cfg(feature = "semantic-roundtrip-hil")]
#[allow(
    clippy::too_many_arguments,
    reason = "this one-shot HIL owns fixed caller buffers while its large Rete node lives in StaticCell"
)]
async fn run_semantic_roundtrip_inline<
    Spi,
    Reset,
    Dio1,
    Busy,
    Power,
    Csd,
    Ctx,
    FemDelay,
    RadioDelay,
>(
    role: HilRole,
    base_mac: &[u8],
    radio: &mut TrackerTxHilRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
    node: &mut SemanticRoundtripNode,
    rng: &mut Trng,
) -> Result<SemanticRoundtripSuccess, SemanticRoundtripRunError>
where
    Spi: embedded_hal_async::spi::SpiDevice<u8>,
    Reset: embedded_hal::digital::OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: embedded_hal::digital::OutputPin,
    Csd: embedded_hal::digital::OutputPin,
    Ctx: embedded_hal::digital::StatefulOutputPin,
    FemDelay: embedded_hal_async::delay::DelayNs,
    RadioDelay: embedded_hal_async::delay::DelayNs,
{
    let local_destination = node.destination_hash();
    let peer_destination =
        semantic_roundtrip_peer_destination_for_base_mac(base_mac).map_err(|error| {
            error!(
                "tx-hil stage=semantic-roundtrip-peer status=FAIL role={} error={error:?}",
                role.label(),
            );
            SemanticRoundtripRunError::new("peer-destination")
        })?;
    let (initiator_destination, responder_destination) = match role {
        HilRole::Initiator => (local_destination, peer_destination),
        HilRole::Responder => (peer_destination, local_destination),
        HilRole::Inert => return Err(SemanticRoundtripRunError::new("inert-role")),
    };
    if (role == HilRole::Initiator && base_mac != INITIATOR_BASE_MAC)
        || (role == HilRole::Responder && base_mac != RESPONDER_BASE_MAC)
    {
        return Err(SemanticRoundtripRunError::new("role-mac-mismatch"));
    }
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
        return Err(SemanticRoundtripRunError::new("payload-fixture"));
    }

    let mut state = SemanticRoundtripState::new(role).map_err(|error| {
        error!(
            "tx-hil stage=semantic-roundtrip-state status=FAIL role={} error={error:?}",
            role.label(),
        );
        SemanticRoundtripRunError::new("state-construction")
    })?;
    let initial_phase: SemanticRoundtripPhase = state.phase();
    info!(
        "tx-hil stage=semantic-roundtrip-start status=ARMED role={} phase={initial_phase:?} local_destination={} peer_destination={} payload_len={} tx_budget=2 maximum_rx_windows={}",
        role.label(),
        HexBytes(local_destination.as_bytes()),
        HexBytes(peer_destination.as_bytes()),
        payload.len(),
        SEMANTIC_ROUNDTRIP_RX_WINDOWS,
    );
    let mut receiver = TimedRnodeRx::new(SEMANTIC_ROUNDTRIP_FRAGMENT_TIMEOUT_TICKS);
    let mut packet = [0_u8; RNODE_HW_MTU];
    let mut tx_done = 0_u8;

    let data_receipt = match role {
        HilRole::Initiator => {
            let announce_action =
                SemanticRoundtripAction::Transmit(SemanticRoundtripStep::InitiatorAnnounce);
            ensure_semantic_action(state, announce_action)?;
            info!(
                "tx-hil stage=semantic-roundtrip-delay role=initiator purpose=responder-startup delay_driver=blocking-esp-hal delay_ms={INITIATOR_STARTUP_DELAY_MS} tx_done=0"
            );
            BlockingDelay::new().delay_millis(INITIATOR_STARTUP_DELAY_MS);
            let mut announce = [0_u8; RNS_MTU];
            log_semantic_roundtrip_heap("before-initiator-announce-sign", role);
            let announce_len = prepare_semantic_roundtrip_announce(
                node,
                semantic_transport_seconds(),
                rng,
                &mut announce,
            )
            .map_err(|error| {
                error!(
                    "tx-hil stage=semantic-roundtrip-announce-build status=FAIL role=initiator error={error:?}"
                );
                SemanticRoundtripRunError::new("initiator-announce-build")
            })?;
            log_semantic_roundtrip_heap("after-initiator-announce-sign", role);
            transmit_semantic_packet(
                radio,
                SemanticRoundtripStep::InitiatorAnnounce,
                SEMANTIC_ROUNDTRIP_INITIATOR_ANNOUNCE_SEQUENCE,
                &announce[..announce_len],
                None,
                &mut tx_done,
            )
            .await?;
            advance_semantic_action(&mut state, announce_action)?;

            let responder_announce_action =
                SemanticRoundtripAction::Receive(SemanticRoundtripStep::ResponderAnnounce);
            ensure_semantic_action(state, responder_announce_action)?;
            let received = receive_semantic_packet(
                radio,
                &mut receiver,
                role,
                SemanticRoundtripStep::ResponderAnnounce,
                None,
                tx_done,
                &mut packet,
            )
            .await?;
            ingest_semantic_announce(
                role,
                node,
                &peer_destination,
                &packet[..received.packet_len],
                rng,
                SemanticRoundtripStep::ResponderAnnounce,
            )?;
            advance_semantic_action(&mut state, responder_announce_action)?;

            info!(
                "tx-hil stage=semantic-roundtrip-delay role=initiator purpose=responder-rx-rearm delay_driver=blocking-esp-hal delay_ms={SEMANTIC_ROUNDTRIP_DATA_GUARD_DELAY_MS} tx_done={tx_done}"
            );
            BlockingDelay::new().delay_millis(SEMANTIC_ROUNDTRIP_DATA_GUARD_DELAY_MS);
            let data_action =
                SemanticRoundtripAction::Transmit(SemanticRoundtripStep::EncryptedData);
            ensure_semantic_action(state, data_action)?;
            let mut encrypted = [0_u8; RNS_MTU];
            log_semantic_roundtrip_heap("before-data-encrypt", role);
            let prepared = node
                .prepare_data_into(
                    &peer_destination,
                    &payload,
                    semantic_transport_seconds(),
                    rng,
                    &mut encrypted,
                )
                .map_err(|error| {
                    error!(
                        "tx-hil stage=semantic-roundtrip-data-build status=FAIL role=initiator error={error:?}"
                    );
                    SemanticRoundtripRunError::new("data-encrypt")
                })?;
            log_semantic_roundtrip_heap("after-data-encrypt", role);
            let receipt = *prepared.receipt().as_bytes();
            transmit_semantic_packet(
                radio,
                SemanticRoundtripStep::EncryptedData,
                SEMANTIC_ROUNDTRIP_DATA_SEQUENCE,
                &encrypted[..usize::from(prepared.packet_len())],
                Some(&receipt),
                &mut tx_done,
            )
            .await?;
            advance_semantic_action(&mut state, data_action)?;

            let proof_action =
                SemanticRoundtripAction::Receive(SemanticRoundtripStep::DeliveryProof);
            ensure_semantic_action(state, proof_action)?;
            let received = receive_semantic_packet(
                radio,
                &mut receiver,
                role,
                SemanticRoundtripStep::DeliveryProof,
                Some(&receipt),
                tx_done,
                &mut packet,
            )
            .await?;
            let mut sink = SemanticReceiptSink::new(prepared.receipt());
            log_semantic_roundtrip_heap("before-proof-validation", role);
            let report = node
                .ingest_with_receipt_sink(
                    &packet[..received.packet_len],
                    semantic_transport_seconds(),
                    SEMANTIC_ROUNDTRIP_INTERFACE,
                    rng,
                    &mut sink,
                )
                .map_err(|_| SemanticRoundtripRunError::new("proof-receipt-backpressure"))?;
            log_semantic_roundtrip_heap("after-proof-validation", role);
            let delivered = matches!(
                sink.terminal,
                Some(ReceiptTerminal::Delivered(candidate))
                    if candidate.kind() == ReceiptKind::Data
                        && candidate.receipt() == prepared.receipt()
            );
            let receipt_slots_used = node.metrics().capacity.receipts.used;
            if report.disposition != IngressDisposition::Processed
                || !report.actions.events.is_empty()
                || !report.actions.packets.is_empty()
                || report.actions.unroutable_packets != 0
                || sink.reservation_attempts != 1
                || !delivered
                || receipt_slots_used != 0
            {
                error!(
                    "tx-hil stage=semantic-roundtrip-proof-ingress status=FAIL disposition={:?} events={} packets={} unroutable={} reservation_attempts={} delivered={} receipt_slots_used={}",
                    report.disposition,
                    report.actions.events.len(),
                    report.actions.packets.len(),
                    report.actions.unroutable_packets,
                    sink.reservation_attempts,
                    delivered,
                    receipt_slots_used,
                );
                return Err(SemanticRoundtripRunError::new("proof-ingress"));
            }
            info!(
                "tx-hil stage=semantic-roundtrip-proof-ingress status=SEMANTIC_VALIDATED role=initiator receipt_kind=Data candidate={} terminal=Delivered receipt_slots_used={} extra_actions=0 proof_packet_hash={}",
                HexBytes(&receipt),
                receipt_slots_used,
                HexBytes(&received.packet_hash),
            );
            advance_semantic_action(&mut state, proof_action)?;
            receipt
        }
        HilRole::Responder => {
            let initiator_announce_action =
                SemanticRoundtripAction::Receive(SemanticRoundtripStep::InitiatorAnnounce);
            ensure_semantic_action(state, initiator_announce_action)?;
            let received = receive_semantic_packet(
                radio,
                &mut receiver,
                role,
                SemanticRoundtripStep::InitiatorAnnounce,
                None,
                tx_done,
                &mut packet,
            )
            .await?;
            ingest_semantic_announce(
                role,
                node,
                &peer_destination,
                &packet[..received.packet_len],
                rng,
                SemanticRoundtripStep::InitiatorAnnounce,
            )?;
            advance_semantic_action(&mut state, initiator_announce_action)?;

            let announce_action =
                SemanticRoundtripAction::Transmit(SemanticRoundtripStep::ResponderAnnounce);
            ensure_semantic_action(state, announce_action)?;
            let mut announce = [0_u8; RNS_MTU];
            log_semantic_roundtrip_heap("before-responder-announce-sign", role);
            let announce_len = prepare_semantic_roundtrip_announce(
                node,
                semantic_transport_seconds(),
                rng,
                &mut announce,
            )
            .map_err(|error| {
                error!(
                    "tx-hil stage=semantic-roundtrip-announce-build status=FAIL role=responder error={error:?}"
                );
                SemanticRoundtripRunError::new("responder-announce-build")
            })?;
            log_semantic_roundtrip_heap("after-responder-announce-sign", role);
            transmit_semantic_packet(
                radio,
                SemanticRoundtripStep::ResponderAnnounce,
                SEMANTIC_ROUNDTRIP_RESPONDER_ANNOUNCE_SEQUENCE,
                &announce[..announce_len],
                None,
                &mut tx_done,
            )
            .await?;
            advance_semantic_action(&mut state, announce_action)?;

            let data_action =
                SemanticRoundtripAction::Receive(SemanticRoundtripStep::EncryptedData);
            ensure_semantic_action(state, data_action)?;
            let received = receive_semantic_packet(
                radio,
                &mut receiver,
                role,
                SemanticRoundtripStep::EncryptedData,
                None,
                tx_done,
                &mut packet,
            )
            .await?;
            let receipt = received.packet_hash;
            log_semantic_roundtrip_heap("before-data-decrypt-and-proof", role);
            let report = node.ingest(
                &packet[..received.packet_len],
                semantic_transport_seconds(),
                SEMANTIC_ROUNDTRIP_INTERFACE,
                rng,
            );
            log_semantic_roundtrip_heap("after-data-decrypt-and-proof", role);
            let payload_is_exact = matches!(
                report.actions.events.as_slice(),
                [NodeEvent::DataReceived { dest_hash, payload }]
                    if dest_hash == &local_destination
                        && validate_semantic_roundtrip_payload(
                            payload,
                            initiator_destination.as_bytes(),
                            responder_destination.as_bytes(),
                        )
            );
            if report.disposition != IngressDisposition::Processed
                || !payload_is_exact
                || report.actions.packets.len() != 1
                || report.actions.unroutable_packets != 0
                || report.actions.packets[0].target()
                    != TxTarget::Only(SEMANTIC_ROUNDTRIP_INTERFACE)
            {
                error!(
                    "tx-hil stage=semantic-roundtrip-data-ingress status=FAIL disposition={:?} payload_exact={} events={} packets={} unroutable={}",
                    report.disposition,
                    payload_is_exact,
                    report.actions.events.len(),
                    report.actions.packets.len(),
                    report.actions.unroutable_packets,
                );
                return Err(SemanticRoundtripRunError::new("data-ingress"));
            }
            let proof = &report.actions.packets[0];
            let proof_packet = Packet::parse(proof.bytes())
                .map_err(|_| SemanticRoundtripRunError::new("generated-proof-parse"))?;
            if proof_packet.packet_type != PacketType::Proof {
                return Err(SemanticRoundtripRunError::new("generated-action-not-proof"));
            }
            info!(
                "tx-hil stage=semantic-roundtrip-data-ingress status=SEMANTIC_VALIDATED role=responder payload_len={} destination={} data_receipt={} proof_actions=1 extra_actions=0",
                SEMANTIC_ROUNDTRIP_PAYLOAD_LEN,
                HexBytes(local_destination.as_bytes()),
                HexBytes(&receipt),
            );
            advance_semantic_action(&mut state, data_action)?;

            let proof_action =
                SemanticRoundtripAction::Transmit(SemanticRoundtripStep::DeliveryProof);
            ensure_semantic_action(state, proof_action)?;
            transmit_semantic_packet(
                radio,
                SemanticRoundtripStep::DeliveryProof,
                SEMANTIC_ROUNDTRIP_PROOF_SEQUENCE,
                proof.bytes(),
                Some(&receipt),
                &mut tx_done,
            )
            .await?;
            advance_semantic_action(&mut state, proof_action)?;
            receipt
        }
        HilRole::Inert => return Err(SemanticRoundtripRunError::new("inert-role")),
    };

    if tx_done != 2 || !state.is_complete() || state.phase() != SemanticRoundtripPhase::Complete {
        error!(
            "tx-hil stage=semantic-roundtrip-completion status=FAIL role={} tx_done={} phase={:?}",
            role.label(),
            tx_done,
            state.phase(),
        );
        return Err(SemanticRoundtripRunError::new("terminal-invariant"));
    }
    Ok(SemanticRoundtripSuccess {
        tx_done,
        local_destination: *local_destination.as_bytes(),
        peer_destination: *peer_destination.as_bytes(),
        data_receipt,
    })
}

#[cfg(not(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil")))]
async fn run_initiator_inline<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>(
    radio: &mut TrackerTxHilRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
) where
    Spi: embedded_hal_async::spi::SpiDevice<u8>,
    Reset: embedded_hal::digital::OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: embedded_hal::digital::OutputPin,
    Csd: embedded_hal::digital::OutputPin,
    Ctx: embedded_hal::digital::StatefulOutputPin,
    FemDelay: embedded_hal_async::delay::DelayNs,
    RadioDelay: embedded_hal_async::delay::DelayNs,
{
    info!(
        "tx-hil stage=initiator-delay bounded=true delay_driver=blocking-esp-hal delay_ms={INITIATOR_STARTUP_DELAY_MS} tx_sent=0"
    );
    // This one-shot HIL delay exists only to let the independently started
    // observer settle. Keep it off the async timer queue so a missed long
    // wakeup cannot invalidate the RF experiment before its sole transmission.
    BlockingDelay::new().delay_millis(INITIATOR_STARTUP_DELAY_MS);

    let mut first = [0_u8; SX1262_FRAME_MTU];
    let mut second = [0_u8; SX1262_FRAME_MTU];
    let frames =
        match frame_rns_packet(HIL_PING_PAYLOAD, HIL_PING_SEQUENCE, &mut first, &mut second) {
            Ok(frames) if frames.second().is_none() => frames,
            Ok(_) | Err(_) => {
                error!("tx-hil stage=ping-frame status=FAIL action=no-transmit");
                return;
            }
        };
    info!(
        "tx-hil stage=ping-frame status=PREPARED physical_len={} rnode_header=0x{:02x} payload_hex={} evidence_layer=phy+rnode-framing rns_valid=false rns_reason=payload_len_{}_below_minimum_{}",
        frames.first().len(),
        frames.first()[0],
        HexBytes(HIL_PING_PAYLOAD),
        HIL_PING_PAYLOAD.len(),
        RNS_MINIMUM_PACKET_LEN,
    );
    match with_timeout(
        Duration::from_millis(TRANSMIT_SOFTWARE_DEADLINE_MS),
        radio.transmit_frame(frames.first()),
    )
    .await
    {
        Ok(Ok(())) => info!(
            "tx-hil stage=ping-tx status=DRIVER_TX_DONE tx_sent=1 tx_budget_frames=1 target_antenna_path_dbm={} sx1262_output_dbm={} evidence_layer=phy+rnode-framing independent_rf_observation_required=true rns_valid=false",
            TRACKER_TX_HIL_TARGET_POWER_DBM, TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
        ),
        Ok(Err(fault)) => {
            error!(
                "tx-hil stage=ping-tx status=FAIL tx_attempts=1 no_retry=true operation={:?} radio={:?}",
                fault.operation, fault.radio,
            );
            return;
        }
        Err(_) => {
            error!(
                "tx-hil stage=ping-tx status=FAIL tx_attempts=1 no_retry=true reason=software-deadline action=radio-owner-fail-closed"
            );
            return;
        }
    }

    let mut buffer = [0_u8; SX1262_FRAME_MTU];
    let mut reply_observed = false;
    for window in 1..=INITIATOR_REPLY_RX_WINDOWS {
        match with_timeout(
            Duration::from_millis(RECEIVE_SOFTWARE_DEADLINE_MS),
            radio.receive_frame(&mut buffer),
        )
        .await
        {
            Ok(Ok(Some(received))) => {
                let frame = &buffer[..usize::from(received.len)];
                let kind = classify_hil_frame(frame);
                info!(
                    "tx-hil stage=initiator-rx window={} physical_len={} rssi_dbm={} snr_db={} hil_kind={kind:?} evidence_layer=phy+rnode-framing rns_status={}",
                    window,
                    received.len,
                    received.rssi_dbm,
                    received.snr_db,
                    if kind == HilFrameKind::Other {
                        "not-evaluated"
                    } else {
                        "invalid-hil-sentinel"
                    },
                );
                if kind == HilFrameKind::Reply {
                    reply_observed = true;
                    break;
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(fault)) => {
                error!(
                    "tx-hil stage=initiator-rx status=FAIL window={} operation={:?} radio={:?}",
                    window, fault.operation, fault.radio,
                );
                break;
            }
            Err(_) => {
                error!(
                    "tx-hil stage=initiator-rx status=FAIL window={} reason=software-deadline action=radio-owner-fail-closed",
                    window,
                );
                break;
            }
        }
    }
    info!(
        "tx-hil stage=reply-evidence status={} reply_observed={} maximum_windows={} tx_sent=1 evidence_layer=phy+rnode-framing rns_valid=false",
        if reply_observed {
            "PASS"
        } else {
            "NOT_OBSERVED"
        },
        reply_observed,
        INITIATOR_REPLY_RX_WINDOWS,
    );
}

#[cfg(all(
    feature = "semantic-announce-hil",
    not(feature = "semantic-roundtrip-hil")
))]
async fn run_semantic_announce_initiator_inline<
    Spi,
    Reset,
    Dio1,
    Busy,
    Power,
    Csd,
    Ctx,
    FemDelay,
    RadioDelay,
>(
    radio: &mut TrackerTxHilRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
) where
    Spi: embedded_hal_async::spi::SpiDevice<u8>,
    Reset: embedded_hal::digital::OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: embedded_hal::digital::OutputPin,
    Csd: embedded_hal::digital::OutputPin,
    Ctx: embedded_hal::digital::StatefulOutputPin,
    FemDelay: embedded_hal_async::delay::DelayNs,
    RadioDelay: embedded_hal_async::delay::DelayNs,
{
    info!(
        "tx-hil stage=initiator-delay bounded=true delay_driver=blocking-esp-hal delay_ms={INITIATOR_STARTUP_DELAY_MS} tx_sent=0 hil_mode=semantic-announce"
    );
    BlockingDelay::new().delay_millis(INITIATOR_STARTUP_DELAY_MS);

    let mut raw = [0_u8; RNS_MTU];
    let packet_len = match build_semantic_announce_packet(&mut raw) {
        Ok(packet_len) => packet_len,
        Err(error) => {
            error!(
                "tx-hil stage=semantic-announce-build status=FAIL error={error:?} tx_sent=0 action=no-transmit"
            );
            return;
        }
    };

    let mut first = [0_u8; SX1262_FRAME_MTU];
    let mut second = [0_u8; SX1262_FRAME_MTU];
    let frames = match frame_rns_packet(
        &raw[..packet_len],
        HIL_PING_SEQUENCE,
        &mut first,
        &mut second,
    ) {
        Ok(frames) if frames.second().is_none() => frames,
        Ok(_) | Err(_) => {
            error!("tx-hil stage=semantic-announce-frame status=FAIL tx_sent=0 action=no-transmit");
            return;
        }
    };
    info!(
        "tx-hil stage=semantic-announce-frame status=PREPARED rns_len={} physical_len={} rnode_header=0x{:02x} destination_hash={} packet_hash={} deterministic_fixture=true rete_validation=PASS rns_valid=true",
        packet_len,
        frames.first().len(),
        frames.first()[0],
        HexBytes(&SEMANTIC_ANNOUNCE_DESTINATION_HASH),
        HexBytes(&SEMANTIC_ANNOUNCE_PACKET_HASH),
    );

    match with_timeout(
        Duration::from_millis(TRANSMIT_SOFTWARE_DEADLINE_MS),
        radio.transmit_frame(frames.first()),
    )
    .await
    {
        Ok(Ok(())) => info!(
            "tx-hil stage=semantic-announce-tx status=DRIVER_TX_DONE tx_sent=1 tx_budget_frames=1 no_retry=true target_antenna_path_dbm={} sx1262_output_dbm={} evidence_layer=phy+rnode-framing+rns-announce independent_rf_and_reference_observation_required=true rns_valid=true",
            TRACKER_TX_HIL_TARGET_POWER_DBM, TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
        ),
        Ok(Err(fault)) => error!(
            "tx-hil stage=semantic-announce-tx status=FAIL tx_attempts=1 no_retry=true operation={:?} radio={:?}",
            fault.operation, fault.radio,
        ),
        Err(_) => error!(
            "tx-hil stage=semantic-announce-tx status=FAIL tx_attempts=1 no_retry=true reason=software-deadline action=radio-owner-fail-closed"
        ),
    }
}

#[cfg(not(any(feature = "semantic-announce-hil", feature = "semantic-roundtrip-hil")))]
async fn run_responder_inline<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>(
    radio: &mut TrackerTxHilRadio<Spi, Reset, Dio1, Busy, Power, Csd, Ctx, FemDelay, RadioDelay>,
) where
    Spi: embedded_hal_async::spi::SpiDevice<u8>,
    Reset: embedded_hal::digital::OutputPin,
    Dio1: Wait,
    Busy: Wait,
    Power: embedded_hal::digital::OutputPin,
    Csd: embedded_hal::digital::OutputPin,
    Ctx: embedded_hal::digital::StatefulOutputPin,
    FemDelay: embedded_hal_async::delay::DelayNs,
    RadioDelay: embedded_hal_async::delay::DelayNs,
{
    info!(
        "tx-hil stage=responder-rx status=ARMED maximum_windows={} whole_operation_outer_deadline_ms={} tx_sent=0 tx_budget_frames=1",
        RESPONDER_RX_WINDOWS, RECEIVE_SOFTWARE_DEADLINE_MS,
    );
    let mut buffer = [0_u8; SX1262_FRAME_MTU];
    let mut ping_observed = false;
    for window in 1..=RESPONDER_RX_WINDOWS {
        match with_timeout(
            Duration::from_millis(RECEIVE_SOFTWARE_DEADLINE_MS),
            radio.receive_frame(&mut buffer),
        )
        .await
        {
            Ok(Ok(Some(received))) => {
                let frame = &buffer[..usize::from(received.len)];
                let kind = classify_hil_frame(frame);
                info!(
                    "tx-hil stage=responder-rx window={} physical_len={} rssi_dbm={} snr_db={} hil_kind={kind:?} evidence_layer=phy+rnode-framing rns_status={}",
                    window,
                    received.len,
                    received.rssi_dbm,
                    received.snr_db,
                    if kind == HilFrameKind::Other {
                        "not-evaluated"
                    } else {
                        "invalid-hil-sentinel"
                    },
                );
                if kind == HilFrameKind::Ping {
                    ping_observed = true;
                    break;
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(fault)) => {
                error!(
                    "tx-hil stage=responder-rx status=FAIL window={} operation={:?} radio={:?}",
                    window, fault.operation, fault.radio,
                );
                break;
            }
            Err(_) => {
                error!(
                    "tx-hil stage=responder-rx status=FAIL window={} reason=software-deadline action=radio-owner-fail-closed",
                    window,
                );
                break;
            }
        }
    }

    if !ping_observed {
        info!(
            "tx-hil stage=ping-evidence status=NOT_OBSERVED maximum_windows={} tx_sent=0 evidence_layer=phy+rnode-framing rns_valid=false",
            RESPONDER_RX_WINDOWS,
        );
        return;
    }

    info!(
        "tx-hil stage=ping-evidence status=PASS payload_hex={} tx_sent=0 evidence_layer=phy+rnode-framing rns_valid=false rns_reason=payload_len_{}_below_minimum_{}",
        HexBytes(HIL_PING_PAYLOAD),
        HIL_PING_PAYLOAD.len(),
        RNS_MINIMUM_PACKET_LEN,
    );
    let mut first = [0_u8; SX1262_FRAME_MTU];
    let mut second = [0_u8; SX1262_FRAME_MTU];
    let frames = match frame_rns_packet(
        HIL_REPLY_PAYLOAD,
        HIL_REPLY_SEQUENCE,
        &mut first,
        &mut second,
    ) {
        Ok(frames) if frames.second().is_none() => frames,
        Ok(_) | Err(_) => {
            error!("tx-hil stage=reply-frame status=FAIL action=no-transmit");
            return;
        }
    };
    info!(
        "tx-hil stage=reply-frame status=PREPARED physical_len={} rnode_header=0x{:02x} payload_hex={} evidence_layer=phy+rnode-framing rns_valid=false",
        frames.first().len(),
        frames.first()[0],
        HexBytes(HIL_REPLY_PAYLOAD),
    );
    match with_timeout(
        Duration::from_millis(TRANSMIT_SOFTWARE_DEADLINE_MS),
        radio.transmit_frame(frames.first()),
    )
    .await
    {
        Ok(Ok(())) => info!(
            "tx-hil stage=reply-tx status=DRIVER_TX_DONE tx_sent=1 tx_budget_frames=1 no_retry=true target_antenna_path_dbm={} sx1262_output_dbm={} evidence_layer=phy+rnode-framing independent_rf_observation_required=true rns_valid=false",
            TRACKER_TX_HIL_TARGET_POWER_DBM, TRACKER_TX_HIL_MODEM_OUTPUT_DBM,
        ),
        Ok(Err(fault)) => error!(
            "tx-hil stage=reply-tx status=FAIL tx_attempts=1 no_retry=true operation={:?} radio={:?}",
            fault.operation, fault.radio,
        ),
        Err(_) => error!(
            "tx-hil stage=reply-tx status=FAIL tx_attempts=1 no_retry=true reason=software-deadline action=radio-owner-fail-closed"
        ),
    }
}

async fn inert_forever() -> ! {
    loop {
        Timer::after(Duration::from_secs(INERT_HEARTBEAT_SECONDS)).await;
        info!("tx-hil stage=inert-heartbeat rf_state=reset_low_fem_low");
    }
}
