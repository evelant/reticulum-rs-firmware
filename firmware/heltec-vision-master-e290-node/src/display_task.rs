//! Sole asynchronous SSD1680 owner for the permanent E290 node.

use core::future::Future;

use allocator_api2::boxed::Box;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Delay, Duration, Instant, Timer, with_timeout};
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
use reticulum_appliance_display_model::{DisplayCommand, DisplayState};
use reticulum_eink_ssd1680::{E290FrameBuffer, Ssd1680};
use reticulum_heltec_vision_master_e290_node::{
    display_coordinator::{DisplayCoordinatorOutput, E290DisplayCoordinator},
    display_handoff::{
        DisplayBootClearOutcome, DisplayCompletion, DisplayHomeTelemetry, DisplayReceiver,
        DisplayRenderOutcome, DisplayRequest, DisplayTelemetryReceiver,
        overlay_observed_home_telemetry,
    },
    display_render::render_display_view,
};

const DISPLAY_BUSY_TIMEOUT_MS: u64 = 10_000;

pub(crate) type DisplaySpiDevice = ExclusiveDevice<Spi<'static, Async>, Output<'static>, Delay>;
pub(crate) type ProductDisplay =
    Ssd1680<DisplaySpiDevice, Output<'static>, Output<'static>, DisplayBusy<Input<'static>>, Delay>;

enum DisplayActorInput {
    Request(DisplayRequest),
    Telemetry(DisplayHomeTelemetry),
    Poll,
}

async fn next_input(
    receiver: &mut DisplayReceiver<CriticalSectionRawMutex>,
    telemetry: &mut DisplayTelemetryReceiver<CriticalSectionRawMutex>,
    poll_at_ms: Option<u64>,
) -> DisplayActorInput {
    match poll_at_ms {
        Some(deadline) => match select3(
            receiver.next(),
            telemetry.next(),
            Timer::at(Instant::from_millis(deadline)),
        )
        .await
        {
            Either3::First(request) => DisplayActorInput::Request(request),
            Either3::Second(snapshot) => DisplayActorInput::Telemetry(snapshot),
            Either3::Third(()) => DisplayActorInput::Poll,
        },
        None => match select(receiver.next(), telemetry.next()).await {
            Either::First(request) => DisplayActorInput::Request(request),
            Either::Second(snapshot) => DisplayActorInput::Telemetry(snapshot),
        },
    }
}

fn coordinate_request(
    coordinator: &mut Option<E290DisplayCoordinator>,
    telemetry: Option<DisplayHomeTelemetry>,
    command: DisplayCommand,
    now_ms: u64,
) -> DisplayCoordinatorOutput {
    match command {
        DisplayCommand::ShowHome { snapshot } => {
            let snapshot = overlay_observed_home_telemetry(snapshot, telemetry);
            let owner = coordinator.insert(E290DisplayCoordinator::new(snapshot));
            owner.resume_home(now_ms)
        }
        DisplayCommand::ShowBooting { label } => coordinator.as_mut().map_or(
            DisplayCoordinatorOutput::Publish(DisplayCommand::ShowBooting { label }),
            |owner| owner.show_booting(label),
        ),
        DisplayCommand::ShowPairing {
            label,
            passkey,
            expires_after_seconds,
        } => match coordinator.as_mut() {
            Some(owner) => owner.show_pairing(label, passkey, expires_after_seconds),
            None => DisplayCoordinatorOutput::Publish(DisplayCommand::ShowPairing {
                label,
                passkey,
                expires_after_seconds,
            }),
        },
        DisplayCommand::ClearPairingSecret { label, reason } => coordinator.as_mut().map_or(
            DisplayCoordinatorOutput::Publish(DisplayCommand::ClearPairingSecret { label, reason }),
            |owner| owner.clear_pairing_secret(label, reason),
        ),
    }
}

