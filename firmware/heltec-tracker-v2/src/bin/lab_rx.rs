// Shared implementation for the explicitly configured Phase 1 receive-only
// lab images. The normal and hazardous backpressure binaries include this
// file through separate crate roots that enforce their feature contracts.
//
// The implementation validates the complete profile, claims every external
// RF-path safety pin and enters continuous receive only through the opaque
// RX-only wrapper. It never owns a generic LoRa handle or a transmit API.

use core::{
    cell::RefCell,
    future::Future,
    num::NonZeroU64,
    sync::atomic::{AtomicBool, Ordering},
};

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex},
    channel::Channel,
};
use embassy_time::{Delay, Duration, Instant, Timer};
use embedded_hal::digital::{Error as DigitalError, ErrorKind as DigitalErrorKind, ErrorType};
use embedded_hal_async::digital::Wait;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::{
    Async,
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig},
    peripherals::TIMG0,
    rng::{Trng, TrngSource},
    rtc_cntl::SocResetReason,
    spi::{
        Mode as SpiMode,
        master::{Config as SpiConfig, Spi},
    },
    time::{Duration as HalDuration, Rate},
    timer::timg::{MwdtStage, TimerGroup, Wdt},
};
use log::{error, info};
use reticulum_board_heltec_tracker_v2::{
    self as board, RxEventTimestampCapture, TrackerRxInterlock, TrackerRxRadio,
    TrackerRxRadioError,
};
#[cfg(feature = "lab-rx-returned-fault-hil")]
use reticulum_lab_rx_returned_fault_hil::{ReturnedFaultEvidence, ReturnedFaultSpiDevice};
use reticulum_radio_interface::{
    HealthyLeaseCommit, LabRxProfile, LabRxProfileConfig, RESET_STORM_QUARANTINE_THRESHOLD,
    RadioRxDiagnostics, RadioRxFaultPhase, RawFrameHandoff, RawFrameHandoffDiagnostics,
    RawFrameHandoffOutcome, RawReceivedFrame, ResetFaultHistory, ResetQuarantineDecision,
    RetainedBootReason, SX1262_FRAME_MTU, complete_healthy_radio_lease,
    prepare_reset_quarantine_boot, record_radio_fault_before_reset,
};
#[cfg(feature = "lab-rx-backpressure")]
use reticulum_radio_interface::{
    LabRxBackpressureHook, LabRxBackpressureHookState, LabRxBackpressureStall,
    LabRxBackpressureValidationError,
};
use reticulum_rns_rete_rx::{
    RETE_MAINTENANCE_INTERVAL_SECONDS, ReceiveOnlyClockSample, ReceiveOnlyIngressMetrics,
    ReceiveOnlyIngressOutcome, ReceiveOnlyInterfaceId, ReceiveOnlyRete, ReceiveOnlyWake,
    receive_only_identity_from_private_key,
};
use static_cell::StaticCell;
use zeroize::{Zeroize, Zeroizing};

mod lab_rx_stack_watermark;
mod rtc_fast_reset_journal;

use lab_rx_stack_watermark::StackWatermarkMonitor;
use rtc_fast_reset_journal::RtcFastResetJournal;

#[cfg(not(feature = "lab-rx"))]
compile_error!("the receive-only lab binary requires the lab-rx feature");

#[cfg(feature = "safe-idle")]
compile_error!(
    "safe-idle and lab-rx are mutually exclusive; use --no-default-features --features lab-rx"
);

const PROFILE_CONFIG: LabRxProfileConfig = LabRxProfileConfig {
    frequency_hz: Some(parse_u32(env!("RETICULUM_LAB_RX_FREQUENCY_HZ"))),
    spreading_factor: parse_u8(env!("RETICULUM_LAB_RX_SPREADING_FACTOR")),
    bandwidth_hz: parse_u32(env!("RETICULUM_LAB_RX_BANDWIDTH_HZ")),
    coding_rate_denominator: parse_u8(env!("RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR")),
    preamble_symbols: parse_u16(env!("RETICULUM_LAB_RX_PREAMBLE_SYMBOLS")),
    explicit_header: parse_bool(env!("RETICULUM_LAB_RX_EXPLICIT_HEADER")),
    crc: parse_bool(env!("RETICULUM_LAB_RX_CRC")),
    iq_inverted: parse_bool(env!("RETICULUM_LAB_RX_IQ_INVERTED")),
};

const PROFILE: LabRxProfile = match board::validate_lab_rx_profile(PROFILE_CONFIG) {
    Ok(profile) => profile,
    Err(_) => panic!("invalid explicit Tracker V2 lab RX profile"),
};

#[cfg(reticulum_lab_rx_regulator = "ldo")]
const LAB_RX_REGULATOR: board::TrackerRxRegulator = board::TrackerRxRegulator::Ldo;
#[cfg(reticulum_lab_rx_regulator = "dcdc")]
const LAB_RX_REGULATOR: board::TrackerRxRegulator = board::TrackerRxRegulator::DcDc;
#[cfg(reticulum_lab_rx_gain = "unboosted")]
const LAB_RX_GAIN: board::TrackerRxGain = board::TrackerRxGain::Unboosted;
#[cfg(reticulum_lab_rx_gain = "boosted")]
const LAB_RX_GAIN: board::TrackerRxGain = board::TrackerRxGain::Boosted;

#[cfg(feature = "lab-rx-electrical-hil")]
const LAB_RX_CONFIGURATION: board::TrackerRxConfiguration =
    board::TrackerRxConfiguration::new(LAB_RX_REGULATOR, LAB_RX_GAIN);
#[cfg(not(feature = "lab-rx-electrical-hil"))]
const LAB_RX_CONFIGURATION: board::TrackerRxConfiguration = board::TRACKER_RX_CONFIGURATION;

const LORA_INTERFACE_ID: ReceiveOnlyInterfaceId = ReceiveOnlyInterfaceId(1);
const RNS_PATH_CAPACITY: usize = 16;
const RNS_ANNOUNCE_CAPACITY: usize = 4;
const RNS_DEDUPLICATION_CAPACITY: usize = 32;
const RNS_LINK_CAPACITY: usize = 2;
const SUPERVISOR_WATCHDOG_SECONDS: u64 = 30;
#[cfg(feature = "lab-rx-backpressure")]
const SUPERVISOR_WATCHDOG_US: u64 = SUPERVISOR_WATCHDOG_SECONDS * 1_000_000;
const HEARTBEAT_INTERVAL_SECONDS: u64 = 60;
const TRACKER_SPI_FREQUENCY_HZ: u32 = 1_000_000;
const TRACKER_BUSY_TIMEOUT_MS: u64 = 100;
const HEALTHY_RADIO_LEASE_SECONDS: u64 = 10 * 60;

