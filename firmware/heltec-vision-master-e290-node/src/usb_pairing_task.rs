//! Sole USB Serial/JTAG and physical-presence GPIO owner.
//!
//! This task terminates only the unauthenticated credential-initialization
//! control records. It is not a Reticulum packet interface and has no access
//! to node routing, radio, credential, journal, or raw flash owners.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Instant, Timer};
use esp_hal::{
    Blocking,
    gpio::Input,
    peripherals::USB_DEVICE,
    usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagTx},
};
use reticulum_device_api_framing::{DecodeEvent, FramedRecord, StreamDecoder};
use reticulum_device_api_pairing_control::{ControlRequest, ControlResponse};
use reticulum_device_api_pairing_policy::{
    ActiveLowButton, ConnectionId, MonotonicMillis as PairingMillis,
};
use reticulum_heltec_vision_master_e290_node::{
    config,
    pairing_control_handoff::{
        ButtonObservationReply, ExclusiveAcquisitionReply, LifecycleAcknowledgement,
        PairingControlCommand, PairingControlReply, PairingControlReplyKind, UsbPairingHandoff,
    },
    usb_pairing_policy::{
        ActiveLowButtonDebouncer, ExactNextSequenceGate, PhysicalPresencePublicationGuard,
        UsbConnectionEvent, UsbConnectionTracker, UsbPairingWork, select_usb_pairing_work,
    },
};

#[derive(Clone, Copy)]
enum PendingPurpose {
    Connected(ConnectionId),
    Disconnected(ConnectionId),
    Button(ConnectionId),
    Exclusive(ConnectionId),
    Control {
        connection: ConnectionId,
        sequence: u64,
    },
}

impl PendingPurpose {
    const fn connection(self) -> ConnectionId {
        match self {
            Self::Connected(connection)
            | Self::Disconnected(connection)
            | Self::Button(connection)
            | Self::Exclusive(connection)
            | Self::Control { connection, .. } => connection,
        }
    }
}

struct PendingCommand {
    command: PairingControlCommand,
    purpose: PendingPurpose,
}

struct PendingTransmission {
    frame: FramedRecord,
}

