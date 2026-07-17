use crate::{config, durability_policy::DurabilityServiceState};
use reticulum_device_api::{
    ApiErrorCode, ApiVersion, DestinationHash as ApiDestinationHash, DeviceRequest, DeviceResponse,
    DispatchContext, DispatchProvenance, IdempotencyKey, Permissions,
    PrincipalId as ApiPrincipalId, RequestEnvelope, RequestId,
    SubmissionFailure as ApiSubmissionFailure, SubmissionId as ApiSubmissionId, SubmissionState,
};
use reticulum_device_api_adapter::dispatch;
use reticulum_radio_tx_dispatch::{RadioTxDispatcherPhase, RadioTxDispatcherStep};
use reticulum_storage_actor::DriveError;
use reticulum_storage_model::{
    FinalDisposition, LifecycleState, PrincipalId, SubmissionFailure, SubmissionId,
};
use reticulum_submission_runtime::{FrameOfferProgress, RecoveryStep, RuntimeError, RuntimeStep};
use reticulum_tx_supervisor::NodeInterfaceSupervisorTransition;

use crate::live_admission_test_support::{LORA_INTERFACE, LiveNodeSystem, LiveSubmissionService};
use std::vec;

const OWNER: ApiPrincipalId = ApiPrincipalId([0x11; 16]);
const FOREIGN: ApiPrincipalId = ApiPrincipalId([0x22; 16]);

fn submit_request<'a>(
    request_id: u64,
    destination: [u8; 16],
    payload: &'a [u8],
    key: u8,
) -> RequestEnvelope<'a> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(request_id),
        request: DeviceRequest::SubmitRnsData {
            destination: ApiDestinationHash(destination),
            payload,
            idempotency_key: IdempotencyKey([key; 16]),
        },
    }
}

fn status_request(request_id: u64, id: ApiSubmissionId) -> RequestEnvelope<'static> {
    RequestEnvelope {
        version: ApiVersion::CURRENT,
        request_id: RequestId(request_id),
        request: DeviceRequest::SubmissionStatus { id },
    }
}

fn full_context(principal: ApiPrincipalId) -> DispatchContext {
    context(
        principal,
        Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA | Permissions::READ_SUBMISSION_STATUS,
    )
}

fn context(principal: ApiPrincipalId, permissions: Permissions) -> DispatchContext {
    DispatchContext::authenticated(
        principal,
        permissions,
        DispatchProvenance::new(principal.0, 7, 9, 1).unwrap(),
    )
}

fn accepted_id(response: DeviceResponse) -> ApiSubmissionId {
    match response {
        DeviceResponse::SubmitRnsDataAccepted(accepted) => accepted.id,
        other => panic!("submission was not accepted: {other:?}"),
    }
}

fn assert_error(response: DeviceResponse, expected: ApiErrorCode) {
    match response {
        DeviceResponse::Error(error) => assert_eq!(error.code, expected),
        other => panic!("expected {expected:?}, received {other:?}"),
    }
}

