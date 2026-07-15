// Shared implementation for the two explicitly armed, RF-inert retained-
// journal HIL images. Separate crate roots select exactly one compile-time
// mutation mode and supply its required environment values.

use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    rtc_cntl::SocResetReason,
};
use log::{error, info};
use reticulum_board_heltec_tracker_v2::TrackerRxInterlock;
#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
use reticulum_radio_interface::record_radio_fault_before_reset;
use reticulum_radio_interface::{
    RESET_QUARANTINE_SLOT_WORDS, RESET_STORM_QUARANTINE_THRESHOLD, ResetFaultHistory,
    ResetQuarantineDecision, ResetQuarantineReason, ResetQuarantineStorage, RetainedBootReason,
    prepare_reset_quarantine_boot,
};

mod rtc_fast_reset_journal;

use rtc_fast_reset_journal::RtcFastResetJournal;

#[cfg(all(
    feature = "lab-rx-reset-journal-corrupt-hil",
    feature = "lab-rx-reset-journal-torn-hil"
))]
compile_error!("select exactly one reset-journal HIL mutation mode");

#[cfg(not(any(
    feature = "lab-rx-reset-journal-corrupt-hil",
    feature = "lab-rx-reset-journal-torn-hil"
)))]
compile_error!("the reset-journal HIL binary requires an explicit mutation-mode feature");

#[cfg(feature = "safe-idle")]
compile_error!("safe-idle and reset-journal HIL modes are mutually exclusive");

#[cfg(feature = "lab-rx")]
compile_error!("lab-rx and reset-journal HIL modes are mutually exclusive");

#[cfg(feature = "lab-rx-reset-journal-corrupt-hil")]
const ARTIFACT_MODE: &str = "lab-rx-reset-journal-corrupt-hil";
#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
const ARTIFACT_MODE: &str = "lab-rx-reset-journal-torn-hil";

#[cfg(feature = "lab-rx-reset-journal-corrupt-hil")]
const CORRUPT_SLOT: usize = parse_usize(env!("RETICULUM_LAB_RX_RESET_JOURNAL_SLOT"));
#[cfg(feature = "lab-rx-reset-journal-corrupt-hil")]
const CORRUPT_WORD: usize = parse_usize(env!("RETICULUM_LAB_RX_RESET_JOURNAL_WORD"));
#[cfg(feature = "lab-rx-reset-journal-corrupt-hil")]
const CORRUPTION_XOR_MASK: u32 = 1;

#[cfg(feature = "lab-rx-reset-journal-corrupt-hil")]
#[allow(
    clippy::absurd_extreme_comparisons,
    reason = "the env-derived constant is intentionally revalidated in the target crate"
)]
const _: () = {
    if CORRUPT_SLOT >= 2 {
        panic!("RETICULUM_LAB_RX_RESET_JOURNAL_SLOT must be 0 or 1");
    }
    if CORRUPT_WORD >= RESET_QUARANTINE_SLOT_WORDS {
        panic!("RETICULUM_LAB_RX_RESET_JOURNAL_WORD must be in 0..=8");
    }
};

#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
const TORN_WRITE_ORDINAL: usize = parse_usize(env!("RETICULUM_LAB_RX_RESET_JOURNAL_WRITE_ORDINAL"));
#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
const LAST_TORN_WRITE_ORDINAL: usize = 9;

#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
#[allow(
    clippy::absurd_extreme_comparisons,
    reason = "the env-derived constant is intentionally revalidated in the target crate"
)]
const _: () = {
    if TORN_WRITE_ORDINAL == 0 || TORN_WRITE_ORDINAL > LAST_TORN_WRITE_ORDINAL {
        panic!("RETICULUM_LAB_RX_RESET_JOURNAL_WRITE_ORDINAL must be in 1..=9");
    }
};

esp_bootloader_esp_idf::esp_app_desc!();

