//! Permanent transport-neutral node owner task.

use core::mem;

use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::rng::Trng;
use log::{error, info, warn};
use reticulum_announce_clock::BootEpoch;
use reticulum_device_api::{DestinationHash as ApiDestinationHash, IdentitySummary};
use reticulum_device_api_handoff::{LocalApiReply, LocalApiRequest, NodeHandoff};
use reticulum_device_api_pairing::{
    AbortCurrentResponse, AbortResult, ActivateFailure, ActivateResponse, BeginResponse,
    PairingFailure, PairingRequest, PairingResponse, ProofStartResponse,
};
use reticulum_device_api_pairing_control::{ControlRequest, ControlResponse, InitializeResult};
use reticulum_device_api_pairing_policy::{
    AcquirePairingExclusive, ButtonEffect, ConnectionId, ExclusiveAcquireOutcome,
    MonotonicMillis as PairingMillis, PolicyEvent,
};
use reticulum_device_api_session::AuthenticatedGrant;
#[cfg(feature = "runtime-measurement-hil")]
use reticulum_heltec_vision_master_e290_node::runtime_measurement::{
    OperationKind as RuntimeOperationKind, RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE,
    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE, RuntimeProofTraceIngressDisposition,
    RuntimeProofTraceIngressMetadata, RuntimeProofTracePacketType,
};
use reticulum_heltec_vision_master_e290_node::{
    announce_time::BootAnnounceClock,
    authenticated_api_node::AuthenticatedApiDispatchFailureKind,
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
    session_admission_handoff::{
        NodeSessionAdmissionHandoff, SessionAdmissionCommand, SessionAdmissionOutcome,
        SessionAdmissionReply,
    },
};
use reticulum_interface_router::{InterfaceDescriptor, SealedIngressPacket};
use reticulum_node_core::{
    AuthorizedFrameObservation, InboundDataProjection, MonotonicInstant, MonotonicMillis,
    MonotonicSeconds, NodeActions, OutboundDispatchInterval, ReceiptCorrelationError,
    TxLeaseDeadline, project_inbound_data,
};
#[cfg(feature = "runtime-measurement-hil")]
use reticulum_node_core::{IngressDisposition, IngressMetadata, PacketType};
use reticulum_rns_inbox_store::{InboxCandidate, InboxDestination};
use reticulum_storage_actor::{DriveError, ProjectorOperationError};
use reticulum_submission_runtime::{FrameOfferProgress, RuntimeError, RuntimeStep};
use reticulum_tx_handoff::AuthorizedFrameNodeHandoff;
use reticulum_tx_supervisor::{
    NodeInterfaceAnnounceFlushResult, NodeInterfaceIngressActionFault,
    NodeInterfaceIngressRecycleFault, NodeInterfaceIngressStep, NodeInterfaceOrdinaryOfferFailure,
    NodeInterfaceSupervisorTransition, NodeInterfaceTickResult, OrdinaryRouterStep,
};

