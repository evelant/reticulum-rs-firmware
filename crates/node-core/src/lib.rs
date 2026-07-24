//! Bounded orchestration between the Reticulum protocol owner and interface
//! actors.
//!
//! This slice owns a concrete RNS node, fixed external-buffer dispatch
//! metadata, and a fixed DATA attempt ledger. A receipt is registered only
//! after both final metadata slots have been reserved. Proofs and timeouts
//! become acknowledged terminal tombstones before RNS releases its receipt
//! state. A separate fixed ordinary-action owner can atomically move a complete
//! allocation-backed `NodeActions` packet batch into caller-owned arrays
//! without consuming that DATA ledger. The crate deliberately has no
//! device-API, executor, radio, board, or persistence dependency.
//!
//! Construction and unrelated RNS paths may still allocate inside the current
//! Rete integration. The external-buffer DATA transaction and the post-action
//! ordinary packet staging implemented here use caller-owned arrays; neither
//! is a claim that the entire node is allocation free.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use core::num::NonZeroU64;

use rand_core::{CryptoRng, RngCore};
use reticulum_rns_rete::{
    AnnounceAdmissionError as RnsAnnounceAdmissionError,
    AnnounceAppDataError as RnsAnnounceAppDataError, DestHash as RnsDestinationHash,
    DestinationRegistrationError as RnsDestinationRegistrationError,
    DestinationType as RnsDestinationType, Direction as RnsDirection, EmbeddedNode as RnsNode,
    EmbeddedNodeConfig as RnsNodeConfig, Identity as RnsIdentity,
    InboundProofPolicy as RnsInboundProofPolicy,
    InboundProofPolicyError as RnsInboundProofPolicyError, InterfaceId as RnsInterfaceId,
    NodeRole as RnsNodeRole, PathRequestBuildError as RnsPathRequestBuildError,
    PrepareBasicLxmfError as RnsPrepareBasicLxmfError, PrepareDataError as RnsPrepareDataError,
    PreparedBasicLxmf as RnsPreparedBasicLxmf, PreparedData as RnsPreparedData, RNS_MTU,
    ReceiptCandidate, ReceiptKind, ReceiptReservationUnavailable, ReceiptTerminal,
    ReceiptTerminalReservation, ReceiptTerminalSink, TxTarget as RnsTxTarget,
};
pub use reticulum_rns_rete::{
    ApplicationEvent, ApplicationEventAcknowledgeFailure, ApplicationEventCapacitySnapshot,
    ApplicationEventDiscardReason, ApplicationEventGeneration, ApplicationEventId,
    ApplicationEventKind, ApplicationEventLease, ApplicationEventOfferError,
    ApplicationEventOfferFailure, ApplicationEventOfferReport, ApplicationEventOwner,
    ApplicationEventOwnerCounters, ApplicationEventQuarantineReason, ApplicationEventRetryError,
    ApplicationEventRetryFailure, ApplicationEventRetryToken, ApplicationEventSequence,
    ApplicationEventSlot, ApplicationEventSlotId, ApplicationLinkBinding, ApplicationLinkRole,
    ApplicationRequestFailReason, DelayedProofCapacitySnapshot, DelayedProofGeneration,
    DelayedProofId, DelayedProofLease, DelayedProofOwner, DelayedProofOwnerCounters,
    DelayedProofReservationError, DelayedProofSequence, DelayedProofSlot, DelayedProofSlotId,
    DelayedProofTransaction, DelayedProofTransactionError, DelayedProofTransactionFailure,
    InboundData, InboundDataProjection, IngressDisposition, IngressMetadata, IngressReport,
    MonotonicInstant, NodeActions, OutboundDispatchInterval, OutboundProtocolToken, PacketType,
    RetainedProofCommitSuccess, project_inbound_data,
};
use sha2::{Digest, Sha256};

mod ordinary_actions;
pub use ordinary_actions::{
    OrdinaryActionAdmissionError, OrdinaryActionAdmissionFailure, OrdinaryActionAdmissionRequest,
    OrdinaryActionBatch, OrdinaryActionCapacitySnapshot, OrdinaryActionOwner,
    OrdinaryActionOwnerClaimError, OrdinaryAuthorizationErrorKind, OrdinaryAuthorizationFailure,
    OrdinaryAuthorizedTx, OrdinaryAvailableBufferError, OrdinaryBufferPool,
    OrdinaryBufferPoolError, OrdinaryBufferRegistrationError, OrdinaryCompletionDisposition,
    OrdinaryCompletionError, OrdinaryCompletionFailure, OrdinaryExpiredAuthorizedTx,
    OrdinaryFrameError, OrdinaryPacketBuffer, OrdinaryPacketGeneration, OrdinaryPacketReturn,
    OrdinaryPacketReturnParkFailure, OrdinaryPacketReturnReason, OrdinaryPacketSlotId,
    OrdinaryPermitCancelMismatch, OrdinaryPermitPendingTx, OrdinaryPermitReplyMismatch,
    OrdinaryPermitResolution, OrdinaryPreparedPacket, OrdinaryQuarantineReason,
    OrdinaryRegisterAndParkError, OrdinaryRegisterAndParkFailure, OrdinaryTxCompletion,
    OrdinaryTxFrame, OrdinaryTxJob, OrdinaryTxPermitDenial, OrdinaryTxPermitDenialReason,
    OrdinaryTxPermitGrant, OrdinaryTxPermitReply, OrdinaryTxPermitRequest, OrdinaryTxQuarantine,
    OrdinaryUnpermittedTx,
};

/// Complete packet storage reserved for one base-MTU Reticulum transmission.
pub const PACKET_CAPACITY: usize = RNS_MTU;

/// Largest destination-DATA plaintext accepted by the current RNS adapter.
pub const MAX_DATA_PAYLOAD: usize = reticulum_rns_rete::MAX_DATA_PAYLOAD;

/// Largest opportunistic LXMF carrier emitted by the basic composer.
pub const MAX_OPPORTUNISTIC_LXMF_CARRIER: usize =
    reticulum_rns_rete::MAX_OPPORTUNISTIC_LXMF_CARRIER;

/// Largest positive whole-millisecond timestamp accepted by basic LXMF composition.
pub const MAX_LXMF_TIMESTAMP_UNIX_MS: u64 = reticulum_rns_rete::MAX_LXMF_TIMESTAMP_UNIX_MS;

/// Largest application-data payload accepted for one local announce.
pub const MAX_ANNOUNCE_APP_DATA: usize = reticulum_rns_rete::MAX_ANNOUNCE_APP_DATA;

/// Native RNS delay before the one scheduled retransmission of an announce.
pub const RNS_ANNOUNCE_RETRANSMIT_SECONDS: u64 = reticulum_rns_rete::ANNOUNCE_RETRANSMIT_SECONDS;

/// Link DATA context that carries ordinary application bytes rather than an
/// RNS control or service message.
pub const APPLICATION_LINK_CONTEXT_NONE: u8 = reticulum_rns_rete::LINK_DATA_CONTEXT_NONE;

/// Extract the compact correlation tag from one explicit RNS delivery proof.
///
/// The packet must have one of the two exact structural wire shapes retained by
/// the product:
///
/// - an ordinary HEADER_1/SINGLE `PROOF`, whose destination is the truncated
///   covered hash; or
/// - a responder HEADER_1/LINK `PROOF`, whose destination is the Link ID.
///
/// Both forms have zero hops, context `NONE`, and the explicit 96-byte
/// `covered_hash[32] || signature[64]` payload. This deliberately excludes
/// Reticulum's 64-byte implicit-proof form, whose signature bytes do not expose
/// a covered hash. The returned value is the little-endian first eight bytes of
/// the covered hash, matching ingress and receipt trace correlation without
/// exposing the implementation parser to product firmware.
pub fn delivery_proof_correlation_tag(raw: &[u8]) -> Option<u64> {
    const RETE_SINGLE_PROOF_FLAGS: u8 = 0x03;
    const RETE_LINK_PROOF_FLAGS: u8 = 0x0f;
    const COVERED_HASH_LEN: usize = 32;
    const SIGNATURE_LEN: usize = 64;
    const EXPLICIT_PROOF_PAYLOAD_LEN: usize = COVERED_HASH_LEN + SIGNATURE_LEN;

    let packet = reticulum_rns_rete::parse_packet(raw).ok()?;
    if packet.hops != 0
        || packet.packet_type != PacketType::Proof
        || packet.context != APPLICATION_LINK_CONTEXT_NONE
        || packet.payload.len() != EXPLICIT_PROOF_PAYLOAD_LEN
    {
        return None;
    }
    let covered_hash = packet.payload.get(..COVERED_HASH_LEN)?;
    let canonical_destination = match packet.flags {
        RETE_SINGLE_PROOF_FLAGS => {
            packet.dest_type == reticulum_rns_rete::DestType::Single
                && packet.destination_hash == &covered_hash[..16]
        }
        RETE_LINK_PROOF_FLAGS => packet.dest_type == reticulum_rns_rete::DestType::Link,
        _ => false,
    };
    if !canonical_destination {
        return None;
    }
    let prefix: [u8; 8] = covered_hash[..8]
        .try_into()
        .expect("an admitted 32-byte covered hash has an eight-byte prefix");
    Some(u64::from_le_bytes(prefix))
}

/// Whether Rete's current heapless backend can instantiate these table
/// capacities.
///
/// Path and Link maps use `FnvIndexMap`, whose capacity must be a power of two
/// greater than one. Announce and deduplication storage use non-empty deques.
/// External packet-buffer dispatch metadata is independent and may have zero
/// slots.
pub const fn capacity_profile_is_supported(
    paths: usize,
    announces: usize,
    deduplication: usize,
    links: usize,
) -> bool {
    paths > 1
        && paths.is_power_of_two()
        && announces > 0
        && deduplication > 0
        && links > 1
        && links.is_power_of_two()
}

/// Scalar result of composing one signed basic LXMF carrier.
///
/// The caller retains the bytes in its supplied output buffer. The scalar
/// contains no private identity material and does not expose the underlying
/// Rete message, signing source, or destination. It privately preserves the
/// composer capability needed by the dedicated opportunistic DATA path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedBasicLxmf(RnsPreparedBasicLxmf);

impl PreparedBasicLxmf {
    /// Exact carrier prefix initialized in the caller's output buffer.
    pub const fn carrier_len(self) -> u16 {
        self.0.carrier_len()
    }

    /// Python-compatible authenticated LXMF message identifier.
    pub const fn message_id(self) -> [u8; 32] {
        self.0.message_id()
    }
}

/// Failure to compose the supported basic opportunistic LXMF subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrepareBasicLxmfError {
    /// Timestamp zero is outside the firmware's useful message subset.
    InvalidTimestamp,
    /// Whole-millisecond timestamp exceeds the exact binary64 subset.
    TimestampTooLarge {
        /// Rejected Unix timestamp in milliseconds.
        actual: u64,
        /// Largest supported Unix timestamp in milliseconds.
        maximum: u64,
    },
    /// The node has no registered inbound Single `lxmf.delivery` source.
    DeliveryDestinationUnavailable,
    /// The node identity could not sign the canonical message.
    Signing,
    /// Canonical content exceeds Python's opportunistic selection boundary.
    PayloadTooLarge {
        /// Computed Python LXMF `content_size`.
        actual: usize,
        /// Largest opportunistically selected content size.
        maximum: usize,
    },
    /// Caller-owned carrier storage is too small.
    OutputTooSmall {
        /// Required carrier bytes.
        required: usize,
        /// Supplied output capacity.
        available: usize,
    },
    /// The pinned packer violated an internal reviewed invariant.
    Invariant,
}

impl From<RnsPrepareBasicLxmfError> for PrepareBasicLxmfError {
    fn from(error: RnsPrepareBasicLxmfError) -> Self {
        match error {
            RnsPrepareBasicLxmfError::InvalidTimestamp => Self::InvalidTimestamp,
            RnsPrepareBasicLxmfError::TimestampTooLarge { actual, maximum } => {
                Self::TimestampTooLarge { actual, maximum }
            }
            RnsPrepareBasicLxmfError::DeliveryDestinationUnavailable => {
                Self::DeliveryDestinationUnavailable
            }
            RnsPrepareBasicLxmfError::Signing => Self::Signing,
            RnsPrepareBasicLxmfError::PayloadTooLarge { actual, maximum } => {
                Self::PayloadTooLarge { actual, maximum }
            }
            RnsPrepareBasicLxmfError::OutputTooSmall {
                required,
                available,
            } => Self::OutputTooSmall {
                required,
                available,
            },
            RnsPrepareBasicLxmfError::Invariant => Self::Invariant,
        }
    }
}

/// Opaque Reticulum identity owned by the node core.
pub struct NodeIdentity(RnsIdentity);

impl NodeIdentity {
    /// Import Reticulum's combined 64-byte X25519-plus-Ed25519 private key.
    pub fn from_private_key(private_key: &[u8]) -> Result<Self, IdentityError> {
        reticulum_rns_rete::identity_from_private_key(private_key)
            .map(Self)
            .map_err(|_| IdentityError::InvalidPrivateKey)
    }

    /// Public X25519-plus-Ed25519 key bytes.
    pub fn public_key(&self) -> [u8; 64] {
        self.0.public_key()
    }

    /// Public 128-bit Reticulum identity hash.
    pub fn identity_hash(&self) -> [u8; 16] {
        *self.0.hash().as_bytes()
    }
}

/// Failure to import a node identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// The private key length or key material is invalid.
    InvalidPrivateKey,
}

/// Product-owned complete 128-bit Reticulum destination hash.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DestinationHash([u8; 16]);

impl DestinationHash {
    /// Construct a destination hash from its complete wire bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete destination hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    fn into_rns(self) -> RnsDestinationHash {
        RnsDestinationHash::from(self.0)
    }
}

impl AsRef<[u8]> for DestinationHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Caller-provided identity for one in-memory node-owner incarnation.
///
/// A value must not be reused when reconstructing the same Reticulum identity
/// while messages carrying old TX leases can still exist. Firmware should
/// derive this from a boot/session nonce or a persisted monotonic boot epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NodeInstanceId([u8; 16]);

impl NodeInstanceId {
    /// Construct an instance identifier from caller-owned epoch/nonce bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete instance identifier.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Opaque scalar binding for one Reticulum identity and in-memory node-owner
/// incarnation.
///
/// This is correlation metadata, not authorization. It lets a persistent
/// external owner machine reject accidental substitution of another
/// [`NodeCore`] before consuming a unique return.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TxOwnerScope {
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
}

impl TxOwnerScope {
    /// Caller-supplied incarnation included in this exact owner binding.
    pub const fn instance_id(self) -> NodeInstanceId {
        self.instance
    }
}

/// Monotonic time in whole seconds for RNS path and receipt state.
///
/// This is deliberately distinct from executor ticks, milliseconds and Unix
/// wall time. Callers must convert at the platform boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicSeconds(u64);

impl MonotonicSeconds {
    /// Construct a monotonic-seconds sample.
    pub const fn new(seconds: u64) -> Self {
        Self(seconds)
    }

    /// Raw whole seconds supplied to RNS.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Reticulum's 40-bit announce-emission ordering value.
///
/// This is deliberately distinct from [`MonotonicSeconds`]. Reticulum embeds
/// the low 40 bits of this value in every locally generated announce and peers
/// use it to reject stale or replayed paths. A persistent identity therefore
/// needs a value that cannot move backwards across reboot. Unix seconds are
/// suitable when trustworthy wall time exists; an offline target may instead
/// use a durably reserved logical epoch.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AnnounceEmissionTime(u64);

impl AnnounceEmissionTime {
    /// Largest value representable in the five-byte Reticulum field.
    pub const MAX: u64 = (1_u64 << 40) - 1;

    /// Construct a checked announce-emission ordering value.
    pub const fn new(value: u64) -> Result<Self, AnnounceEmissionTimeError> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(AnnounceEmissionTimeError::OutsideWireRange)
        }
    }

    /// Raw value supplied to the RNS implementation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Failure to construct a Reticulum announce-emission ordering value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnounceEmissionTimeError {
    /// The value would be truncated by Reticulum's five-byte wire field.
    OutsideWireRange,
}

/// Executor-independent monotonic time in whole milliseconds.
///
/// This clock scopes packet-owner deadlines. It is deliberately distinct
/// from RNS's whole-second protocol clock and from Unix wall time.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicMillis(u64);

impl MonotonicMillis {
    /// Construct a monotonic-millisecond sample.
    pub const fn new(milliseconds: u64) -> Self {
        Self(milliseconds)
    }

    /// Raw whole milliseconds supplied by the platform monotonic clock.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Deadline by which an externally owned packet buffer should return to the
/// node owner or enter retained recovery.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TxLeaseDeadline(MonotonicMillis);

impl TxLeaseDeadline {
    /// Construct a packet-owner deadline from a monotonic instant.
    pub const fn new(deadline: MonotonicMillis) -> Self {
        Self(deadline)
    }

    /// Monotonic instant represented by this deadline.
    pub const fn instant(self) -> MonotonicMillis {
        self.0
    }
}

/// Product-owned packet-interface identifier.
///
/// RNS interface identifiers are one byte. The initial compact
/// [`InterfaceSet`] can represent identifiers zero through 63; larger values
/// remain representable here so a native target is never truncated while a
/// later route-resolution step can reject an unsupported profile explicitly.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PacketInterfaceId(u8);

impl PacketInterfaceId {
    /// Construct a packet-interface identifier from its complete wire value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Raw one-byte interface identifier.
    pub const fn get(self) -> u8 {
        self.0
    }

    fn into_rns(self) -> RnsInterfaceId {
        RnsInterfaceId(self.0)
    }
}

/// Fixed set of the first 64 packet interfaces.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InterfaceSet(u64);

impl InterfaceSet {
    /// Empty interface set.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Construct a set from its complete bit representation.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Complete bit representation of this set.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether this set contains no interfaces.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether this compact set contains an interface.
    ///
    /// Identifiers above 63 are outside this profile and return `false`.
    pub const fn contains(self, interface: PacketInterfaceId) -> bool {
        let index = interface.get();
        index < 64 && self.0 & (1u64 << index) != 0
    }

    /// Add an interface when it fits the compact set.
    ///
    /// `None` reports an identifier above 63 without altering the set.
    pub const fn with(self, interface: PacketInterfaceId) -> Option<Self> {
        let index = interface.get();
        if index < 64 {
            Some(Self(self.0 | (1u64 << index)))
        } else {
            None
        }
    }

    const fn without(self, interface: PacketInterfaceId) -> Option<Self> {
        let index = interface.get();
        if index < 64 {
            Some(Self(self.0 & !(1u64 << index)))
        } else {
            None
        }
    }

    const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    fn take_first(&mut self) -> Option<PacketInterfaceId> {
        if self.0 == 0 {
            return None;
        }
        let index = self.0.trailing_zeros() as u8;
        self.0 &= self.0 - 1;
        Some(PacketInterfaceId::new(index))
    }
}

/// Interface authorization retained with one prepared RNS packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxTarget {
    /// Send on every eligible packet interface.
    All,
    /// Send only on one packet interface.
    Only(PacketInterfaceId),
    /// Send on every eligible interface except one packet interface.
    AllExcept(PacketInterfaceId),
}

impl TxTarget {
    fn from_rns(target: RnsTxTarget) -> Self {
        match target {
            RnsTxTarget::All => Self::All,
            RnsTxTarget::Only(interface) => Self::Only(PacketInterfaceId::new(interface.0)),
            RnsTxTarget::AllExcept(interface) => {
                Self::AllExcept(PacketInterfaceId::new(interface.0))
            }
        }
    }
}

/// Failure to resolve an RNS target against the enabled packet interfaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxRouteError {
    /// A target named an interface outside the compact 0-through-63 profile.
    InterfaceOutsideProfile {
        /// Complete interface identifier that could not be represented.
        interface: PacketInterfaceId,
    },
    /// No enabled packet interface satisfies the target.
    NoEligibleInterface {
        /// Target that resolved to an empty route.
        target: TxTarget,
    },
}

/// Deterministic remaining interfaces for one serialized packet fan-out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxRoutePlan {
    remaining: InterfaceSet,
}

impl TxRoutePlan {
    /// Resolve a target against one synchronous enabled-interface snapshot.
    pub fn resolve(target: TxTarget, enabled: InterfaceSet) -> Result<Self, TxRouteError> {
        let remaining = match target {
            TxTarget::All => enabled,
            TxTarget::Only(interface) => {
                let singleton = InterfaceSet::empty()
                    .with(interface)
                    .ok_or(TxRouteError::InterfaceOutsideProfile { interface })?;
                enabled.intersect(singleton)
            }
            TxTarget::AllExcept(interface) => enabled
                .without(interface)
                .ok_or(TxRouteError::InterfaceOutsideProfile { interface })?,
        };
        if remaining.is_empty() {
            Err(TxRouteError::NoEligibleInterface { target })
        } else {
            Ok(Self { remaining })
        }
    }

    /// Interfaces not yet selected for this serialized fan-out.
    pub const fn remaining(self) -> InterfaceSet {
        self.remaining
    }

    fn take_next(&mut self) -> Option<PacketInterfaceId> {
        self.remaining.take_first()
    }
}

/// Complete caller-owned inputs for one destination-DATA preparation.
///
/// The Reticulum clock and packet-owner clock are deliberately separate. The
/// owner deadline must be strictly later than `owner_now` when preparation
/// begins.
pub struct PrepareDataRequest<'a> {
    /// Complete destination hash for the encrypted DATA packet.
    pub destination: DestinationHash,
    /// Plaintext to encrypt into one base-MTU DATA packet.
    pub plaintext: &'a [u8],
    /// Current monotonic whole-second sample supplied to RNS.
    pub rns_now: MonotonicSeconds,
    /// Current monotonic millisecond sample for owner-lease validation.
    pub owner_now: MonotonicMillis,
    /// Deadline for returning the unique packet-buffer owner.
    pub deadline: TxLeaseDeadline,
    /// Synchronous snapshot of packet interfaces eligible for this dispatch.
    pub enabled_interfaces: InterfaceSet,
}

/// Whether this node terminates only local traffic or also forwards RNS
/// traffic for other nodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeRole {
    /// Local endpoint only.
    Endpoint,
    /// Reticulum transport node.
    Transport,
}

/// Automatic delivery-proof behavior for DATA received at a local destination.
///
/// The default is fail-quiet: inbound DATA is delivered to the application but
/// does not cause an automatic transmission. Firmware must opt in explicitly
/// when delivery proofs are part of its configured Reticulum service policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InboundProofPolicy {
    /// Never generate delivery proofs automatically.
    #[default]
    Never,
    /// Generate a delivery proof for every successfully decrypted DATA packet.
    Always,
    /// Retain the generated proof with its application event until durable
    /// application policy explicitly releases it.
    Retain,
}

impl InboundProofPolicy {
    const fn into_rns(self) -> RnsInboundProofPolicy {
        match self {
            Self::Never => RnsInboundProofPolicy::Never,
            Self::Always => RnsInboundProofPolicy::Always,
            Self::Retain => RnsInboundProofPolicy::Retain,
        }
    }
}

/// Construction policy for the bounded node owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeConfig {
    /// Endpoint or transport behavior.
    pub role: NodeRole,
    /// Maximum destinations registered in addition to the primary one.
    pub max_additional_destinations: usize,
}

impl NodeConfig {
    /// Conservative endpoint profile.
    pub const fn endpoint() -> Self {
        Self {
            role: NodeRole::Endpoint,
            max_additional_destinations: 4,
        }
    }

    /// Transport profile with the same initial local-destination quota.
    pub const fn transport() -> Self {
        Self {
            role: NodeRole::Transport,
            max_additional_destinations: 4,
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::endpoint()
    }
}

/// Failure to construct the concrete RNS owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeConstructionError {
    /// RNS rejected the application name, aspects, identity, or destination.
    InvalidRnsConfiguration,
}

/// Failure to register one additional inbound single-identity destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationRegistrationError {
    /// The construction-time additional-destination quota is exhausted.
    LimitReached {
        /// Configured maximum additional destinations.
        limit: usize,
    },
    /// RNS rejected the application name, aspects, or destination construction.
    InvalidRnsConfiguration,
}

impl From<RnsDestinationRegistrationError> for LocalDestinationRegistrationError {
    fn from(error: RnsDestinationRegistrationError) -> Self {
        match error {
            RnsDestinationRegistrationError::LimitReached { limit } => Self::LimitReached { limit },
            RnsDestinationRegistrationError::Rete(_) => Self::InvalidRnsConfiguration,
        }
    }
}

/// Failure to configure default announce application data for one local
/// destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationAnnounceAppDataError {
    /// The destination is not registered on this node.
    DestinationNotRegistered,
    /// Application data cannot fit in one base-MTU announce.
    AppDataTooLarge {
        /// Supplied application-data length.
        actual: usize,
        /// Maximum supported application-data length.
        maximum: usize,
    },
    /// Owned default application-data storage could not be reserved.
    AllocationFailed,
}

impl From<RnsAnnounceAppDataError> for LocalDestinationAnnounceAppDataError {
    fn from(error: RnsAnnounceAppDataError) -> Self {
        match error {
            RnsAnnounceAppDataError::DestinationNotRegistered => Self::DestinationNotRegistered,
            RnsAnnounceAppDataError::AppDataTooLarge { actual, maximum } => {
                Self::AppDataTooLarge { actual, maximum }
            }
            RnsAnnounceAppDataError::AllocationFailed => Self::AllocationFailed,
        }
    }
}

/// Failure to construct one tagged Reticulum path-request action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathRequestError {
    /// The fixed request could not be encoded into one base-MTU packet.
    PacketBuild,
    /// Owned packet storage could not be reserved.
    AllocationFailed,
}

impl From<RnsPathRequestBuildError> for PathRequestError {
    fn from(error: RnsPathRequestBuildError) -> Self {
        match error {
            RnsPathRequestBuildError::PacketBuild => Self::PacketBuild,
            RnsPathRequestBuildError::AllocationFailed => Self::AllocationFailed,
        }
    }
}

/// Failure to configure delivery-proof behavior for one local destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationProofPolicyError {
    /// The destination is not registered on this node.
    DestinationNotRegistered,
    /// Retained proofs require an inbound encrypted Single destination.
    RetainRequiresInboundSingle,
}

/// Failure to configure inbound Link admission for one local destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDestinationLinkPolicyError {
    /// The destination is not registered on this node.
    DestinationNotRegistered,
}

impl From<RnsInboundProofPolicyError> for LocalDestinationProofPolicyError {
    fn from(error: RnsInboundProofPolicyError) -> Self {
        match error {
            RnsInboundProofPolicyError::DestinationNotRegistered => Self::DestinationNotRegistered,
            RnsInboundProofPolicyError::RetainRequiresInboundSingle => {
                Self::RetainRequiresInboundSingle
            }
        }
    }
}

/// Failure to cache one peer identity for deterministic direct sending.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerRegistrationError {
    /// RNS rejected the peer name, aspects, or identity.
    InvalidPeer,
}

/// Failure to admit one local announce into the bounded protocol queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnounceAdmissionError {
    /// Optional application data cannot fit in a base-MTU announce.
    AppDataTooLarge {
        /// Supplied application-data length in bytes.
        actual: usize,
        /// Maximum accepted application-data length in bytes.
        maximum: usize,
    },
    /// Every configured pending-announce slot is occupied.
    QueueFull {
        /// Configured pending-announce capacity.
        limit: usize,
    },
    /// The native protocol implementation rejected announce construction or
    /// queueing after product-owned admission checks passed.
    NativeRejected,
}

impl From<RnsAnnounceAdmissionError> for AnnounceAdmissionError {
    fn from(error: RnsAnnounceAdmissionError) -> Self {
        match error {
            RnsAnnounceAdmissionError::AppDataTooLarge { actual, maximum } => {
                Self::AppDataTooLarge { actual, maximum }
            }
            RnsAnnounceAdmissionError::QueueFull { limit } => Self::QueueFull { limit },
            RnsAnnounceAdmissionError::NativeRejected => Self::NativeRejected,
        }
    }
}

/// Full RNS hash used to correlate one prepared DATA packet with its delivery
/// proof or timeout.
///
/// This is the Reticulum hashable-part digest covered by a proof. It is not a
/// SHA-256 digest of every byte in the encoded interface packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AttemptToken([u8; 32]);

impl AttemptToken {
    /// Borrow the complete proof-correlation hash.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// SHA-256 digest of every byte in one complete encoded interface packet.
///
/// This is deliberately distinct from [`AttemptToken`], whose bytes cover
/// Reticulum's proof-correlated hashable portion rather than the complete
/// encoded packet.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EncodedPacketSha256([u8; 32]);

impl EncodedPacketSha256 {
    /// Construct a digest from SHA-256 over every encoded packet byte.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete full-packet digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Opaque, generation-checked identity of one submitted DATA attempt.
///
/// Unlike [`AttemptToken`], this handle remains unambiguous if packet hashes
/// are ever repeated after an earlier attempt has been acknowledged. It is
/// scoped to both the Reticulum identity and this in-memory node incarnation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AttemptHandle {
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    slot: usize,
    generation: u64,
}

/// Why a successfully prepared DATA attempt became definitely unsent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptUnsentReason {
    /// A prepared job was proven not to have entered the dispatcher handoff.
    QueueRollback,
    /// The selected interface's synchronous resource policy rejected the final hop.
    PolicyDenied(TxPolicyDenial),
    /// The packet-owner deadline expired before the final hop was permitted.
    PermitDeadlineExpired,
    /// The exact dispatch had already entered fail-closed recovery.
    RecoveryRequired,
    /// A definitely-unpermitted final hop completed without a richer denial.
    Unpermitted(TxCompletionCode),
}

/// Application-visible outcome for one destination-DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptOutcome {
    /// A valid Reticulum delivery proof was accepted.
    Delivered,
    /// The Reticulum delivery receipt expired without a proof.
    DeliveryTimeout,
    /// The complete serialized route ended without any permitted transmission.
    Unsent(AttemptUnsentReason),
}

/// One terminal attempt retained until the application acknowledges it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalAttempt {
    handle: AttemptHandle,
    token: AttemptToken,
    outcome: AttemptOutcome,
}

impl TerminalAttempt {
    /// Opaque handle required to acknowledge this terminal record.
    pub const fn handle(self) -> AttemptHandle {
        self.handle
    }

    /// Complete RNS proof-correlation hash for this attempt.
    pub const fn token(self) -> AttemptToken {
        self.token
    }

    /// Delivery outcome retained by the tombstone.
    pub const fn outcome(self) -> AttemptOutcome {
        self.outcome
    }
}

/// Stable identifier assigned to one externally owned packet buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PacketSlotId(u16);

impl PacketSlotId {
    /// Numeric slot index assigned by the node owner.
    pub const fn get(self) -> u16 {
        self.0
    }

    fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Scalar metadata for one packet committed to an external buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedPacket {
    packet_len: u16,
    encoded_packet_sha256: EncodedPacketSha256,
    attempt: AttemptToken,
    handle: AttemptHandle,
    slot: PacketSlotId,
    target: TxTarget,
    deadline: TxLeaseDeadline,
}

impl PreparedPacket {
    /// Encoded packet length in the external packet buffer.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// SHA-256 over every complete encoded packet byte.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }

    /// Proof-correlation token for this attempt.
    pub const fn attempt(self) -> AttemptToken {
        self.attempt
    }

    /// Generation-scoped handle for later terminal acknowledgement.
    pub const fn handle(self) -> AttemptHandle {
        self.handle
    }

    /// Stable external packet-buffer slot containing these bytes.
    pub const fn slot_id(self) -> PacketSlotId {
        self.slot
    }

    /// RNS interface authorization retained from packet preparation.
    pub const fn target(self) -> TxTarget {
        self.target
    }

    /// Monotonic return/recovery deadline bound to this dispatch.
    pub const fn deadline(self) -> TxLeaseDeadline {
        self.deadline
    }

    fn from_rns(
        prepared: RnsPreparedData,
        encoded_packet_sha256: EncodedPacketSha256,
        handle: AttemptHandle,
        slot: PacketSlotId,
        deadline: TxLeaseDeadline,
    ) -> Self {
        Self {
            packet_len: prepared.packet_len(),
            encoded_packet_sha256,
            attempt: AttemptToken(*prepared.receipt().as_bytes()),
            handle,
            slot,
            target: TxTarget::from_rns(prepared.target()),
            deadline,
        }
    }
}

/// Failure to reserve and prepare one DATA packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitError {
    /// The supplied packet buffer was never registered with this node owner.
    UnregisteredPacketBuffer,
    /// The supplied packet buffer belongs to another node incarnation.
    ForeignPacketBuffer,
    /// The supplied packet buffer is already bound to an active dispatch.
    PacketBufferBusy,
    /// The registered packet slot and node dispatch metadata disagree.
    PacketBufferInvariant,
    /// Every attempt slot is active or retained as an unacknowledged
    /// terminal tombstone.
    AttemptLedgerFull {
        /// Configured DATA-attempt limit.
        limit: usize,
    },
    /// The non-repeating attempt-generation space has been exhausted.
    AttemptIdentifierExhausted,
    /// The non-repeating dispatch-generation space has been exhausted.
    DispatchIdentifierExhausted,
    /// The non-repeating per-hop generation space has been exhausted.
    HopIdentifierExhausted,
    /// The packet-owner deadline was already reached when preparation began.
    LeaseDeadlineExpired {
        /// Current owner-clock sample.
        now: MonotonicMillis,
        /// Supplied owner deadline.
        deadline: TxLeaseDeadline,
    },
    /// No enabled packet interface satisfies the prepared RNS target.
    NoEligibleInterface {
        /// Target that resolved to an empty route.
        target: TxTarget,
    },
    /// A prepared target named an interface outside the compact route profile.
    InterfaceOutsideProfile {
        /// Complete unsupported interface identifier.
        interface: PacketInterfaceId,
    },
    /// Exact receipt cancellation failed while rejecting an empty route.
    RouteReceiptCancellationFailed,
    /// Plaintext cannot fit in one encrypted base-MTU DATA packet.
    PayloadTooLarge {
        /// Supplied plaintext length.
        actual: usize,
        /// Maximum accepted plaintext length.
        maximum: usize,
    },
    /// No cached identity exists for the destination.
    UnknownDestination,
    /// The bounded RNS receipt table has no free entry.
    ReceiptTableFull {
        /// Configured receipt limit.
        limit: usize,
    },
    /// A live receipt already uses the generated hash.
    ReceiptHashAlreadyTracked,
    /// Destination encryption failed.
    Cryptography,
    /// RNS could not construct the packet.
    PacketBuild,
    /// The RNS adapter reported an impossible failure class.
    Invariant,
}

impl From<TxRouteError> for SubmitError {
    fn from(error: TxRouteError) -> Self {
        match error {
            TxRouteError::InterfaceOutsideProfile { interface } => {
                Self::InterfaceOutsideProfile { interface }
            }
            TxRouteError::NoEligibleInterface { target } => Self::NoEligibleInterface { target },
        }
    }
}

impl From<RnsPrepareDataError> for SubmitError {
    fn from(error: RnsPrepareDataError) -> Self {
        match error {
            RnsPrepareDataError::PayloadTooLarge { actual, maximum } => {
                Self::PayloadTooLarge { actual, maximum }
            }
            RnsPrepareDataError::UnknownDestination => Self::UnknownDestination,
            RnsPrepareDataError::ReceiptTableFull { limit } => Self::ReceiptTableFull { limit },
            RnsPrepareDataError::ReceiptHashAlreadyTracked => Self::ReceiptHashAlreadyTracked,
            RnsPrepareDataError::Crypto => Self::Cryptography,
            RnsPrepareDataError::PacketBuild => Self::PacketBuild,
            RnsPrepareDataError::Invariant => Self::Invariant,
        }
    }
}

/// Failure to register one external packet buffer with a node owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferRegistrationError {
    /// Every configured dispatch slot already has a registered buffer.
    PoolFull {
        /// Configured external packet-buffer limit.
        limit: usize,
    },
    /// This buffer is already registered or bound to a dispatch.
    AlreadyRegistered,
}

/// Read-only validation failure for a supposedly reusable packet buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvailableBufferError {
    /// The packet buffer was never registered with any node owner.
    Unregistered,
    /// The packet buffer belongs to another node owner or incarnation.
    ForeignOwner,
    /// The packet buffer is still bound to an active dispatch.
    Busy,
    /// Buffer registration and node dispatch metadata disagree.
    MetadataInvariant,
}

impl From<AvailableBufferError> for SubmitError {
    fn from(error: AvailableBufferError) -> Self {
        match error {
            AvailableBufferError::Unregistered => Self::UnregisteredPacketBuffer,
            AvailableBufferError::ForeignOwner => Self::ForeignPacketBuffer,
            AvailableBufferError::Busy => Self::PacketBufferBusy,
            AvailableBufferError::MetadataInvariant => Self::PacketBufferInvariant,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HopBinding {
    interface: PacketInterfaceId,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BufferBinding {
    Unregistered,
    Available {
        owner_identity: [u8; 16],
        instance: NodeInstanceId,
        slot: PacketSlotId,
    },
    Bound {
        dispatch_generation: u64,
        prepared: PreparedPacket,
        hop: Option<HopBinding>,
    },
}

/// One complete externally owned Reticulum packet buffer.
///
/// This value is intentionally neither Copy nor Clone. Firmware allocates it
/// in static storage, registers it once, and then moves its unique mutable
/// reference through the owning handoff. Packet bytes remain private until an
/// AuthorizedTx exposes one borrowed frame.
pub struct TxPacketBuffer {
    bytes: [u8; PACKET_CAPACITY],
    binding: BufferBinding,
}

impl TxPacketBuffer {
    /// Construct one zero-filled, unregistered packet buffer.
    pub const fn new() -> Self {
        Self {
            bytes: [0; PACKET_CAPACITY],
            binding: BufferBinding::Unregistered,
        }
    }

    /// Stable slot assigned during registration, if any.
    pub const fn slot_id(&self) -> Option<PacketSlotId> {
        match self.binding {
            BufferBinding::Unregistered => None,
            BufferBinding::Available { slot, .. } => Some(slot),
            BufferBinding::Bound { prepared, .. } => Some(prepared.slot_id()),
        }
    }
}

impl Default for TxPacketBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DispatchHandle {
    instance: NodeInstanceId,
    slot: PacketSlotId,
    generation: u64,
}

struct OwnedTx<'a> {
    buffer: &'a mut TxPacketBuffer,
    dispatch: DispatchHandle,
}

/// Opaque identity for one exact transport-owned authorization resource.
///
/// Node core treats both this identity and the associated permit units as
/// opaque values. The interface actor and authorization policy define their
/// transport-specific meaning.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TxPermitResourceId([u8; 16]);

impl TxPermitResourceId {
    /// Construct a resource identity from caller-owned bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete resource identity bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Exact transport-owned resource units required before a packet exposes bytes.
///
/// The interface actor chooses the resource identity and unit semantics. The
/// authorization policy must validate those semantics; node core only requires
/// a nonzero request and an exact-resource covering reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxPermitRequirements {
    resource: TxPermitResourceId,
    required_units: NonZeroU64,
}

impl TxPermitRequirements {
    /// Validate a nonzero requirement for one exact authorization resource.
    pub const fn try_new(
        resource: TxPermitResourceId,
        required_units: u64,
    ) -> Result<Self, TxPermitRequirementsError> {
        match NonZeroU64::new(required_units) {
            Some(required_units) => Ok(Self {
                resource,
                required_units,
            }),
            None => Err(TxPermitRequirementsError::ZeroRequiredUnits),
        }
    }

    /// Exact authorization resource requested by the interface actor.
    pub const fn resource(self) -> TxPermitResourceId {
        self.resource
    }

    /// Nonzero transport-defined units required for this packet.
    pub const fn required_units(self) -> u64 {
        self.required_units.get()
    }
}

/// Invalid transport-neutral permit requirements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxPermitRequirementsError {
    /// An authorization request cannot require zero resource units.
    ZeroRequiredUnits,
}

/// Resource units atomically reserved by an authorization policy.
///
/// A reservation covers requirements only when its resource identity matches
/// exactly and it reserves at least the required number of units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxPermitReservation {
    resource: TxPermitResourceId,
    reserved_units: NonZeroU64,
}

impl TxPermitReservation {
    /// Validate a nonzero reservation for one exact authorization resource.
    pub const fn try_new(
        resource: TxPermitResourceId,
        reserved_units: u64,
    ) -> Result<Self, TxPermitReservationError> {
        match NonZeroU64::new(reserved_units) {
            Some(reserved_units) => Ok(Self {
                resource,
                reserved_units,
            }),
            None => Err(TxPermitReservationError::ZeroReservedUnits),
        }
    }

    /// Exact authorization resource reserved by the policy.
    pub const fn resource(self) -> TxPermitResourceId {
        self.resource
    }

    /// Nonzero transport-defined units atomically reserved at authorization.
    pub const fn reserved_units(self) -> u64 {
        self.reserved_units.get()
    }

    fn covers(self, requirements: TxPermitRequirements) -> bool {
        self.resource == requirements.resource
            && self.reserved_units.get() >= requirements.required_units.get()
    }
}

/// Invalid transport-neutral permit reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxPermitReservationError {
    /// An authorization policy cannot reserve zero resource units.
    ZeroReservedUnits,
}

impl OwnedTx<'_> {
    fn bound(&self) -> (PreparedPacket, Option<HopBinding>) {
        match self.buffer.binding {
            BufferBinding::Bound {
                dispatch_generation,
                prepared,
                hop,
            } if dispatch_generation == self.dispatch.generation
                && prepared.slot_id() == self.dispatch.slot =>
            {
                (prepared, hop)
            }
            BufferBinding::Unregistered
            | BufferBinding::Available { .. }
            | BufferBinding::Bound { .. } => {
                unreachable!("private TX owner and packet-buffer binding diverged")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PermitBinding {
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    slot: PacketSlotId,
    dispatch_generation: u64,
    interface: PacketInterfaceId,
    hop_generation: u64,
    requirements: TxPermitRequirements,
}

impl PermitBinding {
    fn from_owner(owner: &OwnedTx<'_>, requirements: TxPermitRequirements) -> Self {
        let (prepared, hop) = owner.bound();
        let hop = hop.expect("routed TX owner must retain a selected hop");
        Self {
            owner_identity: prepared.handle.owner_identity,
            instance: prepared.handle.instance,
            slot: prepared.slot_id(),
            dispatch_generation: owner.dispatch.generation,
            interface: hop.interface,
            hop_generation: hop.generation,
            requirements,
        }
    }
}

/// Unique ownership of one routed packet before permit negotiation.
#[must_use = "dropping routed TX ownership quarantines its buffer and receipt"]
pub struct RoutedTxJob<'a> {
    owner: OwnedTx<'a>,
}

/// Product-facing name for the routed job produced by DATA preparation.
pub type TxJob<'a> = RoutedTxJob<'a>;

impl<'a> RoutedTxJob<'a> {
    fn prepared_inner(&self) -> PreparedPacket {
        self.owner.bound().0
    }

    fn hop_inner(&self) -> HopBinding {
        self.owner
            .bound()
            .1
            .expect("routed TX job must retain a selected hop")
    }

    /// Stable slot of the uniquely owned packet buffer.
    pub fn slot_id(&self) -> PacketSlotId {
        self.prepared_inner().slot_id()
    }

    /// Encoded packet length retained as scalar metadata.
    pub fn packet_len(&self) -> u16 {
        self.prepared_inner().packet_len()
    }

    /// Complete proof-correlation token for this dispatch.
    pub fn attempt(&self) -> AttemptToken {
        self.prepared_inner().attempt()
    }

    /// Attempt handle used for terminal acknowledgement.
    pub fn attempt_handle(&self) -> AttemptHandle {
        self.prepared_inner().handle()
    }

    /// RNS interface authorization retained at preparation time.
    pub fn target(&self) -> TxTarget {
        self.prepared_inner().target()
    }

    /// Deadline bound to this external-buffer owner generation.
    pub fn deadline(&self) -> TxLeaseDeadline {
        self.prepared_inner().deadline()
    }

    /// Currently selected interface in the deterministic serialized route.
    pub fn interface(&self) -> PacketInterfaceId {
        self.hop_inner().interface
    }

    /// Copy all scalar packet metadata.
    pub fn prepared(&self) -> PreparedPacket {
        self.prepared_inner()
    }

    /// Consume routed ownership into a permit-pending owner and scalar request.
    pub fn begin_permit(
        self,
        requirements: TxPermitRequirements,
    ) -> (PermitPendingTx<'a>, TxPermitRequest) {
        let binding = PermitBinding::from_owner(&self.owner, requirements);
        (
            PermitPendingTx {
                owner: self.owner,
                requirements,
            },
            TxPermitRequest { binding },
        )
    }

    /// Return this hop without requesting authorization.
    pub fn return_unpermitted(self) -> UnpermittedTx<'a> {
        UnpermittedTx {
            owner: self.owner,
            denial: None,
        }
    }

    /// Convert a control-plane or driver fault into an owning recovery return.
    pub fn recovery_fault(self, code: TxCompletionCode) -> TxCompletion<'a> {
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::RecoveryFault(code),
        }
    }
}

/// Non-copy scalar request for one exact routed hop.
#[must_use = "a permit request must be resolved or retained with its pending owner"]
pub struct TxPermitRequest {
    binding: PermitBinding,
}

/// Scalar candidate presented to the authorization policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxAuthorizationCandidate {
    /// Selected packet interface.
    pub interface: PacketInterfaceId,
    /// Complete encoded packet length.
    pub packet_len: u16,
    /// Exact transport-owned resource units requested by the dispatcher.
    pub requirements: TxPermitRequirements,
    /// Monotonic sample supplied to `authorize_tx` for this policy evaluation.
    ///
    /// This is the sample used for the immediately preceding owner-deadline
    /// check. It does not imply that the policy will authorize the candidate.
    pub now: MonotonicMillis,
    /// Return/recovery deadline.
    pub deadline: TxLeaseDeadline,
    /// Whether an earlier hop may already have transmitted.
    pub may_have_transmitted: bool,
}

/// Closed authorization decision vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxPolicyDecision {
    /// Permit this exact routed hop.
    Authorize(TxPermitReservation),
    /// Deny this exact routed hop.
    Deny(TxPolicyDenial),
}

/// Bounded policy-denial vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxPolicyDenial {
    /// General policy rejected the operation.
    PolicyDenied,
    /// The requested authorization resource is unavailable.
    ResourceUnavailable,
    /// The requested resource capacity could not be reserved.
    CapacityUnavailable,
}

/// Synchronous policy invoked at the node-owner linearization point.
pub trait TxAuthorizationPolicy {
    /// Decide whether one fully validated candidate may be authorized.
    fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision;
}

/// Reason a valid permit request was denied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxPermitDenialReason {
    /// Policy rejected the otherwise valid request.
    Policy(TxPolicyDenial),
    /// The delivery attempt became terminal before authorization.
    AttemptTerminal(AttemptOutcome),
    /// The packet-owner deadline expired before authorization.
    DeadlineExpired,
    /// The dispatch was already in retained recovery.
    RecoveryRequired,
}

/// Non-copy grant for one exact routed hop.
#[must_use = "a permit grant must resolve its matching pending owner"]
pub struct TxPermitGrant {
    binding: PermitBinding,
    reservation: TxPermitReservation,
}

impl TxPermitGrant {
    /// Exact reservation atomically consumed by this grant.
    pub const fn reservation(&self) -> TxPermitReservation {
        self.reservation
    }
}

/// Non-copy denial for one exact routed hop.
#[must_use = "a permit denial must resolve its matching pending owner"]
pub struct TxPermitDenial {
    binding: PermitBinding,
    reason: TxPermitDenialReason,
}

impl TxPermitDenial {
    /// Typed denial reason.
    pub const fn reason(&self) -> TxPermitDenialReason {
        self.reason
    }
}

/// Non-copy reply to one exact permit request.
#[must_use = "a permit reply must resolve its matching pending owner"]
pub enum TxPermitReply {
    /// Authorization succeeded and is conservatively possibly transmitted.
    Granted(TxPermitGrant),
    /// Authorization was denied before transmission authorization.
    Denied(TxPermitDenial),
}

impl TxPermitReply {
    fn binding(&self) -> PermitBinding {
        match self {
            Self::Granted(grant) => grant.binding,
            Self::Denied(denial) => denial.binding,
        }
    }
}

/// Scalar request validation failure retaining the unchanged request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxAuthorizationErrorKind {
    /// Request belongs to another node owner or incarnation.
    WrongOwner,
    /// Slot, dispatch, interface, or hop generation is stale.
    StaleOrUnknown,
    /// Private dispatch and receipt metadata disagree.
    Invariant,
}

/// Authorization failure retaining the non-copy request.
#[must_use = "authorization failure retains the exact request"]
pub struct TxAuthorizationFailure {
    reason: TxAuthorizationErrorKind,
    request: TxPermitRequest,
}

impl TxAuthorizationFailure {
    /// Typed validation failure.
    pub const fn reason(&self) -> TxAuthorizationErrorKind {
        self.reason
    }

    /// Recover the unchanged request.
    pub fn into_request(self) -> TxPermitRequest {
        self.request
    }
}

/// Unique packet ownership while a scalar permit reply is outstanding.
#[must_use = "pending TX ownership must resolve a reply or enter recovery"]
pub struct PermitPendingTx<'a> {
    owner: OwnedTx<'a>,
    requirements: TxPermitRequirements,
}

impl<'a> PermitPendingTx<'a> {
    /// Resolve a matching grant or denial into the corresponding typestate.
    // The allocation-free mismatch deliberately retains both unique packet
    // ownership and the exact non-Copy reply. Boxing or dropping either side
    // would violate the handoff recovery contract.
    #[allow(clippy::result_large_err)]
    pub fn resolve(
        self,
        reply: TxPermitReply,
        now: MonotonicMillis,
    ) -> Result<PermitResolution<'a>, PermitReplyMismatch<'a>> {
        let expected = PermitBinding::from_owner(&self.owner, self.requirements);
        if reply.binding() != expected {
            return Err(PermitReplyMismatch {
                pending: self,
                reply,
            });
        }
        match reply {
            TxPermitReply::Granted(grant) if now >= self.owner.bound().0.deadline().instant() => {
                Ok(PermitResolution::Expired(ExpiredAuthorizedTx {
                    owner: self.owner,
                    grant,
                }))
            }
            TxPermitReply::Granted(grant) => Ok(PermitResolution::Authorized(AuthorizedTx {
                owner: self.owner,
                grant,
                frame_taken: false,
            })),
            TxPermitReply::Denied(denial) => Ok(PermitResolution::Unpermitted(UnpermittedTx {
                owner: self.owner,
                denial: Some(denial.reason),
            })),
        }
    }

    /// Convert a lost or corrupt control-plane exchange into recovery.
    pub fn recovery_fault(self, code: TxCompletionCode) -> TxCompletion<'a> {
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::RecoveryFault(code),
        }
    }
}

/// Successful permit-reply resolution.
#[must_use = "resolved TX ownership must be completed or retained"]
pub enum PermitResolution<'a> {
    /// Authorized owner and its one-shot frame capability.
    Authorized(AuthorizedTx<'a>),
    /// Grant arrived at or after its owner deadline and cannot expose bytes.
    Expired(ExpiredAuthorizedTx<'a>),
    /// Definitely-unpermitted owner.
    Unpermitted(UnpermittedTx<'a>),
}

/// Reply mismatch retaining both unique pending ownership and scalar reply.
#[must_use = "permit mismatch retains pending ownership and the unmatched reply"]
pub struct PermitReplyMismatch<'a> {
    pending: PermitPendingTx<'a>,
    reply: TxPermitReply,
}

impl<'a> PermitReplyMismatch<'a> {
    /// Recover pending ownership and the unchanged unmatched reply.
    pub fn into_parts(self) -> (PermitPendingTx<'a>, TxPermitReply) {
        (self.pending, self.reply)
    }
}

/// Failure to expose an authorized frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxFrameError {
    /// The one-shot frame capability was already consumed.
    AlreadyTaken,
    /// The owner deadline was reached before frame bytes were borrowed.
    DeadlineExpired {
        /// Deadline that prohibited byte exposure.
        deadline: TxLeaseDeadline,
    },
    /// Grant and uniquely owned buffer metadata disagree.
    Invariant,
}

/// Transport-neutral scalar observation of one complete authorized frame.
///
/// The full-packet digest is calculated from the exact bytes exposed by the
/// authorized typestate. It is deliberately distinct from [`AttemptToken`],
/// which covers Reticulum's proof-correlated hashable portion rather than the
/// complete encoded interface packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizedFrameObservation {
    handle: AttemptHandle,
    attempt: AttemptToken,
    interface: PacketInterfaceId,
    packet_len: usize,
    encoded_packet_sha256: EncodedPacketSha256,
}

impl AuthorizedFrameObservation {
    /// Generation-checked attempt identity associated with these exact bytes.
    pub const fn attempt_handle(self) -> AttemptHandle {
        self.handle
    }

    /// Complete proof-correlation hash associated with these exact bytes.
    pub const fn attempt(self) -> AttemptToken {
        self.attempt
    }

    /// Packet interface authorized for this exact hop.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Complete encoded interface-packet length.
    pub const fn packet_len(self) -> usize {
        self.packet_len
    }

    /// Independently calculated SHA-256 over every encoded packet byte.
    pub const fn encoded_packet_sha256(self) -> EncodedPacketSha256 {
        self.encoded_packet_sha256
    }
}

/// Borrowed bytes exposed only by an authorized typestate.
pub struct TxFrame<'a> {
    bytes: &'a [u8],
    handle: AttemptHandle,
    attempt: AttemptToken,
    interface: PacketInterfaceId,
}

impl TxFrame<'_> {
    /// Complete encoded Reticulum packet bytes.
    pub const fn bytes(&self) -> &[u8] {
        self.bytes
    }

    /// Proof-correlation token for these bytes.
    pub const fn attempt(&self) -> AttemptToken {
        self.attempt
    }

    /// Generation-checked attempt identity for these exact bytes.
    pub const fn attempt_handle(&self) -> AttemptHandle {
        self.handle
    }

    /// Interface authorized for this exact frame.
    pub const fn interface(&self) -> PacketInterfaceId {
        self.interface
    }

    /// Observe transport-neutral scalar evidence for these exact bytes.
    ///
    /// The full-packet digest is recomputed here rather than copied from the
    /// preparation metadata, allowing a later consumer to compare the two
    /// independently derived values.
    pub fn observation(&self) -> AuthorizedFrameObservation {
        AuthorizedFrameObservation {
            handle: self.handle,
            attempt: self.attempt,
            interface: self.interface,
            packet_len: self.bytes.len(),
            encoded_packet_sha256: EncodedPacketSha256::new(Sha256::digest(self.bytes).into()),
        }
    }
}

/// Unique packet ownership after authorization.
#[must_use = "authorized TX ownership is conservatively possibly transmitted"]
pub struct AuthorizedTx<'a> {
    owner: OwnedTx<'a>,
    grant: TxPermitGrant,
    frame_taken: bool,
}

impl<'a> AuthorizedTx<'a> {
    /// Exact reservation atomically consumed by this authorization.
    pub const fn reservation(&self) -> TxPermitReservation {
        self.grant.reservation
    }

    /// Borrow the authorized packet frame once.
    pub fn frame(&mut self, now: MonotonicMillis) -> Result<TxFrame<'_>, TxFrameError> {
        if self.frame_taken {
            return Err(TxFrameError::AlreadyTaken);
        }
        let binding = PermitBinding::from_owner(&self.owner, self.grant.binding.requirements);
        if binding != self.grant.binding {
            return Err(TxFrameError::Invariant);
        }
        let (prepared, _) = self.owner.bound();
        if now >= prepared.deadline().instant() {
            self.frame_taken = true;
            return Err(TxFrameError::DeadlineExpired {
                deadline: prepared.deadline(),
            });
        }
        let bytes = self
            .owner
            .buffer
            .bytes
            .get(..usize::from(prepared.packet_len()))
            .ok_or(TxFrameError::Invariant)?;
        self.frame_taken = true;
        Ok(TxFrame {
            bytes,
            handle: prepared.handle(),
            attempt: prepared.attempt(),
            interface: binding.interface,
        })
    }

    /// Complete this authorized hop as transmitted or possibly transmitted.
    pub fn complete(self, code: TxCompletionCode) -> TxCompletion<'a> {
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::Authorized(code),
        }
    }

    /// Complete this authorized hop with a retained recovery fault.
    pub fn recovery_fault(self, code: TxCompletionCode) -> TxCompletion<'a> {
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::RecoveryFault(code),
        }
    }
}

/// Authorized owner whose delayed grant can no longer expose packet bytes.
///
/// Permit issuance was irrevocable, so this return remains conservatively
/// possibly transmitted even though this typestate offers no frame accessor.
#[must_use = "an expired authorized TX owner must return to node-core"]
pub struct ExpiredAuthorizedTx<'a> {
    owner: OwnedTx<'a>,
    grant: TxPermitGrant,
}

impl<'a> ExpiredAuthorizedTx<'a> {
    /// Expired deadline that prevented frame exposure.
    pub fn deadline(&self) -> TxLeaseDeadline {
        self.owner.bound().0.deadline()
    }

    /// Complete the expired authorized hop as possibly transmitted.
    pub fn complete(self, code: TxCompletionCode) -> TxCompletion<'a> {
        let _ = self.grant.binding;
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::Authorized(code),
        }
    }

    /// Return a control-plane or driver recovery fault.
    pub fn recovery_fault(self, code: TxCompletionCode) -> TxCompletion<'a> {
        let _ = self.grant.binding;
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::RecoveryFault(code),
        }
    }
}

/// Unique packet ownership for a hop that was definitely not authorized.
#[must_use = "unpermitted TX ownership must return to node-core"]
pub struct UnpermittedTx<'a> {
    owner: OwnedTx<'a>,
    denial: Option<TxPermitDenialReason>,
}

impl<'a> UnpermittedTx<'a> {
    /// Permit denial that produced this owner, if it reached policy handling.
    pub const fn denial(&self) -> Option<TxPermitDenialReason> {
        self.denial
    }

    /// Complete this hop as definitely unsent.
    pub fn complete(self, code: TxCompletionCode) -> TxCompletion<'a> {
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::Unpermitted {
                code,
                denial: self.denial,
            },
        }
    }

    /// Complete this unpermitted hop with a retained recovery fault.
    pub fn recovery_fault(self, code: TxCompletionCode) -> TxCompletion<'a> {
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::RecoveryFault(code),
        }
    }
}

/// Bounded driver/control-plane completion code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxCompletionCode(u16);

impl TxCompletionCode {
    /// Construct a project-owned completion code.
    pub const fn new(code: u16) -> Self {
        Self(code)
    }

    /// Raw bounded completion code.
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy)]
enum CompletionKind {
    Unpermitted {
        code: TxCompletionCode,
        denial: Option<TxPermitDenialReason>,
    },
    Authorized(TxCompletionCode),
    RecoveryFault(TxCompletionCode),
}

/// Owning return from one exact routed hop.
#[must_use = "a TX completion retains the unique packet buffer"]
pub struct TxCompletion<'a> {
    owner: OwnedTx<'a>,
    kind: CompletionKind,
}

impl TxCompletion<'_> {
    /// Copy the scalar packet metadata carried by this exact owning return.
    pub fn prepared(&self) -> PreparedPacket {
        self.owner.bound().0
    }

    /// Interface selected for the exact hop carried by this owning return.
    pub fn interface(&self) -> PacketInterfaceId {
        self.owner
            .bound()
            .1
            .expect("TX completion must retain a selected hop")
            .interface
    }
}

/// Phase active when a dispatch entered recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxRecoveryPriorPhase {
    /// No permit had been issued for the current hop.
    Unpermitted,
    /// A permit had been issued and transmission may have started.
    Authorized,
}

/// Reason retained by one recovery-required dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxRecoveryReason {
    /// Packet-owner deadline expired.
    DeadlineExpired,
    /// Interface or control plane returned a recovery fault.
    CompletionFault(TxCompletionCode),
    /// Exact receipt cancellation unexpectedly failed.
    ReceiptCancellationFailed,
    /// Per-hop generation space was exhausted during fan-out.
    HopIdentifierExhausted,
    /// Same-owner scalar and unique-owner metadata disagreed.
    Invariant,
}

/// Scalar retained recovery record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxRecoveryRecord {
    instance: NodeInstanceId,
    slot: PacketSlotId,
    dispatch_generation: u64,
    interface: Option<PacketInterfaceId>,
    deadline: TxLeaseDeadline,
    observed_at: MonotonicMillis,
    prior_phase: TxRecoveryPriorPhase,
    may_have_transmitted: bool,
    reason: TxRecoveryReason,
}

impl TxRecoveryRecord {
    /// Node-owner incarnation that created this retained lease record.
    pub const fn instance(self) -> NodeInstanceId {
        self.instance
    }

    /// Packet slot retained in recovery.
    pub const fn slot_id(self) -> PacketSlotId {
        self.slot
    }

    /// Generation of the dispatch retained in recovery.
    pub const fn dispatch_generation(self) -> u64 {
        self.dispatch_generation
    }

    /// Interface active at recovery, if routing had selected one.
    pub const fn interface(self) -> Option<PacketInterfaceId> {
        self.interface
    }

    /// Original packet-owner deadline.
    pub const fn deadline(self) -> TxLeaseDeadline {
        self.deadline
    }

    /// Monotonic instant at which recovery was observed.
    pub const fn observed_at(self) -> MonotonicMillis {
        self.observed_at
    }

    /// Phase active before recovery.
    pub const fn prior_phase(self) -> TxRecoveryPriorPhase {
        self.prior_phase
    }

    /// Whether this dispatch may have transmitted on any hop.
    pub const fn may_have_transmitted(self) -> bool {
        self.may_have_transmitted
    }

