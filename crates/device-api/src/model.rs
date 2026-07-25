//! Rete-independent logical API model and authorization vocabulary.

use core::{convert::Infallible, marker::PhantomData, ops::BitOr};

#[cfg(any(feature = "experimental-rns-inbox", feature = "experimental-lxmf"))]
use core::num::NonZeroU64;

/// Device API v1 major version.
pub const API_VERSION_MAJOR: u16 = 1;
/// Device API v1 revision adding bounded NomadNet page fetch.
pub const API_VERSION_MINOR: u16 = 6;

/// Maximum size of one decoded or encoded logical CBOR message.
pub const MAX_MESSAGE_BYTES: usize = 512;
/// Maximum encoded size of the operation-specific body within a message.
pub const MAX_BODY_BYTES: usize = 448;
/// Maximum payload accepted by the experimental RNS DATA submission request.
pub const MAX_SUBMIT_RNS_DATA_PAYLOAD_BYTES: usize = 383;
/// Maximum payload returned by the experimental inbound RNS DATA mailbox.
#[cfg(feature = "experimental-rns-inbox")]
pub const MAX_RNS_INBOX_PAYLOAD_BYTES: usize = 383;
/// Maximum exact normalized LXMF wire bytes returned by one read response.
#[cfg(feature = "experimental-lxmf")]
pub const MAX_LXMF_READ_CHUNK_BYTES: usize = 416;
/// Structural per-field title limit accepted by the basic-LXMF codec.
///
/// The encoded body and product composer can impose a lower limit on a
/// particular title/content combination.
pub const MAX_LXMF_BASIC_TITLE_BYTES: usize = 295;
/// Structural per-field content limit accepted by the basic-LXMF codec.
///
/// The encoded body and product composer can impose a lower limit on a
/// particular title/content combination.
pub const MAX_LXMF_BASIC_CONTENT_BYTES: usize = 295;
/// Maximum authenticated announce application data returned for one nearby LXMF peer.
pub const MAX_LXMF_PEER_APP_DATA_BYTES: usize = 256;
/// Largest UTF-8 NomadNet request path accepted by the experimental fetch API.
pub const MAX_NOMAD_PAGE_PATH_BYTES: usize = 128;
/// Largest valid UTF-8 Micron page returned by the experimental fetch API.
pub const MAX_NOMAD_PAGE_BYTES: usize = 400;
/// Largest JavaScript-safe whole-millisecond request timestamp.
///
/// Converting extreme accepted values to binary64 seconds can lose
/// millisecond precision. Contemporary Unix dates retain millisecond
/// precision; the wire bound promises integer interchange, not exact
/// binary64 spacing across the complete range.
pub const MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS: u64 = (1_u64 << 53) - 1;

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
/// Read the next committed LXMF message summary after an optional stable handle.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_NEXT: u16 = 0xf004;
/// Read one bounded chunk of a committed normalized LXMF wire message.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_READ: u16 = 0xf005;
/// Compose and durably submit one basic LXMF message through the local identity.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_BASIC_SEND: u16 = 0xf006;
/// Read one bounded nearby `lxmf.delivery` peer after an optional boot-scoped cursor.
#[cfg(feature = "experimental-lxmf")]
pub const OP_EXPERIMENTAL_LXMF_PEER_NEXT: u16 = 0xf007;
/// Begin one bounded authenticated NomadNet page fetch.
#[cfg(feature = "experimental-nomad")]
pub const OP_EXPERIMENTAL_NOMAD_FETCH_START: u16 = 0xf008;
/// Poll one principal-owned bounded NomadNet page fetch.
#[cfg(feature = "experimental-nomad")]
pub const OP_EXPERIMENTAL_NOMAD_FETCH_POLL: u16 = 0xf009;

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

/// Validated borrowed UTF-8 NomadNet request path.
///
/// The path is absolute, contains no NUL byte, and remains borrowed directly
/// from the decoded request message.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NomadPagePath<'a>(&'a str);

#[cfg(feature = "experimental-nomad")]
impl<'a> NomadPagePath<'a> {
    /// Validate one bounded absolute NomadNet path.
    pub fn new(path: &'a str) -> Result<Self, InvalidNomadPagePath> {
        let bytes = path.as_bytes();
        if bytes.is_empty() || bytes[0] != b'/' || bytes.contains(&0) {
            return Err(InvalidNomadPagePath::Invalid);
        }
        if bytes.len() > MAX_NOMAD_PAGE_PATH_BYTES {
            return Err(InvalidNomadPagePath::TooLong {
                actual: bytes.len(),
            });
        }
        Ok(Self(path))
    }

    /// Borrow the complete validated path.
    pub const fn as_str(self) -> &'a str {
        self.0
    }

    /// Path length in UTF-8 bytes.
    pub const fn len(self) -> usize {
        self.0.len()
    }

    /// Whether the path is empty.
    ///
    /// A constructed path is never empty; this method supports conventional
    /// collection-style inspection.
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }
}

/// Why a NomadNet request path was rejected.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidNomadPagePath {
    /// The path was empty, relative, or contained a NUL byte.
    Invalid,
    /// The path exceeded the fixed UTF-8 byte limit.
    TooLong {
        /// Rejected path length in bytes.
        actual: usize,
    },
}

#[cfg(feature = "experimental-nomad")]
impl InvalidNomadPagePath {
    /// Largest accepted UTF-8 path length.
    pub const fn maximum(self) -> usize {
        MAX_NOMAD_PAGE_PATH_BYTES
    }
}

/// Caller-selected Unix timestamp for one anonymous NomadNet request.
///
/// The inclusive range is lossless in JSON and JavaScript integer
/// interchange. Conversion to the Reticulum binary64-seconds wire timestamp
/// can lose millisecond precision at extreme dates.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NomadRequestTimestampUnixMs(u64);

