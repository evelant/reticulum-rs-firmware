//! Permanent LoRa actor task for interface slot zero.

use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::{ram, system::software_reset};
use log::{error, info, warn};
use reticulum_interface_router::{
    ActorIngressSendError, AvailableIngressBuffer, IngressSignalObservation,
    InterfaceIngressActorHandoff, InterfaceIngressAuthority, InterfaceLifecycleActorError,
    InterfaceLifecycleActorHandoff, InterfaceLifecycleState, SealedIngressPacket,
};
use reticulum_radio_interface::{
    RNODE_HW_MTU, SX1262_FRAME_MTU, TimedReceiveOutcome, TimedRnodeRx,
};
use reticulum_radio_tx_dispatch::{
    AuthorizedFrameAcknowledgementProgress, DispatchReport, RadioOperationStep, RadioReceiveStep,
    RadioTxDispatcherChannel, RadioTxDispatcherPhase, RadioTxDispatcherStep, embassy_wait_until_us,
};

use crate::{ProductDispatcher, config};
use reticulum_e290_firmware::radio_diagnostics::{
    AcceptedLoRaPacketObservation, RadioDiagnosticsCell,
};
use reticulum_e290_firmware::radio_recovery::{
    self, RadioRecoveryDisposition, RadioRecoveryResetMarkerState,
};

// A nonzero marker survives software reset and bounds recovery for the sole
// physical radio owner. Zero is restored by a power-on reset. Torn writes are
// treated as armed, so interruption while recording the marker cannot create a
// reboot loop.
#[ram(unstable(rtc_fast, persistent))]
static mut RADIO_RECOVERY_RESET_MARKER: [u32; 2] = [0; 2];