use crate::{
    LORA_ONLINE, ProductSupervisor, RADIO_READY, config,
    platform_storage::{
        ProductCredentialInitializationPort, ProductInboundAdmission, ProductInitializationDrive,
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
    pending_session_admission_command: Option<SessionAdmissionCommand>,
    pending_session_admission_reply: Option<SessionAdmissionReply>,
    pending_exclusive: Option<(ConnectionId, AcquirePairingExclusive)>,
    initialization_retry_not_before_ms: Option<u64>,
    pending_live_command: Option<LivePairingCommand>,
    pending_live_operation: Option<LivePairingOperation>,
    pending_live_reply: Option<LivePairingReply>,
    live_retry_not_before_ms: Option<u64>,
    live_lane_faulted: bool,
}

enum AuthenticatedApiNodeState {
    Ready,
    PendingRequest(LocalApiRequest<AuthenticatedGrant>),
    PendingReply(LocalApiReply),
    Quarantined {
        request: LocalApiRequest<AuthenticatedGrant>,
        fault: AuthenticatedApiDispatchFailureKind,
    },
}

const _: () = assert!(
    mem::size_of::<AuthenticatedApiNodeState>()
        <= config::MAXIMUM_AUTHENTICATED_API_NODE_STATE_BYTES
);

impl AuthenticatedApiNodeState {
    const fn new() -> Self {
        Self::Ready
    }
}

pub(crate) struct NodeHandoffs {
    control: NodePairingHandoff<CriticalSectionRawMutex>,
    live: NodeLivePairingHandoff<CriticalSectionRawMutex>,
    session_admission: NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
    authenticated_api: NodeHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    frame: AuthorizedFrameNodeHandoff<CriticalSectionRawMutex>,
}

type NodeHandoffParts = (
    NodePairingHandoff<CriticalSectionRawMutex>,
    NodeLivePairingHandoff<CriticalSectionRawMutex>,
    NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
    NodeHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    AuthorizedFrameNodeHandoff<CriticalSectionRawMutex>,
);

impl NodeHandoffs {
    pub(crate) const fn new(
        control: NodePairingHandoff<CriticalSectionRawMutex>,
        live: NodeLivePairingHandoff<CriticalSectionRawMutex>,
        session_admission: NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
        authenticated_api: NodeHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
        frame: AuthorizedFrameNodeHandoff<CriticalSectionRawMutex>,
    ) -> Self {
        Self {
            control,
            live,
            session_admission,
            authenticated_api,
            frame,
        }
    }

    fn into_parts(self) -> NodeHandoffParts {
        (
            self.control,
            self.live,
            self.session_admission,
            self.authenticated_api,
            self.frame,
        )
    }
}

impl PairingNodeState {
    const fn new() -> Self {
        Self {
            pending_control_command: None,
            pending_control_reply: None,
            pending_session_admission_command: None,
            pending_session_admission_reply: None,
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
    handoffs: NodeHandoffs,
    offline_descriptor: InterfaceDescriptor,
    announce_epoch: BootEpoch,
    mut rng: Trng,
) {
    let identity_summary = IdentitySummary::new(ApiDestinationHash(
        *supervisor.destination_hash().as_bytes(),
    ));
    let (
        mut pairing_handoff,
        mut live_pairing_handoff,
        mut session_admission_handoff,
        mut authenticated_api,
        mut frame_handoff,
    ) = handoffs.into_parts();
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
    let mut authenticated_api_state = AuthenticatedApiNodeState::new();
    let mut pending_inbound: Option<InboxCandidate> = None;
    #[cfg(feature = "runtime-measurement-hil")]
    let mut previous_node_loop_us = now_micros();

    loop {
        #[cfg(feature = "runtime-measurement-hil")]
        {
            let loop_started_us = now_micros();
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE
                .record_node_loop_gap(loop_started_us.saturating_sub(previous_node_loop_us));
            previous_node_loop_us = loop_started_us;
        }
        let mut progressed = false;

        progressed |= step_pairing_frontier(
            storage,
            &mut pairing_handoff,
            &mut live_pairing_handoff,
            &mut session_admission_handoff,
            &mut pairing,
            &mut rng,
            now_millis(),
        );

        if let Some(candidate) = pending_inbound.take() {
            progressed |= drive_inbound_candidate(storage, &mut pending_inbound, candidate);
        }

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
            #[cfg(feature = "runtime-measurement-hil")]
            let authorized_frame_started_us = now_micros();
            let frame_offer = storage.offer_authorized_frame(observation);
            #[cfg(feature = "runtime-measurement-hil")]
            RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_operation(
                RuntimeOperationKind::AuthorizedFrame,
                now_micros().saturating_sub(authorized_frame_started_us),
            );
            match frame_offer {
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
                    let dispatch_started_at = MonotonicInstant::from_micros(now_micros());
                    let pass = supervisor.step(MonotonicMillis::new(now_millis()));
                    let dispatch_completed_at = MonotonicInstant::from_micros(now_micros());
                    let transition = pass.transition();
                    if let NodeInterfaceSupervisorTransition::Ordinary(
                        OrdinaryRouterStep::Routed {
                            interface,
                            protocol_token: Some(token),
                            first_dispatch,
                            ..
                        },
                    ) = transition
                    {
                        let confirmation = OutboundDispatchInterval::new(
                            dispatch_started_at,
                            dispatch_completed_at,
                        )
                        .map(|interval| {
                            supervisor.confirm_outbound_protocol(token, interface, interval)
                        });
                        let disposition = confirmation.map(|confirmed| {
                            config::protocol_dispatch_confirmation_disposition(
                                first_dispatch,
                                confirmed,
                            )
                        });
                        if disposition.is_none()
                            || matches!(
                                disposition,
                                Some(
                                    config::ProtocolDispatchConfirmationDisposition::TerminalFailClosedDrain
                                )
                            )
                        {
                            enter_protocol_dispatch_fail_stop(
                                supervisor,
                                online,
                                &mut fail_closed_draining,
                                interface,
                                first_dispatch,
                                disposition.is_some(),
                            );
                            progressed = true;
                        }
                    }
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
                                    let link_now = MonotonicInstant::from_micros(now_micros());
                                    match supervisor.tick_at(
                                        MonotonicSeconds::new(link_now.as_secs()),
                                        link_now,
                                        config::ordinary_admission(link_now.as_micros() / 1_000),
                                        &mut rng,
                                    ) {
                                        NodeInterfaceTickResult::Accepted(accepted) => {
                                            let report = accepted.into_report();
                                            #[cfg(feature = "runtime-measurement-hil")]
                                            {
                                                let trace_now_ms = now_millis();
                                                RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                                    .record_receipt_timeouts(
                                                        trace_now_ms,
                                                        report.timed_out_attempts as u64,
                                                        report.timed_out_attempt_tag,
                                                        report.timed_out_attempt_tags_consistent,
                                                    );
                                                if report.correlation_fault.is_some() {
                                                    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                                        .record_correlation_fault(trace_now_ms);
                                                }
                                            }
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
                                            #[cfg(feature = "runtime-measurement-hil")]
                                            {
                                                let trace_now_ms = now_millis();
                                                RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                                    .record_receipt_timeouts(
                                                        trace_now_ms,
                                                        report.timed_out_attempts as u64,
                                                        report.timed_out_attempt_tag,
                                                        report.timed_out_attempt_tags_consistent,
                                                    );
                                                if report.correlation_fault.is_some() {
                                                    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE
                                                        .record_correlation_fault(trace_now_ms);
                                                }
                                            }
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
                4 => {
                    progressed |= step_authenticated_api(
                        storage,
                        identity_summary,
                        &mut authenticated_api,
                        &mut authenticated_api_state,
                    );
                }
                _ => {
                    let owner_now = now_millis();
                    if !storage_step_attempted && durability_service.storage_step_due(owner_now) {
                        storage_step_attempted = true;
                        let deadline = TxLeaseDeadline::new(MonotonicMillis::new(
                            owner_now.saturating_add(config::SUBMISSION_OWNER_LEASE_MS),
                        ));
                        #[cfg(feature = "runtime-measurement-hil")]
                        let submission_started_us = now_micros();
                        let submission_step = storage.drive_submission_step(
                            supervisor,
                            MonotonicSeconds::new(now_seconds()),
                            MonotonicMillis::new(owner_now),
                            deadline,
                            &mut rng,
                        );
                        #[cfg(feature = "runtime-measurement-hil")]
                        RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_operation(
                            RuntimeOperationKind::Submission,
                            now_micros().saturating_sub(submission_started_us),
                        );
                        match submission_step {
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

            if let Some(output) = supervisor.take_non_packet_actions() {
                let event_count = output.events.len();
                let mut inbound_data = 0_usize;
                let mut other_events = 0_usize;
                for event in output.events {
                    match project_inbound_data(event) {
                        InboundDataProjection::Data(data) => {
                            inbound_data = inbound_data.saturating_add(1);
                            let (destination, payload) = data.into_parts();
                            match InboxCandidate::new(InboxDestination::new(destination), &payload)
                            {
                                Ok(candidate) if pending_inbound.is_none() => {
                                    let _ = drive_inbound_candidate(
                                        storage,
                                        &mut pending_inbound,
                                        candidate,
                                    );
                                }
                                Ok(_candidate) => {
                                    storage.record_inbound_drop();
                                    warn!(
                                        "e290-node stage=rns-inbox-admission status=DROPPED reason=pending-candidate-capacity overflow_policy=drop-newest payload_len={} proof_semantics=decrypt-only",
                                        payload.len(),
                                    );
                                }
                                Err(reason) => {
                                    storage.record_inbound_drop();
                                    warn!(
                                        "e290-node stage=rns-inbox-admission status=DROPPED reason={reason:?} overflow_policy=drop-newest proof_semantics=decrypt-only"
                                    );
                                }
                            }
                        }
                        InboundDataProjection::Other(_event) => {
                            other_events = other_events.saturating_add(1);
                        }
                    }
                }
                info!(
                    "e290-node stage=local-output status=HANDLED events={event_count} inbound_data={inbound_data} other_events={other_events} unroutable={} inbound_client_delivery=durable-rns-inbox other_event_delivery=not-yet-wired",
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

fn drive_inbound_candidate(
    storage: &mut ProductStorageCoordinator,
    pending: &mut Option<InboxCandidate>,
    candidate: InboxCandidate,
) -> bool {
    let payload_len = candidate.payload().len();
    #[cfg(feature = "runtime-measurement-hil")]
    let inbound_started_us = now_micros();
    let admission = storage.offer_inbound(candidate);
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_operation(
        RuntimeOperationKind::Inbound,
        now_micros().saturating_sub(inbound_started_us),
    );
    match admission {
        ProductInboundAdmission::Committed(id) => {
            info!(
                "e290-node stage=rns-inbox-admission status=COMMITTED item_id={} payload_len={payload_len} durability=commit-readback plaintext_at_rest=true proof_semantics=decrypt-only",
                id.get(),
            );
            true
        }
        ProductInboundAdmission::Deferred(candidate) => {
            *pending = Some(candidate);
            false
        }
        ProductInboundAdmission::DroppedFull => {
            warn!(
                "e290-node stage=rns-inbox-admission status=DROPPED reason=mailbox-full overflow_policy=drop-newest payload_len={payload_len} proof_semantics=decrypt-only"
            );
            true
        }
        ProductInboundAdmission::DroppedUnavailable => {
            warn!(
                "e290-node stage=rns-inbox-admission status=DROPPED reason=mailbox-unavailable overflow_policy=drop-newest payload_len={payload_len} proof_semantics=decrypt-only"
            );
            true
        }
        ProductInboundAdmission::DroppedFaulted(reason) => {
            error!(
                "e290-node stage=rns-inbox-admission status=DROPPED reason=store-fault fault={reason:?} overflow_policy=drop-newest payload_len={payload_len} inbox_service=disabled radio_actor=continue-route-only proof_semantics=decrypt-only"
            );
            true
        }
    }
}

fn step_authenticated_api(
    storage: &mut ProductStorageCoordinator,
    identity: IdentitySummary,
    handoff: &mut NodeHandoff<CriticalSectionRawMutex, AuthenticatedGrant>,
    state: &mut AuthenticatedApiNodeState,
) -> bool {
    // Each invocation performs at most one ownership transition. This keeps a
    // pressured reply or terminal request resident without letting local API
    // traffic consume the node task's other fair lanes.
    if matches!(state, AuthenticatedApiNodeState::PendingReply(_)) {
        let AuthenticatedApiNodeState::PendingReply(reply) =
            mem::replace(state, AuthenticatedApiNodeState::Ready)
        else {
            unreachable!()
        };
        return match handoff.replies().try_send(reply) {
            Ok(()) => true,
            Err(pressure) => {
                *state = AuthenticatedApiNodeState::PendingReply(pressure.into_inner());
                false
            }
        };
    }

    if matches!(state, AuthenticatedApiNodeState::PendingRequest(_)) {
        let AuthenticatedApiNodeState::PendingRequest(request) =
            mem::replace(state, AuthenticatedApiNodeState::Ready)
        else {
            unreachable!()
        };
        #[cfg(feature = "runtime-measurement-hil")]
        let api_dispatch_started_us = now_micros();
        let dispatch = storage.dispatch_authenticated_request(request, identity);
        #[cfg(feature = "runtime-measurement-hil")]
        RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.record_operation(
            RuntimeOperationKind::ApiDispatch,
            now_micros().saturating_sub(api_dispatch_started_us),
        );
        match dispatch {
            Ok(reply) => *state = AuthenticatedApiNodeState::PendingReply(reply),
            Err(failure) => {
                let kind = failure.kind();
                *state = AuthenticatedApiNodeState::Quarantined {
                    request: failure.into_request(),
                    fault: kind,
                };
                error!(
                    "e290-node stage=authenticated-api status=FAIL-STOP reason={kind:?} action=retain-request-and-close-api-lane"
                );
            }
        }
        return true;
    }

    if let AuthenticatedApiNodeState::Quarantined { request, fault } = state {
        let _ = (request, fault);
        return false;
    }
    if let Some(request) = handoff.requests().try_receive() {
        *state = AuthenticatedApiNodeState::PendingRequest(request);
        return true;
    }
    false
}

fn step_pairing_frontier(
    storage: &mut ProductStorageCoordinator,
    control_handoff: &mut NodePairingHandoff<CriticalSectionRawMutex>,
    live_handoff: &mut NodeLivePairingHandoff<CriticalSectionRawMutex>,
    session_admission_handoff: &mut NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
    state: &mut PairingNodeState,
    rng: &mut Trng,
    now_millis: u64,
) -> bool {
    let mut progressed = flush_pairing_reply(control_handoff, &mut state.pending_control_reply);
    progressed |= flush_live_pairing_reply(live_handoff, &mut state.pending_live_reply);
    progressed |= flush_session_admission_reply(
        session_admission_handoff,
        &mut state.pending_session_admission_reply,
    );

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
    if state.pending_session_admission_command.is_none()
        && state.pending_session_admission_reply.is_none()
    {
        state.pending_session_admission_command = session_admission_handoff.try_receive_command();
    }

    // All command channels have the same sole USB producer. Compare captured
    // event time anyway so a queued earlier wire request or scalar event cannot
    // be overtaken. Equal timestamps choose wire work, most notably before a
    // bus-reset disconnect observed in the same millisecond.
    for _ in 0..3 {
        let next_lane = select_pairing_command_lane(
            state
                .pending_session_admission_command
                .as_ref()
                .map(|admission| admission.at().get()),
            state
                .pending_live_command
                .as_ref()
                .map(|live| live.at().get()),
            state
                .pending_control_command
                .as_ref()
                .map(|control| control.at().get()),
        );
        if next_lane == Some(PairingCommandLane::SessionAdmission) {
            let command = state
                .pending_session_admission_command
                .take()
                .expect("the selected session-admission command is retained");
            let connection = command.connection();
            let outcome = match storage.select_ordinary_session(
                command.at(),
                connection,
                command.credential_id(),
            ) {
                Ok(selected) => SessionAdmissionOutcome::Selected(selected),
                Err(_) => SessionAdmissionOutcome::Refused,
            };
            state.pending_session_admission_reply =
                Some(SessionAdmissionReply::new(connection, outcome));
            progressed = true;
        } else if next_lane == Some(PairingCommandLane::Live) {
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
    progressed |= flush_session_admission_reply(
        session_admission_handoff,
        &mut state.pending_session_admission_reply,
    );

    if state.pending_live_command.is_none()
        && state.pending_control_command.is_none()
        && state.pending_session_admission_command.is_none()
    {
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

fn flush_session_admission_reply(
    handoff: &mut NodeSessionAdmissionHandoff<CriticalSectionRawMutex>,
    pending_reply: &mut Option<SessionAdmissionReply>,
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

fn enter_protocol_dispatch_fail_stop(
    supervisor: &mut ProductSupervisor,
    online: InterfaceDescriptor,
    fail_closed_draining: &mut bool,
    routed_interface: reticulum_node_core::PacketInterfaceId,
    first_dispatch: bool,
    interval_ordered: bool,
) {
    match supervisor.set_interface_online(online.lease(), false) {
        Ok(offline) => error!(
            "e290-node stage=protocol-dispatch-confirmation status=INTERFACE-FAIL-STOP routed_interface={} interface={} generation={} online={} first_dispatch={} interval_ordered={} edge=router-egress-acceptance-not-rf-txdone action=deny-fresh-interface-work-and-drain-retained-owners",
            routed_interface.get(),
            offline.lease().interface().get(),
            offline.lease().generation().get(),
            offline.is_online(),
            first_dispatch,
            interval_ordered,
        ),
        Err(reason) => error!(
            "e290-node stage=protocol-dispatch-confirmation status=FAIL trigger=interface-offline:{reason:?} routed_interface={} first_dispatch={} interval_ordered={} edge=router-egress-acceptance-not-rf-txdone action=fail-closed-drain",
            routed_interface.get(),
            first_dispatch,
            interval_ordered,
        ),
    }
    *fail_closed_draining = true;
}

fn step_ingress(
    supervisor: &mut ProductSupervisor,
    rng: &mut Trng,
    mut state: IngressDrainState<'_>,
) -> bool {
    let link_now = MonotonicInstant::from_micros(now_micros());
    match supervisor.step_ingress_at(
        MonotonicSeconds::new(link_now.as_secs()),
        link_now,
        config::ordinary_admission(link_now.as_micros() / 1_000),
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
        NodeInterfaceIngressStep::ActionsBackpressured(_) => {
            #[cfg(feature = "runtime-measurement-hil")]
            RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.record_action_pressure(now_millis());
            false
        }
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
            #[cfg(feature = "runtime-measurement-hil")]
            RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.record_correlation_fault(now_millis());
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
            #[cfg(feature = "runtime-measurement-hil")]
            let actions_backpressured = processed.actions_backpressured().is_some();
            #[cfg(feature = "runtime-measurement-hil")]
            let trace_now_ms = now_millis();
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
            let report = processed.into_report();
            #[cfg(feature = "runtime-measurement-hil")]
            {
                if actions_backpressured || terminal_action_fault.is_some() {
                    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.record_action_pressure(trace_now_ms);
                }
                RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.record_rns_ingress(
                    trace_now_ms,
                    runtime_ingress_disposition(&report.disposition),
                    runtime_ingress_metadata(report.metadata),
                );
            }
            #[cfg(not(feature = "runtime-measurement-hil"))]
            let _ = report;
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
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.record_action_pressure(now_millis());
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

#[cfg(feature = "runtime-measurement-hil")]
fn runtime_ingress_disposition(
    disposition: &IngressDisposition,
) -> RuntimeProofTraceIngressDisposition {
    match disposition {
        IngressDisposition::Processed => RuntimeProofTraceIngressDisposition::Processed,
        IngressDisposition::NativeDuplicate => RuntimeProofTraceIngressDisposition::NativeDuplicate,
        IngressDisposition::NativeInvalid => RuntimeProofTraceIngressDisposition::NativeInvalid,
        IngressDisposition::NoObservableOutcome => {
            RuntimeProofTraceIngressDisposition::NoObservableOutcome
        }
        IngressDisposition::Rejected(_) => RuntimeProofTraceIngressDisposition::Rejected,
    }
}

#[cfg(feature = "runtime-measurement-hil")]
fn runtime_ingress_metadata(metadata: IngressMetadata) -> RuntimeProofTraceIngressMetadata {
    RuntimeProofTraceIngressMetadata {
        wire_packet_type: match metadata.wire_packet_type() {
            None => RuntimeProofTracePacketType::Unparsed,
            Some(PacketType::Data) => RuntimeProofTracePacketType::Data,
            Some(PacketType::Announce) => RuntimeProofTracePacketType::Announce,
            Some(PacketType::LinkRequest) => RuntimeProofTracePacketType::LinkRequest,
            Some(PacketType::Proof) => RuntimeProofTracePacketType::Proof,
        },
        emitted_packets: u64::from(metadata.emitted_packets()),
        generated_proof_actions: u64::from(metadata.generated_proof_actions()),
        delivered_receipt_terminals: u64::from(metadata.delivered_receipt_terminals()),
        timed_out_receipt_terminals: u64::from(metadata.timed_out_receipt_terminals()),
        generated_proof_tag: metadata.generated_proof_tag(),
        delivered_receipt_tag: metadata.delivered_receipt_tag(),
        generated_proof_tags_consistent: metadata.generated_proof_tags_consistent(),
        delivered_receipt_tags_consistent: metadata.delivered_receipt_tags_consistent(),
        counts_saturated: metadata.counts_saturated(),
    }
}

fn now_millis() -> u64 {
    Instant::now().as_millis()
}

fn now_micros() -> u64 {
    Instant::now().as_micros()
}

fn now_seconds() -> u64 {
    Instant::now().as_secs()
}

async fn fail_stop() -> ! {
    loop {
        Timer::after(Duration::from_secs(30)).await;
    }
}