#[cfg(feature = "lab-rx-backpressure")]
const LAB_RX_BACKPRESSURE_POLICY: LabRxBackpressureStall = match LabRxBackpressureStall::validate(
    parse_u64(env!("RETICULUM_LAB_RX_BACKPRESSURE_STALL_US")),
    PROFILE.fragment_timeout_us(),
    SUPERVISOR_WATCHDOG_US,
) {
    Ok(policy) => policy,
    Err(LabRxBackpressureValidationError::StallDoesNotPassFragmentDeadline { .. }) => {
        panic!(
            "RETICULUM_LAB_RX_BACKPRESSURE_STALL_US must strictly exceed the configured fragment timeout"
        )
    }
    Err(LabRxBackpressureValidationError::StallExceedsWatchdogBudget { .. }) => {
        panic!(
            "RETICULUM_LAB_RX_BACKPRESSURE_STALL_US exceeds the supervisor watchdog service budget"
        )
    }
    Err(LabRxBackpressureValidationError::WatchdogCannotReserveServiceMargin { .. }) => {
        panic!("supervisor watchdog cannot reserve the lab backpressure service margin")
    }
};

#[cfg(feature = "lab-rx-backpressure")]
const LAB_RX_BACKPRESSURE_STALL: Duration =
    match Duration::try_from_micros(LAB_RX_BACKPRESSURE_POLICY.duration_us()) {
        Some(duration) => duration,
        None => panic!("lab RX backpressure duration exceeds the monotonic clock range"),
    };

#[cfg(feature = "lab-rx-backpressure")]
const LAB_RX_ARTIFACT_MODE: &str = "lab-rx-backpressure-hil";
#[cfg(all(
    not(feature = "lab-rx-backpressure"),
    feature = "lab-rx-electrical-hil"
))]
const LAB_RX_ARTIFACT_MODE: &str = concat!(
    "lab-rx-electrical-hil;regulator=",
    env!("RETICULUM_LAB_RX_REGULATOR"),
    ";rx_gain=",
    env!("RETICULUM_LAB_RX_GAIN")
);
#[cfg(all(
    not(feature = "lab-rx-backpressure"),
    not(feature = "lab-rx-electrical-hil"),
    feature = "lab-rx-returned-fault-hil"
))]
const LAB_RX_ARTIFACT_MODE: &str = concat!(
    "lab-rx-returned-fault-hil;trigger=",
    env!("RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER"),
    ";policy=",
    env!("RETICULUM_LAB_RX_RETURNED_FAULT_POLICY")
);
#[cfg(all(
    not(feature = "lab-rx-backpressure"),
    not(feature = "lab-rx-electrical-hil"),
    not(feature = "lab-rx-returned-fault-hil")
))]
const LAB_RX_ARTIFACT_MODE: &str = "lab-rx";

static RESET_JOURNAL_LOCK: Mutex<CriticalSectionRawMutex, RefCell<RtcFastResetJournal>> =
    Mutex::new(RefCell::new(RtcFastResetJournal::new()));

type TrackerLabIngress = ReceiveOnlyRete<
    RNS_PATH_CAPACITY,
    RNS_ANNOUNCE_CAPACITY,
    RNS_DEDUPLICATION_CAPACITY,
    RNS_LINK_CAPACITY,
>;

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

/// Raw receive pressure is intentionally bounded. The sole radio owner never
/// waits for this queue: when both slots are occupied, it records and drops the
/// newly received frame before immediately returning to continuous receive.
static RAW_FRAMES: Channel<CriticalSectionRawMutex, RawReceivedFrame, 2> = Channel::new();
static INGRESS: StaticCell<TrackerLabIngress> = StaticCell::new();
static RX_EVENT_TIMESTAMPS: RxEventTimestampCapture =
    RxEventTimestampCapture::new(rx_event_timestamp_ticks);

fn rx_event_timestamp_ticks() -> u64 {
    Instant::now().as_ticks()
}

#[derive(Clone, Copy, Debug)]
struct TrackerRadioStatus {
    radio: RadioRxDiagnostics<TrackerRxRadioError>,
    handoff: RawFrameHandoffDiagnostics,
}

impl TrackerRadioStatus {
    const fn new() -> Self {
        Self {
            radio: RadioRxDiagnostics::new(),
            handoff: RawFrameHandoffDiagnostics {
                offered: 0,
                queued: 0,
                dropped_new: 0,
            },
        }
    }
}

static RADIO_STATUS: Mutex<CriticalSectionRawMutex, RefCell<TrackerRadioStatus>> =
    Mutex::new(RefCell::new(TrackerRadioStatus::new()));

type SupervisorWatchdog = Wdt<TIMG0<'static>>;

static SUPERVISOR_WATCHDOG: Mutex<
    CriticalSectionRawMutex,
    RefCell<Option<SupervisorWatchdog>>,
> = Mutex::new(RefCell::new(None));
static JOURNAL_FAILURE_QUARANTINE: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "lab-rx-returned-fault-hil")]
#[unsafe(no_mangle)]
#[used]
static RETICULUM_RETURNED_FAULT_EVIDENCE: ReturnedFaultEvidence = ReturnedFaultEvidence::new();

#[derive(Clone, Copy, Debug)]
struct LabRxBackpressureEvidence {
    enabled: bool,
    configured_stall_us: u64,
    maximum_stall_us: u64,
    trigger_name: &'static str,
    timer_name: &'static str,
    triggered: bool,
    completed: bool,
    state: &'static str,
    expiry_observed: bool,
    offered_during_stall: u64,
    queued_during_stall: u64,
    dropped_during_stall: u64,
}

impl LabRxBackpressureEvidence {
    #[cfg(not(feature = "lab-rx-backpressure"))]
    const fn disabled() -> Self {
        Self {
            enabled: false,
            configured_stall_us: 0,
            maximum_stall_us: 0,
            trigger_name: "unavailable",
            timer_name: "unavailable",
            triggered: false,
            completed: false,
            state: "unavailable",
            expiry_observed: false,
            offered_during_stall: 0,
            queued_during_stall: 0,
            dropped_during_stall: 0,
        }
    }

    #[cfg(feature = "lab-rx-backpressure")]
    const fn armed() -> Self {
        Self {
            enabled: true,
            configured_stall_us: LAB_RX_BACKPRESSURE_POLICY.duration_us(),
            maximum_stall_us: LAB_RX_BACKPRESSURE_POLICY.maximum_duration_us(),
            trigger_name: "first_awaiting_continuation",
            timer_name: "embassy_async",
            triggered: false,
            completed: false,
            state: "armed",
            expiry_observed: false,
            offered_during_stall: 0,
            queued_during_stall: 0,
            dropped_during_stall: 0,
        }
    }