    /// Retained recovery reason.
    pub const fn reason(self) -> TxRecoveryReason {
        self.reason
    }
}

/// Generation-safe correlation for one retained recovery record.
///
/// This copy-only observation combines the volatile attempt identity and
/// proof token with recovery metadata without embedding either value in the
/// durable-format-neutral [`TxRecoveryRecord`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxRecoveryObservation {
    handle: AttemptHandle,
    attempt: AttemptToken,
    record: TxRecoveryRecord,
}

impl TxRecoveryObservation {
    fn from_prepared(prepared: PreparedPacket, record: TxRecoveryRecord) -> Self {
        Self {
            handle: prepared.handle(),
            attempt: prepared.attempt(),
            record,
        }
    }

    /// Generation-checked attempt identity associated with this recovery.
    pub const fn attempt_handle(self) -> AttemptHandle {
        self.handle
    }

    /// Complete proof-correlation hash associated with this recovery.
    pub const fn attempt(self) -> AttemptToken {
        self.attempt
    }

    /// Scalar retained recovery metadata.
    pub const fn record(self) -> TxRecoveryRecord {
        self.record
    }
}

/// Unique packet buffer retained in fail-closed recovery.
#[must_use = "quarantine is the only owner of a recovery buffer"]
pub struct TxQuarantine<'a> {
    owner: OwnedTx<'a>,
    record: TxRecoveryRecord,
}

impl TxQuarantine<'_> {
    /// Scalar retained recovery record.
    pub const fn record(&self) -> TxRecoveryRecord {
        self.record
    }

    /// Generation-safe scalar observation while this quarantine retains its
    /// unique owner.
    pub fn observation(&self) -> TxRecoveryObservation {
        TxRecoveryObservation::from_prepared(self.owner.bound().0, self.record)
    }

    /// Generation-checked attempt identity retained by this quarantine.
    pub fn attempt_handle(&self) -> AttemptHandle {
        self.observation().attempt_handle()
    }

    /// Complete proof-correlation hash retained by this quarantine.
    pub fn attempt(&self) -> AttemptToken {
        self.observation().attempt()
    }

    /// Stable slot of the quarantined buffer.
    pub fn slot_id(&self) -> PacketSlotId {
        self.owner.dispatch.slot
    }
}

enum PrepareFailureOwner<'a> {
    Available(&'a mut TxPacketBuffer),
    Quarantined(TxQuarantine<'a>),
}

/// Preparation rejection or fail-closed route-cancellation fault.
#[must_use = "preparation failure retains the exact packet buffer owner"]
pub struct PrepareFailure<'a> {
    reason: SubmitError,
    owner: PrepareFailureOwner<'a>,
}

impl<'a> PrepareFailure<'a> {
    /// Typed reason the preparation transaction failed.
    pub const fn reason(&self) -> SubmitError {
        self.reason
    }

    /// Recover an available buffer, or the owning quarantine when fail-closed.
    pub fn into_buffer(self) -> Result<&'a mut TxPacketBuffer, TxQuarantine<'a>> {
        match self.owner {
            PrepareFailureOwner::Available(buffer) => Ok(buffer),
            PrepareFailureOwner::Quarantined(quarantine) => Err(quarantine),
        }
    }
}

/// Failure while rolling back a routed, definitely-unsent dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RollbackErrorKind {
    /// The job belongs to another node owner, incarnation, slot, or generation.
    StaleOrUnknown,
    /// Buffer, dispatch, and attempt metadata disagree.
    Invariant,
    /// An earlier serialized route was already authorized.
    PriorAuthorization,
    /// The corresponding RNS receipt was already absent.
    ReceiptMissing {
        /// Attempt whose receipt could not be cancelled.
        attempt: AttemptToken,
    },
}

/// Rollback failure retaining the exact routed job.
#[must_use = "rollback failure retains the unique routed job"]
pub struct RollbackFailure<'a> {
    reason: RollbackErrorKind,
    job: RoutedTxJob<'a>,
}

impl<'a> RollbackFailure<'a> {
    /// Typed rollback failure.
    pub const fn reason(&self) -> RollbackErrorKind {
        self.reason
    }

    /// Recover the still-bound routed job.
    pub fn into_job(self) -> RoutedTxJob<'a> {
        self.job
    }
}

/// Successful node-owner handling of an owning completion.
#[must_use = "completion disposition retains or returns the unique packet buffer"]
pub enum TxCompletionDisposition<'a> {
    /// Continue serialized fan-out on the next deterministic interface.
    Next(RoutedTxJob<'a>),
    /// Packet buffer is available for reuse.
    Available(&'a mut TxPacketBuffer),
    /// A matching late owner was finalized and its buffer is reusable.
    Recovered {
        /// Reusable packet buffer returned by the exact recovery lease.
        buffer: &'a mut TxPacketBuffer,
        /// Generation-safe recovery observation finalized by this return.
        observation: TxRecoveryObservation,
    },
    /// Packet buffer remains fail-closed in recovery.
    Quarantined(TxQuarantine<'a>),
}

/// Completion validation failure retaining the unchanged owning completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxCompletionErrorKind {
    /// Completion belongs to another node owner or incarnation.
    WrongOwner,
    /// Slot or dispatch generation is stale.
    StaleOrUnknown,
}

/// Owning completion failure.
#[must_use = "completion failure retains the unchanged unique owner"]
pub struct TxCompletionFailure<'a> {
    reason: TxCompletionErrorKind,
    completion: TxCompletion<'a>,
}

impl<'a> TxCompletionFailure<'a> {
    /// Typed completion validation failure.
    pub const fn reason(&self) -> TxCompletionErrorKind {
        self.reason
    }

    /// Recover the unchanged owning completion.
    pub fn into_completion(self) -> TxCompletion<'a> {
        self.completion
    }
}

/// Failure to acknowledge a terminal DATA attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcknowledgeError {
    /// The handle belongs to another node/incarnation or no longer identifies
    /// the same attempt generation.
    StaleOrUnknown,
    /// The attempt is still awaiting a proof or timeout.
    NotTerminal,
    /// An external job still owns the packet bytes for this terminal attempt.
    PacketStillBound,
    /// Private packet/attempt state violates the fixed-pool invariant.
    Invariant,
}

/// Correlation fault between RNS receipt state and the fixed attempt ledger.
///
/// These are invariants, not retryable capacity failures. The triggering
/// proof or timeout remains uncommitted in RNS for diagnosis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptCorrelationError {
    /// RNS presented a channel receipt, which this DATA-only owner cannot
    /// represent.
    UnsupportedChannel,
    /// RNS presented a DATA receipt absent from the fixed ledger.
    UnknownData {
        /// Complete proof-correlation hash RNS presented.
        attempt: AttemptToken,
    },
    /// RNS presented a receipt already retained as a terminal tombstone.
    AlreadyTerminal {
        /// Complete proof-correlation hash RNS presented.
        attempt: AttemptToken,
    },
    /// RNS refused the sink without presenting a classifiable candidate.
    ReservationInvariant,
}

/// Result of one timer-maintenance pass through RNS and the attempt ledger.
#[derive(Debug)]
#[must_use = "timer actions and receipt correlation faults must be handled"]
pub struct MaintenanceReport {
    /// Ordinary RNS events and resolved outbound packets.
    pub actions: NodeActions,
    /// Number of DATA timeouts committed to terminal tombstones in this pass.
    pub timed_out_attempts: usize,
    /// Compact first-eight-byte tag of the first timed-out receipt in this
    /// maintenance pass.
    pub timed_out_attempt_tag: Option<u64>,
    /// Whether every timeout in this pass carried the same compact tag.
    pub timed_out_attempt_tags_consistent: bool,
    /// A non-retryable mismatch between native receipts and the fixed ledger.
    pub correlation_fault: Option<ReceiptCorrelationError>,
}

/// Scalar occupancy snapshot for external dispatch metadata and DATA receipts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacitySnapshot {
    /// Packet buffers registered with this owner incarnation.
    pub packet_buffers_registered: usize,
    /// Registered buffers currently bound to queued jobs.
    pub dispatches_queued: usize,
    /// Dispatches whose current hop has been authorized.
    pub dispatches_authorized: usize,
    /// Dispatches retained in recovery-required state.
    pub dispatches_recovery_required: usize,
    /// Registered buffers reserved or queued by a transaction.
    pub dispatches_used: usize,
    /// Configured dispatch-metadata limit.
    pub dispatches_limit: usize,
    /// Outstanding destination-DATA receipts retained by RNS.
    pub receipts_used: usize,
    /// Configured destination-DATA receipt limit.
    pub receipts_limit: usize,
    /// Attempts still awaiting a proof or timeout.
    pub attempts_active: usize,
    /// Terminal attempts retained until explicit acknowledgement.
    pub attempts_terminal: usize,
    /// Total reserved, active, or terminal attempt slots.
    pub attempts_used: usize,
    /// Configured attempt-ledger limit.
    pub attempts_limit: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AttemptRef {
    slot: usize,
    generation: u64,
}

#[derive(Clone, Copy)]
enum DispatchPhase {
    Routed,
    Authorized,
    RecoveryRequired(TxRecoveryRecord),
}

#[derive(Clone, Copy)]
struct DispatchRecord {
    prepared: RnsPreparedData,
    encoded_packet_sha256: EncodedPacketSha256,
    attempt: AttemptRef,
    generation: u64,
    deadline: TxLeaseDeadline,
    selected: Option<HopBinding>,
    remaining: InterfaceSet,
    may_have_transmitted: bool,
    phase: DispatchPhase,
}

#[derive(Clone, Copy)]
enum DispatchState {
    Unregistered,
    Free,
    Reserved { attempt: AttemptRef },
    Active(DispatchRecord),
}

/// Result of one packet-owner deadline maintenance pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "new recovery-required transitions must be observed"]
pub struct TxMaintenanceReport {
    /// Dispatches newly moved into recovery-required state.
    pub newly_recovery_required: usize,
}

/// Iterator over retained scalar TX recovery records.
pub struct TxRecoveryRecords<'a> {
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    dispatches: &'a [DispatchSlot],
    next: usize,
}

impl Iterator for TxRecoveryRecords<'_> {
    type Item = TxRecoveryObservation;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(slot) = self.dispatches.get(self.next) {
            self.next += 1;
            if let DispatchState::Active(record) = slot.state
                && let DispatchPhase::RecoveryRequired(recovery) = record.phase
            {
                return Some(TxRecoveryObservation {
                    handle: AttemptHandle {
                        owner_identity: self.owner_identity,
                        instance: self.instance,
                        slot: record.attempt.slot,
                        generation: record.attempt.generation,
                    },
                    attempt: AttemptToken(*record.prepared.receipt().as_bytes()),
                    record: recovery,
                });
            }
        }
        None
    }
}

#[derive(Clone, Copy)]
struct DispatchSlot {
    state: DispatchState,
}

impl DispatchSlot {
    const EMPTY: Self = Self {
        state: DispatchState::Unregistered,
    };
}

#[derive(Clone, Copy)]
enum AttemptState {
    Free,
    Reserved {
        generation: u64,
    },
    Active {
        generation: u64,
        token: AttemptToken,
    },
    Terminal {
        generation: u64,
        token: AttemptToken,
        outcome: AttemptOutcome,
    },
}

#[derive(Clone, Copy)]
struct AttemptSlot {
    state: AttemptState,
}

impl AttemptSlot {
    const EMPTY: Self = Self {
        state: AttemptState::Free,
    };
}

/// Iterator over terminal DATA attempts awaiting acknowledgement.
pub struct TerminalAttempts<'a> {
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    attempts: &'a [AttemptSlot],
    next: usize,
}

impl Iterator for TerminalAttempts<'_> {
    type Item = TerminalAttempt;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((slot, attempt)) = self.attempts.get(self.next).map(|value| {
            let slot = self.next;
            self.next += 1;
            (slot, value)
        }) {
            if let AttemptState::Terminal {
                generation,
                token,
                outcome,
            } = attempt.state
            {
                return Some(TerminalAttempt {
                    handle: AttemptHandle {
                        owner_identity: self.owner_identity,
                        instance: self.instance,
                        slot,
                        generation,
                    },
                    token,
                    outcome,
                });
            }
        }
        None
    }
}

/// Single owner of a concrete bounded RNS node and external-buffer metadata.
///
/// `PATHS` also bounds destination-DATA receipts in the current Rete storage
/// backend and this owner's attempt ledger. `PATHS` and `LINKS` must be powers
/// of two greater than one; `ANNOUNCES` and `DEDUPLICATION` must be nonzero.
/// `PACKET_BUFFERS` independently bounds registered external packet buffers
/// and their dispatch metadata and may be zero. The 500-byte arrays live
/// outside this value.
pub struct NodeCore<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const PACKET_BUFFERS: usize,
> {
    rns: RnsNode<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>,
    owner_identity: [u8; 16],
    instance: NodeInstanceId,
    dispatches: [DispatchSlot; PACKET_BUFFERS],
    attempts: [AttemptSlot; PATHS],
    next_registration: usize,
    next_attempt: usize,
    dispatch_generation: u64,
    hop_generation: u64,
    attempt_generation: u64,
    ordinary_action_owner_claimed: bool,
}

impl<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
    const PACKET_BUFFERS: usize,