/// Run the sole pre-authentication USB/GPIO bearer.
#[embassy_executor::task]
pub async fn run(
    usb_device: USB_DEVICE<'static>,
    button: Input<'static>,
    mut handoff: UsbPairingHandoff<CriticalSectionRawMutex>,
) {
    let (mut rx, mut tx) = UsbSerialJtag::<Blocking>::new(usb_device).split();
    let registers = USB_DEVICE::regs();
    registers.int_clr().write(|write| {
        write
            .sof()
            .clear_bit_by_one()
            .usb_bus_reset()
            .clear_bit_by_one()
    });

    let started = Instant::now().as_millis();
    let raw_button = active_low_level(button.is_low());
    let mut debouncer = ActiveLowButtonDebouncer::new(started, raw_button);
    let mut button_publication = PhysicalPresencePublicationGuard::new();
    let mut connection_tracker = UsbConnectionTracker::new(started);
    let mut decoder = StreamDecoder::new();
    let mut sequence_gate: Option<ExactNextSequenceGate> = None;
    let mut pending_command: Option<PendingCommand> = None;
    let mut awaiting_reply: Option<PendingPurpose> = None;
    let mut transmission: Option<PendingTransmission> = None;
    let mut announced_connection: Option<ConnectionId> = None;
    let mut disconnect_pending: Option<ConnectionId> = None;
    let mut tracker_failed = false;
    let mut button_failed = false;
    let mut control_turn_after_button = false;
    let mut last_button_observation_ms =
        started.saturating_sub(config::PAIRING_BUTTON_OBSERVATION_INTERVAL_MS);

    loop {
        let now_millis = Instant::now().as_millis();
        let raw = registers.int_raw().read();
        let saw_sof = raw.sof().bit_is_set();
        let saw_bus_reset = raw.usb_bus_reset().bit_is_set();
        if saw_sof || saw_bus_reset {
            registers.int_clr().write(|write| {
                if saw_sof {
                    write.sof().clear_bit_by_one();
                }
                if saw_bus_reset {
                    write.usb_bus_reset().clear_bit_by_one();
                }
                write
            });
        }

        if !button_failed {
            match debouncer.observe(now_millis, active_low_level(button.is_low())) {
                Ok(observation) => button_publication.observe(observation),
                Err(_) => button_failed = true,
            }
        }

        // While an old connection is being retired, discard fresh SOF
        // observations. A host supplies another SOF one millisecond later,
        // after the disconnect acknowledgement releases the old epoch.
        if !tracker_failed && disconnect_pending.is_none() {
            let previous_connection = connection_tracker.connection();
            match connection_tracker.observe(now_millis, saw_sof, saw_bus_reset) {
                Ok(UsbConnectionEvent::None) => {}
                Ok(UsbConnectionEvent::Connected(connection)) => {
                    discard_rx_fifo(&mut rx);
                    decoder.reset();
                    sequence_gate = Some(ExactNextSequenceGate::new(connection));
                    if !button_failed
                        && debouncer
                            .reset_for_connection(now_millis, active_low_level(button.is_low()))
                            .is_err()
                    {
                        button_failed = true;
                    }
                    button_publication.reset_for_connection();
                    control_turn_after_button = false;
                    last_button_observation_ms =
                        now_millis.saturating_sub(config::PAIRING_BUTTON_OBSERVATION_INTERVAL_MS);
                    queue_command(
                        &mut pending_command,
                        PairingControlCommand::Connected {
                            at: pairing_time(now_millis),
                            connection,
                        },
                        PendingPurpose::Connected(connection),
                    );
                }
                Ok(UsbConnectionEvent::Disconnected(connection)) => {
                    discard_rx_fifo(&mut rx);
                    control_turn_after_button = false;
                    invalidate_connection(
                        connection,
                        &mut decoder,
                        &mut sequence_gate,
                        &mut pending_command,
                        &mut transmission,
                        announced_connection,
                        &mut disconnect_pending,
                    );
                }
                Ok(UsbConnectionEvent::Suspended(_) | UsbConnectionEvent::Resumed(_)) => {
                    // A missed-SOF suspension pauses endpoint work but retains
                    // the connection, sequence gate, and any response bytes
                    // already committed to the hardware TX FIFO. Only a bus
                    // reset retires that causal epoch.
                }
                Err(_) => {
                    tracker_failed = true;
                    discard_rx_fifo(&mut rx);
                    if let Some(connection) = previous_connection {
                        invalidate_connection(
                            connection,
                            &mut decoder,
                            &mut sequence_gate,
                            &mut pending_command,
                            &mut transmission,
                            announced_connection,
                            &mut disconnect_pending,
                        );
                    }
                }
            }
        }

        if connection_tracker.connection().is_none() || disconnect_pending.is_some() {
            discard_rx_fifo(&mut rx);
        }

        if let Some(purpose) = awaiting_reply {
            while let Some(reply) = handoff.try_receive_reply() {
                if reply.connection() != purpose.connection() {
                    continue;
                }
                awaiting_reply = None;
                let button_acknowledged = matches!(purpose, PendingPurpose::Button(_));
                handle_reply(
                    reply,
                    purpose,
                    now_millis,
                    &mut pending_command,
                    &mut transmission,
                    &mut announced_connection,
                    &mut disconnect_pending,
                );
                if button_acknowledged && disconnect_pending.is_none() {
                    control_turn_after_button = true;
                }
                break;
            }
        }

        if connection_tracker.active().is_some()
            && let Some(pending_tx) = transmission.as_mut()
            && step_transmission(&mut tx, pending_tx)
        {
            transmission = None;
        }

        if awaiting_reply.is_none() && pending_command.is_none() {
            if let Some(connection) = disconnect_pending {
                if announced_connection == Some(connection) {
                    queue_command(
                        &mut pending_command,
                        PairingControlCommand::Disconnected {
                            at: pairing_time(now_millis),
                            connection,
                        },
                        PendingPurpose::Disconnected(connection),
                    );
                } else {
                    disconnect_pending = None;
                }
            } else if let Some(connection) = announced_connection
                && connection_tracker.connection() == Some(connection)
            {
                let periodic_button_due = now_millis.saturating_sub(last_button_observation_ms)
                    >= config::PAIRING_BUTTON_OBSERVATION_INTERVAL_MS;
                let button_due =
                    !button_failed && button_publication.publication_due(periodic_button_due);
                let control_ready =
                    connection_tracker.active() == Some(connection) && transmission.is_none();
                match select_usb_pairing_work(button_due, control_ready, control_turn_after_button)
                {
                    UsbPairingWork::ObserveButton => {
                        control_turn_after_button = false;
                        queue_button_observation(
                            &mut pending_command,
                            &mut last_button_observation_ms,
                            &mut button_publication,
                            now_millis,
                            connection,
                            debouncer.current(),
                        );
                    }
                    UsbPairingWork::PollControl {
                        observe_button_if_empty,
                    } => {
                        control_turn_after_button = false;
                        let accepted = receive_control_request(
                            &mut rx,
                            &mut decoder,
                            sequence_gate.as_mut(),
                            connection,
                            now_millis,
                            &mut pending_command,
                        );
                        if observe_button_if_empty && !accepted {
                            queue_button_observation(
                                &mut pending_command,
                                &mut last_button_observation_ms,
                                &mut button_publication,
                                now_millis,
                                connection,
                                debouncer.current(),
                            );
                        }
                    }
                    UsbPairingWork::Wait => {}
                }
            }
        }

        if awaiting_reply.is_none()
            && let Some(pending) = pending_command.take()
        {
            match handoff.try_send_command(pending.command) {
                Ok(()) => {
                    if matches!(pending.purpose, PendingPurpose::Connected(_)) {
                        announced_connection = Some(pending.purpose.connection());
                    }
                    awaiting_reply = Some(pending.purpose);
                }
                Err(pressure) => {
                    pending_command = Some(PendingCommand {
                        command: pressure.into_inner(),
                        purpose: pending.purpose,
                    });
                }
            }
        }

        Timer::after_millis(config::USB_PAIRING_POLL_INTERVAL_MS).await;
    }
}