#[embassy_executor::task]
pub async fn run(
    dispatcher: &'static mut ProductDispatcher,
    timing: config::RadioTaskTiming,
    diagnostics: &'static RadioDiagnosticsCell,
    mut ingress: InterfaceIngressActorHandoff<
        CriticalSectionRawMutex,
        { config::INTERFACE_QUEUE_DEPTH },
    >,
    mut lifecycle: InterfaceLifecycleActorHandoff<CriticalSectionRawMutex>,
    authority: InterfaceIngressAuthority,
) {
    let fragment_timeout = timing.fragment_timeout_us();
    let mut receiver = TimedRnodeRx::new(fragment_timeout);
    let mut physical = [0_u8; SX1262_FRAME_MTU];
    let mut native = [0_u8; RNODE_HW_MTU];
    let mut available: Option<AvailableIngressBuffer> = None;
    let mut sealed_pending: Option<SealedIngressPacket> = None;
    let mut rx_serviced_since_tx_check = false;

    info!(
        "e290-node stage=lora-actor status=READY fragment_timeout_us={} rx_watchdog_us={} cad_watchdog_us={} tx_watchdog_us={} max_packet_airtime_us={}",
        fragment_timeout.get(),
        timing.receive_operation_watchdog_us().get(),
        timing.cad_operation_watchdog_us(),
        timing.tx_operation_watchdog_us(),
        timing.maximum_logical_packet_airtime_us(),
    );
    match lifecycle
        .request_state(authority.lease(), InterfaceLifecycleState::Ready)
        .await
    {
        Ok(descriptor) => {
            diagnostics.mark_online();
            info!(
                "e290-node stage=lora-actor status=ONLINE interface={} generation={} queue={}",
                descriptor.lease().interface().get(),
                descriptor.lease().generation().get(),
                descriptor.lease().queue().get(),
            );
        }
        Err(reason) => {
            error!(
                "e290-node stage=lora-actor status=FAIL reason=lifecycle-ready:{reason:?} action=report-offline-and-fail-stop"
            );
            fail_stop(
                dispatcher,
                diagnostics,
                &mut lifecycle,
                authority,
                available.take(),
                sealed_pending.take(),
            )
            .await
        }
    }
    loop {
        if let Some(packet) = sealed_pending.take() {
            match ingress.try_send(authority, packet) {
                Ok(()) => {
                    diagnostics.record_ingress_enqueued();
                }
                Err(failure) => match failure.reason() {
                    ActorIngressSendError::QueueFull(_) => {
                        diagnostics.record_ingress_deferred();
                        let (_, packet) = failure.into_parts();
                        sealed_pending = Some(packet);
                    }
                    reason => {
                        diagnostics.record_ingress_failed();
                        let (_, packet) = failure.into_parts();
                        error!(
                            "e290-node stage=lora-ingress status=FAIL reason={reason:?} action=quarantine-exact-ingress-owner-and-actor-fail-stop"
                        );
                        fail_stop(
                            dispatcher,
                            diagnostics,
                            &mut lifecycle,
                            authority,
                            available.take(),
                            Some(packet),
                        )
                        .await
                    }
                },
            }
        }

        if available.is_none() && sealed_pending.is_none() {
            available = ingress.try_receive_buffer();
        }

        if dispatcher.phase() == RadioTxDispatcherPhase::Idle
            && sealed_pending.is_none()
            && available.is_some()
            && (!rx_serviced_since_tx_check || receiver.pending().is_some())
        {
            match receive_once(dispatcher, diagnostics, &mut physical, timing).await {
                RadioReceiveStep::Frame(observation) => {
                    let Some(frame) = observation.payload(&physical) else {
                        diagnostics.record_invalid_physical_frame();
                        error!(
                            "e290-node stage=lora-rx status=FAIL reason=observation-length action=actor-fail-stop-no-further-radio-operations"
                        );
                        fail_stop(
                            dispatcher,
                            diagnostics,
                            &mut lifecycle,
                            authority,
                            available.take(),
                            sealed_pending.take(),
                        )
                        .await
                    };
                    let outcome = receiver.feed(
                        frame,
                        observation.received_at_us(),
                        observation.signal(),
                        &mut native,
                    );
                    let accepted = match outcome {
                        Ok(TimedReceiveOutcome::Packet {
                            packet_len, signal, ..
                        }) => Some(AcceptedLoRaPacketObservation::new(
                            observation.received_at_us(),
                            u16::try_from(packet_len)
                                .expect("the admitted base RNS packet length fits in u16"),
                            signal,
                        )),
                        _ => None,
                    };
                    diagnostics.record_receive_pipeline(receiver.diagnostics(), accepted);
                    match outcome {
                        Ok(TimedReceiveOutcome::AwaitingContinuation { .. }) => {
                            // A partial logical packet retains RX priority until
                            // it completes or its profile-derived deadline expires.
                            continue;
                        }
                        Ok(TimedReceiveOutcome::Packet {
                            packet_len, signal, ..
                        }) => {
                            let _ = diagnostics.record_logical_rx(
                                observation.received_at_us(),
                                authority.lease().interface().get(),
                                &native[..packet_len],
                                signal,
                            );
                            let mut buffer = available.take().expect("RX requires an exact buffer");
                            let Some(destination) = buffer.capacity_mut().get_mut(..packet_len)
                            else {
                                diagnostics.record_ingress_failed();
                                error!(
                                    "e290-node stage=lora-rx status=DROP reason=native-packet-capacity packet_len={packet_len}"
                                );
                                available = Some(buffer);
                                rx_serviced_since_tx_check = true;
                                continue;
                            };
                            destination.copy_from_slice(&native[..packet_len]);
                            match buffer.seal_with_signal(
                                packet_len,
                                Some(IngressSignalObservation::new(
                                    signal.rssi_dbm,
                                    signal.snr_db,
                                )),
                            ) {
                                Ok(packet) => match ingress.try_send(authority, packet) {
                                    Ok(()) => {
                                        diagnostics.record_ingress_enqueued();
                                    }
                                    Err(failure) => match failure.reason() {
                                        ActorIngressSendError::QueueFull(_) => {
                                            diagnostics.record_ingress_deferred();
                                            let (_, packet) = failure.into_parts();
                                            sealed_pending = Some(packet);
                                        }
                                        reason => {
                                            diagnostics.record_ingress_failed();
                                            let (_, packet) = failure.into_parts();
                                            error!(
                                                "e290-node stage=lora-ingress status=FAIL reason={reason:?} action=quarantine-exact-ingress-owner-and-actor-fail-stop"
                                            );
                                            fail_stop(
                                                dispatcher,
                                                diagnostics,
                                                &mut lifecycle,
                                                authority,
                                                available.take(),
                                                Some(packet),
                                            )
                                            .await
                                        }
                                    },
                                },
                                Err(failure) => {
                                    diagnostics.record_ingress_failed();
                                    warn!(
                                        "e290-node stage=lora-rx status=DROP reason={:?} packet_len={packet_len}",
                                        failure.reason()
                                    );
                                    available = Some(failure.into_buffer());
                                }
                            }
                            rx_serviced_since_tx_check = true;
                        }
                        Err(reason) => {
                            warn!("e290-node stage=lora-rx status=DROP reason={reason:?}");
                            if receiver.pending().is_some() {
                                continue;
                            }
                            rx_serviced_since_tx_check = true;
                        }
                    }
                }
                RadioReceiveStep::SchedulerYield => {
                    if receiver.pending().is_some() {
                        let _ = receiver.expire(now_us());
                        diagnostics.record_receive_pipeline(receiver.diagnostics(), None);
                        if receiver.pending().is_some() {
                            continue;
                        }
                    }
                    rx_serviced_since_tx_check = true;
                }
                RadioReceiveStep::InvalidFrame => {
                    diagnostics.record_invalid_physical_frame();
                    warn!("e290-node stage=lora-rx status=DROP reason=invalid-physical-frame");
                    if receiver.pending().is_some() {
                        let _ = receiver.expire(now_us());
                        diagnostics.record_receive_pipeline(receiver.diagnostics(), None);
                        if receiver.pending().is_some() {
                            continue;
                        }
                    }
                    rx_serviced_since_tx_check = true;
                }
                RadioReceiveStep::TxPriority(_) => {
                    rx_serviced_since_tx_check = true;
                }
                RadioReceiveStep::CancelledFutureNeedsRecovery => {
                    recover_cancelled_and_drain(dispatcher, diagnostics).await;
                    fail_stop(
                        dispatcher,
                        diagnostics,
                        &mut lifecycle,
                        authority,
                        available.take(),
                        sealed_pending.take(),
                    )
                    .await
                }
                RadioReceiveStep::Fault { phase, class } => {
                    error!(
                        "e290-node stage=lora-rx status=FAIL phase={phase:?} class={class:?} action=actor-fail-stop-no-further-radio-operations"
                    );
                    fail_stop(
                        dispatcher,
                        diagnostics,
                        &mut lifecycle,
                        authority,
                        available.take(),
                        sealed_pending.take(),
                    )
                    .await
                }
                RadioReceiveStep::InvalidObservation { len } => {
                    diagnostics.record_invalid_physical_frame();
                    error!(
                        "e290-node stage=lora-rx status=FAIL reason=invalid-observation len={len} action=actor-fail-stop-no-further-radio-operations"
                    );
                    fail_stop(
                        dispatcher,
                        diagnostics,
                        &mut lifecycle,
                        authority,
                        available.take(),
                        sealed_pending.take(),
                    )
                    .await
                }
                RadioReceiveStep::Disabled(fault) => {
                    error!(
                        "e290-node stage=lora-rx status=FAIL reason={fault:?} action=actor-fail-stop-no-further-radio-operations"
                    );
                    fail_stop(
                        dispatcher,
                        diagnostics,
                        &mut lifecycle,
                        authority,
                        available.take(),
                        sealed_pending.take(),
                    )
                    .await
                }
            }
        }

        if dispatcher.phase() == RadioTxDispatcherPhase::Idle && available.is_none() {
            // No reusable RX owner is available. Do not start receive; give TX
            // one turn and let the node task recycle the pool.
            rx_serviced_since_tx_check = true;
        }

        let phase_before = dispatcher.phase();
        let dispatch_progress = drive_dispatcher_once(dispatcher, diagnostics, timing).await;
        match dispatch_progress {
            DispatchProgress::NeedJob => {
                rx_serviced_since_tx_check = false;
                yield_now().await;
                if available.is_none() || sealed_pending.is_some() {
                    Timer::after(Duration::from_millis(1)).await;
                }
            }
            DispatchProgress::Advanced => {
                if phase_before != RadioTxDispatcherPhase::Idle
                    && dispatcher.phase() == RadioTxDispatcherPhase::Idle
                {
                    rx_serviced_since_tx_check = false;
                }
            }
            DispatchProgress::Disabled => {
                fail_stop(
                    dispatcher,
                    diagnostics,
                    &mut lifecycle,
                    authority,
                    available.take(),
                    sealed_pending.take(),
                )
                .await
            }
        }
    }
}

