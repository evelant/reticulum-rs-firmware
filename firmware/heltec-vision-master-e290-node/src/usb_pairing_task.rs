//! Sole USB Serial/JTAG and physical-presence GPIO owner.
//!
//! This task terminates unauthenticated credential-initialization, live-pairing,
//! and one authenticated local-API session on a shared framed stream. Pairing
//! keeps its independent exact-next sequence space; session handshakes and
//! requests use their own authenticated counters. It is not a Reticulum packet
//! interface and has no access to node routing, radio, credential, journal, or
//! raw flash owners.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{
    Blocking,
    gpio::Input,
    peripherals::USB_DEVICE,
    ram,
    rng::Trng,
    usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagTx},
};
use reticulum_device_api_framing::{DecodeEvent, FramedRecord, Record, StreamDecoder};
use reticulum_device_api_handoff::BearerHandoff;
use reticulum_device_api_pairing_control::ControlResponse;
use reticulum_device_api_pairing_policy::{
    ActiveLowButton, ConnectionId, MonotonicMillis as PairingMillis,
};
use reticulum_device_api_session::{
    AuthenticatedGrant, RECORD_KIND_CLIENT_HELLO, ServerParameters,
};
use reticulum_heltec_vision_master_e290_node::{
    config,
    live_pairing_handoff::{BearerLivePairingHandoff, LivePairingCommand, LivePairingReply},
    pairing_control_handoff::{
        ButtonObservationReply, ExclusiveAcquisitionReply, LifecycleAcknowledgement,
        PairingControlCommand, PairingControlReply, PairingControlReplyKind, UsbPairingHandoff,
    },
    session_admission_handoff::BearerSessionAdmissionHandoff,
    usb_authenticated_session::{
        PairingExclusiveCloseDisposition, UsbAuthenticatedSession, UsbAuthenticatedSessionPhase,
        UsbSessionRxDisposition, UsbSessionTxAdvance,
    },
    usb_pairing_policy::{
        ActiveLowButtonDebouncer, ExactNextSequenceGate, PhysicalPresencePublicationGuard,
        UsbConnectionEvent, UsbConnectionTracker, UsbPairingWork, select_usb_pairing_work,
    },
    usb_pairing_records::{
        UsbPreAuthenticationRequest, UsbPreAuthenticationRequestKind,
        decode_usb_pre_authentication_request,
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
        kind: UsbPreAuthenticationRequestKind,
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
    reset_generation: u32,
}

#[derive(Clone, Copy)]
struct LivePurpose {
    connection: ConnectionId,
    sequence: u64,
    kind: UsbPreAuthenticationRequestKind,
}

struct PendingLiveCommand {
    command: LivePairingCommand,
    purpose: LivePurpose,
    reset_generation: u32,
}

struct PendingTransmission {
    connection: ConnectionId,
    reset_generation: u32,
    frame: FramedRecord,
}

struct ControlReplyContext<'a> {
    pending_command: &'a mut Option<PendingCommand>,
    transmission: &'a mut Option<PendingTransmission>,
    authenticated_session: &'a mut UsbAuthenticatedSession,
    pending_pairing_exclusive: &'a mut Option<(ConnectionId, u32)>,
    announced_connection: &'a mut Option<ConnectionId>,
    disconnect_pending: &'a mut Option<ConnectionId>,
    reset_generation: u32,
}

struct PreAuthenticationRxContext<'a> {
    decoder: &'a mut StreamDecoder,
    sequence_gate: Option<&'a mut ExactNextSequenceGate>,
    pending_command: &'a mut Option<PendingCommand>,
    pending_live_command: &'a mut Option<PendingLiveCommand>,
    connection: ConnectionId,
    now_millis: u64,
    reset_generation: u32,
}

pub(crate) struct UsbHandoffs {
    pairing: UsbPairingHandoff<CriticalSectionRawMutex>,
    live: BearerLivePairingHandoff<CriticalSectionRawMutex>,
    admission: BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
}

impl UsbHandoffs {
    pub(crate) const fn new(
        pairing: UsbPairingHandoff<CriticalSectionRawMutex>,
        live: BearerLivePairingHandoff<CriticalSectionRawMutex>,
        admission: BearerSessionAdmissionHandoff<CriticalSectionRawMutex>,
        authenticated_api: BearerHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    ) -> Self {
        Self {
            pairing,
            live,
            admission,
            authenticated_api,
        }
    }
}

// A bus reset is a security-principal boundary for pre-authentication work.
// The interrupt guard prevents thread-mode code from placing an old-epoch
// frame into the hardware FIFO after reset but before the next task poll.
static USB_RESET_GENERATION: AtomicU32 = AtomicU32::new(0);
static USB_RESET_EXHAUSTED: AtomicBool = AtomicBool::new(false);
static TX_EPOCH_ARMED: AtomicBool = AtomicBool::new(false);
static USB_PAD_FORCED_OFF: AtomicBool = AtomicBool::new(false);
static USB_EPOCH_BLOCKED: AtomicBool = AtomicBool::new(false);
static USB_REATTACH_EXPECTED: AtomicBool = AtomicBool::new(false);
static USB_CLEAN_RESET_GENERATION: AtomicU32 = AtomicU32::new(0);
static USB_ATTACHED_PAD_CONFIGURATION: AtomicU32 = AtomicU32::new(0);

const USB_MEMORY_SCRUB_MICROS: u32 = 10;
const USB_REATTACH_DWELL_MILLIS: u64 = 100;
const USB_PAD_PULL_OVERRIDE_BIT: u32 = 1 << 8;
const USB_DP_PULLUP_BIT: u32 = 1 << 9;
const USB_DP_PULLDOWN_BIT: u32 = 1 << 10;
const USB_DM_PULLUP_BIT: u32 = 1 << 11;
const USB_DM_PULLDOWN_BIT: u32 = 1 << 12;
const USB_PAD_ENABLE_BIT: u32 = 1 << 14;
const USB_CANONICAL_ATTACHED_PAD_CONFIGURATION: u32 = USB_DP_PULLUP_BIT | USB_PAD_ENABLE_BIT;

