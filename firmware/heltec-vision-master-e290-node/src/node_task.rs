//! Permanent transport-neutral node owner task.

use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::rng::Trng;
use log::{error, info, warn};
use reticulum_announce_clock::BootEpoch;
use reticulum_device_api_pairing::{
    AbortCurrentResponse, AbortResult, ActivateFailure, ActivateResponse, BeginResponse,
    PairingFailure, PairingRequest, PairingResponse, ProofStartResponse,
};
use reticulum_device_api_pairing_control::{ControlRequest, ControlResponse, InitializeResult};
use reticulum_device_api_pairing_policy::{
    AcquirePairingExclusive, ButtonEffect, ConnectionId, ExclusiveAcquireOutcome,
    MonotonicMillis as PairingMillis, PolicyEvent,
};
use reticulum_heltec_vision_master_e290_node::{
    announce_time::BootAnnounceClock,
    causal_pairing_frontier::{PairingCommandLane, select_pairing_command_lane},
    credential_pairing::{CredentialPairingStatus, PairingMutation},
    credential_runtime::CredentialInitializationStatus,
    durability_policy::{AuthorizedFrameDurability, DurabilityServiceState},
    live_pairing_handoff::{LivePairingCommand, LivePairingReply, NodeLivePairingHandoff},
    live_pairing_node::{LivePairingOperation, LivePairingOperationStep},
    pairing_control_handoff::{
        ButtonObservationReply, ExclusiveAcquisitionReply, LifecycleAcknowledgement,
        NodePairingHandoff, PairingControlCommand, PairingControlReply, PairingControlReplyKind,
    },
    pairing_control_mapping::{
        public_initialization_drive, public_initialization_refusal, public_initialization_status,
    },
};
use reticulum_interface_router::{InterfaceDescriptor, SealedIngressPacket};
use reticulum_node_core::{
    AuthorizedFrameObservation, MonotonicMillis, MonotonicSeconds, NodeActions,
    ReceiptCorrelationError, TxLeaseDeadline,
};
use reticulum_storage_actor::{DriveError, ProjectorOperationError};
use reticulum_submission_runtime::{FrameOfferProgress, RuntimeError, RuntimeStep};
use reticulum_tx_handoff::AuthorizedFrameNodeHandoff;
use reticulum_tx_supervisor::{
    NodeInterfaceAnnounceFlushResult, NodeInterfaceIngressActionFault,
    NodeInterfaceIngressRecycleFault, NodeInterfaceIngressStep, NodeInterfaceOrdinaryOfferFailure,
    NodeInterfaceSupervisorTransition, NodeInterfaceTickResult,
};

use crate::{
    LORA_ONLINE, ProductSupervisor, RADIO_READY, config,
    platform_storage::{
        ProductCredentialInitializationPort, ProductInitializationDrive,
        ProductInitializationRequest, ProductLivePairingAdmission, ProductStorageCoordinator,
        ProductSubmissionDrive,
    },
};

type RetainedActions = (
    NodeActions,
    reticulum_tx_supervisor::OrdinaryRouterAdmission,
);
type QuarantinedIngressBuffer = (NodeInterfaceIngressRecycleFault, SealedIngressPacket);

enum ActionOfferHandling {
    Retry(RetainedActions),
    RetainAndDrain(RetainedActions),
}

enum ActionRetryStep {
    Accepted,
    Busy,
    Terminal,
}

struct IngressDrainState<'a> {
    observed_recycle_fault: &'a mut Option<NodeInterfaceIngressRecycleFault>,
    terminal_correlation_fault: &'a mut Option<ReceiptCorrelationError>,
    correlation_recycle_pending: &'a mut bool,
    fail_closed_draining: &'a mut bool,
    quarantined_actions: &'a mut Option<RetainedActions>,
    quarantined_ingress_buffer: &'a mut Option<QuarantinedIngressBuffer>,
    local_quarantine_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveRequestKind {
    Begin,
    ProofStart,
    Activate,
    AbortCurrent,
}

impl LiveRequestKind {
    const fn from_request(request: &PairingRequest) -> Self {
        match request {
            PairingRequest::Begin(_) => Self::Begin,
            PairingRequest::ProofStart(_) => Self::ProofStart,
            PairingRequest::Activate(_) => Self::Activate,
            PairingRequest::AbortCurrent(_) => Self::AbortCurrent,
        }
    }

    const fn expected_mutation(self) -> Option<PairingMutation> {
        match self {
            Self::Begin => Some(PairingMutation::AddPending),
            Self::ProofStart => None,
            Self::Activate => Some(PairingMutation::ActivatePending),
            Self::AbortCurrent => Some(PairingMutation::AbortPending),
        }
    }

    fn blocked_response(self, sequence: u64) -> PairingResponse {
        match self {
            Self::Begin => {
                PairingResponse::Begin(BeginResponse::failure(sequence, PairingFailure::Blocked))
            }
            Self::ProofStart => PairingResponse::ProofStart(ProofStartResponse::failure(
                sequence,
                PairingFailure::Blocked,
            )),
            Self::Activate => PairingResponse::Activate(ActivateResponse::failure(
                sequence,
                ActivateFailure::Blocked,
            )),
            Self::AbortCurrent => PairingResponse::AbortCurrent(AbortCurrentResponse::new(
                sequence,
                AbortResult::Blocked,
            )),
        }
    }

    const fn matches_response(self, response: &PairingResponse) -> bool {
        matches!(
            (self, response),
            (Self::Begin, PairingResponse::Begin(_))
                | (Self::ProofStart, PairingResponse::ProofStart(_))
                | (Self::Activate, PairingResponse::Activate(_))
                | (Self::AbortCurrent, PairingResponse::AbortCurrent(_))
        )
    }
}

struct PairingNodeState {
    pending_control_command: Option<PairingControlCommand>,
    pending_control_reply: Option<PairingControlReply>,
    pending_exclusive: Option<(ConnectionId, AcquirePairingExclusive)>,
    initialization_retry_not_before_ms: Option<u64>,
    pending_live_command: Option<LivePairingCommand>,
    pending_live_operation: Option<LivePairingOperation>,
    pending_live_reply: Option<LivePairingReply>,
    live_retry_not_before_ms: Option<u64>,
    live_lane_faulted: bool,
}

pub(crate) struct NodePairingHandoffs {
    control: NodePairingHandoff<CriticalSectionRawMutex>,
    live: NodeLivePairingHandoff<CriticalSectionRawMutex>,
}

impl NodePairingHandoffs {
    pub(crate) const fn new(
        control: NodePairingHandoff<CriticalSectionRawMutex>,
        live: NodeLivePairingHandoff<CriticalSectionRawMutex>,
    ) -> Self {
        Self { control, live }
    }

    fn into_parts(
        self,
    ) -> (
        NodePairingHandoff<CriticalSectionRawMutex>,
        NodeLivePairingHandoff<CriticalSectionRawMutex>,
    ) {
        (self.control, self.live)
    }
}

