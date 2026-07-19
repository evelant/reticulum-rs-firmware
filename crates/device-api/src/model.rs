//! Rete-independent logical API model and authorization vocabulary.

use core::{convert::Infallible, marker::PhantomData, num::NonZeroU64, ops::BitOr};

/// Device API v1 major version.
pub const API_VERSION_MAJOR: u16 = 1;
/// Device API v1 revision adding the optional experimental RNS inbox capability.
pub const API_VERSION_MINOR: u16 = 2;

/// Maximum size of one decoded or encoded logical CBOR message.
pub const MAX_MESSAGE_BYTES: usize = 512;
/// Maximum encoded size of the operation-specific body within a message.
pub const MAX_BODY_BYTES: usize = 448;
/// Maximum payload accepted by the experimental RNS DATA submission request.
pub const MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES: usize = 383;
/// Maximum payload returned by the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
pub const MAX_RNS_INBOX_PAYLOAD_BYTES: usize = 383;

/// `system.capabilities` operation number.
pub const OP_SYSTEM_CAPABILITIES: u16 = 0x0001;
/// `submission.status` operation number.
pub const OP_SUBMISSION_STATUS: u16 = 0x0002;
/// `identity.summary` operation number.
pub const OP_IDENTITY_SUMMARY: u16 = 0x0003;
/// Error response kind used instead of a successful operation number.
pub const RESPONSE_ERROR: u16 = 0x0000;
/// Target-safe outbound RNS DATA submission operation in the experimental range.
#[cfg(feature = "experimental-rns-data")]
pub const OP_EXPERIMENTAL_SUBMIT_RNS_DATA: u16 = 0xf001;
/// Read bounded runtime state for the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
pub const OP_EXPERIMENTAL_RNS_INBOX_STATUS: u16 = 0xf002;
/// Read the oldest item in the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
pub const OP_EXPERIMENTAL_RNS_INBOX_PEEK: u16 = 0xf003;

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

/// Authenticated local-client principal derived from device-owned authority.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrincipalId(pub [u8; 16]);

/// Client-chosen key used to deduplicate a state-changing operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IdempotencyKey(pub [u8; 16]);

/// Complete 128-bit Reticulum destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DestinationHash(pub [u8; 16]);

/// Public, copy-only summary of the node's primary Reticulum identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentitySummary {
    /// Primary destination served by the node.
    primary_destination: DestinationHash,
}

impl IdentitySummary {
    /// Construct the public summary for a node's primary destination.
    pub const fn new(primary_destination: DestinationHash) -> Self {
        Self {
            primary_destination,
        }
    }

    /// Primary destination served by the node.
    pub const fn primary_destination(self) -> DestinationHash {
        self.primary_destination
    }
}

/// Device-assigned identifier for a submitted operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubmissionId(pub u64);

/// Permissions derived from device-owned authority, never from CBOR input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Permissions(u32);

impl Permissions {
    /// No authenticated operation permissions.
    pub const NONE: Self = Self(0);
    /// Read submission state belonging to the authenticated principal.
    pub const READ_SUBMISSION_STATUS: Self = Self(1 << 0);
    /// Submit outbound RNS DATA through the node's transport-neutral router.
    ///
    /// The bit remains part of the stable persisted permission vocabulary even
    /// when this build omits the experimental operation itself.
    pub const EXPERIMENTAL_SUBMIT_RNS_DATA: Self = Self(1 << 1);

    const KNOWN_BITS: u32 = Self::READ_SUBMISSION_STATUS.0 | Self::EXPERIMENTAL_SUBMIT_RNS_DATA.0;

    /// Whether all bits in `required` are present.
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// Raw representation for session-policy adapters and diagnostics.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Decode the stable persisted permission vocabulary without feature drift.
    pub const fn from_bits(bits: u32) -> Result<Self, UnknownPermissionBits> {
        let unknown = bits & !Self::KNOWN_BITS;
        if unknown == 0 {
            Ok(Self(bits))
        } else {
            Err(UnknownPermissionBits { unknown })
        }
    }
}

/// Persisted permission bits unknown to this device-API schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownPermissionBits {
    unknown: u32,
}

impl UnknownPermissionBits {
    /// Bits outside the stable schema-v1 permission vocabulary.
    pub const fn unknown(self) -> u32 {
        self.unknown
    }
}

