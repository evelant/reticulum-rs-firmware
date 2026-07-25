//! Sole asynchronous SSD1680 owner for the permanent E290 node.

use core::future::Future;

use allocator_api2::boxed::Box;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Delay, Duration, with_timeout};
use embedded_hal::digital::{Error as DigitalError, ErrorKind, ErrorType, OutputPin};
use embedded_hal_async::digital::Wait;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_alloc::ExternalMemory;
use esp_hal::{
    Async,
    gpio::{Input, Output},
    spi::master::Spi,
};
use log::{error, info};
use reticulum_appliance_display_model::DisplayState;
use reticulum_eink_ssd1680::{E290FrameBuffer, Ssd1680};
use reticulum_heltec_vision_master_e290_node::{
    display_handoff::{
        DisplayBootClearOutcome, DisplayCompletion, DisplayReceiver, DisplayRenderOutcome,
    },
    display_render::render_display_view,
};

const DISPLAY_BUSY_TIMEOUT_MS: u64 = 10_000;

pub(crate) type DisplaySpiDevice = ExclusiveDevice<Spi<'static, Async>, Output<'static>, Delay>;
pub(crate) type ProductDisplay =
    Ssd1680<DisplaySpiDevice, Output<'static>, Output<'static>, DisplayBusy<Input<'static>>, Delay>;

#[derive(Debug)]
pub(crate) enum DisplayBusyError<E> {
    Pin(E),
    Timeout,
}

impl<E: DigitalError> DigitalError for DisplayBusyError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Pin(error) => error.kind(),
            Self::Timeout => ErrorKind::Other,
        }
    }
}

pub(crate) struct DisplayBusy<Pin> {
    pin: Pin,
}

impl<Pin> DisplayBusy<Pin> {
    pub(crate) const fn new(pin: Pin) -> Self {
        Self { pin }
    }
}

impl<Pin> ErrorType for DisplayBusy<Pin>
where
    Pin: ErrorType,
    Pin::Error: DigitalError,
{
    type Error = DisplayBusyError<Pin::Error>;
}

async fn bounded_wait<F, E>(future: F) -> Result<(), DisplayBusyError<E>>
where
    F: Future<Output = Result<(), E>>,
    E: DigitalError,
{
    match with_timeout(Duration::from_millis(DISPLAY_BUSY_TIMEOUT_MS), future).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(DisplayBusyError::Pin(error)),
        Err(_) => Err(DisplayBusyError::Timeout),
    }
}

impl<Pin> Wait for DisplayBusy<Pin>
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

#[embassy_executor::task]
pub(crate) async fn run(
    mut display: ProductDisplay,
    mut display_power: Output<'static>,
    mut frame: Box<E290FrameBuffer, ExternalMemory>,
    mut receiver: DisplayReceiver<CriticalSectionRawMutex>,
) -> ! {
    frame.clear(embedded_graphics::pixelcolor::BinaryColor::On);
    if refresh_cycle(&mut display, &mut display_power, &frame)
        .await
        .is_err()
    {
        receiver.report_boot_clear(DisplayBootClearOutcome::Faulted);
        error!(
            "e290-node stage=display-boot-clear status=FAIL action=disable-display-dependent-fresh-pairing lora_routing=continue"
        );
        disable_forever(&mut display_power, &mut receiver).await
    }
    receiver.report_boot_clear(DisplayBootClearOutcome::Ready);
    info!("e290-node stage=display-boot-clear status=PASS physical_blank=true display_power=low");

    let mut state = DisplayState::new();
    loop {
        let mut request = receiver.next().await;
        while let Some(newer) = receiver.try_take_latest() {
            request = newer;
        }

        if OutputPin::set_high(&mut display_power).is_err() || display.initialize().await.is_err() {
            let completion = DisplayCompletion::new(
                request.request_id(),
                request.requested_view(),
                DisplayRenderOutcome::Faulted,
            );
            receiver.report_completion(completion);
            error!(
                "e290-node stage=display-refresh status=FAIL phase=initialize action=disable-display-dependent-fresh-pairing lora_routing=continue"
            );
            disable_forever(&mut display_power, &mut receiver).await
        }

        let (request_id, requested_view) = loop {
            while let Some(newer) = receiver.try_take_latest() {
                request = newer;
            }
            let requested_view = request.requested_view();
            let (request_id, command) = request.into_parts();
            let _ = state.apply(command);
            if render_display_view(state.view(), &mut frame).is_err() {
                unreachable!("the fixed framebuffer renderer is infallible");
            }
            match receiver.try_take_latest() {
                Some(newer) => request = newer,
                None => break (request_id, requested_view),
            }
        };

        let outcome = if display.refresh(&frame).await.is_ok()
            && display.deep_sleep().await.is_ok()
            && OutputPin::set_low(&mut display_power).is_ok()
        {
            DisplayRenderOutcome::Rendered
        } else {
            let _ = OutputPin::set_low(&mut display_power);
            DisplayRenderOutcome::Faulted
        };
        receiver.report_completion(DisplayCompletion::new(request_id, requested_view, outcome));

        match outcome {
            DisplayRenderOutcome::Rendered => {
                info!(
                    "e290-node stage=display-refresh status=PASS request={} view={:?} display_power=low",
                    request_id.sequence(),
                    requested_view,
                );
            }
            DisplayRenderOutcome::Faulted => {
                error!(
                    "e290-node stage=display-refresh status=FAIL phase=refresh-or-shutdown request={} view={:?} action=disable-display-dependent-fresh-pairing lora_routing=continue",
                    request_id.sequence(),
                    requested_view,
                );
                disable_forever(&mut display_power, &mut receiver).await
            }
        }
    }
}

async fn refresh_cycle(
    display: &mut ProductDisplay,
    display_power: &mut Output<'static>,
    frame: &E290FrameBuffer,
) -> Result<(), ()> {
    OutputPin::set_high(display_power).map_err(|_| ())?;
    if display.initialize().await.is_err()
        || display.refresh(frame).await.is_err()
        || display.deep_sleep().await.is_err()
    {
        let _ = OutputPin::set_low(display_power);
        return Err(());
    }
    OutputPin::set_low(display_power).map_err(|_| ())
}

async fn disable_forever(
    display_power: &mut Output<'static>,
    receiver: &mut DisplayReceiver<CriticalSectionRawMutex>,
) -> ! {
    let _ = OutputPin::set_low(display_power);
    loop {
        let mut request = receiver.next().await;
        while let Some(newer) = receiver.try_take_latest() {
            request = newer;
        }
        receiver.report_completion(DisplayCompletion::new(
            request.request_id(),
            request.requested_view(),
            DisplayRenderOutcome::Faulted,
        ));
    }
}