impl PairingNodeState {
    const fn new() -> Self {
        Self {
            pending_control_command: None,
            pending_control_reply: None,
            pending_exclusive: None,
            initialization_retry_not_before_ms: None,
            pending_live_command: None,
            pending_live_operation: None,
            pending_live_reply: None,
            live_retry_not_before_ms: None,
            live_lane_faulted: false,
        }
    }
}

#[embassy_executor::task]
pub async fn run(
    supervisor: &'static mut ProductSupervisor,
    storage: &'static mut ProductStorageCoordinator,
    pairing_handoffs: NodePairingHandoffs,
    mut frame_handoff: AuthorizedFrameNodeHandoff<CriticalSectionRawMutex>,
    offline_descriptor: InterfaceDescriptor,
    announce_epoch: BootEpoch,
    mut rng: Trng,
) {
    let (mut pairing_handoff, mut live_pairing_handoff) = pairing_handoffs.into_parts();
    RADIO_READY.wait().await;
    let online = match supervisor.set_interface_online(offline_descriptor.lease(), true) {
        Ok(descriptor) => descriptor,
        Err(reason) => {
            error!("e290-node stage=interface-online status=FAIL reason={reason:?}");
            fail_stop().await
        }
    };
    info!(
        "e290-node stage=interface-online status=PASS interface={} config=0x{:08x} mtu={} queue_depth={}",
        online.lease().interface().get(),
        online.config().get(),
        online.logical_mtu().get(),
        config::INTERFACE_QUEUE_DEPTH,
    );
    LORA_ONLINE.signal(());

    // These slots are interchangeable. Either can hold a locally
    // backpressured envelope or an exact coordinator-rejected envelope.
    let mut retry_actions_a: Option<RetainedActions> = None;
    let mut retry_actions_b: Option<RetainedActions> = None;
    let mut quarantined_actions: Option<RetainedActions> = None;
    let mut quarantined_ingress_buffer: Option<QuarantinedIngressBuffer> = None;
    let mut prefer_retry_b = true;
    let mut terminal_correlation_fault: Option<ReceiptCorrelationError> = None;
    let mut correlation_recycle_pending = false;
    let mut fail_closed_draining = false;
    let mut observed_terminal_transitions = 0_u8;
    let mut announce_clock = BootAnnounceClock::new(announce_epoch);
    let mut next_tick_seconds = now_seconds();
    let mut next_announce_seconds = now_seconds();
    let mut lane = 0_u8;
    let mut observed_recycle_fault: Option<NodeInterfaceIngressRecycleFault> = None;
    let mut durability_service =
        DurabilityServiceState::from_runtime_available(storage.submission_service_available());
    let mut retained_frame: Option<AuthorizedFrameObservation> = None;
    let mut pending_frame_acknowledgement: Option<AuthorizedFrameObservation> = None;
    let mut pairing = PairingNodeState::new();

    loop {
        let mut progressed = false;

        progressed |= step_pairing_frontier(
            storage,
            &mut pairing_handoff,
            &mut live_pairing_handoff,
            &mut pairing,
            &mut rng,
            now_millis(),
        );

        // Authorized-frame evidence outranks completion processing. The LoRa
        // actor does not return the owning completion until this task echoes
        // the identical observation after durable projection. Sending an
        // already-durable pending echo is deliberately independent of the
        // current service state; a later unrelated fault cannot revoke proof
        // that was established before this acknowledgement was queued.
        if let Some(observation) = pending_frame_acknowledgement.take() {
            match frame_handoff.acknowledgements().try_send(observation) {
                Ok(()) => {
                    info!(
                        "e290-node stage=authorized-frame status=DURABLE-ACK interface={} packet_len={}",
                        observation.interface().get(),
                        observation.packet_len(),
                    );
                    progressed = true;
                }
                Err(full) => pending_frame_acknowledgement = Some(full.into_inner()),
            }
        }
        if pending_frame_acknowledgement.is_none()
            && retained_frame.is_none()
            && let Some(observation) = frame_handoff.requests().try_receive()
        {
            retained_frame = Some(observation);
            let previous = durability_service;
            durability_service = durability_service.observe_authorized_frame_request();
            if !previous.is_active_owner_fail_stopped()
                && durability_service.is_active_owner_fail_stopped()
            {
                enter_active_owner_durability_fail_stop(
                    supervisor,
                    online,
                    &mut fail_closed_draining,
                    "frame-arrived-after-durable-service-disabled",
                );
            }
            progressed = true;
        }
        if durability_service.can_offer_authorized_frame()
            && let Some(observation) = retained_frame
        {
            match storage.offer_authorized_frame(observation) {
                Some(Ok(FrameOfferProgress::Retain)) => {}
                Some(Ok(FrameOfferProgress::Durable)) => {
                    retained_frame = None;
                    pending_frame_acknowledgement = Some(observation);
                    progressed = true;
                }
                Some(Err(reason)) => {
                    storage.disable_submission_service();
                    let previous = durability_service;
                    durability_service =
                        durability_service.permanent_failure(AuthorizedFrameDurability::Unresolved);
                    error!(
                        "e290-node stage=authorized-frame status=RETAINED-FAIL-STOP reason={reason:?} authorized_data_owner=retained radio_actor=fail-stop local_submission_admission=closed"
                    );
                    if !previous.is_active_owner_fail_stopped()
                        && durability_service.is_active_owner_fail_stopped()
                    {
                        enter_active_owner_durability_fail_stop(
                            supervisor,
                            online,
                            &mut fail_closed_draining,
                            "authorized-frame-projection-fault",
                        );
                    }
                    progressed = true;
                }
                None => {
                    storage.disable_submission_service();
                    let previous = durability_service;
                    durability_service =
                        durability_service.permanent_failure(AuthorizedFrameDurability::Unresolved);
                    error!(
                        "e290-node stage=authorized-frame status=RETAINED-FAIL-STOP reason=runtime-unavailable authorized_data_owner=retained radio_actor=fail-stop local_submission_admission=closed"
                    );
                    if !previous.is_active_owner_fail_stopped()
                        && durability_service.is_active_owner_fail_stopped()
                    {
                        enter_active_owner_durability_fail_stop(
                            supervisor,
                            online,
                            &mut fail_closed_draining,
                            "authorized-frame-runtime-unavailable",
                        );
                    }
                    progressed = true;
                }
            }
        }

        let mut storage_step_attempted = false;
        for _ in 0..config::NODE_MAX_IMMEDIATE_PASSES {
            let retry_slot_available = retry_actions_a.is_none() || retry_actions_b.is_none();
            if !fail_closed_draining
                && retry_slot_available
                && let Some(rejected) = supervisor.take_rejected_actions()
            {
                let reason = rejected.reason();
                let (actions, elapsed_admission) = rejected.into_parts();
                let retained = match config::rejected_action_disposition(reason) {
                    config::RejectedActionDisposition::RefreshDeadline => {
                        warn!(
                            "e290-node stage=ordinary-admission status=RETRY-WITH-FRESH-DEADLINE reason={reason:?}"
                        );
                        (actions, config::ordinary_admission(now_millis()))
                    }
                    config::RejectedActionDisposition::FailStop => {
                        error!(
                            "e290-node stage=ordinary-admission status=TERMINAL reason={reason:?} packets={} events={} unroutable={} action=retain-rejected-owner-and-fail-closed-drain",
                            actions.packets.len(),
                            actions.events.len(),
                            actions.unroutable_packets,
                        );
                        fail_closed_draining = true;
                        (actions, elapsed_admission)
                    }
                };
                if retry_actions_a.is_none() {
                    retry_actions_a = Some(retained);
                } else {
                    debug_assert!(retry_actions_b.is_none());
                    retry_actions_b = Some(retained);
                }
            }

            match lane {
                0 => {
                    let terminal_residue_can_move = quarantined_actions.is_none()
                        && supervisor.terminal_ingress_action_fault().is_some();
                    let terminal_buffer_can_move = quarantined_ingress_buffer.is_none()
                        && supervisor.terminal_ingress_recycle_fault().is_some();
                    let ingress_actions_pending = supervisor.ingress_actions_pending();
                    let retry_capacity_available =
                        retry_actions_a.is_none() || retry_actions_b.is_none();
                    let settling_ingress = supervisor.ingress_recycle_fault().is_some()
                        || terminal_residue_can_move
                        || terminal_buffer_can_move
                        || (!fail_closed_draining
                            && retry_capacity_available
                            && ingress_actions_pending);
                    // A fresh ingress packet can create another ordinary
                    // envelope. Do not dequeue one while both retry slots are
                    // occupied, or its later rejected output could have no
                    // local owner slot. Terminal drain only returns an exact
                    // RX buffer or moves an existing terminal residue.
                    if (!fail_closed_draining && retry_capacity_available) || settling_ingress {
                        let local_quarantine_available = quarantined_actions.is_none();
                        progressed |= step_ingress(
                            supervisor,
                            &mut rng,
                            IngressDrainState {
                                observed_recycle_fault: &mut observed_recycle_fault,
                                terminal_correlation_fault: &mut terminal_correlation_fault,
                                correlation_recycle_pending: &mut correlation_recycle_pending,
                                fail_closed_draining: &mut fail_closed_draining,
                                quarantined_actions: &mut quarantined_actions,
                                quarantined_ingress_buffer: &mut quarantined_ingress_buffer,
                                local_quarantine_available,
                            },
                        );
                    }
                }
                1 => {
                    let pass = supervisor.step(MonotonicMillis::new(now_millis()));
                    let transition = pass.transition();
                    match config::supervisor_transition_disposition(transition) {
                        config::SupervisorTransitionDisposition::Idle => {}
                        config::SupervisorTransitionDisposition::Progress => progressed = true,
                        config::SupervisorTransitionDisposition::TerminalFailClosedDrain => {
                            fail_closed_draining = true;
                            let observation = terminal_transition_observation(transition);
                            if observed_terminal_transitions & observation == 0 {
                                observed_terminal_transitions |= observation;
                                error!(
                                    "e290-node stage=node-supervisor status=FAIL-CLOSED-DRAIN transition={transition:?} action=deny-fresh-work-and-drain-retained-owners"
                                );
                                progressed = true;
                            }
                        }
                    }
                }
                2 => {
                    if !fail_closed_draining {
                        match config::action_retry_slot(
                            retry_actions_a.is_some(),
                            retry_actions_b.is_some(),
                            prefer_retry_b,
                        ) {
                            config::ActionRetrySlot::First => {
                                prefer_retry_b = true;
                                match retry_retained_actions(
                                    supervisor,
                                    &mut retry_actions_a,
                                    "action-retry-a",
                                ) {
                                    ActionRetryStep::Accepted => progressed = true,
                                    ActionRetryStep::Busy => {}
                                    ActionRetryStep::Terminal => {
                                        fail_closed_draining = true;
                                        progressed = true;
                                    }
                                }
                            }
                            config::ActionRetrySlot::Second => {
                                prefer_retry_b = false;
                                match retry_retained_actions(
                                    supervisor,
                                    &mut retry_actions_b,
                                    "action-retry-b",
                                ) {
                                    ActionRetryStep::Accepted => progressed = true,
                                    ActionRetryStep::Busy => {}
                                    ActionRetryStep::Terminal => {
                                        fail_closed_draining = true;
                                        progressed = true;
                                    }
                                }
                            }
                            config::ActionRetrySlot::None => {
                                let now = now_seconds();
                                if !supervisor.ingress_actions_pending() && now >= next_tick_seconds
                                {
                                    next_tick_seconds =
                                        now.saturating_add(config::PROTOCOL_TICK_INTERVAL_SECONDS);
                                    match supervisor.tick(
                                        MonotonicSeconds::new(now),
                                        config::ordinary_admission(now_millis()),
                                        &mut rng,
                                    ) {
                                        NodeInterfaceTickResult::Accepted(accepted) => {
                                            let report = accepted.into_report();
                                            if let Some(fault) = report.correlation_fault {
                                                observe_terminal_correlation_fault(
                                                    &mut terminal_correlation_fault,
                                                    fault,
                                                    "protocol-tick",
                                                );
                                                fail_closed_draining = true;
                                            }
                                            progressed = true;
                                        }
                                        NodeInterfaceTickResult::ActionOfferRejected(failure) => {
                                            let (report, failure) = failure.into_parts();
                                            if let Some(fault) = report.correlation_fault {
                                                observe_terminal_correlation_fault(
                                                    &mut terminal_correlation_fault,
                                                    fault,
                                                    "protocol-tick",
                                                );
                                                fail_closed_draining = true;
                                            }
                                            match handle_action_offer_failure(
                                                failure,
                                                "protocol-tick",
                                            ) {
                                                ActionOfferHandling::Retry(retained) => {
                                                    retry_actions_a = Some(retained)
                                                }
                                                ActionOfferHandling::RetainAndDrain(retained) => {
                                                    retry_actions_a = Some(retained);
                                                    fail_closed_draining = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                3 => {
                    let now = now_seconds();
                    if retry_actions_a.is_none()
                        && retry_actions_b.is_none()
                        && quarantined_actions.is_none()
                        && !supervisor.ingress_actions_pending()
                        && !fail_closed_draining
                        && !announce_clock.is_exhausted()
                        && now >= next_announce_seconds
                    {
                        next_announce_seconds =
                            now.saturating_add(config::ANNOUNCE_INTERVAL_SECONDS);
                        if let Some(emitted_at) = announce_clock.next_emission() {
                            match supervisor.queue_announce(None, emitted_at, &mut rng) {
                                Ok(()) => {
                                    announce_clock.mark_queued();
                                    match supervisor.flush_announces(
                                        MonotonicSeconds::new(now),
                                        config::ordinary_admission(now_millis()),
                                        &mut rng,
                                    ) {
                                        NodeInterfaceAnnounceFlushResult::Accepted => {
                                            info!(
                                                "e290-node stage=announce status=QUEUED emission_time={} boot_epoch={}",
                                                emitted_at.get(),
                                                announce_clock.epoch().get(),
                                            );
                                            progressed = true;
                                        }
                                        NodeInterfaceAnnounceFlushResult::ActionOfferRejected(
                                            failure,
                                        ) => {
                                            match handle_action_offer_failure(failure, "announce") {
                                                ActionOfferHandling::Retry(retained) => {
                                                    retry_actions_a = Some(retained)
                                                }
                                                ActionOfferHandling::RetainAndDrain(retained) => {
                                                    retry_actions_a = Some(retained);
                                                    fail_closed_draining = true;
                                                }
                                            }
                                        }
                                    }
                                    if announce_clock.is_exhausted() {
                                        error!(
                                            "e290-node stage=announce status=LOCAL-EMISSION-EXHAUSTED boot_epoch={} action=suppress-future-local-announces",
                                            announce_clock.epoch().get(),
                                        );
                                    }
                                }
                                Err(reason) => {
                                    warn!(
                                        "e290-node stage=announce status=DEFERRED reason={reason:?}"
                                    )
                                }
                            }
                        }
                    }
                }
                _ => {
                    let owner_now = now_millis();
                    if !storage_step_attempted && durability_service.storage_step_due(owner_now) {
                        storage_step_attempted = true;
                        let deadline = TxLeaseDeadline::new(MonotonicMillis::new(
                            owner_now.saturating_add(config::SUBMISSION_OWNER_LEASE_MS),
                        ));
                        match storage.drive_submission_step(
                            supervisor,
                            MonotonicSeconds::new(now_seconds()),
                            MonotonicMillis::new(owner_now),
                            deadline,
                            &mut rng,
                        ) {
                            ProductSubmissionDrive::Runtime(Ok(RuntimeStep::Idle)) => {
                                durability_service = durability_service.runtime_progress();
                            }
                            ProductSubmissionDrive::Runtime(Ok(step)) => {
                                durability_service = durability_service.runtime_progress();
                                info!(
                                    "e290-node stage=durable-submission status=PROGRESS step={step:?}"
                                );
                                progressed = true;
                            }
                            ProductSubmissionDrive::Runtime(Err(RuntimeError::Storage(
                                DriveError::Backend(reason),
                            ))) => {
                                let retry_not_before_ms =
                                    owner_now.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                durability_service =
                                    durability_service.retry_at(retry_not_before_ms);
                                warn!(
                                    "e290-node stage=durable-submission status=RETRY reason=backend:{reason:?} retry_not_before_ms={retry_not_before_ms}"
                                );
                            }
                            ProductSubmissionDrive::Runtime(Err(RuntimeError::Storage(
                                DriveError::Busy { pending },
                            )))
                            | ProductSubmissionDrive::Runtime(Err(RuntimeError::Projection(
                                ProjectorOperationError::Busy { pending },
                            ))) => {
                                let retry_not_before_ms =
                                    owner_now.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                durability_service =
                                    durability_service.retry_at(retry_not_before_ms);
                                warn!(
                                    "e290-node stage=durable-submission status=RETRY reason=busy:{pending:?} retry_not_before_ms={retry_not_before_ms}"
                                );
                            }
                            ProductSubmissionDrive::DeferredForCredentialMutation => {
                                let retry_not_before_ms =
                                    owner_now.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                durability_service =
                                    durability_service.retry_at(retry_not_before_ms);
                                info!(
                                    "e290-node stage=durable-submission status=DEFERRED reason=credential-mutation-in-flight retry_not_before_ms={retry_not_before_ms}"
                                );
                            }
                            ProductSubmissionDrive::Runtime(Err(reason)) => {
                                storage.disable_submission_service();
                                let previous = durability_service;
                                durability_service = durability_service.permanent_failure(
                                    authorized_frame_durability(
                                        &retained_frame,
                                        &pending_frame_acknowledgement,
                                    ),
                                );
                                if durability_service.is_active_owner_fail_stopped() {
                                    error!(
                                        "e290-node stage=durable-submission status=FAIL-STOP reason={reason:?} authorized_data_owner=retained radio_actor=fail-stop local_submission_admission=closed"
                                    );
                                    if !previous.is_active_owner_fail_stopped() {
                                        enter_active_owner_durability_fail_stop(
                                            supervisor,
                                            online,
                                            &mut fail_closed_draining,
                                            "durable-runtime-fault-with-active-owner",
                                        );
                                    }
                                } else {
                                    error!(
                                        "e290-node stage=durable-submission status=DISABLED reason={reason:?} radio_actor=continue-route-only local_submission_admission=closed"
                                    );
                                }
                                progressed = true;
                            }
                            ProductSubmissionDrive::RuntimeUnavailable => {
                                storage.disable_submission_service();
                                let previous = durability_service;
                                durability_service = durability_service.permanent_failure(
                                    authorized_frame_durability(
                                        &retained_frame,
                                        &pending_frame_acknowledgement,
                                    ),
                                );
                                if durability_service.is_active_owner_fail_stopped() {
                                    error!(
                                        "e290-node stage=durable-submission status=FAIL-STOP reason=runtime-unavailable authorized_data_owner=retained radio_actor=fail-stop local_submission_admission=closed"
                                    );
                                    if !previous.is_active_owner_fail_stopped() {
                                        enter_active_owner_durability_fail_stop(
                                            supervisor,
                                            online,
                                            &mut fail_closed_draining,
                                            "durable-runtime-unavailable-with-active-owner",
                                        );
                                    }
                                } else {
                                    error!(
                                        "e290-node stage=durable-submission status=DISABLED reason=runtime-unavailable radio_actor=continue-route-only local_submission_admission=closed"
                                    );
                                }
                                progressed = true;
                            }
                        }
                    }
                }
            }
            lane = if lane + 1 == config::NODE_FAIR_LANES {
                0
            } else {
                lane + 1
            };

            // Client delivery is intentionally not part of this first LoRa
            // vertical slice. Drain scalar/event output so transport progress
            // cannot deadlock; the runbook records durable client delivery as
            // a product blocker rather than silently treating this as final.
            if let Some(output) = supervisor.take_non_packet_actions() {
                info!(
                    "e290-node stage=local-output status=DRAINED events={} unroutable={} client_delivery=not-yet-wired",
                    output.events.len(),
                    output.unroutable_packets,
                );
                progressed = true;
            }
            if fail_closed_draining {
                // Keep terminal local evidence live while portable machines
                // finish forced denials and exact completion return.
                let _ = (
                    retry_actions_a.as_ref(),
                    retry_actions_b.as_ref(),
                    quarantined_actions.as_ref(),
                    quarantined_ingress_buffer.as_ref(),
                    terminal_correlation_fault.as_ref(),
                );
            }
        }

        yield_now().await;
        if !progressed {
            // Temporary bounded polling until the aggregate exposes one
            // combined ingress/completion/deadline wait surface.
            Timer::after(Duration::from_millis(config::NODE_POLL_INTERVAL_MS)).await;
        }
    }
}

fn step_pairing_frontier(
    storage: &mut ProductStorageCoordinator,
    control_handoff: &mut NodePairingHandoff<CriticalSectionRawMutex>,
    live_handoff: &mut NodeLivePairingHandoff<CriticalSectionRawMutex>,
    state: &mut PairingNodeState,
    rng: &mut Trng,
    now_millis: u64,
) -> bool {
    let mut progressed = flush_pairing_reply(control_handoff, &mut state.pending_control_reply);
    progressed |= flush_live_pairing_reply(live_handoff, &mut state.pending_live_reply);

    let initialization_status = storage.initialization_status();
    if matches!(
        initialization_status,
        CredentialInitializationStatus::InFlight { .. }
    ) && state
        .initialization_retry_not_before_ms
        .is_none_or(|deadline| now_millis >= deadline)
    {
        let _ = drive_initialization_and_schedule(
            storage,
            &mut state.initialization_retry_not_before_ms,
            now_millis,
        );
        progressed = true;
    } else if !matches!(
        initialization_status,
        CredentialInitializationStatus::InFlight { .. }
    ) {
        state.initialization_retry_not_before_ms = None;
    }

    if state.pending_control_command.is_none() && state.pending_control_reply.is_none() {
        state.pending_control_command = control_handoff.try_receive_command();
    }
    if state.pending_live_command.is_none()
        && state.pending_live_operation.is_none()
        && state.pending_live_reply.is_none()
    {
        state.pending_live_command = live_handoff.try_receive_command();
    }

    // Both command channels have the same sole USB producer. Compare captured
    // event time anyway so a queued earlier scalar event cannot be overtaken.
    // Equal timestamps choose live work: the USB owner only enqueues a scalar
    // command concurrently after transferring that live request, most notably
    // a bus-reset disconnect observed in the same millisecond.
    for _ in 0..2 {
        let next_lane = select_pairing_command_lane(
            state
                .pending_live_command
                .as_ref()
                .map(|live| live.at().get()),
            state
                .pending_control_command
                .as_ref()
                .map(|control| control.at().get()),
        );
        if next_lane == Some(PairingCommandLane::Live) {
            progressed |= admit_live_pairing_command(storage, state, rng);
            if state.pending_live_command.is_some() {
                // Journal-owned mutation is causally after this request. Keep
                // later policy events and timeout polls behind the retained
                // command until journal ownership settles.
                break;
            }
        } else if next_lane == Some(PairingCommandLane::Control) {
            let command = state
                .pending_control_command
                .take()
                .expect("the selected control command is retained");
            handle_pairing_control_command(storage, state, command, now_millis);
            progressed = true;
        } else {
            break;
        }
    }

    progressed |= drive_live_pairing_and_schedule(storage, state, now_millis);
    progressed |= flush_pairing_reply(control_handoff, &mut state.pending_control_reply);
    progressed |= flush_live_pairing_reply(live_handoff, &mut state.pending_live_reply);

    if state.pending_live_command.is_none() && state.pending_control_command.is_none() {
        progressed |= poll_pairing_timeout(storage, &mut state.pending_exclusive, now_millis);
    }
    progressed
}

fn handle_pairing_control_command(
    storage: &mut ProductStorageCoordinator,
    state: &mut PairingNodeState,
    command: PairingControlCommand,
    now_millis: u64,
) {
    let connection = command.connection();
    let reply_kind = match command {
        PairingControlCommand::Connected { at, connection } => {
            state.pending_exclusive = None;
            let _ = storage.pairing_connected(at, connection);
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Connected)
        }
        PairingControlCommand::Disconnected { at, connection } => {
            if state
                .pending_exclusive
                .as_ref()
                .is_some_and(|(owner, _)| *owner == connection)
            {
                state.pending_exclusive = None;
            }
            let _ = storage.pairing_disconnected(at, connection);
            PairingControlReplyKind::Lifecycle(LifecycleAcknowledgement::Disconnected)
        }
        PairingControlCommand::ObserveButton {
            at,
            connection,
            level,
        } => {
            let outcome = match storage.pairing_observe_button(at, level) {
                Some(ButtonEffect::AcquirePairingExclusive(capability)) => {
                    state.pending_exclusive = Some((connection, capability));
                    ButtonObservationReply::AcquireExclusive
                }
                Some(ButtonEffect::Closed(_) | ButtonEffect::Fault(_)) => {
                    if state
                        .pending_exclusive
                        .as_ref()
                        .is_some_and(|(owner, _)| *owner == connection)
                    {
                        state.pending_exclusive = None;
                    }
                    ButtonObservationReply::Observed
                }
                Some(ButtonEffect::None) | None => ButtonObservationReply::Observed,
            };
            PairingControlReplyKind::Button(outcome)
        }
        PairingControlCommand::ExclusiveAcquired { at, connection } => {
            let outcome = if state
                .pending_exclusive
                .as_ref()
                .is_some_and(|(owner, _)| *owner == connection)
            {
                let (_, capability) = state
                    .pending_exclusive
                    .take()
                    .expect("the matching exclusive capability was retained");
                match storage.pairing_exclusive_acquired(at, capability) {
                    Some(ExclusiveAcquireOutcome::Opened(_)) => ExclusiveAcquisitionReply::Opened,
                    Some(ExclusiveAcquireOutcome::Closed(_)) => ExclusiveAcquisitionReply::Closed,
                    Some(ExclusiveAcquireOutcome::Stale | ExclusiveAcquireOutcome::Fault(_))
                    | None => ExclusiveAcquisitionReply::Refused,
                }
            } else {
                ExclusiveAcquisitionReply::Refused
            };
            PairingControlReplyKind::Exclusive(outcome)
        }
        PairingControlCommand::Control {
            at,
            connection,
            request,
        } => {
            let response = match request {
                ControlRequest::Status { sequence } => ControlResponse::status(
                    sequence,
                    public_initialization_status(storage.initialization_status()),
                ),
                ControlRequest::Initialize { sequence } => {
                    let result = match storage.request_initialization(at, connection) {
                        ProductInitializationRequest::IdentityUnavailable
                        | ProductInitializationRequest::DeferredForJournalMutation => {
                            InitializeResult::Retrying
                        }
                        ProductInitializationRequest::Runtime(Err(refusal)) => {
                            public_initialization_refusal(refusal)
                        }
                        ProductInitializationRequest::Runtime(Ok(_accepted)) => {
                            drive_initialization_and_schedule(
                                storage,
                                &mut state.initialization_retry_not_before_ms,
                                now_millis,
                            )
                        }
                    };
                    ControlResponse::initialize(sequence, result)
                }
            };
            PairingControlReplyKind::Control(response)
        }
    };
    debug_assert!(state.pending_control_reply.is_none());
    state.pending_control_reply = Some(PairingControlReply::new(connection, reply_kind));
}

fn admit_live_pairing_command(
    storage: &mut ProductStorageCoordinator,
    state: &mut PairingNodeState,
    rng: &mut Trng,
) -> bool {
    let command = state
        .pending_live_command
        .take()
        .expect("live admission requires one retained command");
    let at = command.at();
    let connection = command.connection();
    let request = command.into_request();
    let sequence = request.sequence();
    let kind = LiveRequestKind::from_request(&request);

    if state.live_lane_faulted {
        state.pending_live_reply = Some(LivePairingReply::new(
            connection,
            kind.blocked_response(sequence),
        ));
        return true;
    }

    match storage.request_live_pairing(at, connection, request, rng) {
        ProductLivePairingAdmission::DeferredForJournalMutation(request) => {
            state.pending_live_command = Some(LivePairingCommand::new(at, connection, request));
        }
        ProductLivePairingAdmission::Immediate(response) => {
            if response.sequence() == sequence && kind.matches_response(&response) {
                state.pending_live_reply = Some(LivePairingReply::new(connection, response));
            } else {
                drop(response);
                state.live_lane_faulted = true;
                state.pending_live_reply = Some(LivePairingReply::new(
                    connection,
                    kind.blocked_response(sequence),
                ));
            }
        }
        ProductLivePairingAdmission::MutationAccepted(mutation) => {
            if kind.expected_mutation() == Some(mutation) {
                state.pending_live_operation =
                    Some(LivePairingOperation::new(connection, sequence, mutation));
                state.live_retry_not_before_ms = None;
            } else {
                state.live_lane_faulted = true;
                state.pending_live_reply = Some(LivePairingReply::new(
                    connection,
                    kind.blocked_response(sequence),
                ));
            }
        }
    }
    true
}

fn drive_live_pairing_and_schedule(
    storage: &mut ProductStorageCoordinator,
    state: &mut PairingNodeState,
    now_millis: u64,
) -> bool {
    if state.pending_live_reply.is_some()
        || state
            .live_retry_not_before_ms
            .is_some_and(|deadline| now_millis < deadline)
    {
        return false;
    }
    let status = storage.live_pairing_status();
    let drive_required = state.pending_live_operation.is_some()
        || matches!(
            status,
            CredentialPairingStatus::AwaitingCleanStore(_)
                | CredentialPairingStatus::MutationPrepared(_)
                | CredentialPairingStatus::ReconcileRequired(_)
                | CredentialPairingStatus::CleanupRequired
        );
    if !drive_required {
        if status == CredentialPairingStatus::Blocked {
            state.live_lane_faulted = true;
        }
        state.live_retry_not_before_ms = None;
        return false;
    }

    let outcome = storage.drive_live_pairing();
    let Some(operation) = state.pending_live_operation.take() else {
        // Cleanup without a request is expected. Any mutation-bearing state
        // without correlation is an internal fault, but must keep driving to a
        // terminal durable state so secret/store ownership is not abandoned.
        match outcome {
            reticulum_heltec_vision_master_e290_node::credential_pairing::CredentialPairingDriveOutcome::CleanupCompleted => {
                state.live_retry_not_before_ms = None;
            }
            reticulum_heltec_vision_master_e290_node::credential_pairing::CredentialPairingDriveOutcome::Retry {
                mutation,
                ..
            } => {
                if mutation.is_some() {
                    state.live_lane_faulted = true;
                }
                state.live_retry_not_before_ms =
                    Some(now_millis.saturating_add(config::STORAGE_RETRY_BACKOFF_MS));
            }
            other => {
                drop(other);
                state.live_lane_faulted = true;
                state.live_retry_not_before_ms = None;
            }
        }
        return true;
    };

    match operation.apply(outcome) {
        LivePairingOperationStep::Progress(operation) => {
            state.pending_live_operation = Some(operation);
            state.live_retry_not_before_ms = None;
        }
        LivePairingOperationStep::Retry { operation, reason } => {
            state.pending_live_operation = Some(operation);
            state.live_retry_not_before_ms =
                Some(now_millis.saturating_add(config::STORAGE_RETRY_BACKOFF_MS));
            warn!("e290-node stage=live-pairing status=RETRY reason={reason:?}");
        }
        LivePairingOperationStep::Reply(reply) => {
            state.pending_live_reply = Some(reply);
            state.live_retry_not_before_ms = None;
        }
        LivePairingOperationStep::Fault(reply) => {
            state.live_lane_faulted = true;
            state.pending_live_reply = Some(reply);
            state.live_retry_not_before_ms = None;
            error!(
                "e290-node stage=live-pairing status=FAIL-CLOSED reason=drive-correlation-mismatch"
            );
        }
    }
    true
}

fn poll_pairing_timeout(
    storage: &mut ProductStorageCoordinator,
    pending_exclusive: &mut Option<(ConnectionId, AcquirePairingExclusive)>,
    now_millis: u64,
) -> bool {
    // Run after any queued command carrying an earlier observation timestamp,
    // so asynchronous handoff delay cannot manufacture a clock regression.
    // This node-owned poll still runs every loop even when a host stops reading
    // and back-pressures the USB TX FIFO indefinitely.
    if matches!(
        storage.pairing_poll_timeout(PairingMillis::new(now_millis)),
        Some(PolicyEvent::Closed(_) | PolicyEvent::Fault(_))
    ) {
        *pending_exclusive = None;
        true
    } else {
        false
    }
}

fn flush_pairing_reply(
    handoff: &mut NodePairingHandoff<CriticalSectionRawMutex>,
    pending_reply: &mut Option<PairingControlReply>,
) -> bool {
    let Some(reply) = pending_reply.take() else {
        return false;
    };
    match handoff.try_send_reply(reply) {
        Ok(()) => true,
        Err(pressure) => {
            *pending_reply = Some(pressure.into_inner());
            false
        }
    }
}

fn flush_live_pairing_reply(
    handoff: &mut NodeLivePairingHandoff<CriticalSectionRawMutex>,
    pending_reply: &mut Option<LivePairingReply>,
) -> bool {
    let Some(reply) = pending_reply.take() else {
        return false;
    };
    match handoff.try_send_reply(reply) {
        Ok(()) => true,
        Err(pressure) => {
            *pending_reply = Some(pressure.into_inner());
            false
        }
    }
}

fn drive_initialization_and_schedule(
    storage: &mut ProductStorageCoordinator,
    retry_not_before_ms: &mut Option<u64>,
    now_millis: u64,
) -> InitializeResult {
    let result = match storage.drive_initialization() {
        ProductInitializationDrive::IdentityUnavailable => InitializeResult::Retrying,
        ProductInitializationDrive::Runtime(outcome) => public_initialization_drive(outcome),
    };
    if result == InitializeResult::Retrying
        && matches!(
            storage.initialization_status(),
            CredentialInitializationStatus::InFlight { .. }
        )
    {
        *retry_not_before_ms = Some(now_millis.saturating_add(config::STORAGE_RETRY_BACKOFF_MS));
    } else {
        *retry_not_before_ms = None;
    }
    result
}

fn authorized_frame_durability(
    retained: &Option<AuthorizedFrameObservation>,
    acknowledgement: &Option<AuthorizedFrameObservation>,
) -> AuthorizedFrameDurability {
    if retained.is_some() {
        AuthorizedFrameDurability::Unresolved
    } else if acknowledgement.is_some() {
        AuthorizedFrameDurability::DurableAcknowledgementPending
    } else {
        AuthorizedFrameDurability::None
    }
}

fn enter_active_owner_durability_fail_stop(
    supervisor: &mut ProductSupervisor,
    online: InterfaceDescriptor,
    fail_closed_draining: &mut bool,
    trigger: &'static str,
) {
    match supervisor.set_interface_online(online.lease(), false) {
        Ok(offline) => error!(
            "e290-node stage=durability-policy status=ACTIVE-OWNER-FAIL-STOP trigger={trigger} interface={} generation={} online={} action=retain-frame-completion-ticket-and-deny-fresh-work",
            offline.lease().interface().get(),
            offline.lease().generation().get(),
            offline.is_online(),
        ),
        Err(reason) => error!(
            "e290-node stage=durability-policy status=FAIL trigger={trigger} reason=interface-offline:{reason:?} action=retain-frame-completion-ticket-and-fail-closed-drain"
        ),
    }
    *fail_closed_draining = true;
}

fn step_ingress(
    supervisor: &mut ProductSupervisor,
    rng: &mut Trng,
    mut state: IngressDrainState<'_>,
) -> bool {
    match supervisor.step_ingress(
        MonotonicSeconds::new(now_seconds()),
        config::ordinary_admission(now_millis()),
        rng,
    ) {
        NodeInterfaceIngressStep::Idle => false,
        NodeInterfaceIngressStep::RecycleBackpressured(fault) => {
            observe_retryable_recycle_fault(&mut *state.observed_recycle_fault, fault);
            false
        }
        NodeInterfaceIngressStep::TerminalRecyclePending(fault) => {
            *state.fail_closed_draining = true;
            quarantine_terminal_ingress_buffer(
                supervisor,
                fault,
                &mut *state.quarantined_ingress_buffer,
            )
        }
        NodeInterfaceIngressStep::ActionsBackpressured(_) => false,
        NodeInterfaceIngressStep::TerminalActionsPending(fault) => {
            *state.fail_closed_draining = true;
            if state.local_quarantine_available {
                quarantine_terminal_ingress_actions(
                    supervisor,
                    fault,
                    &mut *state.quarantined_actions,
                )
            } else {
                error!(
                    "e290-node stage=ingress-actions status=TERMINAL-RETAINED reason={fault:?} owner=supervisor-residue action=fail-closed-drain"
                );
                false
            }
        }
        NodeInterfaceIngressStep::RouteRejected {
            reason,
            recycle_fault,
            ..
        } => {
            if let Some(fault) = recycle_fault {
                handle_ingress_recycle_fault(supervisor, &mut state, fault);
            }
            warn!("e290-node stage=ingress status=ROUTE-REJECTED reason={reason:?}");
            true
        }
        NodeInterfaceIngressStep::CorrelationRejected {
            reason,
            recycle_fault,
            ..
        } => {
            *state.fail_closed_draining = true;
            observe_terminal_correlation_fault(
                &mut *state.terminal_correlation_fault,
                reason,
                "ingress",
            );
            let retryable_recycle_pending = match recycle_fault {
                Some(fault) if fault.is_retryable() => {
                    observe_retryable_recycle_fault(&mut *state.observed_recycle_fault, fault);
                    true
                }
                Some(fault) => {
                    let _ = quarantine_terminal_ingress_buffer(
                        supervisor,
                        fault,
                        &mut *state.quarantined_ingress_buffer,
                    );
                    false
                }
                None => false,
            };
            match config::terminal_ingress_disposition(retryable_recycle_pending) {
                config::TerminalIngressDisposition::DeferUntilBufferRecycled => {
                    *state.correlation_recycle_pending = true;
                    error!(
                        "e290-node stage=ingress status=CORRELATION-REJECTED-DEFERRED reason={reason:?} action=recycle-exact-buffer-then-fail-closed-drain"
                    );
                }
                config::TerminalIngressDisposition::HandleTerminal => {
                    error!(
                        "e290-node stage=ingress status=CORRELATION-REJECTED reason={reason:?} action=fail-closed-drain"
                    );
                }
            }
            true
        }
        NodeInterfaceIngressStep::Processed(processed) => {
            let recycle_fault = processed.recycle_fault();
            let retryable_recycle_pending = match recycle_fault {
                Some(fault) if fault.is_retryable() => {
                    observe_retryable_recycle_fault(&mut *state.observed_recycle_fault, fault);
                    true
                }
                Some(fault) => {
                    *state.fail_closed_draining = true;
                    let _ = quarantine_terminal_ingress_buffer(
                        supervisor,
                        fault,
                        &mut *state.quarantined_ingress_buffer,
                    );
                    false
                }
                None => false,
            };
            let terminal_action_fault = processed.terminal_action_fault();
            let _ = processed.into_report();
            if let Some(fault) = terminal_action_fault {
                *state.fail_closed_draining = true;
                match config::terminal_ingress_disposition(retryable_recycle_pending) {
                    config::TerminalIngressDisposition::DeferUntilBufferRecycled => {
                        warn!(
                            "e290-node stage=ingress-actions status=TERMINAL-DEFERRED reason={fault:?} owner=supervisor-residue action=recycle-exact-buffer-before-quarantine"
                        );
                    }
                    config::TerminalIngressDisposition::HandleTerminal => {
                        if state.local_quarantine_available {
                            let _ = quarantine_terminal_ingress_actions(
                                supervisor,
                                fault,
                                &mut *state.quarantined_actions,
                            );
                        } else {
                            error!(
                                "e290-node stage=ingress-actions status=TERMINAL-RETAINED reason={fault:?} owner=supervisor-residue action=fail-closed-drain"
                            );
                        }
                    }
                }
            }
            true
        }
        NodeInterfaceIngressStep::RecycleRetried(buffer) => {
            if let Some(fault) = state.observed_recycle_fault.take() {
                info!(
                    "e290-node stage=ingress-recycle status=RECOVERED buffer={buffer:?} previous_reason={:?}",
                    fault.reason(),
                );
            }
            if core::mem::take(&mut *state.correlation_recycle_pending)
                && let Some(reason) = state.terminal_correlation_fault.as_ref()
            {
                error!(
                    "e290-node stage=ingress status=CORRELATION-REJECTED-RECYCLED reason={reason:?} buffer={buffer:?} action=continue-fail-closed-drain"
                );
            }
            true
        }
        NodeInterfaceIngressStep::RetainedActionsAdmitted => true,
    }
}

fn handle_ingress_recycle_fault(
    supervisor: &mut ProductSupervisor,
    state: &mut IngressDrainState<'_>,
    fault: NodeInterfaceIngressRecycleFault,
) {
    if fault.is_retryable() {
        observe_retryable_recycle_fault(&mut *state.observed_recycle_fault, fault);
    } else {
        *state.fail_closed_draining = true;
        let _ = quarantine_terminal_ingress_buffer(
            supervisor,
            fault,
            &mut *state.quarantined_ingress_buffer,
        );
    }
}

fn observe_retryable_recycle_fault(
    observed: &mut Option<NodeInterfaceIngressRecycleFault>,
    fault: NodeInterfaceIngressRecycleFault,
) {
    debug_assert!(fault.is_retryable());
    if *observed != Some(fault) {
        warn!(
            "e290-node stage=ingress-recycle status=BACKPRESSURED buffer={:?} reason={:?} action=retain-and-retry",
            fault.buffer(),
            fault.reason(),
        );
        *observed = Some(fault);
    }
}

fn quarantine_terminal_ingress_buffer(
    supervisor: &mut ProductSupervisor,
    observed: NodeInterfaceIngressRecycleFault,
    quarantined: &mut Option<QuarantinedIngressBuffer>,
) -> bool {
    if quarantined.is_some() {
        error!(
            "e290-node stage=ingress-recycle status=TERMINAL-RETAINED observed={observed:?} owner=supervisor-residue reason=local-quarantine-occupied action=fail-closed-drain"
        );
        return false;
    }
    match supervisor.take_terminal_ingress_buffer() {
        Some((retained, packet)) => {
            error!(
                "e290-node stage=ingress-recycle status=TERMINAL-QUARANTINED observed={observed:?} retained={retained:?} buffer={:?} bytes={} action=fail-closed-drain",
                packet.id(),
                packet.len(),
            );
            *quarantined = Some((retained, packet));
            true
        }
        None => {
            error!(
                "e290-node stage=ingress-recycle status=FAIL observed={observed:?} reason=terminal-buffer-missing action=fail-closed-drain"
            );
            false
        }
    }
}

fn quarantine_terminal_ingress_actions(
    supervisor: &mut ProductSupervisor,
    observed: NodeInterfaceIngressActionFault,
    quarantined_actions: &mut Option<RetainedActions>,
) -> bool {
    match supervisor.take_terminal_ingress_actions() {
        Some(terminal) => {
            let retained = terminal.fault();
            let (actions, admission) = terminal.into_parts();
            error!(
                "e290-node stage=ingress-actions status=TERMINAL-QUARANTINED observed={observed:?} retained={retained:?} packets={} events={} unroutable={} action=fail-closed-drain",
                actions.packets.len(),
                actions.events.len(),
                actions.unroutable_packets,
            );
            debug_assert!(quarantined_actions.is_none());
            *quarantined_actions = Some((actions, admission));
            true
        }
        None => {
            error!(
                "e290-node stage=ingress-actions status=FAIL observed={observed:?} reason=terminal-owner-missing action=fail-closed-drain"
            );
            false
        }
    }
}

fn handle_action_offer_failure(
    failure: NodeInterfaceOrdinaryOfferFailure,
    stage: &'static str,
) -> ActionOfferHandling {
    let reason = failure.reason();
    let (actions, admission) = failure.into_parts();
    match config::ordinary_offer_disposition(reason) {
        config::OrdinaryOfferDisposition::RetryBusy => {
            ActionOfferHandling::Retry((actions, admission))
        }
        config::OrdinaryOfferDisposition::QuarantineAndDrain => {
            error!(
                "e290-node stage={stage} status=TERMINAL-OFFER reason={reason:?} packets={} events={} unroutable={} action=quarantine-owner-and-fail-closed-drain",
                actions.packets.len(),
                actions.events.len(),
                actions.unroutable_packets,
            );
            ActionOfferHandling::RetainAndDrain((actions, admission))
        }
    }
}

fn retry_retained_actions(
    supervisor: &mut ProductSupervisor,
    retained: &mut Option<RetainedActions>,
    stage: &'static str,
) -> ActionRetryStep {
    let Some((actions, admission)) = retained.take() else {
        error!(
            "e290-node stage={stage} status=FAIL reason=selected-retry-owner-missing action=fail-closed-drain"
        );
        return ActionRetryStep::Terminal;
    };
    match supervisor.try_offer_actions(actions, admission) {
        Ok(()) => ActionRetryStep::Accepted,
        Err(failure) => match handle_action_offer_failure(failure, stage) {
            ActionOfferHandling::Retry(owner) => {
                *retained = Some(owner);
                ActionRetryStep::Busy
            }
            ActionOfferHandling::RetainAndDrain(owner) => {
                *retained = Some(owner);
                ActionRetryStep::Terminal
            }
        },
    }
}

fn observe_terminal_correlation_fault(
    retained: &mut Option<ReceiptCorrelationError>,
    fault: ReceiptCorrelationError,
    source: &'static str,
) {
    if retained.is_none() {
        error!(
            "e290-node stage={source} status=TERMINAL-CORRELATION reason={fault:?} action=fail-closed-drain"
        );
        *retained = Some(fault);
    }
}

fn terminal_transition_observation(transition: NodeInterfaceSupervisorTransition) -> u8 {
    match transition {
        NodeInterfaceSupervisorTransition::Fault(_) => 1 << 0,
        NodeInterfaceSupervisorTransition::Data(_) => 1 << 1,
        NodeInterfaceSupervisorTransition::Ordinary(_) => 1 << 2,
        NodeInterfaceSupervisorTransition::DataPermit { .. } => 1 << 3,
        NodeInterfaceSupervisorTransition::OrdinaryPermit { .. } => 1 << 4,
        NodeInterfaceSupervisorTransition::CompletionAccepted { .. }
        | NodeInterfaceSupervisorTransition::Idle => 1 << 7,
    }
}

fn now_millis() -> u64 {
    Instant::now().as_millis()
}

fn now_seconds() -> u64 {
    Instant::now().as_secs()
}

async fn fail_stop() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
    }
}