/// Proof that USB endpoint RAM was scrubbed and its pad was detached before
/// the product executor or HAL initialization began.
///
/// The private field makes the earliest synchronous entrypoint the sole token
/// constructor. The token is then moved through product composition into the
/// USB owner, so the bearer cannot be spawned without the quarantine step.
pub(crate) struct BootUsbQuarantine {
    _private: (),
}

/// Detach and scrub USB at the earliest Rust-controlled boot boundary.
///
/// This runs before `esp_hal::init` and before the RTOS executor is created.
/// The pad remains disabled after this function returns. The USB task restores
/// it only after installing the reset ISR, and it keeps the logical epoch
/// blocked until the host supplies a clean enumeration reset.
#[inline(always)]
pub(crate) fn quarantine_usb_at_boot() -> BootUsbQuarantine {
    let registers = USB_DEVICE::regs();

    // Stop a retained fixed-function endpoint before any fallible or lengthy
    // application initialization can run. Do not trust inherited pull-up bits:
    // an earlier reset ISR may itself have left this register detached.
    registers.conf0().modify(|_, write| {
        write
            .pad_pull_override()
            .set_bit()
            .dp_pullup()
            .clear_bit()
            .dp_pulldown()
            .clear_bit()
            .dm_pullup()
            .clear_bit()
            .dm_pulldown()
            .clear_bit()
            .usb_pad_enable()
            .clear_bit()
    });
    registers
        .int_ena()
        .modify(|_, write| write.usb_bus_reset().clear_bit());

    USB_EPOCH_BLOCKED.store(true, Ordering::Release);
    USB_RESET_GENERATION.store(0, Ordering::Release);
    USB_RESET_EXHAUSTED.store(false, Ordering::Release);
    TX_EPOCH_ARMED.store(false, Ordering::Release);
    USB_PAD_FORCED_OFF.store(false, Ordering::Release);
    USB_REATTACH_EXPECTED.store(false, Ordering::Release);
    USB_CLEAN_RESET_GENERATION.store(0, Ordering::Release);
    USB_ATTACHED_PAD_CONFIGURATION.store(0, Ordering::Release);

    // Consume every inherited endpoint/reset state as tainted. Power cycling
    // the controller memory retires a WR_DONE packet that survived SRST; only
    // after that physical scrub is it safe to clear inherited status bits.
    registers
        .mem_conf()
        .modify(|_, write| write.usb_mem_pd().set_bit());
    esp_hal::rom::ets_delay_us(USB_MEMORY_SCRUB_MICROS);
    registers
        .mem_conf()
        .modify(|_, write| write.usb_mem_pd().clear_bit());
    esp_hal::rom::ets_delay_us(USB_MEMORY_SCRUB_MICROS);
    registers.int_clr().write(|write| {
        write
            .sof()
            .clear_bit_by_one()
            .usb_bus_reset()
            .clear_bit_by_one()
    });

    BootUsbQuarantine { _private: () }
}

#[esp_hal::handler]
#[ram]
fn usb_bus_reset_interrupt() {
    let registers = USB_DEVICE::regs();
    if !registers.int_st().read().usb_bus_reset().bit_is_set() {
        return;
    }

    let next_generation =
        USB_RESET_GENERATION.fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
            generation.checked_add(1)
        });
    let reset_exhausted = next_generation.is_err();
    if reset_exhausted {
        USB_RESET_EXHAUSTED.store(true, Ordering::Release);
    }
    USB_EPOCH_BLOCKED.store(true, Ordering::Release);
    let expected_clean_reset = USB_REATTACH_EXPECTED.swap(false, Ordering::AcqRel)
        && !TX_EPOCH_ARMED.load(Ordering::Acquire)
        && !reset_exhausted;
    if expected_clean_reset {
        if let Ok(previous_generation) = next_generation {
            USB_CLEAN_RESET_GENERATION
                .store(previous_generation.wrapping_add(1), Ordering::Release);
        }
    } else {
        // Stop the physical link before returning to thread mode. No
        // replacement epoch can exchange bytes while the task invalidates
        // owners, scrubs USB RAM, and performs a detectable reattachment.
        registers.conf0().modify(|_, write| {
            write
                .pad_pull_override()
                .set_bit()
                .dp_pullup()
                .clear_bit()
                .dp_pulldown()
                .clear_bit()
                .dm_pullup()
                .clear_bit()
                .dm_pulldown()
                .clear_bit()
                .usb_pad_enable()
                .clear_bit()
        });
        USB_PAD_FORCED_OFF.store(true, Ordering::Release);
    }
    registers
        .int_clr()
        .write(|write| write.usb_bus_reset().clear_bit_by_one());
}