#[cfg(feature = "experimental-nomad")]
impl NomadRequestTimestampUnixMs {
    /// Validate a nonzero whole-millisecond Unix timestamp.
    pub const fn new(value: u64) -> Result<Self, InvalidNomadRequestTimestamp> {
        if value == 0 || value > MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS {
            Err(InvalidNomadRequestTimestamp { actual: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Complete validated millisecond value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A NomadNet request timestamp was zero or outside the exact millisecond range.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNomadRequestTimestamp {
    actual: u64,
}

#[cfg(feature = "experimental-nomad")]
impl InvalidNomadRequestTimestamp {
    /// Rejected millisecond value.
    pub const fn actual(self) -> u64 {
        self.actual
    }

    /// Largest accepted millisecond value.
    pub const fn maximum(self) -> u64 {
        MAX_NOMAD_REQUEST_TIMESTAMP_UNIX_MS
    }
}

/// Opaque boot-scoped identifier for one principal-owned NomadNet fetch.
///
/// The first eight bytes identify the boot incarnation. The final eight bytes
/// contain a nonzero big-endian sequence. Clients compare and return all 16
/// bytes without deriving authority from either component.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NomadFetchId([u8; 16]);

#[cfg(feature = "experimental-nomad")]
impl NomadFetchId {
    /// Construct a boot-scoped identifier from its two exact components.
    pub const fn new(incarnation: [u8; 8], sequence: u64) -> Result<Self, InvalidNomadFetchId> {
        if sequence == 0 {
            return Err(InvalidNomadFetchId);
        }
        let sequence = sequence.to_be_bytes();
        Ok(Self([
            incarnation[0],
            incarnation[1],
            incarnation[2],
            incarnation[3],
            incarnation[4],
            incarnation[5],
            incarnation[6],
            incarnation[7],
            sequence[0],
            sequence[1],
            sequence[2],
            sequence[3],
            sequence[4],
            sequence[5],
            sequence[6],
            sequence[7],
        ]))
    }

    /// Validate all opaque bytes received from the wire.
    pub const fn from_bytes(bytes: [u8; 16]) -> Result<Self, InvalidNomadFetchId> {
        let sequence = u64::from_be_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]);
        if sequence == 0 {
            Err(InvalidNomadFetchId)
        } else {
            Ok(Self(bytes))
        }
    }

    /// Borrow all opaque identifier bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Boot-incarnation component.
    pub const fn incarnation(self) -> [u8; 8] {
        [
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5], self.0[6], self.0[7],
        ]
    }

    /// Nonzero sequence component.
    pub const fn sequence(self) -> u64 {
        u64::from_be_bytes([
            self.0[8], self.0[9], self.0[10], self.0[11], self.0[12], self.0[13], self.0[14],
            self.0[15],
        ])
    }
}

/// A fetch identifier's sequence component was zero.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNomadFetchId;

/// Complete borrowed request to begin one bounded NomadNet page fetch.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchStartRequest<'a> {
    destination: DestinationHash,
    path: NomadPagePath<'a>,
    timestamp_unix_ms: NomadRequestTimestampUnixMs,
    idempotency_key: IdempotencyKey,
}

#[cfg(feature = "experimental-nomad")]
impl<'a> NomadFetchStartRequest<'a> {
    /// Construct one invariant-preserving start request.
    pub const fn new(
        destination: DestinationHash,
        path: NomadPagePath<'a>,
        timestamp_unix_ms: NomadRequestTimestampUnixMs,
        idempotency_key: IdempotencyKey,
    ) -> Self {
        Self {
            destination,
            path,
            timestamp_unix_ms,
            idempotency_key,
        }
    }

    /// Complete remote `nomadnetwork.node` destination hash.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Exact validated remote page path.
    pub const fn path(self) -> NomadPagePath<'a> {
        self.path
    }

    /// Caller-selected request timestamp.
    pub const fn timestamp_unix_ms(self) -> NomadRequestTimestampUnixMs {
        self.timestamp_unix_ms
    }

    /// Principal-scoped idempotency key.
    pub const fn idempotency_key(self) -> IdempotencyKey {
        self.idempotency_key
    }
}

/// Request to poll one principal-owned NomadNet fetch.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchPollRequest {
    /// Device-assigned fetch identifier.
    pub id: NomadFetchId,
}

/// Public, copy-only summary of the node's Reticulum destinations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentitySummary {
    /// Primary destination served by the node.
    primary_destination: DestinationHash,
    /// Optional local `lxmf.delivery` destination served by the node.
    lxmf_delivery_destination: Option<DestinationHash>,
}

impl IdentitySummary {
    /// Construct the public summary for a node's primary destination.
    pub const fn new(primary_destination: DestinationHash) -> Self {
        Self {
            primary_destination,
            lxmf_delivery_destination: None,
        }
    }

    /// Construct a summary that also advertises the local `lxmf.delivery` destination.
    pub const fn with_lxmf_delivery_destination(
        primary_destination: DestinationHash,
        lxmf_delivery_destination: DestinationHash,
    ) -> Self {
        Self {
            primary_destination,
            lxmf_delivery_destination: Some(lxmf_delivery_destination),
        }
    }

    /// Primary destination served by the node.
    pub const fn primary_destination(self) -> DestinationHash {
        self.primary_destination
    }

    /// Local `lxmf.delivery` destination when that service is active.
    pub const fn lxmf_delivery_destination(self) -> Option<DestinationHash> {
        self.lxmf_delivery_destination
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
    #[cfg(any(feature = "experimental-rns-data", feature = "experimental-lxmf"))]
    ExperimentalSubmitRnsData,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorizationRequirement {
    Public,
    #[cfg(any(
        feature = "experimental-rns-inbox",
        feature = "experimental-lxmf",
        feature = "experimental-nomad"
    ))]
    Authenticated,
    Permission(RequiredPermission),
}

