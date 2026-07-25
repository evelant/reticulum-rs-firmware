//! Permanent transport-neutral node owner task.

#![allow(clippy::too_many_arguments)]

use core::mem;

use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::rng::Trng;
use log::{error, info, warn};
use reticulum_announce_clock::BootEpoch;
use reticulum_device_api::{
    DestinationHash as ApiDestinationHash, IdentitySummary, LxmfPeerDiscoveryIncarnation,
};
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
    OperationKind as RuntimeOperationKind, RETICULUM_RUNTIME_LXMF_TRACE_EVIDENCE,
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE, RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE,
    RuntimeLxmfCommitKind, RuntimeProofTraceIngressDisposition, RuntimeProofTraceIngressMetadata,
    RuntimeProofTracePacketType,
};
use reticulum_heltec_vision_master_e290_node::{
    announce_time::{BootAnnounceClock, BootAnnounceSchedule, ScheduledAnnounce},
    authenticated_api_node::AuthenticatedApiDispatchFailureKind,
    causal_pairing_frontier::{PairingCommandLane, select_pairing_command_lane},
    credential_pairing::{CredentialPairingStatus, PairingMutation},
    credential_runtime::CredentialInitializationStatus,
    durability_policy::{AuthorizedFrameDurability, DurabilityServiceState},
    live_pairing_handoff::{LivePairingCommand, LivePairingReply, NodeLivePairingHandoff},
    live_pairing_node::{LivePairingOperation, LivePairingOperationStep},
    lxmf_delivery::{
        LXMF_RETRY_QUARANTINE, LxmfAuthorityFault, LxmfProofActionsHolder, LxmfProofSinkFailure,
        LxmfRetentionAction, LxmfRetryClass, LxmfRetryEntry, LxmfRetrySet, LxmfTerminalReject,
        retention_action, retry_after_pre_io_deferral, should_relieve_lxmf_retry, stamp_policy,
        wire_limits,
    },
    nomad_coordinator::{CoordinatorCommand, CoordinatorOperation, NativeRequestPhase},
    nomad_runtime::{NomadDriveStep, NomadEventObservation, ProductNomadRuntimeState},
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
#[cfg(feature = "runtime-measurement-hil")]
use reticulum_lxmf_durable_ingress::DurableIngressCommitKind;
use reticulum_lxmf_durable_ingress::{DurableIngressOutcome, DurableIngressSuccess};
use reticulum_lxmf_ingress::LocalDeliveryDestination;
use reticulum_node_core::{
    APPLICATION_LINK_CONTEXT_NONE, ApplicationEvent, ApplicationEventDiscardReason,
    ApplicationEventLease, ApplicationEventOfferError, ApplicationEventOwner,
    ApplicationEventQuarantineReason, ApplicationLinkRole, AuthorizedFrameObservation,
    DelayedProofOwner, DestinationHash, InitiateLinkError, LinkHandle, MonotonicInstant,
    MonotonicMillis, MonotonicSeconds, NodeActions, OutboundDispatchInterval, PrepareRequestError,
    ReceiptCorrelationError, RequestDispatchConfirmation, RequestDispatchError,
    RequestDispatchReconciliation, RequestHandle, TxLeaseDeadline,
};
#[cfg(feature = "runtime-measurement-hil")]
use reticulum_node_core::{IngressDisposition, IngressMetadata, PacketType};
use reticulum_nomad_protocol::{
    DestinationHash as NomadDestinationHash, LinkFailure, LinkFailureStage, LinkId,
    MonotonicMillis as NomadMillis, RequestFailure, RequestFailureStage, RequestId,
};
use reticulum_peer_discovery::{
    AuthenticatedAnnounceObservation, DestinationHash as PeerDestinationHash, DiscoveredPeers,
    IdentityHash as PeerIdentityHash, IdentityPublicKey, MonotonicMillis as PeerObservedMillis,
    ObservationMetadata, ObservedInterfaceId, SignalObservations,
};
use reticulum_rns_inbox_store::{InboxCandidate, InboxDestination};
use reticulum_storage_actor::{DriveError, ProjectorOperationError};
use reticulum_submission_runtime::{
    FrameOfferProgress, LinkEstablishmentOffer, PathDiscoveryOffer, RuntimeError, RuntimeStep,
};
use reticulum_tx_handoff::AuthorizedFrameNodeHandoff;
use reticulum_tx_supervisor::{
    NodeInterfaceAnnounceFlushResult, NodeInterfaceApplicationEventDrain,
    NodeInterfaceIngressActionFault, NodeInterfaceIngressRecycleFault, NodeInterfaceIngressStep,
    NodeInterfaceOrdinaryOfferFailure, NodeInterfaceSupervisorTransition, NodeInterfaceTickResult,
    OrdinaryRouterCompletionProgress, OrdinaryRouterFaultResidueKind, OrdinaryRouterStep,
};

use crate::{
    LORA_INTERFACE, ProductSupervisor, config,
    platform_storage::{
        ProductCredentialInitializationPort, ProductInboundAdmission, ProductInitializationDrive,
        ProductInitializationRequest, ProductLivePairingAdmission, ProductLxmfAdmission,
        ProductStorageCoordinator, ProductSubmissionDrive,
    },
};

struct RetainedActions {
    actions: NodeActions,
    admission: reticulum_tx_supervisor::OrdinaryRouterAdmission,
    protocol_dispatch: Option<OrdinaryProtocolDispatch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubmissionProtocolDispatch {
    Path(PathDiscoveryOffer),
    Link {
        offer: LinkEstablishmentOffer,
        link: LinkHandle,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NomadProtocolDispatch {
    Path {
        destination: NomadDestinationHash,
    },
    Link {
        destination: NomadDestinationHash,
        link: LinkHandle,
    },
    Request {
        handle: RequestHandle,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinaryProtocolDispatch {
    Submission(SubmissionProtocolDispatch),
    Nomad(NomadProtocolDispatch),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NomadDispatchResolution {
    Committed,
    Reconciled,
    InvariantReconciled,
    CleanupTransferred,
}

impl NomadDispatchResolution {
    const fn requires_fail_closed_drain(self) -> bool {
        matches!(self, Self::InvariantReconciled | Self::CleanupTransferred)
    }
}

impl RetainedActions {
    fn ordinary(
        actions: NodeActions,
        admission: reticulum_tx_supervisor::OrdinaryRouterAdmission,
    ) -> Self {
        Self {
            actions,
            admission,
            protocol_dispatch: None,
        }
    }

    fn with_protocol_dispatch(mut self, protocol: OrdinaryProtocolDispatch) -> Self {
        debug_assert!(self.protocol_dispatch.is_none());
        self.protocol_dispatch = Some(protocol);
        self
    }

    fn protocol_dispatch(&self) -> Option<OrdinaryProtocolDispatch> {
        self.protocol_dispatch
    }

    fn take_protocol_dispatch(&mut self) -> Option<OrdinaryProtocolDispatch> {
        self.protocol_dispatch.take()
    }

    fn has_application_events(&self) -> bool {
        !self.actions.events.is_empty()
    }

    fn into_parts(
        self,
    ) -> (
        NodeActions,
        reticulum_tx_supervisor::OrdinaryRouterAdmission,
        Option<OrdinaryProtocolDispatch>,
    ) {
        (self.actions, self.admission, self.protocol_dispatch)
    }
}
type QuarantinedIngressBuffer = (NodeInterfaceIngressRecycleFault, SealedIngressPacket);

/// Boot-lifetime LXMF scheduler owners placed together in external PSRAM.
///
/// Application-event slots remain in their existing internal static storage;
/// every token here is only structural authority over one of those slots.
#[must_use = "the LXMF volatile owner graph must remain alive for the node task"]
pub(crate) struct ApplicationVolatileState {
    retries: LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    proof_holder: LxmfProofActionsHolder,
    authority_faults: LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    discovered_peers: DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    pending_owner_fault_observed: bool,
    service_fault_observed: bool,
    nomad: ProductNomadRuntimeState,
}

impl ApplicationVolatileState {
    /// Construct the empty boot-time owner graph before any event is admitted.
    pub(crate) const fn new() -> Self {
        Self {
            retries: LxmfRetrySet::new(),
            proof_holder: LxmfProofActionsHolder::new(),
            authority_faults: LxmfAuthorityFault::new(),
            discovered_peers: match DiscoveredPeers::try_new() {
                Ok(peers) => peers,
                Err(_) => panic!("the product peer-discovery capacity must be nonzero"),
            },
            pending_owner_fault_observed: false,
            service_fault_observed: false,
            nomad: ProductNomadRuntimeState::new(),
        }
    }
}

enum ActionOfferHandling {
    Retry(RetainedActions),
    RetainAndDrain(RetainedActions),
}

#[derive(Clone, Copy)]
enum LxmfProofOfferHandling {
    Retry,
    RetainAndDrain,
}

enum ActionRetryStep {
    Accepted(Option<OrdinaryProtocolDispatch>),
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

#[allow(clippy::too_many_arguments)]
#[embassy_executor::task]
pub async fn run(
    supervisor: &'static mut ProductSupervisor,
    storage: &'static mut ProductStorageCoordinator,
    mut application_events: ApplicationEventOwner<'static>,
    mut delayed_proofs: DelayedProofOwner<'static>,
    application_volatile: &'static mut ApplicationVolatileState,
    lxmf_destination: Option<DestinationHash>,
    peer_discovery_incarnation: LxmfPeerDiscoveryIncarnation,
    handoffs: NodeHandoffs,
    offline_descriptor: InterfaceDescriptor,
    announce_epoch: BootEpoch,
    mut rng: Trng,
) {
    let primary_destination = *supervisor.destination_hash().as_bytes();
    let identity_summary = match lxmf_destination {
        Some(lxmf_destination) => IdentitySummary::with_lxmf_delivery_destination(
            ApiDestinationHash(primary_destination),
            ApiDestinationHash(*lxmf_destination.as_bytes()),
        ),
        None => IdentitySummary::new(ApiDestinationHash(primary_destination)),
    };
    let (
        mut pairing_handoff,
        mut live_pairing_handoff,
        mut session_admission_handoff,
        mut authenticated_api,
        mut frame_handoff,
    ) = handoffs.into_parts();
    // The concrete actor now owns Ready/Offline reporting through its fixed
    // interface-fabric lifecycle capability. This descriptor remains only the
    // generation-bound product-policy authority for LoRa-specific durability
    // fail-stop paths.
    let lora_descriptor = offline_descriptor;

    // These slots are interchangeable. Either can hold a locally
    // backpressured envelope or an exact coordinator-rejected envelope.
    let mut retry_actions_a: Option<RetainedActions> = None;
    let mut retry_actions_b: Option<RetainedActions> = None;
    let mut pending_protocol_dispatch: Option<OrdinaryProtocolDispatch> = None;
    let mut quarantined_actions: Option<RetainedActions> = None;
    let mut quarantined_ingress_buffer: Option<QuarantinedIngressBuffer> = None;
    let mut prefer_retry_b = true;
    let mut terminal_correlation_fault: Option<ReceiptCorrelationError> = None;
    let mut correlation_recycle_pending = false;
    let mut fail_closed_draining = false;
    let mut observed_terminal_transitions = 0_u8;
    let mut announce_clock = BootAnnounceClock::new(announce_epoch);
    let mut next_tick_seconds = now_seconds();
    let mut announce_schedule = BootAnnounceSchedule::new(
        now_seconds(),
        primary_destination,
        lxmf_destination.is_some(),
    );
    let mut lane = 0_u8;
    let mut fresh_nomad_turn_armed = false;
    let mut observed_recycle_fault: Option<NodeInterfaceIngressRecycleFault> = None;
    let mut durability_service =
        DurabilityServiceState::from_runtime_available(storage.submission_service_available());
    let mut retained_frame: Option<AuthorizedFrameObservation> = None;
    let mut pending_frame_acknowledgement: Option<AuthorizedFrameObservation> = None;
    let mut pairing = PairingNodeState::new();
    let mut authenticated_api_state = AuthenticatedApiNodeState::new();
    let mut pending_inbound: Option<InboxCandidate> = None;
    let mut application_event_drain_fault: Option<ApplicationEventOfferError> = None;
    let ApplicationVolatileState {
        retries: lxmf_retries,
        proof_holder: lxmf_proof_holder,
        authority_faults: lxmf_authority_fault,
        discovered_peers,
        pending_owner_fault_observed: lxmf_pending_owner_fault_observed,
        service_fault_observed: lxmf_service_fault_observed,
        nomad,
    } = application_volatile;
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
                    lora_descriptor,
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
                            lora_descriptor,
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
                            lora_descriptor,
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
                let protocol_dispatch = pending_protocol_dispatch.take();
                let retained = match config::rejected_action_disposition(reason) {
                    config::RejectedActionDisposition::RefreshDeadline => {
                        warn!(
                            "e290-node stage=ordinary-admission status=RETRY-WITH-FRESH-DEADLINE reason={reason:?}"
                        );
                        RetainedActions::ordinary(actions, config::ordinary_admission(now_millis()))
                    }
                    config::RejectedActionDisposition::FailStop => {
                        error!(
                            "e290-node stage=ordinary-admission status=TERMINAL reason={reason:?} packets={} events={} unroutable={} action=retain-rejected-owner-and-fail-closed-drain",
                            actions.packets.len(),
                            actions.events.len(),
                            actions.unroutable_packets,
                        );
                        fail_closed_draining = true;
                        RetainedActions::ordinary(actions, elapsed_admission)
                    }
                };
                let retained = match protocol_dispatch {
                    Some(protocol) => retained.with_protocol_dispatch(protocol),
                    None => retained,
                };
                if retry_actions_a.is_none() {
                    retry_actions_a = Some(retained);
                } else {
                    debug_assert!(retry_actions_b.is_none());
                    retry_actions_b = Some(retained);
                }
            }

            let exact_nomad_packet_owned = [
                retry_actions_a
                    .as_ref()
                    .and_then(RetainedActions::protocol_dispatch),
                retry_actions_b
                    .as_ref()
                    .and_then(RetainedActions::protocol_dispatch),
                pending_protocol_dispatch,
            ]
            .into_iter()
            .flatten()
            .any(|protocol| matches!(protocol, OrdinaryProtocolDispatch::Nomad(_)));
            if !nomad.fresh_command_pending() || exact_nomad_packet_owned || fail_closed_draining {
                fresh_nomad_turn_armed = false;
            }
            let fresh_nomad_turn_reserved = config::reserve_fresh_nomad_turn(
                fresh_nomad_turn_armed,
                exact_nomad_packet_owned,
                fail_closed_draining,
            );
            // A projected event already in the product-owned FIFO stays
            // authoritative even after upstream intake has failed closed. It
            // may be the exact response/failure/close that resolves the active
            // Nomad request, so timeout cleanup must not overtake it. Upstream
            // owners count only while their remaining drain path is healthy.
            let native_terminal_events_pending = config::application_events_preempt_nomad_timeout(
                application_events.capacities().ready != 0,
                !fail_closed_draining && application_event_drain_fault.is_none(),
                supervisor.ordinary_application_events_pending()
                    || supervisor.ingress_actions_pending()
                    || retry_actions_a
                        .as_ref()
                        .is_some_and(RetainedActions::has_application_events)
                    || retry_actions_b
                        .as_ref()
                        .is_some_and(RetainedActions::has_application_events),
            );

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
                    if (!fail_closed_draining
                        && retry_capacity_available
                        && !fresh_nomad_turn_reserved
                        && pending_protocol_dispatch.is_none())
                        || settling_ingress
                    {
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
                    if let NodeInterfaceSupervisorTransition::Lifecycle(lifecycle) = transition {
                        let acknowledgement = lifecycle.acknowledgement();
                        let request = acknowledgement.request();
                        match acknowledgement.result() {
                            Ok(descriptor) => info!(
                                "e290-node stage=interface-lifecycle status=PASS queue={} interface={} generation={} state={:?} online={} config=0x{:08x} mtu={} queue_depth={}",
                                lifecycle.queue().get(),
                                descriptor.lease().interface().get(),
                                descriptor.lease().generation().get(),
                                request.state(),
                                descriptor.is_online(),
                                descriptor.config().get(),
                                descriptor.logical_mtu().get(),
                                config::INTERFACE_QUEUE_DEPTH,
                            ),
                            Err(reason) => error!(
                                "e290-node stage=interface-lifecycle status=REJECTED queue={} interface={} generation={} state={:?} reason={reason:?} action=retain-authoritative-registry-and-acknowledge-actor-fail-closed",
                                lifecycle.queue().get(),
                                request.lease().interface().get(),
                                request.lease().generation().get(),
                                request.state(),
                            ),
                        }
                    }
                    if let NodeInterfaceSupervisorTransition::Ordinary(
                        OrdinaryRouterStep::Routed {
                            interface,
                            protocol_token,
                            first_dispatch,
                            ..
                        },
                    ) = transition
                    {
                        // Native protocol state may advance only after the
                        // authoritative router accepts the exact packet. A
                        // LINKREQUEST additionally starts the product-owned
                        // establishment clock only after that confirmation.
                        let confirmation = protocol_token.and_then(|token| {
                            OutboundDispatchInterval::new(
                                dispatch_started_at,
                                dispatch_completed_at,
                            )
                            .map(|interval| {
                                supervisor.confirm_outbound_protocol(token, interface, interval)
                            })
                        });
                        let confirmation_disposition = protocol_token.map(|_| {
                            confirmation
                                .map(|confirmed| {
                                    config::protocol_dispatch_confirmation_disposition(
                                        first_dispatch,
                                        confirmed,
                                    )
                                })
                                .unwrap_or(
                                    config::ProtocolDispatchConfirmationDisposition::TerminalFailClosedDrain,
                                )
                        });
                        if matches!(
                            confirmation_disposition,
                            Some(
                                config::ProtocolDispatchConfirmationDisposition::TerminalFailClosedDrain
                            )
                        ) {
                            enter_protocol_dispatch_fail_stop(
                                supervisor,
                                &mut fail_closed_draining,
                                interface,
                                first_dispatch,
                                confirmation.is_some(),
                            );
                            progressed = true;
                        }

                        if let Some(pending) = pending_protocol_dispatch {
                            let dispatched_at =
                                MonotonicMillis::new(dispatch_completed_at.as_micros() / 1_000);
                            match pending {
                                OrdinaryProtocolDispatch::Submission(
                                    SubmissionProtocolDispatch::Path(offer),
                                ) => {
                                    if !first_dispatch || protocol_token.is_some() {
                                        error!(
                                            "e290-node stage=path-discovery status=FAIL reason=dispatch-correlation first_dispatch={first_dispatch} protocol_token_present={} interface={} action=disable-submissions",
                                            protocol_token.is_some(),
                                            interface.get(),
                                        );
                                        disable_submission_for_path_fault(
                                            storage,
                                            &mut durability_service,
                                            &retained_frame,
                                            &pending_frame_acknowledgement,
                                            supervisor,
                                            lora_descriptor,
                                            &mut fail_closed_draining,
                                            "path-request-dispatch-correlation",
                                        );
                                    } else {
                                        pending_protocol_dispatch = None;
                                        match storage.acknowledge_path_request_dispatched(
                                            offer,
                                            dispatched_at,
                                        ) {
                                            Ok(()) => info!(
                                                "e290-node stage=path-discovery status=DISPATCHED submission={} ordinal={} interface={} dispatch_ms={} response_clock=started",
                                                offer.id().get(),
                                                offer.ordinal(),
                                                interface.get(),
                                                dispatched_at.get(),
                                            ),
                                            Err(reason) => {
                                                error!(
                                                    "e290-node stage=path-discovery status=FAIL reason=dispatch-ack:{reason:?} submission={} ordinal={} action=disable-submissions",
                                                    offer.id().get(),
                                                    offer.ordinal(),
                                                );
                                                disable_submission_for_path_fault(
                                                    storage,
                                                    &mut durability_service,
                                                    &retained_frame,
                                                    &pending_frame_acknowledgement,
                                                    supervisor,
                                                    lora_descriptor,
                                                    &mut fail_closed_draining,
                                                    "path-request-dispatch-ack",
                                                );
                                            }
                                        }
                                        progressed = true;
                                    }
                                }
                                OrdinaryProtocolDispatch::Submission(
                                    SubmissionProtocolDispatch::Link { offer, link },
                                ) => {
                                    if !first_dispatch
                                        || protocol_token.is_none()
                                        || confirmation != Some(true)
                                    {
                                        let aborted = supervisor.abort_unestablished_link(link);
                                        error!(
                                            "e290-node stage=link-establishment status=FAIL reason=dispatch-correlation submission={} generation={} first_dispatch={first_dispatch} protocol_token_present={} protocol_confirmed={} interface={} pending_link_aborted={aborted} action=disable-submissions",
                                            offer.id().get(),
                                            offer.generation(),
                                            protocol_token.is_some(),
                                            confirmation == Some(true),
                                            interface.get(),
                                        );
                                        disable_submission_for_path_fault(
                                            storage,
                                            &mut durability_service,
                                            &retained_frame,
                                            &pending_frame_acknowledgement,
                                            supervisor,
                                            lora_descriptor,
                                            &mut fail_closed_draining,
                                            "link-request-dispatch-correlation",
                                        );
                                    } else {
                                        pending_protocol_dispatch = None;
                                        match storage.acknowledge_link_request_dispatched(
                                            offer,
                                            link,
                                            dispatched_at,
                                        ) {
                                            Ok(()) => info!(
                                                "e290-node stage=link-establishment status=DISPATCHED submission={} generation={} link={:02x?} interface={} dispatch_ms={} establishment_clock=started",
                                                offer.id().get(),
                                                offer.generation(),
                                                link.as_bytes(),
                                                interface.get(),
                                                dispatched_at.get(),
                                            ),
                                            Err(reason) => {
                                                let aborted =
                                                    supervisor.abort_unestablished_link(link);
                                                error!(
                                                    "e290-node stage=link-establishment status=FAIL reason=dispatch-ack:{reason:?} submission={} generation={} link={:02x?} pending_link_aborted={aborted} action=disable-submissions",
                                                    offer.id().get(),
                                                    offer.generation(),
                                                    link.as_bytes(),
                                                );
                                                disable_submission_for_path_fault(
                                                    storage,
                                                    &mut durability_service,
                                                    &retained_frame,
                                                    &pending_frame_acknowledgement,
                                                    supervisor,
                                                    lora_descriptor,
                                                    &mut fail_closed_draining,
                                                    "link-request-dispatch-ack",
                                                );
                                            }
                                        }
                                        progressed = true;
                                    }
                                }
                                OrdinaryProtocolDispatch::Nomad(protocol) => {
                                    let resolution = confirm_nomad_dispatch(
                                        supervisor,
                                        nomad,
                                        protocol,
                                        first_dispatch,
                                        protocol_token.is_some(),
                                        confirmation == Some(true),
                                        native_terminal_events_pending,
                                        dispatched_at,
                                    );
                                    pending_protocol_dispatch = None;
                                    if resolution.requires_fail_closed_drain() {
                                        fail_closed_draining = true;
                                    }
                                    progressed = true;
                                }
                            }
                        }
                    }
                    if let NodeInterfaceSupervisorTransition::Ordinary(
                        OrdinaryRouterStep::RouteRejected { slot, reason },
                    ) = transition
                        && pending_protocol_dispatch.is_some()
                    {
                        // Rejection is scoped to one selected interface. The
                        // ordinary router may still advance the same packet to
                        // another eligible transport, so retain exact protocol
                        // correlation until a successful first dispatch or a
                        // definitive Returned completion.
                        warn!(
                            "e290-node stage=submission-protocol-dispatch status=HOP-REJECTED reason={reason:?} slot={} first_dispatch=false action=await-next-route-or-terminal-return",
                            slot.get(),
                        );
                        progressed = true;
                    }
                    if let NodeInterfaceSupervisorTransition::Ordinary(
                        OrdinaryRouterStep::Completion(
                            OrdinaryRouterCompletionProgress::Returned { slot },
                        ),
                    ) = transition
                        && let Some(protocol) = pending_protocol_dispatch.take()
                    {
                        let submission =
                            matches!(protocol, OrdinaryProtocolDispatch::Submission(_));
                        let cleanup_invariant =
                            handle_terminal_protocol_dispatch(supervisor, nomad, protocol, false);
                        if cleanup_invariant {
                            fail_closed_draining = true;
                        }
                        if submission {
                            error!(
                                "e290-node stage=submission-protocol-dispatch status=DISABLED reason=all-routes-returned slot={} first_dispatch=false local_submission_admission=closed",
                                slot.get(),
                            );
                            disable_submission_for_path_fault(
                                storage,
                                &mut durability_service,
                                &retained_frame,
                                &pending_frame_acknowledgement,
                                supervisor,
                                lora_descriptor,
                                &mut fail_closed_draining,
                                "submission-protocol-routes-exhausted",
                            );
                        }
                        progressed = true;
                    }
                    if let NodeInterfaceSupervisorTransition::Ordinary(
                        OrdinaryRouterStep::PendingJobExpired { slot }
                        | OrdinaryRouterStep::PendingJobCancelledForFault { slot },
                    ) = transition
                        && let Some(protocol) = pending_protocol_dispatch.take()
                    {
                        let submission =
                            matches!(protocol, OrdinaryProtocolDispatch::Submission(_));
                        let cleanup_invariant =
                            handle_terminal_protocol_dispatch(supervisor, nomad, protocol, true);
                        if cleanup_invariant {
                            fail_closed_draining = true;
                        }
                        if submission {
                            error!(
                                "e290-node stage=submission-protocol-dispatch status=DISABLED reason=pre-dispatch-cancelled-or-expired slot={} first_dispatch=false local_submission_admission=closed",
                                slot.get(),
                            );
                            disable_submission_for_path_fault(
                                storage,
                                &mut durability_service,
                                &retained_frame,
                                &pending_frame_acknowledgement,
                                supervisor,
                                lora_descriptor,
                                &mut fail_closed_draining,
                                "submission-protocol-pre-dispatch-terminal",
                            );
                        }
                        progressed = true;
                    }
                    let input_fault_terminated_protocol = matches!(
                        transition,
                        NodeInterfaceSupervisorTransition::Ordinary(
                            OrdinaryRouterStep::PendingActionsRetainedForFault
                        )
                    ) || matches!(
                        transition,
                        NodeInterfaceSupervisorTransition::Ordinary(OrdinaryRouterStep::Disabled(
                            _
                        ))
                    ) && matches!(
                        supervisor.ordinary_fault_residue_kind(),
                        Some(
                            OrdinaryRouterFaultResidueKind::Actions
                                | OrdinaryRouterFaultResidueKind::AdmissionFailure
                        )
                    );
                    if input_fault_terminated_protocol
                        && let Some(protocol) = pending_protocol_dispatch.take()
                    {
                        let submission =
                            matches!(protocol, OrdinaryProtocolDispatch::Submission(_));
                        let cleanup_invariant =
                            handle_terminal_protocol_dispatch(supervisor, nomad, protocol, true);
                        if cleanup_invariant {
                            fail_closed_draining = true;
                        }
                        if submission {
                            disable_submission_for_path_fault(
                                storage,
                                &mut durability_service,
                                &retained_frame,
                                &pending_frame_acknowledgement,
                                supervisor,
                                lora_descriptor,
                                &mut fail_closed_draining,
                                "submission-protocol-input-fault-residue",
                            );
                        }
                        error!(
                            "e290-node stage=protocol-dispatch status=TERMINAL reason=ordinary-input-fault-residue action=retain-action-evidence-and-clean-exact-protocol-owner"
                        );
                        fail_closed_draining = true;
                        progressed = true;
                    }
                    match config::supervisor_transition_disposition(transition) {
                        config::SupervisorTransitionDisposition::Idle => {}
                        config::SupervisorTransitionDisposition::Progress => progressed = true,
                        config::SupervisorTransitionDisposition::TerminalFailClosedDrain => {
                            // Aggregate failure may be unrelated to the
                            // ordinary job already owned by the router. Keep
                            // its protocol correlation until that job reports
                            // Routed, Returned, Expired, or Cancelled.
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
                                    ActionRetryStep::Accepted(protocol_dispatch) => {
                                        if let Some(protocol) = protocol_dispatch {
                                            debug_assert!(pending_protocol_dispatch.is_none());
                                            pending_protocol_dispatch = Some(protocol);
                                        }
                                        progressed = true;
                                    }
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
                                    ActionRetryStep::Accepted(protocol_dispatch) => {
                                        if let Some(protocol) = protocol_dispatch {
                                            debug_assert!(pending_protocol_dispatch.is_none());
                                            pending_protocol_dispatch = Some(protocol);
                                        }
                                        progressed = true;
                                    }
                                    ActionRetryStep::Busy => {}
                                    ActionRetryStep::Terminal => {
                                        fail_closed_draining = true;
                                        progressed = true;
                                    }
                                }
                            }
                            config::ActionRetrySlot::None => {
                                let now = now_seconds();
                                if pending_protocol_dispatch.is_none()
                                    && !supervisor.ingress_actions_pending()
                                    && now >= next_tick_seconds
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
                        && pending_protocol_dispatch.is_none()
                        && !supervisor.ingress_actions_pending()
                        && !fail_closed_draining
                        && !fresh_nomad_turn_reserved
                        && !announce_clock.is_exhausted()
                        && let Some(scheduled) = announce_schedule.due(now)
                    {
                        let mut admission_deferred = false;
                        let mut primary_emission = None;
                        if scheduled == ScheduledAnnounce::Primary {
                            let emitted_at = announce_clock
                                .next_emission()
                                .expect("a non-exhausted announce clock has an emission");
                            match supervisor.queue_announce(None, emitted_at, &mut rng) {
                                Ok(()) => {
                                    announce_clock.mark_queued();
                                    primary_emission = Some(emitted_at.get());
                                }
                                Err(reason) => {
                                    admission_deferred = true;
                                    warn!(
                                        "e290-node stage=announce destination=primary status=DEFERRED reason={reason:?} retry_after_seconds={}",
                                        config::ANNOUNCE_ADMISSION_RETRY_SECONDS,
                                    );
                                }
                            }
                        }

                        let mut lxmf_emission = None;
                        if scheduled == ScheduledAnnounce::LxmfDelivery
                            && !*lxmf_service_fault_observed
                            && storage.lxmf_service_available()
                            && let Some(destination) = lxmf_destination
                        {
                            let emitted_at = announce_clock
                                .next_emission()
                                .expect("a non-exhausted announce clock has an emission");
                            match supervisor.queue_announce_for(
                                &destination,
                                Some(&config::LXMF_DELIVERY_ANNOUNCE_APP_DATA),
                                emitted_at,
                                &mut rng,
                            ) {
                                Ok(()) => {
                                    announce_clock.mark_queued();
                                    lxmf_emission = Some(emitted_at.get());
                                }
                                Err(reason) => {
                                    admission_deferred = true;
                                    warn!(
                                        "e290-node stage=announce destination=lxmf.delivery status=DEFERRED reason={reason:?} retry_after_seconds={}",
                                        config::ANNOUNCE_ADMISSION_RETRY_SECONDS,
                                    );
                                }
                            }
                        }

                        if admission_deferred {
                            announce_schedule.defer_attempt(now);
                        } else {
                            // A queued announce completed this event. An LXMF
                            // event whose service was cleanly disabled is also
                            // consumed without spending an announce ordinal.
                            announce_schedule.mark_attempted(now);
                        }

                        if primary_emission.is_some() || lxmf_emission.is_some() {
                            match supervisor.flush_announces(
                                MonotonicSeconds::new(now),
                                config::ordinary_admission(now_millis()),
                                &mut rng,
                            ) {
                                NodeInterfaceAnnounceFlushResult::Accepted => {
                                    info!(
                                        "e290-node stage=announce status=QUEUED primary_emission={primary_emission:?} lxmf_delivery_emission={lxmf_emission:?} boot_epoch={}",
                                        announce_clock.epoch().get(),
                                    );
                                    progressed = true;
                                }
                                NodeInterfaceAnnounceFlushResult::ActionOfferRejected(failure) => {
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
                        }
                        if announce_clock.is_exhausted() {
                            error!(
                                "e290-node stage=announce status=LOCAL-EMISSION-EXHAUSTED boot_epoch={} action=suppress-future-local-announces",
                                announce_clock.epoch().get(),
                            );
                        }
                    }
                }
                4 => {
                    progressed |= step_authenticated_api(
                        storage,
                        supervisor,
                        discovered_peers,
                        peer_discovery_incarnation,
                        now_millis(),
                        identity_summary,
                        &mut authenticated_api,
                        &mut authenticated_api_state,
                    );
                }
                5 => {
                    let fresh_command_pending = nomad.fresh_command_pending();
                    let ordinary_owners_quiescent = retry_actions_a.is_none()
                        && retry_actions_b.is_none()
                        && quarantined_actions.is_none()
                        && pending_protocol_dispatch.is_none()
                        && !supervisor.ingress_actions_pending()
                        && ordinary_router_is_idle(supervisor)
                        && !fail_closed_draining;
                    let nomad_progressed = drive_one_nomad_command(
                        supervisor,
                        nomad,
                        &mut retry_actions_a,
                        &mut retry_actions_b,
                        &mut pending_protocol_dispatch,
                        &mut fail_closed_draining,
                        native_terminal_events_pending,
                        ordinary_owners_quiescent,
                        &mut rng,
                        now_millis(),
                    );
                    progressed |= nomad_progressed;
                    fresh_nomad_turn_armed = config::next_fresh_nomad_turn_armed(
                        fresh_command_pending,
                        exact_nomad_packet_owned,
                        fail_closed_draining,
                        ordinary_owners_quiescent,
                    );
                }
                _ => {
                    let owner_now = now_millis();
                    let ordinary_owners_quiescent = retry_actions_a.is_none()
                        && retry_actions_b.is_none()
                        && quarantined_actions.is_none()
                        && pending_protocol_dispatch.is_none()
                        && !supervisor.ingress_actions_pending()
                        && ordinary_router_is_idle(supervisor);
                    let ordinary_control_step_pending =
                        storage.direct_link_retirement_is_next_step();
                    if config::submission_storage_step_admitted(
                        storage_step_attempted,
                        durability_service.storage_step_due(owner_now),
                        retained_frame.is_some(),
                        ordinary_owners_quiescent
                            && (!fresh_nomad_turn_reserved || ordinary_control_step_pending),
                        fail_closed_draining,
                        ordinary_control_step_pending,
                    ) {
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
                            ProductSubmissionDrive::Runtime(Ok(
                                RuntimeStep::DirectLinkDeliveryTimedOut { terminal, link },
                            )) => {
                                durability_service = durability_service.runtime_progress();
                                let actions = supervisor.close_link(link, &mut rng);
                                if actions.is_empty() {
                                    warn!(
                                        "e290-node stage=direct-link-retirement status=ALREADY-ABSENT link={:02x?} terminal={terminal:?}",
                                        link.as_bytes(),
                                    );
                                } else {
                                    let admission = config::ordinary_admission(owner_now);
                                    match supervisor.try_offer_actions(actions, admission) {
                                        Ok(()) => {
                                            warn!(
                                                "e290-node stage=direct-link-retirement status=ADMITTED reason=delivery-timeout link={:02x?} terminal={terminal:?}",
                                                link.as_bytes(),
                                            );
                                        }
                                        Err(failure) => {
                                            match handle_action_offer_failure(
                                                failure,
                                                "direct-link-retirement",
                                            ) {
                                                ActionOfferHandling::Retry(retained) => {
                                                    assert!(
                                                        retry_actions_a.is_none()
                                                            && retry_actions_b.is_none()
                                                            && quarantined_actions.is_none(),
                                                        "retirement control step requires an empty local ordinary-owner lane"
                                                    );
                                                    retry_actions_a = Some(retained);
                                                }
                                                ActionOfferHandling::RetainAndDrain(retained) => {
                                                    assert!(
                                                        retry_actions_a.is_none()
                                                            && retry_actions_b.is_none()
                                                            && quarantined_actions.is_none(),
                                                        "terminal retirement control step requires an empty local ordinary-owner lane"
                                                    );
                                                    retry_actions_a = Some(retained);
                                                    disable_submission_for_path_fault(
                                                        storage,
                                                        &mut durability_service,
                                                        &retained_frame,
                                                        &pending_frame_acknowledgement,
                                                        supervisor,
                                                        lora_descriptor,
                                                        &mut fail_closed_draining,
                                                        "direct-link-retirement-offer-terminal",
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                progressed = true;
                            }
                            ProductSubmissionDrive::Runtime(Ok(
                                RuntimeStep::PathDiscoveryRequest { offer, progress },
                            )) => {
                                durability_service = durability_service.runtime_progress();
                                match supervisor.request_path(&offer.destination(), &mut rng) {
                                    Ok(actions) => {
                                        let admission = config::ordinary_admission(owner_now);
                                        match supervisor.try_offer_actions(actions, admission) {
                                            Ok(()) => {
                                                debug_assert!(pending_protocol_dispatch.is_none());
                                                pending_protocol_dispatch =
                                                    Some(OrdinaryProtocolDispatch::Submission(
                                                        SubmissionProtocolDispatch::Path(offer),
                                                    ));
                                                info!(
                                                    "e290-node stage=path-discovery status=ADMITTED submission={} ordinal={} progress={progress:?} response_clock=awaiting-first-dispatch",
                                                    offer.id().get(),
                                                    offer.ordinal(),
                                                );
                                                progressed = true;
                                            }
                                            Err(failure) => {
                                                let handling = handle_action_offer_failure(
                                                    failure,
                                                    "path-discovery",
                                                );
                                                let (retained, terminal) = match handling {
                                                    ActionOfferHandling::Retry(retained) => {
                                                        (retained, false)
                                                    }
                                                    ActionOfferHandling::RetainAndDrain(
                                                        retained,
                                                    ) => (retained, true),
                                                };
                                                let retained = retained.with_protocol_dispatch(
                                                    OrdinaryProtocolDispatch::Submission(
                                                        SubmissionProtocolDispatch::Path(offer),
                                                    ),
                                                );
                                                if terminal {
                                                    fail_closed_draining = true;
                                                }
                                                if retry_actions_a.is_none() {
                                                    retry_actions_a = Some(retained);
                                                } else {
                                                    debug_assert!(retry_actions_b.is_none());
                                                    retry_actions_b = Some(retained);
                                                }
                                                progressed = true;
                                            }
                                        }
                                    }
                                    Err(reason) => {
                                        error!(
                                            "e290-node stage=path-discovery status=DISABLED submission={} ordinal={} reason={reason:?} local_submission_admission=closed",
                                            offer.id().get(),
                                            offer.ordinal(),
                                        );
                                        disable_submission_for_path_fault(
                                            storage,
                                            &mut durability_service,
                                            &retained_frame,
                                            &pending_frame_acknowledgement,
                                            supervisor,
                                            lora_descriptor,
                                            &mut fail_closed_draining,
                                            "path-request-build-fault",
                                        );
                                        progressed = true;
                                    }
                                }
                            }
                            ProductSubmissionDrive::Runtime(Ok(
                                RuntimeStep::LinkEstablishment { offer, progress },
                            )) => {
                                durability_service = durability_service.runtime_progress();
                                match supervisor.initiate_link(
                                    &offer.destination(),
                                    MonotonicSeconds::new(now_seconds()),
                                    &mut rng,
                                ) {
                                    Ok((actions, link)) => {
                                        match storage.attach_created_link(offer, link) {
                                            Ok(()) => {
                                                let protocol = OrdinaryProtocolDispatch::Submission(
                                                    SubmissionProtocolDispatch::Link {
                                                        offer,
                                                        link,
                                                    },
                                                );
                                                let admission =
                                                    config::ordinary_admission(owner_now);
                                                match supervisor
                                                    .try_offer_actions(actions, admission)
                                                {
                                                    Ok(()) => {
                                                        debug_assert!(
                                                            pending_protocol_dispatch.is_none()
                                                        );
                                                        pending_protocol_dispatch = Some(protocol);
                                                        info!(
                                                            "e290-node stage=link-establishment status=ADMITTED submission={} generation={} link={:02x?} progress={progress:?} establishment_clock=awaiting-first-dispatch",
                                                            offer.id().get(),
                                                            offer.generation(),
                                                            link.as_bytes(),
                                                        );
                                                        progressed = true;
                                                    }
                                                    Err(failure) => {
                                                        let handling = handle_action_offer_failure(
                                                            failure,
                                                            "link-establishment",
                                                        );
                                                        let (retained, terminal) = match handling {
                                                            ActionOfferHandling::Retry(
                                                                retained,
                                                            ) => (retained, false),
                                                            ActionOfferHandling::RetainAndDrain(
                                                                retained,
                                                            ) => (retained, true),
                                                        };
                                                        let retained = retained
                                                            .with_protocol_dispatch(protocol);
                                                        if terminal {
                                                            error!(
                                                                "e290-node stage=link-establishment status=FAIL reason=ordinary-offer-terminal submission={} generation={} link={:02x?} action=retain-actions-detach-protocol-and-disable-submissions",
                                                                offer.id().get(),
                                                                offer.generation(),
                                                                link.as_bytes(),
                                                            );
                                                            fail_closed_draining = true;
                                                        }
                                                        if retry_actions_a.is_none() {
                                                            retry_actions_a = Some(retained);
                                                        } else {
                                                            debug_assert!(
                                                                retry_actions_b.is_none()
                                                            );
                                                            retry_actions_b = Some(retained);
                                                        }
                                                        progressed = true;
                                                    }
                                                }
                                            }
                                            Err(reason) => {
                                                let aborted =
                                                    supervisor.abort_unestablished_link(link);
                                                error!(
                                                    "e290-node stage=link-establishment status=DISABLED reason=attach-created-link:{reason:?} submission={} generation={} link={:02x?} pending_link_aborted={aborted} local_submission_admission=closed",
                                                    offer.id().get(),
                                                    offer.generation(),
                                                    link.as_bytes(),
                                                );
                                                disable_submission_for_path_fault(
                                                    storage,
                                                    &mut durability_service,
                                                    &retained_frame,
                                                    &pending_frame_acknowledgement,
                                                    supervisor,
                                                    lora_descriptor,
                                                    &mut fail_closed_draining,
                                                    "link-request-attach-fault",
                                                );
                                                progressed = true;
                                            }
                                        }
                                    }
                                    Err(
                                        reason @ (InitiateLinkError::RollbackFailed
                                        | InitiateLinkError::LinkStateNotRetained
                                        | InitiateLinkError::Protocol),
                                    ) => {
                                        error!(
                                            "e290-node stage=link-establishment status=DISABLED submission={} generation={} reason={reason:?} native_link_creation_invariant_or_protocol_fault=true local_submission_admission=closed",
                                            offer.id().get(),
                                            offer.generation(),
                                        );
                                        disable_submission_for_path_fault(
                                            storage,
                                            &mut durability_service,
                                            &retained_frame,
                                            &pending_frame_acknowledgement,
                                            supervisor,
                                            lora_descriptor,
                                            &mut fail_closed_draining,
                                            "link-request-creation-terminal-fault",
                                        );
                                        progressed = true;
                                    }
                                    Err(
                                        reason @ (InitiateLinkError::LinkTableFull { .. }
                                        | InitiateLinkError::LinkIdCollision
                                        | InitiateLinkError::ActionAllocationFailed),
                                    ) => {
                                        let retry_not_before_ms = owner_now
                                            .saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                        durability_service =
                                            durability_service.retry_at(retry_not_before_ms);
                                        warn!(
                                            "e290-node stage=link-establishment status=DEFERRED submission={} generation={} reason={reason:?} retry_not_before_ms={retry_not_before_ms}",
                                            offer.id().get(),
                                            offer.generation(),
                                        );
                                    }
                                }
                            }
                            ProductSubmissionDrive::Runtime(Ok(
                                RuntimeStep::LinkCapacityBackpressured { id, limit },
                            )) => {
                                let retry_not_before_ms =
                                    owner_now.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                durability_service =
                                    durability_service.retry_at(retry_not_before_ms);
                                warn!(
                                    "e290-node stage=link-establishment status=DEFERRED submission={} reason=registry-or-native-link-capacity registry_limit={limit} retry_not_before_ms={retry_not_before_ms}",
                                    id.get(),
                                );
                            }
                            ProductSubmissionDrive::Runtime(Ok(
                                RuntimeStep::DirectLinkAttemptBackpressured { id, link },
                            )) => {
                                let retry_not_before_ms =
                                    owner_now.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                durability_service =
                                    durability_service.retry_at(retry_not_before_ms);
                                warn!(
                                    "e290-node stage=direct-link status=DEFERRED submission={} reason=matching-link-attempt-busy link={:02x?} retry_not_before_ms={retry_not_before_ms}",
                                    id.get(),
                                    link.as_bytes(),
                                );
                            }
                            ProductSubmissionDrive::Runtime(Ok(
                                RuntimeStep::LinkEstablishmentExpired {
                                    offer,
                                    link,
                                    aborted,
                                },
                            )) => {
                                if aborted {
                                    let retry_not_before_ms =
                                        owner_now.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                    durability_service =
                                        durability_service.retry_at(retry_not_before_ms);
                                    warn!(
                                        "e290-node stage=link-establishment status=EXPIRED submission={} generation={} link={:02x?} pending_link_aborted=true retry_not_before_ms={retry_not_before_ms}",
                                        offer.id().get(),
                                        offer.generation(),
                                        link.as_bytes(),
                                    );
                                } else {
                                    error!(
                                        "e290-node stage=link-establishment status=DISABLED reason=expired-link-abort-rejected submission={} generation={} link={:02x?} local_submission_admission=closed",
                                        offer.id().get(),
                                        offer.generation(),
                                        link.as_bytes(),
                                    );
                                    disable_submission_for_path_fault(
                                        storage,
                                        &mut durability_service,
                                        &retained_frame,
                                        &pending_frame_acknowledgement,
                                        supervisor,
                                        lora_descriptor,
                                        &mut fail_closed_draining,
                                        "link-establishment-expiry-abort-fault",
                                    );
                                }
                                progressed = true;
                            }
                            ProductSubmissionDrive::Runtime(Ok(
                                RuntimeStep::LinkEstablishmentLost { offer, link, state },
                            )) => {
                                let retry_not_before_ms =
                                    owner_now.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                durability_service =
                                    durability_service.retry_at(retry_not_before_ms);
                                warn!(
                                    "e290-node stage=link-establishment status=LOST submission={} generation={} link={:02x?} state={state:?} retry_not_before_ms={retry_not_before_ms}",
                                    offer.id().get(),
                                    offer.generation(),
                                    link.as_bytes(),
                                );
                                progressed = true;
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
                            ProductSubmissionDrive::DeferredForLxmfMutation => {
                                let retry_not_before_ms =
                                    owner_now.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
                                durability_service =
                                    durability_service.retry_at(retry_not_before_ms);
                                info!(
                                    "e290-node stage=durable-submission status=DEFERRED reason=lxmf-mutation-in-flight retry_not_before_ms={retry_not_before_ms}"
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
                                            lora_descriptor,
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
                                            lora_descriptor,
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

            let mut application_event_capacity_backpressured = false;
            if application_event_drain_fault.is_none() {
                match supervisor.drain_application_events(&mut application_events) {
                    NodeInterfaceApplicationEventDrain::Idle => {}
                    NodeInterfaceApplicationEventDrain::Drained(report) => {
                        info!(
                            "e290-node stage=application-event-handoff status=ADMITTED events={} unroutable={} owner_capacity={} consumer=transport-neutral",
                            report.accepted_events(),
                            report.unroutable_packets(),
                            config::APPLICATION_EVENT_SLOTS,
                        );
                        progressed = true;
                    }
                    NodeInterfaceApplicationEventDrain::Retained(reason)
                        if reason.is_retryable() =>
                    {
                        application_event_capacity_backpressured = true;
                    }
                    NodeInterfaceApplicationEventDrain::Retained(reason) => {
                        error!(
                            "e290-node stage=application-event-handoff status=QUARANTINED reason={reason:?} exact_actions=retained-in-supervisor packet_completion_lifecycle=continue"
                        );
                        application_event_drain_fault = Some(reason);
                        progressed = true;
                    }
                }
            }
            let ordinary_lane_available = retry_actions_a.is_none()
                && retry_actions_b.is_none()
                && quarantined_actions.is_none()
                && pending_protocol_dispatch.is_none()
                && !supervisor.ingress_actions_pending()
                && !fail_closed_draining;
            if ordinary_lane_available {
                progressed |= drive_one_lxmf_proof(
                    supervisor,
                    &mut delayed_proofs,
                    lxmf_proof_holder,
                    &mut fail_closed_draining,
                    !fresh_nomad_turn_reserved,
                );
            }
            let lxmf_proof_backpressured = should_relieve_lxmf_retry(
                application_event_capacity_backpressured,
                fail_closed_draining,
                lxmf_proof_holder.is_occupied(),
                delayed_proofs.capacities().ready,
            );
            progressed |= drive_one_application_event(
                &mut application_events,
                &mut delayed_proofs,
                lxmf_retries,
                storage,
                supervisor,
                nomad,
                discovered_peers,
                lxmf_destination,
                &mut pending_inbound,
                lxmf_authority_fault,
                *lxmf_pending_owner_fault_observed,
                lxmf_service_fault_observed,
                lxmf_proof_backpressured,
                now_millis(),
            );
            if let Some(pending) = storage.lxmf_pending_message_id()
                && !lxmf_retries.contains_pending_owner(pending)
                && !lxmf_authority_fault.contains_message(pending)
                && !*lxmf_pending_owner_fault_observed
            {
                error!(
                    "e290-node stage=lxmf-pending-owner status=FAIL reason=missing-structural-token message_id={:02x?} action=stop-lxmf-event-processing",
                    pending.as_bytes(),
                );
                *lxmf_pending_owner_fault_observed = true;
            }
            if fail_closed_draining {
                // Retry slots still own their exact action bytes, but those
                // bytes can no longer be re-offered after aggregate
                // fail-closed begins. Detach each pre-dispatch protocol owner
                // before cleaning it so later drain passes retain only the
                // immutable action evidence and cannot clean twice.
                for (slot, trigger, protocol) in [
                    (
                        "a",
                        "submission-protocol-retry-a-fail-closed",
                        retry_actions_a
                            .as_mut()
                            .and_then(RetainedActions::take_protocol_dispatch),
                    ),
                    (
                        "b",
                        "submission-protocol-retry-b-fail-closed",
                        retry_actions_b
                            .as_mut()
                            .and_then(RetainedActions::take_protocol_dispatch),
                    ),
                ] {
                    let Some(protocol) = protocol else {
                        continue;
                    };
                    warn!(
                        "e290-node stage=ordinary-retry-protocol status=DETACHED slot={slot} reason=aggregate-fail-closed action=retain-action-bytes-and-clean-exact-protocol-owner"
                    );
                    let _ = handle_terminal_protocol_dispatch(supervisor, nomad, protocol, true);
                    if matches!(protocol, OrdinaryProtocolDispatch::Submission(_)) {
                        disable_submission_for_path_fault(
                            storage,
                            &mut durability_service,
                            &retained_frame,
                            &pending_frame_acknowledgement,
                            supervisor,
                            lora_descriptor,
                            &mut fail_closed_draining,
                            trigger,
                        );
                    }
                    progressed = true;
                }
                // Keep terminal local evidence live while portable machines
                // finish forced denials and exact completion return.
                let _ = (
                    retry_actions_a.as_ref(),
                    retry_actions_b.as_ref(),
                    pending_protocol_dispatch.as_ref(),
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

fn drive_one_lxmf_proof(
    supervisor: &mut ProductSupervisor,
    delayed_proofs: &mut DelayedProofOwner<'static>,
    holder: &mut LxmfProofActionsHolder,
    fail_closed_draining: &mut bool,
    allow_fresh_staging: bool,
) -> bool {
    let staged = if !holder.is_occupied() {
        if !allow_fresh_staging {
            return false;
        }
        let Some(proof) = delayed_proofs.lease_next() else {
            return false;
        };
        let event_id = proof.event_id();
        let proof_id = proof.id();
        let actions = proof.release_actions();
        #[cfg(feature = "runtime-measurement-hil")]
        RETICULUM_RUNTIME_LXMF_TRACE_EVIDENCE
            .record_proof_released(runtime_lxmf_proof_tag(&actions));
        holder
            .try_stage(actions, config::ordinary_admission(now_millis()))
            .unwrap_or_else(|_| unreachable!("released delayed proof is one packet only"));
        info!(
            "e290-node stage=lxmf-proof status=STAGED event_slot={} event_generation={} proof_slot={} proof_generation={} holder=packet-only ordinary_supervisor=pending",
            event_id.slot().get(),
            event_id.generation().get(),
            proof_id.slot().get(),
            proof_id.generation().get(),
        );
        true
    } else {
        false
    };

    let offer = holder
        .begin_offer()
        .expect("a staged or previously retained LXMF proof exists");
    match offer.try_submit(|actions, admission| {
        supervisor
            .try_offer_actions(actions, admission)
            .map_err(
                |failure| match handle_action_offer_failure(failure, "lxmf-proof") {
                    ActionOfferHandling::Retry(retained) => {
                        let (actions, admission, submission_protocol) = retained.into_parts();
                        debug_assert!(submission_protocol.is_none());
                        LxmfProofSinkFailure::returned(
                            LxmfProofOfferHandling::Retry,
                            actions,
                            admission,
                        )
                    }
                    ActionOfferHandling::RetainAndDrain(retained) => {
                        let (actions, admission, submission_protocol) = retained.into_parts();
                        debug_assert!(submission_protocol.is_none());
                        LxmfProofSinkFailure::returned(
                            LxmfProofOfferHandling::RetainAndDrain,
                            actions,
                            admission,
                        )
                    }
                },
            )
    }) {
        Ok(()) => {
            #[cfg(feature = "runtime-measurement-hil")]
            RETICULUM_RUNTIME_LXMF_TRACE_EVIDENCE.record_proof_handed_off();
            info!(
                "e290-node stage=lxmf-proof status=HANDED-OFF owner=ordinary-supervisor direct_radio_send=false"
            );
            true
        }
        Err(LxmfProofOfferHandling::Retry) => staged,
        Err(LxmfProofOfferHandling::RetainAndDrain) => {
            *fail_closed_draining = true;
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drive_one_application_event(
    owner: &mut ApplicationEventOwner<'static>,
    delayed_proofs: &mut DelayedProofOwner<'static>,
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    storage: &mut ProductStorageCoordinator,
    supervisor: &ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    discovered_peers: &mut DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    lxmf_destination: Option<DestinationHash>,
    pending_inbound: &mut Option<InboxCandidate>,
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    pending_owner_fault_observed: bool,
    service_fault_observed: &mut bool,
    proof_backpressured: bool,
    now_ms: u64,
) -> bool {
    let pending_message = storage.lxmf_pending_message_id();
    let held_store_fault = pending_message.is_some_and(|message_id| {
        retries.has_fault_hold(message_id) || authority_fault.has_fault_hold(message_id)
    });

    if held_store_fault && let Some(entry) = retries.take_nonheld() {
        let class = entry.class();
        let message_id = entry.message_id();
        match owner.try_reacquire_quarantined(entry.into_token()) {
            Ok(lease) => {
                let event_id = lease.id();
                lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
                warn!(
                    "e290-node stage=lxmf-retry status=DISCARDED slot={} generation={} prior_class={class:?} reason=pending-store-fault-held proof=not-sent",
                    event_id.slot().get(),
                    event_id.generation().get(),
                );
            }
            Err(failure) => {
                let reason = failure.reason();
                retain_lxmf_authority_fault(
                    authority_fault,
                    LxmfRetryEntry::new(failure.into_token(), u64::MAX, class, message_id),
                );
                error!(
                    "e290-node stage=lxmf-retry status=FAIL reason={reason:?} action=retain-unchanged-token-and-stop-lxmf"
                );
            }
        }
        return true;
    }

    let lxmf_fail_stopped =
        authority_fault.is_some() || pending_owner_fault_observed || *service_fault_observed;
    if (proof_backpressured || lxmf_fail_stopped)
        && let Some(entry) = retries.take_pressure_relief()
    {
        let class = entry.class();
        let message_id = entry.message_id();
        match owner.try_reacquire_quarantined(entry.into_token()) {
            Ok(lease) => {
                let event_id = lease.id();
                let discard_reason = if proof_backpressured {
                    ApplicationEventDiscardReason::DownstreamCapacity
                } else {
                    ApplicationEventDiscardReason::ConsumerUnavailable
                };
                lease.discard(discard_reason);
                warn!(
                    "e290-node stage=lxmf-retry status=DISCARDED slot={} generation={} prior_class={class:?} reason={} proof=not-sent pending-store-owner=preserved",
                    event_id.slot().get(),
                    event_id.generation().get(),
                    if proof_backpressured {
                        "ordinary-proof-pressure-relief"
                    } else {
                        "lxmf-service-fail-stop-cleanup"
                    },
                );
            }
            Err(failure) => {
                let reason = failure.reason();
                retain_lxmf_authority_fault(
                    authority_fault,
                    LxmfRetryEntry::new(failure.into_token(), u64::MAX, class, message_id),
                );
                error!(
                    "e290-node stage=lxmf-retry status=FAIL reason={reason:?} action=retain-unchanged-token-and-stop-lxmf"
                );
            }
        }
        return true;
    }

    let retry = if authority_fault.is_none() && !pending_owner_fault_observed {
        retries.take_due(now_ms, pending_message)
    } else {
        None
    };
    let (lease, retry_class, retry_message_id) = if let Some(entry) = retry {
        let class = entry.class();
        let message_id = entry.message_id();
        match owner.try_reacquire_quarantined(entry.into_token()) {
            Ok(lease) => (lease, Some(class), message_id),
            Err(failure) => {
                let reason = failure.reason();
                retain_lxmf_authority_fault(
                    authority_fault,
                    LxmfRetryEntry::new(failure.into_token(), u64::MAX, class, message_id),
                );
                error!(
                    "e290-node stage=lxmf-retry status=FAIL reason={reason:?} action=retain-unchanged-token-and-stop-lxmf"
                );
                return true;
            }
        }
    } else {
        let Some(lease) = owner.lease_next() else {
            return false;
        };
        (lease, None, None)
    };

    let is_lxmf = match lease.event() {
        ApplicationEvent::DataReceived { destination, .. } => {
            lxmf_destination.is_some_and(|lxmf| destination == lxmf.as_bytes())
        }
        ApplicationEvent::LinkData {
            binding, context, ..
        } => {
            *context == APPLICATION_LINK_CONTEXT_NONE
                && binding.role() == ApplicationLinkRole::Responder
                && lxmf_destination.is_some_and(|lxmf| binding.destination() == lxmf.as_bytes())
        }
        _ => false,
    };

    if is_lxmf {
        if held_store_fault
            || authority_fault.is_some()
            || pending_owner_fault_observed
            || *service_fault_observed
        {
            let event_id = lease.id();
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
            warn!(
                "e290-node stage=lxmf-ingress status=DISCARDED slot={} generation={} reason=service-fail-stopped proof=not-sent routing=continue",
                event_id.slot().get(),
                event_id.generation().get(),
            );
            return true;
        }
        if pending_message.is_some() && retry_class != Some(LxmfRetryClass::StoreReconcile) {
            if proof_backpressured {
                let event_id = lease.id();
                lease.discard(ApplicationEventDiscardReason::DownstreamCapacity);
                warn!(
                    "e290-node stage=lxmf-ingress status=DISCARDED slot={} generation={} reason=ordinary-proof-pressure-relief pending-store-owner=preserved proof=not-sent",
                    event_id.slot().get(),
                    event_id.generation().get(),
                );
                return true;
            }
            quarantine_lxmf_retry(
                lease,
                retries,
                authority_fault,
                now_ms,
                LxmfRetryClass::StoreBusy,
                None,
            );
            return true;
        }
        return drive_lxmf_event(
            lease,
            delayed_proofs,
            retries,
            storage,
            supervisor,
            lxmf_destination.expect("LXMF event matched an active destination"),
            authority_fault,
            service_fault_observed,
            retry_class,
            retry_message_id,
            now_ms,
        );
    }

    drive_non_lxmf_application_event(
        lease,
        storage,
        supervisor,
        nomad,
        discovered_peers,
        pending_inbound,
        now_ms,
    )
}

fn drive_non_lxmf_application_event(
    lease: ApplicationEventLease<'_, 'static>,
    storage: &mut ProductStorageCoordinator,
    supervisor: &ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    discovered_peers: &mut DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    pending_inbound: &mut Option<InboxCandidate>,
    now_ms: u64,
) -> bool {
    let event_id = lease.id();
    let sequence = lease.sequence();
    let kind = lease.event().kind();

    match nomad.observe_application_event(lease.event()) {
        NomadEventObservation::Applied => {
            let event = lease.acknowledge();
            drop(event);
            info!(
                "e290-node stage=nomad-application-event status=ACKNOWLEDGED kind={kind} slot={} generation={} sequence={}",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            return true;
        }
        NomadEventObservation::Fault(fault) => {
            let event = lease.acknowledge();
            drop(event);
            error!(
                "e290-node stage=nomad-application-event status=FAULT-CLEANED kind={kind} slot={} generation={} sequence={} reason={fault:?}",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            return true;
        }
        NomadEventObservation::Unrelated => {}
    }

    match lease.event() {
        ApplicationEvent::DataReceived {
            destination,
            payload,
        } => match InboxCandidate::new(InboxDestination::new(*destination), payload) {
            Ok(candidate) if pending_inbound.is_none() => {
                let payload_len = payload.len();
                let _ = drive_inbound_candidate(storage, pending_inbound, candidate);
                let event = lease.acknowledge();
                drop(event);
                info!(
                    "e290-node stage=application-event-consumer status=ACKNOWLEDGED kind={kind} slot={} generation={} sequence={} consumer=durable-rns-inbox payload_len={payload_len}",
                    event_id.slot().get(),
                    event_id.generation().get(),
                    sequence.get(),
                );
            }
            Ok(_candidate) => {
                let payload_len = payload.len();
                storage.record_inbound_drop();
                warn!(
                    "e290-node stage=rns-inbox-admission status=DROPPED reason=pending-candidate-capacity overflow_policy=drop-newest payload_len={payload_len} proof_semantics=decrypt-only"
                );
                lease.discard(ApplicationEventDiscardReason::DownstreamCapacity);
            }
            Err(reason) => {
                storage.record_inbound_drop();
                warn!(
                    "e290-node stage=rns-inbox-admission status=DROPPED reason={reason:?} overflow_policy=drop-newest proof_semantics=decrypt-only"
                );
                lease.discard(ApplicationEventDiscardReason::InvalidPayload);
            }
        },
        ApplicationEvent::AnnounceReceived {
            destination,
            identity,
            hops,
            app_data,
        } => {
            let destination = *destination;
            let identity = *identity;
            let hops = *hops;
            let app_data_len = app_data.as_ref().map_or(0, |data| data.len());
            let destination_hash = DestinationHash::new(destination);
            let Some(public_key) = supervisor.recall_lxmf_delivery_identity(&destination_hash)
            else {
                lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
                return true;
            };
            let observation = AuthenticatedAnnounceObservation::new(
                PeerDestinationHash::new(destination),
                PeerIdentityHash::new(identity),
                IdentityPublicKey::new(public_key),
                app_data.as_deref().unwrap_or(&[]),
                ObservationMetadata::new(
                    hops,
                    ObservedInterfaceId::new(LORA_INTERFACE.get()),
                    SignalObservations::UNKNOWN,
                    PeerObservedMillis::new(now_ms),
                ),
            );
            match discovered_peers.observe(observation) {
                Ok(outcome) => {
                    let disposition = outcome.disposition();
                    let generation = outcome.generation().get();
                    let event = lease.acknowledge();
                    drop(event);
                    info!(
                        "e290-node stage=peer-discovery status=OBSERVED destination={destination:02x?} identity={identity:02x?} hops={hops} interface={} app_data_len={app_data_len} generation={generation} disposition={disposition:?}",
                        LORA_INTERFACE.get(),
                    );
                }
                Err(reason) => {
                    warn!(
                        "e290-node stage=peer-discovery status=DROPPED destination={destination:02x?} reason={reason:?} routing=unchanged"
                    );
                    lease.discard(ApplicationEventDiscardReason::DownstreamCapacity);
                }
            }
        }
        ApplicationEvent::Tick { .. } => {
            lease.discard(ApplicationEventDiscardReason::MaintenanceCoalesced);
        }
        ApplicationEvent::ResourceOffered { .. }
        | ApplicationEvent::ResourceProgress { .. }
        | ApplicationEvent::ResourceComplete { .. }
        | ApplicationEvent::ResourceFailed { .. }
        | ApplicationEvent::ResourceRejected { .. }
        | ApplicationEvent::RequestProgress { .. } => {
            error!(
                "e290-node stage=application-event-consumer status=QUARANTINED kind={kind} slot={} generation={} sequence={} reason=resource-ingress-disabled-invariant",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
        }
        // Inbound Link request values belong to the future destination-bound
        // Nomad responder and are never outbound response completions.
        ApplicationEvent::RequestValueReceived { .. } => {
            warn!(
                "e290-node stage=application-event-consumer status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=consumer-unavailable future_consumer=nomad-responder",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
        }
        ApplicationEvent::ProofReceived { .. }
        | ApplicationEvent::ReceiptFailed { .. }
        | ApplicationEvent::LinkEstablished { .. }
        | ApplicationEvent::LinkRttUpdated { .. }
        | ApplicationEvent::LinkData { .. }
        | ApplicationEvent::ChannelMessages { .. }
        // Inbound Link requests are the future destination-bound Nomad
        // responder surface. Keep them explicit and unavailable until the
        // responder owns response dispatch end to end.
        | ApplicationEvent::RequestReceived { .. }
        | ApplicationEvent::ResponseReceived { .. }
        | ApplicationEvent::LinkClosed { .. }
        | ApplicationEvent::LinkIdentified { .. }
        | ApplicationEvent::RequestFailed { .. } => {
            warn!(
                "e290-node stage=application-event-consumer status=DISCARDED kind={kind} slot={} generation={} sequence={} reason=consumer-unavailable future_consumer=lxmf-or-application-service",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn drive_lxmf_event(
    lease: ApplicationEventLease<'_, 'static>,
    delayed_proofs: &mut DelayedProofOwner<'static>,
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    storage: &mut ProductStorageCoordinator,
    supervisor: &ProductSupervisor,
    lxmf_destination: DestinationHash,
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    service_fault_observed: &mut bool,
    attempted_retry_class: Option<LxmfRetryClass>,
    attempted_message_id: Option<reticulum_lxmf_model::MessageId>,
    now_ms: u64,
) -> bool {
    let event_id = lease.id();
    let sequence = lease.sequence();
    let resolver = |source: &[u8; 16]| supervisor.recall_identity(&DestinationHash::new(*source));
    let outcome = storage.commit_lxmf_event(
        lease,
        delayed_proofs,
        LocalDeliveryDestination::new(*lxmf_destination.as_bytes()),
        wire_limits(),
        &resolver,
        stamp_policy(),
    );
    match outcome {
        ProductLxmfAdmission::DeferredForCredentialMutation(lease) => {
            let (class, message_id) = retry_after_pre_io_deferral(
                attempted_retry_class,
                attempted_message_id,
                LxmfRetryClass::CrossStoreBusy,
            );
            quarantine_lxmf_retry(lease, retries, authority_fault, now_ms, class, message_id);
        }
        ProductLxmfAdmission::DeferredForJournalMutation(lease) => {
            let (class, message_id) = retry_after_pre_io_deferral(
                attempted_retry_class,
                attempted_message_id,
                LxmfRetryClass::CrossStoreBusy,
            );
            quarantine_lxmf_retry(lease, retries, authority_fault, now_ms, class, message_id);
        }
        ProductLxmfAdmission::RuntimeUnavailable(lease) => {
            lease.discard(ApplicationEventDiscardReason::ConsumerUnavailable);
            warn!(
                "e290-node stage=lxmf-ingress status=DISCARDED slot={} generation={} sequence={} reason=store-unavailable proof=not-sent routing=continue",
                event_id.slot().get(),
                event_id.generation().get(),
                sequence.get(),
            );
        }
        ProductLxmfAdmission::Ingress(DurableIngressOutcome::Durable(success)) => {
            #[cfg(feature = "runtime-measurement-hil")]
            record_runtime_lxmf_durable(success);
            log_durable_lxmf(success);
        }
        ProductLxmfAdmission::Ingress(DurableIngressOutcome::Retained(retained)) => {
            let pending = storage.lxmf_pending_message_id();
            let action = retention_action(retained.reason(), pending);
            let (lease, reason) = retained.into_parts();
            match action {
                LxmfRetentionAction::Retry(class) => {
                    let (class, message_id) = if matches!(
                        class,
                        LxmfRetryClass::AdmissionDeferred | LxmfRetryClass::DelayedProofBusy
                    ) {
                        retry_after_pre_io_deferral(
                            attempted_retry_class,
                            attempted_message_id,
                            class,
                        )
                    } else if class == LxmfRetryClass::StoreReconcile {
                        (class, pending)
                    } else {
                        (class, None)
                    };
                    quarantine_lxmf_retry(
                        lease,
                        retries,
                        authority_fault,
                        now_ms,
                        class,
                        message_id,
                    );
                    warn!(
                        "e290-node stage=lxmf-ingress status=RETRY reason={reason:?} class={class:?} retry_not_before_ms={} proof=retained",
                        now_ms.saturating_add(config::STORAGE_RETRY_BACKOFF_MS),
                    );
                }
                LxmfRetentionAction::HoldPendingFault => {
                    let Some(message_id) = pending else {
                        lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
                        error!(
                            "e290-node stage=lxmf-ingress status=FAIL reason=pending-fault-without-message-id detail={reason:?} action=quarantine-event"
                        );
                        return true;
                    };
                    quarantine_lxmf_retry(
                        lease,
                        retries,
                        authority_fault,
                        u64::MAX,
                        LxmfRetryClass::StoreFaultHold,
                        Some(message_id),
                    );
                    error!(
                        "e290-node stage=lxmf-ingress status=FAIL-STOP reason={reason:?} message_id={:02x?} action=hold-exact-token-no-retry-until-reset other-flash-mutation=blocked routing=continue proof=not-sent",
                        message_id.as_bytes(),
                    );
                }
                LxmfRetentionAction::Terminal(terminal) => {
                    handle_terminal_lxmf_reject(
                        lease,
                        storage,
                        terminal,
                        &reason,
                        service_fault_observed,
                    );
                }
            }
        }
    }
    true
}

fn log_durable_lxmf(success: DurableIngressSuccess) {
    let receipt = success.receipt();
    info!(
        "e290-node stage=lxmf-ingress status=DURABLE kind={:?} slot={} generation={} message_id={:02x?} handle={} proof_ready={} proof_handoff=ordinary-supervisor",
        success.kind(),
        success.event_id().slot().get(),
        success.event_id().generation().get(),
        receipt.message_id().as_bytes(),
        receipt.handle().get(),
        success.queued_proof_id().is_some(),
    );
}

#[cfg(feature = "runtime-measurement-hil")]
fn record_runtime_lxmf_durable(success: DurableIngressSuccess) {
    let receipt = success.receipt();
    let kind = match success.kind() {
        DurableIngressCommitKind::New => RuntimeLxmfCommitKind::New,
        DurableIngressCommitKind::Replay => RuntimeLxmfCommitKind::AlreadyDurable,
    };
    RETICULUM_RUNTIME_LXMF_TRACE_EVIDENCE.record_durable(
        kind,
        receipt.message_id().as_bytes(),
        receipt.handle().get(),
        success.queued_proof_id().is_some(),
    );
}

#[cfg(feature = "runtime-measurement-hil")]
fn runtime_lxmf_proof_tag(actions: &NodeActions) -> Option<u64> {
    let packet = actions.packets.first()?;
    reticulum_node_core::delivery_proof_correlation_tag(packet.bytes())
}

fn quarantine_lxmf_retry(
    lease: ApplicationEventLease<'_, 'static>,
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    now_ms: u64,
    class: LxmfRetryClass,
    message_id: Option<reticulum_lxmf_model::MessageId>,
) {
    let event_id = lease.id();
    let token = lease.quarantine_for_retry(LXMF_RETRY_QUARANTINE);
    let retry_not_before_ms = if class == LxmfRetryClass::StoreFaultHold {
        u64::MAX
    } else {
        now_ms.saturating_add(config::STORAGE_RETRY_BACKOFF_MS)
    };
    retain_lxmf_retry_entry(
        retries,
        authority_fault,
        LxmfRetryEntry::new(token, retry_not_before_ms, class, message_id),
    );
    info!(
        "e290-node stage=lxmf-retry status=RETAINED slot={} generation={} class={class:?} retry_not_before_ms={retry_not_before_ms}",
        event_id.slot().get(),
        event_id.generation().get(),
    );
}

fn retain_lxmf_retry_entry(
    retries: &mut LxmfRetrySet<'static, { config::APPLICATION_EVENT_SLOTS }>,
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    entry: LxmfRetryEntry<'static>,
) {
    if let Err(failure) = retries.try_insert(entry) {
        let retained = failure.into_entry();
        error!(
            "e290-node stage=lxmf-retry status=FAIL slot={} class={:?} reason=retry-set-slot-occupied action=retain-extra-authority-and-stop-lxmf",
            retained.slot(),
            retained.class(),
        );
        retain_lxmf_authority_fault(authority_fault, retained);
    }
}

fn retain_lxmf_authority_fault(
    authority_fault: &mut LxmfAuthorityFault<'static, { config::APPLICATION_EVENT_SLOTS }>,
    entry: LxmfRetryEntry<'static>,
) {
    if let Err(failure) = authority_fault.try_capture(entry) {
        let retained_overflow_owner = failure.into_entry();
        error!(
            "e290-node stage=lxmf-authority status=FAIL reason=bounded-fault-owner-exhausted action=quarantined-capability-unrecoverable-continue-non-lxmf"
        );
        // At most 16 physical application-event tokens can exist, so a 16-slot
        // fault owner cannot overflow in a valid owner graph. If that invariant
        // is violated, the event itself remains quarantined in its caller-owned
        // slot even though this recovery capability becomes unreachable. Do
        // not globally spin the shared non-LXMF consumer.
        let _ = retained_overflow_owner;
    }
}

fn handle_terminal_lxmf_reject<E: core::fmt::Debug>(
    lease: ApplicationEventLease<'_, 'static>,
    storage: &mut ProductStorageCoordinator,
    terminal: LxmfTerminalReject,
    detail: &E,
    service_fault_observed: &mut bool,
) {
    let event_id = lease.id();
    match terminal {
        LxmfTerminalReject::InvalidMessage => {
            lease.discard(ApplicationEventDiscardReason::InvalidPayload);
            warn!(
                "e290-node stage=lxmf-ingress status=REJECTED slot={} generation={} reason={detail:?} proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
        LxmfTerminalReject::StoreFull
        | LxmfTerminalReject::IndexFull
        | LxmfTerminalReject::HandleExhausted => {
            lease.discard(ApplicationEventDiscardReason::DownstreamCapacity);
            warn!(
                "e290-node stage=lxmf-ingress status=CAPACITY-REJECTED slot={} generation={} policy={terminal:?} reason={detail:?} service=remain-enabled-for-replay proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
        LxmfTerminalReject::StoreFault => {
            let disabled = storage.disable_lxmf_service_if_clean();
            *service_fault_observed = true;
            lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            error!(
                "e290-node stage=lxmf-ingress status=QUARANTINED slot={} generation={} policy={terminal:?} reason={detail:?} service_disabled={disabled} proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
        LxmfTerminalReject::HashCollision => {
            lease.discard(ApplicationEventDiscardReason::InvalidPayload);
            warn!(
                "e290-node stage=lxmf-ingress status=COLLISION-REJECTED slot={} generation={} policy={terminal:?} reason={detail:?} service=remain-enabled-for-other-messages proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
        }
        LxmfTerminalReject::Unrelated
        | LxmfTerminalReject::CandidateContradiction
        | LxmfTerminalReject::ProofInvariant => {
            let disabled = storage.disable_lxmf_service_if_clean();
            *service_fault_observed = true;
            lease.quarantine(ApplicationEventQuarantineReason::ConsumerFault);
            error!(
                "e290-node stage=lxmf-ingress status=QUARANTINED slot={} generation={} policy={terminal:?} reason={detail:?} service_disabled={disabled} proof=not-sent",
                event_id.slot().get(),
                event_id.generation().get(),
            );
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
    supervisor: &ProductSupervisor,
    discovered_peers: &DiscoveredPeers<
        { config::LXMF_DISCOVERED_PEERS },
        { config::LXMF_DISCOVERED_PEER_APP_DATA_BYTES },
    >,
    peer_discovery_incarnation: LxmfPeerDiscoveryIncarnation,
    now_ms: u64,
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
        let dispatch = storage.dispatch_authenticated_request(
            supervisor,
            discovered_peers,
            peer_discovery_incarnation,
            now_ms,
            request,
            identity,
        );
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
        } else if next_lane == Some(PairingCommandLane::Live)
            && state
                .live_retry_not_before_ms
                .is_none_or(|deadline| now_millis >= deadline)
        {
            progressed |= admit_live_pairing_command(storage, state, rng, now_millis);
            if state.pending_live_command.is_some() {
                // A conflicting cross-store mutation is causally after this
                // request. Keep later policy events and timeout polls behind
                // the retained command until storage ownership settles.
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
                        ProductInitializationRequest::DeferredForLxmfMutation => {
                            info!(
                                "e290-node stage=credential-initialization status=DEFERRED reason=lxmf-mutation-in-flight"
                            );
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
    now_millis: u64,
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
    state.live_retry_not_before_ms = None;

    match storage.request_live_pairing(at, connection, request, rng) {
        ProductLivePairingAdmission::DeferredForJournalMutation(request) => {
            state.pending_live_command = Some(LivePairingCommand::new(at, connection, request));
        }
        ProductLivePairingAdmission::DeferredForLxmfMutation(request) => {
            let retry_not_before_ms = now_millis.saturating_add(config::STORAGE_RETRY_BACKOFF_MS);
            state.pending_live_command = Some(LivePairingCommand::new(at, connection, request));
            state.live_retry_not_before_ms = Some(retry_not_before_ms);
            info!(
                "e290-node stage=credential-live-pairing status=DEFERRED reason=lxmf-mutation-in-flight retry_not_before_ms={retry_not_before_ms}"
            );
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

const NOMAD_LINK_PREPARATION_PROTOCOL_CODE: u16 = 1;
const NOMAD_LINK_PREPARATION_STATE_CODE: u16 = 2;
const NOMAD_LINK_PREPARATION_ROLLBACK_CODE: u16 = 3;
const NOMAD_REQUEST_LINK_UNAVAILABLE_CODE: u16 = 1;
const NOMAD_REQUEST_PREPARATION_SIZE_CODE: u16 = 2;
const NOMAD_REQUEST_PREPARATION_CRYPTO_CODE: u16 = 3;
const NOMAD_REQUEST_PREPARATION_PACKET_CODE: u16 = 4;
const NOMAD_REQUEST_PREPARATION_INVARIANT_CODE: u16 = 5;
const NOMAD_REQUEST_DISPATCH_TERMINAL_CODE: u16 = 1;
const NOMAD_REQUEST_DISPATCH_NATIVE_CODE: u16 = 3;
const NOMAD_LINK_DISPATCH_CORRELATION_CODE: u16 = 4;
const NOMAD_LINK_TIMEOUT_CODE: u16 = 1;

#[allow(clippy::too_many_arguments)]
fn drive_one_nomad_command(
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    pending_protocol_dispatch: &mut Option<OrdinaryProtocolDispatch>,
    fail_closed_draining: &mut bool,
    native_terminal_events_pending: bool,
    ordinary_owners_quiescent: bool,
    rng: &mut Trng,
    now_ms: u64,
) -> bool {
    let exact_undispatched_path_owner = [
        retry_actions_a
            .as_ref()
            .and_then(RetainedActions::protocol_dispatch),
        retry_actions_b
            .as_ref()
            .and_then(RetainedActions::protocol_dispatch),
        *pending_protocol_dispatch,
    ]
    .into_iter()
    .flatten()
    .any(|protocol| {
        matches!(
            protocol,
            OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Path { .. })
        )
    });
    let usable_path = nomad.active_destination().is_some_and(|destination| {
        supervisor.has_usable_path(&DestinationHash::new(*destination.as_bytes()))
    });
    let command = match nomad.next_step(
        NomadMillis::new(now_ms),
        usable_path,
        exact_undispatched_path_owner,
    ) {
        Ok(NomadDriveStep::Progressed) => return true,
        Ok(NomadDriveStep::Command(command)) => command,
        Ok(NomadDriveStep::Idle) => return false,
        Err(fault) => {
            error!("e290-node stage=nomad-path-observation status=FAULT reason={fault:?}");
            return true;
        }
    };
    let fresh_command = matches!(
        command,
        CoordinatorCommand::RequestPath { .. }
            | CoordinatorCommand::EstablishLink { .. }
            | CoordinatorCommand::PrepareAnonymousRequest { .. }
    );
    if fresh_command && !ordinary_owners_quiescent {
        return false;
    }
    if native_terminal_events_pending
        && matches!(
            command,
            CoordinatorCommand::AbortTimedOutLink { .. }
                | CoordinatorCommand::CancelTimedOutRequest { .. }
        )
    {
        return false;
    }

    match command {
        CoordinatorCommand::RequestPath { destination } => {
            let native_destination = DestinationHash::new(*destination.as_bytes());
            if supervisor.has_usable_path(&native_destination) {
                if let Err(fault) = nomad.coordinator_mut().path_already_available(destination) {
                    error!(
                        "e290-node stage=nomad-path status=FAULT reason=already-available-transition destination={:02x?} fault={fault:?}",
                        destination.as_bytes(),
                    );
                }
                return true;
            }
            match supervisor.request_path(&native_destination, rng) {
                Ok(actions) => offer_nomad_actions(
                    supervisor,
                    nomad,
                    actions,
                    OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Path { destination }),
                    retry_actions_a,
                    retry_actions_b,
                    pending_protocol_dispatch,
                    fail_closed_draining,
                    now_ms,
                ),
                Err(reason) => {
                    warn!(
                        "e290-node stage=nomad-path status=DEFERRED reason=prepare:{reason:?} destination={:02x?}",
                        destination.as_bytes(),
                    );
                    true
                }
            }
        }
        CoordinatorCommand::EstablishLink { destination } => {
            let native_destination = DestinationHash::new(*destination.as_bytes());
            match supervisor.initiate_link(
                &native_destination,
                MonotonicSeconds::new(now_ms / 1_000),
                rng,
            ) {
                Ok((actions, link)) => offer_nomad_actions(
                    supervisor,
                    nomad,
                    actions,
                    OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Link {
                        destination,
                        link,
                    }),
                    retry_actions_a,
                    retry_actions_b,
                    pending_protocol_dispatch,
                    fail_closed_draining,
                    now_ms,
                ),
                Err(
                    InitiateLinkError::LinkTableFull { .. }
                    | InitiateLinkError::LinkIdCollision
                    | InitiateLinkError::ActionAllocationFailed,
                ) => false,
                Err(reason) => {
                    let code = match reason {
                        InitiateLinkError::Protocol => NOMAD_LINK_PREPARATION_PROTOCOL_CODE,
                        InitiateLinkError::LinkStateNotRetained => {
                            NOMAD_LINK_PREPARATION_STATE_CODE
                        }
                        InitiateLinkError::RollbackFailed => NOMAD_LINK_PREPARATION_ROLLBACK_CODE,
                        InitiateLinkError::LinkTableFull { .. }
                        | InitiateLinkError::LinkIdCollision
                        | InitiateLinkError::ActionAllocationFailed => unreachable!(),
                    };
                    if let Err(fault) = nomad.coordinator_mut().link_preparation_failed(
                        destination,
                        LinkFailure::new(LinkFailureStage::Preparation, code),
                    ) {
                        error!(
                            "e290-node stage=nomad-link status=FAULT reason=preparation-transition destination={:02x?} fault={fault:?}",
                            destination.as_bytes(),
                        );
                    }
                    true
                }
            }
        }
        CoordinatorCommand::PrepareAnonymousRequest {
            destination: _,
            link,
            path,
            requested_at,
        } => {
            let native_link = LinkHandle::new(*link.as_bytes());
            match supervisor.prepare_anonymous_request(
                native_link,
                path.as_str(),
                requested_at.as_seconds_f64(),
                rng,
            ) {
                Ok(Ok(prepared)) => {
                    let handle = prepared.handle();
                    let (actions, _) = prepared.into_parts();
                    match nomad.coordinator_mut().request_prepared(
                        link,
                        RequestId::new(*handle.request()),
                        handle,
                    ) {
                        Ok(_) => offer_nomad_actions(
                            supervisor,
                            nomad,
                            actions,
                            OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Request {
                                handle,
                            }),
                            retry_actions_a,
                            retry_actions_b,
                            pending_protocol_dispatch,
                            fail_closed_draining,
                            now_ms,
                        ),
                        Err(fault) => {
                            let reconciliation = supervisor.reconcile_request_dispatch(handle);
                            let acknowledgement = nomad
                                .coordinator_mut()
                                .acknowledge_invariant_request_cancellation(handle);
                            let expected_reconciliation =
                                reconciliation == RequestDispatchReconciliation::ReclaimedPrepared;
                            if !expected_reconciliation {
                                *fail_closed_draining = true;
                            }
                            error!(
                                "e290-node stage=nomad-request status=FAULT reason=request-prepared-transition link={:02x?} request={:02x?} fault={fault:?} native_reconciliation={reconciliation:?} expected_prepared={expected_reconciliation} cleanup_ack={acknowledgement:?}",
                                handle.link(),
                                handle.request(),
                            );
                            true
                        }
                    }
                }
                Ok(Err(
                    PrepareRequestError::LinkNotFound
                    | PrepareRequestError::LinkNotActive
                    | PrepareRequestError::LinkInterfaceUnknown,
                )) => {
                    if let Err(fault) = nomad
                        .coordinator_mut()
                        .request_link_unavailable(link, NOMAD_REQUEST_LINK_UNAVAILABLE_CODE)
                    {
                        error!(
                            "e290-node stage=nomad-request status=FAULT reason=link-unavailable-transition link={:02x?} fault={fault:?}",
                            link.as_bytes(),
                        );
                    }
                    true
                }
                Ok(Err(
                    PrepareRequestError::DispatchTableFull { .. }
                    | PrepareRequestError::RequestAlreadyTracked
                    | PrepareRequestError::RequestAllocationFailed
                    | PrepareRequestError::ActionAllocationFailed,
                )) => false,
                Ok(Err(reason)) => {
                    let code = match reason {
                        PrepareRequestError::RequestTooLarge { .. } => {
                            NOMAD_REQUEST_PREPARATION_SIZE_CODE
                        }
                        PrepareRequestError::Crypto => NOMAD_REQUEST_PREPARATION_CRYPTO_CODE,
                        PrepareRequestError::PacketBuild => NOMAD_REQUEST_PREPARATION_PACKET_CODE,
                        PrepareRequestError::Invariant => NOMAD_REQUEST_PREPARATION_INVARIANT_CODE,
                        PrepareRequestError::LinkNotFound
                        | PrepareRequestError::LinkNotActive
                        | PrepareRequestError::LinkInterfaceUnknown
                        | PrepareRequestError::DispatchTableFull { .. }
                        | PrepareRequestError::RequestAlreadyTracked
                        | PrepareRequestError::RequestAllocationFailed
                        | PrepareRequestError::ActionAllocationFailed => unreachable!(),
                    };
                    if let Err(fault) = nomad.coordinator_mut().request_preparation_failed(
                        link,
                        RequestFailure::new(RequestFailureStage::Preparation, code),
                    ) {
                        error!(
                            "e290-node stage=nomad-request status=FAULT reason=preparation-transition link={:02x?} fault={fault:?}",
                            link.as_bytes(),
                        );
                    }
                    true
                }
                Err(fault) => {
                    error!(
                        "e290-node stage=nomad-request status=DEFERRED reason=supervisor-fault fault={fault:?}"
                    );
                    false
                }
            }
        }
        CoordinatorCommand::ExpirePath { candidate } => {
            if let Err(fault) = nomad.coordinator_mut().confirm_path_timeout(candidate) {
                error!("e290-node stage=nomad-path status=FAULT reason=timeout fault={fault:?}");
            }
            true
        }
        CoordinatorCommand::AbortTimedOutLink { candidate } => {
            let link = LinkHandle::new(*candidate.link().as_bytes());
            if supervisor.abort_unestablished_link(link) {
                if let Err(fault) = nomad
                    .coordinator_mut()
                    .confirm_link_timeout_after_abort(candidate, NOMAD_LINK_TIMEOUT_CODE)
                {
                    error!(
                        "e290-node stage=nomad-link status=FAULT reason=timeout-after-abort fault={fault:?}"
                    );
                }
                true
            } else {
                let native_state = supervisor.link_state(link);
                let fault = nomad.coordinator_mut().link_native_reconciliation_failed(
                    candidate.destination(),
                    candidate.link(),
                    CoordinatorOperation::LinkFailed,
                );
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-link status=FAULT reason=timeout-native-phase link={:02x?} native_state={native_state:?} fault={fault:?} action=no-timeout-outcome-and-fail-closed-drain",
                    link.as_bytes(),
                );
                true
            }
        }
        CoordinatorCommand::AbortLinkForInvariant { candidate } => {
            let link = LinkHandle::new(*candidate.link().as_bytes());
            let aborted = supervisor.abort_unestablished_link(link);
            if aborted || supervisor.link_state(link).is_none() {
                if let Err(fault) = nomad
                    .coordinator_mut()
                    .acknowledge_invariant_link_abort(candidate)
                {
                    error!(
                        "e290-node stage=nomad-link status=FAULT reason=invariant-abort-ack fault={fault:?}"
                    );
                }
                true
            } else {
                let native_state = supervisor.link_state(link);
                let acknowledgement = nomad
                    .coordinator_mut()
                    .acknowledge_invariant_link_abort(candidate);
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-link status=FAULT reason=invariant-abort-native-phase link={:02x?} native_state={native_state:?} cleanup_ack={acknowledgement:?} action=retain-sticky-fault-and-require-reset",
                    link.as_bytes(),
                );
                true
            }
        }
        CoordinatorCommand::CancelTimedOutRequest { token, candidate } => {
            let reconciliation = supervisor.reconcile_request_dispatch(token);
            if reconciliation == RequestDispatchReconciliation::ReclaimedConfirmed {
                if let Err(fault) = nomad
                    .coordinator_mut()
                    .confirm_request_timeout_after_native_cancel(token, candidate)
                {
                    *fail_closed_draining = true;
                    error!(
                        "e290-node stage=nomad-request status=FAULT reason=timeout-after-reclaim native_reconciliation={reconciliation:?} fault={fault:?} action=fail-closed-drain"
                    );
                }
            } else {
                let fault = nomad
                    .coordinator_mut()
                    .request_native_reconciliation_failed(
                        token,
                        NativeRequestPhase::Confirmed,
                        CoordinatorOperation::ConfirmRequestTimeout,
                    );
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-request status=FAULT reason=timeout-native-phase native_reconciliation={reconciliation:?} fault={fault:?} action=no-timeout-outcome-and-fail-closed-drain"
                );
            }
            true
        }
        CoordinatorCommand::CancelRequestForInvariant {
            token,
            link: _,
            request: _,
            phase: _,
        } => {
            let reconciliation = supervisor.reconcile_request_dispatch(token);
            if let Err(fault) = nomad
                .coordinator_mut()
                .acknowledge_invariant_request_cancellation(token)
            {
                error!(
                    "e290-node stage=nomad-request status=FAULT reason=invariant-reclaim-ack native_reconciliation={reconciliation:?} fault={fault:?}"
                );
            }
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn offer_nomad_actions(
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    actions: NodeActions,
    protocol: OrdinaryProtocolDispatch,
    retry_actions_a: &mut Option<RetainedActions>,
    retry_actions_b: &mut Option<RetainedActions>,
    pending_protocol_dispatch: &mut Option<OrdinaryProtocolDispatch>,
    fail_closed_draining: &mut bool,
    now_ms: u64,
) -> bool {
    match supervisor.try_offer_actions(actions, config::ordinary_admission(now_ms)) {
        Ok(()) => {
            debug_assert!(pending_protocol_dispatch.is_none());
            *pending_protocol_dispatch = Some(protocol);
        }
        Err(failure) => {
            let handling = handle_action_offer_failure(failure, "nomad-command");
            let (retained, terminal) = match handling {
                ActionOfferHandling::Retry(retained) => {
                    (retained.with_protocol_dispatch(protocol), false)
                }
                ActionOfferHandling::RetainAndDrain(retained) => {
                    let _ = handle_terminal_protocol_dispatch(supervisor, nomad, protocol, true);
                    (retained, true)
                }
            };
            if retry_actions_a.is_none() {
                *retry_actions_a = Some(retained);
            } else {
                debug_assert!(retry_actions_b.is_none());
                *retry_actions_b = Some(retained);
            }
            if terminal {
                *fail_closed_draining = true;
                error!(
                    "e290-node stage=nomad-command status=FAIL-STOP reason=ordinary-offer-terminal"
                );
            }
        }
    }
    true
}

fn confirm_nomad_dispatch(
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    protocol: NomadProtocolDispatch,
    first_dispatch: bool,
    protocol_token_present: bool,
    protocol_confirmed: bool,
    native_terminal_events_pending: bool,
    dispatched_at: MonotonicMillis,
) -> NomadDispatchResolution {
    match protocol {
        NomadProtocolDispatch::Path { destination } => {
            if !first_dispatch || protocol_token_present {
                let dispatch = nomad
                    .coordinator_mut()
                    .path_request_dispatched(destination, NomadMillis::new(dispatched_at.get()));
                let terminal = dispatch.and_then(|()| {
                    nomad
                        .coordinator_mut()
                        .path_unavailable(destination)
                        .map(|_| ())
                });
                error!(
                    "e290-node stage=nomad-path status=FAULT reason=dispatch-correlation first_dispatch={first_dispatch} protocol_token_present={protocol_token_present} terminal_reconciliation={terminal:?}"
                );
                return NomadDispatchResolution::InvariantReconciled;
            }
            match nomad
                .coordinator_mut()
                .path_request_dispatched(destination, NomadMillis::new(dispatched_at.get()))
            {
                Ok(()) => NomadDispatchResolution::Committed,
                Err(fault) => {
                    error!("e290-node stage=nomad-path status=FAULT reason={fault:?}");
                    NomadDispatchResolution::InvariantReconciled
                }
            }
        }
        NomadProtocolDispatch::Link { destination, link } => {
            if !first_dispatch || !protocol_token_present || !protocol_confirmed {
                let aborted = supervisor.abort_unestablished_link(link);
                let terminal = nomad.coordinator_mut().link_preparation_failed(
                    destination,
                    LinkFailure::new(
                        LinkFailureStage::Dispatch,
                        NOMAD_LINK_DISPATCH_CORRELATION_CODE,
                    ),
                );
                error!(
                    "e290-node stage=nomad-link status=FAULT reason=dispatch-correlation first_dispatch={first_dispatch} protocol_token_present={protocol_token_present} protocol_confirmed={protocol_confirmed} pending_link_aborted={aborted} terminal_reconciliation={terminal:?}"
                );
                return NomadDispatchResolution::InvariantReconciled;
            }
            let result = nomad.coordinator_mut().link_request_dispatched(
                destination,
                LinkId::new(*link.as_bytes()),
                NomadMillis::new(dispatched_at.get()),
            );
            match result {
                Ok(()) => NomadDispatchResolution::Committed,
                Err(fault) => {
                    let cleanup = nomad.next_command(NomadMillis::new(dispatched_at.get()));
                    let cleanup_transferred = if let Some(
                        CoordinatorCommand::AbortLinkForInvariant { candidate },
                    ) = cleanup
                    {
                        let aborted = supervisor.abort_unestablished_link(link);
                        if aborted {
                            let acknowledgement = nomad
                                .coordinator_mut()
                                .acknowledge_invariant_link_abort(candidate);
                            error!(
                                "e290-node stage=nomad-link status=FAULT reason=dispatch-transition fault={fault:?} pending_link_aborted=true cleanup_ack={acknowledgement:?}"
                            );
                            false
                        } else {
                            error!(
                                "e290-node stage=nomad-link status=FAULT reason=dispatch-transition fault={fault:?} pending_link_aborted=false action=retain-coordinator-cleanup"
                            );
                            true
                        }
                    } else {
                        error!(
                            "e290-node stage=nomad-link status=FAULT reason=dispatch-transition fault={fault:?} cleanup_command={cleanup:?}"
                        );
                        false
                    };
                    if cleanup_transferred {
                        NomadDispatchResolution::CleanupTransferred
                    } else {
                        NomadDispatchResolution::InvariantReconciled
                    }
                }
            }
        }
        NomadProtocolDispatch::Request { handle } => {
            if !first_dispatch || protocol_token_present {
                let reconciliation = supervisor.reconcile_request_dispatch(handle);
                let fault = nomad
                    .coordinator_mut()
                    .request_native_reconciliation_failed(
                        handle,
                        NativeRequestPhase::Prepared,
                        CoordinatorOperation::ConfirmRequestDispatch,
                    );
                error!(
                    "e290-node stage=nomad-request status=FAULT reason=dispatch-correlation first_dispatch={first_dispatch} protocol_token_present={protocol_token_present} native_reconciliation={reconciliation:?} fault={fault:?}"
                );
                return NomadDispatchResolution::InvariantReconciled;
            }
            match supervisor.confirm_request_dispatch(
                handle,
                MonotonicSeconds::new(dispatched_at.get() / 1_000),
                first_dispatch,
            ) {
                Ok(RequestDispatchConfirmation::Confirmed) => {
                    match nomad
                        .coordinator_mut()
                        .request_dispatch_confirmed(handle, NomadMillis::new(dispatched_at.get()))
                    {
                        Ok(()) => NomadDispatchResolution::Committed,
                        Err(fault) => {
                            let reconciliation = supervisor.reconcile_request_dispatch(handle);
                            let acknowledgement = nomad
                                .coordinator_mut()
                                .acknowledge_invariant_request_cancellation(handle);
                            error!(
                                "e290-node stage=nomad-request status=FAULT reason=coordinator-dispatch-confirmation fault={fault:?} native_reconciliation={reconciliation:?} cleanup_ack={acknowledgement:?}"
                            );
                            NomadDispatchResolution::InvariantReconciled
                        }
                    }
                }
                Ok(RequestDispatchConfirmation::NotFirstDispatch) => {
                    let reconciliation = supervisor.reconcile_request_dispatch(handle);
                    let fault = nomad
                        .coordinator_mut()
                        .request_native_reconciliation_failed(
                            handle,
                            NativeRequestPhase::Prepared,
                            CoordinatorOperation::ConfirmRequestDispatch,
                        );
                    error!(
                        "e290-node stage=nomad-request status=FAULT reason=native-not-first-after-router-first native_reconciliation={reconciliation:?} fault={fault:?}"
                    );
                    NomadDispatchResolution::InvariantReconciled
                }
                Err(RequestDispatchError::NotTracked) if native_terminal_events_pending => {
                    match nomad
                        .coordinator_mut()
                        .request_dispatch_confirmed(handle, NomadMillis::new(dispatched_at.get()))
                    {
                        Ok(()) => {
                            warn!(
                                "e290-node stage=nomad-request status=TERMINAL-EVENT-PENDING reason=native-request-already-reclaimed action=preserve-product-correlation-until-event-drain"
                            );
                            NomadDispatchResolution::Committed
                        }
                        Err(fault) => {
                            error!(
                                "e290-node stage=nomad-request status=FAULT reason=terminal-event-pending-transition fault={fault:?}"
                            );
                            NomadDispatchResolution::InvariantReconciled
                        }
                    }
                }
                Err(reason) => {
                    let reconciliation = supervisor.reconcile_request_dispatch(handle);
                    let lifecycle_race = matches!(
                        reason,
                        RequestDispatchError::LinkNotFound | RequestDispatchError::LinkNotActive
                    );
                    if lifecycle_race && reconciliation == RequestDispatchReconciliation::Absent {
                        let terminal = nomad
                            .coordinator_mut()
                            .request_dispatch_failed_after_native_reclaim(
                                handle,
                                RequestFailure::new(
                                    RequestFailureStage::Dispatch,
                                    NOMAD_REQUEST_DISPATCH_NATIVE_CODE,
                                ),
                            );
                        error!(
                            "e290-node stage=nomad-request status=RECONCILED reason=native-dispatch-confirmation:{reason:?} native_reconciliation={reconciliation:?} terminal_reconciliation={terminal:?}"
                        );
                        if terminal.is_ok() {
                            NomadDispatchResolution::Reconciled
                        } else {
                            NomadDispatchResolution::InvariantReconciled
                        }
                    } else {
                        let fault = nomad
                            .coordinator_mut()
                            .request_native_reconciliation_failed(
                                handle,
                                NativeRequestPhase::Prepared,
                                CoordinatorOperation::ConfirmRequestDispatch,
                            );
                        error!(
                            "e290-node stage=nomad-request status=FAULT reason=native-dispatch-confirmation:{reason:?} native_reconciliation={reconciliation:?} fault={fault:?}"
                        );
                        NomadDispatchResolution::InvariantReconciled
                    }
                }
            }
        }
    }
}

fn handle_terminal_protocol_dispatch(
    supervisor: &mut ProductSupervisor,
    nomad: &mut ProductNomadRuntimeState,
    protocol: OrdinaryProtocolDispatch,
    terminal: bool,
) -> bool {
    match protocol {
        OrdinaryProtocolDispatch::Submission(SubmissionProtocolDispatch::Path(_)) => false,
        OrdinaryProtocolDispatch::Submission(SubmissionProtocolDispatch::Link { offer, link }) => {
            let aborted = supervisor.abort_unestablished_link(link);
            warn!(
                "e290-node stage=link-establishment status=ABORT submission={} generation={} link={:02x?} pending_link_aborted={aborted}",
                offer.id().get(),
                offer.generation(),
                link.as_bytes(),
            );
            false
        }
        OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Path { destination }) => {
            warn!(
                "e290-node stage=nomad-path status=RELEASED reason=pre-dispatch-return destination={:02x?}",
                destination.as_bytes(),
            );
            false
        }
        OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Link { destination, link }) => {
            let aborted = supervisor.abort_unestablished_link(link);
            let native_state = supervisor.link_state(link);
            let reconciliation_fault = (!aborted && native_state.is_some()).then(|| {
                nomad.coordinator_mut().link_native_reconciliation_failed(
                    destination,
                    LinkId::new(*link.as_bytes()),
                    CoordinatorOperation::ConfirmLinkRequest,
                )
            });
            warn!(
                "e290-node stage=nomad-link status=ABORT reason=pre-dispatch-return destination={:02x?} link={:02x?} pending_link_aborted={aborted} native_state={native_state:?} reconciliation_fault={reconciliation_fault:?}",
                destination.as_bytes(),
                link.as_bytes(),
            );
            reconciliation_fault.is_some()
        }
        OrdinaryProtocolDispatch::Nomad(NomadProtocolDispatch::Request { handle }) => {
            let reconciliation = supervisor.reconcile_request_dispatch(handle);
            let expected_pre_dispatch =
                reconciliation == RequestDispatchReconciliation::ReclaimedPrepared;
            let result = if !expected_pre_dispatch {
                Err(nomad
                    .coordinator_mut()
                    .request_native_reconciliation_failed(
                        handle,
                        NativeRequestPhase::Prepared,
                        CoordinatorOperation::CancelRequestDispatch,
                    ))
            } else if terminal {
                nomad
                    .coordinator_mut()
                    .request_dispatch_failed_after_native_reclaim(
                        handle,
                        RequestFailure::new(
                            RequestFailureStage::Dispatch,
                            NOMAD_REQUEST_DISPATCH_TERMINAL_CODE,
                        ),
                    )
            } else {
                nomad
                    .coordinator_mut()
                    .request_dispatch_canceled_after_native_cancel(handle)
            };
            if let Err(fault) = result {
                error!(
                    "e290-node stage=nomad-request status=FAULT reason=pre-dispatch-reconciliation terminal={terminal} native_reconciliation={reconciliation:?} expected_pre_dispatch={expected_pre_dispatch} fault={fault:?}"
                );
            }
            !expected_pre_dispatch || result.is_err()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn disable_submission_for_path_fault(
    storage: &mut ProductStorageCoordinator,
    durability_service: &mut DurabilityServiceState,
    retained_frame: &Option<AuthorizedFrameObservation>,
    pending_frame_acknowledgement: &Option<AuthorizedFrameObservation>,
    supervisor: &mut ProductSupervisor,
    online: InterfaceDescriptor,
    fail_closed_draining: &mut bool,
    trigger: &'static str,
) {
    storage.disable_submission_service();
    let previous = *durability_service;
    *durability_service = (*durability_service).permanent_failure(authorized_frame_durability(
        retained_frame,
        pending_frame_acknowledgement,
    ));
    if !previous.is_active_owner_fail_stopped() && durability_service.is_active_owner_fail_stopped()
    {
        enter_active_owner_durability_fail_stop(supervisor, online, fail_closed_draining, trigger);
    }
}

fn enter_active_owner_durability_fail_stop(
    supervisor: &mut ProductSupervisor,
    online: InterfaceDescriptor,
    fail_closed_draining: &mut bool,
    trigger: &'static str,
) {
    match supervisor.disable_interface(online.lease()) {
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
    fail_closed_draining: &mut bool,
    routed_interface: reticulum_node_core::PacketInterfaceId,
    first_dispatch: bool,
    interval_ordered: bool,
) {
    let Some(routed_descriptor) = supervisor.interface_registry().descriptor(routed_interface)
    else {
        error!(
            "e290-node stage=protocol-dispatch-confirmation status=FAIL reason=routed-interface-unregistered routed_interface={} first_dispatch={} interval_ordered={} edge=router-egress-acceptance-not-rf-txdone action=fail-closed-drain",
            routed_interface.get(),
            first_dispatch,
            interval_ordered,
        );
        *fail_closed_draining = true;
        return;
    };
    match supervisor.disable_interface(routed_descriptor.lease()) {
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
            *quarantined_actions = Some(RetainedActions::ordinary(actions, admission));
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
            ActionOfferHandling::Retry(RetainedActions::ordinary(actions, admission))
        }
        config::OrdinaryOfferDisposition::QuarantineAndDrain => {
            error!(
                "e290-node stage={stage} status=TERMINAL-OFFER reason={reason:?} packets={} events={} unroutable={} action=quarantine-owner-and-fail-closed-drain",
                actions.packets.len(),
                actions.events.len(),
                actions.unroutable_packets,
            );
            ActionOfferHandling::RetainAndDrain(RetainedActions::ordinary(actions, admission))
        }
    }
}

fn retry_retained_actions(
    supervisor: &mut ProductSupervisor,
    retained: &mut Option<RetainedActions>,
    stage: &'static str,
) -> ActionRetryStep {
    if retained
        .as_ref()
        .and_then(RetainedActions::protocol_dispatch)
        .is_some()
        && !ordinary_router_is_idle(supervisor)
    {
        return ActionRetryStep::Busy;
    }
    let Some(owner) = retained.take() else {
        error!(
            "e290-node stage={stage} status=FAIL reason=selected-retry-owner-missing action=fail-closed-drain"
        );
        return ActionRetryStep::Terminal;
    };
    let (actions, admission, protocol_dispatch) = owner.into_parts();
    match supervisor.try_offer_actions(actions, admission) {
        Ok(()) => ActionRetryStep::Accepted(protocol_dispatch),
        Err(failure) => match handle_action_offer_failure(failure, stage) {
            ActionOfferHandling::Retry(owner) => {
                *retained = Some(match protocol_dispatch {
                    Some(protocol) => owner.with_protocol_dispatch(protocol),
                    None => owner,
                });
                ActionRetryStep::Busy
            }
            ActionOfferHandling::RetainAndDrain(owner) => {
                *retained = Some(match protocol_dispatch {
                    Some(protocol) => owner.with_protocol_dispatch(protocol),
                    None => owner,
                });
                ActionRetryStep::Terminal
            }
        },
    }
}

fn ordinary_router_is_idle(supervisor: &ProductSupervisor) -> bool {
    let capacities = supervisor.ordinary_capacities();
    capacities.active == 0 && supervisor.ordinary_parked_count() == capacities.registered
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
        NodeInterfaceSupervisorTransition::Lifecycle(_) => 1 << 5,
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
