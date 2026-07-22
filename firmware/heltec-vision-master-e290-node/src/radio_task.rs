//! Permanent LoRa actor task for interface slot zero.

use core::num::NonZeroU64;

use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use log::{error, info, warn};
#[cfg(feature = "runtime-measurement-hil")]
use reticulum_heltec_vision_master_e290_node::runtime_measurement::{
    OperationKind as RuntimeOperationKind, RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE,
    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE,
};
use reticulum_interface_router::{
    ActorIngressSendError, AvailableIngressBuffer, InterfaceIngressActorHandoff,
    InterfaceIngressAuthority, InterfaceLifecycleActorError, InterfaceLifecycleActorHandoff,
    InterfaceLifecycleState, SealedIngressPacket,
};
use reticulum_radio_interface::{
    RNODE_HW_MTU, SX1262_FRAME_MTU, TimedReceiveOutcome, TimedRnodeRx,
};
#[cfg(feature = "runtime-measurement-hil")]
use reticulum_radio_tx_dispatch::DispatchOutcome;
use reticulum_radio_tx_dispatch::{
    AuthorizedFrameAcknowledgementProgress, DispatchReport, RadioOperationStep, RadioReceiveStep,
    RadioTxDispatcherChannel, RadioTxDispatcherPhase, RadioTxDispatcherStep, embassy_wait_until_us,
};

use crate::{ProductDispatcher, config};