impl RequiredPermission {
    const fn bits(self) -> Permissions {
        match self {
            Self::ReadSubmissionStatus => Permissions::READ_SUBMISSION_STATUS,
            #[cfg(any(feature = "experimental-rns-data", feature = "experimental-lxmf"))]
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
    /// Read the next committed LXMF summary after an optional stable handle.
    #[cfg(feature = "experimental-lxmf")]
    LxmfNext {
        /// Exclusive physical-commit-order cursor; `None` selects the first message.
        after: Option<LxmfMessageHandle>,
    },
    /// Read one bounded chunk of an exact committed normalized LXMF wire message.
    #[cfg(feature = "experimental-lxmf")]
    LxmfRead {
        /// Stable committed-message handle.
        handle: LxmfMessageHandle,
        /// Zero-based byte offset in the normalized wire representation.
        offset: u32,
        /// Maximum bytes requested in this response.
        max_bytes: LxmfReadLength,
    },
    /// Compose and durably submit a basic LXMF message using the device-owned source.
    ///
    /// Empty title and content values are valid. The codec bounds fields and
    /// message size; product composition applies its additional carrier rules.
    #[cfg(feature = "experimental-lxmf")]
    LxmfBasicSend {
        /// Complete remote `lxmf.delivery` destination hash.
        destination: DestinationHash,
        /// Caller-selected Unix timestamp in milliseconds.
        ///
        /// The bearer-neutral codec accepts `u64`; the current product composer
        /// accepts exactly `1..=8_796_093_022_207_999`.
        timestamp_unix_ms: u64,
        /// Borrowed binary title; interpretation belongs to the LXMF application.
        ///
        /// The codec's 295-byte field bound is not a guarantee that every
        /// title/content combination fits the encoded body or product carrier.
        title: &'a [u8],
        /// Borrowed binary content; interpretation belongs to the LXMF application.
        ///
        /// The codec's 295-byte field bound is not a guarantee that every
        /// title/content combination fits the encoded body or product carrier.
        content: &'a [u8],
        /// Deduplication key scoped by the authenticated principal and composed message.
        idempotency_key: IdempotencyKey,
    },
    /// Read one nearby `lxmf.delivery` peer from the volatile bounded projection.
    #[cfg(feature = "experimental-lxmf")]
    LxmfPeerNext {
        /// Optional exclusive cursor scoped to one device boot/incarnation.
        ///
        /// `None` starts from the oldest retained record. The wire requires
        /// both cursor fields together, preventing ambiguous partial cursors.
        after: Option<LxmfPeerDiscoveryCursor>,
    },
    /// Begin one authenticated bounded NomadNet page fetch.
    #[cfg(feature = "experimental-nomad")]
    NomadFetchStart(NomadFetchStartRequest<'a>),
    /// Poll one authenticated principal-owned NomadNet page fetch.
    #[cfg(feature = "experimental-nomad")]
    NomadFetchPoll(NomadFetchPollRequest),
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
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfNext { .. } => OP_EXPERIMENTAL_LXMF_NEXT,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfRead { .. } => OP_EXPERIMENTAL_LXMF_READ,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfBasicSend { .. } => OP_EXPERIMENTAL_LXMF_BASIC_SEND,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfPeerNext { .. } => OP_EXPERIMENTAL_LXMF_PEER_NEXT,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchStart(_) => OP_EXPERIMENTAL_NOMAD_FETCH_START,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchPoll(_) => OP_EXPERIMENTAL_NOMAD_FETCH_POLL,
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
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfNext { .. } | Self::LxmfRead { .. } | Self::LxmfPeerNext { .. } => false,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfBasicSend { .. } => true,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchStart(_) => true,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchPoll(_) => false,
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
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfNext { .. } | Self::LxmfRead { .. } | Self::LxmfPeerNext { .. } => {
                AuthorizationRequirement::Authenticated
            }
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchStart(_) | Self::NomadFetchPoll(_) => {
                AuthorizationRequirement::Authenticated
            }
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfBasicSend { .. } => {
                AuthorizationRequirement::Permission(RequiredPermission::ExperimentalSubmitRnsData)
            }
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
        #[cfg(any(
            feature = "experimental-rns-inbox",
            feature = "experimental-lxmf",
            feature = "experimental-nomad"
        ))]
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
    /// Runtime availability of committed LXMF discovery and bounded reads.
    pub(crate) experimental_lxmf: CapabilityAvailability,
    /// Maximum exact normalized wire bytes returned by one LXMF read.
    pub(crate) max_lxmf_read_chunk_bytes: u16,
    /// Runtime availability of source-free basic LXMF composition and submission.
    pub(crate) experimental_lxmf_basic_send: CapabilityAvailability,
    /// Structural per-field title limit advertised by the logical codec.
    ///
    /// Product composition and carrier limits can reduce the accepted
    /// title/content combination.
    pub(crate) max_lxmf_basic_title_bytes: u16,
    /// Structural per-field content limit advertised by the logical codec.
    ///
    /// Product composition and carrier limits can reduce the accepted
    /// title/content combination.
    pub(crate) max_lxmf_basic_content_bytes: u16,
    /// Runtime availability of bounded nearby `lxmf.delivery` peer discovery.
    pub(crate) experimental_lxmf_peer_discovery: CapabilityAvailability,
    /// Maximum authenticated announce application data returned with one peer.
    pub(crate) max_lxmf_peer_app_data_bytes: u16,
    /// Runtime availability of bounded authenticated NomadNet page fetch.
    pub(crate) experimental_nomad: CapabilityAvailability,
    /// Maximum UTF-8 request-path bytes accepted by NomadNet fetch.
    pub(crate) max_nomad_page_path_bytes: u16,
    /// Maximum valid UTF-8 Micron page bytes returned by NomadNet fetch.
    pub(crate) max_nomad_page_bytes: u16,
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
            experimental_lxmf: if cfg!(feature = "experimental-lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_read_chunk_bytes: if cfg!(feature = "experimental-lxmf") {
                416
            } else {
                0
            },
            experimental_lxmf_basic_send: if cfg!(feature = "experimental-lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_basic_title_bytes: if cfg!(feature = "experimental-lxmf") {
                MAX_LXMF_BASIC_TITLE_BYTES as u16
            } else {
                0
            },
            max_lxmf_basic_content_bytes: if cfg!(feature = "experimental-lxmf") {
                MAX_LXMF_BASIC_CONTENT_BYTES as u16
            } else {
                0
            },
            experimental_lxmf_peer_discovery: if cfg!(feature = "experimental-lxmf") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_lxmf_peer_app_data_bytes: if cfg!(feature = "experimental-lxmf") {
                MAX_LXMF_PEER_APP_DATA_BYTES as u16
            } else {
                0
            },
            experimental_nomad: if cfg!(feature = "experimental-nomad") {
                CapabilityAvailability::Available
            } else {
                CapabilityAvailability::Unavailable
            },
            max_nomad_page_path_bytes: if cfg!(feature = "experimental-nomad") {
                MAX_NOMAD_PAGE_PATH_BYTES as u16
            } else {
                0
            },
            max_nomad_page_bytes: if cfg!(feature = "experimental-nomad") {
                MAX_NOMAD_PAGE_BYTES as u16
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
        snapshot.experimental_lxmf = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_read_chunk_bytes = 0;
        snapshot.experimental_lxmf_basic_send = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_basic_title_bytes = 0;
        snapshot.max_lxmf_basic_content_bytes = 0;
        snapshot.experimental_lxmf_peer_discovery = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_peer_app_data_bytes = 0;
        snapshot.experimental_nomad = CapabilityAvailability::Unavailable;
        snapshot.max_nomad_page_path_bytes = 0;
        snapshot.max_nomad_page_bytes = 0;
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
        snapshot.experimental_lxmf = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_read_chunk_bytes = 0;
        snapshot.experimental_lxmf_basic_send = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_basic_title_bytes = 0;
        snapshot.max_lxmf_basic_content_bytes = 0;
        snapshot.experimental_lxmf_peer_discovery = CapabilityAvailability::Unavailable;
        snapshot.max_lxmf_peer_app_data_bytes = 0;
        snapshot.experimental_nomad = CapabilityAvailability::Unavailable;
        snapshot.max_nomad_page_path_bytes = 0;
        snapshot.max_nomad_page_bytes = 0;
        snapshot
    }