async fn receive_once(
    dispatcher: &mut ProductDispatcher,
    diagnostics: &RadioDiagnosticsCell,
    physical: &mut [u8; SX1262_FRAME_MTU],
    timing: config::RadioTaskTiming,
) -> RadioReceiveStep {
    let watchdog = timing.receive_operation_watchdog_us().get();
    let scheduler_yield = Timer::after(Duration::from_micros(timing.rx_scheduler_yield_us()));
    let progress_deadline = async {
        Timer::after(Duration::from_micros(timing.rx_progress_timeout_us())).await;
    };
    let receive = match dispatcher.start_continuous_receive_until(
        physical,
        scheduler_yield,
        progress_deadline,
    ) {
        Ok(receive) => receive,
        Err(step) => return step,
    };
    match with_timeout(Duration::from_micros(watchdog), receive).await {
        Ok(step) => step,
        Err(_) => {
            error!(
                "e290-node stage=lora-rx status=WATCHDOG-EXPIRED watchdog_us={watchdog} action=cancel-drain-rate-limited-software-reset"
            );
            recover_cancelled_and_drain(dispatcher, diagnostics).await;
            RadioReceiveStep::Disabled(
                dispatcher
                    .fault()
                    .unwrap_or(reticulum_radio_tx_dispatch::DispatcherFault::ReceiveCancelled),
            )
        }
    }
}

