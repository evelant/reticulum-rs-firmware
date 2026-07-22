//! Device-API dispatch with explicit public identity metadata and a narrow
//! durable-submission ports.
//!
//! This adapter performs no framing, session establishment, allocation, radio
//! work, raw flash access, or journal construction. It authorizes a trusted
//! device-owned dispatch context, scopes status reads by principal, and passes
//! one complete owned acceptance candidate through [`SubmissionPort`] only
//! after an authorized mutation. Basic LXMF send instead passes source-free
//! semantics plus device-derived authorization through `LxmfComposePort`,
//! whose product implementation owns source selection, composition, and
//! durable acceptance. Ports retain all actor, journal, and backend ownership.

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

#[cfg(feature = "experimental-rns-inbox")]
use core::num::NonZeroU64;
#[cfg(feature = "experimental-rns-inbox")]
use reticulum_device_api::MAX_RNS_INBOX_PAYLOAD_BYTES;

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

/// One complete semantic item returned by an inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct InboundMailboxItem {
    id: NonZeroU64,
    destination: api::DestinationHash,
    payload_len: u16,
    payload: [u8; MAX_RNS_INBOX_PAYLOAD_BYTES],
}

#[cfg(feature = "experimental-rns-inbox")]
impl InboundMailboxItem {
    /// Copy one bounded mailbox payload into an owned semantic item.
    pub fn new(
        id: NonZeroU64,
        destination: api::DestinationHash,
        payload: &[u8],
    ) -> Result<Self, InboundMailboxItemTooLarge> {
        if payload.len() > MAX_RNS_INBOX_PAYLOAD_BYTES {
            return Err(InboundMailboxItemTooLarge {
                actual: payload.len(),
            });
        }
        let mut owned = [0_u8; MAX_RNS_INBOX_PAYLOAD_BYTES];
        owned[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            id,
            destination,
            payload_len: payload.len() as u16,
            payload: owned,
        })
    }

    /// Device-assigned mailbox item identifier.
    pub const fn id(&self) -> u64 {
        self.id.get()
    }

    /// Local Reticulum destination that received the item.
    pub const fn destination(&self) -> api::DestinationHash {
        self.destination
    }

    /// Exact semantic payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len as usize]
    }
}

#[cfg(feature = "experimental-rns-inbox")]
impl core::fmt::Debug for InboundMailboxItem {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InboundMailboxItem")
            .field("id", &self.id)
            .field("destination", &self.destination)
            .field("payload_len", &self.payload_len)
            .finish_non_exhaustive()
    }
}

/// A semantic inbound mailbox item exceeded the logical API payload limit.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InboundMailboxItemTooLarge {
    actual: usize,
}

#[cfg(feature = "experimental-rns-inbox")]
impl InboundMailboxItemTooLarge {
    /// Rejected payload length.
    pub const fn actual(self) -> usize {
        self.actual
    }

    /// Maximum accepted payload length.
    pub const fn maximum(self) -> usize {
        MAX_RNS_INBOX_PAYLOAD_BYTES
    }
}

/// Bounded failure vocabulary exposed by an inbound RNS DATA mailbox port.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboundMailboxPortError {
    /// The build or product profile does not provide mailbox service.
    Unavailable,
    /// The sole mailbox owner is temporarily occupied.
    Busy,
    /// Mailbox state could not be read reliably.
    Backend,
    /// The mailbox owner latched an invariant fault.
    Faulted,
}

/// Narrow transport-neutral semantic port for the inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
pub trait InboundMailboxPort {
    /// Current product/runtime availability of inbox service.
    fn availability(&mut self) -> CapabilityAvailability;

    /// Read bounded mailbox runtime state without exposing an item payload.
    fn status(&mut self) -> Result<api::RnsInboxStatus, InboundMailboxPortError>;

    /// Read the oldest item without consuming it.
    fn peek(&mut self) -> Result<Option<InboundMailboxItem>, InboundMailboxPortError>;
}