/// Run the sole USB/GPIO bearer and boot-lifetime authenticated-session owner.
#[embassy_executor::task]
pub async fn run(
    _boot_quarantine: BootUsbQuarantine,
    usb_device: USB_DEVICE<'static>,
    button: Input<'static>,
    handoffs: UsbHandoffs,
    session_parameters: ServerParameters,
    mut session_rng: Trng,
) {
    let UsbHandoffs {
        pairing: mut handoff,
        live: mut live_handoff,
        admission: mut session_admission,
        mut authenticated_api,
    } = handoffs;
    let mut usb_serial = UsbSerialJtag::<Blocking>::new(usb_device);
    let registers = USB_DEVICE::regs();
    let pin_swap = esp_hal::efuse::read_bit(esp_hal::efuse::USB_EXCHG_PINS);
    USB_ATTACHED_PAD_CONFIGURATION.store(
        USB_CANONICAL_ATTACHED_PAD_CONFIGURATION
            | if pin_swap {
                USB_PAD_PULL_OVERRIDE_BIT
            } else {
                0
            },
        Ordering::Release,
    );
    registers.int_clr().write(|write| {
        write
            .sof()
            .clear_bit_by_one()
            .usb_bus_reset()
            .clear_bit_by_one()
    });
    usb_serial.set_interrupt_handler(usb_bus_reset_interrupt);
    registers
        .int_ena()
        .modify(|_, write| write.usb_bus_reset().set_bit());
    let (mut rx, mut tx) = usb_serial.split();

    // The earliest entrypoint has already scrubbed endpoint RAM and held the
    // pad detached throughout product initialization. Keep it absent for one
    // explicit host-visible dwell after the ISR becomes authoritative, then
    // reuse the runtime reattach path. No RX/TX is admitted until its expected
    // clean enumeration reset advances the generation and unblocks the epoch.
    Timer::after(Duration::from_millis(USB_REATTACH_DWELL_MILLIS)).await;

    let started = Instant::now().as_millis();
    let raw_button = active_low_level(button.is_low());
    let mut debouncer = ActiveLowButtonDebouncer::new(started, raw_button);
    let mut button_publication = PhysicalPresencePublicationGuard::new();
    let mut connection_tracker = UsbConnectionTracker::new(started);
    let mut decoder = StreamDecoder::new();
    let mut sequence_gate: Option<ExactNextSequenceGate> = None;
    let mut pending_command: Option<PendingCommand> = None;
    let mut awaiting_reply: Option<PendingPurpose> = None;
    let mut pending_live_command: Option<PendingLiveCommand> = None;
    let mut awaiting_live_reply: Option<LivePurpose> = None;
    let mut transmission: Option<PendingTransmission> = None;
    let mut authenticated_session = UsbAuthenticatedSession::new(session_parameters);
    let mut pending_pairing_exclusive: Option<(ConnectionId, u32)> = None;
    let mut announced_connection: Option<ConnectionId> = None;
    let mut disconnect_pending: Option<ConnectionId> = None;
    let mut tracker_failed = false;
    let mut button_failed = false;
    let mut control_turn_after_button = false;
    let mut last_button_observation_ms =
        started.saturating_sub(config::PAIRING_BUTTON_OBSERVATION_INTERVAL_MS);
    let mut observed_reset_generation = USB_RESET_GENERATION.load(Ordering::Acquire);
    let mut pad_reenable_pending = true;
    let mut boot_reattach_pending = true;

    loop {
        let now_millis = Instant::now().as_millis();
        let raw = registers.int_raw().read();
        let saw_sof = raw.sof().bit_is_set();
        let reset_generation = USB_RESET_GENERATION.load(Ordering::Acquire);
        let reset_exhausted = USB_RESET_EXHAUSTED.load(Ordering::Acquire);
        let epoch_blocked = USB_EPOCH_BLOCKED.load(Ordering::Acquire);
        let saw_bus_reset = reset_generation != observed_reset_generation || reset_exhausted;
        let mut consumed_bus_reset = false;
        if saw_sof {
            registers
                .int_clr()
                .write(|write| write.sof().clear_bit_by_one());
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
        if !tracker_failed && (disconnect_pending.is_none() || saw_bus_reset) {
            let previous_connection = connection_tracker.connection();
            let eligible_sof = saw_sof && disconnect_pending.is_none() && !epoch_blocked;
            match connection_tracker.observe(now_millis, eligible_sof, saw_bus_reset) {
                Ok(UsbConnectionEvent::None) => {}
                Ok(UsbConnectionEvent::Connected(connection)) => {
                    discard_rx_fifo(&mut rx);
                    decoder.reset();
                    sequence_gate = Some(ExactNextSequenceGate::new(connection));
                    debug_assert_eq!(
                        authenticated_session.phase(),
                        UsbAuthenticatedSessionPhase::Disconnected
                    );
                    if !authenticated_session.begin_connection(connection) {
                        tracker_failed = true;
                        authenticated_session.reset();
                        discard_rx_fifo(&mut rx);
                        continue;
                    }
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
                        reset_generation,
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
                        &mut pending_live_command,
                        &mut transmission,
                        &mut authenticated_session,
                        &mut pending_pairing_exclusive,
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
                            &mut pending_live_command,
                            &mut transmission,
                            &mut authenticated_session,
                            &mut pending_pairing_exclusive,
                            announced_connection,
                            &mut disconnect_pending,
                        );
                    }
                }
            }
            if saw_bus_reset {
                observed_reset_generation = reset_generation;
                consumed_bus_reset = true;
            }
        }

        if USB_PAD_FORCED_OFF.swap(false, Ordering::AcqRel) {
            retire_transmission(&mut transmission);
            authenticated_session.reset();
            pending_pairing_exclusive = None;
            synchronize_tx_epoch(&transmission, &authenticated_session);
            // The pad is already disabled by the reset ISR. Power-cycle the
            // USB memory long enough to discard any byte written after the
            // hardware reset, then expose a clean endpoint only if generation
            // tracking remains usable.
            registers
                .mem_conf()
                .modify(|_, write| write.usb_mem_pd().set_bit());
            Timer::after(Duration::from_micros(u64::from(USB_MEMORY_SCRUB_MICROS))).await;
            registers
                .mem_conf()
                .modify(|_, write| write.usb_mem_pd().clear_bit());
            Timer::after(Duration::from_micros(u64::from(USB_MEMORY_SCRUB_MICROS))).await;
            // Keep the pull-up absent long enough for the host controller and
            // class driver to observe a real detach. The later automatic bus
            // reset is the only event allowed to reopen the epoch.
            Timer::after(Duration::from_millis(USB_REATTACH_DWELL_MILLIS)).await;
            pad_reenable_pending = true;
        }
        if pad_reenable_pending
            && (consumed_bus_reset || boot_reattach_pending)
            && USB_RESET_GENERATION.load(Ordering::Acquire) == reset_generation
            && !tracker_failed
            && !USB_RESET_EXHAUSTED.load(Ordering::Acquire)
        {
            // Reattach only after the tracker consumed this exact generation.
            // Arm the clean-reset marker and expose the scrubbed pad in one
            // interrupt-masked section. Before it, the pad is detached; after
            // pad enable, the host's enumeration reset names this endpoint.
            let reattached = critical_section::with(|_| {
                if USB_RESET_GENERATION.load(Ordering::Acquire) != reset_generation
                    || registers.int_raw().read().usb_bus_reset().bit_is_set()
                {
                    return false;
                }
                USB_REATTACH_EXPECTED.store(true, Ordering::Release);
                let attached = USB_ATTACHED_PAD_CONFIGURATION.load(Ordering::Acquire);
                registers.conf0().modify(|_, write| {
                    write
                        .pad_pull_override()
                        .bit(attached & USB_PAD_PULL_OVERRIDE_BIT != 0)
                        .dp_pullup()
                        .bit(attached & USB_DP_PULLUP_BIT != 0)
                        .dp_pulldown()
                        .bit(attached & USB_DP_PULLDOWN_BIT != 0)
                        .dm_pullup()
                        .bit(attached & USB_DM_PULLUP_BIT != 0)
                        .dm_pulldown()
                        .bit(attached & USB_DM_PULLDOWN_BIT != 0)
                        .usb_pad_enable()
                        .bit(attached & USB_PAD_ENABLE_BIT != 0)
                });
                true
            });
            if reattached {
                let post_enable_generation = USB_RESET_GENERATION.load(Ordering::Acquire);
                let raced_clean_reset = post_enable_generation != reset_generation
                    && USB_CLEAN_RESET_GENERATION.load(Ordering::Acquire) == post_enable_generation;
                if USB_RESET_EXHAUSTED.load(Ordering::Acquire)
                    || (post_enable_generation != reset_generation && !raced_clean_reset)
                {
                    USB_REATTACH_EXPECTED.store(false, Ordering::Release);
                    registers.conf0().modify(|_, write| {
                        write
                            .pad_pull_override()
                            .set_bit()
                            .dp_pullup()
                            .clear_bit()
                            .dp_pulldown()
                            .clear_bit()
                            .dm_pullup()
                            .clear_bit()
                            .dm_pulldown()
                            .clear_bit()
                            .usb_pad_enable()
                            .clear_bit()
                    });
                    USB_PAD_FORCED_OFF.store(true, Ordering::Release);
                } else {
                    pad_reenable_pending = false;
                    boot_reattach_pending = false;
                }
            } else {
                USB_REATTACH_EXPECTED.store(false, Ordering::Release);
                registers.conf0().modify(|_, write| {
                    write
                        .pad_pull_override()
                        .set_bit()
                        .dp_pullup()
                        .clear_bit()
                        .dp_pulldown()
                        .clear_bit()
                        .dm_pullup()
                        .clear_bit()
                        .dm_pulldown()
                        .clear_bit()
                        .usb_pad_enable()
                        .clear_bit()
                });
                USB_PAD_FORCED_OFF.store(true, Ordering::Release);
            }
        }

        if consumed_bus_reset
            && USB_CLEAN_RESET_GENERATION.load(Ordering::Acquire) == reset_generation
            && USB_RESET_GENERATION.load(Ordering::Acquire) == reset_generation
            && !tracker_failed
            && !USB_RESET_EXHAUSTED.load(Ordering::Acquire)
            && !pad_reenable_pending
            && !USB_REATTACH_EXPECTED.load(Ordering::Acquire)
        {
            // No task-mode RX/TX occurs between reattach and this exact clean
            // reset. Drain bytes that raced enumeration, then linearize
            // unblocking against a pending replacement reset.
            discard_rx_fifo(&mut rx);
            decoder.reset();
            critical_section::with(|_| {
                if USB_RESET_GENERATION.load(Ordering::Acquire) != reset_generation
                    || USB_RESET_EXHAUSTED.load(Ordering::Acquire)
                    || registers.int_raw().read().usb_bus_reset().bit_is_set()
                {
                    return;
                }
                USB_EPOCH_BLOCKED.store(false, Ordering::Release);
            });
        }

        if connection_tracker.connection().is_none()
            || disconnect_pending.is_some()
            || matches!(
                authenticated_session.phase(),
                UsbAuthenticatedSessionPhase::Disconnected
                    | UsbAuthenticatedSessionPhase::TerminatedUntilReset
            )
        {
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
                    ControlReplyContext {
                        pending_command: &mut pending_command,
                        transmission: &mut transmission,
                        authenticated_session: &mut authenticated_session,
                        pending_pairing_exclusive: &mut pending_pairing_exclusive,
                        announced_connection: &mut announced_connection,
                        disconnect_pending: &mut disconnect_pending,
                        reset_generation,
                    },
                );
                if button_acknowledged && disconnect_pending.is_none() {
                    control_turn_after_button = true;
                }
                break;
            }
        }

        if let Some(purpose) = awaiting_live_reply
            && let Some(reply) = live_handoff.try_receive_reply()
        {
            if reply.connection() != purpose.connection {
                awaiting_live_reply = None;
                if connection_tracker.connection() == Some(purpose.connection) {
                    disconnect_pending = Some(purpose.connection);
                    retire_transmission(&mut transmission);
                }
            } else {
                awaiting_live_reply = None;
                handle_live_reply(
                    reply,
                    purpose,
                    connection_tracker.connection(),
                    &mut transmission,
                    &mut disconnect_pending,
                    reset_generation,
                );
            }
        }

        while let Some(reply) = session_admission.try_receive_reply() {
            critical_section::with(|_| {
                let _ = authenticated_session.accept_admission_reply(reply, &mut session_rng);
                synchronize_tx_epoch(&transmission, &authenticated_session);
            });
        }
        while let Some(reply) = authenticated_api.replies().try_receive() {
            critical_section::with(|_| {
                if let Err(fault) = authenticated_session.accept_node_reply(reply) {
                    drop(fault.into_reply());
                }
                synchronize_tx_epoch(&transmission, &authenticated_session);
            });
        }

        if transmission.is_some() && authenticated_session.tx_kind().is_some() {
            if let Some(connection) = connection_tracker.connection() {
                disconnect_pending = Some(connection);
            }
            retire_transmission(&mut transmission);
            authenticated_session.reset();
            pending_pairing_exclusive = None;
        }
        synchronize_tx_epoch(&transmission, &authenticated_session);

        if let Some(active_connection) = connection_tracker.active() {
            if let Some(pending_tx) = transmission.as_mut() {
                if pending_tx.connection != active_connection
                    || step_transmission(&mut tx, pending_tx)
                {
                    retire_transmission(&mut transmission);
                }
            } else if authenticated_session.tx_kind().is_some()
                && (authenticated_session.connection() != Some(active_connection)
                    || step_authenticated_transmission(
                        &mut tx,
                        &mut authenticated_session,
                        reset_generation,
                    ))
            {
                disconnect_pending = Some(active_connection);
                authenticated_session.reset();
            }
            synchronize_tx_epoch(&transmission, &authenticated_session);
        }

        if let Some((connection, exclusive_generation)) = pending_pairing_exclusive {
            if connection_tracker.connection() != Some(connection)
                || exclusive_generation != reset_generation
                || !usb_epoch_current(reset_generation)
            {
                pending_pairing_exclusive = None;
                if authenticated_session.connection() == Some(connection) {
                    authenticated_session.reset();
                }
                synchronize_tx_epoch(&transmission, &authenticated_session);
            } else {
                match authenticated_session.close_for_pairing_exclusivity(connection) {
                    PairingExclusiveCloseDisposition::Closed
                    | PairingExclusiveCloseDisposition::AlreadyExclusive => {
                        pending_pairing_exclusive = None;
                        synchronize_tx_epoch(&transmission, &authenticated_session);
                        queue_exclusive_acquired(
                            &mut pending_command,
                            now_millis,
                            connection,
                            reset_generation,
                        );
                    }
                    PairingExclusiveCloseDisposition::DrainBeforeClose { .. } => {}
                    PairingExclusiveCloseDisposition::StaleConnection => {
                        pending_pairing_exclusive = None;
                        disconnect_pending = Some(connection);
                        authenticated_session.reset();
                        synchronize_tx_epoch(&transmission, &authenticated_session);
                    }
                }
            }
        }

        if awaiting_reply.is_none() && pending_command.is_none() && pending_live_command.is_none() {
            if let Some(connection) = disconnect_pending {
                if announced_connection == Some(connection) {
                    queue_command(
                        &mut pending_command,
                        PairingControlCommand::Disconnected {
                            at: pairing_time(now_millis),
                            connection,
                        },
                        PendingPurpose::Disconnected(connection),
                        reset_generation,
                    );
                } else {
                    disconnect_pending = None;
                }
            } else if let Some(connection) = announced_connection
                && connection_tracker.connection() == Some(connection)
            {
                let periodic_button_due = now_millis.saturating_sub(last_button_observation_ms)
                    >= config::PAIRING_BUTTON_OBSERVATION_INTERVAL_MS;
                let button_due = pending_pairing_exclusive.is_none()
                    && !button_failed
                    && button_publication.publication_due(periodic_button_due);
                // Both request families share one wire flight. Scalar button
                // and lifecycle events remain independent so a disconnect can
                // cross the control handoff while the node still owns a live
                // mutation or its eventual reply.
                let control_ready = connection_tracker.active() == Some(connection)
                    && awaiting_live_reply.is_none()
                    && transmission.is_none()
                    && pending_pairing_exclusive.is_none()
                    && authenticated_session.tx_kind().is_none()
                    && authenticated_rx_enabled(authenticated_session.phase())
                    && usb_epoch_current(reset_generation);
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
                            reset_generation,
                        );
                    }
                    UsbPairingWork::PollControl {
                        observe_button_if_empty,
                    } => {
                        control_turn_after_button = false;
                        let accepted = receive_usb_request(
                            &mut rx,
                            PreAuthenticationRxContext {
                                decoder: &mut decoder,
                                sequence_gate: sequence_gate.as_mut(),
                                pending_command: &mut pending_command,
                                pending_live_command: &mut pending_live_command,
                                connection,
                                now_millis,
                                reset_generation,
                            },
                            &mut authenticated_session,
                        );
                        if observe_button_if_empty && !accepted {
                            queue_button_observation(
                                &mut pending_command,
                                &mut last_button_observation_ms,
                                &mut button_publication,
                                now_millis,
                                connection,
                                debouncer.current(),
                                reset_generation,
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
            let PendingCommand {
                command,
                purpose,
                reset_generation,
            } = pending;
            match try_send_in_epoch(reset_generation, command, |command| {
                handoff.try_send_command(command)
            }) {
                Some(Ok(())) => {
                    if matches!(purpose, PendingPurpose::Connected(_)) {
                        announced_connection = Some(purpose.connection());
                    }
                    awaiting_reply = Some(purpose);
                }
                Some(Err(pressure)) => {
                    pending_command = Some(PendingCommand {
                        command: pressure.into_inner(),
                        purpose,
                        reset_generation,
                    });
                }
                None => {}
            }
        }

        if awaiting_live_reply.is_none()
            && let Some(pending) = pending_live_command.take()
        {
            let PendingLiveCommand {
                command,
                purpose,
                reset_generation,
            } = pending;
            match try_send_in_epoch(reset_generation, command, |command| {
                live_handoff.try_send_command(command)
            }) {
                Some(Ok(())) => awaiting_live_reply = Some(purpose),
                Some(Err(pressure)) => {
                    pending_live_command = Some(PendingLiveCommand {
                        command: pressure.into_inner(),
                        purpose,
                        reset_generation,
                    });
                }
                None => {}
            }
        }

        if pending_pairing_exclusive.is_none()
            && disconnect_pending.is_none()
            && let Some(connection) = announced_connection
            && connection_tracker.connection() == Some(connection)
        {
            let epoch_current = try_in_epoch(reset_generation, || {
                authenticated_session.try_send_admission_command(&mut session_admission)
            })
            .is_some()
                && try_in_epoch(reset_generation, || {
                    authenticated_session.try_send_request(authenticated_api.requests())
                })
                .is_some();
            if !epoch_current {
                authenticated_session.reset();
                synchronize_tx_epoch(&transmission, &authenticated_session);
            }
        }

        Timer::after_millis(config::USB_PAIRING_POLL_INTERVAL_MS).await;
    }
}

