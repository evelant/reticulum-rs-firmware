//! Permanent transport-neutral node owner task.

#![allow(clippy::too_many_arguments)]

pub(crate) use core::mem;

pub(crate) use embassy_futures::yield_now;
pub(crate) use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
pub(crate) use embassy_time::{Duration, Instant, Timer};
pub(crate) use esp_hal::rng::Trng;
pub(crate) use log::{debug, error, info, warn};
pub(crate) use rand_core::RngCore;
pub(crate) use reticulum_announce_clock::BootEpoch;
pub(crate) use reticulum_device_api::{
    DestinationHash as ApiDestinationHash, IdentitySummary, LxmfPeerDiscoveryIncarnation,
    RmapDeferredReason,
};
pub(crate) use reticulum_device_api_handoff::{LocalApiReply, LocalApiRequest, NodeHandoff};
pub(crate) use reticulum_device_api_pairing::{
    AbortCurrentResponse, AbortResult, ActivateFailure, ActivateResponse, BeginResponse,
    PairingFailure, PairingRequest, PairingResponse, ProofStartResponse,
};
pub(crate) use reticulum_device_api_pairing_control::{
    ControlRequest, ControlResponse, InitializeResult,
};
pub(crate) use reticulum_device_api_pairing_policy::{
    AcquirePairingExclusive, ButtonEffect, ConnectionId, ExclusiveAcquireOutcome,
    MonotonicMillis as PairingMillis, PolicyEvent,
};
pub(crate) use reticulum_device_api_session::AuthenticatedGrant;
#[cfg(feature = "appliance")]
pub(crate) use reticulum_e290_firmware::ble_bond_handoff::{
    BleBondCommitCommand, BleBondCommitOutcome, BleBondCommitReply, NodeBleBondHandoff,
};
pub(crate) use reticulum_e290_firmware::{
    announce_time::{
        BootAnnounceClock, BootAnnounceSchedule, BootAnnounceTiming, ManualAnnounceSchedule,
        ScheduledAnnounce,
    },
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
        admission_deferred_source, admission_retry_delay_ms, retention_action,
        retry_after_pre_io_deferral, should_relieve_lxmf_retry, stamp_policy, wire_limits,
    },
    nomad_api::{NomadFetchApiState, ProductNomadFetchPort},
    nomad_coordinator::{CoordinatorCommand, CoordinatorOperation, NativeRequestPhase},
    nomad_responder::{
        NOMAD_NODE_ANNOUNCE_APP_DATA, NomadResponderDisposition, classify_nomad_responder_event,
    },
    nomad_runtime::{NomadDriveStep, NomadEventObservation, ProductNomadRuntimeState},
    pairing_control_handoff::{
        ButtonObservationReply, ExclusiveAcquisitionReply, LifecycleAcknowledgement,
        NodePairingHandoff, PairingControlCommand, PairingControlReply, PairingControlReplyKind,
    },
    pairing_control_mapping::{
        public_initialization_drive, public_initialization_refusal, public_initialization_status,
    },
    radio_diagnostics::RadioDiagnosticsCell,
    radio_trace::{
        RadioTraceAttemptOutcome, RadioTraceAttemptTerminal, RadioTraceInboundProofStage,
        RadioTraceProofIngress, RadioTraceRouteResolution, RadioTraceRouteSelected,
    },
    reticulum_probe::{
        PROBE_DATA_OWNER_LEASE_MS, PROBE_PAYLOAD_BYTES, ProductReticulumProbePort,
        ProductReticulumProbeState, ReticulumProbeDrive,
    },
    rmap_discovery::{
        RMAP_DISCOVERY_INTERVAL_SECONDS, RMAP_DISCOVERY_RETRY_SECONDS, RmapDiscoveryRuntime,
        RmapDiscoveryStatusCell, RmapPublicationPolicy, RmapStampStep,
    },
    session_admission_handoff::{
        NodeSessionAdmissionHandoff, SessionAdmissionCommand, SessionAdmissionOutcome,
        SessionAdmissionReply,
    },
};
pub(crate) use reticulum_interface_router::{InterfaceDescriptor, SealedIngressPacket};
pub(crate) use reticulum_lxmf_durable_ingress::{DurableIngressOutcome, DurableIngressSuccess};
pub(crate) use reticulum_lxmf_ingress::LocalDeliveryDestination;
pub(crate) use reticulum_node_core::{
    APPLICATION_LINK_CONTEXT_NONE, AnnounceAdmissionError, ApplicationEvent,
    ApplicationEventDiscardReason, ApplicationEventLease, ApplicationEventOfferError,
    ApplicationEventOwner, ApplicationEventQuarantineReason, ApplicationLinkRole, AttemptOutcome,
    AuthorizedFrameObservation, DelayedProofOwner, DestinationHash, InboundProofEvidence,
    InitiateLinkError, LinkHandle, MonotonicInstant, MonotonicMillis, MonotonicSeconds,
    NodeActions, OrdinaryDispatchToken, OrdinaryTransmissionOutcome, OutboundDispatchInterval,
    PacketInterfaceId, PrepareRequestError, PrepareResponseError, ProofProbeIdentityAliasError,
    ReceiptCorrelationError, RequestDispatchConfirmation, RequestDispatchError,
    RequestDispatchReconciliation, RequestHandle, SubmitError, TxLeaseDeadline, TxTarget,
};
pub(crate) use reticulum_nomad_protocol::{
    DestinationHash as NomadDestinationHash, LinkFailure, LinkFailureStage, LinkId,
    MonotonicMillis as NomadMillis, RequestFailure, RequestFailureStage, RequestId,
};
pub(crate) use reticulum_peer_discovery::{
    AuthenticatedAnnounceObservation, DestinationHash as PeerDestinationHash, DiscoveredPeers,
    IdentityHash as PeerIdentityHash, IdentityPublicKey, MonotonicMillis as PeerObservedMillis,
    ObservationMetadata, ObservedInterfaceId, SignalObservations,
};
pub(crate) use reticulum_radio_interface::LoRaProfile;
pub(crate) use reticulum_rns_interface_discovery::{DiscoveryStampSearch, PackedDiscoveryInfo};
pub(crate) use reticulum_storage_actor::{DriveError, ProjectorOperationError};
pub(crate) use reticulum_submission_runtime::{
    FrameOfferProgress, LinkEstablishmentOffer, PathDiscoveryOffer, RuntimeError, RuntimeStep,
};
pub(crate) use reticulum_tx_handoff::AuthorizedFrameNodeHandoff;
pub(crate) use reticulum_tx_supervisor::{
    DataRouterPrepareRequest, DataRouterPrepareResult, DataRouterStep,
    NodeInterfaceAnnounceFlushResult, NodeInterfaceApplicationEventDrain,
    NodeInterfaceDataPrepareResult, NodeInterfaceIngressActionFault,
    NodeInterfaceIngressRecycleFault, NodeInterfaceIngressStep, NodeInterfaceOrdinaryOfferFailure,
    NodeInterfaceSupervisorTransition, NodeInterfaceTickResult, OrdinaryRouterCompletionProgress,
    OrdinaryRouterFaultResidueKind, OrdinaryRouterStep, RouteDiagnosticsSnapshot,
};

