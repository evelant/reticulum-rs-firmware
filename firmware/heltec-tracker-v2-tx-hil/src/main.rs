//! Dedicated two-board LoRa/RNode-framing HIL.
//!
//! This is not product firmware. Its default mode emits only invalid short
//! sentinels. The explicit `semantic-announce-hil` mode instead emits one
//! fixed signed Reticulum conformance fixture. Radio construction is allowed
//! only for two exact factory eFuse base MACs.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

use core::future::Future;

use embassy_executor::Spawner;
use embassy_time::{Delay, Duration, Timer, with_timeout};
use embedded_hal::digital::{Error as DigitalError, ErrorKind as DigitalErrorKind, ErrorType};
use embedded_hal_async::digital::Wait;
use esp_backtrace as _;
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
#[cfg(not(feature = "semantic-announce-hil"))]
use reticulum_heltec_tracker_v2_tx_hil::{
    HIL_PING_PAYLOAD, HIL_REPLY_PAYLOAD, HIL_REPLY_SEQUENCE, HilFrameKind, RNS_MINIMUM_PACKET_LEN,
    classify_hil_frame,
};
use reticulum_heltec_tracker_v2_tx_hil::{HIL_PING_SEQUENCE, HilRole, role_for_base_mac};
#[cfg(feature = "semantic-announce-hil")]
use reticulum_heltec_tracker_v2_tx_hil::{
    SEMANTIC_ANNOUNCE_DESTINATION_HASH, SEMANTIC_ANNOUNCE_PACKET_HASH,
    build_semantic_announce_packet,
};
use reticulum_radio_interface::{SX1262_FRAME_MTU, frame_rns_packet};
#[cfg(feature = "semantic-announce-hil")]
use reticulum_rns_rete::RNS_MTU;

const TRACKER_SPI_FREQUENCY_HZ: u32 = 1_000_000;
const TRACKER_BUSY_TIMEOUT_MS: u64 = 100;
const INITIATOR_STARTUP_DELAY_MS: u32 = 3_000;
#[cfg(not(feature = "semantic-announce-hil"))]
const RESPONDER_RX_WINDOWS: u8 = 48;
#[cfg(not(feature = "semantic-announce-hil"))]
const INITIATOR_REPLY_RX_WINDOWS: u8 = 24;
const RECEIVE_SOFTWARE_DEADLINE_MS: u64 = 1_500;
const TRANSMIT_SOFTWARE_DEADLINE_MS: u64 = 1_500;
const INERT_HEARTBEAT_SECONDS: u64 = 30;

#[cfg(not(feature = "semantic-announce-hil"))]
const HIL_MODE: &str = "sentinel";
#[cfg(feature = "semantic-announce-hil")]
const HIL_MODE: &str = "semantic-announce";

#[cfg(not(feature = "semantic-announce-hil"))]
const HIL_RNS_POLICY: &str = "invalid-short-sentinel";
#[cfg(feature = "semantic-announce-hil")]
const HIL_RNS_POLICY: &str = "cryptographic-validation-required-before-transmit";

#[cfg(not(feature = "near-field-attenuation-hil"))]
const _: () =
    assert!(TRACKER_TX_HIL_TARGET_POWER_DBM == 14 && TRACKER_TX_HIL_MODEM_OUTPUT_DBM == 0);
#[cfg(feature = "near-field-attenuation-hil")]
const _: () =
    assert!(TRACKER_TX_HIL_TARGET_POWER_DBM == 5 && TRACKER_TX_HIL_MODEM_OUTPUT_DBM == -9);

static TX_ARM: TrackerTxArm = TrackerTxArm::new();

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
        "tx-hil stage=radio-init status=PASS role={} fem_state=powered_settled_ctx_rx sx1262_preamble_symbol_timeout={} receive_whole_operation_outer_deadline_ms={} transmit_whole_operation_outer_deadline_ms={} tx_budget_frames=1",
        role.label(),
        TRACKER_TX_HIL_RX_SYMBOL_TIMEOUT,
        RECEIVE_SOFTWARE_DEADLINE_MS,
        TRANSMIT_SOFTWARE_DEADLINE_MS,
    );

    match role {
        HilRole::Initiator => {
            #[cfg(not(feature = "semantic-announce-hil"))]
            run_initiator_inline(&mut radio).await;
            #[cfg(feature = "semantic-announce-hil")]
            run_semantic_announce_initiator_inline(&mut radio).await;
        }
        HilRole::Responder => {
            #[cfg(not(feature = "semantic-announce-hil"))]
            run_responder_inline(&mut radio).await;
            #[cfg(feature = "semantic-announce-hil")]
            info!(
                "tx-hil stage=semantic-responder status=TX_DISABLED tx_sent=0 tx_budget_frames=0 reason=semantic-mode-has-one-initiator-only"
            );
        }
        HilRole::Inert => {}
    }

    radio.shutdown();
    info!(
        "tx-hil stage=complete role={} radio_active={} action=permanent-rf-inert-hold",
        role.label(),
        radio.is_active(),
    );
    inert_forever().await
}

#[cfg(not(feature = "semantic-announce-hil"))]
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

#[cfg(feature = "semantic-announce-hil")]
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

#[cfg(not(feature = "semantic-announce-hil"))]
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