fn receive_usb_request(
    rx: &mut esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, Blocking>,
    mut context: PreAuthenticationRxContext<'_>,
    authenticated_session: &mut UsbAuthenticatedSession,
) -> bool {
    let Some(event) = receive_decode_event(rx, context.decoder, context.reset_generation) else {
        return false;
    };

    match authenticated_session.phase() {
        UsbAuthenticatedSessionPhase::AwaitingClientHello => match event {
            DecodeEvent::Record(record) if record.kind() == RECORD_KIND_CLIENT_HELLO => {
                let _ =
                    authenticated_session.accept_record(record, pairing_time(context.now_millis));
                true
            }
            DecodeEvent::Record(record) => handle_pre_authentication_record(record, &mut context),
            DecodeEvent::Pending
            | DecodeEvent::MalformedCobs
            | DecodeEvent::MalformedRecord(_)
            | DecodeEvent::Overflow => false,
        },
        UsbAuthenticatedSessionPhase::PairingExclusive => match event {
            DecodeEvent::Record(record) => handle_pre_authentication_record(record, &mut context),
            DecodeEvent::Pending
            | DecodeEvent::MalformedCobs
            | DecodeEvent::MalformedRecord(_)
            | DecodeEvent::Overflow => false,
        },
        UsbAuthenticatedSessionPhase::PendingClientProof => {
            let result =
                authenticated_session.accept_decode_event(event, pairing_time(context.now_millis));
            !matches!(result, Ok(UsbSessionRxDisposition::Pending))
        }
        UsbAuthenticatedSessionPhase::Established => {
            // An idle session remains readable so a freshly opened host can
            // replace it with a canonical ClientHello on this enumeration.
            let result =
                authenticated_session.accept_decode_event(event, pairing_time(context.now_millis));
            !matches!(result, Ok(UsbSessionRxDisposition::Pending))
        }
        UsbAuthenticatedSessionPhase::Disconnected
        | UsbAuthenticatedSessionPhase::AdmissionCommandPending
        | UsbAuthenticatedSessionPhase::AwaitingAdmissionReply
        | UsbAuthenticatedSessionPhase::ServerHelloFlight
        | UsbAuthenticatedSessionPhase::ServerProofFlight
        | UsbAuthenticatedSessionPhase::RequestHandoffPending
        | UsbAuthenticatedSessionPhase::AwaitingReply
        | UsbAuthenticatedSessionPhase::ReplyFlight
        | UsbAuthenticatedSessionPhase::TerminatedUntilReset => false,
    }
}