> NodeCore<PATHS, ANNOUNCES, DEDUPLICATION, LINKS, PACKET_BUFFERS>
{
    /// Construct the concrete RNS owner and empty external-buffer metadata.
    pub fn new(
        identity: NodeIdentity,
        app_name: &str,
        aspects: &[&str],
        instance: NodeInstanceId,
        config: NodeConfig,
    ) -> Result<Self, NodeConstructionError> {
        const {
            assert!(
                PATHS > 1 && PATHS.is_power_of_two(),
                "node-core PATHS must be a power of two greater than one"
            );
            assert!(
                ANNOUNCES > 0,
                "node-core ANNOUNCES must be greater than zero"
            );
            assert!(
                DEDUPLICATION > 0,
                "node-core DEDUPLICATION must be greater than zero"
            );
            assert!(
                LINKS > 1 && LINKS.is_power_of_two(),
                "node-core LINKS must be a power of two greater than one"
            );
            assert!(
                PACKET_BUFFERS <= (u16::MAX as usize) + 1,
                "node-core PACKET_BUFFERS must fit the 16-bit packet-slot namespace"
            );
        }
        let role = match config.role {
            NodeRole::Endpoint => RnsNodeRole::Endpoint,
            NodeRole::Transport => RnsNodeRole::Transport,
        };
        let rns = RnsNode::new(
            identity.0,
            app_name,
            aspects,
            RnsNodeConfig {
                role,
                max_additional_destinations: config.max_additional_destinations,
            },
        )
        .map_err(|_| NodeConstructionError::InvalidRnsConfiguration)?;
        let owner_identity = *rns.identity_hash().as_bytes();

        Ok(Self {
            rns,
            owner_identity,
            instance,
            dispatches: [DispatchSlot::EMPTY; PACKET_BUFFERS],
            attempts: [AttemptSlot::EMPTY; PATHS],
            next_registration: 0,
            next_attempt: 0,
            dispatch_generation: 0,
            hop_generation: 0,
            attempt_generation: 0,
            ordinary_action_owner_claimed: false,
        })
    }

    /// Primary local destination hash.
    pub fn destination_hash(&self) -> DestinationHash {
        DestinationHash::new(*self.rns.destination_hash().as_bytes())
    }

    /// Register one additional inbound encrypted destination owned by this
    /// node identity.
    ///
    /// Protocol services such as `lxmf.delivery` call this before the node is
    /// moved into the permanent interface supervisor. The narrow wrapper does
    /// not expose Rete destination objects or mutable protocol state.
    pub fn register_inbound_single_destination(
        &mut self,
        app_name: &str,
        aspects: &[&str],
    ) -> Result<DestinationHash, LocalDestinationRegistrationError> {
        self.rns
            .register_destination(
                app_name,
                aspects,
                RnsDestinationType::Single,
                RnsDirection::In,
            )
            .map(|destination| DestinationHash::new(*destination.as_bytes()))
            .map_err(LocalDestinationRegistrationError::from)
    }

    /// Compose and sign one basic opportunistic LXMF carrier with this node's
    /// registered inbound Single `lxmf.delivery` source.
    ///
    /// The caller supplies only the remote delivery destination, timestamp,
    /// title, content, and output storage. Source selection and private-key use
    /// remain inside the sole protocol owner. This operation does not prepare
    /// an RNS packet or consume receipt state.
    pub fn prepare_basic_lxmf_into(
        &self,
        destination: &DestinationHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        output: &mut [u8],
    ) -> Result<PreparedBasicLxmf, PrepareBasicLxmfError> {
        self.rns
            .prepare_basic_lxmf_into(
                &RnsDestinationHash::from(*destination.as_bytes()),
                timestamp_unix_ms,
                title,
                content,
                output,
            )
            .map(PreparedBasicLxmf)
            .map_err(PrepareBasicLxmfError::from)
    }

    /// Configure the default application data used for ordinary announces and
    /// path responses from one registered local destination.
    pub fn set_destination_announce_app_data(
        &mut self,
        destination: &DestinationHash,
        app_data: Option<&[u8]>,
    ) -> Result<(), LocalDestinationAnnounceAppDataError> {
        self.rns
            .set_destination_announce_app_data(
                &RnsDestinationHash::from(*destination.as_bytes()),
                app_data,
            )
            .map_err(LocalDestinationAnnounceAppDataError::from)
    }

    /// Copy a public key learned for one announced destination.
    ///
    /// Returning the fixed-size value by copy keeps signature verification
    /// independent of the sole mutable RNS owner's borrow lifetime.
    pub fn recall_identity(&self, destination: &DestinationHash) -> Option<[u8; 64]> {
        let destination = RnsDestinationHash::from(*destination.as_bytes());
        self.rns.recall_identity(&destination)
    }

    /// Confirm the synchronous interface-egress handoff for one token-bearing
    /// LINKREQUEST or LRPROOF packet.
    ///
    /// The first matching interface confirmation wins. A `false` return can
    /// therefore mean either that the token/interface/state was rejected or
    /// that an earlier serialized fan-out hop already confirmed this packet.
    pub fn confirm_outbound_protocol(
        &mut self,
        token: OutboundProtocolToken,
        interface: PacketInterfaceId,
        interval: OutboundDispatchInterval,
    ) -> bool {
        self.rns
            .confirm_outbound_protocol(token, interface.into_rns(), interval)
    }

    /// Caller-supplied identity of this in-memory node-owner incarnation.
    pub const fn instance_id(&self) -> NodeInstanceId {
        self.instance
    }

    /// Opaque exact-owner binding for persistent TX orchestration.
    pub const fn tx_owner_scope(&self) -> TxOwnerScope {
        TxOwnerScope {
            owner_identity: self.owner_identity,
            instance: self.instance,
        }
    }

    /// Issue this node incarnation's sole ordinary protocol-action owner.
    ///
    /// Ordinary slot IDs and generations are local to their fixed pool, so two
    /// pools sharing one [`TxOwnerScope`] could otherwise accept each other's
    /// same-slot, same-generation jobs. The claim is intentionally one-shot
    /// and is not released when the returned owner is dropped. Reconstructing
    /// a node requires a fresh [`NodeInstanceId`] before another ordinary pool
    /// may exist for that identity.
    pub fn take_ordinary_action_owner<const ORDINARY_PACKET_BUFFERS: usize>(
        &mut self,
    ) -> Result<OrdinaryActionOwner<ORDINARY_PACKET_BUFFERS>, OrdinaryActionOwnerClaimError> {
        if self.ordinary_action_owner_claimed {
            return Err(OrdinaryActionOwnerClaimError::AlreadyClaimed);
        }
        self.ordinary_action_owner_claimed = true;
        Ok(OrdinaryActionOwner::new(self.tx_owner_scope()))
    }

    /// Cache one peer identity for deterministic direct sending or tests.
    /// Learned announces will eventually drive the same native identity table.
    pub fn register_peer(
        &mut self,
        peer: &NodeIdentity,
        app_name: &str,
        aspects: &[&str],
        now: MonotonicSeconds,
    ) -> Result<(), PeerRegistrationError> {
        self.rns
            .register_peer(&peer.0, app_name, aspects, now.get())
            .map_err(|_| PeerRegistrationError::InvalidPeer)
    }

    /// Configure automatic delivery proofs for DATA received at this node's
    /// primary local destination.
    ///
    /// This changes protocol behavior only. Transmission remains subject to the
    /// runtime's action ownership, routing, permit, and interface-policy
    /// boundaries.
    pub fn set_inbound_proof_policy(&mut self, policy: InboundProofPolicy) {
        self.rns.set_inbound_proof_policy(policy.into_rns());
    }

    /// Configure automatic delivery proofs for DATA received at one local
    /// destination.
    ///
    /// Retained proofs remain attached to their exact application event until
    /// an [`ApplicationEventOwner`] moves them through a [`DelayedProofOwner`].
    /// The named destination must already be registered on this node.
    pub fn set_destination_inbound_proof_policy(
        &mut self,
        destination: &DestinationHash,
        policy: InboundProofPolicy,
    ) -> Result<(), LocalDestinationProofPolicyError> {
        self.rns
            .set_destination_inbound_proof_policy(
                &RnsDestinationHash::from(*destination.as_bytes()),
                policy.into_rns(),
            )
            .map_err(LocalDestinationProofPolicyError::from)
    }

    /// Enable or disable inbound Link admission for one local destination.
    ///
    /// Transit forwarding, DATA reception, and DATA proof policy are
    /// unaffected. This controls only whether this node terminates a Link
    /// addressed to the named local destination.
    pub fn set_destination_accepts_links(
        &mut self,
        destination: &DestinationHash,
        accepts: bool,
    ) -> Result<(), LocalDestinationLinkPolicyError> {
        if self
            .rns
            .set_accepts_links(&RnsDestinationHash::from(*destination.as_bytes()), accepts)
        {
            Ok(())
        } else {
            Err(LocalDestinationLinkPolicyError::DestinationNotRegistered)
        }
    }

    /// Admit one signed local announce into the bounded protocol queue.
    ///
    /// `app_data` is copied into protocol-owned pending-announce storage. The
    /// announce remains queued until [`Self::flush_announces`] makes its
    /// transmission action available to the runtime.
    pub fn queue_announce<R: RngCore + CryptoRng>(
        &mut self,
        app_data: Option<&[u8]>,
        emitted_at: AnnounceEmissionTime,
        rng: &mut R,
    ) -> Result<(), AnnounceAdmissionError> {
        self.rns
            .queue_announce(app_data, emitted_at.get(), rng)
            .map_err(AnnounceAdmissionError::from)
    }

    /// Admit one signed local announce for a registered destination into the
    /// bounded protocol queue.
    ///
    /// This preserves the same queue and payload admission as
    /// [`Self::queue_announce`] while allowing protocol services such as
    /// `lxmf.delivery` to advertise their distinct derived destination.
    pub fn queue_announce_for<R: RngCore + CryptoRng>(
        &mut self,
        destination: &DestinationHash,
        app_data: Option<&[u8]>,
        emitted_at: AnnounceEmissionTime,
        rng: &mut R,
    ) -> Result<(), AnnounceAdmissionError> {
        self.rns
            .queue_announce_for(
                &RnsDestinationHash::from(*destination.as_bytes()),
                app_data,
                emitted_at.get(),
                rng,
            )
            .map_err(AnnounceAdmissionError::from)
    }

    /// Flush every currently ready local announce into the ordinary runtime
    /// action envelope.
    ///
    /// The returned envelope has no application events and no unroutable
    /// packets. Each packet retains the broadcast target assigned by RNS. A
    /// caller must preserve those owned actions until its interface dispatcher
    /// boundary accepts or rejects them.
    pub fn flush_announces<R: RngCore>(
        &mut self,
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> NodeActions {
        NodeActions::without_retained_proofs(
            Default::default(),
            self.rns.flush_announces(now.get(), rng),
            0,
        )
    }

    /// Build one tagged Reticulum path request as an ordinary broadcast
    /// action. The caller retains the complete action envelope until the
    /// interface router accepts or explicitly rejects it.
    pub fn request_path<R: RngCore + CryptoRng>(
        &self,
        destination: &DestinationHash,
        rng: &mut R,
    ) -> Result<NodeActions, PathRequestError> {
        let packet = self
            .rns
            .request_path(&RnsDestinationHash::from(*destination.as_bytes()), rng)
            .map_err(PathRequestError::from)?;
        let mut packets = alloc::vec::Vec::new();
        packets
            .try_reserve_exact(1)
            .map_err(|_| PathRequestError::AllocationFailed)?;
        packets.push(packet);
        Ok(NodeActions::without_retained_proofs(
            Default::default(),
            packets,
            0,
        ))
    }

    /// Register one externally allocated packet buffer with this owner.
    ///
    /// Registration assigns the next stable slot exactly once. A buffer must
    /// be registered before it can enter the available/completion channel.
    pub fn register_packet_buffer(
        &mut self,
        buffer: &mut TxPacketBuffer,
    ) -> Result<PacketSlotId, BufferRegistrationError> {
        if !matches!(buffer.binding, BufferBinding::Unregistered) {
            return Err(BufferRegistrationError::AlreadyRegistered);
        }
        if PACKET_BUFFERS == 0 {
            return Err(BufferRegistrationError::PoolFull {
                limit: PACKET_BUFFERS,
            });
        }

        let mut index = self.next_registration;
        let mut free = None;
        for _ in 0..PACKET_BUFFERS {
            if matches!(self.dispatches[index].state, DispatchState::Unregistered) {
                free = Some(index);
                break;
            }
            index = Self::next_dispatch_index(index);
        }
        let index = free.ok_or(BufferRegistrationError::PoolFull {
            limit: PACKET_BUFFERS,
        })?;
        let raw = u16::try_from(index).map_err(|_| BufferRegistrationError::PoolFull {
            limit: PACKET_BUFFERS,
        })?;
        let slot = PacketSlotId(raw);
        self.dispatches[index].state = DispatchState::Free;
        buffer.binding = BufferBinding::Available {
            owner_identity: self.owner_identity,
            instance: self.instance,
            slot,
        };
        self.next_registration = Self::next_dispatch_index(index);
        Ok(slot)
    }

    /// Validate that one external packet buffer is currently reusable by this
    /// exact node owner without mutating either side.
    ///
    /// This is the boot-seed and owner-pool counterpart to preparation's
    /// transactional validation. Success returns the stable registered slot;
    /// it does not reserve the buffer or make later preparation infallible.
    pub fn validate_available_buffer(
        &self,
        buffer: &TxPacketBuffer,
    ) -> Result<PacketSlotId, AvailableBufferError> {
        let (owner_identity, instance, slot) = match buffer.binding {
            BufferBinding::Unregistered => return Err(AvailableBufferError::Unregistered),
            BufferBinding::Available {
                owner_identity,
                instance,
                slot,
            } => (owner_identity, instance, slot),
            BufferBinding::Bound { prepared, .. } => {
                return if prepared.handle.owner_identity == self.owner_identity
                    && prepared.handle.instance == self.instance
                {
                    Err(AvailableBufferError::Busy)
                } else {
                    Err(AvailableBufferError::ForeignOwner)
                };
            }
        };
        if owner_identity != self.owner_identity || instance != self.instance {
            return Err(AvailableBufferError::ForeignOwner);
        }
        if !matches!(
            self.dispatches.get(slot.index()).map(|entry| entry.state),
            Some(DispatchState::Free)
        ) {
            return Err(AvailableBufferError::MetadataInvariant);
        }
        Ok(slot)
    }

    /// Prepare encrypted destination DATA directly into one registered
    /// external buffer and bind it to a unique job.
    ///
    /// Buffer, dispatch and attempt capacity plus identifier exhaustion are
    /// rejected before entropy or RNS state mutation. Native preparation
    /// errors restore both metadata reservations and return the same available
    /// buffer. On success no packet bytes move or copy.
    pub fn prepare_data_into_slot<'a, R: RngCore + CryptoRng>(
        &mut self,
        buffer: &'a mut TxPacketBuffer,
        request: PrepareDataRequest<'_>,
        rng: &mut R,
    ) -> Result<RoutedTxJob<'a>, PrepareFailure<'a>> {
        if request.owner_now >= request.deadline.instant() {
            return Err(PrepareFailure {
                reason: SubmitError::LeaseDeadlineExpired {
                    now: request.owner_now,
                    deadline: request.deadline,
                },
                owner: PrepareFailureOwner::Available(buffer),
            });
        }
        let reservation = self.reserve_external_submission(buffer);
        let (slot, attempt, dispatch_generation, hop_generation) = match reservation {
            Ok(reservation) => reservation,
            Err(reason) => {
                return Err(PrepareFailure {
                    reason,
                    owner: PrepareFailureOwner::Available(buffer),
                });
            }
        };

        let result = self.rns.prepare_data_into(
            &request.destination.into_rns(),
            request.plaintext,
            request.rns_now.get(),
            rng,
            &mut buffer.bytes,
        );

        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.dispatches[slot.index()].state = DispatchState::Free;
                self.attempts[attempt.slot].state = AttemptState::Free;
                return Err(PrepareFailure {
                    reason: error.into(),
                    owner: PrepareFailureOwner::Available(buffer),
                });
            }
        };

        let token = AttemptToken(*prepared.receipt().as_bytes());
        let encoded_packet_sha256 = EncodedPacketSha256::new(
            Sha256::digest(&buffer.bytes[..usize::from(prepared.packet_len())]).into(),
        );
        self.attempts[attempt.slot].state = AttemptState::Active {
            generation: attempt.generation,
            token,
        };
        let scalar = PreparedPacket::from_rns(
            prepared,
            encoded_packet_sha256,
            self.handle_for(attempt),
            slot,
            request.deadline,
        );
        let mut route = match TxRoutePlan::resolve(scalar.target(), request.enabled_interfaces) {
            Ok(route) => route,
            Err(route_error) => {
                if self.rns.cancel_data_receipt(prepared.receipt()) {
                    self.dispatches[slot.index()].state = DispatchState::Free;
                    self.attempts[attempt.slot].state = AttemptState::Free;
                    return Err(PrepareFailure {
                        reason: route_error.into(),
                        owner: PrepareFailureOwner::Available(buffer),
                    });
                }
                let recovery = TxRecoveryRecord {
                    instance: self.instance,
                    slot,
                    dispatch_generation,
                    interface: None,
                    deadline: request.deadline,
                    observed_at: request.owner_now,
                    prior_phase: TxRecoveryPriorPhase::Unpermitted,
                    may_have_transmitted: false,
                    reason: TxRecoveryReason::ReceiptCancellationFailed,
                };
                self.dispatches[slot.index()].state = DispatchState::Active(DispatchRecord {
                    prepared,
                    encoded_packet_sha256,
                    attempt,
                    generation: dispatch_generation,
                    deadline: request.deadline,
                    selected: None,
                    remaining: InterfaceSet::empty(),
                    may_have_transmitted: false,
                    phase: DispatchPhase::RecoveryRequired(recovery),
                });
                buffer.binding = BufferBinding::Bound {
                    dispatch_generation,
                    prepared: scalar,
                    hop: None,
                };
                self.commit_submission_identifiers(attempt, dispatch_generation, None);
                let quarantine = TxQuarantine {
                    owner: OwnedTx {
                        buffer,
                        dispatch: DispatchHandle {
                            instance: self.instance,
                            slot,
                            generation: dispatch_generation,
                        },
                    },
                    record: recovery,
                };
                return Err(PrepareFailure {
                    reason: SubmitError::RouteReceiptCancellationFailed,
                    owner: PrepareFailureOwner::Quarantined(quarantine),
                });
            }
        };
        let selected = route
            .take_next()
            .expect("non-empty route must provide its first interface");
        let hop = HopBinding {
            interface: selected,
            generation: hop_generation,
        };
        self.dispatches[slot.index()].state = DispatchState::Active(DispatchRecord {
            prepared,
            encoded_packet_sha256,
            attempt,
            generation: dispatch_generation,
            deadline: request.deadline,
            selected: Some(hop),
            remaining: route.remaining(),
            may_have_transmitted: false,
            phase: DispatchPhase::Routed,
        });
        buffer.binding = BufferBinding::Bound {
            dispatch_generation,
            prepared: scalar,
            hop: Some(hop),
        };
        self.commit_submission_identifiers(attempt, dispatch_generation, Some(hop_generation));

        Ok(RoutedTxJob {
            owner: OwnedTx {
                buffer,
                dispatch: DispatchHandle {
                    instance: self.instance,
                    slot,
                    generation: dispatch_generation,
                },
            },
        })
    }

    /// Roll back a queued job after a handoff proves it was never accepted for
    /// transmission.
    ///
    /// The exact receipt is cancelled before attempt, dispatch or buffer state
    /// becomes reusable. A return at or after the owner deadline follows the
    /// ordinary recovery path instead of bypassing lease observation. Any
    /// mismatch returns the still-bound unique job.
    pub fn rollback_queued<'a>(
        &mut self,
        job: RoutedTxJob<'a>,
        now: MonotonicMillis,
    ) -> Result<TxCompletionDisposition<'a>, RollbackFailure<'a>> {
        let after_deadline = now >= job.deadline().instant();
        let (prepared, attempt) = match self.validate_queued_job(&job, after_deadline) {
            Ok(values) => values,
            Err(reason) => return Err(RollbackFailure { reason, job }),
        };

        if after_deadline {
            return match self.complete_tx(
                job.return_unpermitted().complete(TxCompletionCode::new(0)),
                now,
            ) {
                Ok(disposition) => Ok(disposition),
                Err(failure) => {
                    let completion = failure.into_completion();
                    Err(RollbackFailure {
                        reason: RollbackErrorKind::StaleOrUnknown,
                        job: RoutedTxJob {
                            owner: completion.owner,
                        },
                    })
                }
            };
        }

        match self.terminal_for(attempt) {
            Some(terminal) if terminal.token().as_bytes() == prepared.receipt().as_bytes() => {}
            Some(_) => {
                return Err(RollbackFailure {
                    reason: RollbackErrorKind::Invariant,
                    job,
                });
            }
            None => {
                if !self.attempt_is_active(attempt, prepared) {
                    return Err(RollbackFailure {
                        reason: RollbackErrorKind::Invariant,
                        job,
                    });
                }
                if !self.rns.cancel_data_receipt(prepared.receipt()) {
                    return Err(RollbackFailure {
                        reason: RollbackErrorKind::ReceiptMissing {
                            attempt: AttemptToken(*prepared.receipt().as_bytes()),
                        },
                        job,
                    });
                }
                self.attempts[attempt.slot].state = AttemptState::Terminal {
                    generation: attempt.generation,
                    token: AttemptToken(*prepared.receipt().as_bytes()),
                    outcome: AttemptOutcome::Unsent(AttemptUnsentReason::QueueRollback),
                };
            }
        }

        self.dispatches[job.owner.dispatch.slot.index()].state = DispatchState::Free;
        job.owner.buffer.binding = BufferBinding::Available {
            owner_identity: self.owner_identity,
            instance: self.instance,
            slot: job.owner.dispatch.slot,
        };
        Ok(TxCompletionDisposition::Available(job.owner.buffer))
    }

    /// Validate one scalar permit request and invoke the authorization policy.
    ///
    /// Successful issuance immediately and irrevocably marks the dispatch as
    /// possibly transmitted. Valid denials retain routed scalar state so the
    /// matching pending owner can return it as definitely unsent. A valid
    /// candidate carries the exact `now` sample used for the owner-deadline
    /// check; node-core does not resample between that check and the
    /// synchronous policy call.
    pub fn authorize_tx<P: TxAuthorizationPolicy>(
        &mut self,
        request: TxPermitRequest,
        now: MonotonicMillis,
        policy: &mut P,
    ) -> Result<TxPermitReply, TxAuthorizationFailure> {
        let binding = request.binding;
        if binding.owner_identity != self.owner_identity || binding.instance != self.instance {
            return Err(TxAuthorizationFailure {
                reason: TxAuthorizationErrorKind::WrongOwner,
                request,
            });
        }
        let Some(state) = self
            .dispatches
            .get(binding.slot.index())
            .map(|slot| slot.state)
        else {
            return Err(TxAuthorizationFailure {
                reason: TxAuthorizationErrorKind::StaleOrUnknown,
                request,
            });
        };
        let DispatchState::Active(mut record) = state else {
            return Err(TxAuthorizationFailure {
                reason: TxAuthorizationErrorKind::StaleOrUnknown,
                request,
            });
        };
        if !self.permit_binding_matches(binding, record) {
            return Err(TxAuthorizationFailure {
                reason: TxAuthorizationErrorKind::StaleOrUnknown,
                request,
            });
        }

        if now >= record.deadline.instant()
            && !matches!(record.phase, DispatchPhase::RecoveryRequired(_))
        {
            let recovery =
                self.recovery_for(binding.slot, record, now, TxRecoveryReason::DeadlineExpired);
            record.phase = DispatchPhase::RecoveryRequired(recovery);
            self.dispatches[binding.slot.index()].state = DispatchState::Active(record);
        }

        let denial = match record.phase {
            DispatchPhase::RecoveryRequired(record) => {
                Some(if record.reason() == TxRecoveryReason::DeadlineExpired {
                    TxPermitDenialReason::DeadlineExpired
                } else {
                    TxPermitDenialReason::RecoveryRequired
                })
            }
            DispatchPhase::Authorized => {
                return Err(TxAuthorizationFailure {
                    reason: TxAuthorizationErrorKind::StaleOrUnknown,
                    request,
                });
            }
            DispatchPhase::Routed => match self.terminal_for(record.attempt) {
                Some(terminal)
                    if terminal.token().as_bytes() == record.prepared.receipt().as_bytes() =>
                {
                    Some(TxPermitDenialReason::AttemptTerminal(terminal.outcome()))
                }
                Some(_) => {
                    return Err(TxAuthorizationFailure {
                        reason: TxAuthorizationErrorKind::Invariant,
                        request,
                    });
                }
                None if !self.attempt_is_active(record.attempt, record.prepared) => {
                    return Err(TxAuthorizationFailure {
                        reason: TxAuthorizationErrorKind::Invariant,
                        request,
                    });
                }
                None => None,
            },
        };
        if let Some(reason) = denial {
            return Ok(TxPermitReply::Denied(TxPermitDenial { binding, reason }));
        }

        let candidate = TxAuthorizationCandidate {
            interface: binding.interface,
            packet_len: record.prepared.packet_len(),
            requirements: binding.requirements,
            now,
            deadline: record.deadline,
            may_have_transmitted: record.may_have_transmitted,
        };
        match policy.authorize(candidate) {
            TxPolicyDecision::Deny(reason) => Ok(TxPermitReply::Denied(TxPermitDenial {
                binding,
                reason: TxPermitDenialReason::Policy(reason),
            })),
            TxPolicyDecision::Authorize(reservation)
                if reservation.covers(binding.requirements) =>
            {
                record.may_have_transmitted = true;
                record.phase = DispatchPhase::Authorized;
                self.dispatches[binding.slot.index()].state = DispatchState::Active(record);
                Ok(TxPermitReply::Granted(TxPermitGrant {
                    binding,
                    reservation,
                }))
            }
            TxPolicyDecision::Authorize(_) => Ok(TxPermitReply::Denied(TxPermitDenial {
                binding,
                reason: TxPermitDenialReason::Policy(TxPolicyDenial::CapacityUnavailable),
            })),
        }
    }

    /// Consume one owning hop completion and either advance, release, or
    /// quarantine its unique packet buffer.
    pub fn complete_tx<'a>(
        &mut self,
        completion: TxCompletion<'a>,
        now: MonotonicMillis,
    ) -> Result<TxCompletionDisposition<'a>, TxCompletionFailure<'a>> {
        let (scalar, hop) = completion.owner.bound();
        if scalar.handle.owner_identity != self.owner_identity
            || scalar.handle.instance != self.instance
            || completion.owner.dispatch.instance != self.instance
        {
            return Err(TxCompletionFailure {
                reason: TxCompletionErrorKind::WrongOwner,
                completion,
            });
        }
        let slot_id = completion.owner.dispatch.slot;
        let Some(state) = self.dispatches.get(slot_id.index()).map(|slot| slot.state) else {
            return Err(TxCompletionFailure {
                reason: TxCompletionErrorKind::StaleOrUnknown,
                completion,
            });
        };
        let DispatchState::Active(mut record) = state else {
            return Err(TxCompletionFailure {
                reason: TxCompletionErrorKind::StaleOrUnknown,
                completion,
            });
        };
        if record.generation != completion.owner.dispatch.generation {
            return Err(TxCompletionFailure {
                reason: TxCompletionErrorKind::StaleOrUnknown,
                completion,
            });
        }
        let Some(hop) = hop else {
            return Ok(TxCompletionDisposition::Quarantined(
                self.quarantine_completion(completion, record, now, TxRecoveryReason::Invariant),
            ));
        };
        if record.selected != Some(hop)
            || scalar.packet_len() != record.prepared.packet_len()
            || scalar.encoded_packet_sha256() != record.encoded_packet_sha256
            || scalar.attempt().as_bytes() != record.prepared.receipt().as_bytes()
            || scalar.target() != TxTarget::from_rns(record.prepared.target())
            || scalar.deadline() != record.deadline
            || scalar.handle() != self.handle_for(record.attempt)
        {
            return Ok(TxCompletionDisposition::Quarantined(
                self.quarantine_completion(completion, record, now, TxRecoveryReason::Invariant),
            ));
        }

        let (completion_authorized, completion_unsent_reason) = match completion.kind {
            CompletionKind::Unpermitted { code, denial } => {
                let _ = code.get();
                let reason = match denial {
                    Some(TxPermitDenialReason::Policy(reason)) => {
                        AttemptUnsentReason::PolicyDenied(reason)
                    }
                    Some(TxPermitDenialReason::DeadlineExpired) => {
                        AttemptUnsentReason::PermitDeadlineExpired
                    }
                    Some(TxPermitDenialReason::RecoveryRequired) => {
                        AttemptUnsentReason::RecoveryRequired
                    }
                    Some(TxPermitDenialReason::AttemptTerminal(_)) | None => {
                        AttemptUnsentReason::Unpermitted(code)
                    }
                };
                (false, Some(reason))
            }
            CompletionKind::Authorized(code) => {
                let _ = code.get();
                (true, None)
            }
            CompletionKind::RecoveryFault(code) => {
                return Ok(TxCompletionDisposition::Quarantined(
                    self.quarantine_completion(
                        completion,
                        record,
                        now,
                        TxRecoveryReason::CompletionFault(code),
                    ),
                ));
            }
        };
        if now >= record.deadline.instant()
            && !matches!(record.phase, DispatchPhase::RecoveryRequired(_))
        {
            let recovery =
                self.recovery_for(slot_id, record, now, TxRecoveryReason::DeadlineExpired);
            record.phase = DispatchPhase::RecoveryRequired(recovery);
            self.dispatches[slot_id.index()].state = DispatchState::Active(record);
        }
        let expected_authorized = match record.phase {
            DispatchPhase::Routed => false,
            DispatchPhase::Authorized => true,
            DispatchPhase::RecoveryRequired(recovery) => {
                recovery.prior_phase() == TxRecoveryPriorPhase::Authorized
            }
        };
        if completion_authorized != expected_authorized {
            return Ok(TxCompletionDisposition::Quarantined(
                self.quarantine_completion(completion, record, now, TxRecoveryReason::Invariant),
            ));
        }

        let recovered = match record.phase {
            DispatchPhase::RecoveryRequired(recovery) => Some(recovery),
            DispatchPhase::Routed | DispatchPhase::Authorized => None,
        };

        let terminal = self.terminal_for(record.attempt);
        if terminal.is_some_and(|terminal| {
            terminal.token().as_bytes() != record.prepared.receipt().as_bytes()
        }) {
            return Ok(TxCompletionDisposition::Quarantined(
                self.quarantine_completion(completion, record, now, TxRecoveryReason::Invariant),
            ));
        }
        let stop_fanout = recovered.is_some() || terminal.is_some();

        if !stop_fanout && !record.remaining.is_empty() {
            let Some(next_generation) = self.hop_generation.checked_add(1) else {
                return Ok(TxCompletionDisposition::Quarantined(
                    self.quarantine_completion(
                        completion,
                        record,
                        now,
                        TxRecoveryReason::HopIdentifierExhausted,
                    ),
                ));
            };
            let next = record
                .remaining
                .take_first()
                .expect("non-empty remaining route must have a first interface");
            let next_hop = HopBinding {
                interface: next,
                generation: next_generation,
            };
            record.selected = Some(next_hop);
            record.phase = DispatchPhase::Routed;
            self.hop_generation = next_generation;
            self.dispatches[slot_id.index()].state = DispatchState::Active(record);
            completion.owner.buffer.binding = BufferBinding::Bound {
                dispatch_generation: record.generation,
                prepared: scalar,
                hop: Some(next_hop),
            };
            return Ok(TxCompletionDisposition::Next(RoutedTxJob {
                owner: completion.owner,
            }));
        }

        if terminal.is_none() && !record.may_have_transmitted {
            if !self.attempt_is_active(record.attempt, record.prepared)
                || !self.rns.cancel_data_receipt(record.prepared.receipt())
            {
                return Ok(TxCompletionDisposition::Quarantined(
                    self.quarantine_completion(
                        completion,
                        record,
                        now,
                        TxRecoveryReason::ReceiptCancellationFailed,
                    ),
                ));
            }
            let reason = match record.phase {
                DispatchPhase::RecoveryRequired(recovery)
                    if recovery.reason() == TxRecoveryReason::DeadlineExpired =>
                {
                    AttemptUnsentReason::PermitDeadlineExpired
                }
                DispatchPhase::RecoveryRequired(_) => AttemptUnsentReason::RecoveryRequired,
                DispatchPhase::Routed | DispatchPhase::Authorized => completion_unsent_reason
                    .unwrap_or(AttemptUnsentReason::Unpermitted(TxCompletionCode::new(0))),
            };
            self.attempts[record.attempt.slot].state = AttemptState::Terminal {
                generation: record.attempt.generation,
                token: AttemptToken(*record.prepared.receipt().as_bytes()),
                outcome: AttemptOutcome::Unsent(reason),
            };
        } else if terminal.is_none() && !self.attempt_is_active(record.attempt, record.prepared) {
            return Ok(TxCompletionDisposition::Quarantined(
                self.quarantine_completion(completion, record, now, TxRecoveryReason::Invariant),
            ));
        }

        self.dispatches[slot_id.index()].state = DispatchState::Free;
        completion.owner.buffer.binding = BufferBinding::Available {
            owner_identity: self.owner_identity,
            instance: self.instance,
            slot: slot_id,
        };
        Ok(match recovered {
            Some(record) => TxCompletionDisposition::Recovered {
                buffer: completion.owner.buffer,
                observation: TxRecoveryObservation::from_prepared(scalar, record),
            },
            None => TxCompletionDisposition::Available(completion.owner.buffer),
        })
    }

    /// Return the earliest live packet-owner lease deadline.
    ///
    /// Dispatches already retained in recovery-required state are excluded;
    /// they no longer need a deadline wakeup.
    pub fn next_tx_deadline(&self) -> Option<TxLeaseDeadline> {
        self.dispatches
            .iter()
            .filter_map(|slot| match slot.state {
                DispatchState::Active(record)
                    if !matches!(record.phase, DispatchPhase::RecoveryRequired(_)) =>
                {
                    Some(record.deadline)
                }
                DispatchState::Unregistered
                | DispatchState::Free
                | DispatchState::Reserved { .. }
                | DispatchState::Active(_) => None,
            })
            .min()
    }

    /// Move expired routed or authorized dispatches into scalar recovery.
    ///
    /// This never releases or fabricates a packet-buffer owner.
    pub fn maintain_tx(&mut self, now: MonotonicMillis) -> TxMaintenanceReport {
        let mut newly_recovery_required = 0;
        for index in 0..PACKET_BUFFERS {
            let DispatchState::Active(mut record) = self.dispatches[index].state else {
                continue;
            };
            if now >= record.deadline.instant()
                && !matches!(record.phase, DispatchPhase::RecoveryRequired(_))
            {
                let slot = PacketSlotId(u16::try_from(index).expect("validated slot capacity"));
                let recovery =
                    self.recovery_for(slot, record, now, TxRecoveryReason::DeadlineExpired);
                record.phase = DispatchPhase::RecoveryRequired(recovery);
                self.dispatches[index].state = DispatchState::Active(record);
                newly_recovery_required += 1;
            }
        }
        TxMaintenanceReport {
            newly_recovery_required,
        }
    }

    /// Iterate retained scalar recovery records in stable packet-slot order.
    pub fn recovery_records(&self) -> TxRecoveryRecords<'_> {
        TxRecoveryRecords {
            owner_identity: self.owner_identity,
            instance: self.instance,
            dispatches: &self.dispatches,
            next: 0,
        }
    }

    /// Process one inbound packet while committing any proof to the exact
    /// fixed-ledger attempt before RNS mutates its receipt state.
    ///
    /// Correlation failures are invariant faults, not capacity backpressure;
    /// the packet remains retryable inside RNS for diagnosis or recovery.
    pub fn ingest<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: MonotonicSeconds,
        interface: PacketInterfaceId,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        self.ingest_at(
            raw,
            now,
            MonotonicInstant::from_secs(now.get()),
            interface,
            rng,
        )
    }

    /// Precise Link-clock variant of [`Self::ingest`].
    pub fn ingest_at<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: MonotonicSeconds,
        link_now: MonotonicInstant,
        interface: PacketInterfaceId,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        let (result, fault) = {
            let (rns, attempts) = (&mut self.rns, &mut self.attempts);
            let mut sink = AttemptReceiptSink::new(attempts);
            let result = rns.ingest_with_receipt_sink_at(
                raw,
                now.get(),
                link_now,
                interface.into_rns(),
                rng,
                &mut sink,
            );
            (result, sink.fault)
        };

        match (result, fault) {
            (Ok(report), None) => Ok(report),
            (Ok(_), Some(fault)) | (Err(ReceiptReservationUnavailable), Some(fault)) => Err(fault),
            (Err(ReceiptReservationUnavailable), None) => {
                Err(ReceiptCorrelationError::ReservationInvariant)
            }
        }
    }

    /// Run RNS timer maintenance and transactionally retain DATA timeouts.
    ///
    /// Ordinary maintenance actions are always returned, including when a
    /// receipt/ledger correlation invariant is detected.
    pub fn tick<R: RngCore + CryptoRng>(
        &mut self,
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> MaintenanceReport {
        self.tick_at(now, MonotonicInstant::from_secs(now.get()), rng)
    }

    /// Precise Link-clock variant of [`Self::tick`].
    pub fn tick_at<R: RngCore + CryptoRng>(
        &mut self,
        now: MonotonicSeconds,
        link_now: MonotonicInstant,
        rng: &mut R,
    ) -> MaintenanceReport {
        let (report, fault) = {
            let (rns, attempts) = (&mut self.rns, &mut self.attempts);
            let mut sink = AttemptReceiptSink::new(attempts);
            let report = rns.tick_with_receipt_sink_at(now.get(), link_now, rng, &mut sink);
            (report, sink.fault)
        };
        MaintenanceReport {
            actions: report.actions,
            timed_out_attempts: report.timed_out_receipts,
            timed_out_attempt_tag: report.timed_out_receipt_tag,
            timed_out_attempt_tags_consistent: report.timed_out_receipt_tags_consistent,
            correlation_fault: fault.or_else(|| {
                report
                    .receipt_terminals_deferred
                    .then_some(ReceiptCorrelationError::ReservationInvariant)
            }),
        }
    }

    /// Iterate terminal attempt tombstones in stable ledger-slot order.
    pub fn terminal_attempts(&self) -> TerminalAttempts<'_> {
        TerminalAttempts {
            owner_identity: self.owner_identity,
            instance: self.instance,
            attempts: &self.attempts,
            next: 0,
        }
    }

    /// Release one terminal tombstone after its outcome has been projected or
    /// persisted by the caller.
    pub fn acknowledge_terminal(
        &mut self,
        handle: AttemptHandle,
    ) -> Result<TerminalAttempt, AcknowledgeError> {
        let attempt_ref = self.validate_attempt_handle(handle)?;
        let state = self.attempts[attempt_ref.slot].state;
        let AttemptState::Terminal {
            generation,
            token,
            outcome,
        } = state
        else {
            return match state {
                AttemptState::Active { .. } | AttemptState::Reserved { .. } => {
                    Err(AcknowledgeError::NotTerminal)
                }
                AttemptState::Free | AttemptState::Terminal { .. } => {
                    Err(AcknowledgeError::StaleOrUnknown)
                }
            };
        };

        for slot in &self.dispatches {
            match slot.state {
                DispatchState::Active(DispatchRecord { attempt, .. }) if attempt == attempt_ref => {
                    return Err(AcknowledgeError::PacketStillBound);
                }
                DispatchState::Reserved { attempt, .. } if attempt == attempt_ref => {
                    return Err(AcknowledgeError::Invariant);
                }
                DispatchState::Unregistered
                | DispatchState::Free
                | DispatchState::Reserved { .. }
                | DispatchState::Active(_) => {}
            }
        }

        debug_assert_eq!(generation, attempt_ref.generation);
        let terminal = TerminalAttempt {
            handle,
            token,
            outcome,
        };
        self.attempts[attempt_ref.slot].state = AttemptState::Free;
        Ok(terminal)
    }

    /// Read scalar dispatch and receipt occupancy without exposing RNS
    /// internals.
    pub fn capacities(&self) -> CapacitySnapshot {
        let mut registered = 0;
        let mut queued = 0;
        let mut authorized = 0;
        let mut recovery_required = 0;
        let mut used = 0;
        for slot in &self.dispatches {
            match slot.state {
                DispatchState::Unregistered => {}
                DispatchState::Free => registered += 1,
                DispatchState::Reserved { .. } => {
                    registered += 1;
                    used += 1;
                }
                DispatchState::Active(record) => {
                    registered += 1;
                    used += 1;
                    match record.phase {
                        DispatchPhase::Routed => queued += 1,
                        DispatchPhase::Authorized => authorized += 1,
                        DispatchPhase::RecoveryRequired(_) => recovery_required += 1,
                    }
                }
            }
        }
        let mut attempts_active = 0;
        let mut attempts_terminal = 0;
        let mut attempts_used = 0;
        for slot in &self.attempts {
            match slot.state {
                AttemptState::Free => {}
                AttemptState::Reserved { .. } => attempts_used += 1,
                AttemptState::Active { .. } => {
                    attempts_active += 1;
                    attempts_used += 1;
                }
                AttemptState::Terminal { .. } => {
                    attempts_terminal += 1;
                    attempts_used += 1;
                }
            }
        }
        let rns = self.rns.metrics();
        CapacitySnapshot {
            packet_buffers_registered: registered,
            dispatches_queued: queued,
            dispatches_authorized: authorized,
            dispatches_recovery_required: recovery_required,
            dispatches_used: used,
            dispatches_limit: PACKET_BUFFERS,
            receipts_used: rns.capacity.receipts.used,
            receipts_limit: rns.capacity.receipts.limit,
            attempts_active,
            attempts_terminal,
            attempts_used,
            attempts_limit: PATHS,
        }
    }

    fn reserve_external_submission(
        &mut self,
        buffer: &TxPacketBuffer,
    ) -> Result<(PacketSlotId, AttemptRef, u64, u64), SubmitError> {
        let slot = self.validate_available_buffer(buffer)?;

        let mut attempt_slot = self.next_attempt;
        let mut free_attempt = None;
        for _ in 0..PATHS {
            if matches!(self.attempts[attempt_slot].state, AttemptState::Free) {
                free_attempt = Some(attempt_slot);
                break;
            }
            attempt_slot = Self::next_attempt_index(attempt_slot);
        }
        let attempt_slot = free_attempt.ok_or(SubmitError::AttemptLedgerFull { limit: PATHS })?;
        let attempt_generation = self
            .attempt_generation
            .checked_add(1)
            .ok_or(SubmitError::AttemptIdentifierExhausted)?;
        let dispatch_generation = self
            .dispatch_generation
            .checked_add(1)
            .ok_or(SubmitError::DispatchIdentifierExhausted)?;
        let hop_generation = self
            .hop_generation
            .checked_add(1)
            .ok_or(SubmitError::HopIdentifierExhausted)?;
        let attempt = AttemptRef {
            slot: attempt_slot,
            generation: attempt_generation,
        };
        self.attempts[attempt_slot].state = AttemptState::Reserved {
            generation: attempt_generation,
        };
        self.dispatches[slot.index()].state = DispatchState::Reserved { attempt };
        Ok((slot, attempt, dispatch_generation, hop_generation))
    }

    fn validate_queued_job(
        &self,
        job: &RoutedTxJob<'_>,
        allow_deadline_recovery: bool,
    ) -> Result<(RnsPreparedData, AttemptRef), RollbackErrorKind> {
        if job.owner.dispatch.instance != self.instance {
            return Err(RollbackErrorKind::StaleOrUnknown);
        }
        let (scalar, bound_hop) = match job.owner.buffer.binding {
            BufferBinding::Bound {
                dispatch_generation,
                prepared,
                hop,
            } if dispatch_generation == job.owner.dispatch.generation
                && prepared.slot_id() == job.owner.dispatch.slot
                && hop.is_some() =>
            {
                (prepared, hop)
            }
            BufferBinding::Unregistered
            | BufferBinding::Available { .. }
            | BufferBinding::Bound { .. } => return Err(RollbackErrorKind::Invariant),
        };
        if scalar.handle.owner_identity != self.owner_identity
            || scalar.handle.instance != self.instance
            || scalar.handle.instance != job.owner.dispatch.instance
        {
            return Err(RollbackErrorKind::StaleOrUnknown);
        }
        let Some(slot) = self.dispatches.get(job.owner.dispatch.slot.index()) else {
            return Err(RollbackErrorKind::StaleOrUnknown);
        };
        match slot.state {
            DispatchState::Active(DispatchRecord {
                prepared,
                encoded_packet_sha256,
                attempt,
                generation,
                deadline,
                selected,
                may_have_transmitted,
                phase,
                ..
            }) if generation == job.owner.dispatch.generation
                && prepared.packet_len() == scalar.packet_len()
                && prepared.receipt().as_bytes() == scalar.attempt().as_bytes()
                && encoded_packet_sha256 == scalar.encoded_packet_sha256()
                && TxTarget::from_rns(prepared.target()) == scalar.target()
                && deadline == scalar.deadline()
                && selected == bound_hop
                && self.handle_for(attempt) == scalar.handle()
                && (matches!(phase, DispatchPhase::Routed)
                    || matches!(
                        phase,
                        DispatchPhase::RecoveryRequired(recovery)
                            if allow_deadline_recovery
                                && recovery.reason() == TxRecoveryReason::DeadlineExpired
                                && recovery.prior_phase()
                                    == TxRecoveryPriorPhase::Unpermitted
                    )) =>
            {
                if may_have_transmitted {
                    Err(RollbackErrorKind::PriorAuthorization)
                } else {
                    Ok((prepared, attempt))
                }
            }
            DispatchState::Unregistered
            | DispatchState::Free
            | DispatchState::Reserved { .. }
            | DispatchState::Active(_) => Err(RollbackErrorKind::StaleOrUnknown),
        }
    }

    fn commit_submission_identifiers(
        &mut self,
        attempt: AttemptRef,
        dispatch_generation: u64,
        hop_generation: Option<u64>,
    ) {
        self.next_attempt = Self::next_attempt_index(attempt.slot);
        self.attempt_generation = attempt.generation;
        self.dispatch_generation = dispatch_generation;
        if let Some(hop_generation) = hop_generation {
            self.hop_generation = hop_generation;
        }
    }

    fn permit_binding_matches(&self, binding: PermitBinding, record: DispatchRecord) -> bool {
        let attempt_matches = matches!(
            self.attempts.get(record.attempt.slot).map(|attempt| attempt.state),
            Some(AttemptState::Active { generation, token }
                | AttemptState::Terminal { generation, token, .. })
                if generation == record.attempt.generation
                    && token.as_bytes() == record.prepared.receipt().as_bytes()
        );
        binding.dispatch_generation == record.generation
            && record.selected.is_some_and(|hop| {
                hop.interface == binding.interface && hop.generation == binding.hop_generation
            })
            && attempt_matches
    }

    fn recovery_for(
        &self,
        slot: PacketSlotId,
        record: DispatchRecord,
        observed_at: MonotonicMillis,
        reason: TxRecoveryReason,
    ) -> TxRecoveryRecord {
        TxRecoveryRecord {
            instance: self.instance,
            slot,
            dispatch_generation: record.generation,
            interface: record.selected.map(|hop| hop.interface),
            deadline: record.deadline,
            observed_at,
            prior_phase: if matches!(record.phase, DispatchPhase::Authorized) {
                TxRecoveryPriorPhase::Authorized
            } else {
                TxRecoveryPriorPhase::Unpermitted
            },
            may_have_transmitted: record.may_have_transmitted,
            reason,
        }
    }

    fn quarantine_completion<'a>(
        &mut self,
        completion: TxCompletion<'a>,
        mut record: DispatchRecord,
        now: MonotonicMillis,
        reason: TxRecoveryReason,
    ) -> TxQuarantine<'a> {
        let recovery = match record.phase {
            DispatchPhase::RecoveryRequired(mut recovery) => {
                recovery.observed_at = now;
                recovery.reason = reason;
                recovery
            }
            DispatchPhase::Routed | DispatchPhase::Authorized => {
                self.recovery_for(completion.owner.dispatch.slot, record, now, reason)
            }
        };
        let authoritative = PreparedPacket::from_rns(
            record.prepared,
            record.encoded_packet_sha256,
            self.handle_for(record.attempt),
            completion.owner.dispatch.slot,
            record.deadline,
        );
        // A completion can reach this path because its buffer-side scalar was
        // inconsistent. Canonicalize the private binding from the authoritative
        // dispatch before the quarantine exposes any correlated observation.
        completion.owner.buffer.binding = BufferBinding::Bound {
            dispatch_generation: record.generation,
            prepared: authoritative,
            hop: record.selected,
        };
        record.phase = DispatchPhase::RecoveryRequired(recovery);
        self.dispatches[completion.owner.dispatch.slot.index()].state =
            DispatchState::Active(record);
        TxQuarantine {
            owner: completion.owner,
            record: recovery,
        }
    }

    fn attempt_is_active(&self, attempt: AttemptRef, prepared: RnsPreparedData) -> bool {
        matches!(
            self.attempts.get(attempt.slot).map(|slot| slot.state),
            Some(AttemptState::Active { generation, token })
                if generation == attempt.generation
                    && token.as_bytes() == prepared.receipt().as_bytes()
        )
    }

    fn terminal_for(&self, attempt: AttemptRef) -> Option<TerminalAttempt> {
        let AttemptState::Terminal {
            generation,
            token,
            outcome,
        } = self.attempts.get(attempt.slot)?.state
        else {
            return None;
        };
        (generation == attempt.generation).then_some(TerminalAttempt {
            handle: self.handle_for(attempt),
            token,
            outcome,
        })
    }

    fn handle_for(&self, attempt: AttemptRef) -> AttemptHandle {
        AttemptHandle {
            owner_identity: self.owner_identity,
            instance: self.instance,
            slot: attempt.slot,
            generation: attempt.generation,
        }
    }

    fn validate_attempt_handle(
        &self,
        handle: AttemptHandle,
    ) -> Result<AttemptRef, AcknowledgeError> {
        if handle.owner_identity != self.owner_identity || handle.instance != self.instance {
            return Err(AcknowledgeError::StaleOrUnknown);
        }
        let Some(slot) = self.attempts.get(handle.slot) else {
            return Err(AcknowledgeError::StaleOrUnknown);
        };
        let generation = match slot.state {
            AttemptState::Reserved { generation }
            | AttemptState::Active { generation, .. }
            | AttemptState::Terminal { generation, .. } => generation,
            AttemptState::Free => return Err(AcknowledgeError::StaleOrUnknown),
        };
        if generation != handle.generation {
            return Err(AcknowledgeError::StaleOrUnknown);
        }
        Ok(AttemptRef {
            slot: handle.slot,
            generation,
        })
    }

    fn next_dispatch_index(index: usize) -> usize {
        if index + 1 == PACKET_BUFFERS {
            0
        } else {
            index + 1
        }
    }

    fn next_attempt_index(index: usize) -> usize {
        if index + 1 == PATHS { 0 } else { index + 1 }
    }
}

