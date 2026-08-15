//! Node-side authenticated device-API dispatch without bearer capabilities.
//!
//! The bearer authenticates one record and transfers its exact opaque grant and
//! bounded logical message through `reticulum-device-api-handoff`. This module
//! revalidates that grant against the current immutable device-owned authority,
//! invokes the logical adapter only inside the authority lease callback, and
//! returns one reply with the unchanged routing key. It has no USB, executor,
//! radio, raw-flash, or session-key access.

use reticulum_device_api::{
    ApiErrorCode, ApiErrorResponse, ApiVersion, DeviceResponse, IdentitySummary, ResponseEnvelope,
    decode_request, encode_response,
};
use reticulum_device_api_adapter::{
    LxmfComposePort, LxmfInboxPort, ManualServiceAnnouncePort, NetworkConfigPort,
    NodeDiagnosticsPort, NomadFetchPort, PeerDiscoveryPort, ReticulumProbePort, SubmissionPort,
    dispatch, dispatch_with_lxmf_peer_discovery_and_nomad,
    dispatch_with_lxmf_peer_discovery_nomad_and_network_config,
    dispatch_with_lxmf_peer_discovery_nomad_network_config_and_probe,
};
use reticulum_device_api_credentials::CredentialAuthority;
use reticulum_device_api_handoff::{
    LocalApiReply, LocalApiRequest, MESSAGE_CAPACITY, MessageLength, OwnedMessage,
};
use reticulum_device_api_session::AuthenticatedGrant;

/// Terminal reason an authenticated request could not yield a canonical reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticatedApiDispatchFailureKind {
    /// The authenticated payload was not one canonical logical API request.
    MalformedRequest,
    /// A bounded logical response unexpectedly failed canonical encoding.
    ResponseEncoding,
}

/// Terminal dispatch failure retaining the exact request owner.
///
/// A response-encoding failure may occur after the port has already performed
/// its synchronous operation. The retained request is diagnostic/quarantine
/// ownership and must never be dispatched again.
#[must_use = "a terminal authenticated request must be quarantined or explicitly dropped"]
pub struct AuthenticatedApiDispatchFailure {
    kind: AuthenticatedApiDispatchFailureKind,
    request: LocalApiRequest<AuthenticatedGrant>,
}

impl AuthenticatedApiDispatchFailure {
    /// Stable terminal failure category.
    pub const fn kind(&self) -> AuthenticatedApiDispatchFailureKind {
        self.kind
    }

    /// Recover the complete request, opaque grant, and routing owner.
    pub fn into_request(self) -> LocalApiRequest<AuthenticatedGrant> {
        self.request
    }
}

/// Revalidate and synchronously dispatch one authenticated logical API request.
///
/// `identity` is a copy-only public node summary supplied independently of the
/// submission port; serving it cannot acquire storage or radio capabilities.
/// Missing, replaced, revoked, or generation-mismatched credential state emits
/// a generic `AuthenticationRequired` response without invoking `port`. It is
/// never retried through [`reticulum_device_api::DispatchContext::UNAUTHENTICATED`].
/// The bearer may treat that response as terminal for its live session.
#[allow(
    clippy::result_large_err,
    reason = "terminal failure must retain the exact allocation-free request owner"
)]
pub fn dispatch_authenticated_request<const CREDENTIALS: usize, P>(
    request: LocalApiRequest<AuthenticatedGrant>,
    authority: Option<&CredentialAuthority<CREDENTIALS>>,
    identity: IdentitySummary,
    port: &mut P,
) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
where
    P: SubmissionPort,
{
    let envelope = match decode_request(request.message().encoded()) {
        Ok(envelope) => envelope,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::MalformedRequest,
                request,
            });
        }
    };
    let operation = envelope.request.operation();
    let response = match authority.and_then(|authority| request.grant().revalidate(authority).ok())
    {
        Some(lease) => {
            lease.with_dispatch_context(|context| dispatch(port, identity, context, envelope))
        }
        None => ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: envelope.request_id,
            response: DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::AuthenticationRequired,
                operation: Some(operation),
            }),
        },
    };

    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = match encode_response(&response, &mut encoded) {
        Ok(length) => length,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::ResponseEncoding,
                request,
            });
        }
    };
    let key = request.key();
    drop(request);
    Ok(LocalApiReply::new(
        key,
        OwnedMessage::new(
            MessageLength::new(length).expect("device API and handoff share one capacity"),
            encoded,
        ),
    ))
}

/// Revalidate and dispatch one authenticated request through the complete
/// appliance and bounded NomadNet semantic ports.
///
/// The combined appliance port permits operation-scoped use of one physical
/// flash owner without creating simultaneous mutable aliases. The independent
/// Nomad port borrows only volatile protocol/API state. Authentication failure
/// invokes neither port.
#[allow(
    clippy::result_large_err,
    reason = "terminal failure must retain the exact allocation-free request owner"
)]
pub fn dispatch_authenticated_request_with_lxmf_and_nomad<const CREDENTIALS: usize, P, N>(
    request: LocalApiRequest<AuthenticatedGrant>,
    authority: Option<&CredentialAuthority<CREDENTIALS>>,
    identity: IdentitySummary,
    port: &mut P,
    nomad_port: &mut N,
) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
where
    P: SubmissionPort + LxmfInboxPort + LxmfComposePort + PeerDiscoveryPort,
    N: NomadFetchPort,
{
    let envelope = match decode_request(request.message().encoded()) {
        Ok(envelope) => envelope,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::MalformedRequest,
                request,
            });
        }
    };
    let operation = envelope.request.operation();
    let response = match authority.and_then(|authority| request.grant().revalidate(authority).ok())
    {
        Some(lease) => lease.with_dispatch_context(|context| {
            dispatch_with_lxmf_peer_discovery_and_nomad(
                port, nomad_port, identity, context, envelope,
            )
        }),
        None => ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: envelope.request_id,
            response: DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::AuthenticationRequired,
                operation: Some(operation),
            }),
        },
    };

    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = match encode_response(&response, &mut encoded) {
        Ok(length) => length,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::ResponseEncoding,
                request,
            });
        }
    };
    let key = request.key();
    drop(request);
    Ok(LocalApiReply::new(
        key,
        OwnedMessage::new(
            MessageLength::new(length).expect("device API and handoff share one capacity"),
            encoded,
        ),
    ))
}