fn receive_decode_event(
    rx: &mut esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, Blocking>,
    decoder: &mut StreamDecoder,
    reset_generation: u32,
) -> Option<DecodeEvent> {
    for _ in 0..config::USB_PAIRING_MAX_BYTES_PER_POLL {
        if !usb_epoch_current(reset_generation) {
            decoder.reset();
            return None;
        }
        let Ok(byte) = rx.read_byte() else {
            break;
        };
        if !usb_epoch_current(reset_generation) {
            decoder.reset();
            return None;
        }
        let event = decoder.push(byte);
        if !matches!(event, DecodeEvent::Pending) {
            return Some(event);
        }
    }
    None
}

fn handle_pre_authentication_record(
    record: Record,
    context: &mut PreAuthenticationRxContext<'_>,
) -> bool {
    let Some(sequence_gate) = context.sequence_gate.as_deref_mut() else {
        return false;
    };
    let Ok(request) = decode_usb_pre_authentication_request(record) else {
        return false;
    };
    let sequence = request.sequence();
    let kind = request.kind();
    if !usb_epoch_current(context.reset_generation)
        || sequence_gate.accept(context.connection, sequence).is_err()
        || !usb_epoch_current(context.reset_generation)
    {
        context.decoder.reset();
        return false;
    }
    match request {
        UsbPreAuthenticationRequest::Control(request) => queue_command(
            context.pending_command,
            PairingControlCommand::Control {
                at: pairing_time(context.now_millis),
                connection: context.connection,
                request,
            },
            PendingPurpose::Control {
                connection: context.connection,
                sequence,
                kind,
            },
            context.reset_generation,
        ),
        UsbPreAuthenticationRequest::Live(request) => {
            debug_assert!(context.pending_live_command.is_none());
            let purpose = LivePurpose {
                connection: context.connection,
                sequence,
                kind,
            };
            *context.pending_live_command = Some(PendingLiveCommand {
                command: LivePairingCommand::new(
                    pairing_time(context.now_millis),
                    context.connection,
                    request,
                ),
                purpose,
                reset_generation: context.reset_generation,
            });
        }
    }
    true
}