const fn parse_usize(value: &str) -> usize {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        panic!("empty unsigned integer");
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        panic!("unsigned integer must use canonical decimal form");
    }

    let mut parsed = 0_usize;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii_digit() {
            panic!("invalid unsigned integer");
        }
        let digit = (byte - b'0') as usize;
        if parsed > (usize::MAX - digit) / 10 {
            panic!("unsigned integer overflow");
        }
        parsed = parsed * 10 + digit;
        index += 1;
    }
    parsed
}

const fn retained_boot_reason(reset_reason: Option<SocResetReason>) -> RetainedBootReason {
    match reset_reason {
        Some(SocResetReason::ChipPowerOn) => RetainedBootReason::ChipPowerOn,
        Some(SocResetReason::CoreSw) => RetainedBootReason::CoreSoftwareReset,
        Some(SocResetReason::CoreMwdt0) => RetainedBootReason::SupervisorWatchdogReset,
        Some(_) | None => RetainedBootReason::OtherRetainedReset,
    }
}

const fn is_pristine_power_on_history(history: ResetFaultHistory) -> bool {
    history.generation() == 1
        && history.consecutive_fault_resets() == 0
        && history.total_fault_resets() == 0
        && !history.radio_fault_pending_reset()
}

fn hold_rf_inert_forever() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

fn quarantine_forever(
    reset_reason: Option<SocResetReason>,
    reason: ResetQuarantineReason,
    history: Option<ResetFaultHistory>,
) -> ! {
    error!(
        "phase1 reset-storm quarantine: reset_reason={reset_reason:?} reason={reason:?} history={history:?} threshold={} radio_constructed=false spi_constructed=false executor_timer_constructed=false supervisor_watchdog=off hal_init_watchdogs=disabled rf_state=reset_low_fem_low",
        RESET_STORM_QUARANTINE_THRESHOLD,
    );
    error!(
        "phase1 quarantine remains until startup classifies reset as ChipPowerOn; operator recovery is a verified cold power cycle; esp32s3_reset_reason_0x01_conflates_brownout_and_super_wdt=true"
    );
    hold_rf_inert_forever()
}

#[cfg(feature = "lab-rx-reset-journal-corrupt-hil")]
fn log_artifact_identity(reset_reason: Option<SocResetReason>) {
    info!(
        "phase1 reset-journal HIL artifact identity: mode={} trigger=corrupt-word slot={} word={} xor_mask=0x{:08x} reset_reason={reset_reason:?} radio_constructed=false spi_constructed=false executor_timer_constructed=false supervisor_watchdog=off hal_init_watchdogs=disabled rf_state=reset_low_fem_low",
        ARTIFACT_MODE, CORRUPT_SLOT, CORRUPT_WORD, CORRUPTION_XOR_MASK,
    );
}

#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
fn log_artifact_identity(reset_reason: Option<SocResetReason>) {
    info!(
        "phase1 reset-journal HIL artifact identity: mode={} trigger=torn-write write_ordinal={} reset_reason={reset_reason:?} radio_constructed=false spi_constructed=false executor_timer_constructed=false supervisor_watchdog=off hal_init_watchdogs=disabled rf_state=reset_low_fem_low",
        ARTIFACT_MODE, TORN_WRITE_ORDINAL,
    );
}

#[cfg(feature = "lab-rx-reset-journal-corrupt-hil")]
fn log_expected_quarantine(reset_reason: Option<SocResetReason>) {
    error!(
        "phase1 reset-journal HIL expected quarantine: mode={} trigger=corrupt-word slot={} word={} xor_mask=0x{:08x} reset_reason={reset_reason:?} reason=CorruptOrTornJournal history=None radio_constructed=false spi_constructed=false executor_timer_constructed=false supervisor_watchdog=off rf_state=reset_low_fem_low",
        ARTIFACT_MODE, CORRUPT_SLOT, CORRUPT_WORD, CORRUPTION_XOR_MASK,
    );
}

