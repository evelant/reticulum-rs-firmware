//! Authenticated device-API dispatch over the sole durable storage actor.
//!
//! This adapter performs no framing, session establishment, allocation, radio
//! work, or direct flash access. It authorizes trusted session context, scopes
//! status reads by principal, and publishes a mutating acceptance only after
//! [`reticulum_storage_actor::StorageActor`] reports the exact intent durable.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(all(feature = "host-sim", target_os = "none"))]
compile_error!("the experimental `host-sim` device API adapter is unavailable on target_os=none");

use embedded_storage::nor_flash::MultiwriteNorFlash;
use reticulum_device_api::{
    self as api, ApiErrorCode, ApiErrorResponse, ApiVersion, AuthorizationError, DeviceRequest,
    DeviceResponse, DispatchContext, EncodedPacketSha256, PreparedPacketDetails, RequestEnvelope,
    ResponseEnvelope, SubmissionFailure, SubmissionState, SubmissionStatus, authorize_request,
};
use reticulum_storage_actor::StorageActor;
#[cfg(feature = "host-sim")]
use reticulum_storage_actor::{AcceptanceProgress, DriveError};
use reticulum_storage_model as storage;

/// Authorize and dispatch one decoded logical request against the mounted sole
/// storage actor.
///
/// The response always uses the current device API version and echoes the
/// caller's request identifier. Authentication facts come only from
/// `context`; request CBOR cannot supply or replace them. Although the wire
/// decoder rejects incompatible majors, this boundary repeats that check so a
/// manually constructed envelope cannot bypass version policy.
pub fn dispatch<F, const SUBMISSIONS: usize, const PROJECTED: usize>(
    actor: &mut StorageActor<F, SUBMISSIONS, PROJECTED>,
    context: DispatchContext,
    envelope: RequestEnvelope<'_>,
) -> ResponseEnvelope
where
    F: MultiwriteNorFlash,
{
    let request_id = envelope.request_id;
    let request = envelope.request;
    let operation = request.operation();
    let response = if envelope.version.major != ApiVersion::CURRENT.major {
        api_error(ApiErrorCode::UnsupportedVersion, operation)
    } else {
        match authorize_request(context, &request) {
            Ok(()) => dispatch_authorized(actor, context, request, operation),
            Err(error) => authorization_error(error, operation),
        }
    };
    ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id,
        response,
    }
}

fn dispatch_authorized<F, const SUBMISSIONS: usize, const PROJECTED: usize>(
    actor: &mut StorageActor<F, SUBMISSIONS, PROJECTED>,
    context: DispatchContext,
    request: DeviceRequest<'_>,
    operation: u16,
) -> DeviceResponse
where
    F: MultiwriteNorFlash,
{
    match request {
        DeviceRequest::SystemCapabilities => DeviceResponse::SystemCapabilities(
            api::CapabilitySnapshot::for_dispatch(cfg!(feature = "host-sim")),
        ),
        DeviceRequest::SubmissionStatus { id } => {
            let Some(principal) = context.principal else {
                return api_error(ApiErrorCode::AuthenticationRequired, operation);
            };
            if actor.fault().is_some() || actor.pending_kind().is_some() {
                return api_error(ApiErrorCode::Internal, operation);
            }
            let principal = storage::PrincipalId::new(principal.0);
            let id = storage::SubmissionId::new(id.0);
            match actor.index().get_owned_state(principal, id) {
                Some(state) => DeviceResponse::SubmissionStatus(SubmissionStatus {
                    id: api_submission_id(id),
                    state: api_submission_state(state),
                }),
                None => api_error(ApiErrorCode::NotFound, operation),
            }
        }
        #[cfg(feature = "host-sim")]
        DeviceRequest::PrepareRnsData {
            destination,
            payload,
            idempotency_key,
        } => {
            let Some(principal) = context.principal else {
                return api_error(ApiErrorCode::AuthenticationRequired, operation);
            };
            let intent = match storage::ExperimentalRnsDataIntent::new(
                storage::DestinationHash::new(destination.0),
                payload,
            ) {
                Ok(intent) => intent,
                Err(_) => return api_error(ApiErrorCode::InvalidRequest, operation),
            };
            let candidate = storage::AcceptanceCandidate::new(
                storage::PrincipalId::new(principal.0),
                storage::IdempotencyKey::new(idempotency_key.0),
                intent,
            );
            acceptance_response(actor.accept(candidate), operation)
        }
        _ => api_error(ApiErrorCode::UnsupportedOperation, operation),
    }
}