    /// Restrict submission and NomadNet fetch to two independent dispatcher ports.
    pub const fn for_dispatch_with_nomad(
        experimental_submit_rns_data: bool,
        experimental_nomad: CapabilityAvailability,
    ) -> Self {
        Self::for_dispatch(experimental_submit_rns_data).with_dispatch_nomad(experimental_nomad)
    }

    /// Add the independently owned NomadNet port to an existing dispatcher snapshot.
    ///
    /// This preserves every capability already selected by the higher
    /// dispatcher. It cannot enable NomadNet fetch when that codec feature was
    /// omitted from this crate's build.
    pub const fn with_dispatch_nomad(mut self, experimental_nomad: CapabilityAvailability) -> Self {
        if cfg!(feature = "experimental-nomad") {
            self.experimental_nomad = experimental_nomad;
            let available = !matches!(experimental_nomad, CapabilityAvailability::Unavailable);
            self.max_nomad_page_path_bytes = if available {
                MAX_NOMAD_PAGE_PATH_BYTES as u16
            } else {
                0
            };
            self.max_nomad_page_bytes = if available {
                MAX_NOMAD_PAGE_BYTES as u16
            } else {
                0
            };
        }
        self
    }

    /// Restrict submission, raw-inbox, and LXMF capabilities to a higher dispatcher.
    pub const fn for_dispatch_with_inbox_and_lxmf(
        experimental_submit_rns_data: bool,
        experimental_rns_inbox: CapabilityAvailability,
        experimental_lxmf: CapabilityAvailability,
    ) -> Self {
        let mut snapshot =
            Self::for_dispatch_with_inbox(experimental_submit_rns_data, experimental_rns_inbox);
        if cfg!(feature = "experimental-lxmf") {
            snapshot.experimental_lxmf = experimental_lxmf;
            snapshot.max_lxmf_read_chunk_bytes =
                if matches!(experimental_lxmf, CapabilityAvailability::Unavailable) {
                    0
                } else {
                    416
                };
        }
        snapshot
    }

    /// Restrict submission, inbox, LXMF reads, and basic LXMF send to one dispatcher.
    pub const fn for_dispatch_with_inbox_lxmf_and_basic_send(
        experimental_submit_rns_data: bool,
        experimental_rns_inbox: CapabilityAvailability,
        experimental_lxmf: CapabilityAvailability,
        experimental_lxmf_basic_send: CapabilityAvailability,
    ) -> Self {
        let mut snapshot = Self::for_dispatch_with_inbox_and_lxmf(
            experimental_submit_rns_data,
            experimental_rns_inbox,
            experimental_lxmf,
        );
        if cfg!(feature = "experimental-lxmf") {
            snapshot.experimental_lxmf_basic_send = experimental_lxmf_basic_send;
            let available = !matches!(
                experimental_lxmf_basic_send,
                CapabilityAvailability::Unavailable
            );
            snapshot.max_lxmf_basic_title_bytes = if available {
                MAX_LXMF_BASIC_TITLE_BYTES as u16
            } else {
                0
            };
            snapshot.max_lxmf_basic_content_bytes = if available {
                MAX_LXMF_BASIC_CONTENT_BYTES as u16
            } else {
                0
            };
        }
        snapshot
    }