/// Bounded failure vocabulary exposed by the durable LXMF inbox port.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfInboxPortError {
    /// The build or product profile does not provide the durable LXMF inbox.
    Unavailable,
    /// Another exact retained flash operation currently owns the coordinator.
    Busy,
    /// The requested handle or offset is not a valid readable wire range.
    InvalidRequest,
    /// Durable message bytes could not be read reliably.
    Backend,
    /// The supplied physical binding did not match the mounted store owner.
    Binding,
    /// The mounted owner detected contradictory persisted media.
    Faulted,
}

/// Narrow transport-neutral semantic port for committed LXMF discovery and reads.
#[cfg(feature = "experimental-lxmf")]
pub trait LxmfInboxPort {
    /// Current product/runtime availability of durable LXMF inbox service.
    fn availability(&mut self) -> CapabilityAvailability;

    /// Return the next physical-commit-order summary after an optional cursor.
    fn next(
        &mut self,
        after: Option<api::LxmfMessageHandle>,
    ) -> Result<Option<api::LxmfMessageSummary>, LxmfInboxPortError>;

    /// Return one non-empty caller-bounded normalized-wire chunk.
    fn read(
        &mut self,
        handle: api::LxmfMessageHandle,
        offset: u32,
        max_bytes: api::LxmfReadLength,
    ) -> Result<Option<api::LxmfReadChunk>, LxmfInboxPortError>;
}

/// Source-free semantic input for basic LXMF composition and durable acceptance.
///
/// The authenticated principal and authorization snapshot are derived from the
/// device-owned dispatch context. There is deliberately no source-destination
/// field: the product composer selects the local LXMF identity.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfComposeRequest<'a> {
    principal: storage::PrincipalId,
    destination: storage::DestinationHash,
    timestamp_unix_ms: u64,
    title: &'a [u8],
    content: &'a [u8],
    idempotency_key: storage::IdempotencyKey,
    authorization: storage::AuthorizationSnapshot,
}

#[cfg(feature = "experimental-lxmf")]
impl<'a> LxmfComposeRequest<'a> {
    /// Construct one authenticated source-free composition request.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        principal: storage::PrincipalId,
        destination: storage::DestinationHash,
        timestamp_unix_ms: u64,
        title: &'a [u8],
        content: &'a [u8],
        idempotency_key: storage::IdempotencyKey,
        authorization: storage::AuthorizationSnapshot,
    ) -> Self {
        Self {
            principal,
            destination,
            timestamp_unix_ms,
            title,
            content,
            idempotency_key,
            authorization,
        }
    }

    /// Authenticated principal owning a successful submission.
    pub const fn principal(self) -> storage::PrincipalId {
        self.principal
    }

    /// Complete remote `lxmf.delivery` destination hash.
    pub const fn destination(self) -> storage::DestinationHash {
        self.destination
    }

    /// Caller-selected Unix timestamp in milliseconds.
    pub const fn timestamp_unix_ms(self) -> u64 {
        self.timestamp_unix_ms
    }

    /// Exact borrowed binary title.
    pub const fn title(self) -> &'a [u8] {
        self.title
    }

    /// Exact borrowed binary content.
    pub const fn content(self) -> &'a [u8] {
        self.content
    }

    /// Principal-scoped idempotency key.
    pub const fn idempotency_key(self) -> storage::IdempotencyKey {
        self.idempotency_key
    }

    /// Exact device-derived authorization facts for durable acceptance.
    pub const fn authorization(self) -> storage::AuthorizationSnapshot {
        self.authorization
    }
}

/// Basic LXMF composition result paired with durable-submission acceptance.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfComposeAcceptance {
    acceptance: SubmissionAcceptance,
    message_id: [u8; 32],
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfComposeAcceptance {
    /// Pair durable acceptance progress with the exact composed LXMF message ID.
    pub const fn new(acceptance: SubmissionAcceptance, message_id: [u8; 32]) -> Self {
        Self {
            acceptance,
            message_id,
        }
    }

    /// Durable acceptance or idempotency outcome.
    pub const fn acceptance(self) -> SubmissionAcceptance {
        self.acceptance
    }

    /// Python-compatible LXMF authenticated-message identifier.
    pub const fn message_id(&self) -> &[u8; 32] {
        &self.message_id
    }
}

