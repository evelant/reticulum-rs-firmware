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
#![deny(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use core::{mem::MaybeUninit, num::NonZeroU64, ptr::addr_of_mut};

use rand_core::{CryptoRng, RngCore};
pub(crate) use reticulum_rns_rete::{
    AnnounceAdmissionError as RnsAnnounceAdmissionError,
    AnnounceAppDataError as RnsAnnounceAppDataError, DestHash as RnsDestinationHash,
    DestinationRegistrationError as RnsDestinationRegistrationError,
    DestinationType as RnsDestinationType, Direction as RnsDirection, EmbeddedNode as RnsNode,
    EmbeddedNodeConfig as RnsNodeConfig, Identity as RnsIdentity,
    InboundProofPolicy as RnsInboundProofPolicy,
    InboundProofPolicyError as RnsInboundProofPolicyError,
    IngressObservation as RnsIngressObservation, IngressOrigin as RnsIngressOrigin,
    IngressSignalObservation as RnsIngressSignalObservation, InterfaceId as RnsInterfaceId,
    LinkAdmissionError as RnsLinkAdmissionError, LinkId as RnsLinkId, LinkState as RnsLinkState,
    LocalAnnounceTargetError as RnsLocalAnnounceTargetError, NodeRole as RnsNodeRole,
    PathRequestBuildError as RnsPathRequestBuildError,
    PrepareBasicLxmfError as RnsPrepareBasicLxmfError, PrepareDataError as RnsPrepareDataError,
    PrepareDirectLxmfLinkDataError as RnsPrepareDirectLxmfLinkDataError,
    PrepareDirectRequestError as RnsPrepareDirectRequestError,
    PrepareOpportunisticLxmfDataError as RnsPrepareOpportunisticLxmfDataError,
    PrepareResponseError as RnsPrepareResponseError,
    PreparedBasicDirectLxmf as RnsPreparedBasicDirectLxmf,
    PreparedBasicLxmf as RnsPreparedBasicLxmf, PreparedData as RnsPreparedData,
    PreparedLinkData as RnsPreparedLinkData,
    ProofProbeIdentityAliasError as RnsProofProbeIdentityAliasError, RNS_MTU, ReceiptCandidate,
    ReceiptKind, ReceiptReservationUnavailable, ReceiptTerminal, ReceiptTerminalReservation,
    ReceiptTerminalSink, RequestDispatchConfirmation as RnsRequestDispatchConfirmation,
    RequestDispatchError as RnsRequestDispatchError,
    RequestDispatchReconciliation as RnsRequestDispatchReconciliation,
    RequestHandle as RnsRequestHandle, RouteSnapshot as RnsRouteSnapshot, TxTarget as RnsTxTarget,
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
    EmbeddedNodeMetrics, InboundData, InboundDataProjection, InboundProofEvidence,
    IngressBroadcastPolicy, IngressBroadcastScope, IngressDisposition, IngressDropReason,
    IngressEgressSet, IngressMetadata, IngressReport, LxmfMessageLocation, MonotonicInstant,
    NodeActions, OutboundDispatchInterval, OutboundProtocolToken, PacketType,
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
    OrdinaryCompletionError, OrdinaryCompletionFailure, OrdinaryDispatchToken,
    OrdinaryExpiredAuthorizedTx, OrdinaryFrameError, OrdinaryPacketBuffer,
    OrdinaryPacketGeneration, OrdinaryPacketReturn, OrdinaryPacketReturnParkFailure,
    OrdinaryPacketReturnReason, OrdinaryPacketSlotId, OrdinaryPermitCancelMismatch,
    OrdinaryPermitPendingTx, OrdinaryPermitReplyMismatch, OrdinaryPermitResolution,
    OrdinaryPreparedPacket, OrdinaryQuarantineReason, OrdinaryRegisterAndParkError,
    OrdinaryRegisterAndParkFailure, OrdinaryTransmissionOutcome, OrdinaryTxCompletion,
    OrdinaryTxFrame, OrdinaryTxJob, OrdinaryTxPermitDenial, OrdinaryTxPermitDenialReason,
    OrdinaryTxPermitGrant, OrdinaryTxPermitReply, OrdinaryTxPermitRequest, OrdinaryTxQuarantine,
    OrdinaryUnpermittedTx,
};

mod proof_probe;
pub use proof_probe::{
    ActiveProofProbe, ProofProbeCorrelation, ProofProbeEvictionError, ProofProbeFirstDispatch,
    ProofProbeObservation, ProofProbeStartError, TerminalProofProbe, VolatileProofProbe,
};

mod wrappers;
pub use wrappers::*;

/// Complete packet storage reserved for one base-MTU Reticulum transmission.
pub const PACKET_CAPACITY: usize = RNS_MTU;

/// Largest destination-DATA plaintext accepted by the current RNS adapter.
pub const MAX_DATA_PAYLOAD: usize = reticulum_rns_rete::MAX_DATA_PAYLOAD;

/// Largest opportunistic LXMF carrier emitted by the basic composer.
pub const MAX_OPPORTUNISTIC_LXMF_CARRIER: usize =
    reticulum_rns_rete::MAX_OPPORTUNISTIC_LXMF_CARRIER;

/// Largest complete basic LXMF message carried by one direct Link packet.
pub const MAX_DIRECT_LXMF_WIRE: usize = reticulum_rns_rete::MAX_DIRECT_LXMF_WIRE;

/// Largest Python LXMF content size carried by one direct Link packet.
pub const MAX_DIRECT_LXMF_CONTENT_SIZE: usize = reticulum_rns_rete::MAX_DIRECT_LXMF_CONTENT_SIZE;

/// Largest positive whole-millisecond timestamp accepted by basic LXMF composition.
pub const MAX_LXMF_TIMESTAMP_UNIX_MS: u64 = reticulum_rns_rete::MAX_LXMF_TIMESTAMP_UNIX_MS;

/// Largest application-data payload accepted for one local announce.
pub const MAX_ANNOUNCE_APP_DATA: usize = reticulum_rns_rete::MAX_ANNOUNCE_APP_DATA;

/// Native RNS delay before the one scheduled retransmission of an announce.
pub const RNS_ANNOUNCE_RETRANSMIT_SECONDS: u64 = reticulum_rns_rete::ANNOUNCE_RETRANSMIT_SECONDS;

/// Link DATA context that carries ordinary application bytes rather than an
/// RNS control or service message.
pub const APPLICATION_LINK_CONTEXT_NONE: u8 = reticulum_rns_rete::LINK_DATA_CONTEXT_NONE;

/// Canonical Reticulum proof-probe application name.
pub const PROOF_PROBE_APPLICATION_NAME: &str =
    reticulum_rns_rete::RNSTRANSPORT_PROBE_APPLICATION_NAME;

/// Canonical Reticulum proof-probe destination aspect.
pub const PROOF_PROBE_ASPECT: &str = reticulum_rns_rete::RNSTRANSPORT_PROBE_ASPECT;