    /// Restrict submission, inbox, LXMF, send, and peer discovery to one dispatcher.
    #[allow(clippy::too_many_arguments)]
    pub const fn for_dispatch_with_inbox_lxmf_basic_send_and_peer_discovery(
        experimental_submit_rns_data: bool,
        experimental_rns_inbox: CapabilityAvailability,
        experimental_lxmf: CapabilityAvailability,
        experimental_lxmf_basic_send: CapabilityAvailability,
        experimental_lxmf_peer_discovery: CapabilityAvailability,
        max_lxmf_peer_app_data_bytes: u16,
    ) -> Self {
        let mut snapshot = Self::for_dispatch_with_inbox_lxmf_and_basic_send(
            experimental_submit_rns_data,
            experimental_rns_inbox,
            experimental_lxmf,
            experimental_lxmf_basic_send,
        );
        if cfg!(feature = "experimental-lxmf") {
            snapshot.experimental_lxmf_peer_discovery = experimental_lxmf_peer_discovery;
            snapshot.max_lxmf_peer_app_data_bytes = if matches!(
                experimental_lxmf_peer_discovery,
                CapabilityAvailability::Unavailable
            ) {
                0
            } else if max_lxmf_peer_app_data_bytes > MAX_LXMF_PEER_APP_DATA_BYTES as u16 {
                MAX_LXMF_PEER_APP_DATA_BYTES as u16
            } else {
                max_lxmf_peer_app_data_bytes
            };
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

    /// Runtime availability of committed LXMF discovery and bounded reads.
    pub const fn experimental_lxmf(self) -> CapabilityAvailability {
        self.experimental_lxmf
    }

    /// Maximum exact normalized wire bytes returned by one LXMF read.
    pub const fn max_lxmf_read_chunk_bytes(self) -> u16 {
        self.max_lxmf_read_chunk_bytes
    }

    /// Runtime availability of source-free basic LXMF composition and submission.
    pub const fn experimental_lxmf_basic_send(self) -> CapabilityAvailability {
        self.experimental_lxmf_basic_send
    }

    /// Structural codec limit for one source-free basic-LXMF title.
    ///
    /// The encoded-body and product-carrier limits can reject a smaller
    /// title/content combination.
    pub const fn max_lxmf_basic_title_bytes(self) -> u16 {
        self.max_lxmf_basic_title_bytes
    }

    /// Structural codec limit for one source-free basic-LXMF content value.
    ///
    /// The encoded-body and product-carrier limits can reject a smaller
    /// title/content combination.
    pub const fn max_lxmf_basic_content_bytes(self) -> u16 {
        self.max_lxmf_basic_content_bytes
    }

    /// Runtime availability of bounded nearby `lxmf.delivery` peer discovery.
    pub const fn experimental_lxmf_peer_discovery(self) -> CapabilityAvailability {
        self.experimental_lxmf_peer_discovery
    }

    /// Maximum authenticated announce application data returned with one peer.
    pub const fn max_lxmf_peer_app_data_bytes(self) -> u16 {
        self.max_lxmf_peer_app_data_bytes
    }

    /// Runtime availability of bounded authenticated NomadNet page fetch.
    pub const fn experimental_nomad(self) -> CapabilityAvailability {
        self.experimental_nomad
    }

    /// Maximum UTF-8 request-path bytes accepted by NomadNet fetch.
    pub const fn max_nomad_page_path_bytes(self) -> u16 {
        self.max_nomad_page_path_bytes
    }

    /// Maximum valid UTF-8 Micron page bytes returned by NomadNet fetch.
    pub const fn max_nomad_page_bytes(self) -> u16 {
        self.max_nomad_page_bytes
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

/// Stable non-zero handle for one committed inbound LXMF message.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfMessageHandle(NonZeroU64);

#[cfg(feature = "experimental-lxmf")]
impl LxmfMessageHandle {
    /// Construct a stable handle from its complete numeric representation.
    pub const fn new(value: u64) -> Result<Self, InvalidLxmfMessageHandle> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(InvalidLxmfMessageHandle),
        }
    }

    /// Complete numeric representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Zero cannot identify a committed LXMF message.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfMessageHandle;

/// Valid non-zero upper bound for one bounded LXMF read response.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfReadLength(u16);

#[cfg(feature = "experimental-lxmf")]
impl LxmfReadLength {
    /// Validate a requested response length against the frozen logical limit.
    pub const fn new(value: u16) -> Result<Self, InvalidLxmfReadLength> {
        if value == 0 || value as usize > MAX_LXMF_READ_CHUNK_BYTES {
            Err(InvalidLxmfReadLength { actual: value })
        } else {
            Ok(Self(value))
        }
    }

    /// Maximum bytes requested from the named offset.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A requested LXMF chunk length was zero or exceeded the logical limit.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfReadLength {
    actual: u16,
}

#[cfg(feature = "experimental-lxmf")]
impl InvalidLxmfReadLength {
    /// Rejected requested length.
    pub const fn actual(self) -> u16 {
        self.actual
    }

    /// Maximum accepted requested length.
    pub const fn maximum(self) -> u16 {
        MAX_LXMF_READ_CHUNK_BYTES as u16
    }
}

/// Opaque public token identifying one volatile peer-discovery incarnation.
///
/// The device changes this value whenever its boot-scoped discovery table is
/// reconstructed. It is not a credential or secret.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfPeerDiscoveryIncarnation([u8; 8]);

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerDiscoveryIncarnation {
    /// Construct a token from its complete public wire bytes.
    pub const fn new(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Borrow all public token bytes.
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

/// Exclusive peer-discovery cursor scoped to one device incarnation.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LxmfPeerDiscoveryCursor {
    incarnation: LxmfPeerDiscoveryIncarnation,
    after_generation: u64,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerDiscoveryCursor {
    /// Construct a complete boot-scoped exclusive cursor.
    pub const fn new(incarnation: LxmfPeerDiscoveryIncarnation, after_generation: u64) -> Self {
        Self {
            incarnation,
            after_generation,
        }
    }

    /// Incarnation in which the exclusive generation was observed.
    pub const fn incarnation(self) -> LxmfPeerDiscoveryIncarnation {
        self.incarnation
    }

    /// Exclusive observation generation; zero starts before all observations.
    pub const fn after_generation(self) -> u64 {
        self.after_generation
    }
}

/// Nonzero generation assigned to one retained peer's latest observation.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LxmfPeerGeneration(NonZeroU64);

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerGeneration {
    /// Construct a generation, rejecting the reserved pre-history value zero.
    pub const fn new(value: u64) -> Result<Self, InvalidLxmfPeerGeneration> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(InvalidLxmfPeerGeneration),
        }
    }

    /// Complete nonzero generation value.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Zero is reserved for the cursor before any peer observation.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfPeerGeneration;

/// Public 128-bit hash of the identity that authenticated an announce.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IdentityHash([u8; 16]);

#[cfg(feature = "experimental-lxmf")]
impl IdentityHash {
    /// Construct a public identity hash from all wire bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow all public hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// One retained, authenticated nearby `lxmf.delivery` announce observation.
///
/// This is display and contact-selection evidence, not routing authority.
/// The complete 64-byte Reticulum public key intentionally remains inside the
/// firmware's protocol owner and is not exposed by the local device API.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct LxmfDiscoveredPeer {
    destination: DestinationHash,
    identity_hash: IdentityHash,
    app_data_len: u16,
    app_data: [u8; MAX_LXMF_PEER_APP_DATA_BYTES],
    hops: u8,
    interface_id: u8,
    rssi_dbm: Option<i16>,
    snr_db: Option<i16>,
    observed_age_ms: u64,
    generation: LxmfPeerGeneration,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfDiscoveredPeer {
    /// Copy one bounded, already-authenticated discovery observation.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        destination: DestinationHash,
        identity_hash: IdentityHash,
        app_data: &[u8],
        hops: u8,
        interface_id: u8,
        rssi_dbm: Option<i16>,
        snr_db: Option<i16>,
        observed_age_ms: u64,
        generation: LxmfPeerGeneration,
    ) -> Result<Self, LxmfPeerAppDataTooLarge> {
        if app_data.len() > MAX_LXMF_PEER_APP_DATA_BYTES {
            return Err(LxmfPeerAppDataTooLarge {
                actual: app_data.len(),
            });
        }
        let mut owned = [0_u8; MAX_LXMF_PEER_APP_DATA_BYTES];
        owned[..app_data.len()].copy_from_slice(app_data);
        Ok(Self {
            destination,
            identity_hash,
            app_data_len: app_data.len() as u16,
            app_data: owned,
            hops,
            interface_id,
            rssi_dbm,
            snr_db,
            observed_age_ms,
            generation,
        })
    }

    /// Complete announced `lxmf.delivery` destination hash.
    pub const fn destination(&self) -> DestinationHash {
        self.destination
    }

    /// Public hash of the identity that authenticated the announce.
    pub const fn identity_hash(&self) -> IdentityHash {
        self.identity_hash
    }

    /// Exact authenticated announce application data.
    pub fn app_data(&self) -> &[u8] {
        &self.app_data[..self.app_data_len as usize]
    }

    /// Reticulum hop count reported for the latest observation.
    pub const fn hops(&self) -> u8 {
        self.hops
    }

    /// Product-owned scalar identifying the observing interface.
    pub const fn interface_id(&self) -> u8 {
        self.interface_id
    }

    /// Observed RSSI in whole dBm, when available.
    pub const fn rssi_dbm(&self) -> Option<i16> {
        self.rssi_dbm
    }

    /// Observed signal-to-noise ratio in whole dB, when available.
    pub const fn snr_db(&self) -> Option<i16> {
        self.snr_db
    }

    /// Saturating age in milliseconds at the response snapshot.
    pub const fn observed_age_ms(&self) -> u64 {
        self.observed_age_ms
    }

    /// Generation of the latest retained observation.
    pub const fn generation(&self) -> LxmfPeerGeneration {
        self.generation
    }
}

#[cfg(feature = "experimental-lxmf")]
impl core::fmt::Debug for LxmfDiscoveredPeer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LxmfDiscoveredPeer")
            .field("destination", &self.destination)
            .field("identity_hash", &self.identity_hash)
            .field("app_data_len", &self.app_data_len)
            .field("app_data", &"<redacted>")
            .field("hops", &self.hops)
            .field("interface_id", &self.interface_id)
            .field("rssi_dbm", &self.rssi_dbm)
            .field("snr_db", &self.snr_db)
            .field("observed_age_ms", &self.observed_age_ms)
            .field("generation", &self.generation)
            .finish()
    }
}

/// Authenticated announce application data exceeded the logical response bound.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfPeerAppDataTooLarge {
    actual: usize,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerAppDataTooLarge {
    /// Rejected application-data byte count.
    pub const fn actual(self) -> usize {
        self.actual
    }

    /// Maximum application-data byte count.
    pub const fn maximum(self) -> usize {
        MAX_LXMF_PEER_APP_DATA_BYTES
    }
}

/// One-record page from the volatile nearby-LXMF peer projection.
///
/// A changed incarnation or an ahead-of-history cursor resets the port to its
/// first retained record and sets `history_gap`. The returned next cursor is
/// always scoped to the current incarnation.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfPeerDiscoveryPage {
    next_cursor: LxmfPeerDiscoveryCursor,
    latest_generation: Option<LxmfPeerGeneration>,
    oldest_retained_generation: Option<LxmfPeerGeneration>,
    history_gap: bool,
    peer: Option<LxmfDiscoveredPeer>,
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfPeerDiscoveryPage {
    /// Construct one bounded projection page.
    pub const fn new(
        next_cursor: LxmfPeerDiscoveryCursor,
        latest_generation: Option<LxmfPeerGeneration>,
        oldest_retained_generation: Option<LxmfPeerGeneration>,
        history_gap: bool,
        peer: Option<LxmfDiscoveredPeer>,
    ) -> Self {
        Self {
            next_cursor,
            latest_generation,
            oldest_retained_generation,
            history_gap,
            peer,
        }
    }

    /// Exclusive cursor for the next read, scoped to the current incarnation.
    pub const fn next_cursor(&self) -> LxmfPeerDiscoveryCursor {
        self.next_cursor
    }

    /// Latest accepted observation generation when this page was produced.
    pub const fn latest_generation(&self) -> Option<LxmfPeerGeneration> {
        self.latest_generation
    }

    /// Oldest generation represented by a currently retained peer.
    pub const fn oldest_retained_generation(&self) -> Option<LxmfPeerGeneration> {
        self.oldest_retained_generation
    }

    /// Whether requested history was reset, updated away, or evicted.
    pub const fn history_gap(&self) -> bool {
        self.history_gap
    }

    /// One retained peer after the requested cursor, if present.
    pub const fn peer(&self) -> Option<&LxmfDiscoveredPeer> {
        self.peer.as_ref()
    }
}

/// Metadata-only physical-commit-order entry returned by `experimental.lxmf.next`.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfMessageSummary {
    handle: LxmfMessageHandle,
    message_id: [u8; 32],
    destination: DestinationHash,
    source: DestinationHash,
    timestamp_bits: u64,
    normalized_wire_len: u32,
    title_len: u32,
    content_len: u32,
    fields_encoded_len: u32,
    exact_wire_sha256: [u8; 32],
}

#[cfg(feature = "experimental-lxmf")]
const LXMF_NORMALIZED_WIRE_PREFIX_BYTES: u64 = 16 + 16 + 64;
#[cfg(feature = "experimental-lxmf")]
const LXMF_PAYLOAD_ARRAY_HEADER_BYTES: u64 = 1;
#[cfg(feature = "experimental-lxmf")]
const LXMF_TIMESTAMP_BYTES: u64 = 1 + 8;

#[cfg(feature = "experimental-lxmf")]
const fn minimum_msgpack_binary_bytes(decoded_len: u32) -> u64 {
    let header_len = if decoded_len <= u8::MAX as u32 {
        2
    } else if decoded_len <= u16::MAX as u32 {
        3
    } else {
        5
    };
    decoded_len as u64 + header_len
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfMessageSummary {
    /// Construct one complete committed-message summary.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        handle: LxmfMessageHandle,
        message_id: [u8; 32],
        destination: DestinationHash,
        source: DestinationHash,
        timestamp_bits: u64,
        normalized_wire_len: u32,
        title_len: u32,
        content_len: u32,
        fields_encoded_len: u32,
        exact_wire_sha256: [u8; 32],
    ) -> Result<Self, InvalidLxmfMessageSummary> {
        // A normalized LXMF wire is the destination, source and signature
        // prefix followed by a four- or five-item MessagePack array. The
        // shortest valid payload has a one-byte array header, one float64
        // timestamp, two binary values with their shortest possible length
        // prefixes, and the exact encoded fields map. A fifth stamp can only
        // increase this lower bound. Perform the sum in u64 so adversarial u32
        // component lengths cannot wrap into an apparently small message.
        let minimum_wire_len = LXMF_NORMALIZED_WIRE_PREFIX_BYTES
            + LXMF_PAYLOAD_ARRAY_HEADER_BYTES
            + LXMF_TIMESTAMP_BYTES
            + minimum_msgpack_binary_bytes(title_len)
            + minimum_msgpack_binary_bytes(content_len)
            + fields_encoded_len as u64;
        if fields_encoded_len == 0 || (normalized_wire_len as u64) < minimum_wire_len {
            return Err(InvalidLxmfMessageSummary);
        }
        Ok(Self {
            handle,
            message_id,
            destination,
            source,
            timestamp_bits,
            normalized_wire_len,
            title_len,
            content_len,
            fields_encoded_len,
            exact_wire_sha256,
        })
    }

    /// Stable committed-message handle.
    pub const fn handle(self) -> LxmfMessageHandle {
        self.handle
    }

    /// Python-compatible LXMF authenticated-message identifier.
    pub const fn message_id(&self) -> &[u8; 32] {
        &self.message_id
    }

    /// Local `lxmf.delivery` destination that received the message.
    pub const fn destination(self) -> DestinationHash {
        self.destination
    }

    /// Authenticated source `lxmf.delivery` destination.
    pub const fn source(self) -> DestinationHash {
        self.source
    }

    /// Exact IEEE-754 bits of the LXMF timestamp.
    pub const fn timestamp_bits(self) -> u64 {
        self.timestamp_bits
    }

    /// Complete normalized wire length.
    pub const fn normalized_wire_len(self) -> u32 {
        self.normalized_wire_len
    }

    /// Decoded title byte length.
    pub const fn title_len(self) -> u32 {
        self.title_len
    }

    /// Decoded content byte length.
    pub const fn content_len(self) -> u32 {
        self.content_len
    }

    /// Encoded MessagePack fields-map length.
    pub const fn fields_encoded_len(self) -> u32 {
        self.fields_encoded_len
    }

    /// SHA-256 of the complete normalized wire representation.
    pub const fn exact_wire_sha256(&self) -> &[u8; 32] {
        &self.exact_wire_sha256
    }
}

/// A committed LXMF summary contained an impossible zero wire length.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidLxmfMessageSummary;

/// One owned exact normalized-wire chunk returned by `experimental.lxmf.read`.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct LxmfReadChunk {
    handle: LxmfMessageHandle,
    offset: u32,
    total_len: u32,
    bytes_len: u16,
    bytes: [u8; MAX_LXMF_READ_CHUNK_BYTES],
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfReadChunk {
    /// Copy and validate one non-empty bounded chunk of a committed message.
    pub fn new(
        handle: LxmfMessageHandle,
        offset: u32,
        total_len: u32,
        bytes: &[u8],
    ) -> Result<Self, InvalidLxmfReadChunk> {
        if bytes.is_empty() {
            return Err(InvalidLxmfReadChunk::Empty);
        }
        if bytes.len() > MAX_LXMF_READ_CHUNK_BYTES {
            return Err(InvalidLxmfReadChunk::TooLarge {
                actual: bytes.len(),
            });
        }
        let end = u64::from(offset) + bytes.len() as u64;
        if total_len == 0 || offset >= total_len || end > u64::from(total_len) {
            return Err(InvalidLxmfReadChunk::OutsideMessage {
                offset,
                length: bytes.len(),
                total_len,
            });
        }
        let mut owned = [0_u8; MAX_LXMF_READ_CHUNK_BYTES];
        owned[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            handle,
            offset,
            total_len,
            bytes_len: bytes.len() as u16,
            bytes: owned,
        })
    }

