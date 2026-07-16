//! Rete-independent logical API model and authorization vocabulary.

use core::{convert::Infallible, marker::PhantomData, ops::BitOr};

/// Device API v1 major version.
pub const API_VERSION_MAJOR: u16 = 1;
/// Initial device API v1 minor version.
pub const API_VERSION_MINOR: u16 = 0;

/// Maximum size of one decoded or encoded logical CBOR message.
pub const MAX_MESSAGE_BYTES: usize = 512;
/// Maximum encoded size of the operation-specific body within a message.
pub const MAX_BODY_BYTES: usize = 448;
/// Maximum payload accepted by the experimental RNS DATA preparation request.
pub const MAX_PREPARE_RNS_DATA_PAYLOAD_BYTES: usize = 383;

/// `system.capabilities` operation number.
pub const OP_SYSTEM_CAPABILITIES: u16 = 0x0001;
/// `submission.status` operation number.
pub const OP_SUBMISSION_STATUS: u16 = 0x0002;
/// Error response kind used instead of a successful operation number.
pub const RESPONSE_ERROR: u16 = 0x0000;
/// Host-simulation-only packet-preparation operation in the experimental range.
#[cfg(feature = "host-sim")]
pub const OP_EXPERIMENTAL_PREPARE_RNS_DATA: u16 = 0xf001;

/// Major/minor logical protocol version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiVersion {
    /// Incompatible protocol generation.
    pub major: u16,
    /// Backward-compatible feature revision within a major generation.
    pub minor: u16,
}

impl ApiVersion {
    /// Version implemented by this crate.
    pub const CURRENT: Self = Self {
        major: API_VERSION_MAJOR,
        minor: API_VERSION_MINOR,
    };
}

/// Client-chosen identifier echoed in the corresponding response.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(pub u64);

/// Authenticated local-client principal supplied by the session layer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrincipalId(pub [u8; 16]);

/// Client-chosen key used to deduplicate a state-changing operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyKey(pub [u8; 16]);

/// Complete 128-bit Reticulum destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DestinationHash(pub [u8; 16]);

/// Device-assigned identifier for a submitted operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubmissionId(pub u64);

/// Permissions established by an authenticated session, never by CBOR input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Permissions(u32);

impl Permissions {
    /// No authenticated operation permissions.
    pub const NONE: Self = Self(0);
    /// Read submission state belonging to the authenticated principal.
    pub const READ_SUBMISSION_STATUS: Self = Self(1 << 0);
    /// Exercise the unstable host-only RNS DATA preparation operation.
    #[cfg(feature = "host-sim")]
    pub const EXPERIMENTAL_PREPARE_RNS_DATA: Self = Self(1 << 1);

    /// Whether all bits in `required` are present.
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// Raw representation for session-policy adapters and diagnostics.
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl BitOr for Permissions {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Trusted authentication and authorization facts supplied out of band.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchContext {
    /// Principal authenticated by the local-session layer, if any.
    pub principal: Option<PrincipalId>,
    /// Permissions granted to that authenticated principal.
    pub permissions: Permissions,
}

impl DispatchContext {
    /// Context for a connection without an authenticated application session.
    pub const UNAUTHENTICATED: Self = Self {
        principal: None,
        permissions: Permissions::NONE,
    };

    /// Construct a trusted context for an authenticated principal.
    pub const fn authenticated(principal: PrincipalId, permissions: Permissions) -> Self {
        Self {
            principal: Some(principal),
            permissions,
        }
    }
}

/// Permission category required by an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredPermission {
    /// Read submission status.
    ReadSubmissionStatus,
    /// Exercise the unstable host-simulation preparation path.
    #[cfg(feature = "host-sim")]
    ExperimentalPrepareRnsData,
}

impl RequiredPermission {
    const fn bits(self) -> Permissions {
        match self {
            Self::ReadSubmissionStatus => Permissions::READ_SUBMISSION_STATUS,
            #[cfg(feature = "host-sim")]
            Self::ExperimentalPrepareRnsData => Permissions::EXPERIMENTAL_PREPARE_RNS_DATA,
        }
    }
}

/// Authorization failure established without consulting untrusted message data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationError {
    /// The operation requires an authenticated principal.
    AuthenticationRequired,
    /// The authenticated principal lacks the operation permission.
    PermissionDenied(RequiredPermission),
}