    #[cfg(feature = "lab-rx-backpressure")]
    fn synchronize_hook_state(&mut self, hook: LabRxBackpressureHook) {
        self.triggered = hook.triggered();
        self.completed = hook.completed();
        self.state = match hook.state() {
            LabRxBackpressureHookState::Armed => "armed",
            LabRxBackpressureHookState::Stalling => "stalling",
            LabRxBackpressureHookState::Completed => "completed",
        };
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
    match embassy_time::with_timeout(Duration::from_millis(TRACKER_BUSY_TIMEOUT_MS), future).await {
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

type TrackerSpiDevice = ExclusiveDevice<Spi<'static, Async>, Output<'static>, Delay>;
#[cfg(feature = "lab-rx-returned-fault-hil")]
type TrackerLabSpiDevice = ReturnedFaultSpiDevice<'static, TrackerSpiDevice>;
#[cfg(not(feature = "lab-rx-returned-fault-hil"))]
type TrackerLabSpiDevice = TrackerSpiDevice;
type TrackerLabRadio = TrackerRxRadio<
    TrackerLabSpiDevice,
    Output<'static>,
    Input<'static>,
    BoundedBusy<Input<'static>>,
    Delay,
    Output<'static>,
    Output<'static>,
    Output<'static>,
>;

fn update_radio_status(update: impl FnOnce(&mut TrackerRadioStatus)) {
    RADIO_STATUS.lock(|status| update(&mut status.borrow_mut()));
}

fn radio_status() -> TrackerRadioStatus {
    RADIO_STATUS.lock(|status| *status.borrow())
}

fn install_supervisor_watchdog(watchdog: SupervisorWatchdog) {
    SUPERVISOR_WATCHDOG.lock(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none());
        *slot = Some(watchdog);
    });
}

fn with_supervisor_watchdog(operation: impl FnOnce(&mut SupervisorWatchdog)) {
    SUPERVISOR_WATCHDOG.lock(|slot| {
        let mut slot = slot.borrow_mut();
        let watchdog = slot
            .as_mut()
            .expect("supervisor watchdog must be installed before radio construction");
        operation(watchdog);
    });
}

fn feed_supervisor_watchdog() {
    with_supervisor_watchdog(Wdt::feed);
}

fn disable_supervisor_watchdog() {
    with_supervisor_watchdog(Wdt::disable);
}

fn with_reset_journal<Result>(
    operation: impl FnOnce(&mut RtcFastResetJournal) -> Result,
) -> Result {
    RESET_JOURNAL_LOCK.lock(|journal| operation(&mut journal.borrow_mut()))
}

fn retained_boot_reason(reset_reason: Option<SocResetReason>) -> RetainedBootReason {
    match reset_reason {
        Some(SocResetReason::ChipPowerOn) => RetainedBootReason::ChipPowerOn,
        Some(SocResetReason::CoreSw) => RetainedBootReason::CoreSoftwareReset,
        Some(SocResetReason::CoreMwdt0) => RetainedBootReason::SupervisorWatchdogReset,
        Some(_) | None => RetainedBootReason::OtherRetainedReset,
    }
}

fn quarantine_forever(
    reset_reason: Option<SocResetReason>,
    reason: reticulum_radio_interface::ResetQuarantineReason,
    history: Option<ResetFaultHistory>,
) -> ! {
    error!(
        "phase1 reset-storm quarantine: reset_reason={reset_reason:?} reason={reason:?} history={history:?} threshold={} radio_constructed=false spi_constructed=false supervisor_watchdog=off hal_init_watchdogs=disabled rf_state=reset_low_fem_low",
        RESET_STORM_QUARANTINE_THRESHOLD,
    );
    error!(
        "phase1 quarantine remains until startup classifies reset as ChipPowerOn; operator recovery is a verified cold power cycle; esp32s3_reset_reason_0x01_conflates_brownout_and_super_wdt=true"
    );
    loop {
        core::hint::spin_loop();
    }
}

fn reset_after_radio_fault(phase: RadioRxFaultPhase, fault: TrackerRxRadioError) -> ! {
    // This retained transaction is deliberately the first operation in the
    // explicit fault path. It increments the streak and commits the pending
    // marker before diagnostics, logging or the CoreSw primitive can reset us.
    let retained_fault = with_reset_journal(record_radio_fault_before_reset);
    #[cfg(feature = "lab-rx-returned-fault-hil")]
    {
        let evidence = RETICULUM_RETURNED_FAULT_EVIDENCE.snapshot();
        error!(
            "phase1 returned-fault HIL evidence: mode={} trigger={} policy={} phase={phase:?} operation={:?} primary={:?} cleanup={:?} state={:?} set_rx_forwarded={} get_irq_status_rejected_before_spi={} fired={}",
            LAB_RX_ARTIFACT_MODE,
            env!("RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER"),
            env!("RETICULUM_LAB_RX_RETURNED_FAULT_POLICY"),
            fault.operation,
            fault.primary,
            fault.cleanup,
            evidence.state(),
            evidence.set_rx_forwarded(),
            evidence.get_irq_status_rejected_before_spi(),
            evidence.fired(),
        );
    }
    update_radio_status(|status| {
        status
            .radio
            .record_fault(phase, fault.classification(), fault);
    });
    let history = match retained_fault {
        Ok(history) => history,
        Err(error) => {
            // A verification error does not prove that even the poison store
            // reached retained memory. Do not reset and assume the next boot
            // will reject the old journal. The failed radio owner is already
            // inert; disable the only armed reset source and latch both tasks
            // in this powered session instead.
            JOURNAL_FAILURE_QUARANTINE.store(true, Ordering::Release);
            disable_supervisor_watchdog();
            let status = radio_status();
            error!(
                "phase1 retained radio-fault commit failed closed: error={error:?} action=immediate_rf_inert_quarantine supervisor_watchdog=off software_reset=false"
            );
            error!(
                "phase1 radio fatal: phase={:?} operation={:?} primary={:?} cleanup={:?} faults={} init_faults={} receive_faults={} spi_faults={} irq_faults={} timeout_faults={} fem_faults={} reset_requests={} retained_streak=unknown retained_total=unknown retained_pending=unknown recovery=verified_cold_power_cycle esp32s3_reset_reason_0x01_ambiguity=known_limit",
                phase,
                fault.operation,
                fault.primary,
                fault.cleanup,
                status.radio.faults,
                status.radio.initialization_faults,
                status.radio.receive_faults,
                status.radio.fault_classes.spi,
                status.radio.fault_classes.interrupt,
                status.radio.fault_classes.timeout,
                status.radio.fault_classes.fem,
                status.radio.system_reset_requests,
            );
            loop {
                core::hint::spin_loop();
            }
        }
    };
    update_radio_status(|status| status.radio.record_system_reset_request());
    let status = radio_status();
    error!(
        "phase1 radio fatal: phase={:?} operation={:?} primary={:?} cleanup={:?} faults={} init_faults={} receive_faults={} spi_faults={} irq_faults={} timeout_faults={} fem_faults={} reset_requests={} retained_streak={} retained_total={} retained_pending={} recovery=digital_core_software_reset",
        phase,
        fault.operation,
        fault.primary,
        fault.cleanup,
        status.radio.faults,
        status.radio.initialization_faults,
        status.radio.receive_faults,
        status.radio.fault_classes.spi,
        status.radio.fault_classes.interrupt,
        status.radio.fault_classes.timeout,
        status.radio.fault_classes.fem,
        status.radio.system_reset_requests,
        history.consecutive_fault_resets(),
        history.total_fault_resets(),
        history.radio_fault_pending_reset(),
    );

    // The wrapper has already attempted coordinated FEM shutdown and reset-pin
    // assertion. This ROM primitive resets the digital core; the pin transient
    // through ROM and boot remains a powered HIL requirement.
    esp_hal::rom::software_reset()
}

fn log_phase1_heartbeat(
    now: Instant,
    identity_public_key: &[u8; 64],
    destination_hash: &[u8],
    metrics: ReceiveOnlyIngressMetrics,
    stack_watermark: &mut StackWatermarkMonitor,
    backpressure: LabRxBackpressureEvidence,
) {
    let status = radio_status();
    let heap = esp_alloc::HEAP.stats();
    let heap_free = heap.size.saturating_sub(heap.current_usage);
    let last_radio_signal = status.radio.last_signal;
    let last_rnode_signal = metrics.receive.last_packet_signal;
    let stack = stack_watermark.sample();

    info!(
        "phase1 heartbeat: ticks={} uptime_seconds={} public_key={} destination_hash={} heap_size={} heap_used={} heap_free={} heap_max_used={}",
        now.as_ticks(),
        now.as_secs(),
        HexBytes(identity_public_key),
        HexBytes(destination_hash),
        heap.size,
        heap.current_usage,
        heap_free,
        heap.max_usage,
    );
    info!(
        "phase1 heartbeat stack: reserved_bytes={} usable_above_guard_bytes={} startup_painted_bytes={} high_water_used_bytes={} minimum_remaining_above_guard_bytes={} guard_offset_bytes={} guard_intact={} scan_valid={}",
        stack.stack_reserved_bytes,
        stack.stack_usable_above_guard_bytes,
        stack.startup_painted_bytes,
        stack.high_water_used_bytes,
        stack.minimum_remaining_above_guard_bytes,
        stack.stack_guard_offset_bytes,
        stack.stack_guard_intact,
        stack.scan_valid,
    );
    info!(
        "phase1 heartbeat backpressure: artifact_mode={} enabled={} configured_stall_us={} maximum_stall_us={} triggered={} completed={} state={} expiry_observed={} offered_during_stall={} queued_during_stall={} dropped_during_stall={}",
        LAB_RX_ARTIFACT_MODE,
        backpressure.enabled,
        backpressure.configured_stall_us,
        backpressure.maximum_stall_us,
        backpressure.triggered,
        backpressure.completed,
        backpressure.state,
        backpressure.expiry_observed,
        backpressure.offered_during_stall,
        backpressure.queued_during_stall,
        backpressure.dropped_during_stall,
    );
    info!(
        "phase1 heartbeat radio: frames={} bytes={} last_len={} last_received_at_ticks={:?} last_rssi_dbm={:?} last_snr_db={:?} offered={} queued={} channel_full_drops={} init_attempts={} init_successes={} reinit_attempts={} faults={} spi_faults={} irq_faults={} timeout_faults={} fem_faults={} reset_requests={}",
        status.radio.frames_received,
        status.radio.bytes_received,
        status.radio.last_frame_len,
        status.radio.last_received_at_ticks,
        last_radio_signal.map(|signal| signal.rssi_dbm),
        last_radio_signal.map(|signal| signal.snr_db),
        status.handoff.offered,
        status.handoff.queued,
        status.handoff.dropped_new,
        status.radio.initialization_attempts,
        status.radio.successful_initializations,
        status.radio.reinitialization_attempts,
        status.radio.faults,
        status.radio.fault_classes.spi,
        status.radio.fault_classes.interrupt,
        status.radio.fault_classes.timeout,
        status.radio.fault_classes.fem,
        status.radio.system_reset_requests,
    );
    info!(
        "phase1 heartbeat ingress: handed_off={} raw_dropped={} stale={} future={} out_of_order={} deadline_collisions={} rnode_frames={} rnode_framing_errors={} pending_started={} pending_replaced={} pending_expired={} pending_discarded={} packets_completed={} packets_accepted={} packets_too_long={} last_frame_len={} last_packet_len={} last_raw_packet_sha256={} last_packet_rssi_dbm={:?} last_packet_snr_db={:?} rete_calls={} rete_seen={} rete_admitted={} rete_rejected={} rete_duplicate={} rete_invalid={} rete_no_outcome={} rete_ticks={} suppressed_events={} suppressed_packets={} suppressed_unroutable={} last_latency_ticks={} max_latency_ticks={}",
        metrics.frames_handed_off,
        metrics.raw_frames_dropped,
        metrics.stale_raw_frames,
        metrics.future_receive_timestamps,
        metrics.out_of_order_receive_timestamps,
        metrics.pending_deadline_collisions,
        metrics.receive.frames_seen,
        metrics.receive.framing_errors,
        metrics.receive.pending_started,
        metrics.receive.pending_replaced,
        metrics.receive.pending_expired,
        metrics.receive.pending_discarded,
        metrics.receive.packets_completed,
        metrics.receive.packets_accepted,
        metrics.receive.packets_too_long,
        metrics.receive.last_frame_len,
        metrics.receive.last_packet_len,
        OptionalHexBytes(
            metrics
                .last_raw_packet_sha256
                .as_ref()
                .map(|digest| digest.as_slice()),
        ),
        last_rnode_signal.map(|signal| signal.rssi_dbm),
        last_rnode_signal.map(|signal| signal.snr_db),
        metrics.rete_ingress_calls,
        metrics.node.ingress.seen,
        metrics.node.ingress.admitted,
        metrics.node.ingress.rejected,
        metrics.node.ingress.native_duplicate,
        metrics.node.ingress.native_invalid,
        metrics.node.ingress.native_no_outcome,
        metrics.rete_tick_calls,
        metrics.suppressed.events,
        metrics.suppressed.packets,
        metrics.suppressed.unroutable_packets,
        metrics.last_handoff_latency_ticks,
        metrics.maximum_handoff_latency_ticks,
    );
}

esp_bootloader_esp_idf::esp_app_desc!();

/// Sole owner of the active SX1262 and external FEM.
///
/// The complete `receive()` future is awaited directly and is never raced in
/// `select` or wrapped in a timeout. Runtime coordination occurs only after a
/// physical frame has completed, through a synchronous non-blocking enqueue.
#[embassy_executor::task]
async fn radio_rx_task(mut radio: TrackerLabRadio) {
    let mut frame_buffer = [0_u8; SX1262_FRAME_MTU];
    let mut handoff = RawFrameHandoff::new();

    loop {
        let received = match radio.receive(&mut frame_buffer).await {
            Ok(received) => received,
            Err(fault) => reset_after_radio_fault(RadioRxFaultPhase::Receive, fault),
        };
        let received_at_ticks = received.received_at_ticks;
        update_radio_status(|status| {
            status
                .radio
                .record_frame(received.len, received.signal, received_at_ticks);
        });
        let frame = RawReceivedFrame::new(
            frame_buffer,
            received.len,
            received.signal,
            received_at_ticks,
        );
        let outcome = handoff.offer(frame, |frame| RAW_FRAMES.try_send(frame).map_err(|_| ()));
        frame_buffer = [0_u8; SX1262_FRAME_MTU];

        let diagnostics = handoff.diagnostics();
        update_radio_status(|status| status.handoff = diagnostics);
        let status = radio_status();
        if status.radio.frames_received == 1 || status.radio.frames_received.is_multiple_of(64) {
            let signal = status.radio.last_signal;
            info!(
                "phase1 radio summary: ticks={} frames={} bytes={} last_len={} last_received_at_ticks={:?} last_rssi_dbm={:?} last_snr_db={:?} queued={} channel_full_drops={} init_attempts={} init_successes={} reinit_attempts={} faults={}",
                Instant::now().as_ticks(),
                status.radio.frames_received,
                status.radio.bytes_received,
                status.radio.last_frame_len,
                status.radio.last_received_at_ticks,
                signal.map(|value| value.rssi_dbm),
                signal.map(|value| value.snr_db),
                status.handoff.queued,
                status.handoff.dropped_new,
                status.radio.initialization_attempts,
                status.radio.successful_initializations,
                status.radio.reinitialization_attempts,
                status.radio.faults,
            );
        }
        if outcome == RawFrameHandoffOutcome::DroppedNew
            && (diagnostics.dropped_new == 1 || diagnostics.dropped_new.is_multiple_of(64))
        {
            info!(
                "phase1 radio handoff pressure: ticks={} offered={} queued={} channel_full_drops={}",
                Instant::now().as_ticks(),
                diagnostics.offered,
                diagnostics.queued,
                diagnostics.dropped_new,
            );
        }
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "the firmware entry point initializes target-owned resources"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let boot_reset_reason = esp_hal::system::reset_reason();

    // Establish the same electrical interlock as safe-idle before starting the
    // executor or heap. SX1262 DIO2 is the direct and sole KCT8103L CPS driver;
    // separately exposed header GPIO46 is not part of the RF path.
    let sx1262_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let fem_power = Output::new(peripherals.GPIO7, Level::Low, OutputConfig::default());
    let fem_csd = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
    let fem_ctx = Output::new(peripherals.GPIO5, Level::Low, OutputConfig::default());
    let sx1262_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    let interlock = match TrackerRxInterlock::new(fem_power, fem_csd, fem_ctx) {
        Ok(interlock) => interlock,
        Err(_) => panic!("could not establish Tracker RX interlock"),
    };

    // Keep the shared display/GNSS rail and battery-divider load disabled in
    // both normal startup and the reset-storm quarantine path.
    let _vext = Output::new(peripherals.GPIO3, Level::Low, OutputConfig::default());
    let _battery_divider = Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default());

    // Retained authorization is evaluated only after every RF control has its
    // inert electrical owner, but before SPI, radio, executor timer or the
    // supervisor watchdog is constructed. Quarantine never leaves this branch.
    let boot_fault_history = match with_reset_journal(|journal| {
        prepare_reset_quarantine_boot(journal, retained_boot_reason(boot_reset_reason))
    }) {
        ResetQuarantineDecision::Run(history) => history,
        ResetQuarantineDecision::Quarantine { reason, history } => {
            quarantine_forever(boot_reset_reason, reason, history)
        }
    };

    #[cfg(all(
        feature = "lab-rx-returned-fault-hil",
        reticulum_lab_rx_returned_fault_policy = "one-boot"
    ))]
    {
        let pristine = boot_fault_history.generation() == 1
            && boot_fault_history.consecutive_fault_resets() == 0
            && boot_fault_history.total_fault_resets() == 0
            && !boot_fault_history.radio_fault_pending_reset();
        if boot_reset_reason == Some(SocResetReason::ChipPowerOn) && pristine {
            info!(
                "phase1 returned-fault HIL artifact: mode={} trigger={} policy=one-boot boot_armed=true state=not_constructed radio_constructed=false spi_constructed=false supervisor_watchdog=off rf_state=reset_low_fem_low",
                LAB_RX_ARTIFACT_MODE,
                env!("RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER"),
            );
        } else if boot_reset_reason == Some(SocResetReason::CoreSw)
            && boot_fault_history.consecutive_fault_resets() == 1
            && boot_fault_history.total_fault_resets() == 1
            && !boot_fault_history.radio_fault_pending_reset()
        {
            info!(
                "phase1 returned-fault HIL one-boot complete: mode={} trigger={} policy=one-boot boot_armed=false retained_streak=1 retained_total=1 radio_constructed=false spi_constructed=false supervisor_watchdog=off rf_state=reset_low_fem_low",
                LAB_RX_ARTIFACT_MODE,
                env!("RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER"),
            );
            loop {
                core::hint::spin_loop();
            }
        } else {
            error!(
                "phase1 returned-fault HIL unexpected boot: mode={} trigger={} policy=one-boot reset_reason={boot_reset_reason:?} generation={} retained_streak={} retained_total={} retained_pending={} action=rf_inert_hold radio_constructed=false spi_constructed=false supervisor_watchdog=off rf_state=reset_low_fem_low",
                LAB_RX_ARTIFACT_MODE,
                env!("RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER"),
                boot_fault_history.generation(),
                boot_fault_history.consecutive_fault_resets(),
                boot_fault_history.total_fault_resets(),
                boot_fault_history.radio_fault_pending_reset(),
            );
            loop {
                core::hint::spin_loop();
            }
        }
    }

    #[cfg(all(
        feature = "lab-rx-returned-fault-hil",
        reticulum_lab_rx_returned_fault_policy = "repeat-until-quarantine"
    ))]
    info!(
        "phase1 returned-fault HIL artifact: mode={} trigger={} policy=repeat-until-quarantine boot_armed=true generation={} retained_streak={} retained_total={} state=not_constructed radio_constructed=false spi_constructed=false supervisor_watchdog=off rf_state=reset_low_fem_low",
        LAB_RX_ARTIFACT_MODE,
        env!("RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER"),
        boot_fault_history.generation(),
        boot_fault_history.consecutive_fault_resets(),
        boot_fault_history.total_fault_resets(),
    );

    let mut stack_watermark = match StackWatermarkMonitor::initialize() {
        Ok(monitor) => monitor,
        Err(error) => panic!("lab stack watermark startup qualification failed: {error:?}"),
    };
    info!(
        "phase1 boot: reset_reason={boot_reset_reason:?} reset_journal_generation={} retained_fault_streak={} retained_total_faults={} retained_pending={} reset_storm_threshold={}",
        boot_fault_history.generation(),
        boot_fault_history.consecutive_fault_resets(),
        boot_fault_history.total_fault_resets(),
        boot_fault_history.radio_fault_pending_reset(),
        RESET_STORM_QUARANTINE_THRESHOLD,
    );

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);

    let timers = TimerGroup::new(peripherals.TIMG0);
    let mut supervisor_watchdog = timers.wdt;
    supervisor_watchdog.set_timeout(
        MwdtStage::Stage0,
        HalDuration::from_secs(SUPERVISOR_WATCHDOG_SECONDS),
    );
    supervisor_watchdog.enable();
    supervisor_watchdog.feed();
    install_supervisor_watchdog(supervisor_watchdog);
    let software_interrupts =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);

    // Build every fallible protocol owner while the RF interlock still holds
    // reset, FEM power, CSD and CTX low. The panic handler does not unwind, so
    // constructing these after RX activation would strand energized hardware
    // if entropy, allocation or Rete initialization failed.
    //
    // Phase 1 deliberately uses an ephemeral identity. The ESP32-S3 TRNG
    // source reserves ADC1 for the remainder of this image and must outlive
    // every Trng handle. This is not the production identity, persistence or
    // entropy-health design.
    let _entropy_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    let mut crypto_rng = match Trng::try_new() {
        Ok(rng) => rng,
        Err(_) => panic!("ADC-backed ESP32-S3 TRNG did not become available"),
    };
    let identity = {
        let mut private_key = Zeroizing::new([0_u8; 64]);
        crypto_rng.read(&mut private_key[..]);
        let identity = receive_only_identity_from_private_key(&private_key[..]);
        private_key.zeroize();
        match identity {
            Ok(identity) => identity,
            Err(_) => panic!("generated Reticulum identity key was rejected"),
        }
    };
    let identity_public_key = identity.public_key();

    let fragment_timeout = match Duration::try_from_micros(PROFILE.fragment_timeout_us()) {
        Some(duration) => duration,
        None => panic!("lab RX fragment timeout exceeds the monotonic clock range"),
    };
    let fragment_timeout_ticks = match NonZeroU64::new(fragment_timeout.as_ticks()) {
        Some(ticks) => ticks,
        None => panic!("lab RX fragment timeout rounded to zero ticks"),
    };
    let maintenance_interval = match Duration::try_from_secs(RETE_MAINTENANCE_INTERVAL_SECONDS) {
        Some(duration) => duration,
        None => panic!("Rete maintenance interval exceeds the monotonic clock range"),
    };
    let maintenance_interval_ticks = match NonZeroU64::new(maintenance_interval.as_ticks()) {
        Some(ticks) => ticks,
        None => panic!("Rete maintenance interval rounded to zero ticks"),
    };
    let initial_now = Instant::now();
    let ingress = INGRESS.init_with(move || {
        match TrackerLabIngress::new(
            identity,
            "reticulum-rs-firmware",
            &["heltec-tracker-v2", "lab-rx"],
            fragment_timeout_ticks,
            initial_now.as_ticks(),
            maintenance_interval_ticks,
            LORA_INTERFACE_ID,
        ) {
            Ok(ingress) => ingress,
            Err(_) => panic!("could not construct fail-closed Rete ingress"),
        }
    });
    let destination_hash = ingress.destination_hash();
    info!(
        "phase1 lab identity: public_key={} destination_hash={} destination_name=reticulum-rs-firmware.heltec-tracker-v2.lab-rx",
        HexBytes(&identity_public_key),
        HexBytes(destination_hash.as_ref()),
    );

    let spi = match Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(TRACKER_SPI_FREQUENCY_HZ))
            .with_mode(SpiMode::_0),
    ) {
        Ok(spi) => spi,
        Err(_) => panic!("could not configure Tracker SPI2"),
    }
    .with_sck(peripherals.GPIO9)
    .with_mosi(peripherals.GPIO10)
    .with_miso(peripherals.GPIO11)
    .into_async();
    let spi_device = match embedded_hal_bus::spi::ExclusiveDevice::new(spi, sx1262_nss, Delay) {
        Ok(device) => device,
        Err(_) => panic!("could not establish SX1262 SPI device"),
    };
    #[cfg(feature = "lab-rx-returned-fault-hil")]
    let spi_device =
        ReturnedFaultSpiDevice::new(spi_device, &RETICULUM_RETURNED_FAULT_EVIDENCE);
    // No internal pull is selected. A pull-down can make a disconnected BUSY
    // line look falsely ready; the actual board inputs are qualified in HIL.
    let dio1 = Input::new(peripherals.GPIO14, InputConfig::default());
    let busy = BoundedBusy::new(Input::new(peripherals.GPIO13, InputConfig::default()));
    let mut fem_delay = Delay;
    update_radio_status(|status| status.radio.record_initialization_attempt(false));
    feed_supervisor_watchdog();
    let radio = match TrackerRxRadio::new(
        spi_device,
        sx1262_reset,
        dio1,
        busy,
        Delay,
        &mut fem_delay,
        interlock,
        &RX_EVENT_TIMESTAMPS,
        PROFILE,
        LAB_RX_CONFIGURATION,
    )
    .await
    {
        Ok(radio) => {
            update_radio_status(|status| status.radio.record_initialized());
            radio
        }
        Err(fault) => reset_after_radio_fault(RadioRxFaultPhase::Initialization, fault),
    };
    feed_supervisor_watchdog();
    let healthy_lease_duration = match Duration::try_from_secs(HEALTHY_RADIO_LEASE_SECONDS) {
        Some(duration) => duration,
        None => panic!("healthy radio lease exceeds the monotonic clock range"),
    };
    let healthy_lease_deadline_ticks = Instant::now()
        .as_ticks()
        .saturating_add(healthy_lease_duration.as_ticks());
    let mut healthy_lease_pending = true;

    // The actual stall owner and trigger path do not exist in the normal lab
    // feature. Enabling them requires the separately named compile-time HIL
    // feature and its explicit, build-validated duration environment value.
    #[cfg(feature = "lab-rx-backpressure")]
    let mut backpressure_hook = LabRxBackpressureHook::new();
    #[cfg(feature = "lab-rx-backpressure")]
    let mut backpressure_evidence = LabRxBackpressureEvidence::armed();
    #[cfg(not(feature = "lab-rx-backpressure"))]
    let backpressure_evidence = LabRxBackpressureEvidence::disabled();

    info!(
        "phase1 artifact identity: mode={} backpressure_hook_enabled={} configured_stall_us={} maximum_stall_us={} trigger={} one_shot={} timer={}",
        LAB_RX_ARTIFACT_MODE,
        backpressure_evidence.enabled,
        backpressure_evidence.configured_stall_us,
        backpressure_evidence.maximum_stall_us,
        backpressure_evidence.trigger_name,
        backpressure_evidence.enabled,
        backpressure_evidence.timer_name,
    );
    info!(
        "phase1 runtime source: esp_rtos_source={}",
        env!("RETICULUM_ESP_RTOS_UPSTREAM_IDENTITY"),
    );

    info!(
        "phase1 lab-rx active: board={} frequency_hz={} sf={:?} bandwidth={:?} cr={:?} preamble={} explicit_header={} crc={} iq_inverted={} artifact_mode={} backpressure_hook_enabled={} backpressure_stall_us={}",
        board::BOARD_REVISION,
        PROFILE.frequency_hz(),
        PROFILE.spreading_factor(),
        PROFILE.bandwidth(),
        PROFILE.coding_rate(),
        PROFILE.preamble_symbols(),
        PROFILE.explicit_header(),
        PROFILE.crc(),
        PROFILE.iq_inverted(),
        LAB_RX_ARTIFACT_MODE,
        backpressure_evidence.enabled,
        backpressure_evidence.configured_stall_us,
    );

    info!(
        "phase1 radio configuration: spi_hz={} spi_mode=0 sync_word=0x{:04x} public_network=false tcxo_mv={} dio2_rf_switch=true use_dcdc={} rx_boosted={} fem_power_settle_ms={} fem_csd_settle_ms={} busy_timeout_ms={} watchdog_seconds={} reset_storm_threshold={} healthy_lease_seconds={} healthy_lease_silence_counts=true recovery=digital_core_software_reset",
        TRACKER_SPI_FREQUENCY_HZ,
        board::TRACKER_RX_SYNC_WORD,
        board::TCXO_MILLIVOLTS,
        LAB_RX_CONFIGURATION.uses_dcdc(),
        LAB_RX_CONFIGURATION.boosted(),
        board::FEM_POWER_SETTLE_MS,
        board::FEM_CSD_SETTLE_MS,
        TRACKER_BUSY_TIMEOUT_MS,
        SUPERVISOR_WATCHDOG_SECONDS,
        RESET_STORM_QUARANTINE_THRESHOLD,
        HEALTHY_RADIO_LEASE_SECONDS,
    );

    info!(
        "phase1 receive-only RNS ingress active: maximum_frame_airtime_us={} fragment_timeout_us={} maintenance_seconds={} paths={} announces={} dedup={} links={} identity=ephemeral adc1=reserved",
        PROFILE.maximum_frame_time_on_air_us(),
        PROFILE.fragment_timeout_us(),
        RETE_MAINTENANCE_INTERVAL_SECONDS,
        RNS_PATH_CAPACITY,
        RNS_ANNOUNCE_CAPACITY,
        RNS_DEDUPLICATION_CAPACITY,
        RNS_LINK_CAPACITY,
    );

    let heartbeat_interval = match Duration::try_from_secs(HEARTBEAT_INTERVAL_SECONDS) {
        Some(interval) => interval,
        None => panic!("heartbeat interval exceeds the monotonic clock range"),
    };
    let mut next_heartbeat_ticks = Instant::now()
        .as_ticks()
        .saturating_add(heartbeat_interval.as_ticks());
    log_phase1_heartbeat(
        Instant::now(),
        &identity_public_key,
        destination_hash.as_ref(),
        ingress.metrics(),
        &mut stack_watermark,
        backpressure_evidence,
    );
    feed_supervisor_watchdog();

    let radio_task = match radio_rx_task(radio) {
        Ok(task) => task,
        Err(_) => panic!("could not allocate sole Tracker radio owner task"),
    };
    spawner.spawn(radio_task);

    // This entry task is the sole ingress owner. Its only async race is a raw
    // channel receive against the earlier absolute protocol/healthy-lease
    // deadline; the PHY receive future remains non-cancellable in the
    // independent radio owner above. An early lease timer is a harmless Rete
    // maintenance wake and cannot admit or transmit a packet on its own.
    let mut last_reported_suppressed_packets = 0_u64;
    loop {
        let next_timer_ticks = if healthy_lease_pending {
            ingress.next_wake_ticks().min(healthy_lease_deadline_ticks)
        } else {
            ingress.next_wake_ticks()
        };
        let selected = select(
            Timer::at(Instant::from_ticks(next_timer_ticks)),
            RAW_FRAMES.receive(),
        )
        .await;
        if JOURNAL_FAILURE_QUARANTINE.load(Ordering::Acquire) {
            error!(
                "phase1 main owner entered retained-journal failure quarantine: protocol_maintenance=false healthy_lease_clear=false supervisor_watchdog=off"
            );
            loop {
                core::hint::spin_loop();
            }
        }
        let wake_now = Instant::now();
        let clock = ReceiveOnlyClockSample {
            ticks: wake_now.as_ticks(),
            transport_seconds: wake_now.as_secs(),
        };
        let step = match selected {
            Either::First(()) => ingress.on_wake(ReceiveOnlyWake::Timer, clock, &mut crypto_rng),
            Either::Second(frame) => {
                ingress.on_wake(ReceiveOnlyWake::Frame(&frame), clock, &mut crypto_rng)
            }
        };

        #[cfg(feature = "lab-rx-backpressure")]
        if let Some(ReceiveOnlyIngressOutcome::AwaitingContinuation {
            sequence,
            data_len,
            deadline_ticks,
            ..
        }) = step.frame.as_ref()
            && backpressure_hook.observe_awaiting_continuation()
        {
            backpressure_evidence.synchronize_hook_state(backpressure_hook);
            let handoff_before = radio_status().handoff;
            info!(
                "phase1 backpressure HIL triggered: artifact_mode={} enabled=true configured_stall_us={} triggered={} completed={} state={} sequence={} data_len={} fragment_deadline_ticks={} watchdog_fed_before_async_stall=true",
                LAB_RX_ARTIFACT_MODE,
                backpressure_evidence.configured_stall_us,
                backpressure_evidence.triggered,
                backpressure_evidence.completed,
                backpressure_evidence.state,
                sequence,
                data_len,
                deadline_ticks,
            );

            // This must remain an async timer wait on the ingress owner. The
            // independent radio task can therefore continue receiving and its
            // depth-two drop-new handoff can fill while ingress is paused.
            // Feed immediately before the wait; build-time policy retains five
            // seconds after it for expiry service and the next supervisor feed.
            feed_supervisor_watchdog();
            Timer::after(LAB_RX_BACKPRESSURE_STALL).await;

            // Service the overdue absolute timer before touching the queued
            // frames. This establishes the existing expiry watermark, so
            // frames received at or before that deadline cannot revive or
            // splice with the fragment that triggered the stall.
            let resumed_at = Instant::now();
            let expiry_step = ingress.on_wake(
                ReceiveOnlyWake::Timer,
                ReceiveOnlyClockSample {
                    ticks: resumed_at.as_ticks(),
                    transport_seconds: resumed_at.as_secs(),
                },
                &mut crypto_rng,
            );
            backpressure_evidence.expiry_observed = expiry_step.expired_fragment.is_some();
            match backpressure_hook.complete() {
                Ok(()) => backpressure_evidence.synchronize_hook_state(backpressure_hook),
                Err(_) => panic!("lab RX backpressure hook entered an invalid completion state"),
            }
            let handoff_after = radio_status().handoff;
            backpressure_evidence.offered_during_stall =
                handoff_after.offered.saturating_sub(handoff_before.offered);
            backpressure_evidence.queued_during_stall =
                handoff_after.queued.saturating_sub(handoff_before.queued);
            backpressure_evidence.dropped_during_stall = handoff_after
                .dropped_new
                .saturating_sub(handoff_before.dropped_new);
            info!(
                "phase1 backpressure HIL completed: artifact_mode={} configured_stall_us={} triggered={} completed={} state={} expiry_observed={} resumed_at_ticks={} offered_during_stall={} queued_during_stall={} dropped_during_stall={} queued_frames_service=pending",
                LAB_RX_ARTIFACT_MODE,
                backpressure_evidence.configured_stall_us,
                backpressure_evidence.triggered,
                backpressure_evidence.completed,
                backpressure_evidence.state,
                backpressure_evidence.expiry_observed,
                resumed_at.as_ticks(),
                backpressure_evidence.offered_during_stall,
                backpressure_evidence.queued_during_stall,
                backpressure_evidence.dropped_during_stall,
            );
            if !backpressure_evidence.expiry_observed {
                error!(
                    "phase1 backpressure HIL qualification failure: pending fragment did not expire after build-validated stall"
                );
            }
        }

        let now = Instant::now();
        let metrics = ingress.metrics();
        let report_completed_packet = matches!(
            step.frame.as_ref(),
            Some(ReceiveOnlyIngressOutcome::Packet { .. })
        );

        if step.frame.is_some()
            && (metrics.frames_handed_off == 1
                || metrics.frames_handed_off.is_multiple_of(64)
                || report_completed_packet)
        {
            let status = radio_status();
            let last_frame_signal = metrics.receive.last_frame_signal;
            let last_packet_signal = metrics.receive.last_packet_signal;
            info!(
                "phase1 ingress summary: ticks={} frames={} channel_full_drops={} ingress_raw_dropped={} stale={} future={} out_of_order={} deadline_collisions={} rnode_frames={} rnode_errors={} pending_started={} pending_replaced={} pending_expired={} pending_discarded={} packets_completed={} rnode_accepted={} packets_too_long={} last_frame_len={} last_packet_len={} last_raw_packet_sha256={} last_frame_rssi_dbm={:?} last_frame_snr_db={:?} last_packet_rssi_dbm={:?} last_packet_snr_db={:?} rete_calls={} rete_seen={} rete_admitted={} rete_rejected={} rete_duplicate={} rete_invalid={} rete_no_outcome={} rete_ticks={} suppressed_events={} suppressed_packets={} suppressed_unroutable={} last_latency_ticks={} max_latency_ticks={}",
                now.as_ticks(),
                metrics.frames_handed_off,
                status.handoff.dropped_new,
                metrics.raw_frames_dropped,
                metrics.stale_raw_frames,
                metrics.future_receive_timestamps,
                metrics.out_of_order_receive_timestamps,
                metrics.pending_deadline_collisions,
                metrics.receive.frames_seen,
                metrics.receive.framing_errors,
                metrics.receive.pending_started,
                metrics.receive.pending_replaced,
                metrics.receive.pending_expired,
                metrics.receive.pending_discarded,
                metrics.receive.packets_completed,
                metrics.receive.packets_accepted,
                metrics.receive.packets_too_long,
                metrics.receive.last_frame_len,
                metrics.receive.last_packet_len,
                OptionalHexBytes(
                    metrics
                        .last_raw_packet_sha256
                        .as_ref()
                        .map(|digest| digest.as_slice()),
                ),
                last_frame_signal.map(|signal| signal.rssi_dbm),
                last_frame_signal.map(|signal| signal.snr_db),
                last_packet_signal.map(|signal| signal.rssi_dbm),
                last_packet_signal.map(|signal| signal.snr_db),
                metrics.rete_ingress_calls,
                metrics.node.ingress.seen,
                metrics.node.ingress.admitted,
                metrics.node.ingress.rejected,
                metrics.node.ingress.native_duplicate,
                metrics.node.ingress.native_invalid,
                metrics.node.ingress.native_no_outcome,
                metrics.rete_tick_calls,
                metrics.suppressed.events,
                metrics.suppressed.packets,
                metrics.suppressed.unroutable_packets,
                metrics.last_handoff_latency_ticks,
                metrics.maximum_handoff_latency_ticks,
            );
        }
        if metrics.suppressed.packets > last_reported_suppressed_packets
            && (last_reported_suppressed_packets == 0
                || metrics.suppressed.packets / 64 > last_reported_suppressed_packets / 64)
        {
            info!(
                "phase1 outbound actions suppressed: packets={} events={} unroutable={}",
                metrics.suppressed.packets,
                metrics.suppressed.events,
                metrics.suppressed.unroutable_packets,
            );
            last_reported_suppressed_packets = metrics.suppressed.packets;
        }
        if healthy_lease_pending && now.as_ticks() >= healthy_lease_deadline_ticks {
            match with_reset_journal(complete_healthy_radio_lease) {
                Ok(HealthyLeaseCommit::AlreadyClear(history)) => info!(
                    "phase1 healthy radio lease complete: seconds={} retained_fault_streak={} retained_total_faults={} journal_write=not_needed silence_counts=true",
                    HEALTHY_RADIO_LEASE_SECONDS,
                    history.consecutive_fault_resets(),
                    history.total_fault_resets(),
                ),
                Ok(HealthyLeaseCommit::Cleared(history)) => info!(
                    "phase1 healthy radio lease complete: seconds={} retained_fault_streak={} retained_total_faults={} journal_write=committed silence_counts=true",
                    HEALTHY_RADIO_LEASE_SECONDS,
                    history.consecutive_fault_resets(),
                    history.total_fault_resets(),
                ),
                Err(error) => error!(
                    "phase1 healthy radio lease journal update failed closed: seconds={} error={error:?} current_radio_continues=true next_retained_boot=quarantine",
                    HEALTHY_RADIO_LEASE_SECONDS,
                ),
            }
            healthy_lease_pending = false;
        }
        if now.as_ticks() >= next_heartbeat_ticks {
            log_phase1_heartbeat(
                now,
                &identity_public_key,
                destination_hash.as_ref(),
                metrics,
                &mut stack_watermark,
                backpressure_evidence,
            );
            next_heartbeat_ticks = now.as_ticks().saturating_add(heartbeat_interval.as_ticks());
        }

        // Feed only after a complete selected wake, protocol maintenance and
        // diagnostics pass. A blocked synchronous path therefore resets the
        // digital core instead of leaving an energized RX path indefinitely.
        feed_supervisor_watchdog();
    }
}