    /// Stable committed-message handle.
    pub const fn handle(&self) -> LxmfMessageHandle {
        self.handle
    }

    /// Zero-based offset of the first returned byte.
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Complete normalized wire length.
    pub const fn total_len(&self) -> u32 {
        self.total_len
    }

    /// Exact bytes in this response.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.bytes_len as usize]
    }

    /// Whether this chunk reaches the exact end of the committed message.
    pub const fn is_final(&self) -> bool {
        self.offset as u64 + self.bytes_len as u64 == self.total_len as u64
    }
}

#[cfg(feature = "experimental-lxmf")]
impl core::fmt::Debug for LxmfReadChunk {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LxmfReadChunk")
            .field("handle", &self.handle)
            .field("offset", &self.offset)
            .field("total_len", &self.total_len)
            .field("bytes_len", &self.bytes_len)
            .finish_non_exhaustive()
    }
}

/// A returned LXMF chunk violated its fixed logical or message boundary.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidLxmfReadChunk {
    /// A successful read must make forward progress.
    Empty,
    /// Returned bytes exceeded the fixed response owner.
    TooLarge {
        /// Supplied byte count.
        actual: usize,
    },
    /// Offset and bytes did not fit inside the declared complete message.
    OutsideMessage {
        /// Supplied zero-based offset.
        offset: u32,
        /// Supplied chunk byte count.
        length: usize,
        /// Declared complete normalized wire length.
        total_len: u32,
    },
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