/// Canonical expanded Reticulum proof-probe destination name.
pub const PROOF_PROBE_EXPANDED_NAME: &str = reticulum_rns_rete::RNSTRANSPORT_PROBE_EXPANDED_NAME;

/// Compute Reticulum's full hop-invariant hash for one encoded RNS packet.
///
/// The hash excludes mutable hop-count and transport-header fields, so a DATA
/// packet received after forwarding retains the same bytes used by the
/// originating attempt's delivery-proof correlation token. Invalid encoded
/// packets return `None` instead of fabricating correlation evidence.
pub fn rns_packet_hash(raw: &[u8]) -> Option<[u8; 32]> {
    reticulum_rns_rete::parse_packet(raw)
        .ok()
        .map(|packet| packet.compute_hash())
}

/// Parse one complete RNS packet and return its wire packet type.
///
/// Firmware diagnostics use this narrow projection to classify DATA receive
/// stages without depending on the protocol adapter's concrete parser type.
pub fn rns_packet_type(raw: &[u8]) -> Option<PacketType> {
    reticulum_rns_rete::parse_packet(raw)
        .ok()
        .map(|packet| packet.packet_type)
}

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

#[derive(Clone, Copy)]
enum PreparedReceiptPacket {
    DestinationData(RnsPreparedData),
    LinkData(RnsPreparedLinkData),
}

impl PreparedReceiptPacket {
    fn packet_len(self) -> u16 {
        match self {
            Self::DestinationData(prepared) => prepared.packet_len(),
            Self::LinkData(prepared) => prepared.packet_len(),
        }
    }

    fn target(self) -> RnsTxTarget {
        match self {
            Self::DestinationData(prepared) => prepared.target(),
            Self::LinkData(prepared) => prepared.target(),
        }
    }

    fn receipt(self) -> reticulum_rns_rete::ReceiptId {
        match self {
            Self::DestinationData(prepared) => prepared.receipt(),
            Self::LinkData(prepared) => prepared.receipt(),
        }
    }

    const fn receipt_kind(self) -> DataReceiptKind {
        match self {
            Self::DestinationData(_) => DataReceiptKind::DestinationData,
            Self::LinkData(_) => DataReceiptKind::LinkData,
        }
    }
}