/// Closed failure vocabulary for basic LXMF composition and durable acceptance.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LxmfComposePortError {
    /// The product profile or current runtime has disabled basic LXMF send.
    Unavailable,
    /// The semantic request cannot be composed, including carrier-size overflow.
    InvalidRequest,
    /// Another exact retained durable mutation currently owns the service.
    Busy,
    /// A physical backend operation failed or returned ambiguously.
    Backend,
    /// The supplied physical storage binding did not match the mounted owner.
    Binding,
    /// The mounted owner latched a semantic or physical fault.
    Faulted,
    /// Composition or durable acceptance violated an internal invariant.
    Invariant,
}

/// Narrow product-owned port for basic LXMF composition and durable submission.
///
/// Implementations select the local LXMF source and the final Python-compatible
/// carrier, then durably accept that exact composed message as one operation.
#[cfg(feature = "experimental-lxmf")]
pub trait LxmfComposePort {
    /// Current product/runtime availability of basic LXMF send.
    fn availability(&mut self) -> CapabilityAvailability;

    /// Compose using the device identity and durably accept the exact result.
    fn compose_and_accept(
        &mut self,
        request: LxmfComposeRequest<'_>,
    ) -> Result<LxmfComposeAcceptance, LxmfComposePortError>;
}

/// Authorize and dispatch one decoded logical request against a narrow
/// durable-submission port.
///
/// The response always uses the current device API version and echoes the
/// caller's request identifier. Authentication facts come only from
/// `context`; request CBOR cannot supply or replace them. The public `identity`
/// summary is supplied independently of the submission port so an identity
/// read cannot acquire storage capabilities. Although the wire decoder rejects
/// incompatible majors, this boundary repeats that check so a manually
/// constructed envelope cannot bypass version policy.
pub fn dispatch<P>(
    port: &mut P,
    identity: api::IdentitySummary,
    context: &DispatchContext,
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
            Ok(()) => dispatch_authorized(port, identity, context, request, operation),
            Err(error) => authorization_error(error, operation),
        }
    };
    ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id,
        response,
    }
}

/// Authorize and dispatch one request against independent submission and inbox ports.
///
/// Identity reads invoke neither port. Submission operations invoke only
/// `submission_port`, while experimental inbox operations invoke only
/// `inbox_port`.
#[cfg(feature = "experimental-rns-inbox")]
pub fn dispatch_with_inbox<P, M>(
    submission_port: &mut P,
    inbox_port: &mut M,
    identity: api::IdentitySummary,
    context: &DispatchContext,
    envelope: RequestEnvelope<'_>,
) -> ResponseEnvelope
where
    P: SubmissionPort,
    M: InboundMailboxPort,
{
    let request_id = envelope.request_id;
    let request = envelope.request;
    let operation = request.operation();
    let response = if envelope.version.major != ApiVersion::CURRENT.major {
        api_error(ApiErrorCode::UnsupportedVersion, operation)
    } else {
        match authorize_request(context, &request) {
            Ok(()) => dispatch_authorized_with_inbox(
                submission_port,
                inbox_port,
                identity,
                context,
                request,
                operation,
            ),
            Err(error) => authorization_error(error, operation),
        }
    };
    ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id,
        response,
    }
}

/// Authorize and dispatch one request against a single owner implementing the
/// submission, durable LXMF inbox, and basic-LXMF compose ports.
///
/// This entry point deliberately does not require the raw-RNS inbox feature or
/// port. Products can therefore compile the higher-level LXMF client surface
/// without retaining the raw qualification mailbox.
#[cfg(feature = "experimental-lxmf")]
pub fn dispatch_with_lxmf<P>(
    port: &mut P,
    identity: api::IdentitySummary,
    context: &DispatchContext,
    envelope: RequestEnvelope<'_>,
) -> ResponseEnvelope
where
    P: SubmissionPort + LxmfInboxPort + LxmfComposePort,
{
    let request_id = envelope.request_id;
    let request = envelope.request;
    let operation = request.operation();
    let response = if envelope.version.major != ApiVersion::CURRENT.major {
        api_error(ApiErrorCode::UnsupportedVersion, operation)
    } else {
        match authorize_request(context, &request) {
            Ok(()) => dispatch_authorized_with_lxmf(port, identity, context, request, operation),
            Err(error) => authorization_error(error, operation),
        }
    };
    ResponseEnvelope {
        version: ApiVersion::CURRENT,
        request_id,
        response,
    }
}