enum DispatchProgress {
    NeedJob,
    Advanced,
    Disabled,
}

async fn drive_dispatcher_once(
    dispatcher: &mut ProductDispatcher,
    diagnostics: &RadioDiagnosticsCell,
    timing: config::RadioTaskTiming,
) -> DispatchProgress {
    let step = dispatcher.step(now_us());
    if let Some(report) = dispatcher.take_last_report() {
        record_and_log_dispatch_report(diagnostics, "step", report);
    }
    match step {
        RadioTxDispatcherStep::Advanced => DispatchProgress::Advanced,
        RadioTxDispatcherStep::NeedJob => DispatchProgress::NeedJob,
        RadioTxDispatcherStep::NeedReceiveRecovery => {
            recover_cancelled_and_drain(dispatcher, diagnostics).await;
            DispatchProgress::Disabled
        }
        RadioTxDispatcherStep::WaitUntil { retry_at_us, .. } => {
            embassy_wait_until_us(retry_at_us).await;
            DispatchProgress::Advanced
        }
        RadioTxDispatcherStep::NeedCad(_) => {
            run_radio_operation(
                dispatcher,
                diagnostics,
                timing.cad_operation_watchdog_us(),
                "cad",
            )
            .await
        }
        RadioTxDispatcherStep::NeedTransmit(_) => {
            run_radio_operation(
                dispatcher,
                diagnostics,
                timing.tx_operation_watchdog_us(),
                "tx",
            )
            .await
        }
        RadioTxDispatcherStep::NeedPermitReply {
            grace_deadline_us, ..
        } => wait_until_or(dispatcher, grace_deadline_us, WaitKind::PermitReply).await,
        RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement => {
            match dispatcher.wait_for_authorized_frame_acknowledgement().await {
                AuthorizedFrameAcknowledgementProgress::Matched => {
                    info!(
                        "e290-node stage=authorized-frame status=DURABLE action=release-ticket-bound-completion"
                    );
                    DispatchProgress::Advanced
                }
                AuthorizedFrameAcknowledgementProgress::NotRetained(_) => {
                    DispatchProgress::Advanced
                }
                AuthorizedFrameAcknowledgementProgress::Disabled(fault) => {
                    error!(
                        "e290-node stage=authorized-frame status=FAIL reason={fault:?} mismatch={:?} unexpected={:?} action=actor-fail-stop",
                        dispatcher.authorized_frame_acknowledgement_mismatch(),
                        dispatcher.unexpected_authorized_frame_acknowledgement(),
                    );
                    DispatchProgress::Disabled
                }
            }
        }
        RadioTxDispatcherStep::Backpressured(channel) => match channel {
            RadioTxDispatcherChannel::DataPermitRequest
            | RadioTxDispatcherChannel::OrdinaryPermitRequest => {
                let deadline = match dispatcher.poll_permit_request_capacity(&mut noop_context()) {
                    reticulum_radio_tx_dispatch::PermitRequestCapacity::Pending {
                        grace_deadline_us,
                        ..
                    }
                    | reticulum_radio_tx_dispatch::PermitRequestCapacity::Ready {
                        grace_deadline_us,
                        ..
                    } => grace_deadline_us,
                    reticulum_radio_tx_dispatch::PermitRequestCapacity::NotRetained(_) => {
                        return DispatchProgress::Advanced;
                    }
                };
                wait_until_or(dispatcher, deadline, WaitKind::PermitCapacity).await
            }
            RadioTxDispatcherChannel::AuthorizedFrameObservationRequest => {
                let _ = dispatcher
                    .wait_for_authorized_frame_request_capacity()
                    .await;
                DispatchProgress::Advanced
            }
            RadioTxDispatcherChannel::InterfaceCompletion(_) => {
                let _ = dispatcher.wait_for_interface_completion_capacity().await;
                DispatchProgress::Advanced
            }
        },
        RadioTxDispatcherStep::Disabled(fault) => {
            error!(
                "e290-node stage=lora-dispatch status=FAIL reason={fault:?} residue={:?}",
                dispatcher.fault_residue_kind()
            );
            DispatchProgress::Disabled
        }
    }
}