struct AttemptReceiptSink<'a> {
    attempts: &'a mut [AttemptSlot],
    fault: Option<ReceiptCorrelationError>,
}

impl<'a> AttemptReceiptSink<'a> {
    fn new(attempts: &'a mut [AttemptSlot]) -> Self {
        Self {
            attempts,
            fault: None,
        }
    }

    fn reject(
        &mut self,
        fault: ReceiptCorrelationError,
    ) -> Result<AttemptTerminalReservation<'_>, ReceiptReservationUnavailable> {
        if self.fault.is_none() {
            self.fault = Some(fault);
        }
        Err(ReceiptReservationUnavailable)
    }
}

struct AttemptTerminalReservation<'a> {
    state: &'a mut AttemptState,
    generation: u64,
    token: AttemptToken,
}

impl ReceiptTerminalSink for AttemptReceiptSink<'_> {
    type Reservation<'a>
        = AttemptTerminalReservation<'a>
    where
        Self: 'a;

    fn try_reserve(
        &mut self,
        candidate: ReceiptCandidate,
    ) -> Result<Self::Reservation<'_>, ReceiptReservationUnavailable> {
        if candidate.kind() == ReceiptKind::Channel {
            return self.reject(ReceiptCorrelationError::UnsupportedChannel);
        }
        let token = AttemptToken(*candidate.receipt().as_bytes());
        let active_slot = self.attempts.iter().position(|slot| {
            matches!(slot.state, AttemptState::Active { token: active, .. } if active == token)
        });
        if let Some(index) = active_slot {
            let generation = match self.attempts[index].state {
                AttemptState::Active { generation, .. } => generation,
                AttemptState::Free
                | AttemptState::Reserved { .. }
                | AttemptState::Terminal { .. } => {
                    return self.reject(ReceiptCorrelationError::ReservationInvariant);
                }
            };
            return Ok(AttemptTerminalReservation {
                state: &mut self.attempts[index].state,
                generation,
                token,
            });
        }
        if self.attempts.iter().any(|slot| {
            matches!(
                slot.state,
                AttemptState::Terminal {
                    token: terminal_token,
                    ..
                } if terminal_token == token
            )
        }) {
            self.reject(ReceiptCorrelationError::AlreadyTerminal { attempt: token })
        } else {
            self.reject(ReceiptCorrelationError::UnknownData { attempt: token })
        }
    }
}