/// Revalidate and dispatch the complete appliance request surface through one
/// flash-owning product port plus the independent volatile NomadNet port.
///
/// Network configuration deliberately joins the existing appliance port
/// instead of crossing this boundary as a second mutable alias. Authentication
/// failure invokes neither owner.
#[allow(
    clippy::result_large_err,
    reason = "terminal failure must retain the exact allocation-free request owner"
)]
pub fn dispatch_authenticated_request_with_lxmf_nomad_and_network_config<
    const CREDENTIALS: usize,
    P,
    N,
>(
    request: LocalApiRequest<AuthenticatedGrant>,
    authority: Option<&CredentialAuthority<CREDENTIALS>>,
    identity: IdentitySummary,
    port: &mut P,
    nomad_port: &mut N,
) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
where
    P: SubmissionPort
        + LxmfInboxPort
        + LxmfComposePort
        + PeerDiscoveryPort
        + NetworkConfigPort
        + ManualServiceAnnouncePort
        + NodeDiagnosticsPort,
    N: NomadFetchPort,
{
    let envelope = match decode_request(request.message().encoded()) {
        Ok(envelope) => envelope,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::MalformedRequest,
                request,
            });
        }
    };
    let operation = envelope.request.operation();
    let response = match authority.and_then(|authority| request.grant().revalidate(authority).ok())
    {
        Some(lease) => lease.with_dispatch_context(|context| {
            dispatch_with_lxmf_peer_discovery_nomad_and_network_config(
                port, nomad_port, identity, context, envelope,
            )
        }),
        None => ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: envelope.request_id,
            response: DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::AuthenticationRequired,
                operation: Some(operation),
            }),
        },
    };

    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = match encode_response(&response, &mut encoded) {
        Ok(length) => length,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::ResponseEncoding,
                request,
            });
        }
    };
    let key = request.key();
    drop(request);
    Ok(LocalApiReply::new(
        key,
        OwnedMessage::new(
            MessageLength::new(length).expect("device API and handoff share one capacity"),
            encoded,
        ),
    ))
}

/// Revalidate and dispatch the complete appliance request surface plus one
/// independent boot-scoped Reticulum proof-probe owner.
///
/// The probe owner is volatile and disjoint from the flash-owning product
/// façade. Authentication failure invokes neither owner.
#[allow(
    clippy::result_large_err,
    reason = "terminal failure must retain the exact allocation-free request owner"
)]
pub fn dispatch_authenticated_request_with_lxmf_nomad_network_config_and_probe<
    const CREDENTIALS: usize,
    P,
    N,
    Q,
>(
    request: LocalApiRequest<AuthenticatedGrant>,
    authority: Option<&CredentialAuthority<CREDENTIALS>>,
    identity: IdentitySummary,
    port: &mut P,
    nomad_port: &mut N,
    probe_port: &mut Q,
) -> Result<LocalApiReply, AuthenticatedApiDispatchFailure>
where
    P: SubmissionPort
        + LxmfInboxPort
        + LxmfComposePort
        + PeerDiscoveryPort
        + NetworkConfigPort
        + ManualServiceAnnouncePort
        + NodeDiagnosticsPort,
    N: NomadFetchPort,
    Q: ReticulumProbePort,
{
    let envelope = match decode_request(request.message().encoded()) {
        Ok(envelope) => envelope,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::MalformedRequest,
                request,
            });
        }
    };
    let operation = envelope.request.operation();
    let response = match authority.and_then(|authority| request.grant().revalidate(authority).ok())
    {
        Some(lease) => lease.with_dispatch_context(|context| {
            dispatch_with_lxmf_peer_discovery_nomad_network_config_and_probe(
                port, nomad_port, probe_port, identity, context, envelope,
            )
        }),
        None => ResponseEnvelope {
            version: ApiVersion::CURRENT,
            request_id: envelope.request_id,
            response: DeviceResponse::Error(ApiErrorResponse {
                code: ApiErrorCode::AuthenticationRequired,
                operation: Some(operation),
            }),
        },
    };

    let mut encoded = [0_u8; MESSAGE_CAPACITY];
    let length = match encode_response(&response, &mut encoded) {
        Ok(length) => length,
        Err(_) => {
            return Err(AuthenticatedApiDispatchFailure {
                kind: AuthenticatedApiDispatchFailureKind::ResponseEncoding,
                request,
            });
        }
    };
    let key = request.key();
    drop(request);
    Ok(LocalApiReply::new(
        key,
        OwnedMessage::new(
            MessageLength::new(length).expect("device API and handoff share one capacity"),
            encoded,
        ),
    ))
}

#[cfg(test)]
#[path = "authenticated_api_node_tests.rs"]
mod tests;
