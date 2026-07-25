//! Display-only SSD1680 qualification image for the Vision Master E290.
//!
//! The image holds the HT-RA62/SX1262 in reset, gives SPI3 and GPIO1--6
//! exclusively to the fitted DEPG0290BNS800F6 panel, clears any physically
//! retained content before showing a fixed non-secret pairing demonstration,
//! then deep-sleeps and powers down the display controller.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware active"
)]
#![deny(clippy::large_stack_frames)]

use core::future::Future;

use embassy_executor::Spawner;
use embassy_time::{Delay, Duration, Instant, Timer, with_timeout};
use embedded_graphics::{
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_6X10, FONT_10X20},
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use embedded_hal::digital::{Error as DigitalError, ErrorKind, ErrorType, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig},
    spi::{
        Mode as SpiMode,
        master::{Config as SpiConfig, Spi},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
use log::{error, info};
use reticulum_board_heltec_vision_master_e290::{
    EINK_HEIGHT_PIXELS, EINK_MONOCHROME_FRAME_BYTES, EINK_SPI_FREQUENCY_HZ, EINK_WIDTH_PIXELS,
    FITTED_EINK_CONTROLLER, FITTED_EINK_PANEL,
};
use reticulum_eink_ssd1680::{E290FrameBuffer, Ssd1680};

#[cfg(debug_assertions)]
compile_error!("the E290 display HIL must be built with --release");

const BUSY_TIMEOUT_MS: u64 = 10_000;
const HEARTBEAT_SECONDS: u64 = 30;
const INTERNAL_HEAP_BYTES: usize = 64 * 1024;

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

async fn bounded_wait<F, E>(future: F) -> Result<(), BoundedBusyError<E>>
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
        bounded_wait(self.pin.wait_for_high()).await
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        bounded_wait(self.pin.wait_for_low()).await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        bounded_wait(self.pin.wait_for_rising_edge()).await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        bounded_wait(self.pin.wait_for_falling_edge()).await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        bounded_wait(self.pin.wait_for_any_edge()).await
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "the isolated HIL deliberately owns one exact 4,736-byte framebuffer"
)]
#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Establish the complete board-level RF interlock before timers, SPI, or
    // display power. This HIL must never construct or communicate with LoRa.
    let _radio_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let _radio_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());

    esp_println::logger::init_logger_from_env();
    let base_mac = esp_hal::efuse::base_mac_address();
    info!(
        "e290-display-hil stage=boot status=PASS base_mac={} rf_inert=true panel={} controller={} width={} height={} frame_bytes={}",
        base_mac,
        FITTED_EINK_PANEL,
        FITTED_EINK_CONTROLLER,
        EINK_WIDTH_PIXELS,
        EINK_HEIGHT_PIXELS,
        EINK_MONOCHROME_FRAME_BYTES,
    );

    esp_alloc::heap_allocator!(
        #[esp_hal::ram(reclaimed)]
        size: INTERNAL_HEAP_BYTES
    );
    let timers = TimerGroup::new(peripherals.TIMG0);
    let software_interrupts =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timers.timer0, software_interrupts.software_interrupt0);

    let mut display_power = Output::new(peripherals.GPIO18, Level::Low, OutputConfig::default());
    let chip_select = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
    let data_command = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
    let reset = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());
    let busy = BoundedBusy::new(Input::new(peripherals.GPIO6, InputConfig::default()));

    let spi = match Spi::new(
        peripherals.SPI3,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(EINK_SPI_FREQUENCY_HZ))
            .with_mode(SpiMode::_0),
    ) {
        Ok(spi) => spi,
        Err(_) => fail_inert(&mut display_power, "spi-construction").await,
    }
    .with_sck(peripherals.GPIO2)
    .with_mosi(peripherals.GPIO1)
    .into_async();
    let spi_device = match ExclusiveDevice::new(spi, chip_select, Delay) {
        Ok(device) => device,
        Err(_) => fail_inert(&mut display_power, "spi-device").await,
    };

    if OutputPin::set_high(&mut display_power).is_err() {
        fail_inert(&mut display_power, "display-power-enable").await;
    }
    info!(
        "e290-display-hil stage=display-power status=PASS gpio=18 level=high spi=SPI3 spi_hz={}",
        EINK_SPI_FREQUENCY_HZ
    );

    let mut display = Ssd1680::new(spi_device, data_command, reset, busy, Delay);
    let initialized_at = Instant::now();
    if display.initialize().await.is_err() {
        fail_inert(&mut display_power, "controller-initialize").await;
    }
    info!(
        "e290-display-hil stage=controller-initialize status=PASS elapsed_ms={}",
        initialized_at.elapsed().as_millis()
    );

    // The first physical operation after every reboot clears any passkey or
    // other content retained while the panel had no power.
    let mut frame = E290FrameBuffer::new_white();
    let clear_started = Instant::now();
    if display.refresh(&frame).await.is_err() {
        fail_inert(&mut display_power, "boot-clear").await;
    }
    info!(
        "e290-display-hil stage=boot-clear status=PASS elapsed_ms={} secret_retained=false",
        clear_started.elapsed().as_millis()
    );

    render_demo(&mut frame);
    let demo_started = Instant::now();
    if display.refresh(&frame).await.is_err() {
        fail_inert(&mut display_power, "demo-refresh").await;
    }
    info!(
        "e290-display-hil stage=demo-refresh status=PASS elapsed_ms={} fixed_non_secret_passkey=123456",
        demo_started.elapsed().as_millis()
    );

    if display.deep_sleep().await.is_err() {
        fail_inert(&mut display_power, "controller-deep-sleep").await;
    }
    if OutputPin::set_low(&mut display_power).is_err() {
        fail_inert(&mut display_power, "display-power-disable").await;
    }
    info!(
        "e290-display-hil stage=complete status=PASS controller=deep-sleep display_power=low retained_view=fixed-non-secret-demo rf_inert=true"
    );

    loop {
        Timer::after_secs(HEARTBEAT_SECONDS).await;
        info!("e290-display-hil stage=heartbeat status=PASS display_power=low rf_inert=true");
    }
}