enum WaitKind {
    PermitReply,
    PermitCapacity,
}

async fn wait_until_or(
    dispatcher: &mut ProductDispatcher,
    deadline_us: u64,
    kind: WaitKind,
) -> DispatchProgress {
    let now = now_us();
    if now >= deadline_us {
        return DispatchProgress::Advanced;
    }
    let duration = Duration::from_micros(deadline_us - now);
    match kind {
        WaitKind::PermitReply => {
            let _ = with_timeout(duration, dispatcher.wait_for_permit_reply()).await;
        }
        WaitKind::PermitCapacity => {
            let _ = with_timeout(duration, dispatcher.wait_for_permit_request_capacity()).await;
        }
    }
    DispatchProgress::Advanced
}

async fn run_radio_operation(
    dispatcher: &mut ProductDispatcher,
    diagnostics: &RadioDiagnosticsCell,
    watchdog_us: u64,
    operation: &'static str,
) -> DispatchProgress {
    match with_timeout(
        Duration::from_micros(watchdog_us),
        dispatcher.perform_radio_operation(now_us()),
    )
    .await
    {
        Ok(RadioOperationStep::Advanced) => DispatchProgress::Advanced,
        Ok(RadioOperationStep::CadObserved {
            activity_detected, ..
        }) => {
            diagnostics.record_cad(activity_detected);
            DispatchProgress::Advanced
        }
        Ok(RadioOperationStep::Terminal(report)) => {
            if dispatcher.take_last_report() != Some(report) {
                error!(
                    "e290-node stage=lora-{operation} status=FAIL reason=terminal-report-mismatch action=actor-fail-stop"
                );
                return DispatchProgress::Disabled;
            }
            record_and_log_dispatch_report(diagnostics, operation, report);
            DispatchProgress::Advanced
        }
        Ok(RadioOperationStep::NotReady) => DispatchProgress::Advanced,
        Ok(
            RadioOperationStep::CancelledFutureNeedsRecovery(_)
            | RadioOperationStep::ReceiveFutureNeedsRecovery,
        ) => {
            recover_cancelled_and_drain(dispatcher, diagnostics).await;
            DispatchProgress::Disabled
        }
        Ok(RadioOperationStep::Disabled(fault)) => {
            error!("e290-node stage=lora-{operation} status=FAIL reason={fault:?}");
            DispatchProgress::Disabled
        }
        Err(_) => {
            error!(
                "e290-node stage=lora-{operation} status=WATCHDOG-EXPIRED watchdog_us={watchdog_us} action=cancel-drain-rate-limited-software-reset"
            );
            recover_cancelled_and_drain(dispatcher, diagnostics).await;
            DispatchProgress::Disabled
        }
    }
}