#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
fn log_expected_quarantine(reset_reason: Option<SocResetReason>) {
    error!(
        "phase1 reset-journal HIL expected quarantine: mode={} trigger=torn-write write_ordinal={} reset_reason={reset_reason:?} reason=CorruptOrTornJournal history=None radio_constructed=false spi_constructed=false executor_timer_constructed=false supervisor_watchdog=off rf_state=reset_low_fem_low",
        ARTIFACT_MODE, TORN_WRITE_ORDINAL,
    );
}

#[cfg(feature = "lab-rx-reset-journal-corrupt-hil")]
fn execute_configured_trigger(journal: &mut RtcFastResetJournal) -> ! {
    let index = CORRUPT_SLOT * RESET_QUARANTINE_SLOT_WORDS + CORRUPT_WORD;
    let before = journal.read_word(index);
    let after = before ^ CORRUPTION_XOR_MASK;
    journal.write_word(index, after);
    journal.write_barrier();
    let observed = journal.read_word(index);
    if observed != after {
        error!(
            "phase1 reset-journal HIL mutation readback failed: mode={} trigger=corrupt-word slot={} word={} expected=0x{:08x} observed=0x{:08x} action=immediate_rf_inert_hold software_reset=false",
            ARTIFACT_MODE, CORRUPT_SLOT, CORRUPT_WORD, after, observed,
        );
        hold_rf_inert_forever();
    }

    info!(
        "phase1 reset-journal HIL triggered: mode={} trigger=corrupt-word slot={} word={} before=0x{:08x} after=0x{:08x} readback=verified action=digital_core_software_reset",
        ARTIFACT_MODE, CORRUPT_SLOT, CORRUPT_WORD, before, after,
    );
    esp_hal::system::software_reset()
}

#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
struct ResetAfterWrite<'storage> {
    storage: &'storage mut RtcFastResetJournal,
    trigger_ordinal: usize,
    writes: usize,
}

#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
impl<'storage> ResetAfterWrite<'storage> {
    fn new(storage: &'storage mut RtcFastResetJournal, trigger_ordinal: usize) -> Self {
        Self {
            storage,
            trigger_ordinal,
            writes: 0,
        }
    }

    fn writes(&self) -> usize {
        self.writes
    }
}

#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
impl ResetQuarantineStorage for ResetAfterWrite<'_> {
    fn read_word(&self, index: usize) -> u32 {
        self.storage.read_word(index)
    }

    fn write_word(&mut self, index: usize, value: u32) {
        self.storage.write_word(index, value);
        self.writes = self.writes.saturating_add(1);
        if self.writes != self.trigger_ordinal {
            return;
        }

        // The decorator resets only after the selected aligned store is known
        // to have reached RTC-fast memory. A failed readback cannot assume the
        // next boot will reject the old journal, so it remains inert in this
        // powered session instead of issuing CoreSw.
        self.storage.write_barrier();
        let observed = self.storage.read_word(index);
        if observed != value {
            error!(
                "phase1 reset-journal HIL mutation readback failed: mode={} trigger=torn-write write_ordinal={} index={} expected=0x{:08x} observed=0x{:08x} action=immediate_rf_inert_hold software_reset=false",
                ARTIFACT_MODE, self.trigger_ordinal, index, value, observed,
            );
            hold_rf_inert_forever();
        }

        info!(
            "phase1 reset-journal HIL triggered: mode={} trigger=torn-write write_ordinal={} slot={} word={} value=0x{:08x} readback=verified action=digital_core_software_reset",
            ARTIFACT_MODE,
            self.trigger_ordinal,
            index / RESET_QUARANTINE_SLOT_WORDS,
            index % RESET_QUARANTINE_SLOT_WORDS,
            value,
        );
        esp_hal::system::software_reset()
    }

    fn write_barrier(&mut self) {
        self.storage.write_barrier();
    }
}