impl BitOr for Permissions {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Device-owned credential facts that authorized one dispatch attempt.
///
/// This value is supplied out of band with the trusted dispatch context and is
/// never decoded from the device-API wire message. Its public constructor lets
/// trusted integration code move these scalar facts between portable crates;
/// it is not an unforgeable authorization capability against linked Rust code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchProvenance {
    credential_id: [u8; 16],
    credential_generation: u64,
    authority_revision: u64,
    policy_version: u32,
}

/// Invalid device-owned facts supplied for dispatch provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchProvenanceError {
    /// The all-zero credential identifier is reserved for erased state.
    ZeroCredentialId,
    /// Credential generation zero is reserved for erased state.
    ZeroCredentialGeneration,
    /// Authority revision zero is reserved for erased state.
    ZeroAuthorityRevision,
    /// Authorization-policy version zero is reserved for erased state.
    ZeroPolicyVersion,
    /// A credential generation cannot originate after the observed authority.
    GenerationExceedsAuthorityRevision {
        /// Candidate credential generation.
        credential_generation: u64,
        /// Candidate complete-authority revision.
        authority_revision: u64,
    },
}

impl DispatchProvenance {
    /// Validate and construct provenance from credential-authority state.
    pub const fn new(
        credential_id: [u8; 16],
        credential_generation: u64,
        authority_revision: u64,
        policy_version: u32,
    ) -> Result<Self, DispatchProvenanceError> {
        let mut byte = 0;
        let mut has_nonzero_id_byte = false;
        while byte < credential_id.len() {
            if credential_id[byte] != 0 {
                has_nonzero_id_byte = true;
                break;
            }
            byte += 1;
        }
        if !has_nonzero_id_byte {
            return Err(DispatchProvenanceError::ZeroCredentialId);
        }
        if credential_generation == 0 {
            return Err(DispatchProvenanceError::ZeroCredentialGeneration);
        }
        if authority_revision == 0 {
            return Err(DispatchProvenanceError::ZeroAuthorityRevision);
        }
        if policy_version == 0 {
            return Err(DispatchProvenanceError::ZeroPolicyVersion);
        }
        if credential_generation > authority_revision {
            return Err(
                DispatchProvenanceError::GenerationExceedsAuthorityRevision {
                    credential_generation,
                    authority_revision,
                },
            );
        }
        Ok(Self {
            credential_id,
            credential_generation,
            authority_revision,
            policy_version,
        })
    }

    /// Opaque identifier of the credential revalidated for this attempt.
    pub const fn credential_id(self) -> [u8; 16] {
        self.credential_id
    }

    /// Exact credential generation revalidated for this attempt.
    pub const fn credential_generation(self) -> u64 {
        self.credential_generation
    }

    /// Complete credential-authority revision observed at revalidation.
    pub const fn authority_revision(self) -> u64 {
        self.authority_revision
    }

    /// Authorization-policy version applied by the credential record.
    pub const fn policy_version(self) -> u32 {
        self.policy_version
    }
}

/// Trusted authentication and authorization facts supplied out of band.
#[derive(Debug, Eq, PartialEq)]
pub struct DispatchContext {
    /// Principal derived from device-owned authenticated credential state, if any.
    principal: Option<PrincipalId>,
    /// Permissions granted to that authenticated principal.
    permissions: Permissions,
    /// Credential-authority facts captured by exact pre-dispatch revalidation.
    provenance: Option<DispatchProvenance>,
}

impl DispatchContext {
    /// Context for a connection without an authenticated application session.
    pub const UNAUTHENTICATED: Self = Self {
        principal: None,
        permissions: Permissions::NONE,
        provenance: None,
    };

    /// Construct a trusted context for an authenticated principal.
    pub const fn authenticated(
        principal: PrincipalId,
        permissions: Permissions,
        provenance: DispatchProvenance,
    ) -> Self {
        Self {
            principal: Some(principal),
            permissions,
            provenance: Some(provenance),
        }
    }

    /// Device-owned authenticated principal, if this context has one.
    pub const fn principal(&self) -> Option<PrincipalId> {
        self.principal
    }

    /// Device-owned permissions bound to this dispatch attempt.
    pub const fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Credential-authority facts for this authenticated attempt, if any.
    pub const fn provenance(&self) -> Option<DispatchProvenance> {
        self.provenance
    }
}

/// Permission category required by an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredPermission {
    /// Read submission status.
    ReadSubmissionStatus,
    /// Submit outbound RNS DATA through the unstable transport-neutral path.
    #[cfg(feature = "experimental-rns-data")]
    ExperimentalSubmitRnsData,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorizationRequirement {
    Public,
    #[cfg(feature = "experimental-rns-inbox")]
    Authenticated,
    Permission(RequiredPermission),
}