/// Logical request body. It contains no transport, session, or Rete owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeviceRequest<'a> {
    /// Read API version, safety capabilities, and hard codec limits.
    SystemCapabilities,
    /// Read status for a previously accepted submission.
    SubmissionStatus {
        /// Device-assigned submission identifier.
        id: SubmissionId,
    },
    /// Prepare RNS DATA in host simulation without exposing packet output.
    #[cfg(feature = "host-sim")]
    PrepareRnsData {
        /// Complete Reticulum destination hash.
        destination: DestinationHash,
        /// Borrowed application data; never allocated or copied by decoding.
        payload: &'a [u8],
        /// Deduplication key scoped by the authenticated principal and content.
        idempotency_key: IdempotencyKey,
    },
    /// Uninhabited marker keeping the decode lifetime stable without `host-sim`.
    #[doc(hidden)]
    __Borrowed(Infallible, PhantomData<&'a [u8]>),
}

impl DeviceRequest<'_> {
    /// Stable or experimental operation number encoded on the wire.
    pub const fn operation(&self) -> u16 {
        match self {
            Self::SystemCapabilities => OP_SYSTEM_CAPABILITIES,
            Self::SubmissionStatus { .. } => OP_SUBMISSION_STATUS,
            #[cfg(feature = "host-sim")]
            Self::PrepareRnsData { .. } => OP_EXPERIMENTAL_PREPARE_RNS_DATA,
            Self::__Borrowed(never, _) => match *never {},
        }
    }

    /// Whether this operation can change node state.
    pub const fn is_mutating(&self) -> bool {
        match self {
            Self::SystemCapabilities | Self::SubmissionStatus { .. } => false,
            #[cfg(feature = "host-sim")]
            Self::PrepareRnsData { .. } => true,
            Self::__Borrowed(never, _) => match *never {},
        }
    }

    const fn required_permission(&self) -> Option<RequiredPermission> {
        match self {
            Self::SystemCapabilities => None,
            Self::SubmissionStatus { .. } => Some(RequiredPermission::ReadSubmissionStatus),
            #[cfg(feature = "host-sim")]
            Self::PrepareRnsData { .. } => Some(RequiredPermission::ExperimentalPrepareRnsData),
            Self::__Borrowed(never, _) => match *never {},
        }
    }
}

/// Logical request envelope decoded from exactly one CBOR item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestEnvelope<'a> {
    /// Protocol version selected by the client.
    pub version: ApiVersion,
    /// Client request identifier.
    pub request_id: RequestId,
    /// Operation-specific request.
    pub request: DeviceRequest<'a>,
}

/// Apply common authentication and permission policy to a decoded request.
///
/// The principal is intentionally absent from [`RequestEnvelope`]. Callers
/// must obtain `context` from their separately authenticated session.
pub const fn authorize_request(
    context: DispatchContext,
    request: &DeviceRequest<'_>,
) -> Result<(), AuthorizationError> {
    let Some(required) = request.required_permission() else {
        return Ok(());
    };
    if context.principal.is_none() {
        return Err(AuthorizationError::AuthenticationRequired);
    }
    if !context.permissions.contains(required.bits()) {
        return Err(AuthorizationError::PermissionDenied(required));
    }
    Ok(())
}

/// Runtime availability of a logical capability.
///
/// This is a closed API-v1 wire vocabulary. Adding a numeric value requires a
/// new API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CapabilityAvailability {
    /// This build cannot perform the capability.
    Unavailable = 0,
    /// Code exists, but profile or runtime policy has disabled it.
    Disabled = 1,
    /// The capability is present and enabled.
    Available = 2,
}