impl ReceiptTerminalReservation for AttemptTerminalReservation<'_> {
    fn commit(self, terminal: ReceiptTerminal) {
        let candidate = terminal.candidate();
        debug_assert_eq!(candidate.kind(), ReceiptKind::Data);
        debug_assert_eq!(candidate.receipt().as_bytes(), self.token.as_bytes());
        let outcome = match terminal {
            ReceiptTerminal::Delivered(_) => AttemptOutcome::Delivered,
            ReceiptTerminal::TimedOut(_) => AttemptOutcome::DeliveryTimeout,
        };
        *self.state = AttemptState::Terminal {
            generation: self.generation,
            token: self.token,
            outcome,
        };
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    #[derive(Default)]
    struct CounterRng(u8);

    impl RngCore for CounterRng {
        fn next_u32(&mut self) -> u32 {
            let mut bytes = [0; 4];
            self.fill_bytes(&mut bytes);
            u32::from_le_bytes(bytes)
        }

        fn next_u64(&mut self) -> u64 {
            let mut bytes = [0; 8];
            self.fill_bytes(&mut bytes);
            u64::from_le_bytes(bytes)
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for byte in destination {
                self.0 = self.0.wrapping_add(1);
                *byte = self.0;
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for CounterRng {}

    type TestNode<const PATHS: usize, const BUFFERS: usize> = NodeCore<PATHS, 2, 8, 2, BUFFERS>;

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).unwrap()
    }

    fn node<const PATHS: usize, const BUFFERS: usize>(
        tag: u8,
        aspect: &str,
    ) -> TestNode<PATHS, BUFFERS> {
        node_with_instance(tag, aspect, tag.wrapping_add(0x80))
    }

    fn node_with_instance<const PATHS: usize, const BUFFERS: usize>(
        tag: u8,
        aspect: &str,
        instance_tag: u8,
    ) -> TestNode<PATHS, BUFFERS> {
        TestNode::new(
            identity(tag),
            "reticulum",
            &[aspect],
            NodeInstanceId::new([instance_tag; 16]),
            NodeConfig::endpoint(),
        )
        .unwrap()
    }

    #[test]
    fn additional_inbound_destination_and_identity_lookup_remain_narrow_and_bounded() {
        let mut owner = node_with_instance::<8, 0>(50, "embedded", 0x90);
        let primary = owner.destination_hash();
        let delivery = owner
            .register_inbound_single_destination("lxmf", &["delivery"])
            .unwrap();
        assert_ne!(delivery, primary);

        let mut rebuilt = node_with_instance::<8, 0>(50, "embedded", 0x91);
        assert_eq!(
            rebuilt
                .register_inbound_single_destination("lxmf", &["delivery"])
                .unwrap(),
            delivery
        );

        for aspect in ["propagation", "stamp", "control"] {
            owner
                .register_inbound_single_destination("lxmf", &[aspect])
                .unwrap();
        }
        assert_eq!(
            owner.register_inbound_single_destination("lxmf", &["overflow"]),
            Err(LocalDestinationRegistrationError::LimitReached { limit: 4 })
        );

        let peer = NodeCore::<8, 2, 8, 2, 0>::new(
            identity(51),
            "lxmf",
            &["delivery"],
            NodeInstanceId::new([0x92; 16]),
            NodeConfig::endpoint(),
        )
        .unwrap();
        let peer_destination = peer.destination_hash();
        let peer_public_key = identity(51).public_key();
        owner
            .register_peer(
                &identity(51),
                "lxmf",
                &["delivery"],
                MonotonicSeconds::new(1),
            )
            .unwrap();
        assert_eq!(
            owner.recall_identity(&peer_destination),
            Some(peer_public_key)
        );
        assert_eq!(
            owner.recall_identity(&DestinationHash::new([0xee; 16])),
            None
        );
    }

    #[test]
    fn basic_lxmf_composition_binds_the_registered_source_inside_the_node_owner() {
        let mut owner = node_with_instance::<8, 0>(52, "embedded", 0x93);
        let remote = DestinationHash::new([0xa7; 16]);
        let mut carrier = [0x55_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        assert_eq!(
            owner.prepare_basic_lxmf_into(
                &remote,
                1_700_000_000_000,
                b"title",
                b"content",
                &mut carrier,
            ),
            Err(PrepareBasicLxmfError::DeliveryDestinationUnavailable)
        );
        assert!(carrier.iter().all(|byte| *byte == 0x55));

        let source = owner
            .register_inbound_single_destination("lxmf", &["delivery"])
            .unwrap();
        let prepared = owner
            .prepare_basic_lxmf_into(
                &remote,
                1_700_000_000_000,
                b"title",
                b"content",
                &mut carrier,
            )
            .unwrap();
        assert!(usize::from(prepared.carrier_len()) <= MAX_OPPORTUNISTIC_LXMF_CARRIER);
        assert_eq!(&carrier[..16], source.as_bytes());
        assert_ne!(prepared.message_id(), [0; 32]);
    }

    const fn time(seconds: u64) -> MonotonicSeconds {
        MonotonicSeconds::new(seconds)
    }

    const fn announce_time(value: u64) -> AnnounceEmissionTime {
        match AnnounceEmissionTime::new(value) {
            Ok(value) => value,
            Err(_) => panic!("test announce time must fit the wire field"),
        }
    }

    const fn deadline(milliseconds: u64) -> TxLeaseDeadline {
        TxLeaseDeadline::new(MonotonicMillis::new(milliseconds))
    }

    fn prepare_request(
        destination: DestinationHash,
        plaintext: &[u8],
        rns_now: u64,
        owner_now_ms: u64,
        deadline_ms: u64,
        enabled_interfaces: InterfaceSet,
    ) -> PrepareDataRequest<'_> {
        PrepareDataRequest {
            destination,
            plaintext,
            rns_now: time(rns_now),
            owner_now: owner_time(owner_now_ms),
            deadline: deadline(deadline_ms),
            enabled_interfaces,
        }
    }

    const fn owner_time(milliseconds: u64) -> MonotonicMillis {
        MonotonicMillis::new(milliseconds)
    }

    const fn default_interfaces() -> InterfaceSet {
        InterfaceSet::from_bits(1 << 1)
    }

    fn available(failure: PrepareFailure<'_>) -> &mut TxPacketBuffer {
        match failure.into_buffer() {
            Ok(buffer) => buffer,
            Err(_) => panic!("unexpected preparation quarantine"),
        }
    }

    fn reusable(disposition: TxCompletionDisposition<'_>) -> &mut TxPacketBuffer {
        match disposition {
            TxCompletionDisposition::Available(buffer)
            | TxCompletionDisposition::Recovered { buffer, .. } => buffer,
            TxCompletionDisposition::Next(_) => panic!("rollback advanced fanout"),
            TxCompletionDisposition::Quarantined(_) => panic!("rollback quarantined buffer"),
        }
    }

    fn interfaces(ids: &[u8]) -> InterfaceSet {
        let mut set = InterfaceSet::empty();
        for id in ids {
            set = set
                .with(PacketInterfaceId::new(*id))
                .expect("test interface must fit compact profile");
        }
        set
    }

    const TEST_PERMIT_RESOURCE: TxPermitResourceId = TxPermitResourceId::new([0x54; 16]);

    fn test_permit_requirements() -> TxPermitRequirements {
        TxPermitRequirements::try_new(TEST_PERMIT_RESOURCE, 1).unwrap()
    }

    fn test_permit_reservation() -> TxPermitReservation {
        TxPermitReservation::try_new(TEST_PERMIT_RESOURCE, 1).unwrap()
    }

    struct TestPolicy {
        decision: TxPolicyDecision,
        calls: usize,
        candidate: Option<TxAuthorizationCandidate>,
    }

    impl TestPolicy {
        fn allowing() -> Self {
            Self {
                decision: TxPolicyDecision::Authorize(test_permit_reservation()),
                calls: 0,
                candidate: None,
            }
        }

        const fn denying(reason: TxPolicyDenial) -> Self {
            Self {
                decision: TxPolicyDecision::Deny(reason),
                calls: 0,
                candidate: None,
            }
        }
    }

    impl TxAuthorizationPolicy for TestPolicy {
        fn authorize(&mut self, candidate: TxAuthorizationCandidate) -> TxPolicyDecision {
            self.calls += 1;
            self.candidate = Some(candidate);
            self.decision
        }
    }

    fn register_receiver<const PATHS: usize, const BUFFERS: usize>(
        sender: &mut TestNode<PATHS, BUFFERS>,
        tag: u8,
        aspect: &str,
    ) {
        sender
            .register_peer(&identity(tag), "reticulum", &[aspect], time(0))
            .unwrap();
    }

    fn registered_buffer<const PATHS: usize, const BUFFERS: usize>(
        owner: &mut TestNode<PATHS, BUFFERS>,
    ) -> TxPacketBuffer {
        let mut buffer = TxPacketBuffer::new();
        owner.register_packet_buffer(&mut buffer).unwrap();
        buffer
    }

    fn prepare<'a, const PATHS: usize, const BUFFERS: usize>(
        sender: &mut TestNode<PATHS, BUFFERS>,
        buffer: &'a mut TxPacketBuffer,
        destination: DestinationHash,
        plaintext: &[u8],
        now: u64,
        rng: &mut CounterRng,
    ) -> TxJob<'a> {
        prepare_with_interfaces(
            sender,
            buffer,
            destination,
            plaintext,
            now,
            now * 1_000,
            now * 1_000 + 500,
            default_interfaces(),
            rng,
        )
    }

    fn deliver_prepared<const PATHS: usize, const BUFFERS: usize>(
        sender: &mut TestNode<PATHS, BUFFERS>,
        receiver: &mut TestNode<PATHS, BUFFERS>,
        buffer: &mut TxPacketBuffer,
        plaintext: &[u8],
        now: u64,
        interface: PacketInterfaceId,
        rng: &mut CounterRng,
    ) -> NodeActions {
        let destination = receiver.destination_hash();
        deliver_prepared_to(
            sender,
            receiver,
            buffer,
            destination,
            plaintext,
            now,
            interface,
            InterfaceSet::from_bits(u64::MAX),
            rng,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver_prepared_to<const PATHS: usize, const BUFFERS: usize>(
        sender: &mut TestNode<PATHS, BUFFERS>,
        receiver: &mut TestNode<PATHS, BUFFERS>,
        buffer: &mut TxPacketBuffer,
        destination: DestinationHash,
        plaintext: &[u8],
        now: u64,
        interface: PacketInterfaceId,
        enabled_interfaces: InterfaceSet,
        rng: &mut CounterRng,
    ) -> NodeActions {
        let job = prepare_with_interfaces(
            sender,
            buffer,
            destination,
            plaintext,
            now,
            now * 1_000,
            now * 1_000 + 500,
            enabled_interfaces,
            rng,
        );
        let packet_len = usize::from(job.packet_len());
        let packet = job.owner.buffer.bytes[..packet_len].to_vec();
        let received = receiver.ingest(&packet, time(now), interface, rng).unwrap();
        let disposition = sender
            .complete_tx(
                job.return_unpermitted().complete(TxCompletionCode::new(0)),
                owner_time(now * 1_000 + 100),
            )
            .unwrap_or_else(|failure| {
                panic!(
                    "prepared test packet did not return: {:?}",
                    failure.reason()
                )
            });
        assert!(matches!(disposition, TxCompletionDisposition::Available(_)));
        received.actions
    }

    fn assert_noncovering_reservation_is_denied(
        sender_tag: u8,
        receiver_tag: u8,
        requirements: TxPermitRequirements,
        reservation: TxPermitReservation,
    ) {
        let mut sender = node::<2, 1>(sender_tag, "sender");
        let receiver = node::<2, 1>(receiver_tag, "receiver");
        register_receiver(&mut sender, receiver_tag, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"reservation rejection",
            1,
            &mut rng,
        );
        let (pending, request) = job.begin_permit(requirements);
        let mut policy = TestPolicy {
            decision: TxPolicyDecision::Authorize(reservation),
            calls: 0,
            candidate: None,
        };
        let reply = sender
            .authorize_tx(request, owner_time(1_100), &mut policy)
            .unwrap_or_else(|failure| panic!("valid request failed: {:?}", failure.reason()));

        assert_eq!(policy.calls, 1);
        assert_eq!(policy.candidate.unwrap().requirements, requirements);
        assert!(matches!(
            &reply,
            TxPermitReply::Denied(denial)
                if denial.reason()
                    == TxPermitDenialReason::Policy(TxPolicyDenial::CapacityUnavailable)
        ));
        assert_eq!(sender.capacities().dispatches_authorized, 0);
        assert_eq!(sender.capacities().dispatches_queued, 1);

        let unpermitted = match pending.resolve(reply, owner_time(1_101)) {
            Ok(PermitResolution::Unpermitted(unpermitted)) => unpermitted,
            Ok(PermitResolution::Authorized(_)) => panic!("non-covering reservation authorized"),
            Ok(PermitResolution::Expired(_)) => panic!("non-covering reservation expired"),
            Err(_) => panic!("matching denial reply was rejected"),
        };
        assert!(matches!(
            sender
                .complete_tx(
                    unpermitted.complete(TxCompletionCode::new(0x554e)),
                    owner_time(1_102),
                )
                .unwrap_or_else(|failure| panic!("denied return failed: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_with_interfaces<'a, const PATHS: usize, const BUFFERS: usize>(
        sender: &mut TestNode<PATHS, BUFFERS>,
        buffer: &'a mut TxPacketBuffer,
        destination: DestinationHash,
        plaintext: &[u8],
        now: u64,
        owner_now_ms: u64,
        deadline_ms: u64,
        enabled_interfaces: InterfaceSet,
        rng: &mut CounterRng,
    ) -> RoutedTxJob<'a> {
        match sender.prepare_data_into_slot(
            buffer,
            prepare_request(
                destination,
                plaintext,
                now,
                owner_now_ms,
                deadline_ms,
                enabled_interfaces,
            ),
            rng,
        ) {
            Ok(job) => job,
            Err(failure) => panic!("preparation failed: {:?}", failure.reason()),
        }
    }

    fn proof_for(receiver_tag: u8, attempt: AttemptToken) -> Vec<u8> {
        let signature = identity(receiver_tag).0.sign(attempt.as_bytes()).unwrap();
        let mut proof = std::vec![0u8; 19 + 32 + 64];
        proof[0] = 0x03;
        proof[2..18].copy_from_slice(&attempt.as_bytes()[..16]);
        proof[19..51].copy_from_slice(attempt.as_bytes());
        proof[51..].copy_from_slice(&signature);
        proof
    }

    fn implicit_proof_for(receiver_tag: u8, attempt: AttemptToken) -> Vec<u8> {
        let signature = identity(receiver_tag).0.sign(attempt.as_bytes()).unwrap();
        let mut proof = std::vec![0u8; 19 + 64];
        proof[0] = 0x03;
        proof[2..18].copy_from_slice(&attempt.as_bytes()[..16]);
        proof[19..].copy_from_slice(&signature);
        proof
    }

    fn link_proof_for(receiver_tag: u8, link_id: [u8; 16], attempt: AttemptToken) -> Vec<u8> {
        let signature = identity(receiver_tag).0.sign(attempt.as_bytes()).unwrap();
        let mut proof = std::vec![0u8; 19 + 32 + 64];
        proof[0] = 0x0f;
        proof[2..18].copy_from_slice(&link_id);
        proof[19..51].copy_from_slice(attempt.as_bytes());
        proof[51..].copy_from_slice(&signature);
        proof
    }

    #[test]
    fn delivery_proof_tag_requires_exact_rete_explicit_single_wire_shape() {
        let mut bytes = [0_u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = index as u8;
        }
        let proof = proof_for(77, AttemptToken(bytes));
        assert_eq!(
            delivery_proof_correlation_tag(&proof),
            Some(u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7]))
        );

        assert_eq!(
            delivery_proof_correlation_tag(&implicit_proof_for(77, AttemptToken(bytes))),
            None
        );

        let mut one_byte_short = proof.clone();
        one_byte_short.pop();
        assert_eq!(delivery_proof_correlation_tag(&one_byte_short), None);

        let mut one_byte_long = proof.clone();
        one_byte_long.push(0);
        assert_eq!(delivery_proof_correlation_tag(&one_byte_long), None);

        let mut wrong_context = proof.clone();
        wrong_context[18] = 1;
        assert_eq!(delivery_proof_correlation_tag(&wrong_context), None);

        let mut wrong_destination = proof.clone();
        wrong_destination[2] ^= 0xff;
        assert_eq!(delivery_proof_correlation_tag(&wrong_destination), None);

        let mut forwarded = proof.clone();
        forwarded[1] = 1;
        assert_eq!(delivery_proof_correlation_tag(&forwarded), None);

        let mut not_a_proof = proof.clone();
        not_a_proof[0] = 0;
        assert_eq!(delivery_proof_correlation_tag(&not_a_proof), None);
    }

    #[test]
    fn delivery_proof_tag_accepts_only_exact_responder_link_wire_shape() {
        let mut bytes = [0_u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_add(0x40);
        }
        let attempt = AttemptToken(bytes);
        let link_id = [0xa5; 16];
        let proof = link_proof_for(77, link_id, attempt);
        assert_eq!(
            delivery_proof_correlation_tag(&proof),
            Some(u64::from_le_bytes([
                0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
            ]))
        );

        let mut wrong_packet_shape = proof.clone();
        wrong_packet_shape[0] = 0x0b;
        assert_eq!(delivery_proof_correlation_tag(&wrong_packet_shape), None);

        let mut header_two = proof.clone();
        header_two[0] |= 0x40;
        assert_eq!(delivery_proof_correlation_tag(&header_two), None);

        let mut forwarded = proof.clone();
        forwarded[1] = 1;
        assert_eq!(delivery_proof_correlation_tag(&forwarded), None);

        let mut wrong_context = proof.clone();
        wrong_context[18] = 1;
        assert_eq!(delivery_proof_correlation_tag(&wrong_context), None);

        let mut implicit = proof.clone();
        implicit.drain(19..51);
        assert_eq!(delivery_proof_correlation_tag(&implicit), None);
    }

    #[test]
    fn capacity_and_project_routing_types_have_explicit_bounds() {
        assert!(capacity_profile_is_supported(2, 1, 1, 2));
        assert!(capacity_profile_is_supported(16, 4, 32, 4));
        assert!(!capacity_profile_is_supported(3, 1, 1, 2));
        assert!(!capacity_profile_is_supported(2, 0, 1, 2));
        assert!(!capacity_profile_is_supported(2, 1, 0, 2));
        assert!(!capacity_profile_is_supported(2, 1, 1, 3));

        let first = PacketInterfaceId::new(0);
        let last = PacketInterfaceId::new(63);
        let outside = PacketInterfaceId::new(64);
        let set = InterfaceSet::empty()
            .with(first)
            .unwrap()
            .with(last)
            .unwrap();
        assert!(set.contains(first));
        assert!(set.contains(last));
        assert!(!set.contains(outside));
        assert_eq!(set.with(outside), None);
        assert_eq!(InterfaceSet::from_bits(set.bits()), set);

        assert_eq!(TxTarget::from_rns(RnsTxTarget::All), TxTarget::All);
        assert_eq!(
            TxTarget::from_rns(RnsTxTarget::Only(RnsInterfaceId(7))),
            TxTarget::Only(PacketInterfaceId::new(7))
        );
        assert_eq!(
            TxTarget::from_rns(RnsTxTarget::AllExcept(RnsInterfaceId(9))),
            TxTarget::AllExcept(PacketInterfaceId::new(9))
        );
        assert_eq!(deadline(123).instant().get(), 123);
    }

    #[test]
    fn announce_wrappers_preserve_bounded_admission_and_action_routing() {
        let mut owner = node::<2, 0>(80, "announce");
        let mut rng = CounterRng::default();
        let oversized = [0u8; MAX_ANNOUNCE_APP_DATA + 1];
        let primary = owner.destination_hash();
        let expected_public_key = identity(80).public_key();
        let delivery = owner
            .register_inbound_single_destination("lxmf", &["delivery"])
            .unwrap();

        assert_eq!(
            owner.queue_announce(Some(&oversized), announce_time(1), &mut rng),
            Err(AnnounceAdmissionError::AppDataTooLarge {
                actual: MAX_ANNOUNCE_APP_DATA + 1,
                maximum: MAX_ANNOUNCE_APP_DATA,
            })
        );
        owner
            .queue_announce(None, announce_time(1), &mut rng)
            .unwrap();
        owner
            .queue_announce_for(
                &delivery,
                Some(b"product announce"),
                announce_time(2),
                &mut rng,
            )
            .unwrap();
        assert_eq!(
            owner.queue_announce(None, announce_time(1), &mut rng),
            Err(AnnounceAdmissionError::QueueFull { limit: 2 })
        );

        let actions = owner.flush_announces(time(1), &mut rng);
        assert!(actions.events.is_empty());
        assert_eq!(actions.unroutable_packets, 0);
        assert_eq!(actions.packets.len(), 2);
        assert!(actions.packets.iter().all(|packet| {
            packet.target() == RnsTxTarget::All
                && reticulum_rns_rete::parse_announce_packet(packet.bytes())
                    .is_ok_and(|announce| announce.fields.pub_key == expected_public_key)
        }));
        let primary_announce = actions
            .packets
            .iter()
            .find_map(|packet| {
                let announce = reticulum_rns_rete::parse_announce_packet(packet.bytes()).ok()?;
                (announce.packet.destination_hash == primary.as_bytes()).then_some(announce)
            })
            .expect("primary destination announce must be present and valid");
        assert_eq!(primary_announce.fields.app_data, None);
        let delivery_announce = actions
            .packets
            .iter()
            .find_map(|packet| {
                let announce = reticulum_rns_rete::parse_announce_packet(packet.bytes()).ok()?;
                (announce.packet.destination_hash == delivery.as_bytes()).then_some(announce)
            })
            .expect("LXMF delivery announce must be present and valid");
        assert_eq!(
            delivery_announce.fields.app_data,
            Some(b"product announce".as_slice())
        );
        assert_eq!(
            AnnounceAdmissionError::from(RnsAnnounceAdmissionError::NativeRejected),
            AnnounceAdmissionError::NativeRejected
        );
    }

    #[test]
    fn inbound_proof_policy_is_explicit_and_controls_proof_actions() {
        let mut sender = node::<4, 1>(81, "sender");
        let mut receiver = node::<4, 1>(82, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();

        assert_eq!(InboundProofPolicy::default(), InboundProofPolicy::Never);
        receiver
            .queue_announce(None, announce_time(1), &mut rng)
            .unwrap();
        let announce = receiver.flush_announces(time(1), &mut rng);
        assert_eq!(announce.packets.len(), 1);
        sender
            .ingest(
                announce.packets[0].bytes(),
                time(1),
                PacketInterfaceId::new(4),
                &mut rng,
            )
            .unwrap();

        let default_actions = deliver_prepared(
            &mut sender,
            &mut receiver,
            &mut buffer,
            b"default proof policy",
            2,
            PacketInterfaceId::new(7),
            &mut rng,
        );
        assert!(default_actions.packets.is_empty());

        receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
        let proof_actions = deliver_prepared(
            &mut sender,
            &mut receiver,
            &mut buffer,
            b"proof requested",
            3,
            PacketInterfaceId::new(7),
            &mut rng,
        );
        assert_eq!(proof_actions.packets.len(), 1);
        assert_eq!(
            proof_actions.packets[0].target(),
            RnsTxTarget::Only(RnsInterfaceId(7))
        );
        assert_eq!(
            reticulum_rns_rete::parse_packet(proof_actions.packets[0].bytes())
                .unwrap()
                .packet_type,
            reticulum_rns_rete::PacketType::Proof
        );

        receiver.set_inbound_proof_policy(InboundProofPolicy::Never);
        let disabled_actions = deliver_prepared(
            &mut sender,
            &mut receiver,
            &mut buffer,
            b"proof disabled",
            4,
            PacketInterfaceId::new(7),
            &mut rng,
        );
        assert!(disabled_actions.packets.is_empty());
    }

    #[test]
    fn destination_link_policy_rejects_an_unregistered_destination() {
        let mut receiver = node::<4, 0>(83, "link-policy");

        assert_eq!(
            receiver.set_destination_accepts_links(&DestinationHash::new([0xee; 16]), true),
            Err(LocalDestinationLinkPolicyError::DestinationNotRegistered)
        );
    }

    #[test]
    fn destination_link_policy_only_controls_local_link_admission() {
        let mut sender = node::<8, 1>(85, "sender");
        let mut receiver = node::<8, 1>(86, "primary");
        let delivery = receiver
            .register_inbound_single_destination("lxmf", &["delivery"])
            .unwrap();
        receiver
            .set_destination_inbound_proof_policy(&delivery, InboundProofPolicy::Always)
            .unwrap();
        sender
            .register_peer(&identity(86), "lxmf", &["delivery"], time(0))
            .unwrap();

        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let link_destination = RnsDestinationHash::from(*delivery.as_bytes());
        let (link_request, _) = sender
            .rns
            .initiate_link(link_destination, 1, &mut rng)
            .expect("test LINKREQUEST must construct");

        receiver
            .set_destination_accepts_links(&delivery, false)
            .unwrap();
        let rejected = receiver
            .ingest(
                link_request.bytes(),
                time(1),
                PacketInterfaceId::new(7),
                &mut rng,
            )
            .unwrap();
        assert_eq!(
            rejected.disposition,
            IngressDisposition::Rejected(
                reticulum_rns_rete::IngressDropReason::DestinationDoesNotAcceptLinks,
            )
        );
        assert!(rejected.actions.events.is_empty());
        assert!(rejected.actions.packets.is_empty());

        let links_disabled_data = deliver_prepared_to(
            &mut sender,
            &mut receiver,
            &mut buffer,
            delivery,
            b"DATA while Links are disabled",
            2,
            PacketInterfaceId::new(11),
            default_interfaces(),
            &mut rng,
        );
        assert!(matches!(
            links_disabled_data.events.as_slice(),
            [ApplicationEvent::DataReceived {
                destination,
                payload,
            }] if *destination == *delivery.as_bytes()
                && payload == b"DATA while Links are disabled"
        ));
        assert_eq!(links_disabled_data.packets.len(), 1);
        assert_eq!(
            reticulum_rns_rete::parse_packet(links_disabled_data.packets[0].bytes())
                .unwrap()
                .packet_type,
            PacketType::Proof
        );

        receiver
            .set_destination_accepts_links(&delivery, true)
            .unwrap();
        let admitted = receiver
            .ingest(
                link_request.bytes(),
                time(3),
                PacketInterfaceId::new(7),
                &mut rng,
            )
            .unwrap();
        assert_eq!(admitted.disposition, IngressDisposition::Processed);
        assert!(admitted.actions.events.is_empty());
        assert_eq!(admitted.actions.packets.len(), 1);
        assert!(admitted.actions.packets[0].protocol_token().is_some());
        assert_eq!(
            reticulum_rns_rete::parse_packet(admitted.actions.packets[0].bytes())
                .unwrap()
                .packet_type,
            PacketType::Proof
        );

        let links_enabled_data = deliver_prepared_to(
            &mut sender,
            &mut receiver,
            &mut buffer,
            delivery,
            b"DATA while Links are enabled",
            4,
            PacketInterfaceId::new(13),
            default_interfaces(),
            &mut rng,
        );
        assert!(matches!(
            links_enabled_data.events.as_slice(),
            [ApplicationEvent::DataReceived {
                destination,
                payload,
            }] if *destination == *delivery.as_bytes()
                && payload == b"DATA while Links are enabled"
        ));
        assert_eq!(links_enabled_data.packets.len(), 1);
        assert_eq!(
            reticulum_rns_rete::parse_packet(links_enabled_data.packets[0].bytes())
                .unwrap()
                .packet_type,
            PacketType::Proof
        );
    }

    #[test]
    fn per_destination_retained_proof_moves_through_portable_owners_before_release() {
        let mut sender = node::<8, 1>(83, "sender");
        let mut receiver = node::<8, 1>(84, "primary");
        let primary = receiver.destination_hash();
        let delivery = receiver
            .register_inbound_single_destination("lxmf", &["delivery"])
            .unwrap();
        receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
        receiver
            .set_destination_inbound_proof_policy(&delivery, InboundProofPolicy::Retain)
            .unwrap();

        assert_eq!(
            receiver.set_destination_inbound_proof_policy(
                &DestinationHash::new([0xee; 16]),
                InboundProofPolicy::Retain,
            ),
            Err(LocalDestinationProofPolicyError::DestinationNotRegistered)
        );
        assert_eq!(
            LocalDestinationProofPolicyError::from(
                RnsInboundProofPolicyError::RetainRequiresInboundSingle,
            ),
            LocalDestinationProofPolicyError::RetainRequiresInboundSingle
        );

        sender
            .register_peer(&identity(84), "reticulum", &["primary"], time(0))
            .unwrap();
        sender
            .register_peer(&identity(84), "lxmf", &["delivery"], time(0))
            .unwrap();
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();

        let primary_source = PacketInterfaceId::new(17);
        let primary_actions = deliver_prepared_to(
            &mut sender,
            &mut receiver,
            &mut buffer,
            primary,
            b"primary immediate",
            1,
            primary_source,
            default_interfaces(),
            &mut rng,
        );
        assert_eq!(primary_actions.packets.len(), 1);
        assert_eq!(primary_actions.retained_proof_count(), 0);
        assert_eq!(
            primary_actions.packets[0].target(),
            RnsTxTarget::Only(RnsInterfaceId(primary_source.get()))
        );
        assert_eq!(
            reticulum_rns_rete::parse_packet(primary_actions.packets[0].bytes())
                .unwrap()
                .packet_type,
            reticulum_rns_rete::PacketType::Proof
        );

        let retained_source = PacketInterfaceId::new(23);
        let retained_actions = deliver_prepared_to(
            &mut sender,
            &mut receiver,
            &mut buffer,
            delivery,
            b"durable before proof",
            2,
            retained_source,
            default_interfaces(),
            &mut rng,
        );
        assert!(retained_actions.packets.is_empty());
        assert_eq!(retained_actions.retained_proof_count(), 1);
        assert!(matches!(
            retained_actions.events.as_slice(),
            [ApplicationEvent::DataReceived {
                destination,
                payload,
            }] if *destination == *delivery.as_bytes() && payload == b"durable before proof"
        ));

        let mut event_slots = [ApplicationEventSlot::new()];
        let mut event_owner = ApplicationEventOwner::new(&mut event_slots);
        let offered = event_owner.try_offer_actions(retained_actions).unwrap();
        assert_eq!(offered.accepted_events(), 1);
        assert_eq!(offered.accepted_retained_proofs(), 1);
        let lease = event_owner.lease_next().unwrap();
        assert!(lease.has_retained_proof());

        let mut proof_slots = [DelayedProofSlot::new()];
        let mut proof_owner = DelayedProofOwner::new(&mut proof_slots);
        let committed = lease
            .try_reserve_delayed(&mut proof_owner)
            .unwrap()
            .acknowledge_into_ready();
        assert_eq!(committed.event().kind(), ApplicationEventKind::DataReceived);
        assert_eq!(event_owner.capacities().vacant, 1);
        assert_eq!(proof_owner.capacities().ready, 1);

        let released = proof_owner.lease_next().unwrap().release_actions();
        assert!(released.events.is_empty());
        assert_eq!(released.retained_proof_count(), 0);
        assert_eq!(released.packets.len(), 1);
        assert_eq!(
            released.packets[0].target(),
            RnsTxTarget::Only(RnsInterfaceId(retained_source.get()))
        );
        assert_eq!(
            reticulum_rns_rete::parse_packet(released.packets[0].bytes())
                .unwrap()
                .packet_type,
            reticulum_rns_rete::PacketType::Proof
        );
        assert_eq!(proof_owner.capacities().vacant, 1);
    }

    #[test]
    fn registration_assigns_stable_unique_slots_exactly_once() {
        let mut owner = node::<2, 2>(1, "owner");
        let mut first = TxPacketBuffer::new();
        let mut second = TxPacketBuffer::new();
        let mut extra = TxPacketBuffer::new();

        assert_eq!(owner.register_packet_buffer(&mut first).unwrap().get(), 0);
        assert_eq!(owner.register_packet_buffer(&mut second).unwrap().get(), 1);
        assert_eq!(
            owner.register_packet_buffer(&mut first),
            Err(BufferRegistrationError::AlreadyRegistered)
        );
        assert_eq!(
            owner.register_packet_buffer(&mut extra),
            Err(BufferRegistrationError::PoolFull { limit: 2 })
        );
        assert_eq!(first.slot_id().unwrap().get(), 0);
        assert_eq!(second.slot_id().unwrap().get(), 1);
        assert_eq!(extra.slot_id(), None);
        assert_eq!(owner.capacities().packet_buffers_registered, 2);
    }

    #[test]
    fn available_buffer_validation_is_read_only_owner_bound_and_state_exact() {
        let mut owner = node::<2, 1>(73, "validation-owner");
        let mut other = node::<2, 1>(74, "validation-other");
        let receiver = node::<2, 0>(75, "validation-receiver");
        register_receiver(&mut owner, 75, "validation-receiver");
        let mut buffer = TxPacketBuffer::new();
        let mut foreign = TxPacketBuffer::new();
        let unregistered = TxPacketBuffer::new();
        let slot = owner.register_packet_buffer(&mut buffer).unwrap();
        other.register_packet_buffer(&mut foreign).unwrap();

        let before = owner.capacities();
        assert_eq!(owner.validate_available_buffer(&buffer), Ok(slot));
        assert_eq!(
            owner.validate_available_buffer(&unregistered),
            Err(AvailableBufferError::Unregistered)
        );
        assert_eq!(
            owner.validate_available_buffer(&foreign),
            Err(AvailableBufferError::ForeignOwner)
        );
        assert_eq!(owner.capacities(), before);

        let mut rng = CounterRng::default();
        let job = prepare(
            &mut owner,
            &mut buffer,
            receiver.destination_hash(),
            b"validation busy",
            1,
            &mut rng,
        );
        let busy_before = owner.capacities();
        assert_eq!(
            owner.validate_available_buffer(job.owner.buffer),
            Err(AvailableBufferError::Busy)
        );
        assert_eq!(owner.capacities(), busy_before);
        let disposition = match owner.rollback_queued(job, owner_time(1_100)) {
            Ok(disposition) => disposition,
            Err(failure) => panic!("fresh job must roll back: {:?}", failure.reason()),
        };
        let buffer = match disposition {
            TxCompletionDisposition::Available(buffer) => buffer,
            TxCompletionDisposition::Recovered { .. } => panic!("fresh rollback recovered"),
            TxCompletionDisposition::Next(_) => panic!("fresh rollback continued fanout"),
            TxCompletionDisposition::Quarantined(_) => panic!("fresh rollback quarantined"),
        };
        assert_eq!(owner.validate_available_buffer(buffer), Ok(slot));

        owner.dispatches[slot.index()].state = DispatchState::Unregistered;
        let invariant_before = owner.capacities();
        assert_eq!(
            owner.validate_available_buffer(buffer),
            Err(AvailableBufferError::MetadataInvariant)
        );
        assert_eq!(owner.capacities(), invariant_before);
        owner.dispatches[slot.index()].state = DispatchState::Free;
        assert_eq!(owner.validate_available_buffer(buffer), Ok(slot));
    }

    #[test]
    fn zero_capacity_owner_rejects_registration_without_mutating_buffer() {
        let mut owner = node::<2, 0>(2, "owner");
        let mut buffer = TxPacketBuffer::new();
        assert_eq!(
            owner.register_packet_buffer(&mut buffer),
            Err(BufferRegistrationError::PoolFull { limit: 0 })
        );
        assert_eq!(buffer.slot_id(), None);
        assert_eq!(owner.capacities().packet_buffers_registered, 0);
    }

    #[test]
    fn announce_emission_time_enforces_the_exact_five_byte_wire_range() {
        assert_eq!(
            AnnounceEmissionTime::new(AnnounceEmissionTime::MAX)
                .expect("wire maximum must be accepted")
                .get(),
            0x00ff_ffff_ffff
        );
        assert_eq!(
            AnnounceEmissionTime::new(AnnounceEmissionTime::MAX + 1),
            Err(AnnounceEmissionTimeError::OutsideWireRange)
        );
    }

    #[test]
    fn successful_preparation_is_no_copy_parseable_and_preserves_scalar_metadata() {
        let mut sender = node::<2, 1>(3, "sender");
        let receiver = node::<2, 1>(4, "receiver");
        register_receiver(&mut sender, 4, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let original = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();

        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"external packet",
            1,
            &mut rng,
        );
        assert!(core::ptr::eq(
            core::ptr::from_ref(job.owner.buffer),
            original
        ));
        assert_eq!(job.slot_id().get(), 0);
        assert_eq!(job.target(), TxTarget::All);
        assert_eq!(job.deadline(), deadline(1_500));
        assert_eq!(job.prepared().slot_id(), job.slot_id());

        let bytes = &job.owner.buffer.bytes[..usize::from(job.packet_len())];
        let parsed = reticulum_rns_rete::parse_packet(bytes).unwrap();
        assert_eq!(&parsed.compute_hash(), job.attempt().as_bytes());
        assert_eq!(
            job.prepared().encoded_packet_sha256(),
            EncodedPacketSha256::new(Sha256::digest(bytes).into())
        );
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
    }

    #[test]
    fn learned_path_routes_local_data_only_to_its_ingress_interface() {
        let mut sender = node::<4, 1>(5, "sender");
        let mut receiver = node::<4, 1>(6, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();

        receiver
            .queue_announce(None, announce_time(1), &mut rng)
            .unwrap();
        let announce = receiver.flush_announces(time(1), &mut rng);
        assert_eq!(announce.packets.len(), 1);
        sender
            .ingest(
                announce.packets[0].bytes(),
                time(1),
                PacketInterfaceId::new(4),
                &mut rng,
            )
            .unwrap();

        let job = prepare_with_interfaces(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"one learned path",
            2,
            2_000,
            2_500,
            interfaces(&[1, 4, 7]),
            &mut rng,
        );
        assert_eq!(job.target(), TxTarget::Only(PacketInterfaceId::new(4)));
        assert_eq!(job.interface(), PacketInterfaceId::new(4));

        let returned = job.return_unpermitted().complete(TxCompletionCode::new(0));
        let disposition = sender
            .complete_tx(returned, owner_time(2_100))
            .unwrap_or_else(|failure| {
                panic!("learned-path packet did not return: {:?}", failure.reason())
            });
        assert!(matches!(disposition, TxCompletionDisposition::Available(_)));
    }

    #[test]
    fn dropping_a_job_quarantines_its_buffer_and_dispatch() {
        let mut sender = node::<2, 1>(40, "sender");
        let receiver = node::<2, 1>(41, "receiver");
        register_receiver(&mut sender, 41, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();

        let attempt = {
            let job = prepare(
                &mut sender,
                &mut buffer,
                receiver.destination_hash(),
                b"lost owner",
                1,
                &mut rng,
            );
            job.attempt()
        };
        let entropy_before = rng.0;

        let failure = match sender.prepare_data_into_slot(
            &mut buffer,
            prepare_request(
                receiver.destination_hash(),
                b"must not reuse",
                2,
                2_000,
                3_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("a bound buffer was reused after its job was lost"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), SubmitError::PacketBufferBusy);
        assert_eq!(available(failure).slot_id().unwrap().get(), 0);
        assert_eq!(rng.0, entropy_before);
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert!(matches!(
            sender.dispatches[0].state,
            DispatchState::Active(DispatchRecord { prepared, .. })
                if prepared.receipt().as_bytes() == attempt.as_bytes()
        ));
    }

    #[test]
    fn node_dispatch_slots_do_not_embed_packet_arrays() {
        let no_dispatch = core::mem::size_of::<TestNode<2, 0>>();
        let one_dispatch = core::mem::size_of::<TestNode<2, 1>>();
        let dispatch_metadata = one_dispatch - no_dispatch;

        assert!(core::mem::size_of::<TxPacketBuffer>() >= PACKET_CAPACITY);
        assert!(
            dispatch_metadata < PACKET_CAPACITY,
            "one node dispatch slot unexpectedly embeds packet-sized storage"
        );
    }

    #[test]
    fn unregistered_and_foreign_buffers_reject_before_entropy_or_rns_mutation() {
        let mut first = node::<2, 1>(5, "sender");
        let mut second = node::<2, 1>(6, "other");
        let receiver = node::<2, 1>(7, "receiver");
        register_receiver(&mut first, 7, "receiver");
        register_receiver(&mut second, 7, "receiver");
        let mut unregistered = TxPacketBuffer::new();
        let mut foreign = registered_buffer(&mut second);
        let mut rng = CounterRng::default();

        let unregistered_failure = match first.prepare_data_into_slot(
            &mut unregistered,
            prepare_request(
                receiver.destination_hash(),
                b"no registration",
                1,
                1_000,
                2_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("unregistered buffer was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(
            unregistered_failure.reason(),
            SubmitError::UnregisteredPacketBuffer
        );
        let unregistered = available(unregistered_failure);
        assert_eq!(unregistered.slot_id(), None);

        let foreign_failure = match first.prepare_data_into_slot(
            &mut foreign,
            prepare_request(
                receiver.destination_hash(),
                b"wrong owner",
                1,
                1_000,
                2_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("foreign buffer was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(foreign_failure.reason(), SubmitError::ForeignPacketBuffer);
        assert_eq!(available(foreign_failure).slot_id().unwrap().get(), 0);
        assert_eq!(rng.0, 0);
        assert_eq!(first.capacities().receipts_used, 0);
        assert_eq!(first.capacities().attempts_used, 0);
    }

    #[test]
    fn expired_preparation_rejects_before_entropy_or_state_mutation() {
        let mut sender = node::<2, 1>(71, "sender");
        let receiver = node::<2, 1>(72, "receiver");
        register_receiver(&mut sender, 72, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let pointer = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();

        let failure = match sender.prepare_data_into_slot(
            &mut buffer,
            prepare_request(
                receiver.destination_hash(),
                b"already expired",
                1,
                1_500,
                1_500,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("exact-deadline preparation was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            SubmitError::LeaseDeadlineExpired {
                now: owner_time(1_500),
                deadline: deadline(1_500),
            }
        );
        let returned = available(failure);
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(rng.0, 0);
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().attempts_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.attempt_generation, 0);
        assert_eq!(sender.dispatch_generation, 0);
        assert_eq!(sender.hop_generation, 0);
    }

    #[test]
    fn native_preparation_failures_return_the_same_available_buffer() {
        let mut sender = node::<2, 1>(8, "sender");
        let receiver = node::<2, 1>(9, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let pointer = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();

        let unknown = match sender.prepare_data_into_slot(
            &mut buffer,
            prepare_request(
                DestinationHash::new([0xa5; 16]),
                b"unknown",
                1,
                1_000,
                2_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("unknown destination was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(unknown.reason(), SubmitError::UnknownDestination);
        let returned = available(unknown);
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(rng.0, 0);
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().attempts_used, 0);

        register_receiver(&mut sender, 9, "receiver");
        let oversized = [0x5a; MAX_DATA_PAYLOAD + 1];
        let failure = match sender.prepare_data_into_slot(
            returned,
            prepare_request(
                receiver.destination_hash(),
                &oversized,
                2,
                2_000,
                3_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("oversized payload was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            SubmitError::PayloadTooLarge {
                actual: MAX_DATA_PAYLOAD + 1,
                maximum: MAX_DATA_PAYLOAD,
            }
        );
        let returned = available(failure);
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(rng.0, 0);

        let job = prepare(
            &mut sender,
            returned,
            receiver.destination_hash(),
            b"valid after failures",
            3,
            &mut rng,
        );
        assert_eq!(job.slot_id().get(), 0);
    }

    #[test]
    fn attempt_and_dispatch_exhaustion_reject_before_entropy() {
        let receiver = node::<2, 3>(11, "receiver");

        let mut attempt_full = node::<2, 3>(10, "sender");
        register_receiver(&mut attempt_full, 11, "receiver");
        let mut a = registered_buffer(&mut attempt_full);
        let mut b = registered_buffer(&mut attempt_full);
        let mut c = registered_buffer(&mut attempt_full);
        let mut rng = CounterRng::default();
        let first = prepare(
            &mut attempt_full,
            &mut a,
            receiver.destination_hash(),
            b"a",
            1,
            &mut rng,
        );
        let second = prepare(
            &mut attempt_full,
            &mut b,
            receiver.destination_hash(),
            b"b",
            2,
            &mut rng,
        );
        let rng_before = rng.0;
        let failure = match attempt_full.prepare_data_into_slot(
            &mut c,
            prepare_request(
                receiver.destination_hash(),
                b"c",
                3,
                3_000,
                4_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("full ledger accepted another attempt"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            SubmitError::AttemptLedgerFull { limit: 2 }
        );
        assert_eq!(rng.0, rng_before);
        assert_eq!(attempt_full.capacities().dispatches_queued, 2);
        let _ = (first, second);

        let mut exhausted = node::<2, 1>(12, "sender");
        register_receiver(&mut exhausted, 11, "receiver");
        let mut buffer = registered_buffer(&mut exhausted);
        exhausted.dispatch_generation = u64::MAX;
        let mut rng = CounterRng::default();
        let failure = match exhausted.prepare_data_into_slot(
            &mut buffer,
            prepare_request(
                receiver.destination_hash(),
                b"no dispatch id",
                1,
                1_000,
                2_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("exhausted dispatch generation was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), SubmitError::DispatchIdentifierExhausted);
        assert_eq!(rng.0, 0);
        assert_eq!(exhausted.capacities().dispatches_used, 0);
    }

    #[test]
    fn receipt_hash_collision_rolls_back_only_the_rejected_transaction() {
        let mut sender = node::<2, 2>(13, "sender");
        let receiver = node::<2, 2>(14, "receiver");
        register_receiver(&mut sender, 14, "receiver");
        let mut first_buffer = registered_buffer(&mut sender);
        let mut second_buffer = registered_buffer(&mut sender);
        let mut first_rng = CounterRng::default();
        let first = prepare(
            &mut sender,
            &mut first_buffer,
            receiver.destination_hash(),
            b"repeat",
            1,
            &mut first_rng,
        );
        let mut repeated_rng = CounterRng::default();

        let failure = match sender.prepare_data_into_slot(
            &mut second_buffer,
            prepare_request(
                receiver.destination_hash(),
                b"repeat",
                1,
                1_000,
                1_500,
                default_interfaces(),
            ),
            &mut repeated_rng,
        ) {
            Ok(_) => panic!("duplicate receipt hash was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), SubmitError::ReceiptHashAlreadyTracked);
        let returned = available(failure);
        assert_eq!(returned.slot_id().unwrap().get(), 1);
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(first.slot_id().get(), 0);
    }

    #[test]
    fn queue_handoff_failure_rolls_back_exact_receipt_and_reuses_same_buffer() {
        let mut sender = node::<2, 1>(15, "sender");
        let receiver = node::<2, 1>(16, "receiver");
        register_receiver(&mut sender, 16, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let pointer = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();

        let first = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"queue full",
            1,
            &mut rng,
        );
        let old_handle = first.attempt_handle();
        let returned = match sender.rollback_queued(first, owner_time(1_100)) {
            Ok(disposition) => reusable(disposition),
            Err(failure) => panic!("rollback failed: {:?}", failure.reason()),
        };
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().attempts_used, 1);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(
            sender.acknowledge_terminal(old_handle).unwrap().outcome(),
            AttemptOutcome::Unsent(AttemptUnsentReason::QueueRollback)
        );
        assert_eq!(sender.capacities().attempts_used, 0);

        let second = prepare(
            &mut sender,
            returned,
            receiver.destination_hash(),
            b"reused",
            2,
            &mut rng,
        );
        assert_eq!(second.slot_id().get(), 0);
        assert_ne!(second.attempt_handle(), old_handle);
    }

    #[test]
    fn late_queue_rollback_reports_recovery_before_reuse() {
        let mut sender = node::<2, 1>(80, "sender");
        let receiver = node::<2, 1>(81, "receiver");
        register_receiver(&mut sender, 81, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"late queue rejection",
            1,
            &mut rng,
        );
        let handle = job.attempt_handle();
        let attempt = job.attempt();

        let disposition = sender
            .rollback_queued(job, owner_time(1_500))
            .unwrap_or_else(|failure| panic!("late rollback failed: {:?}", failure.reason()));
        let (returned, observation) = match disposition {
            TxCompletionDisposition::Recovered {
                buffer,
                observation,
            } => (buffer, observation),
            TxCompletionDisposition::Available(_) => panic!("late rollback hid recovery"),
            TxCompletionDisposition::Next(_) => panic!("late rollback advanced fanout"),
            TxCompletionDisposition::Quarantined(_) => panic!("matching rollback quarantined"),
        };
        let record = observation.record();
        assert_eq!(observation.attempt_handle(), handle);
        assert_eq!(observation.attempt(), attempt);
        assert_eq!(returned.slot_id().unwrap().get(), 0);
        assert_eq!(record.instance(), sender.instance_id());
        assert_eq!(record.reason(), TxRecoveryReason::DeadlineExpired);
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Unpermitted);
        assert!(!record.may_have_transmitted());
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.capacities().attempts_used, 1);
        assert_eq!(
            sender.acknowledge_terminal(handle).unwrap().outcome(),
            AttemptOutcome::Unsent(AttemptUnsentReason::PermitDeadlineExpired)
        );

        let second = prepare(
            &mut sender,
            returned,
            receiver.destination_hash(),
            b"maintenance won the deadline race",
            2,
            &mut rng,
        );
        assert_eq!(
            sender.maintain_tx(owner_time(2_500)),
            TxMaintenanceReport {
                newly_recovery_required: 1,
            }
        );
        let disposition = sender
            .rollback_queued(second, owner_time(2_500))
            .unwrap_or_else(|failure| {
                panic!("maintained late rollback failed: {:?}", failure.reason())
            });
        assert!(matches!(
            disposition,
            TxCompletionDisposition::Recovered {
                observation,
                ..
            } if observation.record().reason() == TxRecoveryReason::DeadlineExpired
                && observation.record().prior_phase() == TxRecoveryPriorPhase::Unpermitted
                && !observation.record().may_have_transmitted()
        ));
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.capacities().attempts_used, 1);
    }

    #[test]
    fn rollback_cannot_cancel_after_an_earlier_route_was_authorized() {
        let mut sender = node::<2, 1>(73, "sender");
        let receiver = node::<2, 1>(74, "receiver");
        register_receiver(&mut sender, 74, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let route = interfaces(&[2, 5]);
        let first = prepare_with_interfaces(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"authorized before next queue",
            2,
            2_000,
            3_000,
            route,
            &mut rng,
        );
        let (pending, request) = first.begin_permit(test_permit_requirements());
        let reply = sender
            .authorize_tx(request, owner_time(2_100), &mut TestPolicy::allowing())
            .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));
        let first = match pending.resolve(reply, owner_time(2_100)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Unpermitted(_)) => panic!("first route was denied"),
            Ok(PermitResolution::Expired(_)) => panic!("first route expired"),
            Err(_) => panic!("first permit reply mismatched"),
        };
        let next = match sender
            .complete_tx(first.complete(TxCompletionCode::new(40)), owner_time(2_150))
            .unwrap_or_else(|failure| panic!("first completion failed: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Next(next) => next,
            TxCompletionDisposition::Available(_) => panic!("fanout stopped early"),
            TxCompletionDisposition::Recovered { .. } => panic!("fresh fanout recovered"),
            TxCompletionDisposition::Quarantined(_) => panic!("valid fanout quarantined"),
        };

        let failure = match sender.rollback_queued(next, owner_time(2_200)) {
            Ok(_) => panic!("rollback cancelled a possibly transmitted attempt"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), RollbackErrorKind::PriorAuthorization);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
        let next = failure.into_job();
        assert!(matches!(
            sender
                .complete_tx(
                    next.return_unpermitted()
                        .complete(TxCompletionCode::new(41)),
                    owner_time(2_200),
                )
                .unwrap_or_else(|failure| panic!("final return failed: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
    }

    #[test]
    fn missing_rollback_receipt_retains_the_bound_unique_job() {
        let mut sender = node::<2, 1>(17, "sender");
        let receiver = node::<2, 1>(18, "receiver");
        register_receiver(&mut sender, 18, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"invariant",
            1,
            &mut rng,
        );
        let attempt = job.attempt();
        let DispatchState::Active(DispatchRecord { prepared, .. }) = sender.dispatches[0].state
        else {
            panic!("expected queued dispatch")
        };
        assert!(sender.rns.cancel_data_receipt(prepared.receipt()));

        let failure = match sender.rollback_queued(job, owner_time(1_100)) {
            Ok(_) => panic!("missing receipt was hidden"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            RollbackErrorKind::ReceiptMissing { attempt }
        );
        let job = failure.into_job();
        assert_eq!(job.attempt(), attempt);
        assert!(matches!(
            job.owner.buffer.binding,
            BufferBinding::Bound { .. }
        ));
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
    }

    #[test]
    fn cross_incarnation_rollback_returns_job_to_the_correct_owner() {
        let receiver = node::<2, 1>(20, "receiver");
        let mut first = node_with_instance::<2, 1>(19, "sender", 1);
        let mut reconstructed = node_with_instance::<2, 1>(19, "sender", 2);
        register_receiver(&mut first, 20, "receiver");
        register_receiver(&mut reconstructed, 20, "receiver");
        let mut buffer = registered_buffer(&mut first);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut first,
            &mut buffer,
            receiver.destination_hash(),
            b"owned by first",
            1,
            &mut rng,
        );

        let failure = match reconstructed.rollback_queued(job, owner_time(1_100)) {
            Ok(_) => panic!("cross-incarnation job was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), RollbackErrorKind::StaleOrUnknown);
        let job = failure.into_job();
        assert_eq!(first.capacities().dispatches_queued, 1);
        assert_eq!(reconstructed.capacities().dispatches_used, 0);
        assert!(first.rollback_queued(job, owner_time(1_100)).is_ok());
    }

    #[test]
    fn terminal_token_mismatch_retains_the_bound_job() {
        let mut sender = node::<2, 1>(42, "sender");
        let receiver = node::<2, 1>(43, "receiver");
        register_receiver(&mut sender, 43, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"terminal invariant",
            1,
            &mut rng,
        );
        let handle = job.attempt_handle();
        sender
            .ingest(
                &proof_for(43, job.attempt()),
                time(2),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        sender.attempts[handle.slot].state = AttemptState::Terminal {
            generation: handle.generation,
            token: AttemptToken([0xa5; 32]),
            outcome: AttemptOutcome::Delivered,
        };

        let failure = match sender.rollback_queued(job, owner_time(1_100)) {
            Ok(_) => panic!("a mismatched terminal token released the packet buffer"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), RollbackErrorKind::Invariant);
        let job = failure.into_job();
        assert_eq!(job.attempt_handle(), handle);
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(sender.capacities().attempts_terminal, 1);
    }

    #[test]
    fn valid_proof_commits_terminal_while_job_ownership_remains_bound() {
        let mut sender = node::<2, 1>(21, "sender");
        let receiver = node::<2, 1>(22, "receiver");
        register_receiver(&mut sender, 22, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"proof race",
            1,
            &mut rng,
        );
        let scalar = job.prepared();
        let proof = proof_for(22, job.attempt());

        sender
            .ingest(&proof, time(2), PacketInterfaceId::new(2), &mut rng)
            .unwrap();
        let terminal = sender.terminal_attempts().next().unwrap();
        assert_eq!(terminal.handle(), scalar.handle());
        assert_eq!(terminal.outcome(), AttemptOutcome::Delivered);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.capacities().dispatches_queued, 1);
        assert_eq!(
            sender.acknowledge_terminal(scalar.handle()),
            Err(AcknowledgeError::PacketStillBound)
        );

        let returned = reusable(
            sender
                .rollback_queued(job, owner_time(1_100))
                .unwrap_or_else(|failure| {
                    panic!("terminal rollback failed: {:?}", failure.reason())
                }),
        );
        assert_eq!(returned.slot_id().unwrap(), scalar.slot_id());
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.acknowledge_terminal(scalar.handle()), Ok(terminal));
    }

    #[test]
    fn timeout_commits_terminal_while_job_ownership_remains_bound() {
        let mut sender = node::<2, 1>(23, "sender");
        let receiver = node::<2, 1>(24, "receiver");
        register_receiver(&mut sender, 24, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"timeout race",
            100,
            &mut rng,
        );
        let scalar = job.prepared();

        let report = sender.tick(time(132), &mut rng);
        assert_eq!(report.timed_out_attempts, 1);
        assert_eq!(
            report.timed_out_attempt_tag,
            Some(u64::from_le_bytes(
                job.attempt().as_bytes()[..8].try_into().unwrap()
            ))
        );
        assert!(report.timed_out_attempt_tags_consistent);
        assert_eq!(report.correlation_fault, None);
        let terminal = sender.terminal_attempts().next().unwrap();
        assert_eq!(terminal.outcome(), AttemptOutcome::DeliveryTimeout);
        assert_eq!(
            sender.acknowledge_terminal(scalar.handle()),
            Err(AcknowledgeError::PacketStillBound)
        );
        let returned = reusable(
            sender
                .rollback_queued(job, owner_time(100_100))
                .unwrap_or_else(|failure| {
                    panic!("terminal rollback failed: {:?}", failure.reason())
                }),
        );
        assert_eq!(returned.slot_id().unwrap(), scalar.slot_id());
        assert_eq!(sender.acknowledge_terminal(scalar.handle()), Ok(terminal));
    }

    #[test]
    fn terminal_tombstones_backpressure_until_exact_acknowledgement() {
        let mut sender = node::<2, 2>(44, "sender");
        let receiver = node::<2, 2>(45, "receiver");
        register_receiver(&mut sender, 45, "receiver");
        let mut first_buffer = registered_buffer(&mut sender);
        let mut second_buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();

        let first = prepare(
            &mut sender,
            &mut first_buffer,
            receiver.destination_hash(),
            b"first tombstone",
            1,
            &mut rng,
        );
        let first_scalar = first.prepared();
        sender
            .ingest(
                &proof_for(45, first.attempt()),
                time(2),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        let first_buffer = reusable(
            sender
                .rollback_queued(first, owner_time(1_100))
                .unwrap_or_else(|failure| {
                    panic!("first terminal return failed: {:?}", failure.reason())
                }),
        );

        let second = prepare(
            &mut sender,
            &mut second_buffer,
            receiver.destination_hash(),
            b"second tombstone",
            3,
            &mut rng,
        );
        sender
            .ingest(
                &proof_for(45, second.attempt()),
                time(4),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        let _second_buffer = reusable(
            sender
                .rollback_queued(second, owner_time(3_100))
                .unwrap_or_else(|failure| {
                    panic!("second terminal return failed: {:?}", failure.reason())
                }),
        );
        assert_eq!(sender.capacities().attempts_terminal, 2);

        let entropy_before = rng.0;
        let failure = match sender.prepare_data_into_slot(
            first_buffer,
            prepare_request(
                receiver.destination_hash(),
                b"blocked by tombstones",
                5,
                5_000,
                6_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("terminal tombstones did not backpressure preparation"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            SubmitError::AttemptLedgerFull { limit: 2 }
        );
        assert_eq!(rng.0, entropy_before);
        let first_buffer = available(failure);

        let first_terminal = sender
            .terminal_attempts()
            .find(|terminal| terminal.handle() == first_scalar.handle())
            .unwrap();
        assert_eq!(
            sender.acknowledge_terminal(first_scalar.handle()),
            Ok(first_terminal)
        );
        assert_eq!(
            sender.acknowledge_terminal(first_scalar.handle()),
            Err(AcknowledgeError::StaleOrUnknown)
        );

        let replacement = prepare(
            &mut sender,
            first_buffer,
            receiver.destination_hash(),
            b"after exact acknowledgement",
            6,
            &mut rng,
        );
        assert_ne!(replacement.attempt_handle(), first_scalar.handle());
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().attempts_terminal, 1);
    }

    #[test]
    fn invalid_proof_can_retry_without_releasing_job_or_receipt() {
        let mut sender = node::<2, 1>(25, "sender");
        let receiver = node::<2, 1>(26, "receiver");
        register_receiver(&mut sender, 26, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"proof retry",
            10,
            &mut rng,
        );
        let proof = proof_for(26, job.attempt());
        let mut invalid = proof.clone();
        *invalid.last_mut().unwrap() ^= 0xff;

        sender
            .ingest(&invalid, time(11), PacketInterfaceId::new(2), &mut rng)
            .unwrap();
        assert_eq!(sender.capacities().attempts_active, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().dispatches_queued, 1);

        sender
            .ingest(&proof, time(12), PacketInterfaceId::new(2), &mut rng)
            .unwrap();
        assert_eq!(
            sender.terminal_attempts().next().unwrap().outcome(),
            AttemptOutcome::Delivered
        );
        assert!(sender.rollback_queued(job, owner_time(10_100)).is_ok());
    }

    #[test]
    fn repeated_full_hash_selects_new_active_attempt_over_old_tombstone() {
        let mut sender = node::<2, 1>(27, "sender");
        let receiver = node::<2, 1>(28, "receiver");
        register_receiver(&mut sender, 28, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut first_rng = CounterRng::default();
        let first = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"repeat after proof",
            1,
            &mut first_rng,
        );
        let first_scalar = first.prepared();
        sender
            .ingest(
                &proof_for(28, first.attempt()),
                time(2),
                PacketInterfaceId::new(2),
                &mut first_rng,
            )
            .unwrap();
        let buffer = reusable(
            sender
                .rollback_queued(first, owner_time(1_100))
                .unwrap_or_else(|failure| {
                    panic!("first terminal return failed: {:?}", failure.reason())
                }),
        );

        let mut repeated_rng = CounterRng::default();
        let repeated = prepare(
            &mut sender,
            buffer,
            receiver.destination_hash(),
            b"repeat after proof",
            1,
            &mut repeated_rng,
        );
        assert_eq!(repeated.attempt(), first_scalar.attempt());
        assert_ne!(repeated.attempt_handle(), first_scalar.handle());
        sender
            .ingest(
                &implicit_proof_for(28, repeated.attempt()),
                time(3),
                PacketInterfaceId::new(2),
                &mut repeated_rng,
            )
            .unwrap();

        let terminals = sender.terminal_attempts().collect::<std::vec::Vec<_>>();
        assert_eq!(terminals.len(), 2);
        assert!(
            terminals
                .iter()
                .any(|item| item.handle() == first_scalar.handle())
        );
        assert!(
            terminals
                .iter()
                .any(|item| item.handle() == repeated.attempt_handle())
        );
        assert!(
            terminals
                .iter()
                .all(|item| item.outcome() == AttemptOutcome::Delivered)
        );
    }

    #[test]
    fn unknown_and_already_terminal_receipts_are_explicit_faults() {
        let receiver = node::<2, 1>(30, "receiver");

        let mut unknown = node::<2, 1>(29, "unknown");
        register_receiver(&mut unknown, 30, "receiver");
        let mut unknown_buffer = registered_buffer(&mut unknown);
        let mut rng = CounterRng::default();
        let unknown_job = prepare(
            &mut unknown,
            &mut unknown_buffer,
            receiver.destination_hash(),
            b"orphan",
            1,
            &mut rng,
        );
        unknown.attempts[unknown_job.attempt_handle().slot].state = AttemptState::Free;
        assert_eq!(
            unknown
                .ingest(
                    &proof_for(30, unknown_job.attempt()),
                    time(2),
                    PacketInterfaceId::new(2),
                    &mut rng,
                )
                .unwrap_err(),
            ReceiptCorrelationError::UnknownData {
                attempt: unknown_job.attempt(),
            }
        );
        assert_eq!(unknown.capacities().receipts_used, 1);

        let mut terminal = node::<2, 1>(31, "terminal");
        register_receiver(&mut terminal, 30, "receiver");
        let mut terminal_buffer = registered_buffer(&mut terminal);
        let terminal_job = prepare(
            &mut terminal,
            &mut terminal_buffer,
            receiver.destination_hash(),
            b"premature",
            1,
            &mut rng,
        );
        let handle = terminal_job.attempt_handle();
        terminal.attempts[handle.slot].state = AttemptState::Terminal {
            generation: handle.generation,
            token: terminal_job.attempt(),
            outcome: AttemptOutcome::Delivered,
        };
        assert_eq!(
            terminal
                .ingest(
                    &proof_for(30, terminal_job.attempt()),
                    time(2),
                    PacketInterfaceId::new(2),
                    &mut rng,
                )
                .unwrap_err(),
            ReceiptCorrelationError::AlreadyTerminal {
                attempt: terminal_job.attempt(),
            }
        );
        assert_eq!(terminal.capacities().receipts_used, 1);
    }

    #[test]
    fn attempt_generation_exhaustion_rejects_before_any_transaction_mutation() {
        let mut sender = node::<2, 1>(34, "sender");
        let receiver = node::<2, 1>(35, "receiver");
        register_receiver(&mut sender, 35, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        sender.attempt_generation = u64::MAX;
        let mut rng = CounterRng::default();

        let failure = match sender.prepare_data_into_slot(
            &mut buffer,
            prepare_request(
                receiver.destination_hash(),
                b"no attempt identifier",
                1,
                1_000,
                2_000,
                default_interfaces(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("exhausted attempt generation was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(failure.reason(), SubmitError::AttemptIdentifierExhausted);
        assert_eq!(available(failure).slot_id().unwrap().get(), 0);
        assert_eq!(rng.0, 0);
        assert_eq!(sender.dispatch_generation, 0);
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().attempts_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
    }

    #[test]
    fn unknown_timeout_preserves_native_receipt_and_returns_tick_actions() {
        let mut sender = node::<2, 1>(36, "sender");
        let receiver = node::<2, 1>(37, "receiver");
        register_receiver(&mut sender, 37, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"orphan timeout",
            100,
            &mut rng,
        );
        sender.attempts[job.attempt_handle().slot].state = AttemptState::Free;

        let report = sender.tick(time(132), &mut rng);
        assert_eq!(report.timed_out_attempts, 0);
        assert_eq!(report.timed_out_attempt_tag, None);
        assert!(report.timed_out_attempt_tags_consistent);
        assert_eq!(
            report.correlation_fault,
            Some(ReceiptCorrelationError::UnknownData {
                attempt: job.attempt(),
            })
        );
        assert_eq!(report.actions.events.len(), 1);
        assert!(report.actions.packets.is_empty());
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().dispatches_queued, 1);
    }

    #[test]
    fn outbound_protocol_confirmation_delegates_the_exact_interface_and_interval() {
        let mut initiator = node::<4, 0>(40, "protocol-confirm-initiator");
        let mut responder = node::<4, 0>(41, "protocol-confirm-responder");
        let responder_destination = responder.rns.destination_hash();
        assert!(
            responder
                .rns
                .set_accepts_links(&responder_destination, true)
        );
        let mut rng = CounterRng::default();
        let (request, _) = initiator
            .rns
            .initiate_link(responder_destination, 100, &mut rng)
            .expect("test LINKREQUEST must construct");
        let report = responder
            .ingest_at(
                request.bytes(),
                time(100),
                MonotonicInstant::from_micros(100_062_500),
                PacketInterfaceId::new(7),
                &mut rng,
            )
            .expect("test LINKREQUEST must not hit receipt correlation");
        let token = report.actions.packets[0]
            .protocol_token()
            .expect("LRPROOF must carry a protocol timing token");
        let interval = OutboundDispatchInterval::new(
            MonotonicInstant::from_micros(100_125_000),
            MonotonicInstant::from_micros(100_250_000),
        )
        .unwrap();

        assert!(responder.confirm_outbound_protocol(token, PacketInterfaceId::new(7), interval));
        assert!(!responder.confirm_outbound_protocol(token, PacketInterfaceId::new(7), interval));
    }

    #[test]
    fn channel_receipt_candidate_is_an_explicit_unsupported_fault() {
        let mut initiator = node::<4, 2>(38, "initiator");
        let mut responder = node::<4, 2>(39, "responder");
        register_receiver(&mut initiator, 39, "responder");
        let responder_destination = responder.rns.destination_hash();
        assert!(
            responder
                .rns
                .set_accepts_links(&responder_destination, true)
        );
        let mut rng = CounterRng::default();
        let (request, link_id) = initiator
            .rns
            .initiate_link(responder_destination, 100, &mut rng)
            .unwrap();
        let response = responder
            .rns
            .ingest(request.bytes(), 100, RnsInterfaceId(1), &mut rng);
        let established = initiator.rns.ingest(
            response.actions.packets[0].bytes(),
            101,
            RnsInterfaceId(2),
            &mut rng,
        );
        responder.rns.ingest(
            established.actions.packets[0].bytes(),
            102,
            RnsInterfaceId(1),
            &mut rng,
        );
        let message = initiator
            .rns
            .send_channel_message(&link_id, 7, b"channel proof", 110, &mut rng)
            .unwrap();
        let proof_actions = responder
            .rns
            .ingest(message.bytes(), 110, RnsInterfaceId(1), &mut rng);
        let proof = proof_actions
            .actions
            .packets
            .iter()
            .find(|packet| {
                packet
                    .bytes()
                    .first()
                    .is_some_and(|header| header & 0x03 == 0x03)
            })
            .unwrap();

        assert_eq!(
            initiator
                .ingest(
                    proof.bytes(),
                    time(111),
                    PacketInterfaceId::new(2),
                    &mut rng,
                )
                .unwrap_err(),
            ReceiptCorrelationError::UnsupportedChannel
        );
        assert_eq!(initiator.rns.metrics().capacity.channel_receipts.used, 1);
    }

    #[test]
    fn full_hash_candidate_selects_the_exact_active_attempt() {
        let mut sender = node::<2, 1>(32, "sender");
        let receiver = node::<2, 1>(33, "receiver");
        register_receiver(&mut sender, 33, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"full hash selection",
            1,
            &mut rng,
        );
        let original = job.prepared();
        let generation = original.handle().generation;
        let mut colliding = original.attempt();
        colliding.0[31] ^= 0xff;
        sender.attempts[0].state = AttemptState::Active {
            generation: generation + 1,
            token: colliding,
        };
        sender.attempts[1].state = AttemptState::Active {
            generation,
            token: original.attempt(),
        };
        let DispatchState::Active(mut record) = sender.dispatches[0].state else {
            panic!("expected queued dispatch")
        };
        let exact = AttemptRef {
            slot: 1,
            generation,
        };
        record.attempt = exact;
        sender.dispatches[0].state = DispatchState::Active(record);

        sender
            .ingest(
                &proof_for(33, original.attempt()),
                time(2),
                PacketInterfaceId::new(2),
                &mut rng,
            )
            .unwrap();
        assert!(matches!(
            sender.attempts[0].state,
            AttemptState::Active { token, .. } if token == colliding
        ));
        assert!(matches!(
            sender.attempts[1].state,
            AttemptState::Terminal {
                token,
                outcome: AttemptOutcome::Delivered,
                ..
            } if token == original.attempt()
        ));
        let _ = job;
    }

    #[test]
    fn route_plans_are_explicit_and_ascending_for_every_target_shape() {
        let enabled = interfaces(&[63, 5, 1]);
        let mut all = TxRoutePlan::resolve(TxTarget::All, enabled).unwrap();
        assert_eq!(all.take_next(), Some(PacketInterfaceId::new(1)));
        assert_eq!(all.take_next(), Some(PacketInterfaceId::new(5)));
        assert_eq!(all.take_next(), Some(PacketInterfaceId::new(63)));
        assert_eq!(all.take_next(), None);

        let mut only =
            TxRoutePlan::resolve(TxTarget::Only(PacketInterfaceId::new(5)), enabled).unwrap();
        assert_eq!(only.take_next(), Some(PacketInterfaceId::new(5)));
        assert_eq!(only.take_next(), None);

        let mut except =
            TxRoutePlan::resolve(TxTarget::AllExcept(PacketInterfaceId::new(5)), enabled).unwrap();
        assert_eq!(except.take_next(), Some(PacketInterfaceId::new(1)));
        assert_eq!(except.take_next(), Some(PacketInterfaceId::new(63)));
        assert_eq!(except.take_next(), None);

        assert_eq!(
            TxRoutePlan::resolve(TxTarget::Only(PacketInterfaceId::new(7)), enabled),
            Err(TxRouteError::NoEligibleInterface {
                target: TxTarget::Only(PacketInterfaceId::new(7)),
            })
        );
        assert_eq!(
            TxRoutePlan::resolve(TxTarget::All, InterfaceSet::empty()),
            Err(TxRouteError::NoEligibleInterface {
                target: TxTarget::All,
            })
        );
        assert_eq!(
            TxRoutePlan::resolve(TxTarget::Only(PacketInterfaceId::new(64)), enabled),
            Err(TxRouteError::InterfaceOutsideProfile {
                interface: PacketInterfaceId::new(64),
            })
        );
        assert_eq!(
            TxRoutePlan::resolve(TxTarget::AllExcept(PacketInterfaceId::new(255)), enabled),
            Err(TxRouteError::InterfaceOutsideProfile {
                interface: PacketInterfaceId::new(255),
            })
        );
    }

    #[test]
    fn empty_route_cancels_exact_receipt_and_returns_same_available_buffer() {
        let mut sender = node::<2, 1>(50, "sender");
        let receiver = node::<2, 1>(51, "receiver");
        register_receiver(&mut sender, 51, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let pointer = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();

        let failure = match sender.prepare_data_into_slot(
            &mut buffer,
            prepare_request(
                receiver.destination_hash(),
                b"nowhere to send",
                1,
                1_000,
                1_500,
                InterfaceSet::empty(),
            ),
            &mut rng,
        ) {
            Ok(_) => panic!("empty route was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.reason(),
            SubmitError::NoEligibleInterface {
                target: TxTarget::All,
            }
        );
        let returned = available(failure);
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_ne!(rng.0, 0, "native preparation must precede route resolution");
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().attempts_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.attempt_generation, 0);
        assert_eq!(sender.dispatch_generation, 0);
        assert_eq!(sender.hop_generation, 0);
    }

    #[test]
    fn permit_requirements_and_reservations_are_explicit_and_validated() {
        let resource = TxPermitResourceId::new([0x42; 16]);
        assert_eq!(resource.as_bytes(), &[0x42; 16]);
        assert_eq!(
            TxPermitRequirements::try_new(resource, 0),
            Err(TxPermitRequirementsError::ZeroRequiredUnits)
        );
        let requirements = TxPermitRequirements::try_new(resource, 987_654).unwrap();
        assert_eq!(requirements.resource(), resource);
        assert_eq!(requirements.required_units(), 987_654);

        assert_eq!(
            TxPermitReservation::try_new(resource, 0),
            Err(TxPermitReservationError::ZeroReservedUnits)
        );
        let reservation = TxPermitReservation::try_new(resource, 1_000_000).unwrap();
        assert_eq!(reservation.resource(), resource);
        assert_eq!(reservation.reserved_units(), 1_000_000);
    }

    #[test]
    fn authorization_rejects_underreserved_and_mismatched_resource_grants() {
        let required_resource = TxPermitResourceId::new([0x31; 16]);
        let requirements = TxPermitRequirements::try_new(required_resource, 500_000).unwrap();
        assert_noncovering_reservation_is_denied(
            90,
            91,
            requirements,
            TxPermitReservation::try_new(required_resource, 499_999).unwrap(),
        );
        assert_noncovering_reservation_is_denied(
            92,
            93,
            requirements,
            TxPermitReservation::try_new(TxPermitResourceId::new([0x32; 16]), 500_000).unwrap(),
        );
    }

    #[test]
    fn permit_grant_is_one_shot_and_authorization_keeps_the_receipt_live() {
        let mut sender = node::<2, 1>(52, "sender");
        let receiver = node::<2, 1>(53, "receiver");
        register_receiver(&mut sender, 53, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let pointer = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"authorized frame",
            1,
            &mut rng,
        );
        let packet_len = job.packet_len();
        let handle = job.attempt_handle();
        let attempt = job.attempt();
        let resource = TxPermitResourceId::new([0xa5; 16]);
        let requirements = TxPermitRequirements::try_new(resource, 123_456).unwrap();
        let reservation = TxPermitReservation::try_new(resource, 123_500).unwrap();
        let (pending, request) = job.begin_permit(requirements);
        let mut policy = TestPolicy {
            decision: TxPolicyDecision::Authorize(reservation),
            calls: 0,
            candidate: None,
        };
        let reply = match sender.authorize_tx(request, owner_time(1_001), &mut policy) {
            Ok(reply) => reply,
            Err(failure) => panic!("permit validation failed: {:?}", failure.reason()),
        };
        assert_eq!(policy.calls, 1);
        assert_eq!(
            policy.candidate,
            Some(TxAuthorizationCandidate {
                interface: PacketInterfaceId::new(1),
                packet_len,
                requirements,
                now: owner_time(1_001),
                deadline: deadline(1_500),
                may_have_transmitted: false,
            })
        );
        assert_eq!(
            match &reply {
                TxPermitReply::Granted(grant) => grant.reservation(),
                TxPermitReply::Denied(_) => panic!("covering reservation was denied"),
            },
            reservation
        );
        assert_eq!(sender.capacities().dispatches_authorized, 1);
        assert_eq!(sender.capacities().dispatches_queued, 0);
        let mut authorized = match pending.resolve(reply, owner_time(1_100)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Unpermitted(_)) => panic!("grant resolved as denial"),
            Ok(PermitResolution::Expired(_)) => panic!("fresh grant resolved as expired"),
            Err(_) => panic!("matching grant did not resolve"),
        };
        assert_eq!(authorized.reservation(), reservation);
        {
            let frame = authorized.frame(owner_time(1_100)).unwrap();
            assert_eq!(frame.interface(), PacketInterfaceId::new(1));
            assert_eq!(frame.attempt_handle(), handle);
            assert_eq!(frame.attempt(), attempt);
            assert_eq!(frame.bytes().len(), usize::from(packet_len));
            assert_eq!(
                reticulum_rns_rete::parse_packet(frame.bytes())
                    .unwrap()
                    .compute_hash(),
                *attempt.as_bytes()
            );
        }
        assert!(matches!(
            authorized.frame(owner_time(1_100)),
            Err(TxFrameError::AlreadyTaken)
        ));

        let disposition = sender
            .complete_tx(
                authorized.complete(TxCompletionCode::new(7)),
                owner_time(1_100),
            )
            .unwrap_or_else(|failure| {
                panic!("authorized completion failed: {:?}", failure.reason())
            });
        let returned = match disposition {
            TxCompletionDisposition::Available(buffer) => buffer,
            TxCompletionDisposition::Recovered { .. } => panic!("fresh return recovered"),
            TxCompletionDisposition::Next(_) => panic!("single route fanned out"),
            TxCompletionDisposition::Quarantined(_) => panic!("valid return quarantined"),
        };
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
    }

    #[test]
    fn policy_denial_returns_unpermitted_owner_and_cancels_final_receipt() {
        let mut sender = node::<2, 1>(54, "sender");
        let receiver = node::<2, 1>(55, "receiver");
        register_receiver(&mut sender, 55, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let pointer = core::ptr::from_ref(&buffer);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"deny",
            1,
            &mut rng,
        );
        let handle = job.attempt_handle();
        let (pending, request) = job.begin_permit(test_permit_requirements());
        let mut policy = TestPolicy::denying(TxPolicyDenial::ResourceUnavailable);
        let reply = sender
            .authorize_tx(request, owner_time(1_100), &mut policy)
            .unwrap_or_else(|failure| panic!("denial failed: {:?}", failure.reason()));
        let unpermitted = match pending.resolve(reply, owner_time(1_100)) {
            Ok(PermitResolution::Unpermitted(unpermitted)) => unpermitted,
            Ok(PermitResolution::Authorized(_)) => panic!("denial authorized bytes"),
            Ok(PermitResolution::Expired(_)) => panic!("denial became expired grant"),
            Err(_) => panic!("matching denial did not resolve"),
        };
        assert_eq!(
            unpermitted.denial(),
            Some(TxPermitDenialReason::Policy(
                TxPolicyDenial::ResourceUnavailable
            ))
        );
        let disposition = sender
            .complete_tx(
                unpermitted.complete(TxCompletionCode::new(8)),
                owner_time(1_200),
            )
            .unwrap_or_else(|failure| panic!("denied return failed: {:?}", failure.reason()));
        let returned = match disposition {
            TxCompletionDisposition::Available(buffer) => buffer,
            TxCompletionDisposition::Recovered { .. } => panic!("fresh denial recovered"),
            TxCompletionDisposition::Next(_) => panic!("single route fanned out"),
            TxCompletionDisposition::Quarantined(_) => panic!("valid denial quarantined"),
        };
        assert!(core::ptr::eq(core::ptr::from_ref(returned), pointer));
        assert_eq!(policy.calls, 1);
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.capacities().attempts_terminal, 1);
        assert_eq!(
            sender.acknowledge_terminal(handle).unwrap().outcome(),
            AttemptOutcome::Unsent(AttemptUnsentReason::PolicyDenied(
                TxPolicyDenial::ResourceUnavailable
            ))
        );
    }

    #[test]
    fn mismatched_permit_reply_retains_pending_owner_and_reply() {
        let mut sender = node::<2, 2>(56, "sender");
        let receiver = node::<2, 2>(57, "receiver");
        register_receiver(&mut sender, 57, "receiver");
        let mut first_buffer = registered_buffer(&mut sender);
        let mut second_buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let first = prepare_with_interfaces(
            &mut sender,
            &mut first_buffer,
            receiver.destination_hash(),
            b"first permit",
            1,
            1_000,
            3_000,
            default_interfaces(),
            &mut rng,
        );
        let second = prepare(
            &mut sender,
            &mut second_buffer,
            receiver.destination_hash(),
            b"second permit",
            2,
            &mut rng,
        );
        let (first_pending, first_request) = first.begin_permit(test_permit_requirements());
        let (second_pending, second_request) = second.begin_permit(test_permit_requirements());
        let first_reply = sender
            .authorize_tx(
                first_request,
                owner_time(1_100),
                &mut TestPolicy::allowing(),
            )
            .unwrap_or_else(|failure| panic!("first permit failed: {:?}", failure.reason()));
        let second_reply = sender
            .authorize_tx(
                second_request,
                owner_time(2_100),
                &mut TestPolicy::denying(TxPolicyDenial::PolicyDenied),
            )
            .unwrap_or_else(|failure| panic!("second permit failed: {:?}", failure.reason()));

        let mismatch = match first_pending.resolve(second_reply, owner_time(1_100)) {
            Err(mismatch) => mismatch,
            Ok(_) => panic!("reply for another slot resolved pending ownership"),
        };
        let (first_pending, second_reply) = mismatch.into_parts();
        let first_authorized = match first_pending.resolve(first_reply, owner_time(1_100)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Unpermitted(_)) => panic!("first grant became denial"),
            Ok(PermitResolution::Expired(_)) => panic!("first grant expired unexpectedly"),
            Err(_) => panic!("retained first pending owner did not resolve"),
        };
        let second_unpermitted = match second_pending.resolve(second_reply, owner_time(2_100)) {
            Ok(PermitResolution::Unpermitted(unpermitted)) => unpermitted,
            Ok(PermitResolution::Authorized(_)) => panic!("second denial became grant"),
            Ok(PermitResolution::Expired(_)) => panic!("second denial became expired grant"),
            Err(_) => panic!("retained second reply did not resolve"),
        };

        assert!(matches!(
            sender
                .complete_tx(
                    first_authorized.complete(TxCompletionCode::new(1)),
                    owner_time(2_200),
                )
                .unwrap_or_else(|failure| panic!("first completion: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
        assert!(matches!(
            sender
                .complete_tx(
                    second_unpermitted.complete(TxCompletionCode::new(2)),
                    owner_time(2_200),
                )
                .unwrap_or_else(|failure| panic!("second completion: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
    }

    #[test]
    fn terminal_before_permit_denies_and_terminal_after_permit_stops_fanout() {
        let receiver = node::<2, 1>(59, "receiver");
        let route = interfaces(&[1, 4]);

        let mut before = node::<2, 1>(58, "before");
        register_receiver(&mut before, 59, "receiver");
        let mut before_buffer = registered_buffer(&mut before);
        let mut rng = CounterRng::default();
        let before_job = prepare_with_interfaces(
            &mut before,
            &mut before_buffer,
            receiver.destination_hash(),
            b"terminal first",
            1,
            1_000,
            1_500,
            route,
            &mut rng,
        );
        let before_attempt = before_job.attempt();
        let before_handle = before_job.attempt_handle();
        let (before_pending, before_request) = before_job.begin_permit(test_permit_requirements());
        before
            .ingest(
                &proof_for(59, before_attempt),
                time(1),
                PacketInterfaceId::new(1),
                &mut rng,
            )
            .unwrap();
        let mut policy = TestPolicy::allowing();
        let before_reply = before
            .authorize_tx(before_request, owner_time(1_100), &mut policy)
            .unwrap_or_else(|failure| panic!("terminal denial failed: {:?}", failure.reason()));
        assert_eq!(policy.calls, 0);
        let before_unpermitted = match before_pending.resolve(before_reply, owner_time(1_100)) {
            Ok(PermitResolution::Unpermitted(unpermitted)) => unpermitted,
            Ok(PermitResolution::Authorized(_)) => panic!("terminal work was authorized"),
            Ok(PermitResolution::Expired(_)) => panic!("terminal denial became expired grant"),
            Err(_) => panic!("terminal denial did not resolve"),
        };
        assert!(matches!(
            before_unpermitted.denial(),
            Some(TxPermitDenialReason::AttemptTerminal(
                AttemptOutcome::Delivered
            ))
        ));
        let before_return = before
            .complete_tx(
                before_unpermitted.complete(TxCompletionCode::new(3)),
                owner_time(1_200),
            )
            .unwrap_or_else(|failure| panic!("terminal return failed: {:?}", failure.reason()));
        assert!(matches!(
            before_return,
            TxCompletionDisposition::Available(_)
        ));
        assert_eq!(
            before
                .acknowledge_terminal(before_handle)
                .unwrap()
                .outcome(),
            AttemptOutcome::Delivered
        );

        let mut after = node::<2, 1>(60, "after");
        register_receiver(&mut after, 59, "receiver");
        let mut after_buffer = registered_buffer(&mut after);
        let after_job = prepare_with_interfaces(
            &mut after,
            &mut after_buffer,
            receiver.destination_hash(),
            b"permit first",
            1,
            1_000,
            1_500,
            route,
            &mut rng,
        );
        let after_attempt = after_job.attempt();
        let after_handle = after_job.attempt_handle();
        let (after_pending, after_request) = after_job.begin_permit(test_permit_requirements());
        let after_reply = after
            .authorize_tx(
                after_request,
                owner_time(1_100),
                &mut TestPolicy::allowing(),
            )
            .unwrap_or_else(|failure| panic!("permit failed: {:?}", failure.reason()));
        let after_authorized = match after_pending.resolve(after_reply, owner_time(1_100)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Unpermitted(_)) => panic!("valid work denied"),
            Ok(PermitResolution::Expired(_)) => panic!("valid grant expired unexpectedly"),
            Err(_) => panic!("grant did not resolve"),
        };
        after
            .ingest(
                &proof_for(59, after_attempt),
                time(1),
                PacketInterfaceId::new(1),
                &mut rng,
            )
            .unwrap();
        let after_return = after
            .complete_tx(
                after_authorized.complete(TxCompletionCode::new(4)),
                owner_time(1_200),
            )
            .unwrap_or_else(|failure| panic!("authorized return failed: {:?}", failure.reason()));
        assert!(matches!(
            after_return,
            TxCompletionDisposition::Available(_)
        ));
        assert_eq!(
            after.acknowledge_terminal(after_handle).unwrap().outcome(),
            AttemptOutcome::Delivered
        );
    }

    #[test]
    fn serialized_fanout_cancels_all_unsent_but_preserves_cumulative_authorization() {
        let receiver = node::<2, 1>(62, "receiver");
        let route = interfaces(&[5, 2]);
        let mut rng = CounterRng::default();

        let mut unsent = node::<2, 1>(61, "unsent");
        register_receiver(&mut unsent, 62, "receiver");
        let mut unsent_buffer = registered_buffer(&mut unsent);
        let first = prepare_with_interfaces(
            &mut unsent,
            &mut unsent_buffer,
            receiver.destination_hash(),
            b"all unsent",
            1,
            1_000,
            1_500,
            route,
            &mut rng,
        );
        let unsent_handle = first.attempt_handle();
        assert_eq!(first.interface(), PacketInterfaceId::new(2));
        let second = match unsent
            .complete_tx(
                first
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(10)),
                owner_time(1_100),
            )
            .unwrap_or_else(|failure| panic!("first unsent return: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Next(next) => next,
            TxCompletionDisposition::Available(_) => panic!("fanout stopped after first route"),
            TxCompletionDisposition::Recovered { .. } => panic!("fresh fanout recovered"),
            TxCompletionDisposition::Quarantined(_) => panic!("valid first return quarantined"),
        };
        assert_eq!(second.interface(), PacketInterfaceId::new(5));
        let returned = match unsent
            .complete_tx(
                second
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(11)),
                owner_time(1_200),
            )
            .unwrap_or_else(|failure| panic!("second unsent return: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Available(buffer) => buffer,
            TxCompletionDisposition::Recovered { .. } => panic!("fresh final return recovered"),
            TxCompletionDisposition::Next(_) => panic!("route retried an interface"),
            TxCompletionDisposition::Quarantined(_) => panic!("valid final return quarantined"),
        };
        assert_eq!(returned.slot_id().unwrap().get(), 0);
        assert_eq!(unsent.capacities().receipts_used, 0);
        assert_eq!(unsent.capacities().attempts_terminal, 1);
        assert_eq!(
            unsent
                .acknowledge_terminal(unsent_handle)
                .unwrap()
                .outcome(),
            AttemptOutcome::Unsent(AttemptUnsentReason::Unpermitted(TxCompletionCode::new(11)))
        );

        let mut cumulative = node::<2, 1>(63, "cumulative");
        register_receiver(&mut cumulative, 62, "receiver");
        let mut cumulative_buffer = registered_buffer(&mut cumulative);
        let first = prepare_with_interfaces(
            &mut cumulative,
            &mut cumulative_buffer,
            receiver.destination_hash(),
            b"first authorized",
            2,
            2_000,
            2_500,
            route,
            &mut rng,
        );
        let (pending, request) = first.begin_permit(test_permit_requirements());
        let reply = cumulative
            .authorize_tx(request, owner_time(2_100), &mut TestPolicy::allowing())
            .unwrap_or_else(|failure| panic!("first authorization: {:?}", failure.reason()));
        let first = match pending.resolve(reply, owner_time(2_100)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Unpermitted(_)) => panic!("first hop denied"),
            Ok(PermitResolution::Expired(_)) => panic!("first hop grant expired"),
            Err(_) => panic!("first reply mismatch"),
        };
        let second = match cumulative
            .complete_tx(first.complete(TxCompletionCode::new(12)), owner_time(2_150))
            .unwrap_or_else(|failure| panic!("first authorized return: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Next(next) => next,
            TxCompletionDisposition::Available(_) => panic!("authorized fanout stopped early"),
            TxCompletionDisposition::Recovered { .. } => panic!("fresh authorization recovered"),
            TxCompletionDisposition::Quarantined(_) => panic!("authorized return quarantined"),
        };
        assert_eq!(second.interface(), PacketInterfaceId::new(5));
        let (pending, request) = second.begin_permit(test_permit_requirements());
        let mut policy = TestPolicy::denying(TxPolicyDenial::CapacityUnavailable);
        let reply = cumulative
            .authorize_tx(request, owner_time(2_200), &mut policy)
            .unwrap_or_else(|failure| panic!("second denial: {:?}", failure.reason()));
        assert_eq!(
            policy.candidate,
            Some(TxAuthorizationCandidate {
                interface: PacketInterfaceId::new(5),
                packet_len: policy.candidate.unwrap().packet_len,
                requirements: test_permit_requirements(),
                now: owner_time(2_200),
                deadline: deadline(2_500),
                may_have_transmitted: true,
            })
        );
        let second = match pending.resolve(reply, owner_time(2_200)) {
            Ok(PermitResolution::Unpermitted(unpermitted)) => unpermitted,
            Ok(PermitResolution::Authorized(_)) => panic!("second denial authorized"),
            Ok(PermitResolution::Expired(_)) => panic!("second denial became expired grant"),
            Err(_) => panic!("second reply mismatch"),
        };
        assert!(matches!(
            cumulative
                .complete_tx(
                    second.complete(TxCompletionCode::new(13)),
                    owner_time(2_300)
                )
                .unwrap_or_else(|failure| panic!("second return: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
        assert_eq!(cumulative.capacities().dispatches_used, 0);
        assert_eq!(cumulative.capacities().receipts_used, 1);
        assert_eq!(cumulative.capacities().attempts_active, 1);
    }

    #[test]
    fn next_tx_deadline_tracks_earliest_live_dispatch_and_excludes_recovery() {
        let receiver = node::<2, 1>(91, "deadline-schedule-receiver");
        let mut sender = node::<2, 2>(90, "deadline-schedule-sender");
        register_receiver(&mut sender, 91, "deadline-schedule-receiver");
        let mut later_buffer = registered_buffer(&mut sender);
        let mut earlier_buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();

        assert_eq!(sender.next_tx_deadline(), None);
        let later = prepare_with_interfaces(
            &mut sender,
            &mut later_buffer,
            receiver.destination_hash(),
            b"later routed dispatch",
            1,
            1_000,
            2_500,
            default_interfaces(),
            &mut rng,
        );
        let earlier = prepare_with_interfaces(
            &mut sender,
            &mut earlier_buffer,
            receiver.destination_hash(),
            b"earlier authorized dispatch",
            1,
            1_000,
            1_500,
            default_interfaces(),
            &mut rng,
        );
        let (pending, request) = earlier.begin_permit(test_permit_requirements());
        let reply = sender
            .authorize_tx(request, owner_time(1_400), &mut TestPolicy::allowing())
            .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));
        let earlier = match pending.resolve(reply, owner_time(1_400)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Expired(_)) => panic!("fresh grant expired"),
            Ok(PermitResolution::Unpermitted(_)) => panic!("fresh grant was denied"),
            Err(_) => panic!("fresh grant mismatched"),
        };

        assert_eq!(sender.next_tx_deadline(), Some(deadline(1_500)));
        assert_eq!(
            sender.maintain_tx(owner_time(1_499)),
            TxMaintenanceReport {
                newly_recovery_required: 0,
            }
        );
        assert_eq!(sender.next_tx_deadline(), Some(deadline(1_500)));
        assert_eq!(
            sender.maintain_tx(owner_time(1_500)),
            TxMaintenanceReport {
                newly_recovery_required: 1,
            }
        );
        assert_eq!(sender.next_tx_deadline(), Some(deadline(2_500)));
        assert_eq!(
            sender.maintain_tx(owner_time(2_500)),
            TxMaintenanceReport {
                newly_recovery_required: 1,
            }
        );
        assert_eq!(sender.next_tx_deadline(), None);

        assert!(matches!(
            sender
                .complete_tx(
                    later
                        .return_unpermitted()
                        .complete(TxCompletionCode::new(22)),
                    owner_time(2_501),
                )
                .unwrap_or_else(|failure| panic!("routed recovery return: {:?}", failure.reason())),
            TxCompletionDisposition::Recovered { .. }
        ));
        assert!(matches!(
            sender
                .complete_tx(
                    earlier.complete(TxCompletionCode::new(23)),
                    owner_time(2_501),
                )
                .unwrap_or_else(|failure| panic!(
                    "authorized recovery return: {:?}",
                    failure.reason()
                )),
            TxCompletionDisposition::Recovered { .. }
        ));
    }

    #[test]
    fn exact_deadline_tie_enters_recovery_and_matching_return_reclaims_buffer() {
        let receiver = node::<2, 1>(65, "receiver");
        let mut rng = CounterRng::default();

        let mut before_permit = node::<2, 1>(64, "before-permit");
        register_receiver(&mut before_permit, 65, "receiver");
        let mut before_buffer = registered_buffer(&mut before_permit);
        let job = prepare_with_interfaces(
            &mut before_permit,
            &mut before_buffer,
            receiver.destination_hash(),
            b"deadline before permit",
            1,
            1_000,
            1_500,
            default_interfaces(),
            &mut rng,
        );
        let handle = job.attempt_handle();
        let attempt = job.attempt();
        let (pending, request) = job.begin_permit(test_permit_requirements());
        let mut policy = TestPolicy::allowing();
        let reply = before_permit
            .authorize_tx(request, owner_time(1_500), &mut policy)
            .unwrap_or_else(|failure| panic!("deadline validation failed: {:?}", failure.reason()));
        assert_eq!(policy.calls, 0);
        let unpermitted = match pending.resolve(reply, owner_time(1_500)) {
            Ok(PermitResolution::Unpermitted(unpermitted)) => unpermitted,
            Ok(PermitResolution::Authorized(_)) => panic!("deadline tie authorized TX"),
            Ok(PermitResolution::Expired(_)) => panic!("deadline denial became grant"),
            Err(_) => panic!("deadline denial did not resolve"),
        };
        assert_eq!(
            unpermitted.denial(),
            Some(TxPermitDenialReason::DeadlineExpired)
        );
        let observation = before_permit.recovery_records().next().unwrap();
        let record = observation.record();
        assert_eq!(observation.attempt_handle(), handle);
        assert_eq!(observation.attempt(), attempt);
        assert_eq!(record.instance(), before_permit.instance_id());
        assert_eq!(record.slot_id().get(), 0);
        assert_eq!(record.dispatch_generation(), 1);
        assert_eq!(record.interface(), Some(PacketInterfaceId::new(1)));
        assert_eq!(record.deadline(), deadline(1_500));
        assert_eq!(record.observed_at(), owner_time(1_500));
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Unpermitted);
        assert!(!record.may_have_transmitted());
        assert_eq!(record.reason(), TxRecoveryReason::DeadlineExpired);
        let (returned, recovered) = match before_permit
            .complete_tx(
                unpermitted.complete(TxCompletionCode::new(20)),
                owner_time(1_501),
            )
            .unwrap_or_else(|failure| panic!("deadline return failed: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Recovered {
                buffer,
                observation,
            } => (buffer, observation),
            TxCompletionDisposition::Next(_) => panic!("recovery continued fanout"),
            TxCompletionDisposition::Available(_) => panic!("late return lost recovery record"),
            TxCompletionDisposition::Quarantined(_) => panic!("matching late return quarantined"),
        };
        assert_eq!(recovered, observation);
        assert_eq!(returned.slot_id().unwrap().get(), 0);
        assert_eq!(before_permit.capacities().dispatches_recovery_required, 0);
        assert_eq!(before_permit.capacities().dispatches_used, 0);
        assert_eq!(before_permit.capacities().receipts_used, 0);

        let mut after_permit = node::<2, 1>(66, "after-permit");
        register_receiver(&mut after_permit, 65, "receiver");
        let mut after_buffer = registered_buffer(&mut after_permit);
        let job = prepare_with_interfaces(
            &mut after_permit,
            &mut after_buffer,
            receiver.destination_hash(),
            b"deadline after permit",
            2,
            2_000,
            2_500,
            default_interfaces(),
            &mut rng,
        );
        let handle = job.attempt_handle();
        let attempt = job.attempt();
        let (pending, request) = job.begin_permit(test_permit_requirements());
        let reply = after_permit
            .authorize_tx(request, owner_time(2_400), &mut TestPolicy::allowing())
            .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));
        let authorized = match pending.resolve(reply, owner_time(2_400)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Unpermitted(_)) => panic!("pre-deadline permit denied"),
            Ok(PermitResolution::Expired(_)) => panic!("pre-deadline grant expired"),
            Err(_) => panic!("grant did not resolve"),
        };
        assert_eq!(
            after_permit.maintain_tx(owner_time(2_499)),
            TxMaintenanceReport {
                newly_recovery_required: 0,
            }
        );
        assert_eq!(after_permit.capacities().dispatches_authorized, 1);
        assert_eq!(
            after_permit.maintain_tx(owner_time(2_500)),
            TxMaintenanceReport {
                newly_recovery_required: 1,
            }
        );
        assert_eq!(
            after_permit.maintain_tx(owner_time(2_501)),
            TxMaintenanceReport {
                newly_recovery_required: 0,
            }
        );
        let observation = after_permit.recovery_records().next().unwrap();
        let record = observation.record();
        assert_eq!(observation.attempt_handle(), handle);
        assert_eq!(observation.attempt(), attempt);
        assert_eq!(record.instance(), after_permit.instance_id());
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Authorized);
        assert!(record.may_have_transmitted());
        assert_eq!(record.observed_at(), owner_time(2_500));
        let (returned, recovered) = match after_permit
            .complete_tx(
                authorized.complete(TxCompletionCode::new(21)),
                owner_time(2_600),
            )
            .unwrap_or_else(|failure| panic!("authorized recovery return: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Recovered {
                buffer,
                observation,
            } => (buffer, observation),
            TxCompletionDisposition::Next(_) => panic!("authorized recovery fanned out"),
            TxCompletionDisposition::Available(_) => panic!("late return lost recovery record"),
            TxCompletionDisposition::Quarantined(_) => panic!("matching late return quarantined"),
        };
        assert_eq!(recovered, observation);
        assert_eq!(returned.slot_id().unwrap().get(), 0);
        assert_eq!(after_permit.capacities().dispatches_recovery_required, 0);
        assert_eq!(after_permit.capacities().dispatches_used, 0);
        assert_eq!(after_permit.capacities().receipts_used, 1);
    }

    #[test]
    fn delayed_grant_and_late_frame_never_expose_bytes_after_deadline() {
        let receiver = node::<2, 1>(76, "receiver");
        let mut rng = CounterRng::default();

        let mut delayed = node::<2, 1>(75, "delayed");
        register_receiver(&mut delayed, 76, "receiver");
        let mut delayed_buffer = registered_buffer(&mut delayed);
        let job = prepare_with_interfaces(
            &mut delayed,
            &mut delayed_buffer,
            receiver.destination_hash(),
            b"delayed grant",
            1,
            1_000,
            1_500,
            default_interfaces(),
            &mut rng,
        );
        let handle = job.attempt_handle();
        let attempt = job.attempt();
        let (pending, request) = job.begin_permit(test_permit_requirements());
        let reply = delayed
            .authorize_tx(request, owner_time(1_499), &mut TestPolicy::allowing())
            .unwrap_or_else(|failure| panic!("grant failed: {:?}", failure.reason()));
        let expired = match pending.resolve(reply, owner_time(1_500)) {
            Ok(PermitResolution::Expired(expired)) => expired,
            Ok(PermitResolution::Authorized(_)) => panic!("delayed grant exposed a frame owner"),
            Ok(PermitResolution::Unpermitted(_)) => panic!("issued grant became unpermitted"),
            Err(_) => panic!("delayed matching grant mismatched"),
        };
        assert_eq!(expired.deadline(), deadline(1_500));
        let recovered = delayed
            .complete_tx(
                expired.complete(TxCompletionCode::new(50)),
                owner_time(1_500),
            )
            .unwrap_or_else(|failure| panic!("expired return failed: {:?}", failure.reason()));
        assert!(matches!(
            recovered,
            TxCompletionDisposition::Recovered {
                observation,
                ..
            } if observation.attempt_handle() == handle
                && observation.attempt() == attempt
                && observation.record().prior_phase() == TxRecoveryPriorPhase::Authorized
                && observation.record().may_have_transmitted()
                && observation.record().reason() == TxRecoveryReason::DeadlineExpired
        ));
        assert_eq!(delayed.capacities().dispatches_used, 0);
        assert_eq!(delayed.capacities().receipts_used, 1);

        let mut late_frame = node::<2, 1>(77, "late-frame");
        register_receiver(&mut late_frame, 76, "receiver");
        let mut late_buffer = registered_buffer(&mut late_frame);
        let job = prepare_with_interfaces(
            &mut late_frame,
            &mut late_buffer,
            receiver.destination_hash(),
            b"late frame",
            2,
            2_000,
            2_500,
            default_interfaces(),
            &mut rng,
        );
        let handle = job.attempt_handle();
        let attempt = job.attempt();
        let (pending, request) = job.begin_permit(test_permit_requirements());
        let reply = late_frame
            .authorize_tx(request, owner_time(2_499), &mut TestPolicy::allowing())
            .unwrap_or_else(|failure| panic!("grant failed: {:?}", failure.reason()));
        let mut authorized = match pending.resolve(reply, owner_time(2_499)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Expired(_)) => panic!("fresh grant was already expired"),
            Ok(PermitResolution::Unpermitted(_)) => panic!("fresh grant was denied"),
            Err(_) => panic!("fresh grant mismatched"),
        };
        assert!(matches!(
            authorized.frame(owner_time(2_500)),
            Err(TxFrameError::DeadlineExpired { deadline: value })
                if value == deadline(2_500)
        ));
        assert!(matches!(
            authorized.frame(owner_time(2_499)),
            Err(TxFrameError::AlreadyTaken)
        ));
        assert!(matches!(
            late_frame
                .complete_tx(
                    authorized.complete(TxCompletionCode::new(51)),
                    owner_time(2_500),
                )
                .unwrap_or_else(|failure| panic!("late frame return: {:?}", failure.reason())),
            TxCompletionDisposition::Recovered {
                observation,
                ..
            } if observation.attempt_handle() == handle
                && observation.attempt() == attempt
                && observation.record().prior_phase() == TxRecoveryPriorPhase::Authorized
                && observation.record().reason() == TxRecoveryReason::DeadlineExpired
        ));
        assert_eq!(late_frame.capacities().dispatches_used, 0);
        assert_eq!(late_frame.capacities().receipts_used, 1);
    }

    #[test]
    fn direct_late_unpermitted_return_stops_fanout_and_recovers_buffer() {
        let mut sender = node::<2, 1>(78, "sender");
        let receiver = node::<2, 1>(79, "receiver");
        register_receiver(&mut sender, 79, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare_with_interfaces(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"late before authorization",
            1,
            1_000,
            1_500,
            interfaces(&[1, 4]),
            &mut rng,
        );
        let handle = job.attempt_handle();
        let attempt = job.attempt();

        let disposition = sender
            .complete_tx(
                job.return_unpermitted().complete(TxCompletionCode::new(52)),
                owner_time(1_500),
            )
            .unwrap_or_else(|failure| panic!("late return failed: {:?}", failure.reason()));
        let (returned, observation) = match disposition {
            TxCompletionDisposition::Recovered {
                buffer,
                observation,
            } => (buffer, observation),
            TxCompletionDisposition::Next(_) => panic!("late return advanced fanout"),
            TxCompletionDisposition::Available(_) => panic!("late return hid recovery"),
            TxCompletionDisposition::Quarantined(_) => panic!("matching late return quarantined"),
        };
        let record = observation.record();
        assert_eq!(observation.attempt_handle(), handle);
        assert_eq!(observation.attempt(), attempt);
        assert_eq!(returned.slot_id().unwrap().get(), 0);
        assert_eq!(record.instance(), sender.instance_id());
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Unpermitted);
        assert!(!record.may_have_transmitted());
        assert_eq!(record.reason(), TxRecoveryReason::DeadlineExpired);
        assert_eq!(sender.capacities().dispatches_used, 0);
        assert_eq!(sender.capacities().receipts_used, 0);
        assert_eq!(sender.capacities().attempts_terminal, 1);
        assert_eq!(
            sender.acknowledge_terminal(handle).unwrap().outcome(),
            AttemptOutcome::Unsent(AttemptUnsentReason::PermitDeadlineExpired)
        );
    }

    #[test]
    fn explicit_recovery_fault_supersedes_deadline_without_cancelling_receipt() {
        let mut sender = node::<2, 1>(82, "sender");
        let receiver = node::<2, 1>(83, "receiver");
        register_receiver(&mut sender, 83, "receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare_with_interfaces(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"fault after deadline",
            1,
            1_000,
            1_500,
            default_interfaces(),
            &mut rng,
        );
        let handle = job.attempt_handle();
        let attempt = job.attempt();
        let (pending, request) = job.begin_permit(test_permit_requirements());
        let reply = sender
            .authorize_tx(request, owner_time(1_400), &mut TestPolicy::allowing())
            .unwrap_or_else(|failure| panic!("authorization failed: {:?}", failure.reason()));
        let authorized = match pending.resolve(reply, owner_time(1_400)) {
            Ok(PermitResolution::Authorized(authorized)) => authorized,
            Ok(PermitResolution::Expired(_)) => panic!("fresh grant expired"),
            Ok(PermitResolution::Unpermitted(_)) => panic!("fresh grant denied"),
            Err(_) => panic!("fresh grant mismatched"),
        };
        assert_eq!(
            sender.maintain_tx(owner_time(1_500)),
            TxMaintenanceReport {
                newly_recovery_required: 1,
            }
        );
        assert_eq!(
            sender.recovery_records().next().unwrap().record().reason(),
            TxRecoveryReason::DeadlineExpired
        );

        let code = TxCompletionCode::new(0x55aa);
        let quarantine = match sender
            .complete_tx(authorized.recovery_fault(code), owner_time(1_600))
            .unwrap_or_else(|failure| panic!("fault return failed: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            TxCompletionDisposition::Available(_) => panic!("fault released buffer"),
            TxCompletionDisposition::Recovered { .. } => panic!("fault became ordinary recovery"),
            TxCompletionDisposition::Next(_) => panic!("fault advanced fanout"),
        };
        assert_eq!(quarantine.attempt_handle(), handle);
        assert_eq!(quarantine.attempt(), attempt);
        let record = quarantine.record();
        assert_eq!(record.instance(), sender.instance_id());
        assert_eq!(record.reason(), TxRecoveryReason::CompletionFault(code));
        assert_eq!(record.observed_at(), owner_time(1_600));
        assert_eq!(record.prior_phase(), TxRecoveryPriorPhase::Authorized);
        assert!(record.may_have_transmitted());
        assert_eq!(
            sender.recovery_records().next(),
            Some(quarantine.observation())
        );
        assert_eq!(sender.capacities().dispatches_recovery_required, 1);
        assert_eq!(sender.capacities().receipts_used, 1);
        assert_eq!(sender.capacities().attempts_active, 1);
    }

    #[test]
    fn invariant_quarantine_uses_authoritative_attempt_correlation() {
        let mut sender = node::<2, 1>(84, "invariant-owner");
        let receiver = node::<2, 1>(85, "invariant-receiver");
        register_receiver(&mut sender, 85, "invariant-receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"authoritative quarantine correlation",
            1,
            &mut rng,
        );
        let authoritative_handle = job.attempt_handle();
        let authoritative_attempt = job.attempt();

        let tampered_handle = AttemptHandle {
            generation: authoritative_handle.generation + 1,
            ..authoritative_handle
        };
        let mut tampered_attempt_bytes = *authoritative_attempt.as_bytes();
        tampered_attempt_bytes[0] ^= 0xff;
        let tampered_attempt = AttemptToken(tampered_attempt_bytes);
        let BufferBinding::Bound {
            dispatch_generation,
            prepared,
            hop,
        } = job.owner.buffer.binding
        else {
            panic!("prepared job did not retain a bound buffer")
        };
        job.owner.buffer.binding = BufferBinding::Bound {
            dispatch_generation,
            prepared: PreparedPacket {
                handle: tampered_handle,
                attempt: tampered_attempt,
                ..prepared
            },
            hop,
        };
        assert_eq!(job.attempt_handle(), tampered_handle);
        assert_eq!(job.attempt(), tampered_attempt);

        let quarantine = match sender
            .complete_tx(
                job.return_unpermitted().complete(TxCompletionCode::new(41)),
                owner_time(1_100),
            )
            .unwrap_or_else(|failure| panic!("invariant return failed: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            TxCompletionDisposition::Available(_) => panic!("invariant released buffer"),
            TxCompletionDisposition::Recovered { .. } => panic!("invariant became recovery"),
            TxCompletionDisposition::Next(_) => panic!("invariant continued fanout"),
        };

        let authoritative_observation = sender
            .recovery_records()
            .next()
            .expect("invariant quarantine must remain in authoritative recovery");
        assert_eq!(quarantine.observation(), authoritative_observation);
        assert_eq!(quarantine.attempt_handle(), authoritative_handle);
        assert_eq!(quarantine.attempt(), authoritative_attempt);
        assert_eq!(quarantine.record().reason(), TxRecoveryReason::Invariant);
        assert_ne!(quarantine.attempt_handle(), tampered_handle);
        assert_ne!(quarantine.attempt(), tampered_attempt);
        assert_eq!(quarantine.owner.bound().0.handle(), authoritative_handle);
        assert_eq!(quarantine.owner.bound().0.attempt(), authoritative_attempt);
    }

    #[test]
    fn completion_digest_tamper_quarantines_and_restores_authoritative_metadata() {
        let mut sender = node::<2, 1>(86, "digest-owner");
        let receiver = node::<2, 1>(87, "digest-receiver");
        register_receiver(&mut sender, 87, "digest-receiver");
        let mut buffer = registered_buffer(&mut sender);
        let mut rng = CounterRng::default();
        let job = prepare(
            &mut sender,
            &mut buffer,
            receiver.destination_hash(),
            b"authoritative packet digest",
            1,
            &mut rng,
        );
        let authoritative = job.prepared();
        let BufferBinding::Bound {
            dispatch_generation,
            prepared,
            hop,
        } = job.owner.buffer.binding
        else {
            panic!("prepared job did not retain a bound buffer")
        };
        let tampered = EncodedPacketSha256::new([0xa5; 32]);
        assert_ne!(tampered, authoritative.encoded_packet_sha256());
        job.owner.buffer.binding = BufferBinding::Bound {
            dispatch_generation,
            prepared: PreparedPacket {
                encoded_packet_sha256: tampered,
                ..prepared
            },
            hop,
        };

        let quarantine = match sender
            .complete_tx(
                job.return_unpermitted().complete(TxCompletionCode::new(42)),
                owner_time(1_100),
            )
            .unwrap_or_else(|failure| panic!("digest return failed: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            TxCompletionDisposition::Available(_) => panic!("digest mismatch released buffer"),
            TxCompletionDisposition::Recovered { .. } => panic!("digest mismatch became recovery"),
            TxCompletionDisposition::Next(_) => panic!("digest mismatch continued fanout"),
        };
        assert_eq!(quarantine.record().reason(), TxRecoveryReason::Invariant);
        assert_eq!(
            quarantine.owner.bound().0.encoded_packet_sha256(),
            authoritative.encoded_packet_sha256()
        );
    }

    #[test]
    fn recovery_fault_quarantines_and_wrong_owner_or_stale_returns_retain_ownership() {
        let receiver = node::<2, 1>(68, "receiver");
        let mut rng = CounterRng::default();

        let mut faulted = node::<2, 1>(67, "faulted");
        register_receiver(&mut faulted, 68, "receiver");
        let mut fault_buffer = registered_buffer(&mut faulted);
        let fault_job = prepare(
            &mut faulted,
            &mut fault_buffer,
            receiver.destination_hash(),
            b"control fault",
            1,
            &mut rng,
        );
        let fault_code = TxCompletionCode::new(0x1234);
        let quarantine = match faulted
            .complete_tx(fault_job.recovery_fault(fault_code), owner_time(1_100))
            .unwrap_or_else(|failure| panic!("fault return rejected: {:?}", failure.reason()))
        {
            TxCompletionDisposition::Quarantined(quarantine) => quarantine,
            TxCompletionDisposition::Next(_) => panic!("fault continued fanout"),
            TxCompletionDisposition::Available(_) => panic!("fault released buffer"),
            TxCompletionDisposition::Recovered { .. } => panic!("fault was treated as lateness"),
        };
        assert_eq!(
            quarantine.record().reason(),
            TxRecoveryReason::CompletionFault(fault_code)
        );
        assert_eq!(
            quarantine.record().prior_phase(),
            TxRecoveryPriorPhase::Unpermitted
        );
        assert_eq!(faulted.capacities().dispatches_recovery_required, 1);

        let mut owner = node::<2, 1>(69, "owner");
        let mut other = node::<2, 1>(70, "other");
        register_receiver(&mut owner, 68, "receiver");
        register_receiver(&mut other, 68, "receiver");
        let mut buffer = registered_buffer(&mut owner);
        let job = prepare(
            &mut owner,
            &mut buffer,
            receiver.destination_hash(),
            b"retained return",
            2,
            &mut rng,
        );
        let (pending, request) = job.begin_permit(test_permit_requirements());
        let request_failure =
            match other.authorize_tx(request, owner_time(2_100), &mut TestPolicy::allowing()) {
                Ok(_) => panic!("foreign owner authorized request"),
                Err(failure) => failure,
            };
        assert_eq!(
            request_failure.reason(),
            TxAuthorizationErrorKind::WrongOwner
        );
        let reply = owner
            .authorize_tx(
                request_failure.into_request(),
                owner_time(2_100),
                &mut TestPolicy::denying(TxPolicyDenial::PolicyDenied),
            )
            .unwrap_or_else(|failure| panic!("retained request failed: {:?}", failure.reason()));
        let unpermitted = match pending.resolve(reply, owner_time(2_100)) {
            Ok(PermitResolution::Unpermitted(unpermitted)) => unpermitted,
            Ok(PermitResolution::Authorized(_)) => panic!("denial authorized"),
            Ok(PermitResolution::Expired(_)) => panic!("denial became expired grant"),
            Err(_) => panic!("retained request reply mismatch"),
        };
        let completion = unpermitted.complete(TxCompletionCode::new(30));
        let wrong_owner = match other.complete_tx(completion, owner_time(2_200)) {
            Ok(_) => panic!("foreign owner accepted completion"),
            Err(failure) => failure,
        };
        assert_eq!(wrong_owner.reason(), TxCompletionErrorKind::WrongOwner);
        let completion = wrong_owner.into_completion();

        let original_state = owner.dispatches[0].state;
        let DispatchState::Active(mut stale_record) = original_state else {
            panic!("expected active dispatch")
        };
        stale_record.generation += 1;
        owner.dispatches[0].state = DispatchState::Active(stale_record);
        let stale = match owner.complete_tx(completion, owner_time(2_200)) {
            Ok(_) => panic!("stale completion was accepted"),
            Err(failure) => failure,
        };
        assert_eq!(stale.reason(), TxCompletionErrorKind::StaleOrUnknown);
        owner.dispatches[0].state = original_state;
        assert!(matches!(
            owner
                .complete_tx(stale.into_completion(), owner_time(2_200))
                .unwrap_or_else(|failure| panic!("retained completion: {:?}", failure.reason())),
            TxCompletionDisposition::Available(_)
        ));
        assert_eq!(owner.capacities().dispatches_used, 0);
    }
}