/// Contain a dropped radio future, pass any post-exposure DATA owner through
/// the exact durability gate, then return the ticketed completion before
/// accepting terminal disablement.
///
/// Dispatcher regression tests `dropped_transmit_future...` and
/// `cancelled_cad...` prove that recovery first enters completion return and
/// that a subsequent `step` preserves the exact owner through durability and
/// completion return before Disabled.
async fn recover_cancelled_and_drain(
    dispatcher: &mut ProductDispatcher,
    diagnostics: &RadioDiagnosticsCell,
) {
    let _ = dispatcher.recover_cancelled_radio_operation();
    if let Some(report) = dispatcher.take_last_report() {
        record_and_log_dispatch_report(diagnostics, "recovery", report);
    }
    loop {
        let step = dispatcher.step(now_us());
        if let Some(report) = dispatcher.take_last_report() {
            record_and_log_dispatch_report(diagnostics, "recovery", report);
        }
        match step {
            RadioTxDispatcherStep::Advanced => continue,
            RadioTxDispatcherStep::Backpressured(channel) => match channel {
                RadioTxDispatcherChannel::AuthorizedFrameObservationRequest => {
                    let _ = dispatcher
                        .wait_for_authorized_frame_request_capacity()
                        .await;
                }
                RadioTxDispatcherChannel::InterfaceCompletion(_) => {
                    let _ = dispatcher.wait_for_interface_completion_capacity().await;
                }
                RadioTxDispatcherChannel::DataPermitRequest
                | RadioTxDispatcherChannel::OrdinaryPermitRequest => {
                    error!(
                        "e290-node stage=lora-recovery status=FAIL reason=unexpected-permit-backpressure"
                    );
                    return;
                }
            },
            RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement => {
                match dispatcher.wait_for_authorized_frame_acknowledgement().await {
                    AuthorizedFrameAcknowledgementProgress::Matched => {
                        info!(
                            "e290-node stage=authorized-frame status=DURABLE action=release-cancelled-ticket-bound-completion"
                        );
                    }
                    AuthorizedFrameAcknowledgementProgress::NotRetained(_) => {}
                    AuthorizedFrameAcknowledgementProgress::Disabled(fault) => {
                        error!(
                            "e290-node stage=authorized-frame status=FAIL reason={fault:?} mismatch={:?} unexpected={:?} action=retain-dispatcher-and-fail-stop",
                            dispatcher.authorized_frame_acknowledgement_mismatch(),
                            dispatcher.unexpected_authorized_frame_acknowledgement(),
                        );
                        return;
                    }
                }
            }
            RadioTxDispatcherStep::Disabled(fault) => {
                let residue = dispatcher.fault_residue_kind();
                if matches!(
                    fault,
                    reticulum_radio_tx_dispatch::DispatcherFault::RadioUnavailable
                        | reticulum_radio_tx_dispatch::DispatcherFault::ReceiveCancelled
                ) && residue.is_none()
                {
                    recover_radio_owner_after_exact_drain(diagnostics, fault).await;
                } else {
                    error!(
                        "e290-node stage=lora-recovery status=FAIL reason=exact-owner-drain-incomplete dispatcher_fault={fault:?} residue={residue:?} action=retain-dispatcher-and-fail-stop"
                    );
                }
                return;
            }
            RadioTxDispatcherStep::NeedReceiveRecovery => {
                let _ = dispatcher.recover_cancelled_radio_operation();
                if let Some(report) = dispatcher.take_last_report() {
                    record_and_log_dispatch_report(diagnostics, "recovery", report);
                }
            }
            unexpected => {
                error!(
                    "e290-node stage=lora-recovery status=FAIL reason=unexpected-transition transition={unexpected:?}"
                );
                return;
            }
        }
    }
}