impl CapabilityAvailability {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Device-owned capability and codec-limit handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilitySnapshot {
    /// Highest API version spoken by this device.
    pub(crate) api_version: ApiVersion,
    /// Whether any public operation can return raw prepared packet bytes.
    pub(crate) packet_output: bool,
    /// Availability of physical radio transmission.
    pub(crate) radio_tx: CapabilityAvailability,
    /// Whether the unstable host preparation operation is compiled in.
    pub(crate) experimental_prepare_rns_data: bool,
    /// Hard maximum logical CBOR message size.
    pub(crate) max_message_bytes: u16,
    /// Hard maximum encoded operation-body size.
    pub(crate) max_body_bytes: u16,
    /// Maximum experimental preparation payload.
    pub(crate) max_prepare_rns_data_payload_bytes: u16,
}

impl CapabilitySnapshot {
    /// Snapshot for this crate's current build.
    ///
    /// Packet output and radio TX remain deliberately unavailable in every
    /// feature composition of this slice.
    pub const fn current() -> Self {
        Self {
            api_version: ApiVersion::CURRENT,
            packet_output: false,
            radio_tx: CapabilityAvailability::Unavailable,
            experimental_prepare_rns_data: cfg!(feature = "host-sim"),
            max_message_bytes: MAX_MESSAGE_BYTES as u16,
            max_body_bytes: MAX_BODY_BYTES as u16,
            max_prepare_rns_data_payload_bytes: MAX_PREPARE_RNS_DATA_PAYLOAD_BYTES as u16,
        }
    }

    /// Snapshot restricted to operations implemented by a higher dispatch layer.
    ///
    /// `experimental_prepare_rns_data` can disable the codec-build capability,
    /// but cannot enable an operation omitted from this crate's build. This
    /// keeps Cargo feature unification in another dependency edge from making a
    /// dispatcher advertise an operation that it did not compile locally.
    pub const fn for_dispatch(experimental_prepare_rns_data: bool) -> Self {
        let mut snapshot = Self::current();
        snapshot.experimental_prepare_rns_data &= experimental_prepare_rns_data;
        snapshot
    }

    /// Highest API version spoken by this device.
    pub const fn api_version(self) -> ApiVersion {
        self.api_version
    }

    /// Whether any public operation can return raw prepared packet bytes.
    pub const fn packet_output(self) -> bool {
        self.packet_output
    }

    /// Availability of physical radio transmission.
    pub const fn radio_tx(self) -> CapabilityAvailability {
        self.radio_tx
    }

    /// Whether the unstable host preparation operation is compiled in.
    pub const fn experimental_prepare_rns_data(self) -> bool {
        self.experimental_prepare_rns_data
    }

    /// Hard maximum logical CBOR message size.
    pub const fn max_message_bytes(self) -> u16 {
        self.max_message_bytes
    }

    /// Hard maximum encoded operation-body size.
    pub const fn max_body_bytes(self) -> u16 {
        self.max_body_bytes
    }

    /// Maximum experimental preparation payload.
    pub const fn max_prepare_rns_data_payload_bytes(self) -> u16 {
        self.max_prepare_rns_data_payload_bytes
    }
}

/// SHA-256 digest of every byte in one complete encoded Reticulum packet.
///
/// This is deliberately a distinct type from Reticulum's proof-correlation
/// hash, which covers only the protocol-defined hashable part of a packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EncodedPacketSha256([u8; 32]);

impl EncodedPacketSha256 {
    /// Construct an encoded-packet digest from its complete bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Prepared-packet diagnostics that never expose the packet itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedPacketDetails {
    /// Encoded packet length.
    pub packet_len: u16,
    /// SHA-256 of every byte in the complete encoded packet.
    pub encoded_packet_sha256: EncodedPacketSha256,
}

/// Progress of an accepted submission, without prepared packet bytes.
///
/// State-specific data lives in the corresponding variant, so contradictory
/// combinations such as a queued submission with a packet hash or a failed
/// submission without a failure category cannot be represented. This is a
/// closed API-v1 wire vocabulary; adding a numeric state requires a new API
/// major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmissionState {
    /// Accepted into a bounded intent queue.
    Queued,
    /// Currently being processed by the node owner.
    Preparing,
    /// Packet preparation completed and delivery is pending.
    ///
    /// The details are durable status metadata; this state does not imply that
    /// encoded packet bytes still occupy the node's private transmit outbox.
    AwaitingDelivery(PreparedPacketDetails),
    /// A later proof or application acknowledgement completed the submission.
    Delivered(PreparedPacketDetails),
    /// Submission terminated with a typed failure.
    Failed(SubmissionFailure),
    /// Submission was cancelled before it became irreversible.
    Cancelled,
}