fn authorization_error(error: AuthorizationError, operation: u16) -> DeviceResponse {
    let code = match error {
        AuthorizationError::AuthenticationRequired => ApiErrorCode::AuthenticationRequired,
        AuthorizationError::PermissionDenied(_) => ApiErrorCode::PermissionDenied,
    };
    api_error(code, operation)
}

fn api_error(code: ApiErrorCode, operation: u16) -> DeviceResponse {
    DeviceResponse::Error(ApiErrorResponse {
        code,
        operation: Some(operation),
    })
}

#[cfg(feature = "host-sim")]
fn acceptance_response<E>(
    progress: Result<AcceptanceProgress, DriveError<E>>,
    operation: u16,
) -> DeviceResponse {
    match progress {
        Ok(AcceptanceProgress::Accepted(id) | AcceptanceProgress::Replay(id)) => {
            DeviceResponse::PrepareRnsDataAccepted(api::SubmissionAccepted {
                id: api_submission_id(id),
            })
        }
        Ok(AcceptanceProgress::IdempotencyConflict { .. }) => {
            api_error(ApiErrorCode::IdempotencyConflict, operation)
        }
        Ok(AcceptanceProgress::IndexExhausted | AcceptanceProgress::JournalCapacityExhausted) => {
            api_error(ApiErrorCode::CapacityExhausted, operation)
        }
        Ok(AcceptanceProgress::IdentifierExhausted)
        | Err(DriveError::Backend(_))
        | Err(DriveError::Busy { .. })
        | Err(DriveError::Faulted(_)) => api_error(ApiErrorCode::Internal, operation),
    }
}

fn api_submission_id(id: storage::SubmissionId) -> api::SubmissionId {
    api::SubmissionId(id.get())
}

fn api_submission_state(state: storage::LifecycleState) -> SubmissionState {
    match state {
        storage::LifecycleState::Queued => SubmissionState::Queued,
        storage::LifecycleState::Preparing => SubmissionState::Preparing,
        storage::LifecycleState::AwaitingDelivery(details) => {
            SubmissionState::AwaitingDelivery(api_prepared_details(details))
        }
        storage::LifecycleState::Final(storage::FinalDisposition::Delivered(details)) => {
            SubmissionState::Delivered(api_prepared_details(details))
        }
        storage::LifecycleState::Final(storage::FinalDisposition::Failed(failure)) => {
            SubmissionState::Failed(match failure {
                storage::SubmissionFailure::NoPath => SubmissionFailure::NoPath,
                storage::SubmissionFailure::DeliveryTimeout => SubmissionFailure::DeliveryTimeout,
                storage::SubmissionFailure::Rejected => SubmissionFailure::Rejected,
                storage::SubmissionFailure::Internal(_) => SubmissionFailure::Internal,
            })
        }
        storage::LifecycleState::Final(storage::FinalDisposition::Cancelled) => {
            SubmissionState::Cancelled
        }
    }
}

fn api_prepared_details(details: storage::PreparedPacketDetails) -> PreparedPacketDetails {
    PreparedPacketDetails {
        packet_len: details.packet_len(),
        encoded_packet_sha256: EncodedPacketSha256::new(
            *details.encoded_packet_sha256().as_bytes(),
        ),
    }
}

#[cfg(test)]
mod tests;