#[cfg(feature = "lab-rx-backpressure")]
const fn parse_u64(value: &str) -> u64 {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        panic!("empty unsigned integer");
    }

    let mut parsed = 0_u64;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii_digit() {
            panic!("invalid unsigned integer");
        }
        let digit = (byte - b'0') as u64;
        if parsed > (u64::MAX - digit) / 10 {
            panic!("unsigned integer overflow");
        }
        parsed = parsed * 10 + digit;
        index += 1;
    }
    parsed
}

const fn parse_u32(value: &str) -> u32 {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        panic!("empty unsigned integer");
    }

    let mut parsed = 0_u32;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii_digit() {
            panic!("invalid unsigned integer");
        }
        let digit = (byte - b'0') as u32;
        if parsed > (u32::MAX - digit) / 10 {
            panic!("unsigned integer overflow");
        }
        parsed = parsed * 10 + digit;
        index += 1;
    }
    parsed
}

const fn parse_u16(value: &str) -> u16 {
    let parsed = parse_u32(value);
    if parsed > u16::MAX as u32 {
        panic!("u16 overflow");
    }
    parsed as u16
}

const fn parse_u8(value: &str) -> u8 {
    let parsed = parse_u32(value);
    if parsed > u8::MAX as u32 {
        panic!("u8 overflow");
    }
    parsed as u8
}

const fn parse_bool(value: &str) -> bool {
    match parse_u8(value) {
        0 => false,
        1 => true,
        _ => panic!("boolean must be 0 or 1"),
    }
}