fn retained_radio_recovery_marker() -> RadioRecoveryResetMarkerState {
    // SAFETY: The permanent radio task is the sole runtime owner of this marker.
    // Volatile access creates no reference to the mutable static, and startup
    // deliberately preserves this RTC region across software reset.
    let marker =
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(RADIO_RECOVERY_RESET_MARKER)) };
    radio_recovery::classify_radio_recovery_reset_marker(marker)
}

fn arm_radio_recovery_reset_marker() {
    let marker = core::ptr::addr_of_mut!(RADIO_RECOVERY_RESET_MARKER).cast::<u32>();
    // SAFETY: The permanent radio task is the sole runtime writer. Store the
    // nonzero complement first so either torn-write boundary is classified as
    // a previous recovery attempt.
    unsafe {
        core::ptr::write_volatile(
            marker.add(1),
            radio_recovery::RADIO_RECOVERY_RESET_MARKER_WORDS[1],
        );
        core::ptr::write_volatile(marker, radio_recovery::RADIO_RECOVERY_RESET_MARKER_WORDS[0]);
    }
}

/// Reset the whole MCU only after cancellation recovery has returned every
/// exact packet/completion owner and the dispatcher has reached Disabled.
///
/// The sole-radio contract intentionally destroys initialized SPI/GPIO driver
/// ownership when an operation future is dropped. A software reset is therefore
/// the narrow safe reconstruction path. A rapid repeat returns to the caller,
/// which enters the ordinary interface-local fail-stop with all remaining
/// ingress and dispatcher owners retained.
async fn recover_radio_owner_after_exact_drain(
    diagnostics: &RadioDiagnosticsCell,
    fault: reticulum_radio_tx_dispatch::DispatcherFault,
) {
    diagnostics.mark_faulted();
    let marker = retained_radio_recovery_marker();
    let boot_uptime_ms = Instant::now().as_millis();
    match radio_recovery::radio_recovery_disposition(marker.is_clean(), boot_uptime_ms) {
        RadioRecoveryDisposition::SoftwareReset => {
            arm_radio_recovery_reset_marker();
            error!(
                "e290-node stage=lora-recovery status=RESETTING reason=cancelled-radio-operation dispatcher_fault={fault:?} exact_owner_drain=complete action=software-reset retained_marker={} boot_uptime_ms={} rearm_uptime_ms={}",
                marker.label(),
                boot_uptime_ms,
                radio_recovery::RADIO_RECOVERY_RESET_REARM_UPTIME_MS,
            );
            // Preserve the terminal diagnostic on USB Serial/JTAG before ROM
            // tears down the chip and reconstructs every physical owner.
            Timer::after_millis(100).await;
            software_reset();
        }
        RadioRecoveryDisposition::FailStopUntilPowerCycle => {
            error!(
                "e290-node stage=lora-recovery status=RESET-SUPPRESSED reason=loop-guard dispatcher_fault={fault:?} exact_owner_drain=complete retained_marker={} boot_uptime_ms={} rearm_uptime_ms={} action=actor-fail-stop-until-power-cycle",
                marker.label(),
                boot_uptime_ms,
                radio_recovery::RADIO_RECOVERY_RESET_REARM_UPTIME_MS,
            );
        }
    }
}

