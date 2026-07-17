//! Authenticated device-API dispatch over a narrow durable-submission port.
//!
//! This adapter performs no framing, session establishment, allocation, radio
//! work, raw flash access, or journal construction. It authorizes trusted
//! session context, scopes status reads by principal, and passes one complete
//! owned acceptance candidate through [`SubmissionPort`] only after an
//! authorized mutation. The port retains all actor, journal, and backend
//! ownership.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use reticulum_device_api::{
    self as api, ApiErrorCode, ApiErrorResponse, ApiVersion, AuthorizationError,
    CapabilityAvailability, DeviceRequest, DeviceResponse, DispatchContext, EncodedPacketSha256,
    PreparedPacketDetails, RequestEnvelope, ResponseEnvelope, SubmissionFailure, SubmissionState,
    SubmissionStatus, authorize_request,
};
use reticulum_storage_model as storage;

/// Bounded semantic result of durable submission acceptance.
///
/// This vocabulary is owned by the API boundary. Port implementations map
/// storage-engine-specific progress into these outcomes without exposing an
/// actor, journal, backend, or storage-actor result type to dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionAcceptance {
    /// A new submission was durably accepted.
    Accepted(storage::SubmissionId),
    /// An identical principal-scoped idempotent submission already exists.
    Replay(storage::SubmissionId),
    /// The idempotency key already names different semantic content.
    IdempotencyConflict,
    /// No durable capacity is currently available for a new submission.
    CapacityExhausted,
    /// The durable submission identifier space is permanently exhausted.
    IdentifierExhausted,
}

/// Bounded failure vocabulary exposed by a durable-submission port.
///
/// Backend-specific diagnostics remain inside the port implementation. The
/// logical API deliberately exposes only stable client-actionable categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionPortError {
    /// The product profile or current runtime has disabled submission service.
    Unavailable,
    /// Another exact retained durable mutation currently owns the actor.
    Busy,
    /// A physical backend operation failed or returned ambiguously.
    Backend,
    /// The supplied physical storage binding did not match the mounted owner.
    Binding,
    /// The durable owner latched a semantic or physical invariant fault.
    Faulted,
}

/// Narrow target-safe semantic port required by device-API dispatch.
///
/// Implementations own all storage actors, operation-scoped journal views, and
/// physical backends. No such capability crosses this boundary. Every status
/// lookup is already scoped by the authenticated principal supplied by the
/// adapter.
pub trait SubmissionPort {
    /// Current product/runtime availability of durable submission service.
    fn availability(&mut self) -> CapabilityAvailability;

    /// Read the public state of one principal-owned submission.
    ///
    /// Missing and foreign identifiers must both return `Ok(None)`.
    fn submission_state(
        &mut self,
        principal: storage::PrincipalId,
        id: storage::SubmissionId,
    ) -> Result<Option<storage::LifecycleState>, SubmissionPortError>;

    /// Durably accept or idempotently replay one complete owned candidate.
    fn accept(
        &mut self,
        candidate: storage::AcceptanceCandidate,
    ) -> Result<SubmissionAcceptance, SubmissionPortError>;
}

/// Authorize and dispatch one decoded logical request against a narrow
/// durable-submission port.
///
/// The response always uses the current device API version and echoes the
/// caller's request identifier. Authentication facts come only from
/// `context`; request CBOR cannot supply or replace them. Although the wire
/// decoder rejects incompatible majors, this boundary repeats that check so a
/// manually constructed envelope cannot bypass version policy.
pub fn dispatch<P>(
    port: &mut P,
    context: DispatchContext,
    envelope: RequestEnvelope<'_>,
) -> ResponseEnvelope
where
    P: SubmissionPort,
{
    let request_id = envelope.request_id;
    let request = envelope.request;
    let operation = request.operation();
    let response = if envelope.version.major != ApiVersion::CURRENT.major {
        api_error(ApiErrorCode::UnsupportedVersion, operation)
    } else {
        match authorize_request(context, &request) {
            Ok(()) => dispatch_authorized(port, context, request, operation),
            Err(error) => authorization_error(error, operation),
        }
    };
    ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id,
        response,
    }
}

fn dispatch_authorized<P>(
    port: &mut P,
    context: DispatchContext,
    request: DeviceRequest<'_>,
    operation: u16,
) -> DeviceResponse
where
    P: SubmissionPort,
{
    match request {
        DeviceRequest::SystemCapabilities => {
            let available = cfg!(feature = "experimental-rns-data")
                && port.availability() == CapabilityAvailability::Available;
            DeviceResponse::SystemCapabilities(api::CapabilitySnapshot::for_dispatch(available))
        }
        DeviceRequest::SubmissionStatus { id } => {
            let Some(principal) = context.principal else {
                return api_error(ApiErrorCode::AuthenticationRequired, operation);
            };
            if port.availability() != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
            let principal = storage::PrincipalId::new(principal.0);
            let id = storage::SubmissionId::new(id.0);
            match port.submission_state(principal, id) {
                Ok(Some(state)) => DeviceResponse::SubmissionStatus(SubmissionStatus {
                    id: api_submission_id(id),
                    state: api_submission_state(state),
                }),
                Ok(None) => api_error(ApiErrorCode::NotFound, operation),
                Err(error) => port_error(error, operation),
            }
        }
        #[cfg(feature = "experimental-rns-data")]
        DeviceRequest::SubmitRnsData {
            destination,
            payload,
            idempotency_key,
        } => {
            let Some(principal) = context.principal else {
                return api_error(ApiErrorCode::AuthenticationRequired, operation);
            };
            if port.availability() != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
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
            acceptance_response(port.accept(candidate), operation)
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

#[cfg(feature = "experimental-rns-data")]
fn acceptance_response(
    progress: Result<SubmissionAcceptance, SubmissionPortError>,
    operation: u16,
) -> DeviceResponse {
    match progress {
        Ok(SubmissionAcceptance::Accepted(id) | SubmissionAcceptance::Replay(id)) => {
            DeviceResponse::SubmitRnsDataAccepted(api::SubmissionAccepted {
                id: api_submission_id(id),
            })
        }
        Ok(SubmissionAcceptance::IdempotencyConflict) => {
            api_error(ApiErrorCode::IdempotencyConflict, operation)
        }
        Ok(SubmissionAcceptance::CapacityExhausted) => {
            api_error(ApiErrorCode::CapacityExhausted, operation)
        }
        Ok(SubmissionAcceptance::IdentifierExhausted) => {
            api_error(ApiErrorCode::Internal, operation)
        }
        Err(error) => port_error(error, operation),
    }
}

fn port_error(error: SubmissionPortError, operation: u16) -> DeviceResponse {
    let code = match error {
        SubmissionPortError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        SubmissionPortError::Busy
        | SubmissionPortError::Backend
        | SubmissionPortError::Binding
        | SubmissionPortError::Faulted => ApiErrorCode::Internal,
    };
    api_error(code, operation)
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