fn handle_reply(
    reply: PairingControlReply,
    purpose: PendingPurpose,
    now_millis: u64,
    context: ControlReplyContext<'_>,
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
            if *context.announced_connection == Some(connection) {
                *context.announced_connection = None;
            }
            if *context.disconnect_pending == Some(connection) {
                *context.disconnect_pending = None;
            }
        }
        (
            PendingPurpose::Button(expected),
            PairingControlReplyKind::Button(ButtonObservationReply::AcquireExclusive),
        ) if expected == connection && context.disconnect_pending.is_none() => {
            match context
                .authenticated_session
                .close_for_pairing_exclusivity(connection)
            {
                PairingExclusiveCloseDisposition::Closed
                | PairingExclusiveCloseDisposition::AlreadyExclusive => {
                    synchronize_tx_epoch(context.transmission, context.authenticated_session);
                    queue_exclusive_acquired(
                        context.pending_command,
                        now_millis,
                        connection,
                        context.reset_generation,
                    );
                }
                PairingExclusiveCloseDisposition::DrainBeforeClose { .. } => {
                    debug_assert!(context.pending_pairing_exclusive.is_none());
                    *context.pending_pairing_exclusive =
                        Some((connection, context.reset_generation));
                }
                PairingExclusiveCloseDisposition::StaleConnection => {
                    *context.disconnect_pending = Some(connection);
                    retire_transmission(context.transmission);
                    *context.pending_pairing_exclusive = None;
                    synchronize_tx_epoch(context.transmission, context.authenticated_session);
                }
            }
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
                kind,
            },
            PairingControlReplyKind::Control(response),
        ) if expected == connection
            && response.sequence() == sequence
            && matches_control_response(kind, &response) =>
        {
            if context.disconnect_pending.is_none()
                && !queue_transmission(
                    connection,
                    response.into_record(),
                    context.reset_generation,
                    context.transmission,
                )
            {
                *context.disconnect_pending = Some(connection);
                retire_transmission(context.transmission);
            }
        }
        _ => {
            // A same-epoch causal mismatch is an internal ownership fault.
            // Retire the connection instead of accepting another command.
            *context.disconnect_pending = Some(connection);
            retire_transmission(context.transmission);
        }
    }
}