impl RequiredPermission {
    const fn bits(self) -> Permissions {
        match self {
            Self::ReadSubmissionStatus => Permissions::READ_SUBMISSION_STATUS,
            #[cfg(feature = "experimental-rns-data")]
            Self::ExperimentalSubmitRnsData => Permissions::EXPERIMENTAL_SUBMIT_RNS_DATA,
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
    /// Read the node's public primary Reticulum destination.
    IdentitySummary,
    /// Read status for a previously accepted submission.
    SubmissionStatus {
        /// Device-assigned submission identifier.
        id: SubmissionId,
    },
    /// Read bounded runtime state for the inbound RNS DATA mailbox.
    #[cfg(feature = "experimental-rns-inbox")]
    RnsInboxStatus,
    /// Read the oldest inbound RNS DATA item without consuming it.
    #[cfg(feature = "experimental-rns-inbox")]
    RnsInboxPeek,
    /// Durably submit outbound RNS DATA without selecting a physical transport.
    #[cfg(feature = "experimental-rns-data")]
    SubmitRnsData {
        /// Complete Reticulum destination hash.
        destination: DestinationHash,
        /// Borrowed application data; never allocated or copied by decoding.
        payload: &'a [u8],
        /// Deduplication key scoped by the authenticated principal and content.
        idempotency_key: IdempotencyKey,
    },
    /// Uninhabited marker keeping the decode lifetime stable without the
    /// experimental RNS DATA operation.
    #[doc(hidden)]
    __Borrowed(Infallible, PhantomData<&'a [u8]>),
}

impl DeviceRequest<'_> {
    /// Stable or experimental operation number encoded on the wire.
    pub const fn operation(&self) -> u16 {
        match self {
            Self::SystemCapabilities => OP_SYSTEM_CAPABILITIES,
            Self::IdentitySummary => OP_IDENTITY_SUMMARY,
            Self::SubmissionStatus { .. } => OP_SUBMISSION_STATUS,
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxStatus => OP_EXPERIMENTAL_RNS_INBOX_STATUS,
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxPeek => OP_EXPERIMENTAL_RNS_INBOX_PEEK,
            #[cfg(feature = "experimental-rns-data")]
            Self::SubmitRnsData { .. } => OP_EXPERIMENTAL_SUBMIT_RNS_DATA,
            Self::__Borrowed(never, _) => match *never {},
        }
    }

    /// Whether this operation can change node state.
    pub const fn is_mutating(&self) -> bool {
        match self {
            Self::SystemCapabilities | Self::IdentitySummary | Self::SubmissionStatus { .. } => {
                false
            }
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxStatus | Self::RnsInboxPeek => false,
            #[cfg(feature = "experimental-rns-data")]
            Self::SubmitRnsData { .. } => true,
            Self::__Borrowed(never, _) => match *never {},
        }
    }

    const fn authorization_requirement(&self) -> AuthorizationRequirement {
        match self {
            Self::SystemCapabilities | Self::IdentitySummary => AuthorizationRequirement::Public,
            Self::SubmissionStatus { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::ReadSubmissionStatus)
            }
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxStatus | Self::RnsInboxPeek => AuthorizationRequirement::Authenticated,
            #[cfg(feature = "experimental-rns-data")]
            Self::SubmitRnsData { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::ExperimentalSubmitRnsData)
            }
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
    context: &DispatchContext,
    request: &DeviceRequest<'_>,
) -> Result<(), AuthorizationError> {
    match request.authorization_requirement() {
        AuthorizationRequirement::Public => Ok(()),
        #[cfg(feature = "experimental-rns-inbox")]
        AuthorizationRequirement::Authenticated => {
            if context.principal.is_some() {
                Ok(())
            } else {
                Err(AuthorizationError::AuthenticationRequired)
            }
        }
        AuthorizationRequirement::Permission(required) => {
            if context.principal.is_none() {
                return Err(AuthorizationError::AuthenticationRequired);
            }
            if !context.permissions.contains(required.bits()) {
                return Err(AuthorizationError::PermissionDenied(required));
            }
            Ok(())
        }
    }
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
    /// Availability of raw/direct physical-radio transmission to local clients.
    pub(crate) direct_radio_tx: CapabilityAvailability,
    /// Whether this snapshot advertises transport-neutral outbound RNS DATA submission.
    pub(crate) experimental_submit_rns_data: bool,
    /// Hard maximum logical CBOR message size.
    pub(crate) max_message_bytes: u16,
    /// Hard maximum encoded operation-body size.
    pub(crate) max_body_bytes: u16,
    /// Maximum experimental submission payload.
    pub(crate) max_submit_rns_data_payload_bytes: u16,
    /// Runtime availability of the experimental inbound RNS DATA mailbox.
    pub(crate) experimental_rns_inbox: CapabilityAvailability,
    /// Maximum payload returned by one experimental inbox item.
    pub(crate) max_rns_inbox_payload_bytes: u16,
}

impl CapabilitySnapshot {
    /// Snapshot for this crate's current build.
    ///
    /// Packet output and direct-radio TX remain deliberately unavailable in
    /// every feature composition. Outbound RNS submission is a separate,
    /// transport-neutral capability.
    pub const fn current() -> Self {
        Self {
            api_version: ApiVersion::CURRENT,
            packet_output: false,
            direct_radio_tx: CapabilityAvailability::Unavailable,
            experimental_submit_rns_data: cfg!(feature = "experimental-rns-data"),
            max_message_bytes: MAX_MESSAGE_BYTES as u16,
            max_body_bytes: MAX_BODY_BYTES as u16,
            max_submit_rns_data_payload_bytes: MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES as u16,
            experimental_rns_inbox: if cfg!(feature = "experimental-rns-inbox") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_rns_inbox_payload_bytes: if cfg!(feature = "experimental-rns-inbox") {
                383
            } else {
                0
            },
        }
    }

    /// Snapshot restricted to operations implemented by a higher dispatch layer.
    ///
    /// `experimental_submit_rns_data` can disable the codec-build capability,
    /// but cannot enable an operation omitted from this crate's build. This
    /// keeps Cargo feature unification in another dependency edge from making a
    /// dispatcher advertise an operation that it did not compile locally.
    pub const fn for_dispatch(experimental_submit_rns_data: bool) -> Self {
        let mut snapshot = Self::current();
        snapshot.experimental_submit_rns_data &= experimental_submit_rns_data;
        snapshot.experimental_rns_inbox = CapabilityAvailability::Unavailable;
        snapshot.max_rns_inbox_payload_bytes = 0;
        snapshot
    }

    /// Snapshot restricted to submission and inbox operations implemented by a dispatcher.
    pub const fn for_dispatch_with_inbox(
        experimental_submit_rns_data: bool,
        experimental_rns_inbox: CapabilityAvailability,
    ) -> Self {
        let mut snapshot = Self::current();
        snapshot.experimental_submit_rns_data &= experimental_submit_rns_data;
        if cfg!(feature = "experimental-rns-inbox") {
            snapshot.experimental_rns_inbox = experimental_rns_inbox;
            snapshot.max_rns_inbox_payload_bytes =
                if matches!(experimental_rns_inbox, CapabilityAvailability::Unavailable) {
                    0
                } else {
                    383
                };
        } else {
            snapshot.experimental_rns_inbox = CapabilityAvailability::Unavailable;
            snapshot.max_rns_inbox_payload_bytes = 0;
        }
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

    /// Availability of raw/direct physical-radio transmission to local clients.
    ///
    /// This does not describe transport-neutral RNS submission, which may
    /// route over LoRa or another enabled Reticulum interface.
    pub const fn direct_radio_tx(self) -> CapabilityAvailability {
        self.direct_radio_tx
    }

    /// Whether this snapshot advertises transport-neutral RNS DATA submission.
    pub const fn experimental_submit_rns_data(self) -> bool {
        self.experimental_submit_rns_data
    }

    /// Hard maximum logical CBOR message size.
    pub const fn max_message_bytes(self) -> u16 {
        self.max_message_bytes
    }

    /// Hard maximum encoded operation-body size.
    pub const fn max_body_bytes(self) -> u16 {
        self.max_body_bytes
    }

    /// Maximum experimental submission payload.
    pub const fn max_submit_rns_data_payload_bytes(self) -> u16 {
        self.max_submit_rns_data_payload_bytes
    }

    /// Runtime availability of the experimental inbound RNS DATA mailbox.
    pub const fn experimental_rns_inbox(self) -> CapabilityAvailability {
        self.experimental_rns_inbox
    }

    /// Maximum payload returned by one experimental inbox item.
    pub const fn max_rns_inbox_payload_bytes(self) -> u16 {
        self.max_rns_inbox_payload_bytes
    }
}

/// Bounded runtime state of the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnsInboxStatus {
    /// Number of items currently retained.
    pub depth: u16,
    /// Maximum retained item count.
    pub capacity: u16,
    /// Number of inbound items dropped since this boot.
    pub dropped_since_boot: u64,
    /// Maximum payload length accepted by this mailbox instance.
    pub max_payload_bytes: u16,
    /// Whether retained items survive reboot.
    pub durable: bool,
}

/// An inbound RNS DATA payload exceeded the fixed logical API limit.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RnsInboxPayloadTooLarge {
    actual: usize,
}

#[cfg(feature = "experimental-rns-inbox")]
impl RnsInboxPayloadTooLarge {
    /// Rejected payload length.
    pub const fn actual(self) -> usize {
        self.actual
    }