fn render_demo(frame: &mut E290FrameBuffer) {
    frame.clear(BinaryColor::Off);

    Rectangle::new(
        Point::zero(),
        Size::new(EINK_WIDTH_PIXELS as u32, EINK_HEIGHT_PIXELS as u32),
    )
    .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 2))
    .draw(frame)
    .expect("the framebuffer draw target is infallible");
    Line::new(Point::new(10, 45), Point::new(285, 45))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(frame)
        .expect("the framebuffer draw target is infallible");

    let centered = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build();
    Text::with_text_style(
        "RETICULUM",
        Point::new(148, 12),
        MonoTextStyle::new(&FONT_10X20, BinaryColor::On),
        centered,
    )
    .draw(frame)
    .expect("the framebuffer draw target is infallible");
    Text::with_text_style(
        "PAIR 123456",
        Point::new(148, 58),
        MonoTextStyle::new(&FONT_10X20, BinaryColor::On),
        centered,
    )
    .draw(frame)
    .expect("the framebuffer draw target is infallible");
    Text::with_text_style(
        "DISPLAY HIL - FIXED DEMO CODE",
        Point::new(148, 105),
        MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        centered,
    )
    .draw(frame)
    .expect("the framebuffer draw target is infallible");
}

async fn fail_inert<Power>(display_power: &mut Power, stage: &'static str) -> !
where
    Power: OutputPin,
    Power::Error: DigitalError,
{
    let power_low = OutputPin::set_low(display_power).is_ok();
    error!(
        "e290-display-hil stage={} status=FAIL display_power_low={} rf_inert=true",
        stage, power_low
    );
    loop {
        Timer::after_secs(HEARTBEAT_SECONDS).await;
        error!(
            "e290-display-hil stage=failed-heartbeat status=FAIL failed_stage={} display_power_low={} rf_inert=true",
            stage, power_low
        );
    }
}