/// Acceptance result for source-free basic LXMF submission.
///
/// The device selects the authenticated local LXMF source. The returned
/// message identifier names the exact composed LXMF message, while the
/// submission identifier is used with [`DeviceRequest::SubmissionStatus`]. A
/// successful response means durable acceptance, not peer delivery.
#[cfg(feature = "experimental-lxmf")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LxmfBasicSendAccepted {
    /// Device-assigned durable submission identifier.
    pub id: SubmissionId,
    message_id: [u8; 32],
}

#[cfg(feature = "experimental-lxmf")]
impl LxmfBasicSendAccepted {
    /// Construct a successful basic-LXMF acceptance result.
    pub const fn new(id: SubmissionId, message_id: [u8; 32]) -> Self {
        Self { id, message_id }
    }

    /// Python-compatible LXMF authenticated-message identifier.
    pub const fn message_id(&self) -> &[u8; 32] {
        &self.message_id
    }
}

/// Acceptance result for one authenticated NomadNet page fetch.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NomadFetchStartAccepted {
    /// Device-assigned principal-owned fetch identifier.
    pub id: NomadFetchId,
    /// Whether this request created a fresh fetch or replayed an identical one.
    pub outcome: NomadFetchStartOutcome,
}

/// Principal-scoped idempotency outcome for a successful fetch start.
///
/// This is a closed API-v1 wire vocabulary.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchStartOutcome {
    /// A fresh fetch was accepted.
    Accepted = 0,
    /// An identical request for this principal and idempotency key was replayed.
    Replayed = 1,
}