fn handle_live_reply(
    reply: LivePairingReply,
    purpose: LivePurpose,
    allocated_connection: Option<ConnectionId>,
    transmission: &mut Option<PendingTransmission>,
    disconnect_pending: &mut Option<ConnectionId>,
    reset_generation: u32,
) {
    let connection = purpose.connection;
    let response = reply.into_response();
    if response.sequence() != purpose.sequence || !purpose.kind.matches_live_response(&response) {
        if allocated_connection == Some(connection) {
            *disconnect_pending = Some(connection);
            retire_transmission(transmission);
        }
        return;
    }
    if allocated_connection != Some(connection) || disconnect_pending.is_some() {
        return;
    }
    if !queue_transmission(
        connection,
        response.into_record(),
        reset_generation,
        transmission,
    ) {
        *disconnect_pending = Some(connection);
        retire_transmission(transmission);
    }
}

const fn matches_control_response(
    kind: UsbPreAuthenticationRequestKind,
    response: &ControlResponse,
) -> bool {
    matches!(
        (kind, response),
        (
            UsbPreAuthenticationRequestKind::Status,
            ControlResponse::Status { .. }
        ) | (
            UsbPreAuthenticationRequestKind::Initialize,
            ControlResponse::Initialize { .. }
        )
    )
}

fn queue_transmission(
    connection: ConnectionId,
    record: Record,
    reset_generation: u32,
    transmission: &mut Option<PendingTransmission>,
) -> bool {
    if transmission.is_some() || !tx_epoch_current(reset_generation) {
        return false;
    }
    TX_EPOCH_ARMED.store(true, Ordering::Release);
    if !tx_epoch_current(reset_generation) {
        TX_EPOCH_ARMED.store(false, Ordering::Release);
        return false;
    }
    let Ok(frame) = FramedRecord::encode(&record) else {
        TX_EPOCH_ARMED.store(false, Ordering::Release);
        return false;
    };
    if !tx_epoch_current(reset_generation) {
        TX_EPOCH_ARMED.store(false, Ordering::Release);
        drop(frame);
        return false;
    }
    *transmission = Some(PendingTransmission {
        connection,
        reset_generation,
        frame,
    });
    true
}