/// Scalar metadata for one packet committed to an external buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedPacket {
    packet_len: u16,
    encoded_packet_sha256: EncodedPacketSha256,
    attempt: AttemptToken,
    receipt_kind: DataReceiptKind,
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

    /// Receipt table that owns this packet's proof correlation.
    pub const fn receipt_kind(self) -> DataReceiptKind {
        self.receipt_kind
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
        prepared: PreparedReceiptPacket,
        encoded_packet_sha256: EncodedPacketSha256,
        handle: AttemptHandle,
        slot: PacketSlotId,
        deadline: TxLeaseDeadline,
    ) -> Self {
        Self {
            packet_len: prepared.packet_len(),
            encoded_packet_sha256,
            attempt: AttemptToken(*prepared.receipt().as_bytes()),
            receipt_kind: prepared.receipt_kind(),
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
    /// The selected Link is no longer retained by the protocol owner.
    LinkNotFound,
    /// The selected Link has not completed establishment.
    LinkNotActive,
    /// The selected Link was established to another destination.
    LinkDestinationMismatch,
    /// The authenticated Link has no retained interface binding.
    LinkInterfaceUnknown,
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

impl From<RnsPrepareOpportunisticLxmfDataError> for SubmitError {
    fn from(error: RnsPrepareOpportunisticLxmfDataError) -> Self {
        match error {
            RnsPrepareOpportunisticLxmfDataError::Data(error) => error.into(),
            RnsPrepareOpportunisticLxmfDataError::Header2PayloadTooLarge { actual, maximum } => {
                Self::PayloadTooLarge { actual, maximum }
            }
            RnsPrepareOpportunisticLxmfDataError::InvalidCompleteWire
            | RnsPrepareOpportunisticLxmfDataError::CarrierLengthMismatch { .. }
            | RnsPrepareOpportunisticLxmfDataError::CarrierDigestMismatch
            | RnsPrepareOpportunisticLxmfDataError::CarrierSourceMismatch => Self::Invariant,
        }
    }
}

impl From<RnsPrepareDirectLxmfLinkDataError> for SubmitError {
    fn from(error: RnsPrepareDirectLxmfLinkDataError) -> Self {
        match error {
            RnsPrepareDirectLxmfLinkDataError::LinkMduExceeded { actual, maximum } => {
                Self::PayloadTooLarge { actual, maximum }
            }
            RnsPrepareDirectLxmfLinkDataError::LinkNotFound => Self::LinkNotFound,
            RnsPrepareDirectLxmfLinkDataError::LinkNotActive => Self::LinkNotActive,
            RnsPrepareDirectLxmfLinkDataError::LinkDestinationMismatch => {
                Self::LinkDestinationMismatch
            }
            RnsPrepareDirectLxmfLinkDataError::LinkInterfaceUnknown => Self::LinkInterfaceUnknown,
            RnsPrepareDirectLxmfLinkDataError::ReceiptTableFull { limit } => {
                Self::ReceiptTableFull { limit }
            }
            RnsPrepareDirectLxmfLinkDataError::ReceiptHashAlreadyTracked => {
                Self::ReceiptHashAlreadyTracked
            }
            RnsPrepareDirectLxmfLinkDataError::Crypto => Self::Cryptography,
            RnsPrepareDirectLxmfLinkDataError::PacketBuild => Self::PacketBuild,
            RnsPrepareDirectLxmfLinkDataError::InvalidCompleteWire
            | RnsPrepareDirectLxmfLinkDataError::WireLengthMismatch { .. }
            | RnsPrepareDirectLxmfLinkDataError::WireDigestMismatch
            | RnsPrepareDirectLxmfLinkDataError::WireDestinationMismatch
            | RnsPrepareDirectLxmfLinkDataError::WireSourceMismatch
            | RnsPrepareDirectLxmfLinkDataError::Invariant => Self::Invariant,
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

    /// Complete this authorized hop as possibly transmitted without a
    /// definitive physical `TxDone` observation.
    pub fn complete(self, code: TxCompletionCode) -> TxCompletion<'a> {
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::Authorized(code),
        }
    }

    /// Complete this authorized hop after the concrete interface observed
    /// physical `TxDone` for every frame of the logical packet.
    ///
    /// This distinct edge lets the node owner start a destination-DATA proof
    /// window after interface queueing and channel access have completed. The
    /// exact packet and attempt evidence remain carried by the unique owner.
    pub fn complete_transmitted(
        self,
        code: TxCompletionCode,
        transmitted_at: MonotonicMillis,
    ) -> TxCompletion<'a> {
        TxCompletion {
            owner: self.owner,
            kind: CompletionKind::Transmitted {
                code,
                transmitted_at,
            },
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
    Transmitted {
        code: TxCompletionCode,
        transmitted_at: MonotonicMillis,
    },
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
    /// RNS presented a channel receipt, which this DATA-attempt owner cannot
    /// represent.
    UnsupportedChannel,
    /// A tracked receipt hash arrived from a different native receipt table.
    WrongKind {
        /// Complete proof-correlation hash RNS presented.
        attempt: AttemptToken,
        /// Receipt table retained by the fixed attempt ledger.
        expected: DataReceiptKind,
        /// Receipt table presented by RNS.
        actual: DataReceiptKind,
    },
    /// RNS presented a DATA or Link-DATA receipt absent from the fixed ledger.
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
    /// Number of destination-DATA and Link-DATA timeouts committed to terminal
    /// tombstones in this pass.
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
    /// Outstanding Link-DATA receipts retained by RNS.
    pub link_data_receipts_used: usize,
    /// Configured Link-DATA receipt limit.
    pub link_data_receipts_limit: usize,
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
    prepared: PreparedReceiptPacket,
    encoded_packet_sha256: EncodedPacketSha256,
    attempt: AttemptRef,
    generation: u64,
    deadline: TxLeaseDeadline,
    receipt_registered_at: MonotonicSeconds,
    receipt_registered_owner_at: MonotonicMillis,
    selected: Option<HopBinding>,
    remaining: InterfaceSet,
    may_have_transmitted: bool,
    phase: DispatchPhase,
}

#[derive(Clone, Copy)]
#[allow(
    clippy::large_enum_variant,
    reason = "the fixed no-alloc dispatch ledger retains each unique scalar record in place; boxing or another indirect owner would violate the embedded ownership model"
)]
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
        kind: DataReceiptKind,
        link: Option<LinkHandle>,
    },
    Terminal {
        generation: u64,
        token: AttemptToken,
        kind: DataReceiptKind,
        link: Option<LinkHandle>,
        outcome: AttemptOutcome,
        ingress: Option<PacketIngressObservation>,
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
                kind,
                link,
                outcome,
                ingress,
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
                    kind,
                    link,
                    outcome,
                    ingress,
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
    ///
    /// Keep this one-shot constructor as an explicit frame boundary. Embedded
    /// products audit its emitted stack independently from their async
    /// composition task; inlining it would hide that cumulative startup path.
    #[inline(never)]
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
                shared_medium_interfaces: config.shared_medium_interfaces,
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

    /// Construct the bounded node owner directly in caller-provided storage.
    ///
    /// The nested RNS owner and both capacity-sized metadata arrays are
    /// initialized at their final addresses. This avoids complete
    /// `NodeCore`, dispatch-table, and attempt-ledger temporaries on the boot
    /// stack. On error, `destination` remains uninitialized and may be reused
    /// for another construction attempt.
    #[allow(
        unsafe_code,
        reason = "audited field projections initialize one MaybeUninit NodeCore in place"
    )]
    #[inline(never)]
    pub fn new_in<'a>(
        destination: &'a mut MaybeUninit<Self>,
        identity: NodeIdentity,
        app_name: &str,
        aspects: &[&str],
        instance: NodeInstanceId,
        config: NodeConfig,
    ) -> Result<&'a mut Self, NodeConstructionError> {
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
        let destination_ptr = destination.as_mut_ptr();

        // SAFETY: `destination_ptr` is derived from the sole mutable borrow of
        // the complete uninitialized destination. `addr_of_mut!` projects a
        // raw field pointer without creating a reference to uninitialized
        // `RnsNode` bytes, and `MaybeUninit<T>` preserves `T`'s layout. No
        // other field is touched before recursive construction succeeds.
        let rns_destination = unsafe {
            &mut *addr_of_mut!((*destination_ptr).rns)
                .cast::<MaybeUninit<RnsNode<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>>>()
        };
        let owner_identity = {
            let rns = RnsNode::new_in(
                rns_destination,
                identity.0,
                app_name,
                aspects,
                RnsNodeConfig {
                    role,
                    max_additional_destinations: config.max_additional_destinations,
                    shared_medium_interfaces: config.shared_medium_interfaces,
                },
            )
            .map_err(|_| NodeConstructionError::InvalidRnsConfiguration)?;
            *rns.identity_hash().as_bytes()
        };

        // Recursive construction is the sole fallible operation. Initialize
        // the capacity arrays element-wise at their final addresses to avoid
        // materializing `[EMPTY; N]` values on the boot stack.
        // SAFETY: `rns` is initialized and every other field is written once
        // through the exclusive destination pointer before the complete value
        // is exposed by `assume_init_mut`.
        unsafe {
            addr_of_mut!((*destination_ptr).owner_identity).write(owner_identity);
            addr_of_mut!((*destination_ptr).instance).write(instance);

            let dispatches = addr_of_mut!((*destination_ptr).dispatches).cast::<DispatchSlot>();
            for index in 0..PACKET_BUFFERS {
                dispatches.add(index).write(DispatchSlot::EMPTY);
            }

            let attempts = addr_of_mut!((*destination_ptr).attempts).cast::<AttemptSlot>();
            for index in 0..PATHS {
                attempts.add(index).write(AttemptSlot::EMPTY);
            }

            addr_of_mut!((*destination_ptr).next_registration).write(0);
            addr_of_mut!((*destination_ptr).next_attempt).write(0);
            addr_of_mut!((*destination_ptr).dispatch_generation).write(0);
            addr_of_mut!((*destination_ptr).hop_generation).write(0);
            addr_of_mut!((*destination_ptr).attempt_generation).write(0);
            addr_of_mut!((*destination_ptr).ordinary_action_owner_claimed).write(false);

            Ok(destination.assume_init_mut())
        }
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

    /// Register the canonical inbound `rnstransport.probe` responder.
    ///
    /// This additional Single destination rejects Link admission and uses
    /// Reticulum's `PROVE_ALL` behavior, making it compatible with the Python
    /// `rnprobe` utility and other standard proof-probe clients.
    pub fn register_proof_probe_destination(
        &mut self,
    ) -> Result<DestinationHash, LocalDestinationRegistrationError> {
        self.rns
            .register_probe_destination()
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

    /// Compose and sign one opportunistic LXMF carrier with an authenticated
    /// Sideband-compatible location field.
    pub fn prepare_basic_lxmf_with_location_into(
        &self,
        destination: &DestinationHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        location: LxmfMessageLocation,
        output: &mut [u8],
    ) -> Result<PreparedBasicLxmf, PrepareBasicLxmfError> {
        self.rns
            .prepare_basic_lxmf_with_location_into(
                &RnsDestinationHash::from(*destination.as_bytes()),
                timestamp_unix_ms,
                title,
                content,
                location,
                output,
            )
            .map(PreparedBasicLxmf)
            .map_err(PrepareBasicLxmfError::from)
    }

    /// Compose and sign one complete basic direct-LXMF message with this node's
    /// registered inbound Single `lxmf.delivery` source.
    ///
    /// This read-only operation prepares no Link or packet and consumes no
    /// receipt state. Messages above the direct packet boundary are rejected
    /// until the Resource path is separately enabled.
    pub fn prepare_basic_direct_lxmf_into(
        &self,
        destination: &DestinationHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        output: &mut [u8],
    ) -> Result<PreparedBasicDirectLxmf, PrepareBasicLxmfError> {
        self.rns
            .prepare_basic_direct_lxmf_into(
                &RnsDestinationHash::from(*destination.as_bytes()),
                timestamp_unix_ms,
                title,
                content,
                output,
            )
            .map(PreparedBasicDirectLxmf)
            .map_err(PrepareBasicLxmfError::from)
    }

    /// Compose and sign one complete direct-LXMF message with an authenticated
    /// Sideband-compatible location field.
    pub fn prepare_basic_direct_lxmf_with_location_into(
        &self,
        destination: &DestinationHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        location: LxmfMessageLocation,
        output: &mut [u8],
    ) -> Result<PreparedBasicDirectLxmf, PrepareBasicLxmfError> {
        self.rns
            .prepare_basic_direct_lxmf_with_location_into(
                &RnsDestinationHash::from(*destination.as_bytes()),
                timestamp_unix_ms,
                title,
                content,
                location,
                output,
            )
            .map(PreparedBasicDirectLxmf)
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

    /// Derive the canonical remote `rnstransport.probe` destination from any
    /// announce-known destination owned by that retained identity.
    ///
    /// This read-only helper centralizes Reticulum identity and destination
    /// hashing in the protocol owner. It returns `None` when no authenticated
    /// identity is retained for the supplied destination.
    pub fn proof_probe_destination_for(
        &self,
        announced_destination: &DestinationHash,
    ) -> Option<DestinationHash> {
        self.rns
            .proof_probe_destination_for(&announced_destination.into_rns())
            .map(|destination| DestinationHash::new(*destination.as_bytes()))
    }

    /// Alias an authenticated retained peer identity under its canonical
    /// `rnstransport.probe` destination without creating a path.
    ///
    /// This enables destination-DATA encryption for the derived probe hash
    /// while preserving Reticulum's need to discover or broadcast a real
    /// route independently.
    pub fn prepare_proof_probe_destination_for(
        &mut self,
        announced_destination: &DestinationHash,
    ) -> Result<DestinationHash, ProofProbeIdentityAliasError> {
        self.rns
            .prepare_proof_probe_destination_for(&announced_destination.into_rns())
            .map(|destination| DestinationHash::new(*destination.as_bytes()))
            .map_err(ProofProbeIdentityAliasError::from)
    }

    /// Recall an announce-learned identity only when it derives the supplied
    /// exact `lxmf.delivery` destination.
    ///
    /// This preserves Reticulum's destination construction inside the protocol
    /// adapter instead of duplicating it in application discovery code.
    pub fn recall_lxmf_delivery_identity(&self, destination: &DestinationHash) -> Option<[u8; 64]> {
        let destination = RnsDestinationHash::from(*destination.as_bytes());
        self.rns.recall_lxmf_delivery_identity(&destination)
    }

    /// Whether an exact destination currently has a retained RNS path.
    pub fn has_path(&self, destination: &DestinationHash) -> bool {
        self.rns.has_path(&destination.into_rns())
    }

    /// Remove one exact retained RNS path.
    ///
    /// Product delivery policy calls this after a non-Link receipt timeout so
    /// a later attempt cannot reuse the route that just failed. Identity and
    /// Link state are independent and remain retained.
    pub fn remove_retained_path(&mut self, destination: &DestinationHash) -> bool {
        self.rns.remove_path(&destination.into_rns())
    }

    /// Visit copy-only snapshots of all routes currently retained by RNS.
    ///
    /// Native map iteration order is unspecified. Callers presenting a full
    /// table must copy and sort by [`RetainedRouteSnapshot::destination`].
    /// The returned count cannot exceed this node's `PATHS` const generic.
    pub fn visit_retained_routes(&self, mut visitor: impl FnMut(RetainedRouteSnapshot)) -> usize {
        self.rns
            .visit_routes(|route| visitor(RetainedRouteSnapshot::from_rns(route)))
    }

    /// Number of routes currently retained by the native RNS path table.
    pub fn retained_route_count(&self) -> usize {
        self.rns.route_count()
    }

    /// Native routing target retained with one known destination path.
    ///
    /// `None` means no path exists. A retained path without an exact ingress
    /// interface uses [`TxTarget::All`], preserving Reticulum's direct
    /// broadcast fallback while leaving current interface eligibility to the
    /// permanent supervisor.
    pub fn retained_path_target(&self, destination: &DestinationHash) -> Option<TxTarget> {
        self.rns
            .route(&destination.into_rns())
            .map(|route| match route.received_on {
                Some(interface) => TxTarget::Only(PacketInterfaceId::new(interface.0)),
                None => TxTarget::All,
            })
    }

    /// Hop count retained with one known destination path.
    pub fn retained_path_hops(&self, destination: &DestinationHash) -> Option<u8> {
        self.rns
            .route(&destination.into_rns())
            .map(|route| route.hops)
    }

    /// Next-hop transport identity retained for one known destination path.
    ///
    /// Direct routes and missing routes both return `None`; callers that need
    /// to distinguish those cases must pair this with [`Self::has_path`].
    pub fn retained_path_next_hop(&self, destination: &DestinationHash) -> Option<[u8; 16]> {
        self.rns
            .route(&destination.into_rns())
            .and_then(|route| route.via.map(|identity| *identity.as_bytes()))
    }

    /// Whether the shared native owned-Link table can currently retain one
    /// additional session.
    ///
    /// This includes locally initiated and inbound responder Links because
    /// both occupy the same bounded Rete table. Link construction revalidates
    /// capacity under the sole mutable owner, so this is only a scheduling
    /// preflight.
    pub fn can_initiate_link(&self) -> bool {
        self.rns.can_initiate_link()
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

    /// Construct and retain one outbound Link request as an ordinary action.
    ///
    /// The returned handle identifies the exact retained Link. Its packet
    /// remains subject to the ordinary-action routing and protocol-token
    /// confirmation path before establishment can complete.
    pub fn initiate_link<R: RngCore + CryptoRng>(
        &mut self,
        destination: &DestinationHash,
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> Result<(NodeActions, LinkHandle), InitiateLinkError> {
        let (packet, link_id) = self
            .rns
            .initiate_link(destination.into_rns(), now.get(), rng)
            .map_err(InitiateLinkError::from)?;
        let mut packets = alloc::vec::Vec::new();
        if packets.try_reserve_exact(1).is_err() {
            return Err(if self.rns.abort_unestablished_link(&link_id) {
                InitiateLinkError::ActionAllocationFailed
            } else {
                InitiateLinkError::RollbackFailed
            });
        }
        packets.push(packet);
        Ok((
            NodeActions::without_retained_proofs(Default::default(), packets, 0),
            LinkHandle::from_rns(link_id),
        ))
    }

    /// Prepare one canonical anonymous NomadNet request as an ordinary action.
    ///
    /// The wire timestamp is encoded unchanged but starts no response timeout.
    /// The native move-only preparation authority remains inside this node;
    /// only the returned copyable handle may cross task or queue boundaries.
    /// Outer action storage is reserved before native preparation, so an
    /// allocation failure returns without mutating RNS request state.
    pub fn prepare_anonymous_request<R: RngCore + CryptoRng>(
        &mut self,
        link: LinkHandle,
        path: &str,
        requested_at_secs: f64,
        rng: &mut R,
    ) -> Result<PreparedRequestActions, PrepareRequestError> {
        let mut packets = alloc::vec::Vec::new();
        packets
            .try_reserve_exact(1)
            .map_err(|_| PrepareRequestError::ActionAllocationFailed)?;
        let prepared = self
            .rns
            .prepare_anonymous_request(&link.into_rns(), path, requested_at_secs, rng)
            .map_err(PrepareRequestError::from)?;
        let handle = RequestHandle::from_rns(prepared.handle());
        let (packet, exact_handle) = prepared.into_parts();
        debug_assert_eq!(exact_handle, handle.into_rns());
        packets.push(packet);
        Ok(PreparedRequestActions {
            actions: NodeActions::without_retained_proofs(Default::default(), packets, 0),
            handle,
        })
    }

    /// Prepare one response for an exact inbound request binding.
    ///
    /// The opaque binding is accepted only from a projected application event;
    /// the adapter revalidates its Link, destination, exact role, lifecycle,
    /// interface and negotiated MDU before encryption. Outer action storage is
    /// reserved first, so allocation failure cannot consume entropy or mutate
    /// retained Link timing. The returned envelope owns exactly one ordinary
    /// packet and no receipt, protocol-token or cancellation authority.
    pub fn prepare_response_actions<R: RngCore + CryptoRng>(
        &mut self,
        binding: &ApplicationLinkBinding,
        request: &[u8; 16],
        data: &[u8],
        now: MonotonicSeconds,
        rng: &mut R,
    ) -> Result<NodeActions, PrepareResponseError> {
        let mut packets = alloc::vec::Vec::new();
        packets
            .try_reserve_exact(1)
            .map_err(|_| PrepareResponseError::ActionAllocationFailed)?;
        let packet = self
            .rns
            .prepare_response(binding, request, data, now.get(), rng)
            .map_err(PrepareResponseError::from)?;
        packets.push(packet);
        Ok(NodeActions::without_retained_proofs(
            Default::default(),
            packets,
            0,
        ))
    }

    /// Apply one router-observed first-dispatch decision to an exact request.
    ///
    /// `first_dispatch == false` is a no-op. A true decision starts native
    /// response-timeout tracking at the supplied monotonic whole-second
    /// sample. The caller should perform this transition only after the
    /// ordinary router reports successful interface acceptance.
    pub fn confirm_request_dispatch(
        &mut self,
        handle: RequestHandle,
        sent_at: MonotonicSeconds,
        first_dispatch: bool,
    ) -> Result<RequestDispatchConfirmation, RequestDispatchError> {
        self.rns
            .confirm_request_dispatch(handle.into_rns(), sent_at.get(), first_dispatch)
            .map(RequestDispatchConfirmation::from)
            .map_err(RequestDispatchError::from)
    }

    /// Reclaim one exact request from every native dispatch owner.
    ///
    /// This phase-agnostic operation is allocation-free and idempotent. It is
    /// intended for invariant recovery after router evidence or native state
    /// disagrees with the product coordinator, not normal cancellation.
    pub fn reconcile_request_dispatch(
        &mut self,
        handle: RequestHandle,
    ) -> RequestDispatchReconciliation {
        self.rns
            .reconcile_request_dispatch(handle.into_rns())
            .into()
    }

    /// Cancel an exact request only while it still awaits first dispatch.
    pub fn cancel_prepared_request(
        &mut self,
        handle: RequestHandle,
    ) -> Result<(), RequestDispatchError> {
        self.rns
            .cancel_prepared_request(handle.into_rns())
            .map_err(RequestDispatchError::from)
    }

    /// Cancel an exact request only after first dispatch started its timeout.
    pub fn cancel_confirmed_request(
        &mut self,
        handle: RequestHandle,
    ) -> Result<(), RequestDispatchError> {
        self.rns
            .cancel_confirmed_request(handle.into_rns())
            .map_err(RequestDispatchError::from)
    }

    /// Read the current lifecycle state of one retained Link.
    ///
    /// `None` means the handle is unknown to this in-memory protocol owner or
    /// its Link has already been purged.
    pub fn link_state(&self, link: LinkHandle) -> Option<LinkState> {
        self.rns.link_state(&link.into_rns()).map(LinkState::from)
    }

    /// Authenticated packet interface bound to one retained Link.
    ///
    /// This does not imply that the interface is currently online. The
    /// permanent interface supervisor combines this scalar with its
    /// authoritative eligible-interface snapshot.
    pub fn link_interface(&self, link: LinkHandle) -> Option<PacketInterfaceId> {
        self.rns
            .link_interface(&link.into_rns())
            .map(|interface| PacketInterfaceId::new(interface.0))
    }

    /// Remove a retained Link that is neither active nor stale without
    /// emitting network traffic.
    ///
    /// This is intended for pending or handshaking outbound requests. Active
    /// and stale Links are deliberately rejected; their authenticated teardown
    /// must use the normal Link-close lifecycle.
    pub fn abort_unestablished_link(&mut self, link: LinkHandle) -> bool {
        self.rns.abort_unestablished_link(&link.into_rns())
    }

    /// Close one retained active or stale Link through the authenticated
    /// Reticulum teardown lifecycle.
    ///
    /// The Link is removed locally before the returned `LINKCLOSE` action is
    /// dispatched. An unknown or already-removed handle returns an empty
    /// action envelope. Callers must retain and route every non-empty action
    /// envelope through the ordinary protocol owner.
    pub fn close_link<R: RngCore + CryptoRng>(
        &mut self,
        link: LinkHandle,
        rng: &mut R,
    ) -> NodeActions {
        self.rns.close_link(&link.into_rns(), rng)
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

    /// Bind one local announce destination and its native retransmits to an exact interface.
    pub fn set_local_announce_interface_target(
        &mut self,
        destination: &DestinationHash,
        interface: PacketInterfaceId,
    ) -> Result<(), LocalDestinationAnnounceTargetError> {
        self.rns
            .set_local_announce_interface_target(
                &RnsDestinationHash::from(*destination.as_bytes()),
                RnsInterfaceId(interface.get()),
            )
            .map_err(LocalDestinationAnnounceTargetError::from)
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
        self.prepare_data_into_slot_with(buffer, request, DataPreparationMode::Generic, rng)
    }

    /// Prepare one exact durable, locally signed LXMF wire message through the
    /// dedicated opportunistic destination-DATA path.
    ///
    /// `request.plaintext` is the complete LXMF wire message including its
    /// destination prefix. The native adapter revalidates that prefix, local
    /// source, and signature before applying the Header-1 LXMF size exception.
    /// All packet-buffer, receipt, route, and attempt ownership is otherwise
    /// identical to [`Self::prepare_data_into_slot`].
    pub fn prepare_rehydrated_opportunistic_lxmf_into_slot<'a, R: RngCore + CryptoRng>(
        &mut self,
        buffer: &'a mut TxPacketBuffer,
        request: PrepareDataRequest<'_>,
        rng: &mut R,
    ) -> Result<RoutedTxJob<'a>, PrepareFailure<'a>> {
        self.prepare_data_into_slot_with(
            buffer,
            request,
            DataPreparationMode::RehydratedOpportunisticLxmf,
            rng,
        )
    }

    /// Prepare one exact durable, locally signed LXMF wire message through an
    /// already-active authenticated Link.
    ///
    /// The opaque Link handle must still be retained, active, bound to an
    /// interface, and associated with `request.destination`. All external
    /// packet-buffer, route, permit, receipt and attempt ownership is shared
    /// with destination DATA.
    pub fn prepare_rehydrated_direct_lxmf_into_slot<'a, R: RngCore + CryptoRng>(
        &mut self,
        buffer: &'a mut TxPacketBuffer,
        link: LinkHandle,
        request: PrepareDataRequest<'_>,
        rng: &mut R,
    ) -> Result<RoutedTxJob<'a>, PrepareFailure<'a>> {
        self.prepare_data_into_slot_with(
            buffer,
            request,
            DataPreparationMode::RehydratedDirectLxmf(link.into_rns()),
            rng,
        )
    }

    fn prepare_data_into_slot_with<'a, R: RngCore + CryptoRng>(
        &mut self,
        buffer: &'a mut TxPacketBuffer,
        request: PrepareDataRequest<'_>,
        mode: DataPreparationMode,
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

        let attempt_link = match mode {
            DataPreparationMode::RehydratedDirectLxmf(link) => Some(LinkHandle::from_rns(link)),
            DataPreparationMode::Generic | DataPreparationMode::RehydratedOpportunisticLxmf => None,
        };
        let result = match mode {
            DataPreparationMode::Generic => self
                .rns
                .prepare_data_into(
                    &request.destination.into_rns(),
                    request.plaintext,
                    request.rns_now.get(),
                    rng,
                    &mut buffer.bytes,
                )
                .map(PreparedReceiptPacket::DestinationData)
                .map_err(SubmitError::from),
            DataPreparationMode::RehydratedOpportunisticLxmf => {
                let destination_matches = request
                    .plaintext
                    .get(..16)
                    .is_some_and(|prefix| prefix == request.destination.as_bytes());
                if !destination_matches {
                    Err(SubmitError::Invariant)
                } else {
                    self.rns
                        .prepare_rehydrated_opportunistic_lxmf_data_into(
                            request.plaintext,
                            request.rns_now.get(),
                            rng,
                            &mut buffer.bytes,
                        )
                        .map(PreparedReceiptPacket::DestinationData)
                        .map_err(SubmitError::from)
                }
            }
            DataPreparationMode::RehydratedDirectLxmf(link_id) => {
                let destination_matches = request
                    .plaintext
                    .get(..16)
                    .is_some_and(|prefix| prefix == request.destination.as_bytes());
                if !destination_matches {
                    Err(SubmitError::Invariant)
                } else {
                    self.rns
                        .prepare_rehydrated_direct_lxmf_link_data_into(
                            request.plaintext,
                            &link_id,
                            request.rns_now.get(),
                            rng,
                            &mut buffer.bytes,
                        )
                        .map(PreparedReceiptPacket::LinkData)
                        .map_err(SubmitError::from)
                }
            }
        };

        let prepared = match result {
            Ok(prepared) => prepared,
            Err(reason) => {
                self.dispatches[slot.index()].state = DispatchState::Free;
                self.attempts[attempt.slot].state = AttemptState::Free;
                return Err(PrepareFailure {
                    reason,
                    owner: PrepareFailureOwner::Available(buffer),
                });
            }
        };

        if let PreparedReceiptPacket::DestinationData(destination_data) = prepared
            && !self
                .rns
                .park_data_receipt(destination_data, request.rns_now.get())
        {
            self.dispatches[slot.index()].state = DispatchState::Free;
            self.attempts[attempt.slot].state = AttemptState::Free;
            return Err(PrepareFailure {
                reason: SubmitError::Invariant,
                owner: PrepareFailureOwner::Available(buffer),
            });
        }

        let token = AttemptToken(*prepared.receipt().as_bytes());
        let encoded_packet_sha256 = EncodedPacketSha256::new(
            Sha256::digest(&buffer.bytes[..usize::from(prepared.packet_len())]).into(),
        );
        self.attempts[attempt.slot].state = AttemptState::Active {
            generation: attempt.generation,
            token,
            kind: prepared.receipt_kind(),
            link: attempt_link,
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
                if self.cancel_prepared_receipt(prepared) {
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
                    receipt_registered_at: request.rns_now,
                    receipt_registered_owner_at: request.owner_now,
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
            receipt_registered_at: request.rns_now,
            receipt_registered_owner_at: request.owner_now,
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
            Some(terminal)
                if terminal.token().as_bytes() == prepared.receipt().as_bytes()
                    && terminal.receipt_kind() == prepared.receipt_kind() => {}
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
                if !self.cancel_prepared_receipt(prepared) {
                    return Err(RollbackFailure {
                        reason: RollbackErrorKind::ReceiptMissing {
                            attempt: AttemptToken(*prepared.receipt().as_bytes()),
                        },
                        job,
                    });
                }
                let link = self.attempt_link(attempt);
                self.attempts[attempt.slot].state = AttemptState::Terminal {
                    generation: attempt.generation,
                    token: AttemptToken(*prepared.receipt().as_bytes()),
                    kind: prepared.receipt_kind(),
                    link,
                    outcome: AttemptOutcome::Unsent(AttemptUnsentReason::QueueRollback),
                    ingress: None,
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
                    if terminal.token().as_bytes() == record.prepared.receipt().as_bytes()
                        && terminal.receipt_kind() == record.prepared.receipt_kind() =>
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
            || scalar.receipt_kind() != record.prepared.receipt_kind()
            || scalar.target() != TxTarget::from_rns(record.prepared.target())
            || scalar.deadline() != record.deadline
            || scalar.handle() != self.handle_for(record.attempt)
        {
            return Ok(TxCompletionDisposition::Quarantined(
                self.quarantine_completion(completion, record, now, TxRecoveryReason::Invariant),
            ));
        }

        let (completion_authorized, receipt_boundary, completion_unsent_reason) =
            match completion.kind {
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
                    (false, None, Some(reason))
                }
                CompletionKind::Authorized(code) => {
                    let _ = code.get();
                    (true, Some(now), None)
                }
                CompletionKind::Transmitted {
                    code,
                    transmitted_at,
                } => {
                    let _ = code.get();
                    if transmitted_at < record.receipt_registered_owner_at
                        || transmitted_at.get() > now.get().saturating_add(1)
                    {
                        return Ok(TxCompletionDisposition::Quarantined(
                            self.quarantine_completion(
                                completion,
                                record,
                                now,
                                TxRecoveryReason::Invariant,
                            ),
                        ));
                    }
                    (true, Some(transmitted_at), None)
                }
                CompletionKind::RecoveryFault(code) => {
                    if record.may_have_transmitted
                        && self.terminal_for(record.attempt).is_none()
                        && matches!(record.prepared, PreparedReceiptPacket::DestinationData(_))
                        && !self.arm_destination_receipt(record, now)
                    {
                        return Ok(TxCompletionDisposition::Quarantined(
                            self.quarantine_completion(
                                completion,
                                record,
                                now,
                                TxRecoveryReason::Invariant,
                            ),
                        ));
                    }
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
                || terminal.receipt_kind() != record.prepared.receipt_kind()
        }) {
            return Ok(TxCompletionDisposition::Quarantined(
                self.quarantine_completion(completion, record, now, TxRecoveryReason::Invariant),
            ));
        }
        let stop_fanout = recovered.is_some() || terminal.is_some();
        if terminal.is_none()
            && matches!(record.prepared, PreparedReceiptPacket::DestinationData(_))
        {
            let receipt_updated = if !stop_fanout && !record.remaining.is_empty() {
                match receipt_boundary {
                    Some(receipt_boundary) => {
                        self.park_destination_receipt(record, receipt_boundary)
                    }
                    None => true,
                }
            } else {
                match receipt_boundary.or_else(|| record.may_have_transmitted.then_some(now)) {
                    Some(receipt_boundary) => {
                        self.arm_destination_receipt(record, receipt_boundary)
                    }
                    None => true,
                }
            };
            if !receipt_updated {
                return Ok(TxCompletionDisposition::Quarantined(
                    self.quarantine_completion(
                        completion,
                        record,
                        now,
                        TxRecoveryReason::Invariant,
                    ),
                ));
            }
        }

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
                || !self.cancel_prepared_receipt(record.prepared)
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
            let link = self.attempt_link(record.attempt);
            self.attempts[record.attempt.slot].state = AttemptState::Terminal {
                generation: record.attempt.generation,
                token: AttemptToken(*record.prepared.receipt().as_bytes()),
                kind: record.prepared.receipt_kind(),
                link,
                outcome: AttemptOutcome::Unsent(reason),
                ingress: None,
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
        self.ingest_observed(
            raw,
            now,
            PacketIngressObservation::remote(interface, None),
            rng,
        )
    }

    /// Process one inbound packet with exact interface provenance and optional
    /// physical-link signal values.
    pub fn ingest_observed<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: MonotonicSeconds,
        ingress: PacketIngressObservation,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        self.ingest_at_observed(
            raw,
            now,
            MonotonicInstant::from_secs(now.get()),
            ingress,
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
        self.ingest_at_observed(
            raw,
            now,
            link_now,
            PacketIngressObservation::remote(interface, None),
            rng,
        )
    }

    /// Precise Link-clock variant of [`Self::ingest_observed`].
    pub fn ingest_at_observed<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: MonotonicSeconds,
        link_now: MonotonicInstant,
        ingress: PacketIngressObservation,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        self.ingest_at_with_broadcast_scope_observed(
            raw,
            now,
            link_now,
            ingress,
            IngressBroadcastScope::SharedMedium,
            rng,
        )
    }

    /// Precise Link-clock ingress with media-aware derived-broadcast policy.
    pub fn ingest_at_with_broadcast_scope<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: MonotonicSeconds,
        link_now: MonotonicInstant,
        interface: PacketInterfaceId,
        broadcast_scope: IngressBroadcastScope,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        self.ingest_at_with_broadcast_scope_observed(
            raw,
            now,
            link_now,
            PacketIngressObservation::remote(interface, None),
            broadcast_scope,
            rng,
        )
    }

    /// Precise Link-clock ingress with exact provenance and media-aware
    /// derived-broadcast policy.
    #[allow(
        clippy::too_many_arguments,
        reason = "receipt-atomic ingress keeps both clocks, packet provenance, media policy and entropy owner explicit"
    )]
    pub fn ingest_at_with_broadcast_scope_observed<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: MonotonicSeconds,
        link_now: MonotonicInstant,
        ingress: PacketIngressObservation,
        broadcast_scope: IngressBroadcastScope,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        self.ingest_at_with_broadcast_policy_observed(
            raw,
            now,
            link_now,
            ingress,
            IngressBroadcastPolicy::new(broadcast_scope),
            rng,
        )
    }

    /// Precise Link-clock ingress with exact provenance, media topology and an
    /// optional exact discovery-egress restriction.
    #[allow(
        clippy::too_many_arguments,
        reason = "receipt-atomic ingress keeps both clocks, packet provenance, routing policy and entropy owner explicit"
    )]
    pub fn ingest_at_with_broadcast_policy_observed<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: MonotonicSeconds,
        link_now: MonotonicInstant,
        ingress: PacketIngressObservation,
        policy: IngressBroadcastPolicy,
        rng: &mut R,
    ) -> Result<IngressReport, ReceiptCorrelationError> {
        let (result, fault) = {
            let (rns, attempts) = (&mut self.rns, &mut self.attempts);
            let mut sink = AttemptReceiptSink::new(attempts);
            let result = rns.ingest_observed_with_receipt_sink_at_with_broadcast_policy(
                raw,
                now.get(),
                link_now,
                ingress.into_rns(),
                policy,
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
            timed_out_attempts: report
                .timed_out_receipts
                .saturating_add(report.timed_out_link_data_receipts),
            timed_out_attempt_tag: report.timed_out_receipt_tag,
            timed_out_attempt_tags_consistent: report.timed_out_receipt_tags_consistent,
            correlation_fault: fault.or_else(|| {
                report
                    .receipt_terminals_deferred
                    .then_some(ReceiptCorrelationError::ReservationInvariant)
            }),
        }
    }

    /// Whether one exact Link has an outstanding or unacknowledged DATA
    /// attempt bound to it.
    ///
    /// Active attempts remain outstanding through transmission and receipt
    /// correlation. Terminal attempts remain visible until their durable
    /// outcome has been acknowledged. Reserved slots and destination DATA,
    /// which is not bound to a Link, do not occupy a Link.
    pub fn link_has_unacknowledged_attempt(&self, link: LinkHandle) -> bool {
        self.attempts.iter().any(|slot| {
            matches!(
                slot.state,
                AttemptState::Active {
                    link: Some(bound),
                    ..
                } | AttemptState::Terminal {
                    link: Some(bound),
                    ..
                } if bound == link
            )
        })
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
            kind,
            link,
            outcome,
            ingress,
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
            kind,
            link,
            outcome,
            ingress,
        };
        self.attempts[attempt_ref.slot].state = AttemptState::Free;
        Ok(terminal)
    }

    /// Copy the complete allocation-free RNS telemetry and capacity snapshot.
    pub fn metrics(&self) -> EmbeddedNodeMetrics {
        self.rns.metrics()
    }

    /// Read scalar dispatch and receipt occupancy without exposing RNS internals.
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
            link_data_receipts_used: rns.capacity.link_data_receipts.used,
            link_data_receipts_limit: rns.capacity.link_data_receipts.limit,
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
    ) -> Result<(PreparedReceiptPacket, AttemptRef), RollbackErrorKind> {
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
                && prepared.receipt_kind() == scalar.receipt_kind()
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
            Some(AttemptState::Active {
                generation,
                token,
                kind,
                ..
            }
                | AttemptState::Terminal {
                    generation,
                    token,
                    kind,
                    ..
                })
                if generation == record.attempt.generation
                    && token.as_bytes() == record.prepared.receipt().as_bytes()
                    && kind == record.prepared.receipt_kind()
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

    fn attempt_is_active(&self, attempt: AttemptRef, prepared: PreparedReceiptPacket) -> bool {
        matches!(
            self.attempts.get(attempt.slot).map(|slot| slot.state),
            Some(AttemptState::Active {
                generation,
                token,
                kind,
                ..
            })
                if generation == attempt.generation
                    && token.as_bytes() == prepared.receipt().as_bytes()
                    && kind == prepared.receipt_kind()
        )
    }

    fn attempt_link(&self, attempt: AttemptRef) -> Option<LinkHandle> {
        match self.attempts.get(attempt.slot).map(|slot| slot.state) {
            Some(
                AttemptState::Active {
                    generation, link, ..
                }
                | AttemptState::Terminal {
                    generation, link, ..
                },
            ) if generation == attempt.generation => link,
            Some(
                AttemptState::Free
                | AttemptState::Reserved { .. }
                | AttemptState::Active { .. }
                | AttemptState::Terminal { .. },
            )
            | None => None,
        }
    }

    fn terminal_for(&self, attempt: AttemptRef) -> Option<TerminalAttempt> {
        let AttemptState::Terminal {
            generation,
            token,
            kind,
            link,
            outcome,
            ingress,
        } = self.attempts.get(attempt.slot)?.state
        else {
            return None;
        };
        (generation == attempt.generation).then_some(TerminalAttempt {
            handle: self.handle_for(attempt),
            token,
            kind,
            link,
            outcome,
            ingress,
        })
    }

    fn cancel_prepared_receipt(&mut self, prepared: PreparedReceiptPacket) -> bool {
        match prepared {
            PreparedReceiptPacket::DestinationData(prepared) => {
                self.rns.cancel_data_receipt(prepared.receipt())
            }
            PreparedReceiptPacket::LinkData(prepared) => {
                self.rns.cancel_link_data_receipt(prepared.receipt())
            }
        }
    }

    fn arm_destination_receipt(
        &mut self,
        record: DispatchRecord,
        observed_at: MonotonicMillis,
    ) -> bool {
        let PreparedReceiptPacket::DestinationData(prepared) = record.prepared else {
            return false;
        };
        self.rns
            .rearm_data_receipt(prepared, Self::receipt_time_at(record, observed_at))
    }

    fn park_destination_receipt(
        &mut self,
        record: DispatchRecord,
        observed_at: MonotonicMillis,
    ) -> bool {
        let PreparedReceiptPacket::DestinationData(prepared) = record.prepared else {
            return false;
        };
        self.rns
            .park_data_receipt(prepared, Self::receipt_time_at(record, observed_at))
    }

    fn receipt_time_at(record: DispatchRecord, observed_at: MonotonicMillis) -> u64 {
        let elapsed_millis = observed_at
            .get()
            .saturating_sub(record.receipt_registered_owner_at.get());
        let elapsed_seconds =
            elapsed_millis / 1_000 + u64::from(!elapsed_millis.is_multiple_of(1_000));
        record
            .receipt_registered_at
            .get()
            .saturating_add(elapsed_seconds)
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

    fn try_reserve_data(
        &mut self,
        actual_kind: DataReceiptKind,
        token: AttemptToken,
    ) -> Result<AttemptTerminalReservation<'_>, ReceiptReservationUnavailable> {
        let active_slot = self.attempts.iter().position(|slot| {
            matches!(slot.state, AttemptState::Active { token: active, .. } if active == token)
        });
        if let Some(index) = active_slot {
            let (generation, expected_kind, link) = match self.attempts[index].state {
                AttemptState::Active {
                    generation,
                    kind,
                    link,
                    ..
                } => (generation, kind, link),
                AttemptState::Free
                | AttemptState::Reserved { .. }
                | AttemptState::Terminal { .. } => {
                    return self.reject(ReceiptCorrelationError::ReservationInvariant);
                }
            };
            if expected_kind != actual_kind {
                return self.reject(ReceiptCorrelationError::WrongKind {
                    attempt: token,
                    expected: expected_kind,
                    actual: actual_kind,
                });
            }
            return Ok(AttemptTerminalReservation {
                state: &mut self.attempts[index].state,
                generation,
                token,
                kind: expected_kind,
                link,
            });
        }
        let terminal_kind = self.attempts.iter().find_map(|slot| match slot.state {
            AttemptState::Terminal {
                token: terminal_token,
                kind,
                ..
            } if terminal_token == token => Some(kind),
            AttemptState::Free
            | AttemptState::Reserved { .. }
            | AttemptState::Active { .. }
            | AttemptState::Terminal { .. } => None,
        });
        match terminal_kind {
            Some(expected) if expected != actual_kind => {
                self.reject(ReceiptCorrelationError::WrongKind {
                    attempt: token,
                    expected,
                    actual: actual_kind,
                })
            }
            Some(_) => self.reject(ReceiptCorrelationError::AlreadyTerminal { attempt: token }),
            None => self.reject(ReceiptCorrelationError::UnknownData { attempt: token }),
        }
    }
}