fn receive_control_request(
    rx: &mut esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, Blocking>,
    decoder: &mut StreamDecoder,
    sequence_gate: Option<&mut ExactNextSequenceGate>,
    connection: ConnectionId,
    now_millis: u64,
    pending_command: &mut Option<PendingCommand>,
) -> bool {
    let Some(sequence_gate) = sequence_gate else {
        return false;
    };
    for _ in 0..config::USB_PAIRING_MAX_BYTES_PER_POLL {
        let Ok(byte) = rx.read_byte() else {
            break;
        };
        let DecodeEvent::Record(record) = decoder.push(byte) else {
            continue;
        };
        let Ok(request) = ControlRequest::from_record(record) else {
            continue;
        };
        if sequence_gate
            .accept(connection, request.sequence())
            .is_err()
        {
            continue;
        }
        queue_command(
            pending_command,
            PairingControlCommand::Control {
                at: pairing_time(now_millis),
                connection,
                request,
            },
            PendingPurpose::Control {
                connection,
                sequence: request.sequence(),
            },
        );
        return true;
    }
    false
}

fn handle_reply(
    reply: PairingControlReply,
    purpose: PendingPurpose,
    now_millis: u64,
    pending_command: &mut Option<PendingCommand>,
    transmission: &mut Option<PendingTransmission>,
    announced_connection: &mut Option<ConnectionId>,
    disconnect_pending: &mut Option<ConnectionId>,
) {
    let connection = purpose.connection();
    match (purpose, reply.into_kind()) {
        (
            PendingPurpose::Connected(expected),
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Connected),
        ) if expected == connection => {}
        (
            PendingPurpose::Disconnected(expected),
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Disconnected),
        ) if expected == connection => {
            if *announced_connection == Some(connection) {
                *announced_connection = None;
            }
            if *disconnect_pending == Some(connection) {
                *disconnect_pending = None;
            }
        }
        (
            PendingPurpose::Button(expected),
            PairingControlReplyKind::Button(ButtonObservationReply::AcquireExclusive),
        ) if expected == connection && disconnect_pending.is_none() => {
            queue_command(
                pending_command,
                PairingControlCommand::ExclusiveAcquired {
                    at: pairing_time(now_millis),
                    connection,
                },
                PendingPurpose::Exclusive(connection),
            );
        }
        (
            PendingPurpose::Button(expected),
            PairingControlReplyKind::Button(ButtonObservationReply::Observed),
        ) if expected == connection => {}
        (
            PendingPurpose::Exclusive(expected),
            PairingControlReplyKind::Exclusive(
                ExclusiveAcquisitionReply::Opened
                | ExclusiveAcquisitionReply::Closed
                | ExclusiveAcquisitionReply::Refused,
            ),
        ) if expected == connection => {}
        (
            PendingPurpose::Control {
                connection: expected,
                sequence,
            },
            PairingControlReplyKind::Control(response),
        ) if expected == connection && response.sequence() == sequence => {
            if disconnect_pending.is_none()
                && let Ok(frame) = FramedRecord::encode(&response.into_record())
            {
                *transmission = Some(PendingTransmission { frame });
            }
        }
        _ => {
            // A same-epoch causal mismatch is an internal ownership fault.
            // Retire the connection instead of accepting another command.
            *disconnect_pending = Some(connection);
            *transmission = None;
        }
    }
}