fn retain_poll_deadline(
    output: &DisplayCoordinatorOutput,
    current: Option<u64>,
    polling: bool,
) -> Option<u64> {
    match output {
        DisplayCoordinatorOutput::Deferred { refresh_at_ms } => *refresh_at_ms,
        DisplayCoordinatorOutput::Publish(_) => None,
        DisplayCoordinatorOutput::Unchanged if polling => None,
        DisplayCoordinatorOutput::Unchanged => current,
    }
}

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
    mut telemetry_receiver: DisplayTelemetryReceiver<CriticalSectionRawMutex>,
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
    let mut coordinator: Option<E290DisplayCoordinator> = None;
    // Absence is distinct from an observed zero count: the first Home request
    // already carries the durable boot projection and must not be overwritten
    // before the node actor publishes its first live telemetry snapshot.
    let mut telemetry = None;
    let mut poll_at_ms = None;
    loop {
        let input = next_input(&mut receiver, &mut telemetry_receiver, poll_at_ms).await;
        let polling = matches!(input, DisplayActorInput::Poll);
        let mut request = match input {
            DisplayActorInput::Request(request) => Some(request),
            DisplayActorInput::Telemetry(snapshot) => {
                telemetry = Some(snapshot);
                None
            }
            DisplayActorInput::Poll => None,
        };
        while let Some(newer) = receiver.try_take_latest() {
            request = Some(newer);
        }
        while let Some(newer) = telemetry_receiver.try_take_latest() {
            telemetry = Some(newer);
        }

        let now_ms = Instant::now().as_millis();
        let mut requested_completion = None;
        let output = if let Some(request) = request {
            let requested_view = request.requested_view();
            let (request_id, command) = request.into_parts();
            requested_completion = Some((request_id, requested_view));
            coordinate_request(&mut coordinator, telemetry, command, now_ms)
        } else if let (Some(owner), Some(telemetry)) = (coordinator.as_mut(), telemetry) {
            if polling {
                owner.poll(now_ms)
            } else {
                owner.set_uncollected_messages(telemetry.uncollected_messages(), now_ms)
            }
        } else {
            DisplayCoordinatorOutput::Unchanged
        };
        poll_at_ms = retain_poll_deadline(&output, poll_at_ms, polling);
        let Some(mut command) = output.into_command() else {
            if let Some((request_id, requested_view)) = requested_completion {
                receiver.report_completion(DisplayCompletion::new(
                    request_id,
                    requested_view,
                    DisplayRenderOutcome::Rendered,
                ));
            }
            continue;
        };

        while let Some(newer) = telemetry_receiver.try_take_latest() {
            telemetry = Some(newer);
        }
        if let (Some(owner), Some(telemetry)) = (coordinator.as_mut(), telemetry) {
            let output = owner.set_uncollected_messages(telemetry.uncollected_messages(), now_ms);
            poll_at_ms = retain_poll_deadline(&output, poll_at_ms, false);
            if let Some(newer) = output.into_command() {
                command = newer;
            }
        }

        if OutputPin::set_high(&mut display_power).is_err() || display.initialize().await.is_err() {
            if let Some((request_id, requested_view)) = requested_completion {
                receiver.report_completion(DisplayCompletion::new(
                    request_id,
                    requested_view,
                    DisplayRenderOutcome::Faulted,
                ));
            }
            error!(
                "e290-node stage=display-refresh status=FAIL phase=initialize action=disable-display-dependent-fresh-pairing lora_routing=continue"
            );
            disable_forever(&mut display_power, &mut receiver).await
        }

        loop {
            while let Some(newer) = receiver.try_take_latest() {
                let requested_view = newer.requested_view();
                let (request_id, newer_command) = newer.into_parts();
                requested_completion = Some((request_id, requested_view));
                let output = coordinate_request(&mut coordinator, telemetry, newer_command, now_ms);
                poll_at_ms = retain_poll_deadline(&output, poll_at_ms, false);
                if let Some(newer_command) = output.into_command() {
                    command = newer_command;
                }
            }
            while let Some(newer) = telemetry_receiver.try_take_latest() {
                telemetry = Some(newer);
            }
            if let (Some(owner), Some(telemetry)) = (coordinator.as_mut(), telemetry) {
                let output =
                    owner.set_uncollected_messages(telemetry.uncollected_messages(), now_ms);
                poll_at_ms = retain_poll_deadline(&output, poll_at_ms, false);
                if let Some(newer_command) = output.into_command() {
                    command = newer_command;
                }
            }
            let _ = state.apply(command);
            if render_display_view(state.view(), &mut frame).is_err() {
                unreachable!("the fixed framebuffer renderer is infallible");
            }
            match receiver.try_take_latest() {
                Some(newer) => {
                    let requested_view = newer.requested_view();
                    let (request_id, newer_command) = newer.into_parts();
                    requested_completion = Some((request_id, requested_view));
                    let output =
                        coordinate_request(&mut coordinator, telemetry, newer_command, now_ms);
                    poll_at_ms = retain_poll_deadline(&output, poll_at_ms, false);
                    match output.into_command() {
                        Some(newer_command) => command = newer_command,
                        None => break,
                    }
                }
                None => break,
            }
        }

        let outcome = if display.refresh(&frame).await.is_ok()
            && display.deep_sleep().await.is_ok()
            && OutputPin::set_low(&mut display_power).is_ok()
        {
            DisplayRenderOutcome::Rendered
        } else {
            let _ = OutputPin::set_low(&mut display_power);
            DisplayRenderOutcome::Faulted
        };
        if let Some((request_id, requested_view)) = requested_completion {
            receiver.report_completion(DisplayCompletion::new(request_id, requested_view, outcome));
        }

        match outcome {
            DisplayRenderOutcome::Rendered => {
                if let Some((request_id, requested_view)) = requested_completion {
                    info!(
                        "e290-node stage=display-refresh status=PASS request={} view={:?} display_power=low",
                        request_id.sequence(),
                        requested_view,
                    );
                } else {
                    info!(
                        "e290-node stage=display-refresh status=PASS source=home-telemetry display_power=low"
                    );
                }
            }
            DisplayRenderOutcome::Faulted => {
                if let Some((request_id, requested_view)) = requested_completion {
                    error!(
                        "e290-node stage=display-refresh status=FAIL phase=refresh-or-shutdown request={} view={:?} action=disable-display-dependent-fresh-pairing lora_routing=continue",
                        request_id.sequence(),
                        requested_view,
                    );
                } else {
                    error!(
                        "e290-node stage=display-refresh status=FAIL phase=refresh-or-shutdown source=home-telemetry action=disable-display-dependent-fresh-pairing lora_routing=continue"
                    );
                }
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