#[cfg(feature = "experimental-nomad")]
impl NomadFetchStartOutcome {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Non-terminal phase returned by a NomadNet fetch poll.
///
/// This is a closed API-v1 wire vocabulary.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchPhase {
    /// Path discovery is in progress.
    PathLookup = 0,
    /// Link establishment is in progress.
    LinkEstablishment = 1,
    /// The anonymous request is being prepared.
    RequestPreparation = 2,
    /// A prepared request awaits first-dispatch confirmation.
    AwaitingDispatchConfirmation = 3,
    /// A confirmed request awaits its exactly correlated response.
    AwaitingResponse = 4,
}

#[cfg(feature = "experimental-nomad")]
impl NomadFetchPhase {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// Stable terminal failure returned by a NomadNet fetch poll.
///
/// Link identifiers, request identifiers, and adapter-local diagnostic codes
/// remain inside the product owner. This is a closed API-v1 wire vocabulary.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NomadFetchFailure {
    /// Path discovery completed without a usable path.
    NoPath = 0,
    /// Link preparation, dispatch, establishment, or retention failed.
    Link = 1,
    /// Request preparation, dispatch, or remote processing failed.
    Request = 2,
    /// A confirmed request exceeded its bounded response window.
    Timeout = 3,
    /// The decoded page exceeded the fixed direct-response limit.
    PageTooLarge = 4,
    /// The decoded page was not valid UTF-8 Micron text.
    InvalidUtf8 = 5,
    /// The product owner detected an internal invariant or backend failure.
    Internal = 6,
}

#[cfg(feature = "experimental-nomad")]
impl NomadFetchFailure {
    /// Frozen API-v1 numeric representation.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }
}

/// One owned bounded valid UTF-8 Micron page.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct NomadPage {
    bytes: [u8; MAX_NOMAD_PAGE_BYTES],
    len: u16,
}

#[cfg(feature = "experimental-nomad")]
impl NomadPage {
    /// Validate and copy one complete page body.
    pub fn new(bytes: &[u8]) -> Result<Self, InvalidNomadPage> {
        if bytes.len() > MAX_NOMAD_PAGE_BYTES {
            return Err(InvalidNomadPage::TooLarge {
                actual: bytes.len(),
            });
        }
        if core::str::from_utf8(bytes).is_err() {
            return Err(InvalidNomadPage::InvalidUtf8);
        }
        let mut owned = [0_u8; MAX_NOMAD_PAGE_BYTES];
        owned[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: owned,
            len: bytes.len() as u16,
        })
    }

    /// Borrow the complete page bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// Borrow the complete page as valid UTF-8.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).expect("NomadPage validates UTF-8 at construction")
    }

    /// Complete page length in bytes.
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the page is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(feature = "experimental-nomad")]
impl core::fmt::Debug for NomadPage {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("NomadPage")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// A candidate NomadNet page violated its fixed logical boundary.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidNomadPage {
    /// The page exceeded fixed response storage.
    TooLarge {
        /// Rejected byte count.
        actual: usize,
    },
    /// The page was not valid UTF-8.
    InvalidUtf8,
}

/// Result returned by polling one principal-owned NomadNet fetch.
#[cfg(feature = "experimental-nomad")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// The ready page remains inline so this no-alloc boundary owns one complete
// response without indirection or a product-lifetime borrow.
#[allow(clippy::large_enum_variant)]
pub enum NomadFetchPollResponse {
    /// The fetch remains in progress.
    Pending(NomadFetchPhase),
    /// One complete bounded Micron page is ready.
    Ready(NomadPage),
    /// The fetch ended with a stable terminal failure.
    Failed(NomadFetchFailure),
}

#[cfg(feature = "experimental-nomad")]
impl NomadFetchPollResponse {
    /// Frozen state discriminator encoded at response body key zero.
    pub const fn wire_code(&self) -> u8 {
        match self {
            Self::Pending(_) => 0,
            Self::Ready(_) => 1,
            Self::Failed(_) => 2,
        }
    }
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
    /// Next committed LXMF metadata entry in physical commit order.
    #[cfg(feature = "experimental-lxmf")]
    LxmfNext(LxmfMessageSummary),
    /// Bounded exact normalized-wire bytes for one committed LXMF message.
    #[cfg(feature = "experimental-lxmf")]
    LxmfRead(LxmfReadChunk),
    /// Accepted source-free basic LXMF submission.
    #[cfg(feature = "experimental-lxmf")]
    LxmfBasicSendAccepted(LxmfBasicSendAccepted),
    /// One bounded page from nearby `lxmf.delivery` peer discovery.
    #[cfg(feature = "experimental-lxmf")]
    LxmfPeerNext(LxmfPeerDiscoveryPage),
    /// Accepted bounded NomadNet page fetch.
    #[cfg(feature = "experimental-nomad")]
    NomadFetchStartAccepted(NomadFetchStartAccepted),
    /// Current or terminal state of one bounded NomadNet page fetch.
    #[cfg(feature = "experimental-nomad")]
    NomadFetchPoll(NomadFetchPollResponse),
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
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfNext(_) => OP_EXPERIMENTAL_LXMF_NEXT,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfRead(_) => OP_EXPERIMENTAL_LXMF_READ,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfBasicSendAccepted(_) => OP_EXPERIMENTAL_LXMF_BASIC_SEND,
            #[cfg(feature = "experimental-lxmf")]
            Self::LxmfPeerNext(_) => OP_EXPERIMENTAL_LXMF_PEER_NEXT,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchStartAccepted(_) => OP_EXPERIMENTAL_NOMAD_FETCH_START,
            #[cfg(feature = "experimental-nomad")]
            Self::NomadFetchPoll(_) => OP_EXPERIMENTAL_NOMAD_FETCH_POLL,
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