fn step_transmission(
    tx: &mut UsbSerialJtagTx<'static, Blocking>,
    pending: &mut PendingTransmission,
) -> bool {
    let mut acknowledged = 0_usize;
    for byte in pending
        .frame
        .next_chunk(config::USB_PAIRING_MAX_BYTES_PER_POLL)
    {
        if tx.write_byte_nb(*byte).is_err() {
            break;
        }
        acknowledged += 1;
    }
    pending
        .frame
        .advance(acknowledged)
        .expect("USB acknowledged no more than the selected frame chunk");
    if !pending.frame.is_complete() {
        return false;
    }

    // `flush_tx_nb` sets the hardware WR_DONE bit before it reports whether
    // transfer completion is immediately observable. Once every byte is in
    // the endpoint FIFO and WR_DONE has been requested, the hardware owns the
    // response. Keeping this software owner until a later `Ok` can deadlock RX
    // after the host has already received the frame. A subsequent response
    // remains losslessly backpressured by `write_byte_nb` until FIFO space is
    // available.
    let _ = tx.flush_tx_nb();
    true
}

fn discard_rx_fifo(rx: &mut esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, Blocking>) {
    let mut discarded = [0_u8; config::USB_PAIRING_MAX_BYTES_PER_POLL];
    let _ = rx.drain_rx_fifo(&mut discarded);
}

#[allow(clippy::too_many_arguments)]
fn invalidate_connection(
    connection: ConnectionId,
    decoder: &mut StreamDecoder,
    sequence_gate: &mut Option<ExactNextSequenceGate>,
    pending_command: &mut Option<PendingCommand>,
    transmission: &mut Option<PendingTransmission>,
    announced_connection: Option<ConnectionId>,
    disconnect_pending: &mut Option<ConnectionId>,
) {
    decoder.reset();
    *sequence_gate = None;
    *transmission = None;
    if pending_command
        .as_ref()
        .is_some_and(|pending| pending.purpose.connection() == connection)
    {
        *pending_command = None;
    }
    if announced_connection == Some(connection) {
        *disconnect_pending = Some(connection);
    }
}

fn queue_command(
    pending: &mut Option<PendingCommand>,
    command: PairingControlCommand,
    purpose: PendingPurpose,
) {
    debug_assert!(pending.is_none());
    *pending = Some(PendingCommand { command, purpose });
}

fn queue_button_observation(
    pending: &mut Option<PendingCommand>,
    last_observation_ms: &mut u64,
    publication: &mut PhysicalPresencePublicationGuard,
    now_millis: u64,
    connection: ConnectionId,
    debounced_level: ActiveLowButton,
) {
    // Each control turn is bounded to one input chunk and one accepted
    // request. The next due observation regains priority, so traffic cannot
    // hide an unobserved release/re-press indefinitely.
    *last_observation_ms = now_millis;
    let level = publication.policy_level(debounced_level);
    queue_command(
        pending,
        PairingControlCommand::ObserveButton {
            at: pairing_time(now_millis),
            connection,
            level,
        },
        PendingPurpose::Button(connection),
    );
    publication.publication_queued();
}

fn active_low_level(is_low: bool) -> ActiveLowButton {
    if is_low {
        ActiveLowButton::Low
    } else {
        ActiveLowButton::High
    }
}

const fn pairing_time(now_millis: u64) -> PairingMillis {
    PairingMillis::new(now_millis)
}

// Keep the response type visible in this module's dependency audit even
// though replies reach it through the handoff enum.
const _: Option<ControlResponse> = None;