fn record_and_log_dispatch_report(
    diagnostics: &RadioDiagnosticsCell,
    operation: &str,
    report: DispatchReport,
) {
    diagnostics.record_dispatch_report(report, now_us());
    info!(
        "e290-node stage=lora-{operation} status=TERMINAL family={:?} outcome={:?} frame_count={} progress={:?} authorized_frame={:?}",
        report.family(),
        report.outcome(),
        report.frame_count(),
        report.progress(),
        report.authorized_frame(),
    );
}

fn now_us() -> u64 {
    Instant::now().as_micros()
}

// This context is used only for one advisory, non-owning readiness snapshot.
// The subsequent cancellation-safe async wait registers the real task waker.
fn noop_context() -> core::task::Context<'static> {
    core::task::Context::from_waker(core::task::Waker::noop())
}

async fn fail_stop(
    dispatcher: &mut ProductDispatcher,
    diagnostics: &RadioDiagnosticsCell,
    lifecycle: &mut InterfaceLifecycleActorHandoff<CriticalSectionRawMutex>,
    authority: InterfaceIngressAuthority,
    retained_available: Option<AvailableIngressBuffer>,
    retained_sealed: Option<SealedIngressPacket>,
) -> ! {
    diagnostics.mark_faulted();
    // The actor takes no further radio operations. The dispatcher may already
    // have shut down the radio as part of terminal cancellation recovery, but
    // this generic path does not claim or attempt a separate hardware shutdown.
    let offline_descriptor = loop {
        let result = match lifecycle
            .request_state(authority.lease(), InterfaceLifecycleState::Offline)
            .await
        {
            Err(InterfaceLifecycleActorError::ExchangePending(_)) => {
                lifecycle.finish_pending_request().await
            }
            result => result,
        };
        match result {
            Ok(descriptor) if !descriptor.is_online() => break descriptor,
            Ok(descriptor) => warn!(
                "e290-node stage=lora-actor status=FAIL-STOPPING lifecycle=READY interface={} generation={} action=retry-offline",
                descriptor.lease().interface().get(),
                descriptor.lease().generation().get(),
            ),
            Err(reason) => error!(
                "e290-node stage=lora-actor status=FAIL-STOPPING lifecycle=RETRY reason={reason:?} available_owner_quarantined={} sealed_owner_quarantined={} action=retry-offline-no-further-radio-operations",
                retained_available.is_some(),
                retained_sealed.is_some(),
            ),
        }
        Timer::after(Duration::from_secs(1)).await;
    };
    warn!(
        "e290-node stage=lora-actor status=FAIL-STOPPED lifecycle=OFFLINE interface={} generation={} online={} available_owner_quarantined={} sealed_owner_quarantined={}",
        offline_descriptor.lease().interface().get(),
        offline_descriptor.lease().generation().get(),
        offline_descriptor.is_online(),
        retained_available.is_some(),
        retained_sealed.is_some(),
    );
    loop {
        // Keep every exact ingress owner and the dispatcher-owned TX state
        // live in this never-returning actor task.
        let _ = (
            dispatcher.phase(),
            retained_available.as_ref(),
            retained_sealed.as_ref(),
        );
        Timer::after(Duration::from_secs(30)).await;
    }
}
