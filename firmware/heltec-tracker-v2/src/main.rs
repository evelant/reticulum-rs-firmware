//! RF-inert Phase-0 firmware entry point.
//!
//! This image proves the current ESP/Rete/lora-phy graph can link for the
//! Tracker. It intentionally contains no radio initialization or transmit
//! path. Do not turn this into an RX/TX driver by adding calls here; concrete
//! radio ownership and the fail-closed TX interlock belong in the Tracker BSP.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware in an unsafe state"
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    timer::timg::TimerGroup,
};
use log::info;
use reticulum_board_heltec_tracker_v2 as board;

#[cfg(not(feature = "safe-idle"))]
compile_error!("the Phase-0 Tracker scaffold must include the safe-idle feature");

#[cfg(feature = "lab-rx")]
compile_error!("safe-idle and lab-rx are mutually exclusive firmware modes");

esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "the firmware entry point initializes target-owned resources"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Establish the electrical interlock before starting the executor or heap:
    // - SX1262 held in reset
    // - KCT8103L power and chip-enable off
    // - KCT8103L CTX held in receive (never transmit)
    // - SX1262 DIO2 remains the sole direct KCT8103L CPS connection
    // - SX1262 NSS inactive
    let _sx1262_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let _fem_power = Output::new(peripherals.GPIO7, Level::Low, OutputConfig::default());
    let _fem_csd = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
    let _fem_ctx = Output::new(peripherals.GPIO5, Level::Low, OutputConfig::default());
    let _sx1262_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());

    // Keep the shared display/GNSS rail and battery-divider load disabled.
    let _vext = Output::new(peripherals.GPIO3, Level::Low, OutputConfig::default());
    let _battery_divider = Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default());

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);

    let timers = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);

    let candidate = reticulum_rns_rete_rx::metadata();
    info!(
        "phase0 safe-idle: board={} rns={} status={:?} tx_enabled={} frequency={:?}",
        board::BOARD_REVISION,
        candidate.id,
        candidate.status,
        board::TX_ENABLED_BY_DEFAULT,
        board::DEFAULT_FREQUENCY_HZ,
    );
    info!(
        "phase0 runtime patch: esp_rtos_main_stack_slice={}",
        env!("RETICULUM_ESP_RTOS_MAIN_STACK_PATCH"),
    );
    info!("RF interlock active; SX1262 reset and KCT8103L controls are held low");

    let _ = spawner;
    loop {
        Timer::after(Duration::from_secs(30)).await;
        info!("phase0 safe-idle alive; no radio initialized");
    }
}