impl SubmissionState {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::Preparing => 1,
            Self::AwaitingDelivery(_) => 2,
            Self::Delivered(_) => 3,
            Self::Failed(_) => 4,
            Self::Cancelled => 5,
        }
    }
}

/// Stable failure category suitable for a submission status response.
///
/// This is a closed API-v1 wire vocabulary. Adding a numeric category requires
/// a new API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SubmissionFailure {
    /// Destination is not currently reachable through a known path.
    NoPath = 0,
    /// An accepted submission received no required proof or acknowledgement
    /// before its delivery deadline.
    DeliveryTimeout = 1,
    /// Accepted work was later rejected by a downstream protocol or policy
    /// stage that could not decide at request admission.
    Rejected = 2,
    /// Processing failed for a non-client fault.
    Internal = 3,
}

impl SubmissionFailure {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Scalar-only status for an accepted submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmissionStatus {
    /// Submission being described.
    pub id: SubmissionId,
    /// Current state.
    pub state: SubmissionState,
}

/// Acceptance result for the host-only experimental preparation operation.
///
/// The response contains only the device-assigned identifier used with
/// `submission.status`; it never contains prepared packet bytes.
#[cfg(feature = "host-sim")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmissionAccepted {
    /// Device-assigned submission identifier.
    ///
    /// Acceptance means the device reserved the bounded capacity needed to own
    /// the submission. It does not guarantee delivery; a later status may
    /// report [`SubmissionFailure::DeliveryTimeout`].
    pub id: SubmissionId,
}

/// Typed API error returned in a logical response.
///
/// This is a closed API-v1 wire vocabulary. Adding a numeric error category
/// requires a new API major version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ApiErrorCode {
    /// Client selected an unknown operation number.
    UnsupportedOperation = 1,
    /// Client selected an incompatible API major version.
    UnsupportedVersion = 2,
    /// Operation requires an authenticated application session.
    AuthenticationRequired = 3,
    /// Authenticated principal lacks the required permission.
    PermissionDenied = 4,
    /// Requested object does not exist for this principal.
    NotFound = 5,
    /// Request is semantically invalid after decoding.
    InvalidRequest = 6,
    /// Build or runtime profile cannot perform the operation.
    CapabilityUnavailable = 7,
    /// Device failed without a client-actionable category.
    Internal = 8,
    /// Operation was not accepted because a bounded queue or table is full.
    ///
    /// No submission identifier is allocated. Retrying later may succeed.
    CapacityExhausted = 9,
    /// This principal already used the supplied idempotency key for different
    /// request content.
    ///
    /// The conflicting request is not accepted. Repeating the original
    /// request content remains safe.
    IdempotencyConflict = 10,
}

impl ApiErrorCode {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u16 {
        self as u16
    }
}

/// Error response body with optional numeric operation context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiErrorResponse {
    /// Stable machine-readable category.
    pub code: ApiErrorCode,
    /// Request operation related to the error, when known.
    pub operation: Option<u16>,
}

/// Successful or failed logical response body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeviceResponse {
    /// Result of `system.capabilities`.
    SystemCapabilities(CapabilitySnapshot),
    /// Result of `submission.status`.
    SubmissionStatus(SubmissionStatus),
    /// Accepted host-only experimental packet-preparation intent.
    #[cfg(feature = "host-sim")]
    PrepareRnsDataAccepted(SubmissionAccepted),
    /// Typed request failure.
    Error(ApiErrorResponse),
}

impl DeviceResponse {
    /// Operation or response-kind number encoded on the wire.
    pub const fn kind(&self) -> u16 {
        match self {
            Self::SystemCapabilities(_) => OP_SYSTEM_CAPABILITIES,
            Self::SubmissionStatus(_) => OP_SUBMISSION_STATUS,
            #[cfg(feature = "host-sim")]
            Self::PrepareRnsDataAccepted(_) => OP_EXPERIMENTAL_PREPARE_RNS_DATA,
            Self::Error(_) => RESPONSE_ERROR,
        }
    }
}

/// Logical response envelope encoded as exactly one CBOR item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseEnvelope {
    /// Protocol version selected by the device.
    pub version: ApiVersion,
    /// Request identifier copied from the request.
    pub request_id: RequestId,
    /// Operation-specific response.
    pub response: DeviceResponse,
}