#[cfg(feature = "appliance")]
pub(crate) use crate::platform_storage::ProductBleBondCommitOutcome;
pub(crate) use crate::{
    ProductSupervisor, config,
    platform_storage::{
        ProductCredentialInitializationPort, ProductInitializationDrive,
        ProductInitializationRequest, ProductLivePairingAdmission, ProductLxmfAdmission,
        ProductStorageCoordinator, ProductSubmissionDrive,
    },
};
#[cfg(feature = "display")]
pub(crate) use reticulum_e290_firmware::display_handoff::{
    DisplayHomeTelemetry, DisplayTelemetryPublisher,
};

mod state;
pub(crate) use state::*;
mod steps;
pub(crate) use steps::*;

#[allow(clippy::too_many_arguments)]
#[embassy_executor::task]
pub async fn run(
    supervisor: &'static mut ProductSupervisor,
    storage: &'static mut ProductStorageCoordinator,
    diagnostics: NodeDiagnosticsOwners,
    mut application_events: ApplicationEventOwner<'static>,
    mut delayed_proofs: DelayedProofOwner<'static>,
    application_volatile: &'static mut ApplicationVolatileState,
    destinations: NodeApplicationDestinations,
    rmap_publication_policy: RmapPublicationPolicy,
    automatic_announces_enabled: bool,
    peer_discovery_incarnation: LxmfPeerDiscoveryIncarnation,
    handoffs: NodeHandoffs,
    offline_descriptor: InterfaceDescriptor,
    announce_epoch: BootEpoch,
    lxmf_delivery_announce_app_data: &'static [u8],
    mut rng: Trng,
) {
    let NodeApplicationDestinations {
        lxmf: lxmf_destination,
        nomad: nomad_destination,
        proof_probe: proof_probe_destination,
        rmap: rmap_destination,
    } = destinations;
    let NodeDiagnosticsOwners {
        radio: radio_diagnostics,
        routes: route_diagnostics,
        lora_profile,
    } = diagnostics;
    let primary_destination = *supervisor.destination_hash().as_bytes();
    let identity_summary = match lxmf_destination {
        Some(lxmf_destination) => IdentitySummary::with_lxmf_delivery_destination(
            ApiDestinationHash(primary_destination),
            ApiDestinationHash(*lxmf_destination.as_bytes()),
        ),
        None => IdentitySummary::new(ApiDestinationHash(primary_destination)),
    };
    let NodeHandoffParts {
        control: mut pairing_handoff,
        live: mut live_pairing_handoff,
        #[cfg(feature = "appliance")]
            ble_bond: mut ble_bond_handoff,
        session_admission: mut session_admission_handoff,
        mut authenticated_api,
        frame: mut frame_handoff,
        #[cfg(feature = "display")]
        mut display_telemetry,
    } = handoffs.into_parts();
    // The concrete actor now owns Ready/Offline reporting through its fixed
    // interface-fabric lifecycle capability. This descriptor remains only the
    // generation-bound product-policy authority for LoRa-specific durability
    // fail-stop paths.
    let lora_descriptor = offline_descriptor;
    let lora_interface = lora_descriptor.lease().interface();

    // These slots are interchangeable. Either can hold a locally
    // backpressured envelope or an exact coordinator-rejected envelope.
    let mut retry_actions_a: Option<RetainedActions> = None;
    let mut retry_actions_b: Option<RetainedActions> = None;
    // RMAP action admission is always placed in retry slot A while both
    // ordinary retry owners are empty. Cadence advances only when that exact
    // owner, or the initial flush, reaches coordinator acceptance.
    let mut rmap_retry_pending = false;
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
    let announce_timing = BootAnnounceTiming::for_profile(lora_profile);
    let mut announce_schedule = BootAnnounceSchedule::new(
        now_seconds(),
        primary_destination,
        lxmf_destination.is_some(),
        announce_timing,
    );
    let mut manual_announce_schedule =
        ManualAnnounceSchedule::new(lxmf_destination.is_some(), announce_timing);
    let mut lane = 0_u8;
    let mut fresh_nomad_turn_armed = false;
    let mut observed_recycle_fault: Option<NodeInterfaceIngressRecycleFault> = None;
    let mut durability_service =
        DurabilityServiceState::from_runtime_available(storage.submission_service_available());
    let mut retained_frame: Option<AuthorizedFrameObservation> = None;
    let mut pending_frame_acknowledgement: Option<AuthorizedFrameObservation> = None;
    let mut pending_probe_frame_acknowledgement: Option<AuthorizedFrameObservation> = None;
    let mut pairing = PairingNodeState::new();
    let mut authenticated_api_state = AuthenticatedApiNodeState::new();
    let mut application_event_drain_fault: Option<ApplicationEventOfferError> = None;
    let mut lxmf_proof_trace_evidence: Option<InboundProofEvidence> = None;
    let ApplicationVolatileState {
        retries: lxmf_retries,
        proof_holder: lxmf_proof_holder,
        authority_faults: lxmf_authority_fault,
        discovered_peers,
        pending_owner_fault_observed: lxmf_pending_owner_fault_observed,
        service_fault_observed: lxmf_service_fault_observed,
        nomad,
        nomad_api,
        reticulum_probe,
        rmap,
    } = application_volatile;
    #[cfg(feature = "display")]
    let mut displayed_uncollected_messages = None;

    loop {
        let mut progressed = false;

        #[cfg(feature = "display")]
        {
            if let Some(status) = storage.lxmf_mailbox_status() {
                let uncollected_messages = status.uncollected_count();
                if displayed_uncollected_messages != Some(uncollected_messages) {
                    if let Some(publisher) = display_telemetry.as_mut() {
                        publisher.publish_latest(DisplayHomeTelemetry::new(uncollected_messages));
                        progressed = true;
                    }
                    displayed_uncollected_messages = Some(uncollected_messages);
                }
            }
        }

        match rmap.step_stamp_search(now_seconds()) {
            RmapStampStep::Completed { attempts } => {
                info!(
                    "e290-node stage=rmap-discovery-stamp status=READY attempts={attempts} publication=due"
                );
            }
            RmapStampStep::Exhausted => {
                warn!(
                    "e290-node stage=rmap-discovery-stamp status=DISABLED reason=counter-exhausted lora_routing=continue"
                );
            }
            RmapStampStep::Disabled | RmapStampStep::Ready | RmapStampStep::Progressed { .. } => {}
        }

        progressed |= step_pairing_frontier(
            storage,
            &mut pairing_handoff,
            &mut live_pairing_handoff,
            #[cfg(feature = "appliance")]
            &mut ble_bond_handoff,
            &mut session_admission_handoff,
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
        if let Some(observation) = pending_probe_frame_acknowledgement.take() {
            match frame_handoff.acknowledgements().try_send(observation) {
                Ok(()) => {
                    info!(
                        "e290-node stage=reticulum-proof-probe-frame status=VOLATILE-ACK interface={} packet_len={} durable_submission_journal=bypassed",
                        observation.interface().get(),
                        observation.packet_len(),
                    );
                    progressed = true;
                }
                Err(full) => {
                    pending_probe_frame_acknowledgement = Some(full.into_inner());
                }
            }
        }
        if pending_probe_frame_acknowledgement.is_none()
            && let Some(observation) = pending_frame_acknowledgement.take()
        {
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
        if pending_probe_frame_acknowledgement.is_none()
            && pending_frame_acknowledgement.is_none()
            && retained_frame.is_none()
            && let Some(observation) = frame_handoff.requests().try_receive()
        {
            match reticulum_probe.classifies_authorized(observation) {
                Ok(true) => match reticulum_probe.observe_authorized(observation) {
                    Ok(()) => {
                        pending_probe_frame_acknowledgement = Some(observation);
                        progressed = true;
                    }
                    Err(reason) => {
                        error!(
                            "e290-node stage=reticulum-proof-probe-frame status=FAIL reason={reason:?} action=fail-closed-drain"
                        );
                        fail_closed_draining = true;
                        progressed = true;
                    }
                },
                Ok(false) => {
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
                Err(reason) => {
                    error!(
                        "e290-node stage=reticulum-proof-probe-frame status=FAIL reason={reason:?} action=retain-unacknowledged-frame-and-fail-closed-drain"
                    );
                    retained_frame = Some(observation);
                    fail_closed_draining = true;
                    progressed = true;
                }
            }
        }
        if durability_service.can_offer_authorized_frame()
            && let Some(observation) = retained_frame
        {
            let frame_offer = storage.offer_authorized_frame(observation);
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
            progressed |= drive_reticulum_probe_recovery(
                supervisor,
                reticulum_probe,
                &mut fail_closed_draining,
            );
            progressed |= drive_reticulum_probe_terminal(
                supervisor,
                reticulum_probe,
                &mut fail_closed_draining,
                now_millis(),
            );
            let retry_slot_available = retry_actions_a.is_none() || retry_actions_b.is_none();
            if !fail_closed_draining
                && retry_slot_available
                && let Some(rejected) = supervisor.take_rejected_actions()
            {
                let reason = rejected.reason();
                let (actions, elapsed_admission) = rejected.into_parts();
                let retain_in_flight_path = matches!(
                    pending_protocol_dispatch,
                    Some(OrdinaryProtocolDispatch::Submission(
                        SubmissionProtocolDispatch::Path {
                            token: staged_token,
                            ..
                        }
                    )) if config::pending_path_protocol_disposition(staged_token)
                        == config::PendingPathProtocolDisposition::RetainInFlight
                );
                let protocol_dispatch = if retain_in_flight_path {
                    None
                } else {
                    pending_protocol_dispatch.take()
                };
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

            // A durably retained inbound proof is the highest-priority fresh
            // ordinary admission. Offering it before every fair lane lets it
            // overtake queued ingress forwarding, retries, protocol ticks and
            // announces. The ordinary coordinator still returns Busy while an
            // already-owned packet is active, preserving that packet exactly.
            if !fail_closed_draining {
                progressed |= drive_one_lxmf_proof(
                    supervisor,
                    &mut delayed_proofs,
                    lxmf_proof_holder,
                    &mut lxmf_proof_trace_evidence,
                    radio_diagnostics,
                    lora_interface,
                    &mut retry_actions_a,
                    &mut retry_actions_b,
                    pending_protocol_dispatch.is_some(),
                    &mut fail_closed_draining,
                );
            }
            let lxmf_proof_admission_pending = config::lxmf_proof_admission_pending(
                lxmf_proof_holder.is_occupied(),
                delayed_proofs.capacities().ready,
            );

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
                    let retry_capacity_available =
                        retry_actions_a.is_none() || retry_actions_b.is_none();
                    let settling_ingress = supervisor.ingress_recycle_fault().is_some()
                        || terminal_residue_can_move
                        || terminal_buffer_can_move;
                    // A fresh ingress packet can create another ordinary
                    // envelope. Do not dequeue one while both retry slots are
                    // occupied, or its later rejected output could have no
                    // local owner slot. Terminal drain only returns an exact
                    // RX buffer or moves an existing terminal residue.
                    if (!fail_closed_draining
                        && retry_capacity_available
                        && !fresh_nomad_turn_reserved
                        && !lxmf_proof_admission_pending
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
                    if let NodeInterfaceSupervisorTransition::Data(DataRouterStep::Routed(hop)) =
                        transition
                    {
                        if let Some((submission, destination)) =
                            storage.routed_submission_context(hop.prepared())
                        {
                            let hops = supervisor.retained_path_hops(&destination).unwrap_or(0);
                            let resolution = match hop.prepared().target() {
                                TxTarget::Only(_) => RadioTraceRouteResolution::ExactRetainedPath,
                                TxTarget::All | TxTarget::AllExcept(_) | TxTarget::Selected(_) => {
                                    RadioTraceRouteResolution::BroadcastFallback
                                }
                            };
                            radio_diagnostics.record_route_selected(
                                dispatch_completed_at.as_micros(),
                                RadioTraceRouteSelected::new(
                                    submission.get(),
                                    *destination.as_bytes(),
                                    supervisor.retained_path_next_hop(&destination),
                                    hops,
                                    hop.interface().get(),
                                    resolution,
                                    hop.prepared().packet_len(),
                                    *hop.prepared().encoded_packet_sha256().as_bytes(),
                                    *hop.prepared().attempt().as_bytes(),
                                ),
                            );
                        }
                        match reticulum_probe.destination_for_prepared(hop.prepared()) {
                            Ok(Some(_)) => {
                                let dispatched_at =
                                    MonotonicMillis::new(dispatch_completed_at.as_micros() / 1_000);
                                match reticulum_probe
                                    .observe_routed(hop.prepared(), dispatched_at.get())
                                {
                                    Ok(true) => info!(
                                        "e290-node stage=reticulum-proof-probe status=DISPATCHED interface={} route_hops=preparation-snapshot rtt_clock=first-router-dispatch-acceptance",
                                        hop.interface().get(),
                                    ),
                                    Ok(false) => {
                                        error!(
                                            "e290-node stage=reticulum-proof-probe status=FAIL reason=exact-routed-owner-became-unrelated action=fail-closed-drain"
                                        );
                                        fail_closed_draining = true;
                                    }
                                    Err(reason) => {
                                        error!(
                                            "e290-node stage=reticulum-proof-probe status=FAIL reason={reason:?} action=fail-closed-drain"
                                        );
                                        fail_closed_draining = true;
                                    }
                                }
                                progressed = true;
                            }
                            Ok(None) => {}
                            Err(reason) => {
                                error!(
                                    "e290-node stage=reticulum-proof-probe status=FAIL reason={reason:?} source=data-router-routed action=fail-closed-drain"
                                );
                                fail_closed_draining = true;
                                progressed = true;
                            }
                        }
                    }
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
                        OrdinaryRouterStep::PacketStaged { token, .. },
                    ) = transition
                        && let Some(OrdinaryProtocolDispatch::Submission(
                            SubmissionProtocolDispatch::Path {
                                offer,
                                token: pending_token,
                            },
                        )) = pending_protocol_dispatch
                    {
                        match config::exact_packet_correlation(pending_token, token) {
                            config::ExactPacketCorrelation::Capture => {
                                pending_protocol_dispatch =
                                    Some(OrdinaryProtocolDispatch::Submission(
                                        SubmissionProtocolDispatch::Path {
                                            offer,
                                            token: Some(token),
                                        },
                                    ));
                                info!(
                                    "e290-node stage=path-discovery status=STAGED submission={} ordinal={} slot={} generation={} response_clock=not-started",
                                    offer.id().get(),
                                    offer.ordinal(),
                                    token.slot_id().get(),
                                    token.generation().get(),
                                );
                            }
                            config::ExactPacketCorrelation::Exact => {
                                error!(
                                    "e290-node stage=path-discovery status=FAIL reason=duplicate-exact-stage submission={} ordinal={} slot={} generation={} action=disable-submissions",
                                    offer.id().get(),
                                    offer.ordinal(),
                                    token.slot_id().get(),
                                    token.generation().get(),
                                );
                                disable_submission_for_path_fault(
                                    storage,
                                    &mut durability_service,
                                    &retained_frame,
                                    &pending_frame_acknowledgement,
                                    supervisor,
                                    lora_descriptor,
                                    &mut fail_closed_draining,
                                    "path-request-stage-correlation",
                                );
                            }
                            config::ExactPacketCorrelation::Unrelated => {
                                // Active ordinary packet generations may overlap. The
                                // path owner remains pending until its concrete actor
                                // reports complete transmission, so a different token
                                // belongs to independent ordinary work.
                            }
                        }
                        progressed = true;
                    }
                    if let NodeInterfaceSupervisorTransition::Ordinary(
                        OrdinaryRouterStep::Routed {
                            token: dispatch_token,
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
                                    SubmissionProtocolDispatch::Path {
                                        offer,
                                        token: expected_token,
                                    },
                                ) => {
                                    match config::retained_packet_correlation(
                                        expected_token,
                                        dispatch_token,
                                    ) {
                                        config::RetainedPacketCorrelation::Exact
                                            if protocol_token.is_some() =>
                                        {
                                            error!(
                                                "e290-node stage=path-discovery status=FAIL reason=exact-dispatch-has-protocol-token first_dispatch={first_dispatch} interface={} slot={} generation={} action=disable-submissions",
                                                interface.get(),
                                                dispatch_token.slot_id().get(),
                                                dispatch_token.generation().get(),
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
                                        }
                                        config::RetainedPacketCorrelation::Exact => {
                                            info!(
                                                "e290-node stage=path-discovery status=ACTOR-QUEUED submission={} ordinal={} interface={} first_dispatch={first_dispatch} slot={} generation={} response_clock=not-started action=await-interface-completion",
                                                offer.id().get(),
                                                offer.ordinal(),
                                                interface.get(),
                                                dispatch_token.slot_id().get(),
                                                dispatch_token.generation().get(),
                                            );
                                        }
                                        config::RetainedPacketCorrelation::Unrelated => {
                                            // The transition belongs to another active
                                            // ordinary packet generation. Its protocol
                                            // token, when present, was confirmed above.
                                        }
                                    }
                                    progressed = true;
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
                        OrdinaryRouterStep::Completion(completion),
                    ) = transition
                    {
                        let (route_returned, completed) = match completion {
                            OrdinaryRouterCompletionProgress::Next { completed, .. } => {
                                (false, completed)
                            }
                            OrdinaryRouterCompletionProgress::Returned { completed, .. } => {
                                (true, completed)
                            }
                        };
                        if let Some(OrdinaryProtocolDispatch::Submission(
                            SubmissionProtocolDispatch::Path {
                                offer,
                                token: expected_token,
                            },
                        )) = pending_protocol_dispatch
                        {
                            let completed_at =
                                MonotonicMillis::new(dispatch_completed_at.as_micros() / 1_000);
                            let correlation = config::retained_packet_correlation(
                                expected_token,
                                completed.token(),
                            );
                            if correlation == config::RetainedPacketCorrelation::Unrelated {
                                // Another ordinary packet completed while the exact
                                // path request is waiting to stage or remains owned by
                                // its interface actor.
                            } else if completed.transmission()
                                == OrdinaryTransmissionOutcome::Transmitted
                            {
                                pending_protocol_dispatch = None;
                                match storage
                                    .acknowledge_path_request_dispatched(offer, completed_at)
                                {
                                    Ok(()) => info!(
                                        "e290-node stage=path-discovery status=TRANSMITTED submission={} ordinal={} interface={} slot={} generation={} completed_ms={} response_clock=started",
                                        offer.id().get(),
                                        offer.ordinal(),
                                        completed.interface().get(),
                                        completed.token().slot_id().get(),
                                        completed.token().generation().get(),
                                        completed_at.get(),
                                    ),
                                    Err(reason) => {
                                        error!(
                                            "e290-node stage=path-discovery status=FAIL reason=transmission-ack:{reason:?} submission={} ordinal={} action=disable-submissions",
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
                                            "path-request-transmission-ack",
                                        );
                                    }
                                }
                            } else if route_returned {
                                pending_protocol_dispatch = None;
                                let retry_not_before = MonotonicMillis::new(
                                    completed_at
                                        .get()
                                        .saturating_add(config::STORAGE_RETRY_BACKOFF_MS),
                                );
                                match storage.retry_path_request_dispatch(offer, retry_not_before) {
                                    Ok(()) => warn!(
                                        "e290-node stage=path-discovery status=NOT-TRANSMITTED submission={} ordinal={} interface={} slot={} generation={} response_clock=not-started retry_not_before_ms={}",
                                        offer.id().get(),
                                        offer.ordinal(),
                                        completed.interface().get(),
                                        completed.token().slot_id().get(),
                                        completed.token().generation().get(),
                                        retry_not_before.get(),
                                    ),
                                    Err(reason) => {
                                        error!(
                                            "e290-node stage=path-discovery status=FAIL reason=negative-ack:{reason:?} submission={} ordinal={} action=disable-submissions",
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
                                            "path-request-negative-ack",
                                        );
                                    }
                                }
                            } else {
                                info!(
                                    "e290-node stage=path-discovery status=HOP-NOT-TRANSMITTED submission={} ordinal={} interface={} slot={} generation={} response_clock=not-started action=await-next-interface",
                                    offer.id().get(),
                                    offer.ordinal(),
                                    completed.interface().get(),
                                    completed.token().slot_id().get(),
                                    completed.token().generation().get(),
                                );
                            }
                            progressed = true;
                        }
                    }
                    if let NodeInterfaceSupervisorTransition::Ordinary(
                        OrdinaryRouterStep::Completion(
                            OrdinaryRouterCompletionProgress::Returned { slot, .. },
                        ),
                    ) = transition
                        && !matches!(
                            pending_protocol_dispatch,
                            Some(OrdinaryProtocolDispatch::Submission(
                                SubmissionProtocolDispatch::Path { .. }
                            ))
                        )
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
                        && !matches!(
                            pending_protocol_dispatch,
                            Some(OrdinaryProtocolDispatch::Submission(
                                SubmissionProtocolDispatch::Path { .. }
                            ))
                        )
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
                    let input_fault_owns_pending_protocol = !matches!(
                        pending_protocol_dispatch,
                        Some(OrdinaryProtocolDispatch::Submission(
                            SubmissionProtocolDispatch::Path {
                                token: staged_token,
                                ..
                            }
                        )) if config::pending_path_protocol_disposition(staged_token)
                            == config::PendingPathProtocolDisposition::RetainInFlight
                    );
                    if input_fault_terminated_protocol
                        && input_fault_owns_pending_protocol
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
                    if !fail_closed_draining && !lxmf_proof_admission_pending {
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
                                        if rmap_retry_pending {
                                            rmap.mark_queue_accepted(now_seconds());
                                            rmap_retry_pending = false;
                                            info!(
                                                "e290-node stage=rmap-discovery-announce status=QUEUED source=retained-retry next_after_seconds={}",
                                                RMAP_DISCOVERY_INTERVAL_SECONDS,
                                            );
                                        }
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
                                // Receipt, path, and Link clocks must receive a
                                // fair bounded maintenance turn even while an
                                // earlier protocol dispatch or ingress action
                                // remains queued. This lane has an empty retry
                                // slot, so any generated actions that cannot
                                // enter the ordinary coordinator are retained
                                // behind that earlier owner instead of lost.
                                if now >= next_tick_seconds {
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
                    let announce_lane_ready = retry_actions_a.is_none()
                        && retry_actions_b.is_none()
                        && quarantined_actions.is_none()
                        && pending_protocol_dispatch.is_none()
                        && !supervisor.ingress_actions_pending()
                        && !fail_closed_draining
                        && !lxmf_proof_admission_pending
                        && !fresh_nomad_turn_reserved
                        && !announce_clock.is_exhausted();
                    let manual_scheduled = manual_announce_schedule.due(now);
                    let initial_rmap_interface_ready = rmap_publication_policy
                        .initial_interface()
                        .is_none_or(|interface| {
                            supervisor
                                .interface_registry()
                                .descriptor(interface)
                                .is_some_and(|descriptor| descriptor.is_online())
                        });
                    rmap.observe_publication_gate(initial_rmap_interface_ready);
                    let rmap_due = manual_scheduled.is_none()
                        && rmap_destination.is_some()
                        && rmap
                            .due_app_data_for_policy(
                                now,
                                rmap_publication_policy,
                                initial_rmap_interface_ready,
                            )
                            .is_some();
                    let scheduled = manual_scheduled.or_else(|| {
                        if rmap_due || !automatic_announces_enabled {
                            None
                        } else {
                            announce_schedule.due(now)
                        }
                    });
                    if announce_lane_ready && let Some(scheduled) = scheduled {
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
                                Some(lxmf_delivery_announce_app_data),
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

                        let mut nomad_emission = None;
                        if scheduled == ScheduledAnnounce::NomadNode {
                            let emitted_at = announce_clock
                                .next_emission()
                                .expect("a non-exhausted announce clock has an emission");
                            match supervisor.queue_announce_for(
                                &nomad_destination,
                                Some(NOMAD_NODE_ANNOUNCE_APP_DATA.as_bytes()),
                                emitted_at,
                                &mut rng,
                            ) {
                                Ok(()) => {
                                    announce_clock.mark_queued();
                                    nomad_emission = Some(emitted_at.get());
                                }
                                Err(reason) => {
                                    admission_deferred = true;
                                    warn!(
                                        "e290-node stage=announce destination=nomadnetwork.node status=DEFERRED reason={reason:?} retry_after_seconds={}",
                                        config::ANNOUNCE_ADMISSION_RETRY_SECONDS,
                                    );
                                }
                            }
                        }

                        if admission_deferred {
                            if manual_scheduled.is_some() {
                                manual_announce_schedule.defer_attempt(now);
                            } else {
                                announce_schedule.defer_attempt(now);
                            }
                        } else {
                            // A queued announce completed this event. An LXMF
                            // event whose service was cleanly disabled is also
                            // consumed without spending an announce ordinal.
                            if manual_scheduled.is_some() {
                                manual_announce_schedule.mark_attempted(now);
                            } else {
                                announce_schedule.mark_attempted(now);
                            }
                        }

                        if primary_emission.is_some()
                            || lxmf_emission.is_some()
                            || nomad_emission.is_some()
                        {
                            match supervisor.flush_announces(
                                MonotonicSeconds::new(now),
                                config::ordinary_admission(now_millis()),
                                &mut rng,
                            ) {
                                NodeInterfaceAnnounceFlushResult::Accepted => {
                                    info!(
                                        "e290-node stage=announce status=QUEUED primary_emission={primary_emission:?} lxmf_delivery_emission={lxmf_emission:?} nomad_node_emission={nomad_emission:?} boot_epoch={}",
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
                    if announce_lane_ready
                        && scheduled.is_none()
                        && rmap_due
                        && let Some(destination) = rmap_destination
                    {
                        let emitted_at = announce_clock
                            .next_emission()
                            .expect("a non-exhausted announce clock has an emission");
                        let app_data = rmap
                            .due_app_data_for_policy(
                                now,
                                rmap_publication_policy,
                                initial_rmap_interface_ready,
                            )
                            .expect("the selected RMAP event remains due for this node turn");
                        match supervisor.queue_announce_for(
                            &destination,
                            Some(app_data.as_bytes()),
                            emitted_at,
                            &mut rng,
                        ) {
                            Ok(()) => {
                                announce_clock.mark_queued();
                                match supervisor.flush_announces(
                                    MonotonicSeconds::new(now),
                                    config::ordinary_admission(now_millis()),
                                    &mut rng,
                                ) {
                                    NodeInterfaceAnnounceFlushResult::Accepted => {
                                        rmap.mark_queue_accepted(now);
                                        info!(
                                            "e290-node stage=rmap-discovery-announce status=QUEUED emission={} boot_epoch={} next_after_seconds={}",
                                            emitted_at.get(),
                                            announce_clock.epoch().get(),
                                            RMAP_DISCOVERY_INTERVAL_SECONDS,
                                        );
                                        progressed = true;
                                    }
                                    NodeInterfaceAnnounceFlushResult::ActionOfferRejected(
                                        failure,
                                    ) => {
                                        rmap.mark_ordinary_admission_deferred(now);
                                        rmap_retry_pending = true;
                                        match handle_action_offer_failure(
                                            failure,
                                            "rmap-discovery-announce",
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
                            Err(reason) => {
                                let deferred_reason = match reason {
                                    AnnounceAdmissionError::AppDataTooLarge { .. } => {
                                        RmapDeferredReason::AnnouncePayloadTooLarge
                                    }
                                    AnnounceAdmissionError::QueueFull { .. } => {
                                        RmapDeferredReason::AnnounceQueueFull
                                    }
                                    AnnounceAdmissionError::NativeRejected => {
                                        RmapDeferredReason::AnnounceConstructionRejected
                                    }
                                };
                                rmap.defer_announce(now, deferred_reason);
                                warn!(
                                    "e290-node stage=rmap-discovery-announce status=DEFERRED reason={reason:?} retry_after_seconds={}",
                                    RMAP_DISCOVERY_RETRY_SECONDS,
                                );
                            }
                        }
                    }
                }
                4 => {
                    let manual_announce_was_pending = manual_announce_schedule.is_pending();
                    let authenticated_api_progressed = step_authenticated_api(
                        storage,
                        supervisor,
                        radio_diagnostics,
                        route_diagnostics,
                        discovered_peers,
                        peer_discovery_incarnation,
                        now_millis(),
                        &mut manual_announce_schedule,
                        now_seconds(),
                        nomad,
                        nomad_api,
                        reticulum_probe,
                        !fail_closed_draining,
                        identity_summary,
                        &mut authenticated_api,
                        &mut authenticated_api_state,
                    );
                    progressed |= authenticated_api_progressed;
                    if authenticated_api_progressed
                        && !manual_announce_was_pending
                        && manual_announce_schedule.is_pending()
                        && rmap.request_manual_publication(now_seconds())
                    {
                        info!(
                            "e290-node stage=rmap-discovery-announce status=REQUESTED source=manual-service-announce"
                        );
                    }
                }
                5 => {
                    let fresh_command_pending = nomad.fresh_command_pending();
                    let ordinary_owners_quiescent = retry_actions_a.is_none()
                        && retry_actions_b.is_none()
                        && quarantined_actions.is_none()
                        && pending_protocol_dispatch.is_none()
                        && !supervisor.ingress_actions_pending()
                        && ordinary_router_is_idle(supervisor)
                        && !lxmf_proof_admission_pending
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
                6 => {
                    let ordinary_owners_quiescent = retry_actions_a.is_none()
                        && retry_actions_b.is_none()
                        && quarantined_actions.is_none()
                        && pending_protocol_dispatch.is_none()
                        && !supervisor.ingress_actions_pending()
                        && ordinary_router_is_idle(supervisor)
                        && !lxmf_proof_admission_pending
                        && !fail_closed_draining;
                    progressed |= drive_one_reticulum_probe(
                        supervisor,
                        reticulum_probe,
                        &mut retry_actions_a,
                        &mut retry_actions_b,
                        &mut fail_closed_draining,
                        ordinary_owners_quiescent,
                        &mut rng,
                        now_millis(),
                    );
                }
                _ => {
                    let owner_now = now_millis();
                    let ordinary_owners_quiescent = retry_actions_a.is_none()
                        && retry_actions_b.is_none()
                        && quarantined_actions.is_none()
                        && pending_protocol_dispatch.is_none()
                        && !supervisor.ingress_actions_pending()
                        && ordinary_router_is_idle(supervisor)
                        && !lxmf_proof_admission_pending;
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
                        let submission_step = storage.drive_submission_step(
                            supervisor,
                            MonotonicSeconds::new(now_seconds()),
                            MonotonicMillis::new(owner_now),
                            deadline,
                            &mut rng,
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
                                                        SubmissionProtocolDispatch::path(offer),
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
                                                        SubmissionProtocolDispatch::path(offer),
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
                            ProductSubmissionDrive::Runtime(Ok(RuntimeStep::Terminal {
                                terminal,
                                progress,
                            })) => {
                                durability_service = durability_service.runtime_progress();
                                let outcome = match terminal.outcome() {
                                    AttemptOutcome::Delivered => {
                                        RadioTraceAttemptOutcome::Delivered
                                    }
                                    AttemptOutcome::DeliveryTimeout => {
                                        RadioTraceAttemptOutcome::DeliveryTimeout
                                    }
                                    AttemptOutcome::Unsent(_) => RadioTraceAttemptOutcome::Unsent,
                                };
                                let proof_ingress = terminal.ingress().map(|ingress| {
                                    RadioTraceProofIngress::new(
                                        ingress.interface().get(),
                                        ingress
                                            .signal()
                                            .map(|signal| (signal.rssi_dbm(), signal.snr_db())),
                                    )
                                });
                                radio_diagnostics.record_attempt_terminal(
                                    now_micros(),
                                    RadioTraceAttemptTerminal::new(
                                        *terminal.token().as_bytes(),
                                        outcome,
                                        proof_ingress,
                                    ),
                                );
                                info!(
                                    "e290-node stage=durable-submission status=PROGRESS step=Terminal {{ terminal: {terminal:?}, progress: {progress:?} }}"
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
                        debug!(
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
            if !fail_closed_draining {
                progressed |= drive_one_lxmf_proof(
                    supervisor,
                    &mut delayed_proofs,
                    lxmf_proof_holder,
                    &mut lxmf_proof_trace_evidence,
                    radio_diagnostics,
                    lora_interface,
                    &mut retry_actions_a,
                    &mut retry_actions_b,
                    pending_protocol_dispatch.is_some(),
                    &mut fail_closed_draining,
                );
            }
            let lxmf_proof_backpressured = should_relieve_lxmf_retry(
                application_event_capacity_backpressured,
                fail_closed_draining,
                lxmf_proof_holder.is_occupied(),
                delayed_proofs.capacities().ready,
            );
            // A responder request can produce one ordinary response envelope.
            // Leave it in the FIFO while both generic retry slots are occupied
            // so successful preparation can always establish an exact owner
            // before the request event is acknowledged.
            if retry_actions_a.is_none() || retry_actions_b.is_none() {
                progressed |= drive_one_application_event(
                    &mut application_events,
                    &mut delayed_proofs,
                    lxmf_retries,
                    storage,
                    supervisor,
                    radio_diagnostics,
                    lora_interface,
                    nomad,
                    discovered_peers,
                    lxmf_destination,
                    nomad_destination,
                    proof_probe_destination,
                    &mut retry_actions_a,
                    &mut retry_actions_b,
                    &mut fail_closed_draining,
                    lxmf_authority_fault,
                    *lxmf_pending_owner_fault_observed,
                    lxmf_service_fault_observed,
                    lxmf_proof_backpressured,
                    &mut rng,
                    now_millis(),
                );
                if !fail_closed_draining {
                    progressed |= drive_one_lxmf_proof(
                        supervisor,
                        &mut delayed_proofs,
                        lxmf_proof_holder,
                        &mut lxmf_proof_trace_evidence,
                        radio_diagnostics,
                        lora_interface,
                        &mut retry_actions_a,
                        &mut retry_actions_b,
                        pending_protocol_dispatch.is_some(),
                        &mut fail_closed_draining,
                    );
                }
            }
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
            // Bound idle polling without holding any lane owner across sleep.
            Timer::after(Duration::from_millis(config::NODE_POLL_INTERVAL_MS)).await;
        }
    }
}