/// Authorize and dispatch one request against a single owner implementing the
/// submission, raw-RNS inbox, durable LXMF inbox, and basic-LXMF compose ports.
///
/// A single combined owner is intentional: both durable submission and LXMF
/// reads may need operation-scoped access to the same physical flash device.
/// Dispatch invokes only the method selected by the decoded operation, so no
/// physical capability is aliased and public identity reads invoke no port.
#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
pub fn dispatch_with_inbox_and_lxmf<P>(
    port: &mut P,
    identity: api::IdentitySummary,
    context: &DispatchContext,
    envelope: RequestEnvelope<'_>,
) -> ResponseEnvelope
where
    P: SubmissionPort + InboundMailboxPort + LxmfInboxPort + LxmfComposePort,
{
    let request_id = envelope.request_id;
    let request = envelope.request;
    let operation = request.operation();
    let response = if envelope.version.major != ApiVersion::CURRENT.major {
        api_error(ApiErrorCode::UnsupportedVersion, operation)
    } else {
        match authorize_request(context, &request) {
            Ok(()) => {
                dispatch_authorized_with_inbox_and_lxmf(port, identity, context, request, operation)
            }
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
    identity: api::IdentitySummary,
    context: &DispatchContext,
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
        DeviceRequest::IdentitySummary => DeviceResponse::IdentitySummary(identity),
        DeviceRequest::SubmissionStatus { id } => {
            let Some(principal) = context.principal() else {
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
            let Some(principal) = context.principal() else {
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
            let Some(provenance) = context.provenance() else {
                return api_error(ApiErrorCode::AuthenticationRequired, operation);
            };
            let authorization = match storage::AuthorizationSnapshot::new(
                provenance.credential_id(),
                provenance.credential_generation(),
                provenance.authority_revision(),
                provenance.policy_version(),
                context.permissions().bits(),
            ) {
                Ok(authorization) => authorization,
                Err(_) => return api_error(ApiErrorCode::Internal, operation),
            };
            let candidate = storage::AcceptanceCandidate::new(
                storage::PrincipalId::new(principal.0),
                storage::IdempotencyKey::new(idempotency_key.0),
                intent,
                authorization,
            );
            acceptance_response(port.accept(candidate), operation)
        }
        _ => api_error(ApiErrorCode::UnsupportedOperation, operation),
    }
}

#[cfg(feature = "experimental-rns-inbox")]
fn dispatch_authorized_with_inbox<P, M>(
    submission_port: &mut P,
    inbox_port: &mut M,
    identity: api::IdentitySummary,
    context: &DispatchContext,
    request: DeviceRequest<'_>,
    operation: u16,
) -> DeviceResponse
where
    P: SubmissionPort,
    M: InboundMailboxPort,
{
    match request {
        DeviceRequest::SystemCapabilities => {
            let submit_available = cfg!(feature = "experimental-rns-data")
                && submission_port.availability() == CapabilityAvailability::Available;
            let inbox_availability = inbox_port.availability();
            DeviceResponse::SystemCapabilities(api::CapabilitySnapshot::for_dispatch_with_inbox(
                submit_available,
                inbox_availability,
            ))
        }
        DeviceRequest::IdentitySummary => DeviceResponse::IdentitySummary(identity),
        DeviceRequest::RnsInboxStatus => {
            if inbox_port.availability() != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
            match inbox_port.status() {
                Ok(status) => DeviceResponse::RnsInboxStatus(status),
                Err(error) => inbox_port_error(error, operation),
            }
        }
        DeviceRequest::RnsInboxPeek => {
            if inbox_port.availability() != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
            match inbox_port.peek() {
                Ok(Some(item)) => {
                    match api::RnsInboxItem::new(item.id, item.destination(), item.payload()) {
                        Ok(item) => DeviceResponse::RnsInboxPeek(item),
                        Err(_) => api_error(ApiErrorCode::Internal, operation),
                    }
                }
                Ok(None) => api_error(ApiErrorCode::NotFound, operation),
                Err(error) => inbox_port_error(error, operation),
            }
        }
        other => dispatch_authorized(submission_port, identity, context, other, operation),
    }
}

#[cfg(all(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
fn dispatch_authorized_with_inbox_and_lxmf<P>(
    port: &mut P,
    identity: api::IdentitySummary,
    context: &DispatchContext,
    request: DeviceRequest<'_>,
    operation: u16,
) -> DeviceResponse
where
    P: SubmissionPort + InboundMailboxPort + LxmfInboxPort + LxmfComposePort,
{
    match request {
        DeviceRequest::SystemCapabilities => {
            let submit_available = cfg!(feature = "experimental-rns-data")
                && SubmissionPort::availability(port) == CapabilityAvailability::Available;
            let rns_inbox = InboundMailboxPort::availability(port);
            let lxmf = LxmfInboxPort::availability(port);
            let lxmf_basic_send = LxmfComposePort::availability(port);
            DeviceResponse::SystemCapabilities(
                api::CapabilitySnapshot::for_dispatch_with_inbox_lxmf_and_basic_send(
                    submit_available,
                    rns_inbox,
                    lxmf,
                    lxmf_basic_send,
                ),
            )
        }
        DeviceRequest::IdentitySummary => DeviceResponse::IdentitySummary(identity),
        DeviceRequest::RnsInboxStatus => {
            if InboundMailboxPort::availability(port) != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
            match InboundMailboxPort::status(port) {
                Ok(status) => DeviceResponse::RnsInboxStatus(status),
                Err(error) => inbox_port_error(error, operation),
            }
        }
        DeviceRequest::RnsInboxPeek => {
            if InboundMailboxPort::availability(port) != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
            match InboundMailboxPort::peek(port) {
                Ok(Some(item)) => {
                    match api::RnsInboxItem::new(item.id, item.destination(), item.payload()) {
                        Ok(item) => DeviceResponse::RnsInboxPeek(item),
                        Err(_) => api_error(ApiErrorCode::Internal, operation),
                    }
                }
                Ok(None) => api_error(ApiErrorCode::NotFound, operation),
                Err(error) => inbox_port_error(error, operation),
            }
        }
        other => dispatch_authorized_with_lxmf(port, identity, context, other, operation),
    }
}

#[cfg(feature = "experimental-lxmf")]
fn dispatch_authorized_with_lxmf<P>(
    port: &mut P,
    identity: api::IdentitySummary,
    context: &DispatchContext,
    request: DeviceRequest<'_>,
    operation: u16,
) -> DeviceResponse
where
    P: SubmissionPort + LxmfInboxPort + LxmfComposePort,
{
    match request {
        DeviceRequest::SystemCapabilities => {
            let submit_available = cfg!(feature = "experimental-rns-data")
                && SubmissionPort::availability(port) == CapabilityAvailability::Available;
            let lxmf = LxmfInboxPort::availability(port);
            let lxmf_basic_send = LxmfComposePort::availability(port);
            DeviceResponse::SystemCapabilities(
                api::CapabilitySnapshot::for_dispatch_with_inbox_lxmf_and_basic_send(
                    submit_available,
                    CapabilityAvailability::Unavailable,
                    lxmf,
                    lxmf_basic_send,
                ),
            )
        }
        DeviceRequest::IdentitySummary => DeviceResponse::IdentitySummary(identity),
        DeviceRequest::LxmfNext { after } => {
            if LxmfInboxPort::availability(port) != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
            match LxmfInboxPort::next(port, after) {
                Ok(Some(summary)) => DeviceResponse::LxmfNext(summary),
                Ok(None) => api_error(ApiErrorCode::NotFound, operation),
                Err(error) => lxmf_port_error(error, operation),
            }
        }
        DeviceRequest::LxmfRead {
            handle,
            offset,
            max_bytes,
        } => {
            if LxmfInboxPort::availability(port) != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
            match LxmfInboxPort::read(port, handle, offset, max_bytes) {
                Ok(Some(chunk)) => DeviceResponse::LxmfRead(chunk),
                Ok(None) => api_error(ApiErrorCode::NotFound, operation),
                Err(error) => lxmf_port_error(error, operation),
            }
        }
        DeviceRequest::LxmfBasicSend {
            destination,
            timestamp_unix_ms,
            title,
            content,
            idempotency_key,
        } => {
            let Some(principal) = context.principal() else {
                return api_error(ApiErrorCode::AuthenticationRequired, operation);
            };
            if LxmfComposePort::availability(port) != CapabilityAvailability::Available {
                return api_error(ApiErrorCode::CapabilityUnavailable, operation);
            }
            let Some(authorization) = authorization_snapshot(context) else {
                return api_error(ApiErrorCode::Internal, operation);
            };
            let request = LxmfComposeRequest::new(
                storage::PrincipalId::new(principal.0),
                storage::DestinationHash::new(destination.0),
                timestamp_unix_ms,
                title,
                content,
                storage::IdempotencyKey::new(idempotency_key.0),
                authorization,
            );
            lxmf_compose_response(port.compose_and_accept(request), operation)
        }
        other => dispatch_authorized(port, identity, context, other, operation),
    }
}

fn authorization_error(error: AuthorizationError, operation: u16) -> DeviceResponse {
    let code = match error {
        AuthorizationError::AuthenticationRequired => ApiErrorCode::AuthenticationRequired,
        AuthorizationError::PermissionDenied(_) => ApiErrorCode::PermissionDenied,
    };
    api_error(code, operation)
}

#[cfg(feature = "experimental-lxmf")]
fn authorization_snapshot(context: &DispatchContext) -> Option<storage::AuthorizationSnapshot> {
    let provenance = context.provenance()?;
    storage::AuthorizationSnapshot::new(
        provenance.credential_id(),
        provenance.credential_generation(),
        provenance.authority_revision(),
        provenance.policy_version(),
        context.permissions().bits(),
    )
    .ok()
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

#[cfg(feature = "experimental-rns-inbox")]
fn inbox_port_error(error: InboundMailboxPortError, operation: u16) -> DeviceResponse {
    let code = match error {
        InboundMailboxPortError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        InboundMailboxPortError::Busy
        | InboundMailboxPortError::Backend
        | InboundMailboxPortError::Faulted => ApiErrorCode::Internal,
    };
    api_error(code, operation)
}

#[cfg(feature = "experimental-lxmf")]
fn lxmf_port_error(error: LxmfInboxPortError, operation: u16) -> DeviceResponse {
    let code = match error {
        LxmfInboxPortError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        LxmfInboxPortError::InvalidRequest => ApiErrorCode::InvalidRequest,
        LxmfInboxPortError::Busy
        | LxmfInboxPortError::Backend
        | LxmfInboxPortError::Binding
        | LxmfInboxPortError::Faulted => ApiErrorCode::Internal,
    };
    api_error(code, operation)
}

#[cfg(feature = "experimental-lxmf")]
fn lxmf_compose_response(
    progress: Result<LxmfComposeAcceptance, LxmfComposePortError>,
    operation: u16,
) -> DeviceResponse {
    match progress {
        Ok(progress) => match progress.acceptance() {
            SubmissionAcceptance::Accepted(id) | SubmissionAcceptance::Replay(id) => {
                DeviceResponse::LxmfBasicSendAccepted(api::LxmfBasicSendAccepted::new(
                    api_submission_id(id),
                    *progress.message_id(),
                ))
            }
            SubmissionAcceptance::IdempotencyConflict => {
                api_error(ApiErrorCode::IdempotencyConflict, operation)
            }
            SubmissionAcceptance::CapacityExhausted => {
                api_error(ApiErrorCode::CapacityExhausted, operation)
            }
            SubmissionAcceptance::IdentifierExhausted => {
                api_error(ApiErrorCode::Internal, operation)
            }
        },
        Err(error) => lxmf_compose_port_error(error, operation),
    }
}

#[cfg(feature = "experimental-lxmf")]
fn lxmf_compose_port_error(error: LxmfComposePortError, operation: u16) -> DeviceResponse {
    let code = match error {
        LxmfComposePortError::Unavailable => ApiErrorCode::CapabilityUnavailable,
        LxmfComposePortError::InvalidRequest => ApiErrorCode::InvalidRequest,
        LxmfComposePortError::Busy
        | LxmfComposePortError::Backend
        | LxmfComposePortError::Binding
        | LxmfComposePortError::Faulted
        | LxmfComposePortError::Invariant => ApiErrorCode::Internal,
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