#[test]
fn authenticated_submission_crosses_lora_policy_and_scripted_radio_durability() {
    let (mut service, recovery) = LiveSubmissionService::formatted(7);
    assert_eq!(recovery, vec![RecoveryStep::Complete]);
    let mut system = LiveNodeSystem::new();
    let destination = *system.destination.as_bytes();
    let baseline_writes = service.write_attempts();

    let unauthenticated = dispatch(
        &mut service,
        &DispatchContext::UNAUTHENTICATED,
        submit_request(1, destination, b"live LoRa submission", 0x31),
    );
    assert_error(
        unauthenticated.response,
        ApiErrorCode::AuthenticationRequired,
    );
    assert_eq!(service.write_attempts(), baseline_writes);

    let denied = dispatch(
        &mut service,
        &context(OWNER, Permissions::READ_SUBMISSION_STATUS),
        submit_request(2, destination, b"live LoRa submission", 0x31),
    );
    assert_error(denied.response, ApiErrorCode::PermissionDenied);
    assert_eq!(service.write_attempts(), baseline_writes);

    let accepted = dispatch(
        &mut service,
        &full_context(OWNER),
        submit_request(3, destination, b"live LoRa submission", 0x31),
    );
    let api_id = accepted_id(accepted.response);
    assert_eq!(api_id, ApiSubmissionId(1));
    let id = SubmissionId::new(api_id.0);
    let owner = PrincipalId::new(OWNER.0);
    assert_eq!(service.state_for(owner, id), Some(LifecycleState::Queued));

    let after_accept_writes = service.write_attempts();
    let second_novel = dispatch(
        &mut service,
        &full_context(OWNER),
        submit_request(4, destination, b"second novel submission", 0x32),
    );
    assert_error(second_novel.response, ApiErrorCode::CapacityExhausted);
    assert_eq!(
        service.write_attempts(),
        after_accept_writes,
        "the one-acceptance product cap rejects before touching NOR"
    );

    let available_before = system.supervisor.data_parked_counts().available();
    assert_eq!(available_before, config::DATA_BUFFERS);
    assert!(matches!(
        service
            .drive_step(&mut system.supervisor, 1_000, 1_000, &mut system.node_rng)
            .expect("preparation barrier plans"),
        RuntimeStep::PreparationBarrier { id: observed, .. } if observed == id
    ));
    assert_eq!(
        system.supervisor.data_parked_counts().available(),
        available_before,
        "the node is untouched until Preparing is durable"
    );
    assert!(matches!(
        service
            .drive_step(&mut system.supervisor, 1_000, 1_000, &mut system.node_rng)
            .expect("preparation barrier persists"),
        RuntimeStep::Persistence(_)
    ));
    assert_eq!(
        system.supervisor.data_parked_counts().available(),
        available_before
    );
    assert!(matches!(
        service
            .drive_step(&mut system.supervisor, 1_000, 1_000, &mut system.node_rng)
            .expect("durable intent prepares through the node"),
        RuntimeStep::Preparation { id: observed, .. } if observed == id
    ));
    assert_eq!(
        system.supervisor.data_parked_counts().available(),
        available_before - 1
    );

    let report = system.transmit_data_to_durability_gate();
    let report_frame = report
        .authorized_frame()
        .expect("authorized DATA transmit exposes exact frame metadata");
    assert_eq!(report_frame.interface(), LORA_INTERFACE);
    assert_eq!(system.dispatcher.radio().tx_calls(), 1);
    assert_eq!(system.dispatcher.radio().captured().len(), 1);
    let handoff_frame = system.take_authorized_frame_request();
    assert_eq!(handoff_frame, report_frame);
    assert_eq!(
        system.dispatcher.phase(),
        RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
    );
    assert_eq!(
        system.supervisor.data_parked_counts().available(),
        available_before - 1,
        "the interface completion cannot escape before durability"
    );
    assert_eq!(system.supervisor.terminal_attempts().count(), 0);

    assert_eq!(
        service.offer_authorized_frame(handoff_frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert_eq!(service.pending_acknowledgements(), 0);
    assert_eq!(
        system.dispatcher.step(2_000_200),
        RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement
    );
    assert!(matches!(
        service
            .drive_step(&mut system.supervisor, 2_000, 2_000, &mut system.node_rng)
            .expect("exact frame observation persists"),
        RuntimeStep::Persistence(_)
    ));
    assert_eq!(
        service.offer_authorized_frame(handoff_frame),
        Ok(FrameOfferProgress::Durable)
    );
    assert!(matches!(
        service.state_for(owner, id),
        Some(LifecycleState::AwaitingDelivery(_))
    ));

    let awaiting = dispatch(
        &mut service,
        &full_context(OWNER),
        status_request(5, api_id),
    );
    assert!(matches!(
        awaiting.response,
        DeviceResponse::SubmissionStatus(status)
            if status.id == api_id
                && matches!(status.state, SubmissionState::AwaitingDelivery(_))
    ));

    system.acknowledge_authorized_frame(handoff_frame);
    let completion_transitions = system.drain_completion();
    assert!(completion_transitions.iter().any(|transition| matches!(
        transition,
        NodeInterfaceSupervisorTransition::CompletionAccepted { .. }
    )));
    assert_eq!(
        system.supervisor.data_parked_counts().available(),
        config::DATA_BUFFERS
    );

    system.force_delivery_timeout();
    for _ in 0..8 {
        let _ = service
            .drive_step(
                &mut system.supervisor,
                1_000_000,
                1_000_000_000,
                &mut system.node_rng,
            )
            .expect("timeout projection advances");
        if matches!(service.state_for(owner, id), Some(LifecycleState::Final(_)))
            && service.pending_acknowledgements() == 0
        {
            break;
        }
    }
    assert_eq!(
        service.state_for(owner, id),
        Some(LifecycleState::Final(FinalDisposition::Failed(
            SubmissionFailure::DeliveryTimeout
        )))
    );
    assert_eq!(service.pending_acknowledgements(), 0);

    let final_status = dispatch(
        &mut service,
        &full_context(OWNER),
        status_request(6, api_id),
    );
    assert!(matches!(
        final_status.response,
        DeviceResponse::SubmissionStatus(status)
            if status.state
                == SubmissionState::Failed(ApiSubmissionFailure::DeliveryTimeout)
    ));
    let foreign_status = dispatch(
        &mut service,
        &full_context(FOREIGN),
        status_request(7, api_id),
    );
    assert_error(foreign_status.response, ApiErrorCode::NotFound);

    let flash = service.into_flash();
    let (mut remounted, remount_recovery) =
        LiveSubmissionService::mount(flash, 8).expect("durable final state remounts");
    assert!(matches!(
        remount_recovery.as_slice(),
        [RecoveryStep::Submission { id: recovered, .. }, RecoveryStep::Complete]
            if *recovered == id
    ));
    let after_remount = dispatch(
        &mut remounted,
        &full_context(OWNER),
        status_request(8, api_id),
    );
    assert!(matches!(
        after_remount.response,
        DeviceResponse::SubmissionStatus(status)
            if status.state
                == SubmissionState::Failed(ApiSubmissionFailure::DeliveryTimeout)
    ));
}

#[test]
fn permanent_post_frame_storage_failure_fail_stops_lora_with_all_owners_retained() {
    let (mut service, _) = LiveSubmissionService::formatted(11);
    let mut system = LiveNodeSystem::new();
    let destination = *system.destination.as_bytes();
    let accepted = dispatch(
        &mut service,
        &full_context(OWNER),
        submit_request(20, destination, b"fail-stop frame", 0x51),
    );
    let api_id = accepted_id(accepted.response);
    let id = SubmissionId::new(api_id.0);
    let owner = PrincipalId::new(OWNER.0);

    for expected in ["barrier", "barrier persistence", "preparation"] {
        service
            .drive_step(&mut system.supervisor, 1_000, 1_000, &mut system.node_rng)
            .unwrap_or_else(|error| panic!("{expected} failed: {error:?}"));
    }
    let report = system.transmit_data_to_durability_gate();
    let frame = system.take_authorized_frame_request();
    assert_eq!(report.authorized_frame(), Some(frame));
    assert!(system.queue_ordinary_announce_behind_frame());
    assert_eq!(system.dispatcher.radio().tx_calls(), 1);

    assert_eq!(
        service.offer_authorized_frame(frame),
        Ok(FrameOfferProgress::Retain)
    );
    assert!(matches!(
        service.drive_binding_failure(&mut system.supervisor, &mut system.node_rng),
        Err(RuntimeError::Storage(DriveError::Binding(_)))
    ));
    assert_eq!(
        service.service_state(),
        DurabilityServiceState::ActiveOwnerFailStopped
    );
    system.fail_stop_lora();

    for now_us in [3_000_000, 4_000_000, 5_000_000] {
        assert_eq!(
            system.dispatcher.step(now_us),
            RadioTxDispatcherStep::NeedAuthorizedFrameAcknowledgement
        );
        let _ = system
            .supervisor
            .step(reticulum_node_core::MonotonicMillis::new(now_us / 1_000));
    }
    assert_eq!(
        system.dispatcher.phase(),
        RadioTxDispatcherPhase::AuthorizedFrameAcknowledgementWait
    );
    assert_eq!(system.dispatcher.radio().tx_calls(), 1);
    assert_eq!(system.dispatcher.radio().rx_calls(), 0);
    assert_eq!(system.dispatcher.radio().captured().len(), 1);
    assert_eq!(
        system.supervisor.data_parked_counts().available(),
        config::DATA_BUFFERS - 1,
        "no interface completion released the DATA packet owner"
    );
    assert_eq!(system.supervisor.terminal_attempts().count(), 0);
    assert_eq!(service.pending_acknowledgements(), 0);
    assert_eq!(
        service.state_for(owner, id),
        Some(LifecycleState::Preparing)
    );
}