    /// Maximum accepted payload length.
    pub const fn maximum(self) -> usize {
        MAX_RNS_INBOX_PAYLOAD_BYTES
    }
}

/// One complete owned item returned by the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RnsInboxItem {
    id: NonZeroU64,
    destination: DestinationHash,
    payload_len: u16,
    payload: [u8; MAX_RNS_INBOX_PAYLOAD_BYTES],
}

#[cfg(feature = "experimental-rns-inbox")]
impl RnsInboxItem {
    /// Copy one bounded payload into a fixed-capacity response owner.
    pub fn new(
        id: NonZeroU64,
        destination: DestinationHash,
        payload: &[u8],
    ) -> Result<Self, RnsInboxPayloadTooLarge> {
        if payload.len() > MAX_RNS_INBOX_PAYLOAD_BYTES {
            return Err(RnsInboxPayloadTooLarge {
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

    /// Local Reticulum destination that received this item.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Exact payload bytes, excluding unused fixed-buffer capacity.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len as usize]
    }

    /// Valid payload length.
    pub const fn payload_len(&self) -> u16 {
        self.payload_len
    }
}

#[cfg(feature = "experimental-rns-inbox")]
impl core::fmt::Debug for RnsInboxItem {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RnsInboxItem")
            .field("id", &self.id)
            .field("destination", &self.destination)
            .field("payload_len", &self.payload_len)
            .finish_non_exhaustive()
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

/// Acceptance result for the experimental outbound RNS DATA submission.
///
/// The response contains only the device-assigned identifier used with
/// `submission.status`; it never contains prepared packet bytes.
#[cfg(feature = "experimental-rns-data")]
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
// The inbox variant deliberately owns its fixed-capacity payload so response
// dispatch remains allocation-free and cannot retain a mailbox borrow.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeviceResponse {
    /// Result of `system.capabilities`.
    SystemCapabilities(CapabilitySnapshot),
    /// Result of `identity.summary`.
    IdentitySummary(IdentitySummary),
    /// Result of `submission.status`.
    SubmissionStatus(SubmissionStatus),
    /// Result of `experimental.rns_inbox.status`.
    #[cfg(feature = "experimental-rns-inbox")]
    RnsInboxStatus(RnsInboxStatus),
    /// Occupied result of `experimental.rns_inbox.peek`.
    #[cfg(feature = "experimental-rns-inbox")]
    RnsInboxPeek(RnsInboxItem),
    /// Accepted experimental outbound RNS DATA submission.
    #[cfg(feature = "experimental-rns-data")]
    SubmitRnsDataAccepted(SubmissionAccepted),
    /// Typed request failure.
    Error(ApiErrorResponse),
}

impl DeviceResponse {
    /// Operation or response-kind number encoded on the wire.
    pub const fn kind(&self) -> u16 {
        match self {
            Self::SystemCapabilities(_) => OP_SYSTEM_CAPABILITIES,
            Self::IdentitySummary(_) => OP_IDENTITY_SUMMARY,
            Self::SubmissionStatus(_) => OP_SUBMISSION_STATUS,
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxStatus(_) => OP_EXPERIMENTAL_RNS_INBOX_STATUS,
            #[cfg(feature = "experimental-rns-inbox")]
            Self::RnsInboxPeek(_) => OP_EXPERIMENTAL_RNS_INBOX_PEEK,
            #[cfg(feature = "experimental-rns-data")]
            Self::SubmitRnsDataAccepted(_) => OP_EXPERIMENTAL_SUBMIT_RNS_DATA,
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