struct AttemptTerminalReservation<'a> {
    state: &'a mut AttemptState,
    generation: u64,
    token: AttemptToken,
    kind: DataReceiptKind,
    link: Option<LinkHandle>,
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
        let Some(actual_kind) = DataReceiptKind::from_rns(candidate.kind()) else {
            return self.reject(ReceiptCorrelationError::UnsupportedChannel);
        };
        let token = AttemptToken(*candidate.receipt().as_bytes());
        self.try_reserve_data(actual_kind, token)
    }
}

impl ReceiptTerminalReservation for AttemptTerminalReservation<'_> {
    fn commit(self, terminal: ReceiptTerminal) {
        let candidate = terminal.candidate();
        debug_assert_eq!(DataReceiptKind::from_rns(candidate.kind()), Some(self.kind));
        debug_assert_eq!(candidate.receipt().as_bytes(), self.token.as_bytes());
        let outcome = match terminal {
            ReceiptTerminal::Delivered(_) => AttemptOutcome::Delivered,
            ReceiptTerminal::TimedOut(_) => AttemptOutcome::DeliveryTimeout,
        };
        *self.state = AttemptState::Terminal {
            generation: self.generation,
            token: self.token,
            kind: self.kind,
            link: self.link,
            outcome,
            ingress: candidate.ingress().map(PacketIngressObservation::from_rns),
        };
    }
}

#[cfg(test)]
mod tests;