#[cfg(feature = "lab-rx-reset-journal-torn-hil")]
fn execute_configured_trigger(journal: &mut RtcFastResetJournal) -> ! {
    // Construct the decorator only after the ordinary ChipPowerOn transaction
    // established both complete baseline slots. The ordinal therefore counts
    // only the subsequent normal returned-radio-fault journal transaction.
    let mut reset_after_write = ResetAfterWrite::new(journal, TORN_WRITE_ORDINAL);
    let result = record_radio_fault_before_reset(&mut reset_after_write);
    error!(
        "phase1 reset-journal HIL unexpected result: mode={} trigger=torn-write write_ordinal={} observed_writes={} transaction_result={result:?} action=immediate_rf_inert_hold software_reset=false",
        ARTIFACT_MODE,
        TORN_WRITE_ORDINAL,
        reset_after_write.writes(),
    );
    hold_rf_inert_forever()
}

#[allow(
    clippy::large_stack_frames,
    reason = "the firmware entry point initializes target-owned RF-inert resources"
)]
#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let boot_reset_reason = esp_hal::system::reset_reason();

    // Claim every RF-path fail-safe output before touching retained state.
    // SX1262 DIO2 directly owns the KCT8103L CPS net. GPIO46 is a separate
    // header pin on the corrected schematic and is deliberately not claimed.
    let _sx1262_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let fem_power = Output::new(peripherals.GPIO7, Level::Low, OutputConfig::default());
    let fem_csd = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
    let fem_ctx = Output::new(peripherals.GPIO5, Level::Low, OutputConfig::default());
    let _sx1262_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());
    let _interlock = match TrackerRxInterlock::new(fem_power, fem_csd, fem_ctx) {
        Ok(interlock) => interlock,
        Err(_) => panic!("could not establish Tracker RF-inert interlock"),
    };
    let _vext = Output::new(peripherals.GPIO3, Level::Low, OutputConfig::default());
    let _battery_divider = Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default());

    log_artifact_identity(boot_reset_reason);

    let mut journal = RtcFastResetJournal::new();
    match prepare_reset_quarantine_boot(&mut journal, retained_boot_reason(boot_reset_reason)) {
        ResetQuarantineDecision::Run(history) => {
            if boot_reset_reason != Some(SocResetReason::ChipPowerOn)
                || !is_pristine_power_on_history(history)
            {
                error!(
                    "phase1 reset-journal HIL unexpected result: mode={} reset_reason={boot_reset_reason:?} decision=Run history={history:?} expected=pristine_ChipPowerOn_baseline action=immediate_rf_inert_hold software_reset=false",
                    ARTIFACT_MODE,
                );
                hold_rf_inert_forever();
            }

            info!(
                "phase1 reset-journal HIL baseline: mode={} reset_reason={boot_reset_reason:?} generation={} retained_fault_streak={} retained_total_faults={} retained_pending={} next_action=inject_then_CoreSw",
                ARTIFACT_MODE,
                history.generation(),
                history.consecutive_fault_resets(),
                history.total_fault_resets(),
                history.radio_fault_pending_reset(),
            );
            execute_configured_trigger(&mut journal)
        }
        ResetQuarantineDecision::Quarantine { reason, history } => {
            if boot_reset_reason == Some(SocResetReason::CoreSw)
                && reason == ResetQuarantineReason::CorruptOrTornJournal
                && history.is_none()
            {
                log_expected_quarantine(boot_reset_reason);
            } else {
                error!(
                    "phase1 reset-journal HIL unexpected result: mode={} reset_reason={boot_reset_reason:?} decision=Quarantine reason={reason:?} history={history:?} expected=CoreSw_CorruptOrTornJournal action=ordinary_rf_inert_quarantine",
                    ARTIFACT_MODE,
                );
            }
            quarantine_forever(boot_reset_reason, reason, history)
        }
    }
}