#[embassy_executor::task]
pub async fn run(
    dispatcher: &'static mut ProductDispatcher,
    mut ingress: InterfaceIngressActorHandoff<
        CriticalSectionRawMutex,
        { config::INTERFACE_QUEUE_DEPTH },
    >,
    mut lifecycle: InterfaceLifecycleActorHandoff<CriticalSectionRawMutex>,
    authority: InterfaceIngressAuthority,
) {
    let fragment_timeout = NonZeroU64::new(
        reticulum_board_heltec_vision_master_e290_radio::E290_NA915_DEV_PROFILE
            .fragment_timeout_us(),
    )
    .expect("the validated E290 profile has a non-zero fragment timeout");
    let mut receiver = TimedRnodeRx::new(fragment_timeout);
    let mut physical = [0_u8; SX1262_FRAME_MTU];
    let mut native = [0_u8; RNODE_HW_MTU];
    let mut available: Option<AvailableIngressBuffer> = None;
    let mut sealed_pending: Option<SealedIngressPacket> = None;
    let mut rx_serviced_since_tx_check = false;

    info!(
        "e290-node stage=lora-actor status=READY fragment_timeout_us={} rx_watchdog_us={} cad_watchdog_us={} tx_watchdog_us={} max_packet_airtime_us={}",
        fragment_timeout.get(),
        dispatcher.maximum_receive_operation_us().get(),
        config::CAD_OPERATION_WATCHDOG_US,
        config::TX_OPERATION_WATCHDOG_US,
        config::MAXIMUM_LOGICAL_PACKET_AIRTIME_US,
    );
    match lifecycle
        .request_state(authority.lease(), InterfaceLifecycleState::Ready)
        .await
    {
        Ok(descriptor) => info!(
            "e290-node stage=lora-actor status=ONLINE interface={} generation={} queue={}",
            descriptor.lease().interface().get(),
            descriptor.lease().generation().get(),
            descriptor.lease().queue().get(),
        ),
        Err(reason) => {
            error!(
                "e290-node stage=lora-actor status=FAIL reason=lifecycle-ready:{reason:?} action=report-offline-and-fail-stop"
            );
            fail_stop(
                dispatcher,
                &mut lifecycle,
                authority,
                available.take(),
                sealed_pending.take(),
            )
            .await
        }
    }
    #[cfg(feature = "runtime-measurement-hil")]
    let mut previous_radio_loop_us = now_us();

    loop {
        #[cfg(feature = "runtime-measurement-hil")]
        {
            let loop_started_us = now_us();
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE
                .record_radio_loop_gap(loop_started_us.saturating_sub(previous_radio_loop_us));
            previous_radio_loop_us = loop_started_us;
        }
        if let Some(packet) = sealed_pending.take() {
            match ingress.try_send(authority, packet) {
                Ok(()) => {
                    #[cfg(feature = "runtime-measurement-hil")]
                    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                        .record_ingress_enqueued(now_us() / 1_000);
                }
                Err(failure) => match failure.reason() {
                    ActorIngressSendError::QueueFull(_) => {
                        #[cfg(feature = "runtime-measurement-hil")]
                        RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                            .record_ingress_deferred(now_us() / 1_000);
                        let (_, packet) = failure.into_parts();
                        sealed_pending = Some(packet);
                    }
                    reason => {
                        #[cfg(feature = "runtime-measurement-hil")]
                        RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                            .record_ingress_failed(now_us() / 1_000);
                        let (_, packet) = failure.into_parts();
                        error!(
                            "e290-node stage=lora-ingress status=FAIL reason={reason:?} action=quarantine-exact-ingress-owner-and-actor-fail-stop"
                        );
                        fail_stop(
                            dispatcher,
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
            match receive_once(dispatcher, &mut physical).await {
                RadioReceiveStep::Frame(observation) => {
                    let Some(frame) = observation.payload(&physical) else {
                        error!(
                            "e290-node stage=lora-rx status=FAIL reason=observation-length action=actor-fail-stop-no-further-radio-operations"
                        );
                        fail_stop(
                            dispatcher,
                            &mut lifecycle,
                            authority,
                            available.take(),
                            sealed_pending.take(),
                        )
                        .await
                    };
                    match receiver.feed(
                        frame,
                        observation.received_at_us(),
                        observation.signal(),
                        &mut native,
                    ) {
                        Ok(TimedReceiveOutcome::AwaitingContinuation { .. }) => {
                            // A partial logical packet retains RX priority until
                            // it completes or its profile-derived deadline expires.
                            continue;
                        }
                        Ok(TimedReceiveOutcome::Packet { packet_len, .. }) => {
                            #[cfg(feature = "runtime-measurement-hil")]
                            RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                .record_logical_rx_completed(now_us() / 1_000);
                            let mut buffer = available.take().expect("RX requires an exact buffer");
                            let Some(destination) = buffer.capacity_mut().get_mut(..packet_len)
                            else {
                                #[cfg(feature = "runtime-measurement-hil")]
                                RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                    .record_ingress_failed(now_us() / 1_000);
                                error!(
                                    "e290-node stage=lora-rx status=DROP reason=native-packet-capacity packet_len={packet_len}"
                                );
                                available = Some(buffer);
                                rx_serviced_since_tx_check = true;
                                continue;
                            };
                            destination.copy_from_slice(&native[..packet_len]);
                            match buffer.seal(packet_len) {
                                Ok(packet) => match ingress.try_send(authority, packet) {
                                    Ok(()) => {
                                        #[cfg(feature = "runtime-measurement-hil")]
                                        RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                            .record_ingress_enqueued(now_us() / 1_000);
                                    }
                                    Err(failure) => match failure.reason() {
                                        ActorIngressSendError::QueueFull(_) => {
                                            #[cfg(feature = "runtime-measurement-hil")]
                                            RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                                .record_ingress_deferred(now_us() / 1_000);
                                            let (_, packet) = failure.into_parts();
                                            sealed_pending = Some(packet);
                                        }
                                        reason => {
                                            #[cfg(feature = "runtime-measurement-hil")]
                                            RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                                .record_ingress_failed(now_us() / 1_000);
                                            let (_, packet) = failure.into_parts();
                                            error!(
                                                "e290-node stage=lora-ingress status=FAIL reason={reason:?} action=quarantine-exact-ingress-owner-and-actor-fail-stop"
                                            );
                                            fail_stop(
                                                dispatcher,
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
                                    #[cfg(feature = "runtime-measurement-hil")]
                                    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                        .record_ingress_failed(now_us() / 1_000);
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
                        if receiver.pending().is_some() {
                            continue;
                        }
                    }
                    rx_serviced_since_tx_check = true;
                }
                RadioReceiveStep::InvalidFrame => {
                    warn!("e290-node stage=lora-rx status=DROP reason=invalid-physical-frame");
                    if receiver.pending().is_some() {
                        let _ = receiver.expire(now_us());
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
                    recover_cancelled_and_drain(dispatcher).await;
                    fail_stop(
                        dispatcher,
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
                        &mut lifecycle,
                        authority,
                        available.take(),
                        sealed_pending.take(),
                    )
                    .await
                }
                RadioReceiveStep::InvalidObservation { len } => {
                    error!(
                        "e290-node stage=lora-rx status=FAIL reason=invalid-observation len={len} action=actor-fail-stop-no-further-radio-operations"
                    );
                    fail_stop(
                        dispatcher,
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
        let dispatch_progress = drive_dispatcher_once(dispatcher).await;
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

#[cfg(not(feature = "runtime-measurement-hil"))]
async fn receive_once(
    dispatcher: &mut ProductDispatcher,
    physical: &mut [u8; SX1262_FRAME_MTU],
) -> RadioReceiveStep {
    let watchdog = dispatcher.maximum_receive_operation_us().get();
    let scheduler_yield = Timer::after(Duration::from_micros(config::RX_SCHEDULER_YIELD_US));
    let progress_deadline = async {
        Timer::after(Duration::from_micros(config::RX_PROGRESS_TIMEOUT_US)).await;
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
                "e290-node stage=lora-rx status=WATCHDOG-EXPIRED watchdog_us={watchdog} action=cancel-recover-actor-fail-stop"
            );
            recover_cancelled_and_drain(dispatcher).await;
            RadioReceiveStep::Disabled(
                dispatcher
                    .fault()
                    .unwrap_or(reticulum_radio_tx_dispatch::DispatcherFault::ReceiveCancelled),
            )
        }
    }
}

#[cfg(feature = "runtime-measurement-hil")]
async fn receive_once(
    dispatcher: &mut ProductDispatcher,
    physical: &mut [u8; SX1262_FRAME_MTU],
) -> RadioReceiveStep {
    let receive_started_us = now_us();
    let watchdog = dispatcher.maximum_receive_operation_us().get();
    let scheduler_yield = Timer::after(Duration::from_micros(config::RX_SCHEDULER_YIELD_US));
    let progress_deadline = async {
        Timer::after(Duration::from_micros(config::RX_PROGRESS_TIMEOUT_US)).await;
    };
    let receive = match dispatcher.start_continuous_receive_until(
        physical,
        scheduler_yield,
        progress_deadline,
    ) {
        Ok(receive) => receive,
        Err(step) => {
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_operation(
                RuntimeOperationKind::Receive,
                now_us().saturating_sub(receive_started_us),
            );
            return step;
        }
    };
    let (step, timed_out) = match with_timeout(Duration::from_micros(watchdog), receive).await {
        Ok(step) => (step, false),
        Err(_) => {
            error!(
                "e290-node stage=lora-rx status=WATCHDOG-EXPIRED watchdog_us={watchdog} action=cancel-recover-actor-fail-stop"
            );
            recover_cancelled_and_drain(dispatcher).await;
            (
                RadioReceiveStep::Disabled(
                    dispatcher
                        .fault()
                        .unwrap_or(reticulum_radio_tx_dispatch::DispatcherFault::ReceiveCancelled),
                ),
                true,
            )
        }
    };
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_operation(
        RuntimeOperationKind::Receive,
        now_us().saturating_sub(receive_started_us),
    );
    if timed_out {
        RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_radio_timeout(RuntimeOperationKind::Receive);
    }
    step
}

enum DispatchProgress {
    NeedJob,
    Advanced,
    Disabled,
}

async fn drive_dispatcher_once(dispatcher: &mut ProductDispatcher) -> DispatchProgress {
    let step = dispatcher.step(now_us());
    if let Some(report) = dispatcher.take_last_report() {
        log_dispatch_report("step", report);
    }
    match step {
        RadioTxDispatcherStep::Advanced => DispatchProgress::Advanced,
        RadioTxDispatcherStep::NeedJob => DispatchProgress::NeedJob,
        RadioTxDispatcherStep::NeedReceiveRecovery => {
            recover_cancelled_and_drain(dispatcher).await;
            DispatchProgress::Disabled
        }
        RadioTxDispatcherStep::WaitUntil { retry_at_us, .. } => {
            embassy_wait_until_us(retry_at_us).await;
            DispatchProgress::Advanced
        }
        RadioTxDispatcherStep::NeedCad(_) => {
            run_radio_operation(dispatcher, config::CAD_OPERATION_WATCHDOG_US, "cad").await
        }
        RadioTxDispatcherStep::NeedTransmit(_) => {
            run_radio_operation(dispatcher, config::TX_OPERATION_WATCHDOG_US, "tx").await
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

#[cfg(not(feature = "runtime-measurement-hil"))]
async fn run_radio_operation(
    dispatcher: &mut ProductDispatcher,
    watchdog_us: u64,
    operation: &'static str,
) -> DispatchProgress {
    match with_timeout(
        Duration::from_micros(watchdog_us),
        dispatcher.perform_radio_operation(now_us()),
    )
    .await
    {
        Ok(RadioOperationStep::Advanced | RadioOperationStep::CadObserved { .. }) => {
            DispatchProgress::Advanced
        }
        Ok(RadioOperationStep::Terminal(report)) => {
            if dispatcher.take_last_report() != Some(report) {
                error!(
                    "e290-node stage=lora-{operation} status=FAIL reason=terminal-report-mismatch action=actor-fail-stop"
                );
                return DispatchProgress::Disabled;
            }
            log_dispatch_report(operation, report);
            DispatchProgress::Advanced
        }
        Ok(RadioOperationStep::NotReady) => DispatchProgress::Advanced,
        Ok(
            RadioOperationStep::CancelledFutureNeedsRecovery(_)
            | RadioOperationStep::ReceiveFutureNeedsRecovery,
        ) => {
            recover_cancelled_and_drain(dispatcher).await;
            DispatchProgress::Disabled
        }
        Ok(RadioOperationStep::Disabled(fault)) => {
            error!("e290-node stage=lora-{operation} status=FAIL reason={fault:?}");
            DispatchProgress::Disabled
        }
        Err(_) => {
            error!(
                "e290-node stage=lora-{operation} status=WATCHDOG-EXPIRED watchdog_us={watchdog_us} action=cancel-recover-actor-fail-stop"
            );
            recover_cancelled_and_drain(dispatcher).await;
            DispatchProgress::Disabled
        }
    }
}

#[cfg(feature = "runtime-measurement-hil")]
async fn run_radio_operation(
    dispatcher: &mut ProductDispatcher,
    watchdog_us: u64,
    operation: &'static str,
) -> DispatchProgress {
    let operation_started_us = now_us();
    let (progress, timed_out) = match with_timeout(
        Duration::from_micros(watchdog_us),
        dispatcher.perform_radio_operation(now_us()),
    )
    .await
    {
        Ok(RadioOperationStep::Terminal(report)) => {
            if dispatcher.take_last_report() != Some(report) {
                record_runtime_radio_tx_not_confirmed_success(operation);
                error!(
                    "e290-node stage=lora-{operation} status=FAIL reason=terminal-report-mismatch action=actor-fail-stop"
                );
                (DispatchProgress::Disabled, false)
            } else {
                record_runtime_radio_tx_terminal(operation, report);
                log_dispatch_report(operation, report);
                (DispatchProgress::Advanced, false)
            }
        }
        Ok(RadioOperationStep::Advanced | RadioOperationStep::CadObserved { .. }) => {
            record_runtime_radio_tx_not_confirmed_success(operation);
            (DispatchProgress::Advanced, false)
        }
        Ok(RadioOperationStep::NotReady) => {
            record_runtime_radio_tx_not_confirmed_success(operation);
            (DispatchProgress::Advanced, false)
        }
        Ok(
            RadioOperationStep::CancelledFutureNeedsRecovery(_)
            | RadioOperationStep::ReceiveFutureNeedsRecovery,
        ) => {
            record_runtime_radio_tx_not_confirmed_success(operation);
            recover_cancelled_and_drain(dispatcher).await;
            (DispatchProgress::Disabled, false)
        }
        Ok(RadioOperationStep::Disabled(fault)) => {
            record_runtime_radio_tx_not_confirmed_success(operation);
            error!("e290-node stage=lora-{operation} status=FAIL reason={fault:?}");
            (DispatchProgress::Disabled, false)
        }
        Err(_) => {
            record_runtime_radio_tx_not_confirmed_success(operation);
            error!(
                "e290-node stage=lora-{operation} status=WATCHDOG-EXPIRED watchdog_us={watchdog_us} action=cancel-recover-actor-fail-stop"
            );
            recover_cancelled_and_drain(dispatcher).await;
            (DispatchProgress::Disabled, true)
        }
    };
    let measurement_kind = match operation {
        "cad" => Some(RuntimeOperationKind::Cad),
        "tx" => Some(RuntimeOperationKind::Transmit),
        _ => None,
    };
    if let Some(measurement_kind) = measurement_kind {
        RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_operation(
            measurement_kind,
            now_us().saturating_sub(operation_started_us),
        );
        if timed_out {
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_radio_timeout(measurement_kind);
        }
    } else {
        RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_unexpected_error();
    }
    progress
}

#[cfg(feature = "runtime-measurement-hil")]
fn record_runtime_radio_tx_terminal(operation: &str, report: DispatchReport) {
    if operation != "tx" {
        return;
    }
    let confirmed_success = report.outcome() == DispatchOutcome::Transmitted
        && report
            .progress()
            .is_some_and(|progress| progress.completed_frame_count() == report.frame_count());
    if confirmed_success {
        RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.record_radio_tx_confirmed_success();
    } else {
        RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.record_radio_tx_not_confirmed_success();
    }
}

#[cfg(feature = "runtime-measurement-hil")]
fn record_runtime_radio_tx_not_confirmed_success(operation: &str) {
    if operation == "tx" {
        RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.record_radio_tx_not_confirmed_success();
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
async fn recover_cancelled_and_drain(dispatcher: &mut ProductDispatcher) {
    let _ = dispatcher.recover_cancelled_radio_operation();
    if let Some(report) = dispatcher.take_last_report() {
        log_dispatch_report("recovery", report);
    }
    loop {
        let step = dispatcher.step(now_us());
        if let Some(report) = dispatcher.take_last_report() {
            log_dispatch_report("recovery", report);
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
            RadioTxDispatcherStep::Disabled(_) => return,
            RadioTxDispatcherStep::NeedReceiveRecovery => {
                let _ = dispatcher.recover_cancelled_radio_operation();
                if let Some(report) = dispatcher.take_last_report() {
                    log_dispatch_report("recovery", report);
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

fn log_dispatch_report(operation: &str, report: DispatchReport) {
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
    lifecycle: &mut InterfaceLifecycleActorHandoff<CriticalSectionRawMutex>,
    authority: InterfaceIngressAuthority,
    retained_available: Option<AvailableIngressBuffer>,
    retained_sealed: Option<SealedIngressPacket>,
) -> ! {
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