fn step_transmission(
    tx: &mut UsbSerialJtagTx<'static, Blocking>,
    pending: &mut PendingTransmission,
) -> bool {
    if !tx_epoch_current(pending.reset_generation) {
        return true;
    }
    let mut acknowledged = 0_usize;
    for byte in pending
        .frame
        .next_chunk(config::USB_PAIRING_MAX_BYTES_PER_POLL)
    {
        if !tx_epoch_current(pending.reset_generation) {
            return true;
        }
        if tx.write_byte_nb(*byte).is_err() {
            break;
        }
        acknowledged += 1;
        if !tx_epoch_current(pending.reset_generation) {
            return true;
        }
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
    if !tx_epoch_current(pending.reset_generation) {
        return true;
    }
    let _ = tx.flush_tx_nb();
    if !tx_epoch_current(pending.reset_generation) {
        return true;
    }
    true
}

fn step_authenticated_transmission(
    tx: &mut UsbSerialJtagTx<'static, Blocking>,
    session: &mut UsbAuthenticatedSession,
    reset_generation: u32,
) -> bool {
    if !tx_epoch_current(reset_generation) {
        return false;
    }
    let Some(chunk) = session.next_tx_chunk(config::USB_PAIRING_MAX_BYTES_PER_POLL) else {
        return true;
    };
    let mut acknowledged = 0_usize;
    for byte in chunk {
        if !tx_epoch_current(reset_generation) {
            return false;
        }
        if tx.write_byte_nb(*byte).is_err() {
            break;
        }
        acknowledged += 1;
        if !tx_epoch_current(reset_generation) {
            return false;
        }
    }
    let advance = match session.advance_tx(acknowledged) {
        Ok(advance) => advance,
        Err(_) => return true,
    };
    if matches!(advance, UsbSessionTxAdvance::RecordComplete { .. }) {
        if !tx_epoch_current(reset_generation) {
            return false;
        }
        let _ = tx.flush_tx_nb();
        if !tx_epoch_current(reset_generation) {
            return false;
        }
    }
    false
}

fn synchronize_tx_epoch(
    transmission: &Option<PendingTransmission>,
    session: &UsbAuthenticatedSession,
) {
    debug_assert!(!(transmission.is_some() && session.tx_kind().is_some()));
    TX_EPOCH_ARMED.store(
        transmission.is_some() || session.tx_kind().is_some(),
        Ordering::Release,
    );
}

const fn authenticated_rx_enabled(phase: UsbAuthenticatedSessionPhase) -> bool {
    // In-flight request/reply phases are deliberately absent. A replacement
    // hello cannot be consumed until the exact authenticated owner completes.
    matches!(
        phase,
        UsbAuthenticatedSessionPhase::AwaitingClientHello
            | UsbAuthenticatedSessionPhase::PendingClientProof
            | UsbAuthenticatedSessionPhase::Established
            | UsbAuthenticatedSessionPhase::PairingExclusive
    )
}

fn tx_epoch_current(reset_generation: u32) -> bool {
    usb_epoch_current(reset_generation)
}

fn usb_epoch_current(reset_generation: u32) -> bool {
    !USB_RESET_EXHAUSTED.load(Ordering::Acquire)
        && !USB_EPOCH_BLOCKED.load(Ordering::Acquire)
        && USB_RESET_GENERATION.load(Ordering::Acquire) == reset_generation
}

fn try_in_epoch<R>(reset_generation: u32, action: impl FnOnce() -> R) -> Option<R> {
    critical_section::with(|_| {
        if !usb_epoch_current(reset_generation)
            || USB_DEVICE::regs()
                .int_raw()
                .read()
                .usb_bus_reset()
                .bit_is_set()
        {
            return None;
        }
        Some(action())
    })
}

fn try_send_in_epoch<T, E>(
    reset_generation: u32,
    owner: T,
    send: impl FnOnce(T) -> Result<(), E>,
) -> Option<Result<(), E>> {
    let mut owner = Some(owner);
    critical_section::with(|_| {
        // Linearize the final generation/pending-IRQ check and the depth-one
        // enqueue against the reset ISR. A reset after this section is later
        // than an already admitted/event-timestamped command; node ordering
        // processes it before Disconnected, while BLOCKED prevents its reply
        // from crossing into the replacement epoch.
        if !usb_epoch_current(reset_generation)
            || USB_DEVICE::regs()
                .int_raw()
                .read()
                .usb_bus_reset()
                .bit_is_set()
        {
            return None;
        }
        Some(send(
            owner
                .take()
                .expect("the epoch-guarded handoff owns one command"),
        ))
    })
}

fn retire_transmission(transmission: &mut Option<PendingTransmission>) {
    if transmission.take().is_some() {
        TX_EPOCH_ARMED.store(false, Ordering::Release);
    }
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
    pending_live_command: &mut Option<PendingLiveCommand>,
    transmission: &mut Option<PendingTransmission>,
    authenticated_session: &mut UsbAuthenticatedSession,
    pending_pairing_exclusive: &mut Option<(ConnectionId, u32)>,
    announced_connection: Option<ConnectionId>,
    disconnect_pending: &mut Option<ConnectionId>,
) {
    // An already-transferred live command is deliberately absent from this
    // reset surface. Its correlation remains in `awaiting_live_reply` until
    // the durable node owner returns a reply, which this task then drains and
    // drops as stale instead of leaking it into a replacement epoch.
    decoder.reset();
    *sequence_gate = None;
    retire_transmission(transmission);
    authenticated_session.reset();
    *pending_pairing_exclusive = None;
    synchronize_tx_epoch(transmission, authenticated_session);
    if pending_command
        .as_ref()
        .is_some_and(|pending| pending.purpose.connection() == connection)
    {
        *pending_command = None;
    }
    if pending_live_command
        .as_ref()
        .is_some_and(|pending| pending.purpose.connection == connection)
    {
        *pending_live_command = None;
    }
    if announced_connection == Some(connection) {
        *disconnect_pending = Some(connection);
    }
}

fn queue_command(
    pending: &mut Option<PendingCommand>,
    command: PairingControlCommand,
    purpose: PendingPurpose,
    reset_generation: u32,
) {
    debug_assert!(pending.is_none());
    *pending = Some(PendingCommand {
        command,
        purpose,
        reset_generation,
    });
}

fn queue_exclusive_acquired(
    pending: &mut Option<PendingCommand>,
    now_millis: u64,
    connection: ConnectionId,
    reset_generation: u32,
) {
    queue_command(
        pending,
        PairingControlCommand::ExclusiveAcquired {
            at: pairing_time(now_millis),
            connection,
        },
        PendingPurpose::Exclusive(connection),
        reset_generation,
    );
}

fn queue_button_observation(
    pending: &mut Option<PendingCommand>,
    last_observation_ms: &mut u64,
    publication: &mut PhysicalPresencePublicationGuard,
    now_millis: u64,
    connection: ConnectionId,
    debounced_level: ActiveLowButton,
    reset_generation: u32,
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
        reset_generation,
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
