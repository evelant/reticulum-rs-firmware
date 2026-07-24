//! Owning, fail-closed firmware boundary around Rete's native `NodeCore`.
//!
//! Rete's heapless storage bounds several maps, but its public `NodeCore`
//! still permits oversized hosted ingress, silently loses some table
//! insertions, and exposes interface routing after the native ingress context
//! has been forgotten. This module owns the core and resolves that routing
//! while the relevant context is still available, admitting only the subset we
//! can make deterministic and observable on embedded targets.

use alloc::{collections::BTreeMap, vec::Vec};

use rand_core::{CryptoRng, RngCore};
use rete_core::{
    CONTEXT_NONE, CONTEXT_PATH_RESPONSE, CONTEXT_RESOURCE, CONTEXT_RESOURCE_ADV,
    CONTEXT_RESOURCE_HMU, CONTEXT_RESOURCE_ICL, CONTEXT_RESOURCE_PRF, CONTEXT_RESOURCE_RCL,
    CONTEXT_RESOURCE_REQ, DestHash, DestType, HeaderType, Identity, IdentityHash, LinkId,
    MonotonicInstant, Packet, PacketBuilder, PacketType, TRUNCATED_HASH_LEN,
};
use rete_lxmf_core::{LXMessage, LxmfMessageError};
use rete_stack::node_core::{OutboundDispatchInterval, OutboundProtocolToken};
use rete_stack::{
    ConfirmedRequestDispatch as NativeConfirmedRequestDispatch, DestinationType, Direction,
    IngestOutcome, IngestRejection, LinkTableKind, NodeCore, NodeEvent as NativeNodeEvent,
    OutboundPacket, PacketRouting, PreparedRequestConfirmation as NativeRequestConfirmation,
    ReceiptToken, RequestDispatchError as NativeRequestDispatchError,
    RequestFailReason as NativeRequestFailReason,
};
use rete_transport::{HeaplessStorage, LinkRole, LinkState, SendError, Transport};
use sha2::{Digest, Sha256};

use crate::capacity::{
    HeaplessCapacitySnapshot, LinkAdmissionError, heapless_capacity_snapshot,
    try_initiate_heapless_link_at,
};

/// Largest application payload that can be queued in one base-MTU channel
/// envelope without a later packet-build failure.
pub const MAX_CHANNEL_PAYLOAD: usize =
    rete_transport::LINK_MDU - rete_transport::ENVELOPE_HEADER_SIZE;

/// Base Reticulum packet MTU used by the embedded radio profile.
pub const RNS_MTU: usize = rete_core::MTU;

/// RNS Link DATA context that carries ordinary application bytes.
pub const LINK_DATA_CONTEXT_NONE: u8 = CONTEXT_NONE;

/// Largest plaintext accepted by destination-DATA encryption at the base MTU.
pub const MAX_DATA_PAYLOAD: usize = rete_core::ENCRYPTED_MDU;

/// Largest Python LXMF `content_size` selected for opportunistic delivery.
///
/// Python LXMF 1.0.1 computes this value as the canonical MessagePack payload
/// length minus eight timestamp bytes and eight bytes of structural overhead.
/// It is not the complete MessagePack payload or RNS plaintext length. The
/// Python calculation is signed: the canonical empty-title, empty-content
/// payload is 15 bytes and therefore has a `content_size` of -1.
pub const MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE: usize = 295;

/// Largest source-hash, signature, and MessagePack carrier accepted by the
/// dedicated opportunistic LXMF destination-DATA path.
///
/// A maximum-size canonical basic message contains 16 source-hash bytes, 64
/// signature bytes, 16 bytes excluded by Python's `content_size` calculation,
/// and 295 selected bytes. Its encrypted representation fits a Header-1 RNS
/// DATA packet but not a Header-2 packet routed through a repeater.
pub const MAX_OPPORTUNISTIC_LXMF_CARRIER: usize = 391;

/// Largest Python LXMF `content_size` selected for one direct Link packet.
///
/// Direct Link DATA carries the complete LXMF wire message, so Python subtracts
/// the complete 112-byte LXMF overhead from the 431-byte Link MDU.
pub const MAX_DIRECT_LXMF_CONTENT_SIZE: usize = 319;

/// Largest complete basic LXMF wire message carried by one Link DATA packet.
///
/// Messages above this boundary require the separately qualified RNS Resource
/// path; they must not be truncated or silently sent opportunistically.
pub const MAX_DIRECT_LXMF_WIRE: usize = rete_transport::LINK_MDU;

/// Largest whole-millisecond Unix timestamp supported by basic composition.
///
/// Python LXMF serialises timestamps as binary64 seconds. Restricting this API
/// to positive integer milliseconds below 2^43 seconds keeps the integer cast
/// exact and the binary64 spacing below one millisecond, so distinct accepted
/// millisecond inputs remain distinct on the wire. Python accepts a broader
/// floating-point domain; this is the portable subset used by firmware and
/// JavaScript clients.
pub const MAX_LXMF_TIMESTAMP_UNIX_MS: u64 = (1_u64 << 43) * 1_000 - 1;

const LXMF_DESTINATION_HASH_LENGTH: usize = TRUNCATED_HASH_LEN;
const LXMF_SIGNATURE_LENGTH: usize = 64;
const LXMF_SELECTION_OVERHEAD: usize = 16;
const LXMF_WIRE_PREFIX_LENGTH: usize = LXMF_DESTINATION_HASH_LENGTH * 2 + LXMF_SIGNATURE_LENGTH;
const LXMF_APPLICATION_NAME: &str = "lxmf";
const LXMF_DELIVERY_ASPECT: &str = "delivery";
const LXMF_DELIVERY_EXPANDED_NAME: &str = "lxmf.delivery";
const SINGLE_PACKET_REQUEST_OVERHEAD: usize = 1 + 9 + 2 + TRUNCATED_HASH_LEN;
const _: () = assert!(
    LXMF_DESTINATION_HASH_LENGTH
        + LXMF_SIGNATURE_LENGTH
        + LXMF_SELECTION_OVERHEAD
        + MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE
        == MAX_OPPORTUNISTIC_LXMF_CARRIER
);
const _: () = assert!(MAX_OPPORTUNISTIC_LXMF_CARRIER > MAX_DATA_PAYLOAD);
const _: () = assert!(
    LXMF_WIRE_PREFIX_LENGTH + LXMF_SELECTION_OVERHEAD + MAX_DIRECT_LXMF_CONTENT_SIZE
        == MAX_DIRECT_LXMF_WIRE
);

/// Interface identifier carried across the synchronous ingest boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceId(pub u8);

/// Firmware routing target with native interface routing already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxTarget {
    /// Send on every eligible interface.
    All,
    /// Send only on one exact interface selected from protocol or ingress state.
    Only(InterfaceId),
    /// Send on every eligible interface except the triggering interface.
    AllExcept(InterfaceId),
}

/// Stable product-owned identifier for one packet delivery receipt.
///
/// The complete hash is retained so cancellation and terminal correlation do
/// not depend on Rete's truncated receipt-table key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceiptId([u8; 32]);

impl ReceiptId {
    fn from_native(receipt: ReceiptToken) -> Self {
        Self(*receipt.packet_hash())
    }

    /// Complete packet hash covered by the delivery proof.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Product-owned class of delivery receipt reaching a terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptKind {
    /// Receipt for an outbound destination-DATA packet.
    Data,
    /// Receipt for ordinary context-NONE DATA on an owned Link.
    LinkData,
    /// Receipt for an outbound reliable channel message.
    Channel,
}

/// Exact delivery receipt presented before native receipt state is mutated.
///
/// The complete receipt ID and its class let a bounded application sink
/// reserve the correct submission record without depending on Rete types or
/// its truncated receipt-table keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptCandidate {
    kind: ReceiptKind,
    receipt: ReceiptId,
}

impl ReceiptCandidate {
    fn from_native(candidate: rete_stack::ReceiptCandidate) -> Self {
        let kind = match candidate.kind {
            rete_stack::ReceiptKind::Data => ReceiptKind::Data,
            rete_stack::ReceiptKind::LinkData => ReceiptKind::LinkData,
            rete_stack::ReceiptKind::Channel => ReceiptKind::Channel,
        };
        Self {
            kind,
            receipt: ReceiptId(candidate.packet_hash),
        }
    }

    /// Class of receipt that may become terminal.
    pub const fn kind(self) -> ReceiptKind {
        self.kind
    }

    /// Complete packet hash identifying the receipt.
    pub const fn receipt(self) -> ReceiptId {
        self.receipt
    }
}

/// Delivery outcome committed after native proof validation or timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptTerminal {
    /// A valid proof covered the reserved receipt candidate.
    Delivered(ReceiptCandidate),
    /// A receipt expired or its retained Link closed without a valid proof.
    TimedOut(ReceiptCandidate),
}

impl ReceiptTerminal {
    /// Candidate whose state became terminal.
    pub const fn candidate(self) -> ReceiptCandidate {
        match self {
            Self::Delivered(candidate) | Self::TimedOut(candidate) => candidate,
        }
    }
}

/// A terminal sink could not reserve an infallible commit slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptReservationUnavailable;

impl core::fmt::Display for ReceiptReservationUnavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("receipt terminal reservation unavailable")
    }
}

/// One product-owned terminal slot reserved before native receipt mutation.
pub trait ReceiptTerminalReservation {
    /// Commit a terminal outcome without allocation or failure.
    fn commit(self, terminal: ReceiptTerminal);
}

/// Reservable product-owned destination for receipt terminal outcomes.
///
/// A successful reservation must make its later commit infallible. Dropping
/// an unused reservation (for example after an invalid proof) must release it.
pub trait ReceiptTerminalSink {
    /// Reservation borrowing this sink until commit or drop.
    type Reservation<'a>: ReceiptTerminalReservation
    where
        Self: 'a;

    /// Reserve capacity for one exact candidate before Rete mutates it.
    fn try_reserve(
        &mut self,
        candidate: ReceiptCandidate,
    ) -> Result<Self::Reservation<'_>, ReceiptReservationUnavailable>;
}

/// Scalar metadata for encrypted DATA prepared into caller-owned storage.
///
/// The packet bytes remain in the caller's exact-size output buffer. Returning
/// only Copy metadata ends Rete's temporary borrow before the caller commits
/// its already-reserved outbox slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedData {
    packet_len: u16,
    target: TxTarget,
    receipt: ReceiptId,
}

impl PreparedData {
    /// Number of complete Reticulum packet bytes written to the output slot.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// Interfaces on which this packet may be transmitted.
    pub const fn target(self) -> TxTarget {
        self.target
    }

    /// Receipt committed transactionally with the packet.
    pub const fn receipt(self) -> ReceiptId {
        self.receipt
    }
}

/// Scalar metadata for ordinary Link DATA prepared into caller-owned storage.
///
/// The packet bytes remain in the caller's exact-size output buffer. This
/// product-owned value exposes neither the Link's peer key nor either
/// identity's private state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedLinkData {
    packet_len: u16,
    target: TxTarget,
    receipt: ReceiptId,
}

impl PreparedLinkData {
    /// Number of complete Reticulum packet bytes written to the output slot.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// Exact interface retained by the authenticated Link.
    pub const fn target(self) -> TxTarget {
        self.target
    }

    /// Complete packet hash identifying the Link-DATA delivery receipt.
    pub const fn receipt(self) -> ReceiptId {
        self.receipt
    }
}

/// Scalar result of composing one basic opportunistic LXMF carrier.
///
/// The complete carrier bytes remain in the caller's output buffer. The
/// destination prefix is intentionally omitted because RNS destination DATA
/// already carries that hash in its packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedBasicLxmf {
    carrier_len: u16,
    carrier_sha256: [u8; 32],
    message_id: [u8; 32],
    destination: DestHash,
}

/// Scalar result of composing one complete basic LXMF wire message for direct
/// Link DATA.
///
/// The caller retains the exact bytes in its output buffer. The private digest
/// binds a later Link-packet transaction to those bytes without exposing the
/// node identity or signing source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedBasicDirectLxmf {
    wire_len: u16,
    wire_sha256: [u8; 32],
    message_id: [u8; 32],
    destination: DestHash,
}

impl PreparedBasicDirectLxmf {
    /// Number of complete destination, source, signature, and MessagePack bytes
    /// initialized in the caller's output buffer.
    pub const fn wire_len(self) -> u16 {
        self.wire_len
    }

    /// Canonical LXMF message identifier for the composed four-item payload.
    pub const fn message_id(self) -> [u8; 32] {
        self.message_id
    }

    /// Remote `lxmf.delivery` destination bound into the complete wire message.
    pub const fn destination(self) -> DestHash {
        self.destination
    }
}

impl PreparedBasicLxmf {
    /// Number of source-hash, signature, and MessagePack bytes written.
    pub const fn carrier_len(self) -> u16 {
        self.carrier_len
    }

    /// Canonical LXMF message identifier for the composed four-item payload.
    pub const fn message_id(self) -> [u8; 32] {
        self.message_id
    }

    /// RNS destination whose implied hash completes the signed LXMF wire.
    pub const fn destination(self) -> DestHash {
        self.destination
    }
}

struct ComposedBasicLxmf {
    packed: Vec<u8>,
    payload_len: usize,
    message_id: [u8; 32],
}

/// Owned packet emitted by the protocol core for an interface actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxPacket {
    /// Complete Reticulum packet bytes.
    bytes: Vec<u8>,
    /// Interfaces on which the packet may be sent.
    target: TxTarget,
    /// Opaque marker returned after the selected interface accepts dispatch.
    protocol_token: Option<OutboundProtocolToken>,
}

/// Exact application delivery proof withheld from ordinary ingress transmission.
///
/// The proof remains a single move-only owner until application policy allows
/// it to re-enter the ordinary transmission path. Its packet bytes, covered
/// hash and source interface are deliberately absent from `Debug` output.
#[must_use = "a retained inbound proof must be released or explicitly discarded"]
pub(crate) struct RetainedInboundProof {
    packet: TxPacket,
}

impl core::fmt::Debug for RetainedInboundProof {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RetainedInboundProof")
            .field("packet_len", &self.packet.bytes.len())
            .finish_non_exhaustive()
    }
}

impl RetainedInboundProof {
    pub(crate) fn into_packet(self) -> TxPacket {
        self.packet
    }
}

#[cfg(test)]
pub(crate) fn retained_proof_for_owner_test(marker: u8) -> (RetainedInboundProof, *const u8) {
    let bytes = alloc::vec![marker; 37];
    let pointer = bytes.as_ptr();
    (
        RetainedInboundProof {
            packet: TxPacket {
                bytes,
                target: TxTarget::Only(InterfaceId(marker)),
                protocol_token: None,
            },
        },
        pointer,
    )
}

/// Move-only proof owner bound to one exact semantic event index.
///
/// Keeping this owner inside [`ApplicationEvents`] but outside
/// [`ApplicationEvent`] preserves the transport-neutral event contract. The
/// public wrapper exposes only immutable semantic events, so safe downstream
/// code cannot separate this proof from the collection it was created beside.
#[must_use = "a retained application proof must stay owned with its event collection"]
pub(crate) struct RetainedApplicationProof {
    event_index: usize,
    proof: RetainedInboundProof,
}

impl RetainedApplicationProof {
    /// Zero-based index in the accompanying semantic-event collection.
    pub(crate) const fn event_index(&self) -> usize {
        self.event_index
    }

    /// Consume into the exact event index and sole retained-proof owner.
    pub(crate) fn into_parts(self) -> (usize, RetainedInboundProof) {
        (self.event_index, self.proof)
    }

    #[cfg(test)]
    pub(crate) fn packet_bytes_ptr(&self) -> *const u8 {
        self.proof.packet.bytes.as_ptr()
    }
}

impl core::fmt::Debug for RetainedApplicationProof {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RetainedApplicationProof")
            .field("event_index", &self.event_index)
            .field("proof_present", &true)
            .finish()
    }
}

impl TxPacket {
    fn broadcast(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            target: TxTarget::All,
            protocol_token: None,
        }
    }

    /// Complete Reticulum packet bytes for transmission.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Interfaces on which this packet may be transmitted.
    pub const fn target(&self) -> TxTarget {
        self.target
    }

    /// Opaque timing marker for LINKREQUEST or LRPROOF egress confirmation.
    pub const fn protocol_token(&self) -> Option<OutboundProtocolToken> {
        self.protocol_token
    }

    /// Consume the packet into bytes, routing authorization and timing marker.
    pub fn into_parts(self) -> (Vec<u8>, TxTarget, Option<OutboundProtocolToken>) {
        (self.bytes, self.target, self.protocol_token)
    }
}

/// Stable product-owned correlation for one direct Link request.
///
/// The fields are private so callers cannot forge a Link/request association.
/// The value remains copyable because queue and retry bookkeeping carry only
/// this scalar; the owning [`EmbeddedNode`] retains Rete's move-only dispatch
/// authorities until the request is dispatched or canceled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestHandle {
    link: [u8; TRUNCATED_HASH_LEN],
    request: [u8; TRUNCATED_HASH_LEN],
}

impl RequestHandle {
    fn from_native(confirmation: &NativeRequestConfirmation) -> Self {
        Self {
            link: *confirmation.link_id().as_bytes(),
            request: *confirmation.request_id().as_bytes(),
        }
    }

    /// Exact Link identifier used by application-event correlation.
    pub const fn link(&self) -> &[u8; TRUNCATED_HASH_LEN] {
        &self.link
    }

    /// Exact request identifier used by response and failure correlation.
    pub const fn request(&self) -> &[u8; TRUNCATED_HASH_LEN] {
        &self.request
    }
}

/// One direct request packet whose confirmation authority remains node-owned.
///
/// Splitting this value moves the sole packet owner while returning only a
/// copyable opaque handle. If no interface accepts the packet, the handle must
/// be returned to [`EmbeddedNode::cancel_prepared_request`].
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a prepared request must be dispatched or canceled"]
pub struct PreparedDirectRequest {
    packet: TxPacket,
    handle: RequestHandle,
}

impl PreparedDirectRequest {
    /// Complete encrypted Reticulum packet prepared for one active Link.
    pub const fn packet(&self) -> &TxPacket {
        &self.packet
    }

    /// Exact product-owned correlation handle.
    pub const fn handle(&self) -> RequestHandle {
        self.handle
    }

    /// Consume into the packet owner and copyable correlation handle.
    pub fn into_parts(self) -> (TxPacket, RequestHandle) {
        (self.packet, self.handle)
    }
}

/// Failure to prepare one direct, single-packet Link request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareDirectRequestError {
    /// The selected Link is not retained.
    LinkNotFound,
    /// The selected Link has not reached the active state.
    LinkNotActive,
    /// The active Link has no authenticated interface binding.
    LinkInterfaceUnknown,
    /// The complete canonical request exceeds the negotiated Link MDU.
    RequestTooLarge { actual: usize, maximum: usize },
    /// Supplied request data is not exactly one encoded MessagePack value.
    InvalidRequestValue,
    /// The adapter cannot retain another dispatch authority.
    DispatchTableFull { limit: usize },
    /// A still-live request already uses the generated request identifier.
    RequestAlreadyTracked,
    /// Native bounded request storage could not be reserved.
    AllocationFailed,
    /// Link encryption failed.
    Crypto,
    /// Rete could not build or parse the base-MTU Link packet.
    PacketBuild,
    /// Rete returned an error or packet shape impossible for this operation.
    Invariant,
}

impl core::fmt::Display for PrepareDirectRequestError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LinkNotFound => formatter.write_str("selected Link is not retained"),
            Self::LinkNotActive => formatter.write_str("selected Link is not active"),
            Self::LinkInterfaceUnknown => {
                formatter.write_str("selected Link has no authenticated interface binding")
            }
            Self::RequestTooLarge { actual, maximum } => write!(
                formatter,
                "canonical request length {actual} exceeds Link MDU {maximum}"
            ),
            Self::InvalidRequestValue => {
                formatter.write_str("request data is not exactly one MessagePack value")
            }
            Self::DispatchTableFull { limit } => {
                write!(formatter, "request dispatch table is full (limit {limit})")
            }
            Self::RequestAlreadyTracked => {
                formatter.write_str("request identifier is already tracked")
            }
            Self::AllocationFailed => formatter.write_str("request storage allocation failed"),
            Self::Crypto => formatter.write_str("request Link encryption failed"),
            Self::PacketBuild => formatter.write_str("request packet construction failed"),
            Self::Invariant => formatter.write_str("unexpected native request failure"),
        }
    }
}

/// Result of applying a router's first-dispatch decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a confirmed request must be marked dispatched or canceled"]
pub enum RequestDispatchConfirmation {
    /// This was not the packet's first interface dispatch; no state changed.
    NotFirstDispatch,
    /// Timeout tracking started and the exact confirmed authority is retained.
    Confirmed,
}

/// Native request phase removed by one exact product cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanceledRequestDispatch {
    /// The packet had not reached its first interface dispatch.
    Prepared,
    /// Timeout tracking had started for the dispatched packet.
    Confirmed,
}

/// Failure to transition or cancel one exact request dispatch authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDispatchError {
    /// No dispatch authority exists for this exact Link/request pair.
    NotTracked,
    /// The exact request is not awaiting first dispatch.
    NotPrepared,
    /// The exact request is not awaiting dispatch completion or rollback.
    NotConfirmed,
    /// The Link was removed after request preparation.
    LinkNotFound,
    /// The Link stopped being active after request preparation.
    LinkNotActive,
    /// Adapter and native request state no longer agree.
    NativeStateMismatch,
}

impl core::fmt::Display for RequestDispatchError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::NotTracked => "request dispatch authority is not tracked",
            Self::NotPrepared => "request is not awaiting first dispatch",
            Self::NotConfirmed => "request is not awaiting dispatch completion",
            Self::LinkNotFound => "prepared request Link was removed",
            Self::LinkNotActive => "prepared request Link is not active",
            Self::NativeStateMismatch => "adapter and native request state disagree",
        })
    }
}

enum NativeRequestDispatchAuthority {
    Prepared(NativeRequestConfirmation),
    Confirmed(NativeConfirmedRequestDispatch),
}

struct RequestDispatchEntry {
    handle: RequestHandle,
    authority: Option<NativeRequestDispatchAuthority>,
}

/// Read-only native Link timing state exposed only to conformance tests.
#[cfg(any(test, feature = "conformance"))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConformanceLinkSnapshot {
    /// Native lifecycle state.
    pub state: LinkState,
    /// Negotiated binary64 RTT in seconds.
    pub rtt: f64,
    /// Immutable provisional or dispatch-confirmed request origin.
    pub request_started_at: MonotonicInstant,
    /// Whether runtime dispatch replaced the provisional request origin.
    pub request_time_confirmed: bool,
    /// Interface whose first successful dispatch confirmed the request edge.
    pub request_dispatch_interface: Option<u8>,
    /// Interface bound by authenticated Link establishment traffic.
    pub bound_interface: Option<u8>,
    /// Retained or learned hop count.
    pub expected_hops: Option<u8>,
    /// Last authenticated inbound activity.
    pub last_inbound: MonotonicInstant,
    /// Last outbound activity recorded by the protocol core.
    pub last_outbound: MonotonicInstant,
    /// Last outbound keepalive probe.
    pub last_keepalive: MonotonicInstant,
    /// Current precise keepalive interval.
    pub keepalive_interval: rete_core::MonotonicDuration,
    /// Current precise stale interval.
    pub stale_time: rete_core::MonotonicDuration,
}

/// Resolve a packet created without ingress context.
///
/// The pinned native origin APIs may select all interfaces or one retained
/// exact interface. Source-relative routing would be an upstream contract
/// violation: broadcasting it would leak traffic onto unrelated transports,
/// while silently dropping it would strand already-mutated protocol state.
fn resolve_origin_packet(packet: OutboundPacket) -> TxPacket {
    let protocol_token = packet.protocol_token();
    let target = match packet.routing {
        PacketRouting::All => TxTarget::All,
        PacketRouting::ExactInterface(interface) | PacketRouting::BoundInterface(interface) => {
            TxTarget::Only(InterfaceId(interface))
        }
        PacketRouting::SourceInterface | PacketRouting::AllExceptSource => {
            panic!("pinned Rete origin API emitted source-relative routing")
        }
    };
    TxPacket {
        bytes: packet.data,
        target,
        protocol_token,
    }
}

/// Product-owned reason a pending Link request failed.
///
/// This deliberately mirrors the complete pinned native reason set without
/// exposing Rete's enum in the firmware-facing event surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationRequestFailReason {
    /// The request timed out.
    Timeout,
    /// The Link closed before a response arrived.
    LinkClosed,
    /// The response Resource transfer failed.
    ResourceFailed,
}

/// Payload-free classification for one [`ApplicationEvent`].
///
/// The stable labels expose no packet body, key material, or identifier bytes,
/// so callers can record explicit delivery/discard policy without formatting
/// the owned event itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApplicationEventKind {
    /// A valid announce was received.
    AnnounceReceived,
    /// Decrypted DATA was addressed to a local destination.
    DataReceived,
    /// A valid proof covered one sent packet.
    ProofReceived,
    /// One sent-packet receipt expired.
    ReceiptFailed,
    /// A locally owned Link became active.
    LinkEstablished,
    /// An authenticated LRRTT updated one active Link.
    LinkRttUpdated,
    /// Decrypted best-effort data arrived on an active Link.
    LinkData,
    /// One or more reliable Channel messages arrived on a Link.
    ChannelMessages,
    /// A Link request arrived.
    RequestReceived,
    /// A Link response arrived.
    ResponseReceived,
    /// A locally owned Link closed.
    LinkClosed,
    /// A remote peer authenticated its identity on a Link.
    LinkIdentified,
    /// A Resource advertisement was retained by the native core.
    ResourceOffered,
    /// Resource transfer progress was reported.
    ResourceProgress,
    /// A Resource transfer completed.
    ResourceComplete,
    /// A Resource transfer failed.
    ResourceFailed,
    /// A peer rejected one Resource sent by this node.
    ResourceRejected,
    /// A pending Link request failed.
    RequestFailed,
    /// Response-as-Resource progress was reported for a pending request.
    RequestProgress,
    /// Periodic protocol maintenance completed.
    Tick,
}

impl ApplicationEventKind {
    /// Return a stable payload-free label suitable for logs and metrics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnnounceReceived => "announce_received",
            Self::DataReceived => "data_received",
            Self::ProofReceived => "proof_received",
            Self::ReceiptFailed => "receipt_failed",
            Self::LinkEstablished => "link_established",
            Self::LinkRttUpdated => "link_rtt_updated",
            Self::LinkData => "link_data",
            Self::ChannelMessages => "channel_messages",
            Self::RequestReceived => "request_received",
            Self::ResponseReceived => "response_received",
            Self::LinkClosed => "link_closed",
            Self::LinkIdentified => "link_identified",
            Self::ResourceOffered => "resource_offered",
            Self::ResourceProgress => "resource_progress",
            Self::ResourceComplete => "resource_complete",
            Self::ResourceFailed => "resource_failed",
            Self::ResourceRejected => "resource_rejected",
            Self::RequestFailed => "request_failed",
            Self::RequestProgress => "request_progress",
            Self::Tick => "tick",
        }
    }
}

impl core::fmt::Display for ApplicationEventKind {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Authoritative association between one retained owned Link and its destination.
///
/// Firmware can inspect this binding but cannot construct or alter it. The
/// owning [`EmbeddedNode`] creates bindings only from Rete's retained Link
/// state, keeping consumer-supplied Link and destination values out of the
/// application-event trust boundary.
///
/// ```compile_fail
/// use reticulum_rns_rete::{ApplicationLinkBinding, ApplicationLinkRole};
///
/// let _forged = ApplicationLinkBinding {
///     link: [0_u8; 16],
///     destination: [0_u8; 16],
///     role: ApplicationLinkRole::Responder,
/// };
/// ```
#[must_use = "a Link binding is authoritative application-event provenance"]
pub struct ApplicationLinkBinding {
    link: [u8; rete_core::TRUNCATED_HASH_LEN],
    destination: [u8; rete_core::TRUNCATED_HASH_LEN],
    role: ApplicationLinkRole,
}

/// Local endpoint role of one retained owned Link.
///
/// The role is copied only from Rete's retained Link state. It lets application
/// policy distinguish a Link accepted at a local destination from one this
/// node initiated without exposing mutable native state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApplicationLinkRole {
    /// This node initiated the Link.
    Initiator,
    /// This node accepted the Link at a local destination.
    Responder,
}

impl ApplicationLinkRole {
    fn from_retained(role: LinkRole) -> Self {
        match role {
            LinkRole::Initiator => Self::Initiator,
            LinkRole::Responder => Self::Responder,
        }
    }
}

impl ApplicationLinkBinding {
    fn from_retained_link(link: &rete_transport::Link) -> Self {
        Self {
            link: *link.link_id.as_bytes(),
            destination: *link.destination_hash.as_bytes(),
            role: ApplicationLinkRole::from_retained(link.role),
        }
    }

    /// Stable identifier of the retained owned Link.
    pub const fn link(&self) -> &[u8; rete_core::TRUNCATED_HASH_LEN] {
        &self.link
    }

    /// Destination to which the retained owned Link was established.
    pub const fn destination(&self) -> &[u8; rete_core::TRUNCATED_HASH_LEN] {
        &self.destination
    }

    /// Whether this node initiated or accepted the retained Link.
    pub const fn role(&self) -> ApplicationLinkRole {
        self.role
    }
}

impl core::fmt::Debug for ApplicationLinkBinding {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ApplicationLinkBinding")
            .field("link", &self.link)
            .field("destination", &self.destination)
            .field("role", &self.role)
            .finish()
    }
}

/// One product-owned application event projected from the pinned Rete core.
///
/// Every protocol identifier is copied into a stable byte array. Allocation-
/// backed bodies move directly from the native event without cloning. The enum
/// intentionally does not implement `Clone`: each payload has one exact owner
/// that the caller must retain, deliver, or explicitly discard.
#[must_use = "application events must be retained, delivered, or explicitly discarded"]
pub enum ApplicationEvent {
    /// A valid announce was received.
    AnnounceReceived {
        /// Destination hash of the announcing identity.
        destination: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Identity hash of the announcer.
        identity: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Hop count at receipt.
        hops: u8,
        /// Owned announce application data.
        app_data: Option<Vec<u8>>,
    },
    /// Decrypted DATA addressed to one local destination.
    DataReceived {
        /// Addressed destination hash.
        destination: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved plaintext owner.
        payload: Vec<u8>,
    },
    /// A valid proof covered one sent packet.
    ProofReceived {
        /// Complete covered packet hash.
        packet_hash: [u8; 32],
    },
    /// One sent-packet receipt expired.
    ReceiptFailed {
        /// Complete expired packet hash.
        packet_hash: [u8; 32],
    },
    /// A locally owned Link became active.
    LinkEstablished {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// An authenticated LRRTT updated one active Link.
    LinkRttUpdated {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Updated round-trip time in seconds.
        rtt_seconds: f64,
    },
    /// Decrypted best-effort data arrived on an active Link.
    LinkData {
        /// Authoritative retained Link-to-destination association.
        binding: ApplicationLinkBinding,
        /// Exact moved plaintext owner.
        data: Vec<u8>,
        /// Reticulum Link context byte.
        context: u8,
    },
    /// One or more reliable Channel messages arrived on a Link.
    ChannelMessages {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved `(message_type, payload)` owners.
        messages: Vec<(u16, Vec<u8>)>,
    },
    /// A Link request arrived.
    RequestReceived {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request-path hash bytes.
        path: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved request body.
        data: Vec<u8>,
    },
    /// A Link response arrived.
    ResponseReceived {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved response body.
        data: Vec<u8>,
    },
    /// A locally owned Link closed.
    LinkClosed {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// A remote peer authenticated its identity on a Link.
    LinkIdentified {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable remote identity hash bytes.
        identity: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Remote public key. Debug output is redacted.
        public_key: [u8; 64],
    },
    /// A Resource advertisement was retained by the native core.
    ///
    /// The current embedded ingress gate rejects every Resource context, so
    /// this variant documents the future lossless surface without enabling it.
    ResourceOffered {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Advertised transfer size.
        total_size: usize,
    },
    /// Resource transfer progress.
    ResourceProgress {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Received parts.
        current: usize,
        /// Expected parts.
        total: usize,
    },
    /// A Resource transfer completed.
    ResourceComplete {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved assembled body.
        data: Vec<u8>,
    },
    /// A Resource transfer failed.
    ResourceFailed {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// A peer rejected one Resource sent by this node.
    ResourceRejected {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Truncated Resource hash.
        resource_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// A pending Link request failed.
    RequestFailed {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Typed terminal reason.
        reason: ApplicationRequestFailReason,
    },
    /// Response-as-Resource progress for a pending Link request.
    RequestProgress {
        /// Stable Link identifier bytes.
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Received parts.
        current: usize,
        /// Expected parts.
        total: usize,
    },
    /// Periodic protocol maintenance completed.
    Tick {
        /// Paths expired by this tick.
        expired_paths: usize,
        /// Links closed by establishment or stale-session maintenance.
        closed_links: usize,
    },
}

impl ApplicationEvent {
    /// Classify this event without inspecting or formatting owned payloads.
    ///
    /// This match intentionally has no wildcard. Adding a project event must
    /// therefore define its explicit logging and discard-policy classification.
    pub fn kind(&self) -> ApplicationEventKind {
        match self {
            Self::AnnounceReceived { .. } => ApplicationEventKind::AnnounceReceived,
            Self::DataReceived { .. } => ApplicationEventKind::DataReceived,
            Self::ProofReceived { .. } => ApplicationEventKind::ProofReceived,
            Self::ReceiptFailed { .. } => ApplicationEventKind::ReceiptFailed,
            Self::LinkEstablished { .. } => ApplicationEventKind::LinkEstablished,
            Self::LinkRttUpdated { .. } => ApplicationEventKind::LinkRttUpdated,
            Self::LinkData { .. } => ApplicationEventKind::LinkData,
            Self::ChannelMessages { .. } => ApplicationEventKind::ChannelMessages,
            Self::RequestReceived { .. } => ApplicationEventKind::RequestReceived,
            Self::ResponseReceived { .. } => ApplicationEventKind::ResponseReceived,
            Self::LinkClosed { .. } => ApplicationEventKind::LinkClosed,
            Self::LinkIdentified { .. } => ApplicationEventKind::LinkIdentified,
            Self::ResourceOffered { .. } => ApplicationEventKind::ResourceOffered,
            Self::ResourceProgress { .. } => ApplicationEventKind::ResourceProgress,
            Self::ResourceComplete { .. } => ApplicationEventKind::ResourceComplete,
            Self::ResourceFailed { .. } => ApplicationEventKind::ResourceFailed,
            Self::ResourceRejected { .. } => ApplicationEventKind::ResourceRejected,
            Self::RequestFailed { .. } => ApplicationEventKind::RequestFailed,
            Self::RequestProgress { .. } => ApplicationEventKind::RequestProgress,
            Self::Tick { .. } => ApplicationEventKind::Tick,
        }
    }
}

impl core::fmt::Debug for ApplicationEvent {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AnnounceReceived {
                destination,
                identity,
                hops,
                app_data,
            } => formatter
                .debug_struct("AnnounceReceived")
                .field("destination", destination)
                .field("identity", identity)
                .field("hops", hops)
                .field("app_data_len", &app_data.as_ref().map(Vec::len))
                .finish(),
            Self::DataReceived {
                destination,
                payload,
            } => formatter
                .debug_struct("DataReceived")
                .field("destination", destination)
                .field("payload_len", &payload.len())
                .finish(),
            Self::ProofReceived { packet_hash } => formatter
                .debug_struct("ProofReceived")
                .field("packet_hash", packet_hash)
                .finish(),
            Self::ReceiptFailed { packet_hash } => formatter
                .debug_struct("ReceiptFailed")
                .field("packet_hash", packet_hash)
                .finish(),
            Self::LinkEstablished { link } => formatter
                .debug_struct("LinkEstablished")
                .field("link", link)
                .finish(),
            Self::LinkRttUpdated { link, rtt_seconds } => formatter
                .debug_struct("LinkRttUpdated")
                .field("link", link)
                .field("rtt_seconds", rtt_seconds)
                .finish(),
            Self::LinkData {
                binding,
                data,
                context,
            } => formatter
                .debug_struct("LinkData")
                .field("binding", binding)
                .field("data_len", &data.len())
                .field("context", context)
                .finish(),
            Self::ChannelMessages { link, messages } => formatter
                .debug_struct("ChannelMessages")
                .field("link", link)
                .field("message_count", &messages.len())
                .field(
                    "payload_bytes",
                    &messages
                        .iter()
                        .map(|(_, payload)| payload.len())
                        .sum::<usize>(),
                )
                .finish(),
            Self::RequestReceived {
                link,
                request,
                path,
                data,
            } => formatter
                .debug_struct("RequestReceived")
                .field("link", link)
                .field("request", request)
                .field("path", path)
                .field("data_len", &data.len())
                .finish(),
            Self::ResponseReceived {
                link,
                request,
                data,
            } => formatter
                .debug_struct("ResponseReceived")
                .field("link", link)
                .field("request", request)
                .field("data_len", &data.len())
                .finish(),
            Self::LinkClosed { link } => formatter
                .debug_struct("LinkClosed")
                .field("link", link)
                .finish(),
            Self::LinkIdentified { link, identity, .. } => formatter
                .debug_struct("LinkIdentified")
                .field("link", link)
                .field("identity", identity)
                .field("public_key", &"[redacted; 64 bytes]")
                .finish(),
            Self::ResourceOffered {
                link,
                resource_hash,
                total_size,
            } => formatter
                .debug_struct("ResourceOffered")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .field("total_size", total_size)
                .finish(),
            Self::ResourceProgress {
                link,
                resource_hash,
                current,
                total,
            } => formatter
                .debug_struct("ResourceProgress")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .field("current", current)
                .field("total", total)
                .finish(),
            Self::ResourceComplete {
                link,
                resource_hash,
                data,
            } => formatter
                .debug_struct("ResourceComplete")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .field("data_len", &data.len())
                .finish(),
            Self::ResourceFailed {
                link,
                resource_hash,
            } => formatter
                .debug_struct("ResourceFailed")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .finish(),
            Self::ResourceRejected {
                link,
                resource_hash,
            } => formatter
                .debug_struct("ResourceRejected")
                .field("link", link)
                .field("resource_hash", resource_hash)
                .finish(),
            Self::RequestFailed {
                link,
                request,
                reason,
            } => formatter
                .debug_struct("RequestFailed")
                .field("link", link)
                .field("request", request)
                .field("reason", reason)
                .finish(),
            Self::RequestProgress {
                link,
                request,
                current,
                total,
            } => formatter
                .debug_struct("RequestProgress")
                .field("link", link)
                .field("request", request)
                .field("current", current)
                .field("total", total)
                .finish(),
            Self::Tick {
                expired_paths,
                closed_links,
            } => formatter
                .debug_struct("Tick")
                .field("expired_paths", expired_paths)
                .field("closed_links", closed_links)
                .finish(),
        }
    }
}

/// Consume one pinned native event into the exhaustive project-owned surface.
///
/// This match intentionally has no wildcard so a Rete update that adds an event
/// cannot compile until its ownership and redaction policy is reviewed here.
fn project_application_event(
    event: NativeNodeEvent,
    link_binding: Option<ApplicationLinkBinding>,
) -> Result<ApplicationEvent, ApplicationEventProjectionError> {
    Ok(match event {
        NativeNodeEvent::AnnounceReceived {
            dest_hash,
            identity_hash,
            hops,
            app_data,
        } => ApplicationEvent::AnnounceReceived {
            destination: *dest_hash.as_bytes(),
            identity: *identity_hash.as_bytes(),
            hops,
            app_data,
        },
        NativeNodeEvent::DataReceived { dest_hash, payload } => ApplicationEvent::DataReceived {
            destination: *dest_hash.as_bytes(),
            payload,
        },
        NativeNodeEvent::ProofReceived { packet_hash } => {
            ApplicationEvent::ProofReceived { packet_hash }
        }
        NativeNodeEvent::ReceiptFailed { packet_hash } => {
            ApplicationEvent::ReceiptFailed { packet_hash }
        }
        NativeNodeEvent::LinkEstablished { link_id } => ApplicationEvent::LinkEstablished {
            link: *link_id.as_bytes(),
        },
        NativeNodeEvent::LinkRttUpdated { link_id, rtt } => ApplicationEvent::LinkRttUpdated {
            link: *link_id.as_bytes(),
            rtt_seconds: rtt,
        },
        NativeNodeEvent::LinkData {
            link_id,
            data,
            context,
        } => ApplicationEvent::LinkData {
            binding: link_binding.ok_or(ApplicationEventProjectionError::LinkStateNotRetained {
                link: *link_id.as_bytes(),
            })?,
            data,
            context,
        },
        NativeNodeEvent::ChannelMessages { link_id, messages } => {
            ApplicationEvent::ChannelMessages {
                link: *link_id.as_bytes(),
                messages,
            }
        }
        NativeNodeEvent::RequestReceived {
            link_id,
            request_id,
            path_hash,
            data,
        } => ApplicationEvent::RequestReceived {
            link: *link_id.as_bytes(),
            request: *request_id.as_bytes(),
            path: *path_hash.as_bytes(),
            data,
        },
        NativeNodeEvent::ResponseReceived {
            link_id,
            request_id,
            data,
        } => ApplicationEvent::ResponseReceived {
            link: *link_id.as_bytes(),
            request: *request_id.as_bytes(),
            data,
        },
        NativeNodeEvent::LinkClosed { link_id } => ApplicationEvent::LinkClosed {
            link: *link_id.as_bytes(),
        },
        NativeNodeEvent::LinkIdentified {
            link_id,
            identity_hash,
            public_key,
        } => ApplicationEvent::LinkIdentified {
            link: *link_id.as_bytes(),
            identity: *identity_hash.as_bytes(),
            public_key,
        },
        NativeNodeEvent::ResourceOffered {
            link_id,
            resource_hash,
            total_size,
        } => ApplicationEvent::ResourceOffered {
            link: *link_id.as_bytes(),
            resource_hash,
            total_size,
        },
        NativeNodeEvent::ResourceProgress {
            link_id,
            resource_hash,
            current,
            total,
        } => ApplicationEvent::ResourceProgress {
            link: *link_id.as_bytes(),
            resource_hash,
            current,
            total,
        },
        NativeNodeEvent::ResourceComplete {
            link_id,
            resource_hash,
            data,
        } => ApplicationEvent::ResourceComplete {
            link: *link_id.as_bytes(),
            resource_hash,
            data,
        },
        NativeNodeEvent::ResourceFailed {
            link_id,
            resource_hash,
        } => ApplicationEvent::ResourceFailed {
            link: *link_id.as_bytes(),
            resource_hash,
        },
        NativeNodeEvent::ResourceRejected {
            link_id,
            resource_hash,
        } => ApplicationEvent::ResourceRejected {
            link: *link_id.as_bytes(),
            resource_hash,
        },
        NativeNodeEvent::RequestFailed {
            link_id,
            request_id,
            reason,
        } => ApplicationEvent::RequestFailed {
            link: *link_id.as_bytes(),
            request: *request_id.as_bytes(),
            reason: match reason {
                NativeRequestFailReason::Timeout => ApplicationRequestFailReason::Timeout,
                NativeRequestFailReason::LinkClosed => ApplicationRequestFailReason::LinkClosed,
                NativeRequestFailReason::ResourceFailed => {
                    ApplicationRequestFailReason::ResourceFailed
                }
            },
        },
        NativeNodeEvent::RequestProgress {
            link_id,
            request_id,
            current,
            total,
        } => ApplicationEvent::RequestProgress {
            link: *link_id.as_bytes(),
            request: *request_id.as_bytes(),
            current,
            total,
        },
        NativeNodeEvent::Tick {
            expired_paths,
            closed_links,
        } => ApplicationEvent::Tick {
            expired_paths,
            closed_links,
        },
    })
}

fn project_local_close_event(event: NativeNodeEvent) -> ApplicationEvent {
    match project_application_event(event, None) {
        Ok(event @ ApplicationEvent::LinkClosed { .. }) => event,
        Ok(event) => panic!(
            "pinned Rete close_link emitted unexpected application event {}",
            event.kind()
        ),
        Err(error) => {
            panic!("pinned Rete close_link emitted an unprojectable application event: {error:?}")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplicationEventProjectionError {
    LinkStateNotRetained {
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
}

/// Read-only application-event collection owned by one [`NodeActions`]
/// envelope.
///
/// Its vector and optional retained-proof owner move as one opaque value and
/// cannot be separated or mutated by downstream safe code. The public field
/// shape still supports existing read-only `.as_slice()`, `.len()`,
/// `.is_empty()`, `.first()`, `.iter()`, and indexing call sites.
pub struct ApplicationEvents {
    events: Vec<ApplicationEvent>,
    retained_proof: Option<RetainedApplicationProof>,
}

impl ApplicationEvents {
    fn without_retained_proofs(events: Vec<ApplicationEvent>) -> Self {
        Self {
            events,
            retained_proof: None,
        }
    }

    fn retained(events: Vec<ApplicationEvent>, retained_proof: RetainedApplicationProof) -> Self {
        Self {
            events,
            retained_proof: Some(retained_proof),
        }
    }

    pub(crate) const fn retained_proof(&self) -> Option<&RetainedApplicationProof> {
        self.retained_proof.as_ref()
    }

    pub(crate) fn into_parts(self) -> (Vec<ApplicationEvent>, Option<RetainedApplicationProof>) {
        (self.events, self.retained_proof)
    }

    fn owns_no_actions(&self) -> bool {
        let Self {
            events,
            retained_proof,
        } = self;
        events.is_empty() && retained_proof.is_none()
    }

    fn retained_proof_count(&self) -> usize {
        usize::from(self.retained_proof.is_some())
    }

    #[cfg(test)]
    pub(crate) fn clear_semantic_events_for_test(&mut self) {
        self.events.clear();
    }

    #[cfg(test)]
    pub(crate) fn replace_semantic_event_for_test(
        &mut self,
        index: usize,
        event: ApplicationEvent,
    ) {
        self.events[index] = event;
    }

    /// All transport-neutral events in original order.
    pub fn as_slice(&self) -> &[ApplicationEvent] {
        self.events.as_slice()
    }

    /// Number of transport-neutral events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether no transport-neutral event is owned.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// First event, if present.
    pub fn first(&self) -> Option<&ApplicationEvent> {
        self.events.first()
    }

    /// Event at one exact index, if present.
    pub fn get(&self, index: usize) -> Option<&ApplicationEvent> {
        self.events.get(index)
    }

    /// Iterate over immutable events in original order.
    pub fn iter(&self) -> core::slice::Iter<'_, ApplicationEvent> {
        self.events.iter()
    }

    /// Stable allocation pointer for read-only ownership-correlation tests.
    pub fn as_ptr(&self) -> *const ApplicationEvent {
        self.events.as_ptr()
    }

    /// Current event-vector capacity for read-only ownership tests.
    pub fn capacity(&self) -> usize {
        self.events.capacity()
    }
}

impl Default for ApplicationEvents {
    fn default() -> Self {
        Self::without_retained_proofs(Vec::new())
    }
}

impl core::fmt::Debug for ApplicationEvents {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ApplicationEvents")
            .field("len", &self.events.len())
            .field("retained_proof_bound", &self.retained_proof.is_some())
            .field("events", &"<redacted>")
            .finish()
    }
}

impl core::ops::Index<usize> for ApplicationEvents {
    type Output = ApplicationEvent;

    fn index(&self, index: usize) -> &Self::Output {
        &self.events[index]
    }
}

/// Application events and resolved transmission actions from one core call.
#[derive(Debug, Default)]
#[must_use = "every protocol event, packet, and unroutable-action count must be drained or retained"]
pub struct NodeActions {
    /// Exhaustive project-owned events with exact moved payload owners.
    pub events: ApplicationEvents,
    /// Packets with their interface target resolved while ingress context is
    /// still available.
    pub packets: Vec<TxPacket>,
    /// Native source-dependent actions dropped because no source interface was
    /// available. This should remain zero for timer-driven work.
    pub unroutable_packets: usize,
}

/// Scalar counts returned when a complete action envelope is intentionally
/// destroyed without routing its contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscardedNodeActionCounts {
    events: usize,
    packets: usize,
    unroutable_packets: usize,
}

impl DiscardedNodeActionCounts {
    /// Application events destroyed with the envelope.
    pub const fn events(self) -> usize {
        self.events
    }

    /// Outbound packets destroyed with the envelope, including any privately
    /// retained proof owner.
    pub const fn packets(self) -> usize {
        self.packets
    }

    /// Source-dependent actions already classified as unroutable upstream.
    pub const fn unroutable_packets(self) -> usize {
        self.unroutable_packets
    }
}

impl NodeActions {
    /// Construct an action envelope that cannot contain retained proof
    /// authority.
    pub fn without_retained_proofs(
        events: Vec<ApplicationEvent>,
        packets: Vec<TxPacket>,
        unroutable_packets: usize,
    ) -> Self {
        Self {
            events: ApplicationEvents::without_retained_proofs(events),
            packets,
            unroutable_packets,
        }
    }

    /// Whether this envelope owns no event, retained proof, packet, or
    /// unroutable-action observation.
    pub fn is_empty(&self) -> bool {
        let Self {
            events,
            packets,
            unroutable_packets,
        } = self;
        events.owns_no_actions() && packets.is_empty() && *unroutable_packets == 0
    }

    /// Number of retained proofs privately bound to events in this envelope.
    pub fn retained_proof_count(&self) -> usize {
        self.events.retained_proof_count()
    }

    /// Intentionally destroy every action and return only exhaustive scalar
    /// counts.
    ///
    /// Exact destructuring here deliberately makes a future owning field fail
    /// compilation until this destruction boundary is reviewed.
    pub fn discard(self) -> DiscardedNodeActionCounts {
        let Self {
            events,
            packets,
            unroutable_packets,
        } = self;
        let counts = DiscardedNodeActionCounts {
            events: events.len(),
            packets: packets.len().saturating_add(events.retained_proof_count()),
            unroutable_packets,
        };
        drop(events);
        drop(packets);
        counts
    }
}

/// Product-owned plaintext from one native destination-DATA event.
///
/// Projection moves the native payload allocation into this value unchanged.
/// It deliberately applies no mailbox capacity or payload-length policy: a
/// caller can therefore observe DATA for future destination types whose
/// plaintext limit differs from the current encrypted SINGLE-destination
/// limit.
#[must_use = "inbound DATA must be retained, delivered, or explicitly discarded"]
pub struct InboundData {
    destination: [u8; rete_core::TRUNCATED_HASH_LEN],
    payload: Vec<u8>,
}

impl core::fmt::Debug for InboundData {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InboundData")
            .field("destination", &self.destination)
            .field("payload_len", &self.payload.len())
            .finish_non_exhaustive()
    }
}

impl InboundData {
    /// Complete destination hash addressed by the received DATA packet.
    pub const fn destination(&self) -> &[u8; rete_core::TRUNCATED_HASH_LEN] {
        &self.destination
    }

    /// Decrypted application payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Consume this value into its destination and exact moved payload owner.
    pub fn into_parts(self) -> ([u8; rete_core::TRUNCATED_HASH_LEN], Vec<u8>) {
        (self.destination, self.payload)
    }
}

/// Result of projecting one application event onto the inbound-DATA surface.
#[must_use = "the projected DATA or unchanged non-DATA event must be handled"]
pub enum InboundDataProjection {
    /// Decrypted destination DATA with its original payload allocation.
    Data(InboundData),
    /// Any other application event, returned unchanged to its caller.
    Other(ApplicationEvent),
}

impl core::fmt::Debug for InboundDataProjection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Data(data) => formatter.debug_tuple("Data").field(data).finish(),
            Self::Other(_) => formatter.write_str("Other(..)"),
        }
    }
}

/// Consume one application event and project destination DATA without cloning.
pub fn project_inbound_data(event: ApplicationEvent) -> InboundDataProjection {
    match event {
        ApplicationEvent::DataReceived {
            destination,
            payload,
        } => InboundDataProjection::Data(InboundData {
            destination,
            payload,
        }),
        other => InboundDataProjection::Other(other),
    }
}

/// Whether this node only terminates traffic or also forwards ordinary RNS
/// traffic for other nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// Local endpoint only.
    Endpoint,
    /// Forward announces and ordinary packets. HEADER_2 relayed Link and DATA
    /// admission is transactional; arbitrary HEADER_1 remote LINKREQUEST
    /// ingress remains disabled until interface roles distinguish it from
    /// local-origin injection.
    Transport,
}

/// Delivery-proof behavior for traffic received through a local destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InboundProofPolicy {
    /// Never generate delivery proofs automatically.
    #[default]
    Never,
    /// Immediately prove successfully decrypted direct Single DATA or responder
    /// context-NONE Link DATA.
    Always,
    /// Retain the sole proof owner on either direct Single DATA or responder
    /// context-NONE Link DATA until durable application policy releases it.
    Retain,
}

impl InboundProofPolicy {
    fn into_rete(self) -> rete_stack::ProofStrategy {
        match self {
            Self::Never => rete_stack::ProofStrategy::ProveNone,
            Self::Always => rete_stack::ProofStrategy::ProveAll,
            // `ProveApp` is an inert marker because this owning adapter never
            // installs NodeHooks. Direct Single ingress temporarily arms the
            // destination as `ProveAll`; responder Link ingress constructs its
            // exact proof only after native authentication and deduplication.
            Self::Retain => rete_stack::ProofStrategy::ProveApp,
        }
    }
}

/// A destination cannot accept the requested inbound-proof policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundProofPolicyError {
    /// The destination is not registered on this node.
    DestinationNotRegistered,
    /// Retained proofs are supported only for an inbound Single destination.
    RetainRequiresInboundSingle,
}

impl core::fmt::Display for InboundProofPolicyError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DestinationNotRegistered => formatter.write_str("destination is not registered"),
            Self::RetainRequiresInboundSingle => {
                formatter.write_str("retained proofs require an inbound Single destination")
            }
        }
    }
}

/// Construction policy for the owning embedded node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedNodeConfig {
    /// Endpoint or transport behavior.
    pub role: NodeRole,
    /// Maximum additional destinations registered after the primary one.
    pub max_additional_destinations: usize,
}

impl EmbeddedNodeConfig {
    /// Conservative endpoint profile.
    pub const fn endpoint() -> Self {
        Self {
            role: NodeRole::Endpoint,
            max_additional_destinations: 4,
        }
    }

    /// Transport profile with fail-closed bounded relay admission.
    pub const fn transport() -> Self {
        Self {
            role: NodeRole::Transport,
            max_additional_destinations: 4,
        }
    }
}

impl Default for EmbeddedNodeConfig {
    fn default() -> Self {
        Self::endpoint()
    }
}

/// Why a retained application delivery proof could not be captured atomically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedProofInvariant {
    /// Native ingress did not emit exactly one DATA event for the packet.
    DataEventCount { actual: usize },
    /// The sole native DATA event named a different destination.
    DataEventDestination,
    /// Native ingress did not emit exactly one Link DATA event for the packet.
    LinkDataEventCount { actual: usize },
    /// The sole Link DATA event did not preserve the expected Link and context.
    LinkDataEventBinding,
    /// Rete could not construct the fixed responder-side Link proof.
    LinkProofBuild,
    /// Native ingress did not emit exactly one source-interface direct proof.
    ProofActionCount { actual: usize },
    /// The sole source-interface candidate was not a parseable proof packet.
    MalformedProof,
    /// The sole proof did not have the exact normal direct-proof wire shape or hash.
    MismatchedProof,
}

/// Why an ingress packet was rejected before or after native processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressDropReason {
    /// Embedded profiles accept only the base 500-byte RNS MTU.
    PacketTooLong { actual: usize, maximum: usize },
    /// Rete's zero-copy parser rejected the packet.
    Malformed(rete_core::Error),
    /// Resource receive remains disabled until it has bounded storage and
    /// backpressure semantics.
    ResourceIngressDisabled { context: u8 },
    /// A LINKREQUEST used a destination type other than SINGLE.
    LinkRequestDestinationType(DestType),
    /// A LINKREQUEST used a non-zero context.
    LinkRequestContext(u8),
    /// Python Reticulum accepts only the 64-byte base request or its 67-byte
    /// signalling form.
    LinkRequestPayloadLength(usize),
    /// The addressed destination is not an inbound SINGLE destination that
    /// currently accepts Links.
    DestinationDoesNotAcceptLinks,
    /// A new locally owned Link would exceed the configured table.
    OwnedLinkTableFull { limit: usize },
    /// A new relayed Link would exceed the configured relay table.
    RelayLinkTableFull { limit: usize },
    /// Rete reported a local Link handshake but did not retain its state.
    LinkStateNotRetained,
    /// Retained application proof construction violated the pinned native contract.
    RetainedProofInvariant(RetainedProofInvariant),
    /// A valid LINKREQUEST could not receive a unique egress timing token.
    ProtocolTokenExhausted { link_id: LinkId },
    /// A fresh egress timing token could not be bound to the admitted Link.
    ProtocolTokenAssignmentFailed { link_id: LinkId },
    /// Arbitrary remote HEADER_1 LINKREQUEST ingress remains disabled until
    /// interface roles can distinguish it from local-origin injection.
    Header1RemoteLinkRequestDisabled,
    /// A non-announce HEADER_2 packet names another transport identity.
    Header2NotAddressedToUs { transport_id: IdentityHash },
    /// Forwarding would require reverse state that cannot be retained.
    ReverseTableFull {
        limit: usize,
        truncated_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
    /// A truncated packet hash is already bound to a different reverse route.
    ReverseRouteConflict {
        truncated_hash: [u8; rete_core::TRUNCATED_HASH_LEN],
    },
}

impl core::fmt::Display for IngressDropReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PacketTooLong { actual, maximum } => {
                write!(f, "packet length {actual} exceeds embedded MTU {maximum}")
            }
            Self::Malformed(error) => write!(f, "malformed Reticulum packet: {error}"),
            Self::ResourceIngressDisabled { context } => {
                write!(f, "Resource ingress context {context:#04x} is disabled")
            }
            Self::LinkRequestDestinationType(dest_type) => {
                write!(
                    f,
                    "LINKREQUEST destination type is {dest_type:?}, not Single"
                )
            }
            Self::LinkRequestContext(context) => {
                write!(f, "LINKREQUEST context is {context:#04x}, not zero")
            }
            Self::LinkRequestPayloadLength(length) => {
                write!(f, "LINKREQUEST payload length {length} is not 64 or 67")
            }
            Self::DestinationDoesNotAcceptLinks => {
                write!(f, "destination does not accept inbound Links")
            }
            Self::OwnedLinkTableFull { limit } => {
                write!(f, "owned Link table is full (limit {limit})")
            }
            Self::RelayLinkTableFull { limit } => {
                write!(f, "relay Link table is full (limit {limit})")
            }
            Self::LinkStateNotRetained => {
                write!(
                    f,
                    "Rete emitted Link handshake output without retained state"
                )
            }
            Self::RetainedProofInvariant(invariant) => {
                write!(
                    f,
                    "retained application proof invariant failed: {invariant:?}"
                )
            }
            Self::ProtocolTokenExhausted { link_id } => {
                write!(f, "protocol timing token exhausted for Link {link_id:02x?}")
            }
            Self::ProtocolTokenAssignmentFailed { link_id } => {
                write!(
                    f,
                    "protocol timing token assignment failed for Link {link_id:02x?}"
                )
            }
            Self::Header1RemoteLinkRequestDisabled => {
                write!(
                    f,
                    "remote HEADER_1 LINKREQUEST requires an explicit ingress role"
                )
            }
            Self::Header2NotAddressedToUs { transport_id } => {
                write!(f, "HEADER_2 packet targets transport {transport_id:?}")
            }
            Self::ReverseTableFull { limit, .. } => {
                write!(f, "reverse-routing table is full (limit {limit})")
            }
            Self::ReverseRouteConflict { truncated_hash } => {
                write!(f, "reverse route conflicts for hash {truncated_hash:02x?}")
            }
        }
    }
}

/// Observable result classification for one admitted or rejected packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressDisposition {
    /// Native processing produced an event or transmission action.
    Processed,
    /// Native deduplication rejected a packet already in its rolling window.
    NativeDuplicate,
    /// Native transport counters identify an invalid packet.
    NativeInvalid,
    /// Native processing produced no observable action and no classifiable
    /// counter. This preserves Rete's current opaque-drop limitation.
    NoObservableOutcome,
    /// The owning boundary rejected the packet fail-closed.
    Rejected(IngressDropReason),
}

/// Complete result of one synchronous ingress call.
#[derive(Debug)]
pub struct IngressReport {
    /// Processing or rejection classification.
    pub disposition: IngressDisposition,
    /// Allocation-free wire/action/receipt correlation metadata.
    pub metadata: IngressMetadata,
    /// Events and already-resolved transmission actions.
    pub actions: NodeActions,
}

/// Small allocation-free classification metadata for one synchronous ingress
/// call.
///
/// This deliberately carries no complete hashes or payloads on the default
/// product path. It preserves the minimum scalar evidence needed to
/// distinguish an inbound PROOF, proof generation, and a committed receipt
/// terminal after the allocation-backed actions have moved into their next
/// owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressMetadata {
    wire_packet_type: Option<PacketType>,
    emitted_packets: u16,
    generated_proof_actions: u16,
    delivered_receipt_terminals: u16,
    timed_out_receipt_terminals: u16,
    generated_proof_tag: u64,
    delivered_receipt_tag: u64,
    generated_proof_tag_present: bool,
    delivered_receipt_tag_present: bool,
    generated_proof_tags_consistent: bool,
    delivered_receipt_tags_consistent: bool,
    counts_saturated: bool,
}

// Keep the always-present product-path evidence compact on both the 32-bit
// firmware target and ordinary 64-bit host builds. This ceiling deliberately
// allows layout padding without pinning a target-specific exact ABI.
const _: () = assert!(core::mem::size_of::<IngressMetadata>() <= 40);

// The report's non-action state must remain bounded independently of the
// pointer width and allocation-backed `NodeActions` representation.
const _: () =
    assert!(core::mem::size_of::<IngressReport>() <= core::mem::size_of::<NodeActions>() + 64);

impl IngressMetadata {
    fn parsed(packet: &Packet<'_>) -> Self {
        Self {
            wire_packet_type: Some(packet.packet_type),
            ..Self::default()
        }
    }

    /// Packet type decoded from the admitted wire packet, when parsing reached
    /// the common Reticulum header.
    pub const fn wire_packet_type(self) -> Option<PacketType> {
        self.wire_packet_type
    }

    /// Number of transmission actions returned by this ingress call.
    pub const fn emitted_packets(self) -> u16 {
        self.emitted_packets
    }

    /// Number of locally generated explicit delivery-PROOF actions returned to the source interface.
    pub const fn generated_proof_actions(self) -> u16 {
        self.generated_proof_actions
    }

    /// Number of delivered receipt terminals committed for this packet.
    pub const fn delivered_receipt_terminals(self) -> u16 {
        self.delivered_receipt_terminals
    }

    /// Number of timed-out receipt terminals committed for this packet.
    ///
    /// Ingress is not expected to commit timeouts; exposing the count keeps a
    /// future violation distinct from delivery-proof success.
    pub const fn timed_out_receipt_terminals(self) -> u16 {
        self.timed_out_receipt_terminals
    }

    /// Compact diagnostic tag from the covered packet hash in the first
    /// generated explicit PROOF payload.
    ///
    /// This is the first eight covered-hash bytes interpreted little-endian.
    /// It is a correlation aid, not a cryptographic or globally unique
    /// identifier. A generated PROOF with no explicit covered hash is still
    /// counted but produces no tag.
    pub const fn generated_proof_tag(self) -> Option<u64> {
        if self.generated_proof_tag_present {
            Some(self.generated_proof_tag)
        } else {
            None
        }
    }

    /// Compact diagnostic tag from the first delivered receipt ID.
    ///
    /// This uses the same first-eight-byte convention as
    /// [`Self::generated_proof_tag`].
    pub const fn delivered_receipt_tag(self) -> Option<u64> {
        if self.delivered_receipt_tag_present {
            Some(self.delivered_receipt_tag)
        } else {
            None
        }
    }

    /// Whether every generated explicit PROOF covered hash produced the same tag.
    pub const fn generated_proof_tags_consistent(self) -> bool {
        self.generated_proof_tags_consistent
    }

    /// Whether every delivered receipt produced the same tag.
    pub const fn delivered_receipt_tags_consistent(self) -> bool {
        self.delivered_receipt_tags_consistent
    }

    /// Whether any scalar action or terminal count exceeded `u16`.
    pub const fn counts_saturated(self) -> bool {
        self.counts_saturated
    }
}

impl Default for IngressMetadata {
    fn default() -> Self {
        Self {
            wire_packet_type: None,
            emitted_packets: 0,
            generated_proof_actions: 0,
            delivered_receipt_terminals: 0,
            timed_out_receipt_terminals: 0,
            generated_proof_tag: 0,
            delivered_receipt_tag: 0,
            generated_proof_tag_present: false,
            delivered_receipt_tag_present: false,
            generated_proof_tags_consistent: true,
            delivered_receipt_tags_consistent: true,
            counts_saturated: false,
        }
    }
}

/// Monotonic counters owned by the firmware adapter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngressCounters {
    pub seen: u64,
    pub admitted: u64,
    pub rejected: u64,
    pub packet_too_long: u64,
    pub malformed: u64,
    pub resource_disabled: u64,
    pub invalid_link_request: u64,
    pub owned_link_full: u64,
    pub relay_link_full: u64,
    pub link_state_not_retained: u64,
    pub retained_proof_invariant: u64,
    pub protocol_token_exhausted: u64,
    pub protocol_token_assignment_failed: u64,
    pub header1_remote_link_request_disabled: u64,
    pub native_duplicate: u64,
    pub native_invalid: u64,
    pub native_no_outcome: u64,
    pub premature_link_events_suppressed: u64,
    pub routing_invariant_drops: u64,
    pub header2_filtered: u64,
    pub reverse_table_full: u64,
    pub reverse_route_conflict: u64,
    pub endpoint_forward_suppressed: u64,
    /// Registered local path requests answered on their ingress interface.
    pub local_path_responses: u64,
    /// Local path requests consumed after response construction failed.
    pub local_path_response_failures: u64,
}

/// Monotonic counters for locally requested state and transmission admission.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionCounters {
    pub destination_limit: u64,
    pub announce_queue_full: u64,
    pub announce_native_rejected: u64,
    pub outbound_link_full: u64,
    pub outbound_link_collision: u64,
    pub outbound_link_not_retained: u64,
    pub outbound_protocol_token_exhausted: u64,
    pub outbound_protocol_token_assignment_failed: u64,
    pub receipt_table_full: u64,
    pub link_data_receipt_table_full: u64,
    pub channel_receipt_table_full: u64,
    pub channel_payload_too_large: u64,
    pub data_payload_too_large: u64,
    pub link_payload_too_large: u64,
}

/// Monotonic counters for the bounded receipt-terminal handoff.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptTerminalCounters {
    /// Proof or timeout processing deferred because the sink could not reserve
    /// an infallible terminal slot.
    pub reservation_backpressure: u64,
}

/// Allocation-free copy of Rete's cumulative transport counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TransportCounters {
    pub packets_received: u64,
    /// Native Rete counter. At the pinned revision this covers announce-queue
    /// output but omits several other packet-building paths; do not use it as
    /// an interface transmission total.
    pub packets_sent: u64,
    pub packets_forwarded: u64,
    pub packets_dropped_dedup: u64,
    pub packets_dropped_invalid: u64,
    pub announces_received: u64,
    pub announces_sent: u64,
    pub announces_retransmitted: u64,
    pub announces_rate_limited: u64,
    pub links_established: u64,
    pub links_closed: u64,
    pub links_failed: u64,
    pub link_requests_received: u64,
    pub paths_learned: u64,
    pub paths_expired: u64,
    pub crypto_failures: u64,
    pub started_at: u64,
}

impl From<&rete_transport::TransportStats> for TransportCounters {
    fn from(value: &rete_transport::TransportStats) -> Self {
        Self {
            packets_received: value.packets_received,
            packets_sent: value.packets_sent,
            packets_forwarded: value.packets_forwarded,
            packets_dropped_dedup: value.packets_dropped_dedup,
            packets_dropped_invalid: value.packets_dropped_invalid,
            announces_received: value.announces_received,
            announces_sent: value.announces_sent,
            announces_retransmitted: value.announces_retransmitted,
            announces_rate_limited: value.announces_rate_limited,
            links_established: value.links_established,
            links_closed: value.links_closed,
            links_failed: value.links_failed,
            link_requests_received: value.link_requests_received,
            paths_learned: value.paths_learned,
            paths_expired: value.paths_expired,
            crypto_failures: value.crypto_failures,
            started_at: value.started_at,
        }
    }
}

/// Read-only route information without exposing Rete's mutable transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteSnapshot {
    pub via: Option<IdentityHash>,
    pub hops: u8,
    pub received_on: Option<InterfaceId>,
}

/// Combined adapter and native telemetry snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedNodeMetrics {
    pub ingress: IngressCounters,
    pub admission: AdmissionCounters,
    pub receipt_terminals: ReceiptTerminalCounters,
    pub transport: TransportCounters,
    pub capacity: HeaplessCapacitySnapshot,
}

/// Result of timer maintenance using a bounded receipt-terminal sink.
#[derive(Debug)]
#[must_use = "receipt terminal notifications may have been deferred"]
pub struct ReceiptTickReport {
    /// Ordinary events and resolved outbound packets produced by maintenance.
    pub actions: NodeActions,
    /// Number of destination-DATA timeouts committed to the supplied sink.
    pub timed_out_receipts: usize,
    /// Number of Link-DATA timeouts or Link-close failures committed to the sink.
    pub timed_out_link_data_receipts: usize,
    /// Compact diagnostic tag from the first timed-out receipt ID.
    ///
    /// This is the first eight hash bytes interpreted little-endian. It is a
    /// bounded correlation aid, not a cryptographic or globally unique ID.
    pub timed_out_receipt_tag: Option<u64>,
    /// Whether every timeout committed in this pass carried the same compact
    /// receipt tag.
    pub timed_out_receipt_tags_consistent: bool,
    /// At least one expired receipt remains pending because no slot was
    /// available. The caller must retry maintenance after draining the sink.
    pub receipt_terminals_deferred: bool,
}

/// Failure to queue a local announce through the owning runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnounceAdmissionError {
    AppDataTooLarge { actual: usize, maximum: usize },
    QueueFull { limit: usize },
    NativeRejected,
}

impl core::fmt::Display for AnnounceAdmissionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AppDataTooLarge { actual, maximum } => {
                write!(f, "announce app data length {actual} exceeds {maximum}")
            }
            Self::QueueFull { limit } => write!(f, "announce queue is full (limit {limit})"),
            Self::NativeRejected => write!(f, "Rete rejected announce construction or queueing"),
        }
    }
}

/// Failure to configure the default application data used by local path
/// responses for one registered destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceAppDataError {
    /// The named destination is not registered on this node.
    DestinationNotRegistered,
    /// Application data cannot fit in one base-MTU announce.
    AppDataTooLarge { actual: usize, maximum: usize },
    /// Owned default application-data storage could not be reserved.
    AllocationFailed,
}

impl core::fmt::Display for AnnounceAppDataError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DestinationNotRegistered => f.write_str("destination is not registered"),
            Self::AppDataTooLarge { actual, maximum } => {
                write!(f, "announce app data length {actual} exceeds {maximum}")
            }
            Self::AllocationFailed => f.write_str("announce app data allocation failed"),
        }
    }
}

/// Failure to construct one tagged Reticulum path request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRequestBuildError {
    /// The fixed protocol request did not fit the base-MTU packet buffer.
    PacketBuild,
    /// Owned packet storage could not be reserved.
    AllocationFailed,
}

impl core::fmt::Display for PathRequestBuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PacketBuild => f.write_str("Reticulum path request construction failed"),
            Self::AllocationFailed => f.write_str("Reticulum path request allocation failed"),
        }
    }
}

/// Failure to register another local destination through the owning adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationRegistrationError {
    LimitReached { limit: usize },
    Rete(rete_core::Error),
}

impl core::fmt::Display for DestinationRegistrationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LimitReached { limit } => {
                write!(f, "additional destination limit reached ({limit})")
            }
            Self::Rete(error) => write!(f, "Rete destination registration failed: {error}"),
        }
    }
}

impl From<rete_core::Error> for DestinationRegistrationError {
    fn from(value: rete_core::Error) -> Self {
        Self::Rete(value)
    }
}

/// Failure to compose one basic, unstamped, empty-fields LXMF message with the
/// node-owned identity.
///
/// This deliberately flattens the pinned packer's error vocabulary so the
/// firmware-facing transaction neither exposes Rete types nor private keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareBasicLxmfError {
    /// Unix time zero is not a useful LXMF message timestamp.
    InvalidTimestamp,
    /// Whole-millisecond input exceeds the injective binary64 subset.
    TimestampTooLarge { actual: u64, maximum: u64 },
    /// This node has not registered its inbound Single `lxmf.delivery`
    /// destination, so it has no designated LXMF signing source.
    DeliveryDestinationUnavailable,
    /// Rete could not sign the canonical four-item LXMF payload.
    Signing,
    /// The canonical MessagePack payload exceeds the selected single-packet
    /// carrier under Python LXMF 1.0.1 selection rules.
    PayloadTooLarge { actual: usize, maximum: usize },
    /// Caller-owned carrier storage cannot hold the prepared basic message.
    OutputTooSmall { required: usize, available: usize },
    /// The pinned basic LXMF packer violated its reviewed wire invariants.
    Invariant,
}

impl core::fmt::Display for PrepareBasicLxmfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidTimestamp => f.write_str("LXMF timestamp must be nonzero Unix time"),
            Self::TimestampTooLarge { actual, maximum } => write!(
                f,
                "LXMF timestamp {actual}ms exceeds supported maximum {maximum}ms"
            ),
            Self::DeliveryDestinationUnavailable => {
                f.write_str("inbound Single lxmf.delivery destination is not registered")
            }
            Self::Signing => f.write_str("LXMF basic-message signing failed"),
            Self::PayloadTooLarge { actual, maximum } => {
                write!(
                    f,
                    "LXMF content size {actual} exceeds selected packet limit {maximum}"
                )
            }
            Self::OutputTooSmall {
                required,
                available,
            } => write!(
                f,
                "LXMF message requires {required} bytes but output holds {available}"
            ),
            Self::Invariant => f.write_str("LXMF basic-message packer invariant failed"),
        }
    }
}

/// Failure to wrap a prepared basic LXMF carrier in destination DATA.
///
/// This separate vocabulary keeps the ordinary 383-byte DATA API unchanged
/// while making the LXMF-only Header-1 exception explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareOpportunisticLxmfDataError {
    /// A rehydrated complete wire message is malformed or its signature is not
    /// valid for this node's identity.
    InvalidCompleteWire,
    /// The supplied carrier length differs from the composed carrier length.
    CarrierLengthMismatch { actual: usize, expected: usize },
    /// The supplied carrier bytes differ from the exact composed carrier.
    CarrierDigestMismatch,
    /// The carrier does not begin with this node's registered `lxmf.delivery`
    /// source hash.
    CarrierSourceMismatch,
    /// A Header-2 route cannot carry plaintext above the ordinary DATA MDU.
    Header2PayloadTooLarge { actual: usize, maximum: usize },
    /// Ordinary destination-DATA preparation failed.
    Data(PrepareDataError),
}

impl core::fmt::Display for PrepareOpportunisticLxmfDataError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidCompleteWire => {
                f.write_str("complete LXMF wire is malformed or has an invalid local signature")
            }
            Self::CarrierLengthMismatch { actual, expected } => write!(
                f,
                "LXMF carrier length {actual} does not match prepared length {expected}"
            ),
            Self::CarrierDigestMismatch => {
                f.write_str("LXMF carrier bytes do not match the prepared carrier")
            }
            Self::CarrierSourceMismatch => {
                f.write_str("LXMF carrier source is not this node's lxmf.delivery destination")
            }
            Self::Header2PayloadTooLarge { actual, maximum } => write!(
                f,
                "LXMF carrier length {actual} exceeds Header-2 DATA limit {maximum}"
            ),
            Self::Data(error) => write!(f, "LXMF destination DATA preparation failed: {error}"),
        }
    }
}

impl From<PrepareDataError> for PrepareOpportunisticLxmfDataError {
    fn from(value: PrepareDataError) -> Self {
        Self::Data(value)
    }
}

/// Failure to transactionally wrap an exact basic LXMF wire message in Link DATA.
///
/// Composer-token and Link-binding failures are checked before encryption
/// consumes entropy or native receipt state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareDirectLxmfLinkDataError {
    /// The complete durable LXMF wire is malformed or does not verify against
    /// this node's local signing identity.
    InvalidCompleteWire,
    /// The supplied wire length differs from the composer-produced length.
    WireLengthMismatch { actual: usize, expected: usize },
    /// The supplied wire bytes differ from the exact composer-produced bytes.
    WireDigestMismatch,
    /// The wire destination prefix differs from the prepared destination.
    WireDestinationMismatch,
    /// The wire source is not this node's registered `lxmf.delivery` destination.
    WireSourceMismatch,
    /// The selected Link was established to a different destination.
    LinkDestinationMismatch,
    /// The selected Link is not retained.
    LinkNotFound,
    /// The selected Link is not active.
    LinkNotActive,
    /// The selected Link has no authenticated interface binding.
    LinkInterfaceUnknown,
    /// The complete LXMF wire exceeds this Link's negotiated plaintext MDU.
    LinkMduExceeded { actual: usize, maximum: usize },
    /// The bounded Link-DATA receipt table cannot retain another attempt.
    ReceiptTableFull { limit: usize },
    /// A still-live Link-DATA receipt already uses the generated packet hash.
    ReceiptHashAlreadyTracked,
    /// Link encryption failed.
    Crypto,
    /// Rete could not build or parse the base-MTU Link packet.
    PacketBuild,
    /// Rete returned an error class impossible for this operation.
    Invariant,
}

impl core::fmt::Display for PrepareDirectLxmfLinkDataError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidCompleteWire => {
                f.write_str("complete LXMF wire is malformed or has an invalid local signature")
            }
            Self::WireLengthMismatch { actual, expected } => write!(
                f,
                "LXMF wire length {actual} does not match prepared length {expected}"
            ),
            Self::WireDigestMismatch => {
                f.write_str("LXMF wire bytes do not match the prepared message")
            }
            Self::WireDestinationMismatch => {
                f.write_str("LXMF wire destination does not match the prepared destination")
            }
            Self::WireSourceMismatch => {
                f.write_str("LXMF wire source is not this node's lxmf.delivery destination")
            }
            Self::LinkDestinationMismatch => {
                f.write_str("selected Link destination does not match the LXMF destination")
            }
            Self::LinkNotFound => f.write_str("selected Link is not retained"),
            Self::LinkNotActive => f.write_str("selected Link is not active"),
            Self::LinkInterfaceUnknown => {
                f.write_str("selected Link has no authenticated interface binding")
            }
            Self::LinkMduExceeded { actual, maximum } => {
                write!(f, "LXMF wire length {actual} exceeds Link MDU {maximum}")
            }
            Self::ReceiptTableFull { limit } => {
                write!(f, "Link DATA receipt table is full (limit {limit})")
            }
            Self::ReceiptHashAlreadyTracked => {
                f.write_str("Link DATA receipt hash is already tracked")
            }
            Self::Crypto => f.write_str("Link DATA encryption failed"),
            Self::PacketBuild => f.write_str("Link DATA packet construction failed"),
            Self::Invariant => f.write_str("unexpected native Link DATA failure"),
        }
    }
}

/// Failure to prepare destination DATA into caller-owned packet storage.
///
/// This deliberately flattens native error payloads so the firmware-facing
/// transaction does not expose Rete types as part of its public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareDataError {
    /// Plaintext cannot fit in one encrypted base-MTU DATA packet.
    PayloadTooLarge { actual: usize, maximum: usize },
    /// No cached identity exists for the destination.
    UnknownDestination,
    /// The bounded DATA receipt table cannot retain another attempt.
    ReceiptTableFull { limit: usize },
    /// A still-live receipt already uses the generated packet hash.
    ReceiptHashAlreadyTracked,
    /// Destination encryption failed.
    Crypto,
    /// Rete could not build or parse the base-MTU packet.
    PacketBuild,
    /// Rete returned an error class that is impossible for this operation.
    Invariant,
}

impl core::fmt::Display for PrepareDataError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PayloadTooLarge { actual, maximum } => {
                write!(f, "destination DATA length {actual} exceeds {maximum}")
            }
            Self::UnknownDestination => write!(f, "destination identity is unknown"),
            Self::ReceiptTableFull { limit } => {
                write!(f, "delivery receipt table is full (limit {limit})")
            }
            Self::ReceiptHashAlreadyTracked => {
                write!(f, "delivery receipt hash is already tracked")
            }
            Self::Crypto => write!(f, "destination DATA encryption failed"),
            Self::PacketBuild => write!(f, "destination DATA packet construction failed"),
            Self::Invariant => write!(f, "unexpected native destination DATA failure"),
        }
    }
}

/// Failure to emit data whose delivery state must be retained locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedSendError {
    ReceiptTableFull { limit: usize },
    ChannelReceiptTableFull { limit: usize },
    ChannelPayloadTooLarge { actual: usize, maximum: usize },
    DataPayloadTooLarge { actual: usize, maximum: usize },
    LinkPayloadTooLarge { actual: usize, maximum: usize },
    Rete(SendError),
}

impl core::fmt::Display for EmbeddedSendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ReceiptTableFull { limit } => {
                write!(f, "delivery receipt table is full (limit {limit})")
            }
            Self::ChannelReceiptTableFull { limit } => {
                write!(f, "channel receipt table is full (limit {limit})")
            }
            Self::ChannelPayloadTooLarge { actual, maximum } => {
                write!(f, "channel payload length {actual} exceeds {maximum}")
            }
            Self::DataPayloadTooLarge { actual, maximum } => {
                write!(f, "destination DATA length {actual} exceeds {maximum}")
            }
            Self::LinkPayloadTooLarge { actual, maximum } => {
                write!(f, "Link DATA length {actual} exceeds {maximum}")
            }
            Self::Rete(error) => write!(f, "Rete send failed: {error}"),
        }
    }
}

impl From<SendError> for EmbeddedSendError {
    fn from(value: SendError) -> Self {
        Self::Rete(value)
    }
}

struct IngressPreflight {
    before_duplicate: u64,
    before_invalid: u64,
    metadata: IngressMetadata,
    proof_expectation: Option<InboundProofExpectation>,
    local_path_request: Option<DestHash>,
    path_response: Option<DestHash>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetainedDestinationProofExpectation {
    destination: DestHash,
    packet_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponderLinkProofDelivery {
    Immediate,
    Retain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResponderLinkProofExpectation {
    link_id: LinkId,
    packet_hash: [u8; 32],
    delivery: ResponderLinkProofDelivery,
}

enum InboundProofExpectation {
    DestinationData(RetainedDestinationProofExpectation),
    ResponderLinkData(ResponderLinkProofExpectation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalCommitCounts {
    delivered: usize,
    timed_out: usize,
    first_delivered_tag: Option<u64>,
    delivered_tags_consistent: bool,
    first_timed_out_tag: Option<u64>,
    timed_out_tags_consistent: bool,
}

impl Default for TerminalCommitCounts {
    fn default() -> Self {
        Self {
            delivered: 0,
            timed_out: 0,
            first_delivered_tag: None,
            delivered_tags_consistent: true,
            first_timed_out_tag: None,
            timed_out_tags_consistent: true,
        }
    }
}

impl TerminalCommitCounts {
    fn record_delivered(&mut self, receipt: ReceiptId) {
        let tag = correlation_tag(receipt.as_bytes());
        if let Some(first) = self.first_delivered_tag {
            self.delivered_tags_consistent &= first == tag;
        } else {
            self.first_delivered_tag = Some(tag);
            self.delivered_tags_consistent = true;
        }
    }

    fn record_timed_out(&mut self, receipt: ReceiptId) {
        let tag = correlation_tag(receipt.as_bytes());
        if let Some(first) = self.first_timed_out_tag {
            self.timed_out_tags_consistent &= first == tag;
        } else {
            self.first_timed_out_tag = Some(tag);
            self.timed_out_tags_consistent = true;
        }
    }
}

struct NativeReceiptSink<'a, S> {
    sink: &'a mut S,
    committed: TerminalCommitCounts,
}

impl<'a, S> NativeReceiptSink<'a, S> {
    fn new(sink: &'a mut S) -> Self {
        Self {
            sink,
            committed: TerminalCommitCounts::default(),
        }
    }
}

struct NativeReceiptReservation<'a, R> {
    reservation: R,
    candidate: ReceiptCandidate,
    committed: &'a mut TerminalCommitCounts,
}

impl<S: ReceiptTerminalSink> rete_stack::ReceiptTerminalSink for NativeReceiptSink<'_, S> {
    type Reservation<'a>
        = NativeReceiptReservation<'a, S::Reservation<'a>>
    where
        Self: 'a;

    fn try_reserve(
        &mut self,
        candidate: rete_stack::ReceiptCandidate,
    ) -> Result<Self::Reservation<'_>, rete_stack::ReceiptSinkFull> {
        let candidate = ReceiptCandidate::from_native(candidate);
        let reservation = self
            .sink
            .try_reserve(candidate)
            .map_err(|ReceiptReservationUnavailable| rete_stack::ReceiptSinkFull)?;
        Ok(NativeReceiptReservation {
            reservation,
            candidate,
            committed: &mut self.committed,
        })
    }
}

impl<R: ReceiptTerminalReservation> rete_stack::ReceiptTerminalReservation
    for NativeReceiptReservation<'_, R>
{
    fn commit(self, terminal: rete_stack::ReceiptTerminal) {
        assert_eq!(
            terminal.packet_hash(),
            self.candidate.receipt().as_bytes(),
            "Rete committed a receipt terminal for a different candidate"
        );
        let terminal = match terminal {
            rete_stack::ReceiptTerminal::Delivered(_) => {
                self.committed.delivered = self.committed.delivered.saturating_add(1);
                self.committed.record_delivered(self.candidate.receipt());
                ReceiptTerminal::Delivered(self.candidate)
            }
            rete_stack::ReceiptTerminal::Failed(_) => {
                self.committed.timed_out = self.committed.timed_out.saturating_add(1);
                self.committed.record_timed_out(self.candidate.receipt());
                ReceiptTerminal::TimedOut(self.candidate)
            }
        };
        self.reservation.commit(terminal);
    }
}

/// Rete node whose mutable core and transport cannot escape into firmware.
pub struct EmbeddedNode<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
> {
    core: NodeCore<HeaplessStorage<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>>,
    role: NodeRole,
    max_additional_destinations: usize,
    additional_destinations: usize,
    ingress: IngressCounters,
    admission: AdmissionCounters,
    receipt_terminals: ReceiptTerminalCounters,
    request_dispatches: [Option<RequestDispatchEntry>; PATHS],
}

impl<const PATHS: usize, const ANNOUNCES: usize, const DEDUPLICATION: usize, const LINKS: usize>
    EmbeddedNode<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>
{
    /// Construct an owning node around a persisted or freshly generated
    /// identity. No mutable access to the inner Rete core is exposed.
    pub fn new(
        identity: Identity,
        app_name: &str,
        aspects: &[&str],
        config: EmbeddedNodeConfig,
    ) -> Result<Self, rete_core::Error> {
        let mut core = NodeCore::new(identity, app_name, aspects)?;
        if config.role == NodeRole::Transport {
            core.enable_transport();
        }
        Ok(Self {
            core,
            role: config.role,
            max_additional_destinations: config.max_additional_destinations,
            additional_destinations: 0,
            ingress: IngressCounters::default(),
            admission: AdmissionCounters::default(),
            receipt_terminals: ReceiptTerminalCounters::default(),
            request_dispatches: core::array::from_fn(|_| None),
        })
    }
    /// Primary destination hash.
    pub fn destination_hash(&self) -> DestHash {
        *self.core.dest_hash()
    }

    /// Copy a public key previously authenticated by a destination announce.
    ///
    /// This is a read-only view of the bounded native identity table. Returning
    /// the fixed-size key by value prevents an application worker from holding
    /// a borrow into the sole mutable RNS owner while it verifies a signature.
    pub fn recall_identity(&self, destination: &DestHash) -> Option<[u8; 64]> {
        self.core.transport.recall_identity(destination).copied()
    }

    /// Recall an announce-learned identity only for its exact `lxmf.delivery` hash.
    ///
    /// The destination is recomputed from the retained public key rather than
    /// trusting application metadata or asking downstream discovery code to
    /// duplicate Reticulum's identity/destination hash construction.
    pub fn recall_lxmf_delivery_identity(
        &self,
        announced_destination: &DestHash,
    ) -> Option<[u8; 64]> {
        let public_key = self.recall_identity(announced_destination)?;
        let identity_hash = rete_core::identity_hash(&public_key);
        let expected =
            rete_core::destination_hash(LXMF_DELIVERY_EXPANDED_NAME, Some(&identity_hash));
        (expected == *announced_destination).then_some(public_key)
    }

    /// Local identity hash without allocating a formatted string.
    pub fn identity_hash(&self) -> IdentityHash {
        self.core.identity().hash()
    }

    /// Configured endpoint/transport role.
    pub const fn role(&self) -> NodeRole {
        self.role
    }

    /// Set proof behavior for traffic received through the primary destination.
    pub fn set_inbound_proof_policy(&mut self, policy: InboundProofPolicy) {
        let primary = self.destination_hash();
        self.set_destination_inbound_proof_policy(&primary, policy)
            .expect("the primary destination must remain an inbound Single destination");
    }

    /// Set proof behavior for traffic received through one local destination.
    ///
    /// Retained policy is rejected unless the named destination is an inbound
    /// Single destination. Other policies preserve Rete's native
    /// registered-destination behavior.
    pub fn set_destination_inbound_proof_policy(
        &mut self,
        destination: &DestHash,
        policy: InboundProofPolicy,
    ) -> Result<(), InboundProofPolicyError> {
        let Some(native) = self.core.get_destination_mut(destination) else {
            return Err(InboundProofPolicyError::DestinationNotRegistered);
        };
        if policy == InboundProofPolicy::Retain
            && (native.direction != Direction::In || native.dest_type != DestinationType::Single)
        {
            return Err(InboundProofPolicyError::RetainRequiresInboundSingle);
        }
        native.set_proof_strategy(policy.into_rete());
        Ok(())
    }

    /// Register another destination, subject to the product-owned quota.
    pub fn register_destination(
        &mut self,
        app_name: &str,
        aspects: &[&str],
        destination_type: DestinationType,
        direction: Direction,
    ) -> Result<DestHash, DestinationRegistrationError> {
        if self.additional_destinations >= self.max_additional_destinations {
            self.admission.destination_limit = self.admission.destination_limit.saturating_add(1);
            return Err(DestinationRegistrationError::LimitReached {
                limit: self.max_additional_destinations,
            });
        }
        let hash =
            self.core
                .register_destination_typed(app_name, aspects, destination_type, direction)?;
        self.additional_destinations += 1;
        Ok(hash)
    }

    /// Configure default application data for ordinary announces and local
    /// path responses from one registered destination.
    pub fn set_destination_announce_app_data(
        &mut self,
        destination: &DestHash,
        app_data: Option<&[u8]>,
    ) -> Result<(), AnnounceAppDataError> {
        if let Some(data) = app_data
            && data.len() > crate::MAX_ANNOUNCE_APP_DATA
        {
            return Err(AnnounceAppDataError::AppDataTooLarge {
                actual: data.len(),
                maximum: crate::MAX_ANNOUNCE_APP_DATA,
            });
        }
        let owned = match app_data {
            Some(data) => {
                let mut owned = Vec::new();
                owned
                    .try_reserve_exact(data.len())
                    .map_err(|_| AnnounceAppDataError::AllocationFailed)?;
                owned.extend_from_slice(data);
                Some(owned)
            }
            None => None,
        };
        let native = self
            .core
            .get_destination_mut(destination)
            .ok_or(AnnounceAppDataError::DestinationNotRegistered)?;
        native.set_default_app_data(owned);
        Ok(())
    }

    /// Enable or disable inbound Link admission on one local destination.
    pub fn set_accepts_links(&mut self, destination: &DestHash, accepts: bool) -> bool {
        let Some(destination) = self.core.get_destination_mut(destination) else {
            return false;
        };
        destination.accepts_links = accepts;
        true
    }

    /// Register a peer identity for deterministic direct sending or tests.
    pub fn register_peer(
        &mut self,
        peer: &Identity,
        app_name: &str,
        aspects: &[&str],
        now: u64,
    ) -> Result<(), rete_core::Error> {
        self.core.register_peer(peer, app_name, aspects, now)
    }

    /// Inspect a learned route without exposing mutable Rete state.
    pub fn route(&self, destination: &DestHash) -> Option<RouteSnapshot> {
        self.core
            .transport
            .get_path(destination)
            .map(|path| RouteSnapshot {
                via: path.via,
                hops: path.hops,
                received_on: path.received_on.map(InterfaceId),
            })
    }

    /// Whether an exact destination currently has a retained route.
    pub fn has_path(&self, destination: &DestHash) -> bool {
        self.core.transport.get_path(destination).is_some()
    }

    /// Construct one tagged Reticulum path request for broadcast across every
    /// currently eligible interface.
    ///
    /// Transport nodes include their identity hash before the random request
    /// tag, matching Python Reticulum's on-wire request shape. Endpoint nodes
    /// include only the requested destination and tag.
    pub fn request_path<R: RngCore + CryptoRng>(
        &self,
        destination: &DestHash,
        rng: &mut R,
    ) -> Result<TxPacket, PathRequestBuildError> {
        let mut payload = [0_u8; TRUNCATED_HASH_LEN * 3];
        payload[..TRUNCATED_HASH_LEN].copy_from_slice(destination.as_ref());
        let tag_start = if self.role == NodeRole::Transport {
            payload[TRUNCATED_HASH_LEN..TRUNCATED_HASH_LEN * 2]
                .copy_from_slice(self.identity_hash().as_ref());
            TRUNCATED_HASH_LEN * 2
        } else {
            TRUNCATED_HASH_LEN
        };
        rng.fill_bytes(&mut payload[tag_start..tag_start + TRUNCATED_HASH_LEN]);
        let payload_length = tag_start + TRUNCATED_HASH_LEN;

        let mut raw = [0_u8; rete_core::MTU];
        let length = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Plain)
            .destination_hash(rete_transport::PATH_REQUEST_DEST.as_ref())
            .context(CONTEXT_NONE)
            .payload(&payload[..payload_length])
            .build()
            .map_err(|_| PathRequestBuildError::PacketBuild)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| PathRequestBuildError::AllocationFailed)?;
        bytes.extend_from_slice(&raw[..length]);
        Ok(TxPacket::broadcast(bytes))
    }

    /// Resolve the interface selected by the retained path for locally
    /// originated destination DATA. A peer without an observed ingress
    /// interface deliberately retains Reticulum's unknown-path broadcast
    /// fallback.
    fn destination_target(&self, destination: &DestHash) -> TxTarget {
        self.core
            .transport
            .get_path(destination)
            .and_then(|path| path.received_on)
            .map_or(TxTarget::All, |interface| {
                TxTarget::Only(InterfaceId(interface))
            })
    }

    /// Resolve the one local destination authorised to sign LXMF messages.
    ///
    /// Looking up the identity-derived hash is not sufficient on its own: the
    /// destination must also be registered with the exact inbound Single
    /// shape. This prevents the composer from becoming an arbitrary
    /// source-hash signing oracle.
    fn registered_lxmf_delivery_source(&self) -> Option<DestHash> {
        let identity_hash = self.core.identity().hash();
        let source = rete_core::destination_hash(LXMF_DELIVERY_EXPANDED_NAME, Some(&identity_hash));
        let registered = self.core.get_destination(&source)?;
        if registered.dest_type != DestinationType::Single
            || registered.direction != Direction::In
            || registered.app_name != LXMF_APPLICATION_NAME
            || registered.aspects.len() != 1
            || registered
                .aspects
                .first()
                .map(alloc::string::String::as_str)
                != Some(LXMF_DELIVERY_ASPECT)
        {
            return None;
        }
        Some(source)
    }

    /// Current state of one locally owned Link.
    pub fn link_state(&self, link_id: &LinkId) -> Option<LinkState> {
        self.core.transport.get_link(link_id).map(|link| link.state)
    }

    /// Authenticated interface currently bound to one retained Link.
    ///
    /// The binding is a read-only protocol fact. Callers must separately
    /// check Link lifecycle and current product-interface eligibility.
    pub fn link_interface(&self, link_id: &LinkId) -> Option<InterfaceId> {
        self.core.transport.link_interface(link_id).map(InterfaceId)
    }

    /// Discard one retained Link that has not reached an established state.
    ///
    /// This is the fail-closed rollback path for an outbound LINKREQUEST that
    /// cannot complete before its product-owned establishment deadline.
    /// Active and stale Links are never removed by this operation.
    pub fn abort_unestablished_link(&mut self, link_id: &LinkId) -> bool {
        self.core.transport.discard_unestablished_link(link_id)
    }

    /// Current negotiated round-trip time for one locally owned Link.
    ///
    /// The value is a read-only binary64 lifecycle snapshot in seconds. It is
    /// absent after the Link has been purged.
    pub fn link_rtt(&self, link_id: &LinkId) -> Option<f64> {
        self.core.transport.get_link(link_id).map(|link| link.rtt)
    }

    /// Inspect precise native Link timing state for host conformance only.
    #[cfg(any(test, feature = "conformance"))]
    pub fn link_snapshot_for_conformance(
        &self,
        link_id: &LinkId,
    ) -> Option<ConformanceLinkSnapshot> {
        self.core
            .transport
            .get_link(link_id)
            .map(|link| ConformanceLinkSnapshot {
                state: link.state,
                rtt: link.rtt,
                request_started_at: link.request_started_at(),
                request_time_confirmed: link.request_time_confirmed(),
                request_dispatch_interface: link.request_dispatch_interface(),
                bound_interface: link.bound_interface(),
                expected_hops: link.expected_hops(),
                last_inbound: link.last_inbound,
                last_outbound: link.last_outbound,
                last_keepalive: link.last_keepalive,
                keepalive_interval: link.keepalive_interval,
                stale_time: link.stale_time,
            })
    }

    /// Build one fresh encrypted LRRTT packet for host lifecycle conformance.
    #[cfg(any(test, feature = "conformance"))]
    pub fn build_lrrtt_for_conformance<R: RngCore + CryptoRng>(
        &self,
        link_id: &LinkId,
        encoded_plaintext: &[u8],
        rng: &mut R,
    ) -> Result<TxPacket, SendError> {
        let interface = self
            .core
            .transport
            .link_interface(link_id)
            .ok_or(SendError::LinkInterfaceUnknown)?;
        let bytes = self
            .core
            .transport
            .build_lrrtt_packet(link_id, encoded_plaintext, rng)?;
        Ok(TxPacket {
            bytes,
            target: TxTarget::Only(InterfaceId(interface)),
            protocol_token: None,
        })
    }

    /// Allocation-free metrics and capacity snapshot.
    pub fn metrics(&self) -> EmbeddedNodeMetrics {
        EmbeddedNodeMetrics {
            ingress: self.ingress,
            admission: self.admission,
            receipt_terminals: self.receipt_terminals,
            transport: self.core.transport.stats().into(),
            capacity: heapless_capacity_snapshot(&self.core),
        }
    }

    /// Process a complete base-MTU packet with fail-closed admission.
    pub fn ingest<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: u64,
        interface: InterfaceId,
        rng: &mut R,
    ) -> IngressReport {
        self.ingest_at(raw, now, MonotonicInstant::from_secs(now), interface, rng)
    }

    /// Process a complete base-MTU packet with a precise monotonic Link clock.
    ///
    /// The pinned core reuses this synchronous pre-decrypt ingress sample for
    /// liveness, RTT measurement and activation. Python samples those internal
    /// edges separately; the resulting approximation is bounded by one LRRTT
    /// handler execution.
    pub fn ingest_at<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: u64,
        link_now: MonotonicInstant,
        interface: InterfaceId,
        rng: &mut R,
    ) -> IngressReport {
        let preflight = match self.preflight_ingest(raw, interface) {
            Ok(preflight) => preflight,
            Err(report) => return report,
        };
        self.arm_retained_proof(&preflight);
        let mut native = self
            .core
            .handle_ingest_at(raw, now, link_now, interface.0, rng);
        self.restore_retained_proof(&preflight);
        self.answer_local_path_request(&mut native, &preflight, now, rng);
        self.suppress_path_response_rebroadcast(&mut native, &preflight);
        self.finish_ingest(
            native,
            preflight,
            interface,
            TerminalCommitCounts::default(),
        )
    }

    /// Process one complete base-MTU packet with allocation-atomic receipt
    /// terminal delivery.
    ///
    /// Proofs for outstanding DATA or channel receipts reserve the exact
    /// product-owned terminal candidate before Rete mutates receipt or
    /// deduplication state. If reservation is unavailable, the caller must
    /// retain and retry the packet. Invalid proofs drop their unused
    /// reservation and leave the receipt outstanding.
    pub fn ingest_with_receipt_sink<R, S>(
        &mut self,
        raw: &[u8],
        now: u64,
        interface: InterfaceId,
        rng: &mut R,
        sink: &mut S,
    ) -> Result<IngressReport, ReceiptReservationUnavailable>
    where
        R: RngCore + CryptoRng,
        S: ReceiptTerminalSink,
    {
        self.ingest_with_receipt_sink_at(
            raw,
            now,
            MonotonicInstant::from_secs(now),
            interface,
            rng,
            sink,
        )
    }

    /// Precise Link-clock variant of [`Self::ingest_with_receipt_sink`] with
    /// the same single-sample ingress semantics as [`Self::ingest_at`].
    pub fn ingest_with_receipt_sink_at<R, S>(
        &mut self,
        raw: &[u8],
        now: u64,
        link_now: MonotonicInstant,
        interface: InterfaceId,
        rng: &mut R,
        sink: &mut S,
    ) -> Result<IngressReport, ReceiptReservationUnavailable>
    where
        R: RngCore + CryptoRng,
        S: ReceiptTerminalSink,
    {
        let preflight = match self.preflight_ingest(raw, interface) {
            Ok(preflight) => preflight,
            Err(report) => return Ok(report),
        };
        let mut native_sink = NativeReceiptSink::new(sink);
        self.arm_retained_proof(&preflight);
        let native = self.core.handle_ingest_with_receipt_sink_at(
            raw,
            now,
            link_now,
            interface.0,
            rng,
            &mut native_sink,
        );
        self.restore_retained_proof(&preflight);
        let mut native = match native {
            Ok(native) => native,
            Err(rete_stack::ReceiptSinkFull) => {
                self.receipt_terminals.reservation_backpressure = self
                    .receipt_terminals
                    .reservation_backpressure
                    .saturating_add(1);
                return Err(ReceiptReservationUnavailable);
            }
        };
        self.answer_local_path_request(&mut native, &preflight, now, rng);
        self.suppress_path_response_rebroadcast(&mut native, &preflight);
        Ok(self.finish_ingest(native, preflight, interface, native_sink.committed))
    }

    #[allow(
        clippy::result_large_err,
        reason = "the synchronous owning failure must return the exact allocation-backed ingress report"
    )]
    fn preflight_ingest(
        &mut self,
        raw: &[u8],
        interface: InterfaceId,
    ) -> Result<IngressPreflight, IngressReport> {
        self.ingress.seen = self.ingress.seen.saturating_add(1);

        if raw.len() > rete_core::MTU {
            return Err(self.reject(
                IngressDropReason::PacketTooLong {
                    actual: raw.len(),
                    maximum: rete_core::MTU,
                },
                IngressMetadata::default(),
            ));
        }

        let packet = match Packet::parse(raw) {
            Ok(packet) => packet,
            Err(error) => {
                return Err(self.reject(
                    IngressDropReason::Malformed(error),
                    IngressMetadata::default(),
                ));
            }
        };
        let metadata = IngressMetadata::parsed(&packet);

        if let Err(reason) = self.preflight_header2(&packet) {
            return Err(self.reject(reason, metadata));
        }

        if packet.dest_type == DestType::Link && is_resource_context(packet.context) {
            return Err(self.reject(
                IngressDropReason::ResourceIngressDisabled {
                    context: packet.context,
                },
                metadata,
            ));
        }

        if let Err(reason) = self.preflight_h1_reverse_admission(&packet, interface) {
            return Err(self.reject(reason, metadata));
        }

        if packet.packet_type == PacketType::LinkRequest {
            match self.preflight_link_request(&packet, raw) {
                Ok(()) => {}
                Err(reason) => return Err(self.reject(reason, metadata)),
            }
        }

        let proof_expectation = match self.preflight_inbound_proof(&packet, interface) {
            Ok(proof_expectation) => proof_expectation,
            Err(invariant) => {
                return Err(self.reject(
                    IngressDropReason::RetainedProofInvariant(invariant),
                    metadata,
                ));
            }
        };

        Ok(IngressPreflight {
            before_duplicate: self.core.transport.stats().packets_dropped_dedup,
            before_invalid: self.core.transport.stats().packets_dropped_invalid,
            metadata,
            proof_expectation,
            local_path_request: self.local_path_request(&packet),
            path_response: (packet.header_type == HeaderType::Header1
                && packet.packet_type == PacketType::Announce
                && packet.dest_type == DestType::Single
                && packet.context == CONTEXT_PATH_RESPONSE)
                .then(|| DestHash::from_slice(packet.destination_hash)),
        })
    }

    fn preflight_inbound_proof(
        &self,
        packet: &Packet<'_>,
        interface: InterfaceId,
    ) -> Result<Option<InboundProofExpectation>, RetainedProofInvariant> {
        if packet.header_type != HeaderType::Header1 || packet.packet_type != PacketType::Data {
            return Ok(None);
        }

        if packet.dest_type == DestType::Single {
            let destination = DestHash::from_slice(packet.destination_hash);
            return Ok(self
                .core
                .get_destination(&destination)
                .is_some_and(|native| {
                    native.direction == Direction::In
                        && native.dest_type == DestinationType::Single
                        && native.proof_strategy == rete_stack::ProofStrategy::ProveApp
                })
                .then(|| {
                    InboundProofExpectation::DestinationData(RetainedDestinationProofExpectation {
                        destination,
                        packet_hash: packet.compute_hash(),
                    })
                }));
        }

        if packet.dest_type != DestType::Link || packet.context != CONTEXT_NONE {
            return Ok(None);
        }

        let link_id = LinkId::from_slice(packet.destination_hash);
        let Some(link) = self.core.transport.get_link(&link_id) else {
            return Ok(None);
        };
        let Some(bound_interface) = link.bound_interface() else {
            return Ok(None);
        };
        if link.role != LinkRole::Responder
            || !matches!(link.state, LinkState::Active | LinkState::Stale)
            || bound_interface != interface.0
        {
            return Ok(None);
        }
        let destination = link.destination_hash;
        let Some(destination_state) = self.core.get_destination(&destination) else {
            return Ok(None);
        };
        if destination_state.direction != Direction::In
            || destination_state.dest_type != DestinationType::Single
        {
            return Ok(None);
        }

        let delivery = match destination_state.proof_strategy {
            rete_stack::ProofStrategy::ProveNone => return Ok(None),
            rete_stack::ProofStrategy::ProveAll => ResponderLinkProofDelivery::Immediate,
            rete_stack::ProofStrategy::ProveApp => ResponderLinkProofDelivery::Retain,
        };

        Ok(Some(InboundProofExpectation::ResponderLinkData(
            ResponderLinkProofExpectation {
                link_id,
                packet_hash: packet.compute_hash(),
                delivery,
            },
        )))
    }

    fn local_path_request(&self, packet: &Packet<'_>) -> Option<DestHash> {
        if self.role != NodeRole::Transport
            || packet.header_type != HeaderType::Header1
            || packet.packet_type != PacketType::Data
            || packet.dest_type != DestType::Plain
            || packet.context != CONTEXT_NONE
            || packet.destination_hash != rete_transport::PATH_REQUEST_DEST.as_ref()
            || packet.payload.len() < TRUNCATED_HASH_LEN
        {
            return None;
        }
        let requested = DestHash::from_slice(&packet.payload[..TRUNCATED_HASH_LEN]);
        self.core
            .get_destination(&requested)
            .is_some_and(|destination| {
                destination.direction == Direction::In
                    && destination.dest_type == DestinationType::Single
            })
            .then_some(requested)
    }

    fn arm_retained_proof(&mut self, preflight: &IngressPreflight) {
        let Some(InboundProofExpectation::DestinationData(expected)) =
            preflight.proof_expectation.as_ref()
        else {
            return;
        };
        self.core
            .get_destination_mut(&expected.destination)
            .expect("preflight retained destination must remain registered")
            .set_proof_strategy(rete_stack::ProofStrategy::ProveAll);
    }

    fn restore_retained_proof(&mut self, preflight: &IngressPreflight) {
        let Some(InboundProofExpectation::DestinationData(expected)) =
            preflight.proof_expectation.as_ref()
        else {
            return;
        };
        self.core
            .get_destination_mut(&expected.destination)
            .expect("armed retained destination must remain registered")
            .set_proof_strategy(rete_stack::ProofStrategy::ProveApp);
    }

    fn answer_local_path_request<R: RngCore + CryptoRng>(
        &mut self,
        native: &mut IngestOutcome,
        preflight: &IngressPreflight,
        now: u64,
        rng: &mut R,
    ) {
        let Some(destination) = preflight.local_path_request else {
            return;
        };
        let before = native.packets.len();
        native
            .packets
            .retain(|outbound| !is_forwarded_path_request_for(&outbound.data, &destination));
        if native.packets.len() == before {
            // Native Rete's destination throttle classified this request as a
            // duplicate, so it must not produce another response.
            return;
        }

        let Some(response) = self.build_local_path_response(&destination, now, rng) else {
            self.ingress.local_path_response_failures =
                self.ingress.local_path_response_failures.saturating_add(1);
            return;
        };
        native.packets.push(OutboundPacket::new(
            response,
            PacketRouting::SourceInterface,
        ));
        self.ingress.local_path_responses = self.ingress.local_path_responses.saturating_add(1);
    }

    fn suppress_path_response_rebroadcast(
        &mut self,
        native: &mut IngestOutcome,
        preflight: &IngressPreflight,
    ) {
        let Some(destination) = preflight.path_response else {
            return;
        };
        native
            .packets
            .retain(|outbound| match Packet::parse(&outbound.data) {
                Ok(packet) => {
                    packet.packet_type != PacketType::Announce
                        || packet.dest_type != DestType::Single
                        || packet.destination_hash != destination.as_ref()
                }
                Err(_) => true,
            });

        let pending = self.core.transport.announce_count();
        for _ in 0..pending {
            let Some(announce) = self.core.transport.next_announce() else {
                break;
            };
            let is_path_response = announce.dest_hash == destination
                && !announce.local
                && Packet::parse(&announce.raw).is_ok_and(|packet| {
                    packet.packet_type == PacketType::Announce
                        && packet.dest_type == DestType::Single
                        && packet.context == CONTEXT_PATH_RESPONSE
                });
            if !is_path_response {
                let requeued = self.core.transport.queue_announce(announce);
                debug_assert!(requeued, "popped announce must fit its original queue");
            }
        }
    }

    fn build_local_path_response<R: RngCore + CryptoRng>(
        &self,
        destination: &DestHash,
        now: u64,
        rng: &mut R,
    ) -> Option<Vec<u8>> {
        let destination = self.core.get_destination(destination)?;
        if destination.direction != Direction::In
            || destination.dest_type != DestinationType::Single
        {
            return None;
        }
        let mut aspects = Vec::new();
        aspects.try_reserve_exact(destination.aspects.len()).ok()?;
        aspects.extend(destination.aspects.iter().map(|aspect| aspect.as_str()));

        let mut raw = [0_u8; rete_core::MTU];
        let length =
            Transport::<HeaplessStorage<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>>::create_announce(
                self.core.identity(),
                &destination.app_name,
                &aspects,
                destination.default_app_data.as_deref(),
                None,
                rng,
                now,
                &mut raw,
            )
            .ok()?;
        raw[2 + TRUNCATED_HASH_LEN] = CONTEXT_PATH_RESPONSE;

        let mut response = Vec::new();
        response.try_reserve_exact(length).ok()?;
        response.extend_from_slice(&raw[..length]);
        Some(response)
    }

    fn project_retained_application_event(
        &self,
        event: NativeNodeEvent,
    ) -> Result<ApplicationEvent, ApplicationEventProjectionError> {
        let link_binding = match &event {
            NativeNodeEvent::LinkData { link_id, .. } => self
                .core
                .transport
                .get_link(link_id)
                .map(ApplicationLinkBinding::from_retained_link),
            _ => None,
        };
        project_application_event(event, link_binding)
    }

    fn finish_ingest(
        &mut self,
        mut native: IngestOutcome,
        mut preflight: IngressPreflight,
        interface: InterfaceId,
        terminal_commits: TerminalCommitCounts,
    ) -> IngressReport {
        self.reclaim_native_request_terminals(&native.events);
        if let Some(rejection) = native.rejection.take() {
            let reason = match rejection {
                IngestRejection::LinkTableFull {
                    table: LinkTableKind::Owned,
                    ..
                } => IngressDropReason::OwnedLinkTableFull { limit: LINKS },
                IngestRejection::LinkTableFull {
                    table: LinkTableKind::Relay,
                    ..
                } => IngressDropReason::RelayLinkTableFull { limit: LINKS },
                IngestRejection::ReverseTableFull { truncated_hash } => {
                    IngressDropReason::ReverseTableFull {
                        limit: PATHS,
                        truncated_hash,
                    }
                }
                IngestRejection::ReverseRouteConflict { truncated_hash } => {
                    IngressDropReason::ReverseRouteConflict { truncated_hash }
                }
                IngestRejection::ProtocolTokenExhausted { link_id } => {
                    IngressDropReason::ProtocolTokenExhausted { link_id }
                }
                IngestRejection::ProtocolTokenAssignmentFailed { link_id } => {
                    IngressDropReason::ProtocolTokenAssignmentFailed { link_id }
                }
            };
            return self.reject(reason, preflight.metadata);
        }

        let unretained_link = native.events.iter().find_map(|event| {
            let link_id = match event {
                NativeNodeEvent::LinkEstablished { link_id }
                | NativeNodeEvent::LinkData { link_id, .. } => link_id,
                _ => return None,
            };
            self.core
                .transport
                .get_link(link_id)
                .is_none()
                .then_some(*link_id)
        });
        if unretained_link.is_some() {
            self.ingress.link_state_not_retained =
                self.ingress.link_state_not_retained.saturating_add(1);
            return self.reject(IngressDropReason::LinkStateNotRetained, preflight.metadata);
        }

        // Defensive invariant: the pinned Rete revision emits establishment
        // only after authenticated LRRTT activation. Retain the state check so
        // a future native regression cannot leak a premature product event.
        let before_events = native.events.len();
        native.events.retain(|event| match event {
            NativeNodeEvent::LinkEstablished { link_id } => self
                .core
                .transport
                .get_link(link_id)
                .is_some_and(|link| link.state == LinkState::Active),
            _ => true,
        });
        self.ingress.premature_link_events_suppressed = self
            .ingress
            .premature_link_events_suppressed
            .saturating_add((before_events - native.events.len()) as u64);

        if self.role == NodeRole::Endpoint {
            let before_packets = native.packets.len();
            native
                .packets
                .retain(|packet| endpoint_retains_ingress_packet(packet.routing));
            self.ingress.endpoint_forward_suppressed = self
                .ingress
                .endpoint_forward_suppressed
                .saturating_add((before_packets - native.packets.len()) as u64);
        }

        let after = self.core.transport.stats();
        let native_duplicate = after.packets_dropped_dedup > preflight.before_duplicate;
        let native_invalid = after.packets_dropped_invalid > preflight.before_invalid;
        let identity = self.core.identity();
        let proof_expectation = preflight.proof_expectation.take();
        let retained_proof = if native_duplicate || native_invalid {
            suppress_inbound_proof_actions(&mut native, proof_expectation.as_ref(), identity);
            None
        } else {
            match intercept_inbound_proof_actions::<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>(
                &mut native,
                proof_expectation,
                interface,
                identity,
            ) {
                Ok(proof) => proof,
                Err(invariant) => {
                    return self.reject(
                        IngressDropReason::RetainedProofInvariant(invariant),
                        preflight.metadata,
                    );
                }
            }
        };

        self.ingress.admitted = self.ingress.admitted.saturating_add(1);
        let disposition = if native_duplicate {
            self.ingress.native_duplicate = self.ingress.native_duplicate.saturating_add(1);
            IngressDisposition::NativeDuplicate
        } else if native_invalid {
            self.ingress.native_invalid = self.ingress.native_invalid.saturating_add(1);
            IngressDisposition::NativeInvalid
        } else if native.events.is_empty()
            && native.packets.is_empty()
            && terminal_commits.delivered == 0
            && terminal_commits.timed_out == 0
        {
            self.ingress.native_no_outcome = self.ingress.native_no_outcome.saturating_add(1);
            IngressDisposition::NoObservableOutcome
        } else {
            IngressDisposition::Processed
        };

        let emitted_packets = native.packets.len();
        let mut generated_proof_actions = 0_usize;
        let mut generated_proof_tag = None;
        let mut generated_proof_tags_consistent = true;
        for outbound in &native.packets {
            if outbound.routing != PacketRouting::SourceInterface {
                continue;
            }
            let Ok(packet) = Packet::parse(&outbound.data) else {
                continue;
            };
            // Explicit destination/channel delivery proofs use CONTEXT_NONE
            // and start with the covered packet hash. LRPROOF is also a
            // source-interface PROOF, but its payload starts with handshake
            // signature material and must not enter delivery correlation.
            if packet.packet_type != PacketType::Proof || packet.context != CONTEXT_NONE {
                continue;
            }
            generated_proof_actions = generated_proof_actions.saturating_add(1);
            let Some(covered_hash) = packet.payload.get(..32) else {
                continue;
            };
            let tag = correlation_tag(covered_hash);
            if let Some(first) = generated_proof_tag {
                generated_proof_tags_consistent &= first == tag;
            } else {
                generated_proof_tag = Some(tag);
            }
        }
        let (emitted_packets, emitted_saturated) = saturating_u16(emitted_packets);
        let (generated_proof_actions, proof_saturated) = saturating_u16(generated_proof_actions);
        let (delivered_receipt_terminals, delivered_saturated) =
            saturating_u16(terminal_commits.delivered);
        let (timed_out_receipt_terminals, timed_out_saturated) =
            saturating_u16(terminal_commits.timed_out);
        let metadata = IngressMetadata {
            emitted_packets,
            generated_proof_actions,
            delivered_receipt_terminals,
            timed_out_receipt_terminals,
            generated_proof_tag: generated_proof_tag.unwrap_or(0),
            delivered_receipt_tag: terminal_commits.first_delivered_tag.unwrap_or(0),
            generated_proof_tag_present: generated_proof_tag.is_some(),
            delivered_receipt_tag_present: terminal_commits.first_delivered_tag.is_some(),
            generated_proof_tags_consistent,
            delivered_receipt_tags_consistent: terminal_commits.delivered_tags_consistent,
            counts_saturated: emitted_saturated
                || proof_saturated
                || delivered_saturated
                || timed_out_saturated,
            ..preflight.metadata
        };

        let events = match native
            .events
            .drain(..)
            .map(|event| self.project_retained_application_event(event))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(events) => events,
            Err(ApplicationEventProjectionError::LinkStateNotRetained { .. }) => {
                self.ingress.link_state_not_retained =
                    self.ingress.link_state_not_retained.saturating_add(1);
                return self.reject(IngressDropReason::LinkStateNotRetained, metadata);
            }
        };
        IngressReport {
            disposition,
            metadata,
            actions: resolve_ingest_actions(native, events, interface, retained_proof),
        }
    }

    fn preflight_link_request(
        &mut self,
        packet: &Packet<'_>,
        raw: &[u8],
    ) -> Result<(), IngressDropReason> {
        if packet.dest_type != DestType::Single {
            return Err(IngressDropReason::LinkRequestDestinationType(
                packet.dest_type,
            ));
        }
        if packet.context != 0 {
            return Err(IngressDropReason::LinkRequestContext(packet.context));
        }
        if !matches!(packet.payload.len(), 64 | 67) {
            return Err(IngressDropReason::LinkRequestPayloadLength(
                packet.payload.len(),
            ));
        }

        let destination = DestHash::from_slice(packet.destination_hash);
        if !self.core.transport.is_local_destination(&destination) {
            return if self.role == NodeRole::Transport && packet.header_type == HeaderType::Header2
            {
                Ok(())
            } else if self.role == NodeRole::Transport {
                Err(IngressDropReason::Header1RemoteLinkRequestDisabled)
            } else {
                Err(IngressDropReason::DestinationDoesNotAcceptLinks)
            };
        }

        let accepts = self
            .core
            .get_destination(&destination)
            .is_some_and(|destination| {
                destination.direction == Direction::In
                    && destination.dest_type == DestinationType::Single
                    && destination.accepts_links
            });
        if !accepts {
            return Err(IngressDropReason::DestinationDoesNotAcceptLinks);
        }
        let link_id = rete_transport::compute_link_id(raw).map_err(IngressDropReason::Malformed)?;
        if self.core.transport.get_link(&link_id).is_none()
            && self.core.transport.link_count() >= LINKS
        {
            // Avoid responder ECDH when the product-owned table is already
            // saturated while retaining the same full-hash retry contract as
            // native transactional admission.
            let packet_hash = packet.compute_hash();
            if self.core.transport.is_duplicate(&packet_hash) {
                // Let native ingest produce its ordinary duplicate
                // disposition without responder ECDH or state mutation.
                return Ok(());
            }
            return Err(IngressDropReason::OwnedLinkTableFull { limit: LINKS });
        }
        Ok(())
    }

    fn preflight_header2(&self, packet: &Packet<'_>) -> Result<(), IngressDropReason> {
        if packet.header_type != HeaderType::Header2 {
            return Ok(());
        }

        let Some(transport_id) = packet.transport_id.map(IdentityHash::from_slice) else {
            return Err(IngressDropReason::Malformed(
                rete_core::Error::MissingField("HEADER_2 transport ID"),
            ));
        };
        let ours = self.identity_hash();

        // Python Reticulum deliberately lets HEADER_2 announces fall through
        // normal announce validation, irrespective of their transport ID.
        if packet.packet_type == PacketType::Announce {
            return Ok(());
        }

        if transport_id != ours {
            return Err(IngressDropReason::Header2NotAddressedToUs { transport_id });
        }

        // The pinned native dispatcher normalizes owned HEADER_2 local
        // traffic and transactionally admits narrow DATA/SINGLE and
        // LINKREQUEST/SINGLE relay traffic. Keep ownership filtering here for
        // stable product diagnostics, then let typed native dispatch decide.
        Ok(())
    }

    fn preflight_h1_reverse_admission(
        &mut self,
        packet: &Packet<'_>,
        interface: InterfaceId,
    ) -> Result<(), IngressDropReason> {
        if self.role != NodeRole::Transport
            || packet.header_type != HeaderType::Header1
            || packet.packet_type != PacketType::Data
            || packet.dest_type == DestType::Link
        {
            return Ok(());
        }

        let destination = DestHash::from_slice(packet.destination_hash);
        if destination == rete_transport::PATH_REQUEST_DEST
            || self.core.transport.is_local_destination(&destination)
            || self.core.transport.get_path(&destination).is_none()
        {
            return Ok(());
        }
        let packet_hash = packet.compute_hash();
        let mut key = [0u8; rete_core::TRUNCATED_HASH_LEN];
        key.copy_from_slice(&packet_hash[..rete_core::TRUNCATED_HASH_LEN]);
        let Some(outbound) = self
            .core
            .transport
            .get_path(&destination)
            .and_then(|path| path.received_on)
        else {
            // A caller-registered identity can have a direct path without a
            // learned interface. There is no usable route to reserve in that
            // case, so leave fail-closed classification to native ingest.
            return Ok(());
        };
        let reason = match self.core.transport.get_reverse(&key) {
            Some(existing)
                if existing.received_on != interface.0 || existing.forwarded_to != outbound =>
            {
                Some(IngressDropReason::ReverseRouteConflict {
                    truncated_hash: key,
                })
            }
            None if self.core.transport.reverse_count() >= PATHS => {
                Some(IngressDropReason::ReverseTableFull {
                    limit: PATHS,
                    truncated_hash: key,
                })
            }
            _ => None,
        };
        if let Some(reason) = reason {
            // Native H2 admission records the full packet hash before a typed
            // rejection. Match that retry contract for the temporary H1
            // local-origin compatibility shim. A replay is allowed into
            // native ingest solely so it is classified as a duplicate.
            if self.core.transport.is_duplicate(&packet_hash) {
                return Ok(());
            }
            return Err(reason);
        }
        Ok(())
    }

    fn reject(&mut self, reason: IngressDropReason, metadata: IngressMetadata) -> IngressReport {
        self.ingress.rejected = self.ingress.rejected.saturating_add(1);
        match &reason {
            IngressDropReason::PacketTooLong { .. } => {
                self.ingress.packet_too_long = self.ingress.packet_too_long.saturating_add(1);
            }
            IngressDropReason::Malformed(_) => {
                self.ingress.malformed = self.ingress.malformed.saturating_add(1);
            }
            IngressDropReason::ResourceIngressDisabled { .. } => {
                self.ingress.resource_disabled = self.ingress.resource_disabled.saturating_add(1);
            }
            IngressDropReason::OwnedLinkTableFull { .. } => {
                self.ingress.owned_link_full = self.ingress.owned_link_full.saturating_add(1);
            }
            IngressDropReason::RelayLinkTableFull { .. } => {
                self.ingress.relay_link_full = self.ingress.relay_link_full.saturating_add(1);
            }
            IngressDropReason::LinkStateNotRetained => {}
            IngressDropReason::RetainedProofInvariant(_) => {
                self.ingress.retained_proof_invariant =
                    self.ingress.retained_proof_invariant.saturating_add(1);
            }
            IngressDropReason::ProtocolTokenExhausted { .. } => {
                self.ingress.protocol_token_exhausted =
                    self.ingress.protocol_token_exhausted.saturating_add(1);
            }
            IngressDropReason::ProtocolTokenAssignmentFailed { .. } => {
                self.ingress.protocol_token_assignment_failed = self
                    .ingress
                    .protocol_token_assignment_failed
                    .saturating_add(1);
            }
            IngressDropReason::Header1RemoteLinkRequestDisabled => {
                self.ingress.header1_remote_link_request_disabled = self
                    .ingress
                    .header1_remote_link_request_disabled
                    .saturating_add(1);
            }
            IngressDropReason::Header2NotAddressedToUs { .. } => {
                self.ingress.header2_filtered = self.ingress.header2_filtered.saturating_add(1);
            }
            IngressDropReason::ReverseTableFull { .. } => {
                self.ingress.reverse_table_full = self.ingress.reverse_table_full.saturating_add(1);
            }
            IngressDropReason::ReverseRouteConflict { .. } => {
                self.ingress.reverse_route_conflict =
                    self.ingress.reverse_route_conflict.saturating_add(1);
            }
            IngressDropReason::LinkRequestDestinationType(_)
            | IngressDropReason::LinkRequestContext(_)
            | IngressDropReason::LinkRequestPayloadLength(_)
            | IngressDropReason::DestinationDoesNotAcceptLinks => {
                self.ingress.invalid_link_request =
                    self.ingress.invalid_link_request.saturating_add(1);
            }
        }
        IngressReport {
            disposition: IngressDisposition::Rejected(reason),
            metadata,
            actions: NodeActions::default(),
        }
    }

    /// Compose and sign one basic opportunistic LXMF carrier into caller-owned
    /// storage without exposing the node identity or accepting a caller-chosen
    /// signing source.
    ///
    /// This deliberately supports only the compatibility-proven first slice:
    /// a four-item payload containing a Unix timestamp, binary title, binary
    /// content, and an empty fields map, with no stamp. The complete LXMF
    /// destination is used for signing but omitted from `output`, matching the
    /// carrier bytes placed inside encrypted destination DATA by Python LXMF.
    /// The source is always this node's exact registered inbound Single
    /// `lxmf.delivery` destination.
    ///
    /// `timestamp_unix_ms` deliberately accepts only positive whole
    /// milliseconds through [`MAX_LXMF_TIMESTAMP_UNIX_MS`]. LXMF itself uses
    /// binary64 seconds; this narrower subset preserves distinct millisecond
    /// inputs and covers the JavaScript `Date` range.
    pub fn prepare_basic_lxmf_into(
        &self,
        destination: &DestHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        output: &mut [u8],
    ) -> Result<PreparedBasicLxmf, PrepareBasicLxmfError> {
        let composed = self.compose_basic_lxmf(destination, timestamp_unix_ms, title, content)?;
        // Python performs signed subtraction here. Its canonical empty-title,
        // empty-content payload is 15 bytes, producing content_size = -1 and
        // remaining opportunistic. Compare the untranslated payload length so
        // Rust exactly preserves that upper-bound decision without unsigned
        // underflow or inventing a different reported size.
        if composed.payload_len > LXMF_SELECTION_OVERHEAD + MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE {
            let content_size = composed.payload_len - LXMF_SELECTION_OVERHEAD;
            return Err(PrepareBasicLxmfError::PayloadTooLarge {
                actual: content_size,
                maximum: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE,
            });
        }
        if composed.packed.get(..LXMF_DESTINATION_HASH_LENGTH) != Some(destination.as_ref()) {
            return Err(PrepareBasicLxmfError::Invariant);
        }
        let carrier = composed
            .packed
            .get(LXMF_DESTINATION_HASH_LENGTH..)
            .ok_or(PrepareBasicLxmfError::Invariant)?;
        if carrier.len() > MAX_OPPORTUNISTIC_LXMF_CARRIER {
            return Err(PrepareBasicLxmfError::Invariant);
        }
        let output_len = output.len();
        let carrier_output =
            output
                .get_mut(..carrier.len())
                .ok_or(PrepareBasicLxmfError::OutputTooSmall {
                    required: carrier.len(),
                    available: output_len,
                })?;
        carrier_output.copy_from_slice(carrier);
        Ok(PreparedBasicLxmf {
            carrier_len: carrier.len() as u16,
            carrier_sha256: Sha256::digest(carrier).into(),
            message_id: composed.message_id,
            destination: *destination,
        })
    }

    /// Compose and sign one complete basic LXMF message for ordinary Link DATA.
    ///
    /// Unlike [`Self::prepare_basic_lxmf_into`], this writes the destination
    /// prefix because a Link packet does not imply the final LXMF destination.
    /// The exact Python 1.0.1 319-byte direct-content boundary fits in one
    /// 431-byte Link MDU. A larger message requires an RNS Resource and is
    /// rejected without modifying `output`.
    pub fn prepare_basic_direct_lxmf_into(
        &self,
        destination: &DestHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        output: &mut [u8],
    ) -> Result<PreparedBasicDirectLxmf, PrepareBasicLxmfError> {
        let composed = self.compose_basic_lxmf(destination, timestamp_unix_ms, title, content)?;
        if composed.payload_len > LXMF_SELECTION_OVERHEAD + MAX_DIRECT_LXMF_CONTENT_SIZE {
            let content_size = composed.payload_len - LXMF_SELECTION_OVERHEAD;
            return Err(PrepareBasicLxmfError::PayloadTooLarge {
                actual: content_size,
                maximum: MAX_DIRECT_LXMF_CONTENT_SIZE,
            });
        }
        if composed.packed.len() > MAX_DIRECT_LXMF_WIRE
            || composed.packed.get(..LXMF_DESTINATION_HASH_LENGTH) != Some(destination.as_ref())
        {
            return Err(PrepareBasicLxmfError::Invariant);
        }
        let output_len = output.len();
        let wire_output = output.get_mut(..composed.packed.len()).ok_or(
            PrepareBasicLxmfError::OutputTooSmall {
                required: composed.packed.len(),
                available: output_len,
            },
        )?;
        wire_output.copy_from_slice(&composed.packed);
        Ok(PreparedBasicDirectLxmf {
            wire_len: composed.packed.len() as u16,
            wire_sha256: Sha256::digest(&composed.packed).into(),
            message_id: composed.message_id,
            destination: *destination,
        })
    }

    fn compose_basic_lxmf(
        &self,
        destination: &DestHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
    ) -> Result<ComposedBasicLxmf, PrepareBasicLxmfError> {
        if timestamp_unix_ms == 0 {
            return Err(PrepareBasicLxmfError::InvalidTimestamp);
        }
        if timestamp_unix_ms > MAX_LXMF_TIMESTAMP_UNIX_MS {
            return Err(PrepareBasicLxmfError::TimestampTooLarge {
                actual: timestamp_unix_ms,
                maximum: MAX_LXMF_TIMESTAMP_UNIX_MS,
            });
        }
        let source = self
            .registered_lxmf_delivery_source()
            .ok_or(PrepareBasicLxmfError::DeliveryDestinationUnavailable)?;
        let timestamp = timestamp_unix_ms as f64 / 1_000.0;
        let message = LXMessage::new(
            *destination,
            source,
            self.core.identity(),
            title,
            content,
            BTreeMap::new(),
            timestamp,
        )
        .map_err(|error| match error {
            LxmfMessageError::SigningFailed => PrepareBasicLxmfError::Signing,
            LxmfMessageError::TooShort
            | LxmfMessageError::InvalidSignature
            | LxmfMessageError::Msgpack(_)
            | LxmfMessageError::InvalidArrayLen => PrepareBasicLxmfError::Invariant,
        })?;
        let message_id = message.message_id();
        let packed = message.pack();
        let payload_len = packed
            .len()
            .checked_sub(LXMF_WIRE_PREFIX_LENGTH)
            .ok_or(PrepareBasicLxmfError::Invariant)?;
        Ok(ComposedBasicLxmf {
            packed,
            payload_len,
            message_id,
        })
    }

    /// Wrap one composer-produced opportunistic LXMF carrier in encrypted RNS
    /// destination DATA.
    ///
    /// This is the sole exception to [`MAX_DATA_PAYLOAD`]. Canonical LXMF at
    /// Python's exact 295-byte selection boundary produces a 391-byte carrier;
    /// identity encryption expands it to 480 bytes and a Header-1 packet to
    /// 499 bytes. A retained repeater route requires Header-2 and therefore
    /// remains limited to [`MAX_DATA_PAYLOAD`].
    ///
    /// The opaque [`PreparedBasicLxmf`] scalar binds the implied destination
    /// and exact carrier bytes. The carrier digest and source prefix are
    /// rechecked against the composer result and this node's registered
    /// `lxmf.delivery` destination before entropy or receipt state is consumed.
    pub fn prepare_opportunistic_lxmf_data_into<R: RngCore + CryptoRng>(
        &mut self,
        prepared: PreparedBasicLxmf,
        carrier: &[u8],
        now: u64,
        rng: &mut R,
        output: &mut [u8; RNS_MTU],
    ) -> Result<PreparedData, PrepareOpportunisticLxmfDataError> {
        let expected = usize::from(prepared.carrier_len());
        if carrier.len() != expected {
            return Err(PrepareOpportunisticLxmfDataError::CarrierLengthMismatch {
                actual: carrier.len(),
                expected,
            });
        }
        let carrier_sha256: [u8; 32] = Sha256::digest(carrier).into();
        if carrier_sha256 != prepared.carrier_sha256 {
            return Err(PrepareOpportunisticLxmfDataError::CarrierDigestMismatch);
        }
        let source = self
            .registered_lxmf_delivery_source()
            .ok_or(PrepareOpportunisticLxmfDataError::CarrierSourceMismatch)?;
        if carrier.get(..LXMF_DESTINATION_HASH_LENGTH) != Some(source.as_ref()) {
            return Err(PrepareOpportunisticLxmfDataError::CarrierSourceMismatch);
        }
        self.check_data_payload_limit(carrier.len(), MAX_OPPORTUNISTIC_LXMF_CARRIER)?;

        let destination = prepared.destination();
        if carrier.len() > MAX_DATA_PAYLOAD
            && self
                .core
                .transport
                .get_path(&destination)
                .and_then(|path| path.via)
                .is_some()
        {
            return Err(PrepareOpportunisticLxmfDataError::Header2PayloadTooLarge {
                actual: carrier.len(),
                maximum: MAX_DATA_PAYLOAD,
            });
        }
        self.prepare_data_into_preflighted(&destination, carrier, now, rng, output)
            .map_err(Into::into)
    }

    /// Rehydrate one exact locally composed LXMF wire message and wrap its
    /// destination-free carrier in opportunistic destination DATA.
    ///
    /// Durable product storage cannot retain the private
    /// [`PreparedBasicLxmf`] composer token across reboot. This boundary
    /// restores that binding only after parsing the complete wire message,
    /// verifying its signature against this node's identity, and checking its
    /// exact local `lxmf.delivery` source. It then delegates to the same
    /// dedicated Header-1-aware preparation path as freshly composed
    /// opportunistic LXMF.
    ///
    /// Complete messages whose carrier exceeds
    /// [`MAX_OPPORTUNISTIC_LXMF_CARRIER`] remain direct-Link or Resource work;
    /// they are never passed through the ordinary [`MAX_DATA_PAYLOAD`] path.
    pub fn prepare_rehydrated_opportunistic_lxmf_data_into<R: RngCore + CryptoRng>(
        &mut self,
        wire: &[u8],
        now: u64,
        rng: &mut R,
        output: &mut [u8; RNS_MTU],
    ) -> Result<PreparedData, PrepareOpportunisticLxmfDataError> {
        let carrier = wire
            .get(LXMF_DESTINATION_HASH_LENGTH..)
            .ok_or(PrepareOpportunisticLxmfDataError::InvalidCompleteWire)?;
        self.check_data_payload_limit(carrier.len(), MAX_OPPORTUNISTIC_LXMF_CARRIER)?;

        let message = LXMessage::unpack(wire, Some(self.core.identity()))
            .map_err(|_| PrepareOpportunisticLxmfDataError::InvalidCompleteWire)?;
        let source = self
            .registered_lxmf_delivery_source()
            .ok_or(PrepareOpportunisticLxmfDataError::CarrierSourceMismatch)?;
        if message.source_hash != source
            || carrier.get(..LXMF_DESTINATION_HASH_LENGTH) != Some(source.as_ref())
        {
            return Err(PrepareOpportunisticLxmfDataError::CarrierSourceMismatch);
        }
        let destination = message.destination_hash;
        let prepared = PreparedBasicLxmf {
            carrier_len: carrier.len() as u16,
            carrier_sha256: Sha256::digest(carrier).into(),
            message_id: message.message_id(),
            destination,
        };
        self.prepare_opportunistic_lxmf_data_into(prepared, carrier, now, rng, output)
    }

    /// Rehydrate one exact durable LXMF wire message and encrypt it as
    /// ordinary context-None Link DATA.
    ///
    /// Durable storage cannot retain the private
    /// [`PreparedBasicDirectLxmf`] composer token across reboot. This method
    /// restores that binding only after parsing the complete message,
    /// verifying its signature against this node's identity, and checking its
    /// exact local `lxmf.delivery` source. It then delegates to
    /// [`Self::prepare_direct_lxmf_link_data_into`], which independently binds
    /// the destination, active Link, negotiated MDU, output packet, and
    /// Link-DATA receipt.
    ///
    /// Failure before delegation consumes no entropy and creates no receipt.
    pub fn prepare_rehydrated_direct_lxmf_link_data_into<R: RngCore + CryptoRng>(
        &mut self,
        wire: &[u8],
        link_id: &LinkId,
        now: u64,
        rng: &mut R,
        output: &mut [u8; RNS_MTU],
    ) -> Result<PreparedLinkData, PrepareDirectLxmfLinkDataError> {
        if wire.len() > MAX_DIRECT_LXMF_WIRE {
            return Err(PrepareDirectLxmfLinkDataError::LinkMduExceeded {
                actual: wire.len(),
                maximum: MAX_DIRECT_LXMF_WIRE,
            });
        }
        let message = LXMessage::unpack(wire, Some(self.core.identity()))
            .map_err(|_| PrepareDirectLxmfLinkDataError::InvalidCompleteWire)?;
        let source = self
            .registered_lxmf_delivery_source()
            .ok_or(PrepareDirectLxmfLinkDataError::WireSourceMismatch)?;
        if message.source_hash != source
            || wire.get(
                LXMF_DESTINATION_HASH_LENGTH
                    ..LXMF_DESTINATION_HASH_LENGTH + LXMF_DESTINATION_HASH_LENGTH,
            ) != Some(source.as_ref())
        {
            return Err(PrepareDirectLxmfLinkDataError::WireSourceMismatch);
        }
        let prepared = PreparedBasicDirectLxmf {
            wire_len: wire.len() as u16,
            wire_sha256: Sha256::digest(wire).into(),
            message_id: message.message_id(),
            destination: message.destination_hash,
        };
        self.prepare_direct_lxmf_link_data_into(prepared, wire, link_id, now, rng, output)
    }

    /// Encrypt one exact composer-produced basic LXMF message as ordinary Link DATA.
    ///
    /// The caller must reserve its final base-MTU packet slot before invoking
    /// this method. The opaque [`PreparedBasicDirectLxmf`] value is re-bound to
    /// the exact complete wire bytes, this node's registered source, the
    /// selected destination, and an active authenticated Link before native
    /// encryption consumes entropy or registers a receipt.
    ///
    /// On success the packet and Link-DATA receipt are committed together. A
    /// caller that cannot hand off the packet must cancel the returned receipt
    /// with [`Self::cancel_link_data_receipt`].
    pub fn prepare_direct_lxmf_link_data_into<R: RngCore + CryptoRng>(
        &mut self,
        prepared: PreparedBasicDirectLxmf,
        wire: &[u8],
        link_id: &LinkId,
        now: u64,
        rng: &mut R,
        output: &mut [u8; RNS_MTU],
    ) -> Result<PreparedLinkData, PrepareDirectLxmfLinkDataError> {
        let expected = usize::from(prepared.wire_len());
        if wire.len() != expected {
            return Err(PrepareDirectLxmfLinkDataError::WireLengthMismatch {
                actual: wire.len(),
                expected,
            });
        }
        let wire_sha256: [u8; 32] = Sha256::digest(wire).into();
        if wire_sha256 != prepared.wire_sha256 {
            return Err(PrepareDirectLxmfLinkDataError::WireDigestMismatch);
        }
        if wire.get(..LXMF_DESTINATION_HASH_LENGTH) != Some(prepared.destination().as_ref()) {
            return Err(PrepareDirectLxmfLinkDataError::WireDestinationMismatch);
        }
        let source = self
            .registered_lxmf_delivery_source()
            .ok_or(PrepareDirectLxmfLinkDataError::WireSourceMismatch)?;
        if wire.get(
            LXMF_DESTINATION_HASH_LENGTH
                ..LXMF_DESTINATION_HASH_LENGTH + LXMF_DESTINATION_HASH_LENGTH,
        ) != Some(source.as_ref())
        {
            return Err(PrepareDirectLxmfLinkDataError::WireSourceMismatch);
        }

        let (target, maximum) = {
            let link = self
                .core
                .transport
                .get_link(link_id)
                .ok_or(PrepareDirectLxmfLinkDataError::LinkNotFound)?;
            if link.destination_hash != prepared.destination() {
                return Err(PrepareDirectLxmfLinkDataError::LinkDestinationMismatch);
            }
            if link.state != LinkState::Active {
                return Err(PrepareDirectLxmfLinkDataError::LinkNotActive);
            }
            let interface = link
                .bound_interface()
                .ok_or(PrepareDirectLxmfLinkDataError::LinkInterfaceUnknown)?;
            (TxTarget::Only(InterfaceId(interface)), link.mdu())
        };
        if wire.len() > maximum {
            self.admission.link_payload_too_large =
                self.admission.link_payload_too_large.saturating_add(1);
            return Err(PrepareDirectLxmfLinkDataError::LinkMduExceeded {
                actual: wire.len(),
                maximum,
            });
        }
        if self.core.transport.link_data_receipt_count() >= PATHS {
            self.admission.link_data_receipt_table_full = self
                .admission
                .link_data_receipt_table_full
                .saturating_add(1);
            return Err(PrepareDirectLxmfLinkDataError::ReceiptTableFull { limit: PATHS });
        }

        let native = self
            .core
            .prepare_link_data_packet_into(link_id, wire, rng, now, output)
            .map_err(|error| match error {
                SendError::LinkNotFound => PrepareDirectLxmfLinkDataError::LinkNotFound,
                SendError::LinkNotActive => PrepareDirectLxmfLinkDataError::LinkNotActive,
                SendError::LinkInterfaceUnknown => {
                    PrepareDirectLxmfLinkDataError::LinkInterfaceUnknown
                }
                SendError::ReceiptTableFull => {
                    self.admission.link_data_receipt_table_full = self
                        .admission
                        .link_data_receipt_table_full
                        .saturating_add(1);
                    PrepareDirectLxmfLinkDataError::ReceiptTableFull { limit: PATHS }
                }
                SendError::ReceiptHashAlreadyTracked => {
                    PrepareDirectLxmfLinkDataError::ReceiptHashAlreadyTracked
                }
                SendError::Crypto(_) => PrepareDirectLxmfLinkDataError::Crypto,
                SendError::PacketBuild(_) => PrepareDirectLxmfLinkDataError::PacketBuild,
                SendError::UnknownDestination
                | SendError::LinkAlreadyExists
                | SendError::LinkTableFull
                | SendError::KeepaliveRoleMismatch
                | SendError::WindowFull
                | SendError::OutputAllocationFailed
                | SendError::InvalidRequestValue
                | SendError::ProtocolTokenExhausted
                | SendError::ProtocolTokenAssignmentFailed
                | SendError::ResourceLimit => PrepareDirectLxmfLinkDataError::Invariant,
            })?;
        debug_assert!(matches!(
            native.routing,
            PacketRouting::BoundInterface(interface)
                if target == TxTarget::Only(InterfaceId(interface))
        ));
        Ok(PreparedLinkData {
            packet_len: native.data.len() as u16,
            target,
            receipt: ReceiptId(*native.receipt.packet_hash()),
        })
    }

    /// Prepare encrypted destination DATA directly into caller-reserved
    /// storage and transactionally retain its delivery receipt.
    ///
    /// Callers should reserve their final outbox slot before invoking this
    /// method. On success the returned bytes already live in that slot and the
    /// returned receipt can be cancelled or correlated without exposing Rete
    /// types. On failure no receipt is retained.
    pub fn prepare_data_into<R: RngCore + CryptoRng>(
        &mut self,
        destination: &DestHash,
        plaintext: &[u8],
        now: u64,
        rng: &mut R,
        output: &mut [u8; RNS_MTU],
    ) -> Result<PreparedData, PrepareDataError> {
        self.check_data_payload_limit(plaintext.len(), MAX_DATA_PAYLOAD)?;
        self.prepare_data_into_preflighted(destination, plaintext, now, rng, output)
    }

    fn check_data_payload_limit(
        &mut self,
        actual: usize,
        maximum: usize,
    ) -> Result<(), PrepareDataError> {
        if actual > maximum {
            self.admission.data_payload_too_large =
                self.admission.data_payload_too_large.saturating_add(1);
            return Err(PrepareDataError::PayloadTooLarge { actual, maximum });
        }
        Ok(())
    }

    fn prepare_data_into_preflighted<R: RngCore + CryptoRng>(
        &mut self,
        destination: &DestHash,
        plaintext: &[u8],
        now: u64,
        rng: &mut R,
        output: &mut [u8; RNS_MTU],
    ) -> Result<PreparedData, PrepareDataError> {
        if self.core.transport.receipt_count() >= PATHS {
            self.admission.receipt_table_full = self.admission.receipt_table_full.saturating_add(1);
            return Err(PrepareDataError::ReceiptTableFull { limit: PATHS });
        }

        let target = self.destination_target(destination);

        let prepared = self
            .core
            .prepare_data_packet_into(destination, plaintext, rng, now, output)
            .map_err(|error| match error {
                SendError::UnknownDestination => PrepareDataError::UnknownDestination,
                SendError::ReceiptTableFull => {
                    self.admission.receipt_table_full =
                        self.admission.receipt_table_full.saturating_add(1);
                    PrepareDataError::ReceiptTableFull { limit: PATHS }
                }
                SendError::ReceiptHashAlreadyTracked => PrepareDataError::ReceiptHashAlreadyTracked,
                SendError::Crypto(_) => PrepareDataError::Crypto,
                SendError::PacketBuild(_) => PrepareDataError::PacketBuild,
                SendError::LinkAlreadyExists
                | SendError::LinkTableFull
                | SendError::LinkNotFound
                | SendError::LinkNotActive
                | SendError::KeepaliveRoleMismatch
                | SendError::LinkInterfaceUnknown
                | SendError::WindowFull
                | SendError::OutputAllocationFailed
                | SendError::InvalidRequestValue
                | SendError::ProtocolTokenExhausted
                | SendError::ProtocolTokenAssignmentFailed
                | SendError::ResourceLimit => PrepareDataError::Invariant,
            })?;
        Ok(PreparedData {
            packet_len: prepared.data.len() as u16,
            target,
            receipt: ReceiptId::from_native(prepared.receipt),
        })
    }

    /// Cancel an outstanding destination-DATA receipt by its complete packet
    /// hash.
    ///
    /// Cancellation is appropriate whenever a higher-level transaction no
    /// longer needs proof correlation, including a packet proven unsent or a
    /// redundant retry superseded by a sibling proof.
    pub fn cancel_data_receipt(&mut self, receipt: ReceiptId) -> bool {
        self.core.transport.cancel_receipt(receipt.as_bytes())
    }

    /// Cancel an outstanding ordinary Link-DATA receipt by complete packet hash.
    ///
    /// This is required when a prepared packet never gains an interface owner,
    /// or when a sibling attempt makes its proof correlation obsolete.
    pub fn cancel_link_data_receipt(&mut self, receipt: ReceiptId) -> bool {
        self.core
            .transport
            .cancel_link_data_receipt(receipt.as_bytes())
    }

    /// Build encrypted destination DATA only when its delivery receipt can be
    /// retained.
    ///
    /// This allocation-backed compatibility method does not return the
    /// receipt token. New firmware code should reserve its final queue slot and
    /// call [`Self::prepare_data_into`].
    pub fn send_data<R: RngCore + CryptoRng>(
        &mut self,
        destination: &DestHash,
        plaintext: &[u8],
        now: u64,
        rng: &mut R,
    ) -> Result<TxPacket, EmbeddedSendError> {
        if plaintext.len() > MAX_DATA_PAYLOAD {
            self.admission.data_payload_too_large =
                self.admission.data_payload_too_large.saturating_add(1);
            return Err(EmbeddedSendError::DataPayloadTooLarge {
                actual: plaintext.len(),
                maximum: MAX_DATA_PAYLOAD,
            });
        }
        if self.core.transport.receipt_count() >= PATHS {
            self.admission.receipt_table_full = self.admission.receipt_table_full.saturating_add(1);
            return Err(EmbeddedSendError::ReceiptTableFull { limit: PATHS });
        }
        let target = self.destination_target(destination);
        let bytes = self
            .core
            .build_data_packet(destination, plaintext, rng, now)?;
        Ok(TxPacket {
            bytes,
            target,
            protocol_token: None,
        })
    }

    /// Initiate a locally owned Link only when Rete can retain its state.
    ///
    /// Callers that schedule multiple independent work items can use
    /// [`Self::can_initiate_link`] to avoid allowing a full native Link table
    /// to serialize unrelated work behind a transaction that cannot yet be
    /// admitted. The mutable operation below remains authoritative.
    pub fn can_initiate_link(&self) -> bool {
        self.core.transport.link_count() < LINKS
    }

    /// Initiate a locally owned Link only when Rete can retain its state.
    pub fn initiate_link<R: RngCore + CryptoRng>(
        &mut self,
        destination: DestHash,
        now: u64,
        rng: &mut R,
    ) -> Result<(TxPacket, LinkId), LinkAdmissionError> {
        self.initiate_link_at(destination, now, MonotonicInstant::from_secs(now), rng)
    }

    /// Initiate a locally owned Link with a precise provisional request time.
    pub fn initiate_link_at<R: RngCore + CryptoRng>(
        &mut self,
        destination: DestHash,
        now: u64,
        link_now: MonotonicInstant,
        rng: &mut R,
    ) -> Result<(TxPacket, LinkId), LinkAdmissionError> {
        match try_initiate_heapless_link_at(&mut self.core, destination, now, link_now, rng) {
            Ok((packet, link_id)) => Ok((resolve_origin_packet(packet), link_id)),
            Err(error) => {
                match error {
                    LinkAdmissionError::LinkTableFull { .. } => {
                        self.admission.outbound_link_full =
                            self.admission.outbound_link_full.saturating_add(1);
                    }
                    LinkAdmissionError::LinkIdCollision => {
                        self.admission.outbound_link_collision =
                            self.admission.outbound_link_collision.saturating_add(1);
                    }
                    LinkAdmissionError::LinkStateNotRetained => {
                        self.admission.outbound_link_not_retained =
                            self.admission.outbound_link_not_retained.saturating_add(1);
                    }
                    LinkAdmissionError::Rete(SendError::ProtocolTokenExhausted) => {
                        self.admission.outbound_protocol_token_exhausted = self
                            .admission
                            .outbound_protocol_token_exhausted
                            .saturating_add(1);
                    }
                    LinkAdmissionError::Rete(SendError::ProtocolTokenAssignmentFailed) => {
                        self.admission.outbound_protocol_token_assignment_failed = self
                            .admission
                            .outbound_protocol_token_assignment_failed
                            .saturating_add(1);
                    }
                    LinkAdmissionError::Rete(_) => {}
                }
                Err(error)
            }
        }
    }

    /// Confirm that an interface accepted one token-bearing protocol packet.
    ///
    /// The first valid interface confirmation wins. A false return therefore
    /// means the token/interface/state did not match or another fan-out hop
    /// already confirmed the same packet.
    pub fn confirm_outbound_protocol(
        &mut self,
        token: OutboundProtocolToken,
        interface: InterfaceId,
        interval: OutboundDispatchInterval,
    ) -> bool {
        self.core
            .confirm_outbound_protocol(token, interface.0, interval)
    }

    /// Prepare a canonical anonymous direct request on one active Link.
    ///
    /// This is the NomadNet page-request shape: the third request-array value
    /// is MessagePack `nil`. The wall-clock timestamp is encoded unchanged and
    /// does not start timeout tracking. The returned handle must be confirmed
    /// only when the router reports its first interface dispatch.
    pub fn prepare_anonymous_request<R: RngCore + CryptoRng>(
        &mut self,
        link_id: &LinkId,
        path: &str,
        requested_at_secs: f64,
        rng: &mut R,
    ) -> Result<PreparedDirectRequest, PrepareDirectRequestError> {
        self.prepare_direct_request_value(link_id, path, None, requested_at_secs, rng)
    }

    /// Prepare one canonical, direct, single-packet request without starting
    /// its response timeout.
    ///
    /// `value` is either absent for canonical MessagePack `nil`, or exactly
    /// one already-encoded MessagePack value. Only the fixed path hash is
    /// carried on the wire, so owned request allocation is bounded by the
    /// negotiated Link MDU regardless of path-string length.
    pub fn prepare_direct_request_value<R: RngCore + CryptoRng>(
        &mut self,
        link_id: &LinkId,
        path: &str,
        value: Option<&[u8]>,
        requested_at_secs: f64,
        rng: &mut R,
    ) -> Result<PreparedDirectRequest, PrepareDirectRequestError> {
        let (target, maximum) = {
            let link = self
                .core
                .transport
                .get_link(link_id)
                .ok_or(PrepareDirectRequestError::LinkNotFound)?;
            if link.state != LinkState::Active {
                return Err(PrepareDirectRequestError::LinkNotActive);
            }
            let interface = link
                .bound_interface()
                .ok_or(PrepareDirectRequestError::LinkInterfaceUnknown)?;
            (TxTarget::Only(InterfaceId(interface)), link.mdu())
        };
        let value_len = value.map_or(1, <[u8]>::len);
        let actual = SINGLE_PACKET_REQUEST_OVERHEAD.saturating_add(value_len);
        if actual > maximum {
            return Err(PrepareDirectRequestError::RequestTooLarge { actual, maximum });
        }
        let dispatch_slot = self
            .request_dispatches
            .iter()
            .position(Option::is_none)
            .ok_or(PrepareDirectRequestError::DispatchTableFull { limit: PATHS })?;

        let native = self
            .core
            .prepare_single_packet_request_value(link_id, path, value, requested_at_secs, rng)
            .map_err(|error| match error {
                SendError::LinkNotFound => PrepareDirectRequestError::LinkNotFound,
                SendError::LinkNotActive => PrepareDirectRequestError::LinkNotActive,
                SendError::LinkInterfaceUnknown => PrepareDirectRequestError::LinkInterfaceUnknown,
                SendError::InvalidRequestValue => PrepareDirectRequestError::InvalidRequestValue,
                SendError::ReceiptHashAlreadyTracked => {
                    PrepareDirectRequestError::RequestAlreadyTracked
                }
                SendError::OutputAllocationFailed => PrepareDirectRequestError::AllocationFailed,
                SendError::Crypto(_) => PrepareDirectRequestError::Crypto,
                SendError::PacketBuild(rete_core::Error::PayloadTooLarge) => {
                    PrepareDirectRequestError::RequestTooLarge { actual, maximum }
                }
                SendError::PacketBuild(_) => PrepareDirectRequestError::PacketBuild,
                SendError::UnknownDestination
                | SendError::LinkAlreadyExists
                | SendError::LinkTableFull
                | SendError::KeepaliveRoleMismatch
                | SendError::WindowFull
                | SendError::ReceiptTableFull
                | SendError::ProtocolTokenExhausted
                | SendError::ProtocolTokenAssignmentFailed
                | SendError::ResourceLimit => PrepareDirectRequestError::Invariant,
            })?;
        let handle = RequestHandle {
            link: *native.link_id().as_bytes(),
            request: *native.request_id().as_bytes(),
        };
        let (outbound, confirmation) = native.into_parts();
        debug_assert_eq!(handle, RequestHandle::from_native(&confirmation));
        let packet = resolve_origin_packet(outbound);
        let exact_packet = Packet::parse(packet.bytes()).is_ok_and(|parsed| {
            parsed.packet_type == PacketType::Data
                && parsed.dest_type == DestType::Link
                && parsed.context == rete_core::CONTEXT_REQUEST
                && parsed.destination_hash == handle.link()
                && parsed.compute_hash()[..TRUNCATED_HASH_LEN] == handle.request()[..]
        });
        if packet.target() != target || packet.protocol_token().is_some() || !exact_packet {
            let canceled = self.core.cancel_prepared_request(confirmation);
            debug_assert!(canceled, "fresh native prepared request must roll back");
            return Err(PrepareDirectRequestError::Invariant);
        }

        self.request_dispatches[dispatch_slot] = Some(RequestDispatchEntry {
            handle,
            authority: Some(NativeRequestDispatchAuthority::Prepared(confirmation)),
        });
        Ok(PreparedDirectRequest { packet, handle })
    }

    /// Apply the router's exact first-dispatch decision to one prepared request.
    ///
    /// A false `first_dispatch` is an explicit no-op, including after a prior
    /// successful dispatch. A true value consumes the adapter-owned native
    /// preparation authority, starts timeout tracking at `sent_at`, and keeps
    /// the confirmed authority for exact response, failure, or cancellation
    /// cleanup.
    pub fn confirm_request_dispatch(
        &mut self,
        handle: RequestHandle,
        sent_at: u64,
        first_dispatch: bool,
    ) -> Result<RequestDispatchConfirmation, RequestDispatchError> {
        let index = self
            .request_dispatches
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.handle == handle))
            .ok_or(RequestDispatchError::NotTracked)?;
        if !first_dispatch {
            return Ok(RequestDispatchConfirmation::NotFirstDispatch);
        }
        let authority = self.request_dispatches[index]
            .as_mut()
            .ok_or(RequestDispatchError::NativeStateMismatch)?
            .authority
            .take()
            .ok_or(RequestDispatchError::NativeStateMismatch)?;
        let NativeRequestDispatchAuthority::Prepared(confirmation) = authority else {
            self.request_dispatches[index]
                .as_mut()
                .ok_or(RequestDispatchError::NativeStateMismatch)?
                .authority = Some(authority);
            return Err(RequestDispatchError::NotPrepared);
        };
        match self.core.confirm_prepared_request(confirmation, sent_at) {
            Ok(confirmed) => {
                debug_assert_eq!(confirmed.link_id().as_bytes(), handle.link());
                debug_assert_eq!(confirmed.request_id().as_bytes(), handle.request());
                self.request_dispatches[index]
                    .as_mut()
                    .ok_or(RequestDispatchError::NativeStateMismatch)?
                    .authority = Some(NativeRequestDispatchAuthority::Confirmed(confirmed));
                Ok(RequestDispatchConfirmation::Confirmed)
            }
            Err(error) => {
                self.request_dispatches[index] = None;
                Err(match error {
                    NativeRequestDispatchError::LinkNotFound => RequestDispatchError::LinkNotFound,
                    NativeRequestDispatchError::LinkNotActive => {
                        RequestDispatchError::LinkNotActive
                    }
                    NativeRequestDispatchError::NotPrepared => {
                        RequestDispatchError::NativeStateMismatch
                    }
                })
            }
        }
    }

    /// Cancel one exact request regardless of whether first dispatch occurred.
    ///
    /// This is the product timeout and definitive router-failure rollback.
    pub fn cancel_request(
        &mut self,
        handle: RequestHandle,
    ) -> Result<CanceledRequestDispatch, RequestDispatchError> {
        let index = self
            .request_dispatches
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.handle == handle))
            .ok_or(RequestDispatchError::NotTracked)?;
        let entry = self.request_dispatches[index]
            .take()
            .ok_or(RequestDispatchError::NativeStateMismatch)?;
        match entry
            .authority
            .ok_or(RequestDispatchError::NativeStateMismatch)?
        {
            NativeRequestDispatchAuthority::Prepared(confirmation) => self
                .core
                .cancel_prepared_request(confirmation)
                .then_some(CanceledRequestDispatch::Prepared)
                .ok_or(RequestDispatchError::NativeStateMismatch),
            NativeRequestDispatchAuthority::Confirmed(confirmed) => self
                .core
                .cancel_confirmed_request(confirmed)
                .then_some(CanceledRequestDispatch::Confirmed)
                .ok_or(RequestDispatchError::NativeStateMismatch),
        }
    }

    /// Cancel only when the exact request still awaits first dispatch.
    pub fn cancel_prepared_request(
        &mut self,
        handle: RequestHandle,
    ) -> Result<(), RequestDispatchError> {
        let index = self
            .request_dispatches
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.handle == handle))
            .ok_or(RequestDispatchError::NotTracked)?;
        if !matches!(
            self.request_dispatches[index]
                .as_ref()
                .and_then(|entry| entry.authority.as_ref()),
            Some(NativeRequestDispatchAuthority::Prepared(_))
        ) {
            return Err(RequestDispatchError::NotPrepared);
        }
        let entry = self.request_dispatches[index]
            .take()
            .ok_or(RequestDispatchError::NativeStateMismatch)?;
        let Some(NativeRequestDispatchAuthority::Prepared(confirmation)) = entry.authority else {
            return Err(RequestDispatchError::NativeStateMismatch);
        };
        self.core
            .cancel_prepared_request(confirmation)
            .then_some(())
            .ok_or(RequestDispatchError::NativeStateMismatch)
    }

    /// Cancel only when first dispatch already started timeout tracking.
    pub fn cancel_confirmed_request(
        &mut self,
        handle: RequestHandle,
    ) -> Result<(), RequestDispatchError> {
        let index = self
            .request_dispatches
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.handle == handle))
            .ok_or(RequestDispatchError::NotTracked)?;
        if !matches!(
            self.request_dispatches[index]
                .as_ref()
                .and_then(|entry| entry.authority.as_ref()),
            Some(NativeRequestDispatchAuthority::Confirmed(_))
        ) {
            return Err(RequestDispatchError::NotConfirmed);
        }
        let entry = self.request_dispatches[index]
            .take()
            .ok_or(RequestDispatchError::NativeStateMismatch)?;
        let Some(NativeRequestDispatchAuthority::Confirmed(confirmed)) = entry.authority else {
            return Err(RequestDispatchError::NativeStateMismatch);
        };
        self.core
            .cancel_confirmed_request(confirmed)
            .then_some(())
            .ok_or(RequestDispatchError::NativeStateMismatch)
    }

    #[cfg(test)]
    fn request_dispatch_count(&self) -> usize {
        self.request_dispatches.iter().flatten().count()
    }

    fn reclaim_terminal_request_dispatch(&mut self, handle: RequestHandle) {
        let Some(index) = self
            .request_dispatches
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.handle == handle))
        else {
            return;
        };
        let Some(entry) = self.request_dispatches[index].take() else {
            return;
        };
        match entry.authority {
            Some(NativeRequestDispatchAuthority::Prepared(confirmation)) => {
                // Native response handling deliberately cannot consume a
                // Prepared request. If an exact terminal event nevertheless
                // races product confirmation, close that state explicitly.
                let _ = self.core.cancel_prepared_request(confirmation);
            }
            Some(NativeRequestDispatchAuthority::Confirmed(confirmed)) => {
                // Response/failure processing already removed native pending
                // state; consuming the dispatch authority records intentional
                // completion rather than leaking a must-use owner.
                let _ = confirmed.dispatched();
            }
            None => {}
        }
    }

    fn reclaim_link_request_dispatches(&mut self, link: &[u8; TRUNCATED_HASH_LEN]) {
        while let Some(index) = self.request_dispatches.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.handle.link() == link)
        }) {
            let Some(entry) = self.request_dispatches[index].take() else {
                continue;
            };
            match entry.authority {
                Some(NativeRequestDispatchAuthority::Prepared(confirmation)) => {
                    let _ = self.core.cancel_prepared_request(confirmation);
                }
                Some(NativeRequestDispatchAuthority::Confirmed(confirmed)) => {
                    let _ = confirmed.dispatched();
                }
                None => {}
            }
        }
    }

    fn reclaim_native_request_terminals(&mut self, events: &[NativeNodeEvent]) {
        for event in events {
            let exact = match event {
                NativeNodeEvent::ResponseReceived {
                    link_id,
                    request_id,
                    ..
                }
                | NativeNodeEvent::RequestFailed {
                    link_id,
                    request_id,
                    ..
                } => Some(RequestHandle {
                    link: *link_id.as_bytes(),
                    request: *request_id.as_bytes(),
                }),
                _ => None,
            };
            if let Some(handle) = exact {
                self.reclaim_terminal_request_dispatch(handle);
            }

            let closed_link = match event {
                NativeNodeEvent::LinkClosed { link_id } => Some(*link_id.as_bytes()),
                _ => None,
            };
            if let Some(link) = closed_link {
                self.reclaim_link_request_dispatches(&link);
            }
        }
    }

    fn cancel_link_request_dispatches(&mut self, link_id: &LinkId) {
        for entry in self
            .request_dispatches
            .iter()
            .flatten()
            .filter(|entry| entry.handle.link() == link_id.as_bytes())
        {
            let request_id = rete_core::RequestId::from(*entry.handle.request());
            let expected = match entry.authority.as_ref() {
                Some(NativeRequestDispatchAuthority::Prepared(_)) => {
                    rete_stack::RequestStatus::Prepared
                }
                Some(NativeRequestDispatchAuthority::Confirmed(_)) => {
                    rete_stack::RequestStatus::Sent
                }
                None => panic!("tracked request has no native dispatch authority"),
            };
            assert_eq!(
                self.core.get_request_status(&request_id),
                Some(expected),
                "tracked request cannot be canceled before local Link removal"
            );
        }

        while let Some(handle) = self
            .request_dispatches
            .iter()
            .filter_map(Option::as_ref)
            .find(|entry| entry.handle.link() == link_id.as_bytes())
            .map(|entry| entry.handle)
        {
            assert!(
                self.cancel_request(handle).is_ok(),
                "tracked request cancellation failed before local Link removal"
            );
        }
    }

    /// Send bounded best-effort data over an active Link.
    pub fn send_link_data<R: RngCore + CryptoRng>(
        &mut self,
        link_id: &LinkId,
        data: &[u8],
        now: u64,
        rng: &mut R,
    ) -> Result<TxPacket, EmbeddedSendError> {
        let maximum = core::cmp::min(self.core.get_link_mdu(link_id), rete_transport::LINK_MDU);
        if data.len() > maximum {
            self.admission.link_payload_too_large =
                self.admission.link_payload_too_large.saturating_add(1);
            return Err(EmbeddedSendError::LinkPayloadTooLarge {
                actual: data.len(),
                maximum,
            });
        }
        let packet = self.core.send_link_data(link_id, data, rng)?;
        if let Some(link) = self.core.transport.get_link_mut(link_id) {
            link.last_outbound = MonotonicInstant::from_secs(now);
        }
        Ok(resolve_origin_packet(packet))
    }

    /// Send a reliable channel message only while a receipt slot is available.
    pub fn send_channel_message<R: RngCore + CryptoRng>(
        &mut self,
        link_id: &LinkId,
        message_type: u16,
        payload: &[u8],
        now: u64,
        rng: &mut R,
    ) -> Result<TxPacket, EmbeddedSendError> {
        let maximum = core::cmp::min(self.core.get_link_mdu(link_id), rete_transport::LINK_MDU)
            .saturating_sub(rete_transport::ENVELOPE_HEADER_SIZE);
        if payload.len() > maximum {
            self.admission.channel_payload_too_large =
                self.admission.channel_payload_too_large.saturating_add(1);
            return Err(EmbeddedSendError::ChannelPayloadTooLarge {
                actual: payload.len(),
                maximum,
            });
        }
        if self.core.transport.channel_receipt_count() >= LINKS {
            self.admission.channel_receipt_table_full =
                self.admission.channel_receipt_table_full.saturating_add(1);
            return Err(EmbeddedSendError::ChannelReceiptTableFull { limit: LINKS });
        }
        let packet = self
            .core
            .send_channel_message(link_id, message_type, payload, now, rng)?;
        Ok(resolve_origin_packet(packet))
    }

    /// Queue a local announce with explicit queue and payload admission.
    pub fn queue_announce<R: RngCore + CryptoRng>(
        &mut self,
        app_data: Option<&[u8]>,
        now: u64,
        rng: &mut R,
    ) -> Result<(), AnnounceAdmissionError> {
        let destination = self.destination_hash();
        self.queue_announce_for(&destination, app_data, now, rng)
    }

    /// Queue an announce for one registered local destination with explicit
    /// queue and payload admission.
    pub fn queue_announce_for<R: RngCore + CryptoRng>(
        &mut self,
        destination: &DestHash,
        app_data: Option<&[u8]>,
        now: u64,
        rng: &mut R,
    ) -> Result<(), AnnounceAdmissionError> {
        if let Some(data) = app_data
            && data.len() > crate::MAX_ANNOUNCE_APP_DATA
        {
            return Err(AnnounceAdmissionError::AppDataTooLarge {
                actual: data.len(),
                maximum: crate::MAX_ANNOUNCE_APP_DATA,
            });
        }
        if self.core.transport.announce_count() >= ANNOUNCES {
            self.admission.announce_queue_full =
                self.admission.announce_queue_full.saturating_add(1);
            return Err(AnnounceAdmissionError::QueueFull { limit: ANNOUNCES });
        }
        if !self
            .core
            .queue_announce_for(destination, app_data, rng, now)
        {
            self.admission.announce_native_rejected =
                self.admission.announce_native_rejected.saturating_add(1);
            return Err(AnnounceAdmissionError::NativeRejected);
        }
        Ok(())
    }

    /// Flush queued announces into broadcast transmission actions.
    pub fn flush_announces<R: RngCore>(&mut self, now: u64, rng: &mut R) -> Vec<TxPacket> {
        self.core
            .flush_announces(now, rng)
            .into_iter()
            .map(|packet| TxPacket::broadcast(packet.data))
            .collect()
    }

    /// Close a locally owned Link and return the packet/event actions.
    pub fn close_link<R: RngCore + CryptoRng>(
        &mut self,
        link_id: &LinkId,
        rng: &mut R,
    ) -> NodeActions {
        self.cancel_link_request_dispatches(link_id);
        let (packet, event) = self.core.close_link(link_id, rng);
        NodeActions::without_retained_proofs(
            event.into_iter().map(project_local_close_event).collect(),
            packet.into_iter().map(resolve_origin_packet).collect(),
            0,
        )
    }

    /// Run timer maintenance. Broadcast and exact-interface output can be
    /// resolved without ingress context; source-dependent actions are dropped
    /// and counted rather than guessed onto an interface.
    pub fn tick<R: RngCore + CryptoRng>(&mut self, now: u64, rng: &mut R) -> NodeActions {
        self.tick_at(now, MonotonicInstant::from_secs(now), rng)
    }

    /// Run timer maintenance with a precise monotonic Link clock.
    pub fn tick_at<R: RngCore + CryptoRng>(
        &mut self,
        now: u64,
        link_now: MonotonicInstant,
        rng: &mut R,
    ) -> NodeActions {
        let native = self.core.handle_tick_at(now, link_now, rng);
        self.finish_tick(native)
    }

    /// Run timer maintenance with allocation-atomic DATA timeout delivery.
    ///
    /// If the sink cannot reserve a candidate, that receipt remains
    /// outstanding and [`ReceiptTickReport::receipt_terminals_deferred`] asks
    /// the caller to retry after draining terminal capacity.
    pub fn tick_with_receipt_sink<R, S>(
        &mut self,
        now: u64,
        rng: &mut R,
        sink: &mut S,
    ) -> ReceiptTickReport
    where
        R: RngCore + CryptoRng,
        S: ReceiptTerminalSink,
    {
        self.tick_with_receipt_sink_at(now, MonotonicInstant::from_secs(now), rng, sink)
    }

    /// Precise Link-clock variant of [`Self::tick_with_receipt_sink`].
    pub fn tick_with_receipt_sink_at<R, S>(
        &mut self,
        now: u64,
        link_now: MonotonicInstant,
        rng: &mut R,
        sink: &mut S,
    ) -> ReceiptTickReport
    where
        R: RngCore + CryptoRng,
        S: ReceiptTerminalSink,
    {
        let mut native_sink = NativeReceiptSink::new(sink);
        let native =
            self.core
                .handle_tick_with_receipt_sink_at(now, link_now, rng, &mut native_sink);
        debug_assert_eq!(
            native.failed_receipts + native.failed_link_data_receipts,
            native_sink.committed.timed_out
        );
        if native.receipt_notifications_deferred {
            self.receipt_terminals.reservation_backpressure = self
                .receipt_terminals
                .reservation_backpressure
                .saturating_add(1);
        }
        ReceiptTickReport {
            actions: self.finish_tick(native.outcome),
            timed_out_receipts: native.failed_receipts,
            timed_out_link_data_receipts: native.failed_link_data_receipts,
            timed_out_receipt_tag: native_sink.committed.first_timed_out_tag,
            timed_out_receipt_tags_consistent: native_sink.committed.timed_out_tags_consistent,
            receipt_terminals_deferred: native.receipt_notifications_deferred,
        }
    }

    fn finish_tick(&mut self, mut native: IngestOutcome) -> NodeActions {
        self.reclaim_native_request_terminals(&native.events);
        let mut events = Vec::with_capacity(native.events.len());
        for event in native.events.drain(..) {
            match self.project_retained_application_event(event) {
                Ok(event) => events.push(event),
                Err(ApplicationEventProjectionError::LinkStateNotRetained { .. }) => {
                    self.ingress.link_state_not_retained =
                        self.ingress.link_state_not_retained.saturating_add(1);
                }
            }
        }
        let actions = resolve_tick_actions(native, events);
        self.ingress.routing_invariant_drops = self
            .ingress
            .routing_invariant_drops
            .saturating_add(actions.unroutable_packets as u64);
        actions
    }
}

fn is_resource_context(context: u8) -> bool {
    matches!(
        context,
        CONTEXT_RESOURCE
            | CONTEXT_RESOURCE_ADV
            | CONTEXT_RESOURCE_REQ
            | CONTEXT_RESOURCE_HMU
            | CONTEXT_RESOURCE_PRF
            | CONTEXT_RESOURCE_ICL
            | CONTEXT_RESOURCE_RCL
    )
}

fn saturating_u16(value: usize) -> (u16, bool) {
    match u16::try_from(value) {
        Ok(value) => (value, false),
        Err(_) => (u16::MAX, true),
    }
}

fn correlation_tag(bytes: &[u8]) -> u64 {
    let prefix: [u8; 8] = bytes[..8]
        .try_into()
        .expect("Reticulum covered-packet and receipt hashes are at least eight bytes");
    u64::from_le_bytes(prefix)
}

#[allow(
    clippy::match_like_matches_macro,
    reason = "an exhaustive match must fail to compile when Rete adds a routing policy"
)]
const fn endpoint_retains_ingress_packet(routing: PacketRouting) -> bool {
    match routing {
        PacketRouting::All => true,
        PacketRouting::SourceInterface => true,
        // Endpoint-mode native transport does not relay, so exact forwarding
        // actions here are local rather than forwarding authority. Bound
        // routing is emitted only by a locally owned Link.
        PacketRouting::ExactInterface(_) => true,
        PacketRouting::BoundInterface(_) => true,
        PacketRouting::AllExceptSource => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetainedProofCandidate {
    Unrelated,
    Malformed,
    Mismatched,
    Exact,
}

fn classify_retained_proof_candidate(
    outbound: &OutboundPacket,
    expected: RetainedDestinationProofExpectation,
    identity: &Identity,
) -> RetainedProofCandidate {
    let packet = match Packet::parse(&outbound.data) {
        Ok(packet) => packet,
        // A malformed action produced by local DATA handling cannot be proven
        // unrelated, so retaining mode suppresses it regardless of routing.
        Err(_) => return RetainedProofCandidate::Malformed,
    };
    if packet.packet_type != PacketType::Proof {
        return RetainedProofCandidate::Unrelated;
    }
    // Every native PROOF action belongs to the retained-proof transaction.
    // Only source-relative routing is valid; any other route must fail closed
    // instead of escaping as unrelated outbound traffic.
    if outbound.routing != PacketRouting::SourceInterface {
        return RetainedProofCandidate::Mismatched;
    }

    let destination_matches =
        packet.destination_hash == &expected.packet_hash[..rete_core::TRUNCATED_HASH_LEN];
    let covered_hash_matches = packet
        .payload
        .get(..32)
        .is_some_and(|covered| covered == expected.packet_hash);
    let signature_matches = packet
        .payload
        .get(32..96)
        .is_some_and(|signature| identity.verify(&expected.packet_hash, signature).is_ok());
    if packet.flags == 0x03
        && packet.hops == 0
        && packet.context == CONTEXT_NONE
        && destination_matches
        && packet.payload.len() == 96
        && covered_hash_matches
        && signature_matches
        && outbound.protocol_token().is_none()
    {
        RetainedProofCandidate::Exact
    } else {
        RetainedProofCandidate::Mismatched
    }
}

fn responder_link_proof_is_exact(
    proof: &RetainedInboundProof,
    link_id: LinkId,
    packet_hash: [u8; 32],
    interface: InterfaceId,
    identity: &Identity,
) -> bool {
    let tx = &proof.packet;
    if tx.target != TxTarget::Only(interface) || tx.protocol_token.is_some() {
        return false;
    }
    let Ok(packet) = Packet::parse(&tx.bytes) else {
        return false;
    };
    packet.flags == 0x0f
        && packet.hops == 0
        && packet.header_type == HeaderType::Header1
        && packet.packet_type == PacketType::Proof
        && packet.dest_type == DestType::Link
        && packet.context == CONTEXT_NONE
        && packet.destination_hash == link_id.as_ref()
        && packet.payload.len() == 96
        && packet.payload[..32] == packet_hash
        && identity
            .verify(&packet_hash, &packet.payload[32..96])
            .is_ok()
}

fn is_forwarded_path_request_for(raw: &[u8], destination: &DestHash) -> bool {
    Packet::parse(raw).is_ok_and(|packet| {
        packet.header_type == HeaderType::Header1
            && packet.packet_type == PacketType::Data
            && packet.dest_type == DestType::Plain
            && packet.context == CONTEXT_NONE
            && packet.destination_hash == rete_transport::PATH_REQUEST_DEST.as_ref()
            && packet.payload.starts_with(destination.as_ref())
    })
}

fn suppress_inbound_proof_actions(
    native: &mut IngestOutcome,
    expected: Option<&InboundProofExpectation>,
    identity: &Identity,
) {
    let Some(expected) = expected else {
        return;
    };
    match expected {
        InboundProofExpectation::DestinationData(expected) => {
            native.packets.retain(|packet| {
                classify_retained_proof_candidate(packet, *expected, identity)
                    == RetainedProofCandidate::Unrelated
            });
        }
        InboundProofExpectation::ResponderLinkData(_) => {
            // A duplicate or invalid ingress must never produce a Link proof.
            // No malformed action or PROOF action can be proven unrelated, so
            // consume it instead of allowing a future native auto-proof to
            // bypass the authenticated event boundary.
            native.packets.retain(|outbound| {
                Packet::parse(&outbound.data)
                    .is_ok_and(|packet| packet.packet_type != PacketType::Proof)
            });
        }
    }
}

fn intercept_inbound_proof_actions<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
>(
    native: &mut IngestOutcome,
    expected: Option<InboundProofExpectation>,
    source: InterfaceId,
    identity: &Identity,
) -> Result<Option<RetainedApplicationProof>, RetainedProofInvariant> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    match expected {
        InboundProofExpectation::DestinationData(expected) => {
            intercept_retained_destination_proof(native, expected, source, identity)
        }
        InboundProofExpectation::ResponderLinkData(expected) => {
            bind_responder_link_proof::<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>(
                native, expected, source, identity,
            )
        }
    }
}

fn intercept_retained_destination_proof(
    native: &mut IngestOutcome,
    expected: RetainedDestinationProofExpectation,
    source: InterfaceId,
    identity: &Identity,
) -> Result<Option<RetainedApplicationProof>, RetainedProofInvariant> {
    if native.events.is_empty() && native.packets.is_empty() {
        return Ok(None);
    }
    let mut data_event_count = 0_usize;
    let mut matching_event = None;
    for (index, event) in native.events.iter().enumerate() {
        if let NativeNodeEvent::DataReceived { dest_hash, .. } = event {
            data_event_count = data_event_count.saturating_add(1);
            if *dest_hash == expected.destination {
                matching_event = Some(index);
            }
        }
    }
    if data_event_count != 1 {
        return Err(RetainedProofInvariant::DataEventCount {
            actual: data_event_count,
        });
    }
    let Some(event_index) = matching_event else {
        return Err(RetainedProofInvariant::DataEventDestination);
    };

    let mut candidates = Vec::new();
    let mut unrelated = Vec::with_capacity(native.packets.len());
    for packet in core::mem::take(&mut native.packets) {
        let classification = classify_retained_proof_candidate(&packet, expected, identity);
        match classification {
            RetainedProofCandidate::Unrelated => unrelated.push(packet),
            RetainedProofCandidate::Malformed
            | RetainedProofCandidate::Mismatched
            | RetainedProofCandidate::Exact => {
                candidates.push((packet, classification));
            }
        }
    }
    native.packets = unrelated;

    if candidates.len() != 1 {
        return Err(RetainedProofInvariant::ProofActionCount {
            actual: candidates.len(),
        });
    }
    let (packet, classification) = candidates.pop().expect("one candidate was counted");
    match classification {
        RetainedProofCandidate::Malformed => {
            return Err(RetainedProofInvariant::MalformedProof);
        }
        RetainedProofCandidate::Mismatched => {
            return Err(RetainedProofInvariant::MismatchedProof);
        }
        RetainedProofCandidate::Exact => {}
        RetainedProofCandidate::Unrelated => unreachable!("unrelated packets were not retained"),
    }

    Ok(Some(RetainedApplicationProof {
        event_index,
        proof: RetainedInboundProof {
            packet: resolve_packet(packet, source),
        },
    }))
}

fn bind_responder_link_proof<
    const PATHS: usize,
    const ANNOUNCES: usize,
    const DEDUPLICATION: usize,
    const LINKS: usize,
>(
    native: &mut IngestOutcome,
    expected: ResponderLinkProofExpectation,
    source: InterfaceId,
    identity: &Identity,
) -> Result<Option<RetainedApplicationProof>, RetainedProofInvariant> {
    if native.events.is_empty() && native.packets.is_empty() {
        return Ok(None);
    }

    let mut link_event_count = 0_usize;
    let mut matching_event = None;
    for (index, event) in native.events.iter().enumerate() {
        if let NativeNodeEvent::LinkData {
            link_id, context, ..
        } = event
        {
            link_event_count = link_event_count.saturating_add(1);
            if *link_id == expected.link_id && *context == CONTEXT_NONE {
                matching_event = Some(index);
            }
        }
    }
    if link_event_count != 1 {
        return Err(RetainedProofInvariant::LinkDataEventCount {
            actual: link_event_count,
        });
    }
    let Some(event_index) = matching_event else {
        return Err(RetainedProofInvariant::LinkDataEventBinding);
    };

    // ProveAll Link DATA produces a native proof, while ProveApp remains
    // application-selected. Consume either shape here so the wrapper owns
    // exactly one policy-selected immediate or retained proof; malformed or
    // contradictory proof actions fail closed.
    let mut proof_action_count = 0_usize;
    let mut proof_action_mismatch = false;
    native.packets.retain(|outbound| {
        let Ok(packet) = Packet::parse(&outbound.data) else {
            // A malformed native action cannot be proven unrelated to the
            // delivery proof. Consume it and reject the semantic event.
            proof_action_count = proof_action_count.saturating_add(1);
            proof_action_mismatch = true;
            return false;
        };
        if packet.packet_type != PacketType::Proof {
            return true;
        }
        proof_action_count = proof_action_count.saturating_add(1);
        let exact = outbound.routing == PacketRouting::SourceInterface
            && packet.flags == 0x0f
            && packet.hops == 0
            && packet.header_type == HeaderType::Header1
            && packet.dest_type == DestType::Link
            && packet.context == CONTEXT_NONE
            && packet.destination_hash == expected.link_id.as_ref()
            && packet.payload.len() == 96
            && packet.payload[..32] == expected.packet_hash
            && identity
                .verify(&expected.packet_hash, &packet.payload[32..96])
                .is_ok()
            && outbound.protocol_token().is_none();
        proof_action_mismatch |= !exact;
        false
    });
    if proof_action_count > 1 {
        return Err(RetainedProofInvariant::ProofActionCount {
            actual: proof_action_count,
        });
    }
    if proof_action_mismatch {
        return Err(RetainedProofInvariant::MismatchedProof);
    }
    let bytes =
        Transport::<HeaplessStorage<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>>::build_link_proof_packet(
            identity,
            &expected.packet_hash,
            &expected.link_id,
        )
        .ok_or(RetainedProofInvariant::LinkProofBuild)?;
    let proof = RetainedInboundProof {
        packet: TxPacket {
            bytes,
            target: TxTarget::Only(source),
            protocol_token: None,
        },
    };
    if !responder_link_proof_is_exact(
        &proof,
        expected.link_id,
        expected.packet_hash,
        source,
        identity,
    ) {
        return Err(RetainedProofInvariant::MismatchedProof);
    }

    match expected.delivery {
        ResponderLinkProofDelivery::Retain => {
            Ok(Some(RetainedApplicationProof { event_index, proof }))
        }
        ResponderLinkProofDelivery::Immediate => {
            native.packets.push(OutboundPacket::new(
                proof.packet.bytes,
                PacketRouting::SourceInterface,
            ));
            Ok(None)
        }
    }
}

fn resolve_ingest_actions(
    native: IngestOutcome,
    events: Vec<ApplicationEvent>,
    source: InterfaceId,
    retained_proof: Option<RetainedApplicationProof>,
) -> NodeActions {
    let events = match retained_proof {
        Some(retained_proof) => ApplicationEvents::retained(events, retained_proof),
        None => ApplicationEvents::without_retained_proofs(events),
    };
    NodeActions {
        events,
        packets: native
            .packets
            .into_iter()
            .map(|packet| resolve_packet(packet, source))
            .collect(),
        unroutable_packets: 0,
    }
}

fn resolve_packet(packet: OutboundPacket, source: InterfaceId) -> TxPacket {
    let protocol_token = packet.protocol_token();
    let target = match packet.routing {
        PacketRouting::All => TxTarget::All,
        PacketRouting::SourceInterface => TxTarget::Only(source),
        PacketRouting::ExactInterface(interface) | PacketRouting::BoundInterface(interface) => {
            TxTarget::Only(InterfaceId(interface))
        }
        PacketRouting::AllExceptSource => TxTarget::AllExcept(source),
    };
    TxPacket {
        bytes: packet.data,
        target,
        protocol_token,
    }
}

fn resolve_tick_actions(native: IngestOutcome, events: Vec<ApplicationEvent>) -> NodeActions {
    let mut packets = Vec::with_capacity(native.packets.len());
    let mut unroutable_packets = 0;
    for packet in native.packets {
        let protocol_token = packet.protocol_token();
        match packet.routing {
            PacketRouting::All => packets.push(TxPacket {
                bytes: packet.data,
                target: TxTarget::All,
                protocol_token,
            }),
            PacketRouting::ExactInterface(interface) | PacketRouting::BoundInterface(interface) => {
                packets.push(TxPacket {
                    bytes: packet.data,
                    target: TxTarget::Only(InterfaceId(interface)),
                    protocol_token,
                });
            }
            PacketRouting::SourceInterface | PacketRouting::AllExceptSource => {
                unroutable_packets += 1;
            }
        }
    }
    NodeActions {
        events: ApplicationEvents::without_retained_proofs(events),
        packets,
        unroutable_packets,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{format, string::String, vec};

    use super::*;
    use rete_core::{
        CONTEXT_KEEPALIVE, CONTEXT_LINKCLOSE, CONTEXT_LRPROOF, CONTEXT_LRRTT, PacketBuilder,
    };
    use serde::Deserialize;

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

    #[derive(Default)]
    struct RecordingReceiptSink {
        refuse: bool,
        attempted: Vec<ReceiptCandidate>,
        terminals: Vec<ReceiptTerminal>,
        active_reservations: usize,
    }

    struct RecordingReceiptReservation<'a> {
        candidate: ReceiptCandidate,
        terminals: &'a mut Vec<ReceiptTerminal>,
        active_reservations: &'a mut usize,
    }

    impl ReceiptTerminalSink for RecordingReceiptSink {
        type Reservation<'a>
            = RecordingReceiptReservation<'a>
        where
            Self: 'a;

        fn try_reserve(
            &mut self,
            candidate: ReceiptCandidate,
        ) -> Result<Self::Reservation<'_>, ReceiptReservationUnavailable> {
            self.attempted.push(candidate);
            if self.refuse {
                return Err(ReceiptReservationUnavailable);
            }
            self.active_reservations += 1;
            Ok(RecordingReceiptReservation {
                candidate,
                terminals: &mut self.terminals,
                active_reservations: &mut self.active_reservations,
            })
        }
    }

    impl ReceiptTerminalReservation for RecordingReceiptReservation<'_> {
        fn commit(self, terminal: ReceiptTerminal) {
            assert_eq!(terminal.candidate(), self.candidate);
            self.terminals.push(terminal);
        }
    }

    impl Drop for RecordingReceiptReservation<'_> {
        fn drop(&mut self) {
            *self.active_reservations -= 1;
        }
    }

    type TestNode = EmbeddedNode<4, 2, 8, 2>;
    type TwoLinkNode = EmbeddedNode<4, 2, 8, 2>;

    fn test_link_binding(
        link: [u8; rete_core::TRUNCATED_HASH_LEN],
        destination: [u8; rete_core::TRUNCATED_HASH_LEN],
        role: ApplicationLinkRole,
    ) -> ApplicationLinkBinding {
        ApplicationLinkBinding {
            link,
            destination,
            role,
        }
    }

    #[test]
    fn inbound_data_projection_moves_the_exact_payload_owner() {
        let expected_destination = [0x42; rete_core::TRUNCATED_HASH_LEN];
        let mut payload = Vec::with_capacity(64);
        payload.extend_from_slice(b"exact moved payload");
        let expected_pointer = payload.as_ptr();
        let expected_capacity = payload.capacity();

        let projection = project_inbound_data(
            project_application_event(
                NativeNodeEvent::DataReceived {
                    dest_hash: DestHash::from(expected_destination),
                    payload,
                },
                None,
            )
            .unwrap(),
        );
        let InboundDataProjection::Data(data) = projection else {
            panic!("destination DATA must project onto the product-owned value")
        };
        assert_eq!(data.destination(), &expected_destination);
        assert_eq!(data.payload(), b"exact moved payload");

        let (destination, payload) = data.into_parts();
        assert_eq!(destination, expected_destination);
        assert_eq!(payload.as_ptr(), expected_pointer);
        assert_eq!(payload.capacity(), expected_capacity);
    }

    #[test]
    fn inbound_data_projection_returns_non_data_event_unchanged() {
        let expected_link = LinkId::from([0x7b; rete_core::TRUNCATED_HASH_LEN]);
        let mut expected_data = Vec::with_capacity(48);
        expected_data.extend_from_slice(b"unchanged link data");
        let expected_pointer = expected_data.as_ptr();
        let expected_capacity = expected_data.capacity();
        let expected_destination = [0x6c; rete_core::TRUNCATED_HASH_LEN];
        let projection = project_inbound_data(
            project_application_event(
                NativeNodeEvent::LinkData {
                    link_id: expected_link,
                    data: expected_data,
                    context: 0x5a,
                },
                Some(test_link_binding(
                    *expected_link.as_bytes(),
                    expected_destination,
                    ApplicationLinkRole::Responder,
                )),
            )
            .unwrap(),
        );

        let InboundDataProjection::Other(ApplicationEvent::LinkData {
            binding,
            data,
            context,
        }) = projection
        else {
            panic!("non-DATA event must return through the unchanged projection")
        };
        assert_eq!(binding.link(), expected_link.as_bytes());
        assert_eq!(binding.destination(), &expected_destination);
        assert_eq!(binding.role(), ApplicationLinkRole::Responder);
        assert_eq!(context, 0x5a);
        assert_eq!(data, b"unchanged link data");
        assert_eq!(data.as_ptr(), expected_pointer);
        assert_eq!(data.capacity(), expected_capacity);
    }

    #[test]
    fn link_data_projection_fails_closed_without_retained_link_state() {
        let missing_link = LinkId::from([0x6d; rete_core::TRUNCATED_HASH_LEN]);
        let mut node = node(0x6e);

        assert!(matches!(
            node.project_retained_application_event(NativeNodeEvent::LinkData {
                link_id: missing_link,
                data: vec![0xa5; 8],
                context: LINK_DATA_CONTEXT_NONE,
            }),
            Err(ApplicationEventProjectionError::LinkStateNotRetained { link })
                if link == *missing_link.as_bytes()
        ));

        let before = node.metrics().ingress.link_state_not_retained;
        let actions = node.finish_tick(IngestOutcome {
            events: vec![NativeNodeEvent::LinkData {
                link_id: missing_link,
                data: vec![0x5a; 8],
                context: LINK_DATA_CONTEXT_NONE,
            }],
            packets: Vec::new(),
            rejection: None,
        });
        assert!(actions.events.is_empty());
        assert!(actions.packets.is_empty());
        assert_eq!(node.metrics().ingress.link_state_not_retained, before + 1);
    }

    #[test]
    #[should_panic(expected = "pinned Rete close_link emitted unexpected application event tick")]
    fn local_close_projection_fails_closed_on_an_unexpected_native_event() {
        let _ = project_local_close_event(NativeNodeEvent::Tick {
            expired_paths: 0,
            closed_links: 0,
        });
    }

    #[test]
    fn inbound_data_projection_preserves_maximum_encrypted_data() {
        let payload = vec![0xa5; MAX_DATA_PAYLOAD];
        let expected_pointer = payload.as_ptr();
        let projection = project_inbound_data(ApplicationEvent::DataReceived {
            destination: [0x33; rete_core::TRUNCATED_HASH_LEN],
            payload,
        });
        let InboundDataProjection::Data(data) = projection else {
            panic!("maximum encrypted DATA must project")
        };
        let (_, payload) = data.into_parts();
        assert_eq!(payload.len(), MAX_DATA_PAYLOAD);
        assert_eq!(payload.as_ptr(), expected_pointer);
        assert!(payload.iter().all(|byte| *byte == 0xa5));
    }

    #[test]
    fn inbound_data_projection_does_not_apply_an_encrypted_payload_limit() {
        let payload = vec![0x5a; MAX_DATA_PAYLOAD + 1];
        let projection = project_inbound_data(ApplicationEvent::DataReceived {
            destination: [0x66; rete_core::TRUNCATED_HASH_LEN],
            payload,
        });
        let InboundDataProjection::Data(data) = projection else {
            panic!("projection must leave mailbox and destination-size policy to its caller")
        };
        assert_eq!(data.payload().len(), MAX_DATA_PAYLOAD + 1);
    }

    #[test]
    fn inbound_data_debug_redacts_data_and_non_data_payloads() {
        let destination = [0x42; rete_core::TRUNCATED_HASH_LEN];
        let data = project_inbound_data(ApplicationEvent::DataReceived {
            destination,
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        });
        let data_debug = format!("{data:?}");
        assert!(data_debug.contains("payload_len: 4"));
        assert!(!data_debug.contains("222, 173, 190, 239"));

        let other = project_inbound_data(ApplicationEvent::LinkData {
            binding: test_link_binding(
                [0x24; rete_core::TRUNCATED_HASH_LEN],
                [0x25; rete_core::TRUNCATED_HASH_LEN],
                ApplicationLinkRole::Initiator,
            ),
            data: vec![0xca, 0xfe, 0xba, 0xbe],
            context: 0x55,
        });
        let other_debug = format!("{other:?}");
        assert_eq!(other_debug, "Other(..)");
        assert!(!other_debug.contains("202, 254, 186, 190"));
    }

    #[test]
    fn application_event_projection_covers_every_pinned_native_variant() {
        let link = LinkId::from([0x11; rete_core::TRUNCATED_HASH_LEN]);
        let destination = DestHash::from([0x22; rete_core::TRUNCATED_HASH_LEN]);
        let identity = IdentityHash::from([0x33; rete_core::TRUNCATED_HASH_LEN]);
        let request = rete_core::RequestId::from([0x44; rete_core::TRUNCATED_HASH_LEN]);
        let path = rete_core::PathHash::from([0x55; rete_core::TRUNCATED_HASH_LEN]);
        let resource_hash = [0x66; rete_core::TRUNCATED_HASH_LEN];

        let projected: Vec<_> = vec![
            NativeNodeEvent::AnnounceReceived {
                dest_hash: destination,
                identity_hash: identity,
                hops: 3,
                app_data: Some(vec![0xa1, 0xa2]),
            },
            NativeNodeEvent::DataReceived {
                dest_hash: destination,
                payload: vec![0xb1, 0xb2],
            },
            NativeNodeEvent::ProofReceived {
                packet_hash: [0xc1; 32],
            },
            NativeNodeEvent::ReceiptFailed {
                packet_hash: [0xc2; 32],
            },
            NativeNodeEvent::LinkEstablished { link_id: link },
            NativeNodeEvent::LinkRttUpdated {
                link_id: link,
                rtt: 1.25,
            },
            NativeNodeEvent::LinkData {
                link_id: link,
                data: vec![0xd1, 0xd2],
                context: 7,
            },
            NativeNodeEvent::ChannelMessages {
                link_id: link,
                messages: vec![(9, vec![0xe1, 0xe2])],
            },
            NativeNodeEvent::RequestReceived {
                link_id: link,
                request_id: request,
                path_hash: path,
                data: vec![0xf1, 0xf2],
            },
            NativeNodeEvent::ResponseReceived {
                link_id: link,
                request_id: request,
                data: vec![0xf3, 0xf4],
            },
            NativeNodeEvent::LinkClosed { link_id: link },
            NativeNodeEvent::LinkIdentified {
                link_id: link,
                identity_hash: identity,
                public_key: [0x77; 64],
            },
            NativeNodeEvent::ResourceOffered {
                link_id: link,
                resource_hash,
                total_size: 123,
            },
            NativeNodeEvent::ResourceProgress {
                link_id: link,
                resource_hash,
                current: 2,
                total: 4,
            },
            NativeNodeEvent::ResourceComplete {
                link_id: link,
                resource_hash,
                data: vec![0x81, 0x82],
            },
            NativeNodeEvent::ResourceFailed {
                link_id: link,
                resource_hash,
            },
            NativeNodeEvent::ResourceRejected {
                link_id: link,
                resource_hash,
            },
            NativeNodeEvent::RequestFailed {
                link_id: link,
                request_id: request,
                reason: NativeRequestFailReason::ResourceFailed,
            },
            NativeNodeEvent::RequestProgress {
                link_id: link,
                request_id: request,
                current: 5,
                total: 8,
            },
            NativeNodeEvent::Tick {
                expired_paths: 6,
                closed_links: 7,
            },
        ]
        .into_iter()
        .map(|event| {
            let binding = match &event {
                NativeNodeEvent::LinkData { link_id, .. } => Some(test_link_binding(
                    *link_id.as_bytes(),
                    *destination.as_bytes(),
                    ApplicationLinkRole::Responder,
                )),
                _ => None,
            };
            project_application_event(event, binding).unwrap()
        })
        .collect();

        let expected_kinds = [
            (ApplicationEventKind::AnnounceReceived, "announce_received"),
            (ApplicationEventKind::DataReceived, "data_received"),
            (ApplicationEventKind::ProofReceived, "proof_received"),
            (ApplicationEventKind::ReceiptFailed, "receipt_failed"),
            (ApplicationEventKind::LinkEstablished, "link_established"),
            (ApplicationEventKind::LinkRttUpdated, "link_rtt_updated"),
            (ApplicationEventKind::LinkData, "link_data"),
            (ApplicationEventKind::ChannelMessages, "channel_messages"),
            (ApplicationEventKind::RequestReceived, "request_received"),
            (ApplicationEventKind::ResponseReceived, "response_received"),
            (ApplicationEventKind::LinkClosed, "link_closed"),
            (ApplicationEventKind::LinkIdentified, "link_identified"),
            (ApplicationEventKind::ResourceOffered, "resource_offered"),
            (ApplicationEventKind::ResourceProgress, "resource_progress"),
            (ApplicationEventKind::ResourceComplete, "resource_complete"),
            (ApplicationEventKind::ResourceFailed, "resource_failed"),
            (ApplicationEventKind::ResourceRejected, "resource_rejected"),
            (ApplicationEventKind::RequestFailed, "request_failed"),
            (ApplicationEventKind::RequestProgress, "request_progress"),
            (ApplicationEventKind::Tick, "tick"),
        ];
        assert_eq!(projected.len(), expected_kinds.len());
        for (event, (expected_kind, expected_label)) in projected.iter().zip(expected_kinds) {
            assert_eq!(event.kind(), expected_kind);
            assert_eq!(event.kind().as_str(), expected_label);
            assert_eq!(format!("{}", event.kind()), expected_label);
        }

        assert!(matches!(
            &projected[0],
            ApplicationEvent::AnnounceReceived {
                destination: observed_destination,
                identity: observed_identity,
                hops: 3,
                app_data: Some(data),
            } if *observed_destination == *destination.as_bytes()
                && *observed_identity == *identity.as_bytes()
                && data == &[0xa1, 0xa2]
        ));
        assert!(matches!(
            &projected[1],
            ApplicationEvent::DataReceived {
                destination: observed,
                payload,
            } if *observed == *destination.as_bytes() && payload == &[0xb1, 0xb2]
        ));
        assert!(matches!(
            &projected[2],
            ApplicationEvent::ProofReceived { packet_hash } if packet_hash == &[0xc1; 32]
        ));
        assert!(matches!(
            &projected[3],
            ApplicationEvent::ReceiptFailed { packet_hash } if packet_hash == &[0xc2; 32]
        ));
        assert!(matches!(
            &projected[4],
            ApplicationEvent::LinkEstablished { link: observed } if *observed == *link.as_bytes()
        ));
        assert!(matches!(
            &projected[5],
            ApplicationEvent::LinkRttUpdated {
                link: observed,
                rtt_seconds: 1.25,
            } if *observed == *link.as_bytes()
        ));
        assert!(matches!(
            &projected[6],
            ApplicationEvent::LinkData {
                binding,
                data,
                context: 7,
            } if binding.link() == link.as_bytes()
                && binding.destination() == destination.as_bytes()
                && data == &[0xd1, 0xd2]
        ));
        assert!(matches!(
            &projected[7],
            ApplicationEvent::ChannelMessages {
                link: observed,
                messages,
            } if *observed == *link.as_bytes()
                && messages.len() == 1
                && messages[0].0 == 9
                && messages[0].1 == [0xe1, 0xe2]
        ));
        assert!(matches!(
            &projected[8],
            ApplicationEvent::RequestReceived {
                link: observed_link,
                request: observed_request,
                path: observed_path,
                data,
            } if *observed_link == *link.as_bytes()
                && *observed_request == *request.as_bytes()
                && *observed_path == *path.as_bytes()
                && data == &[0xf1, 0xf2]
        ));
        assert!(matches!(
            &projected[9],
            ApplicationEvent::ResponseReceived {
                link: observed_link,
                request: observed_request,
                data,
            } if *observed_link == *link.as_bytes()
                && *observed_request == *request.as_bytes()
                && data == &[0xf3, 0xf4]
        ));
        assert!(matches!(
            &projected[10],
            ApplicationEvent::LinkClosed { link: observed } if *observed == *link.as_bytes()
        ));
        assert!(matches!(
            &projected[11],
            ApplicationEvent::LinkIdentified {
                link: observed_link,
                identity: observed_identity,
                public_key,
            } if *observed_link == *link.as_bytes()
                && *observed_identity == *identity.as_bytes()
                && public_key == &[0x77; 64]
        ));
        assert!(matches!(
            &projected[12],
            ApplicationEvent::ResourceOffered {
                link: observed,
                resource_hash: observed_hash,
                total_size: 123,
            } if *observed == *link.as_bytes() && observed_hash == &resource_hash
        ));
        assert!(matches!(
            &projected[13],
            ApplicationEvent::ResourceProgress {
                link: observed,
                resource_hash: observed_hash,
                current: 2,
                total: 4,
            } if *observed == *link.as_bytes() && observed_hash == &resource_hash
        ));
        assert!(matches!(
            &projected[14],
            ApplicationEvent::ResourceComplete {
                link: observed,
                resource_hash: observed_hash,
                data,
            } if *observed == *link.as_bytes()
                && observed_hash == &resource_hash
                && data == &[0x81, 0x82]
        ));
        assert!(matches!(
            &projected[15],
            ApplicationEvent::ResourceFailed {
                link: observed,
                resource_hash: observed_hash,
            } if *observed == *link.as_bytes() && observed_hash == &resource_hash
        ));
        assert!(matches!(
            &projected[16],
            ApplicationEvent::ResourceRejected {
                link: observed,
                resource_hash: observed_hash,
            } if *observed == *link.as_bytes() && observed_hash == &resource_hash
        ));
        assert!(matches!(
            &projected[17],
            ApplicationEvent::RequestFailed {
                link: observed_link,
                request: observed_request,
                reason: ApplicationRequestFailReason::ResourceFailed,
            } if *observed_link == *link.as_bytes()
                && *observed_request == *request.as_bytes()
        ));
        assert!(matches!(
            &projected[18],
            ApplicationEvent::RequestProgress {
                link: observed_link,
                request: observed_request,
                current: 5,
                total: 8,
            } if *observed_link == *link.as_bytes()
                && *observed_request == *request.as_bytes()
        ));
        assert!(matches!(
            &projected[19],
            ApplicationEvent::Tick {
                expired_paths: 6,
                closed_links: 7,
            }
        ));
    }

    #[test]
    fn application_event_debug_redacts_owned_bodies_and_public_key() {
        let resource = ApplicationEvent::ResourceComplete {
            link: [0x11; rete_core::TRUNCATED_HASH_LEN],
            resource_hash: [0x22; rete_core::TRUNCATED_HASH_LEN],
            data: vec![222, 173, 190, 239],
        };
        let resource_debug = format!("{resource:?}");
        assert!(resource_debug.contains("data_len: 4"));
        assert!(!resource_debug.contains("222, 173, 190, 239"));

        let identified = ApplicationEvent::LinkIdentified {
            link: [0x33; rete_core::TRUNCATED_HASH_LEN],
            identity: [0x44; rete_core::TRUNCATED_HASH_LEN],
            public_key: [0x55; 64],
        };
        let identified_debug = format!("{identified:?}");
        assert!(identified_debug.contains("[redacted; 64 bytes]"));
        assert!(!identified_debug.contains("85, 85, 85"));
    }

    fn identity(tag: u8) -> Identity {
        Identity::from_seed(&[tag; 32]).unwrap()
    }

    fn node(tag: u8) -> TestNode {
        TestNode::new(
            identity(tag),
            "reticulum",
            &["embedded"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap()
    }

    fn fixture_hash(encoded: &str) -> DestHash {
        let bytes = hex::decode(encoded).expect("fixture hash is hexadecimal");
        DestHash::from_slice(&bytes)
    }

    #[derive(Deserialize)]
    struct LxmfCorpus {
        fixture_identity: LxmfCorpusIdentity,
        messages: Vec<LxmfCorpusMessage>,
    }

    #[derive(Deserialize)]
    struct LxmfCorpusIdentity {
        destination_public_key_hex: String,
    }

    #[derive(Deserialize)]
    struct LxmfCorpusMessage {
        decoded: LxmfCorpusDecoded,
        destination_hash_hex: String,
        full_wire_hex: String,
        message_id_hex: String,
        name: String,
        selection_content_size: i64,
        source_hash_hex: String,
    }

    #[derive(Deserialize)]
    struct LxmfCorpusDecoded {
        content_hex: String,
        timestamp_f64_bits_hex: String,
        title_hex: String,
    }

    fn python_lxmf_corpus() -> LxmfCorpus {
        serde_json::from_str(include_str!("../../../interop/vectors/lxmf-1.0.1-v1.json"))
            .expect("checked-in Python LXMF corpus parses")
    }

    fn corpus_message<'a>(corpus: &'a LxmfCorpus, name: &str) -> &'a LxmfCorpusMessage {
        corpus
            .messages
            .iter()
            .find(|message| message.name == name)
            .expect("named Python LXMF vector exists")
    }

    fn corpus_timestamp_ms(message: &LxmfCorpusMessage) -> u64 {
        let bits = u64::from_str_radix(&message.decoded.timestamp_f64_bits_hex, 16)
            .expect("fixture timestamp bits are hexadecimal");
        let seconds = f64::from_bits(bits);
        let milliseconds = (seconds * 1_000.0) as u64;
        assert_eq!(milliseconds as f64 / 1_000.0, seconds);
        milliseconds
    }

    fn python_lxmf_fixture_node_without_delivery() -> TestNode {
        let mut private_key = [0_u8; 64];
        private_key[..32].fill(0x05);
        private_key[32..].fill(0x06);
        TestNode::new(
            Identity::from_private_key(&private_key).expect("fixture identity imports"),
            "reticulum",
            &["embedded"],
            EmbeddedNodeConfig::endpoint(),
        )
        .expect("fixture node constructs")
    }

    fn python_lxmf_fixture_node() -> TestNode {
        let mut node = python_lxmf_fixture_node_without_delivery();
        let source = node
            .register_destination(
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                DestinationType::Single,
                Direction::In,
            )
            .expect("fixture LXMF delivery destination registers");
        assert_eq!(source, fixture_hash("20f7e44b55b06cff39719106f2bd1fd2"));
        node
    }

    fn python_lxmf_fixture_recipient_identity() -> Identity {
        let mut private_key = [0_u8; 64];
        private_key[..32].fill(0x07);
        private_key[32..].fill(0x08);
        Identity::from_private_key(&private_key).expect("fixture recipient identity imports")
    }

    fn python_lxmf_fixture_recipient_node() -> (TestNode, DestHash) {
        let mut node = TestNode::new(
            python_lxmf_fixture_recipient_identity(),
            "reticulum",
            &["embedded"],
            EmbeddedNodeConfig::endpoint(),
        )
        .expect("fixture recipient node constructs");
        let destination = node
            .register_destination(
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                DestinationType::Single,
                Direction::In,
            )
            .expect("fixture recipient delivery destination registers");
        assert!(node.set_accepts_links(&destination, true));
        (node, destination)
    }

    fn python_lxmf_fixture_sender(fixture: &LxmfCorpusMessage) -> (TestNode, DestHash) {
        let mut sender = python_lxmf_fixture_node();
        let recipient = python_lxmf_fixture_recipient_identity();
        let destination = fixture_hash(&fixture.destination_hash_hex);
        sender
            .register_peer(
                &recipient,
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                1,
            )
            .unwrap();
        (sender, destination)
    }

    #[test]
    fn basic_lxmf_composition_matches_python_1_0_1_exactly() {
        let node = python_lxmf_fixture_node();
        let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");
        let expected = hex::decode(
            "021e68345db8a80c29d0c2f193baa5f4\
             20f7e44b55b06cff39719106f2bd1fd2\
             cfeaf89e57248baad43791a115345482f6b54b6e90aa0d02b5d8eddad1dc6a6\
             a323ec74921c618ae95e69153e9645db6f223d5d387db37ae23f58ef1f0560700\
             94cb41d954fc40000000c4094772656574696e6773\
             c41648656c6c6f2066726f6d20507974686f6e204c584d4680",
        )
        .expect("Python LXMF fixture is hexadecimal");
        let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];

        let prepared = node
            .prepare_basic_lxmf_into(
                &destination,
                1_700_000_000_000,
                b"Greetings",
                b"Hello from Python LXMF",
                &mut carrier,
            )
            .expect("Python-compatible basic message prepares");

        assert_eq!(
            &carrier[..usize::from(prepared.carrier_len())],
            &expected[LXMF_DESTINATION_HASH_LENGTH..]
        );
        let expected_message_id: [u8; 32] =
            hex::decode("c00af1f9ba72e66d4b9a41fbe76a55d6bbb1c8dfb9271f0cf660ed101e174c96")
                .unwrap()
                .try_into()
                .unwrap();
        assert_eq!(prepared.message_id(), expected_message_id);
        assert_eq!(prepared.destination(), destination);
    }

    #[test]
    fn basic_direct_lxmf_composition_matches_python_and_exact_link_boundary() {
        let corpus = python_lxmf_corpus();
        let at_limit = corpus_message(&corpus, "direct_limit_319");
        let over_limit = corpus_message(&corpus, "direct_over_320");
        assert_eq!(
            at_limit.selection_content_size,
            MAX_DIRECT_LXMF_CONTENT_SIZE as i64
        );
        assert_eq!(
            over_limit.selection_content_size,
            MAX_DIRECT_LXMF_CONTENT_SIZE as i64 + 1
        );

        let node = python_lxmf_fixture_node();
        let destination = fixture_hash(&at_limit.destination_hash_hex);
        let mut wire = [0xa2_u8; MAX_DIRECT_LXMF_WIRE];
        let prepared = node
            .prepare_basic_direct_lxmf_into(
                &destination,
                corpus_timestamp_ms(at_limit),
                &hex::decode(&at_limit.decoded.title_hex).unwrap(),
                &hex::decode(&at_limit.decoded.content_hex).unwrap(),
                &mut wire,
            )
            .expect("Python's exact direct packet boundary prepares");
        let expected_wire = hex::decode(&at_limit.full_wire_hex).unwrap();
        assert_eq!(expected_wire.len(), MAX_DIRECT_LXMF_WIRE);
        assert_eq!(usize::from(prepared.wire_len()), expected_wire.len());
        assert_eq!(wire.as_slice(), expected_wire.as_slice());
        assert_eq!(prepared.destination(), destination);
        let expected_message_id: [u8; 32] = hex::decode(&at_limit.message_id_hex)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(prepared.message_id(), expected_message_id);

        let mut untouched = [0x5c_u8; MAX_DIRECT_LXMF_WIRE];
        assert_eq!(
            node.prepare_basic_direct_lxmf_into(
                &fixture_hash(&over_limit.destination_hash_hex),
                corpus_timestamp_ms(over_limit),
                &hex::decode(&over_limit.decoded.title_hex).unwrap(),
                &hex::decode(&over_limit.decoded.content_hex).unwrap(),
                &mut untouched,
            ),
            Err(PrepareBasicLxmfError::PayloadTooLarge {
                actual: MAX_DIRECT_LXMF_CONTENT_SIZE + 1,
                maximum: MAX_DIRECT_LXMF_CONTENT_SIZE,
            })
        );
        assert!(untouched.iter().all(|byte| *byte == 0x5c));
    }

    #[test]
    fn exact_python_direct_lxmf_round_trips_over_bound_link_with_typed_receipt() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "direct_limit_319");
        let expected_wire = hex::decode(&fixture.full_wire_hex).unwrap();
        let mut initiator = python_lxmf_fixture_node();
        let (mut responder, destination) = python_lxmf_fixture_recipient_node();
        assert_eq!(destination, fixture_hash(&fixture.destination_hash_hex));
        initiator
            .register_peer(
                &python_lxmf_fixture_recipient_identity(),
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                99,
            )
            .unwrap();
        responder
            .set_destination_inbound_proof_policy(&destination, InboundProofPolicy::Always)
            .unwrap();
        let mut rng = CounterRng::default();

        let (request, link_id) = initiator.initiate_link(destination, 100, &mut rng).unwrap();
        let response = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
        let established = initiator.ingest(
            response.actions.packets[0].bytes(),
            101,
            InterfaceId(3),
            &mut rng,
        );
        responder.ingest(
            established.actions.packets[0].bytes(),
            102,
            InterfaceId(7),
            &mut rng,
        );
        assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
        assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));

        let mut packet = [0_u8; RNS_MTU];
        let prepared = initiator
            .prepare_rehydrated_direct_lxmf_link_data_into(
                &expected_wire,
                &link_id,
                110,
                &mut rng,
                &mut packet,
            )
            .expect("exact direct wire gains one Link-DATA receipt");
        assert_eq!(prepared.target(), TxTarget::Only(InterfaceId(3)));
        let outbound = Packet::parse(&packet[..usize::from(prepared.packet_len())]).unwrap();
        assert_eq!(&outbound.compute_hash(), prepared.receipt().as_bytes());
        assert_eq!(initiator.core.transport.link_data_receipt_count(), 1);

        let received = responder.ingest(
            &packet[..usize::from(prepared.packet_len())],
            110,
            InterfaceId(7),
            &mut rng,
        );
        assert!(matches!(
            received.actions.events.as_slice(),
            [ApplicationEvent::LinkData {
                binding,
                data,
                context: LINK_DATA_CONTEXT_NONE,
            }] if binding.link() == link_id.as_bytes()
                && binding.destination() == destination.as_bytes()
                && binding.role() == ApplicationLinkRole::Responder
                && data.as_slice() == expected_wire.as_slice()
        ));
        assert_eq!(received.metadata.generated_proof_actions(), 1);
        let proof = received
            .actions
            .packets
            .iter()
            .find(|packet| {
                Packet::parse(packet.bytes())
                    .is_ok_and(|packet| packet.packet_type == PacketType::Proof)
            })
            .expect("ordinary Link DATA produces an explicit proof");
        assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));

        let expected = ReceiptCandidate {
            kind: ReceiptKind::LinkData,
            receipt: prepared.receipt(),
        };
        let mut sink = RecordingReceiptSink::default();
        let delivered = initiator
            .ingest_with_receipt_sink(proof.bytes(), 111, InterfaceId(3), &mut rng, &mut sink)
            .unwrap();
        assert_eq!(delivered.metadata.delivered_receipt_terminals(), 1);
        assert_eq!(sink.attempted, [expected]);
        assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
        assert_eq!(initiator.core.transport.link_data_receipt_count(), 0);
    }

    #[test]
    fn direct_lxmf_substitution_is_rejected_before_entropy_or_receipt_state() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "direct_limit_319");
        let mut sender = python_lxmf_fixture_node();
        let (mut responder, destination) = python_lxmf_fixture_recipient_node();
        sender
            .register_peer(
                &python_lxmf_fixture_recipient_identity(),
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                99,
            )
            .unwrap();
        let mut rng = CounterRng::default();
        let (request, link_id) = sender.initiate_link(destination, 100, &mut rng).unwrap();
        let response = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
        let established = sender.ingest(
            response.actions.packets[0].bytes(),
            101,
            InterfaceId(3),
            &mut rng,
        );
        responder.ingest(
            established.actions.packets[0].bytes(),
            102,
            InterfaceId(7),
            &mut rng,
        );

        let mut wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
        let composed = sender
            .prepare_basic_direct_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut wire,
            )
            .unwrap();
        let mut output = [0xa5_u8; RNS_MTU];
        let entropy_before = rng.0;

        assert_eq!(
            sender.prepare_direct_lxmf_link_data_into(
                composed,
                &wire[..wire.len() - 1],
                &link_id,
                110,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::WireLengthMismatch {
                actual: MAX_DIRECT_LXMF_WIRE - 1,
                expected: MAX_DIRECT_LXMF_WIRE,
            })
        );
        assert_eq!(rng.0, entropy_before);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
        assert!(output.iter().all(|byte| *byte == 0xa5));

        let mut substituted = wire;
        *substituted.last_mut().unwrap() ^= 0x01;
        assert_eq!(
            sender.prepare_direct_lxmf_link_data_into(
                composed,
                &substituted,
                &link_id,
                110,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::WireDigestMismatch)
        );
        assert_eq!(rng.0, entropy_before);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
        assert!(output.iter().all(|byte| *byte == 0xa5));

        let inconsistent_destination = PreparedBasicDirectLxmf {
            destination: DestHash::from([0xd1; LXMF_DESTINATION_HASH_LENGTH]),
            ..composed
        };
        assert_eq!(
            sender.prepare_direct_lxmf_link_data_into(
                inconsistent_destination,
                &wire,
                &link_id,
                110,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::WireDestinationMismatch)
        );
        assert_eq!(rng.0, entropy_before);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);

        let other_destination = DestHash::from([0xd2; LXMF_DESTINATION_HASH_LENGTH]);
        let mut other_destination_wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
        let other_destination_composed = sender
            .prepare_basic_direct_lxmf_into(
                &other_destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut other_destination_wire,
            )
            .unwrap();
        assert_eq!(
            sender.prepare_direct_lxmf_link_data_into(
                other_destination_composed,
                &other_destination_wire,
                &link_id,
                110,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::LinkDestinationMismatch)
        );
        assert_eq!(rng.0, entropy_before);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);

        let mut other_source = node(77);
        other_source
            .register_destination(
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                DestinationType::Single,
                Direction::In,
            )
            .unwrap();
        let mut other_source_wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
        let other_source_composed = other_source
            .prepare_basic_direct_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut other_source_wire,
            )
            .unwrap();
        assert_eq!(
            sender.prepare_direct_lxmf_link_data_into(
                other_source_composed,
                &other_source_wire,
                &link_id,
                110,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::WireSourceMismatch)
        );
        assert_eq!(rng.0, entropy_before);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
        assert!(output.iter().all(|byte| *byte == 0xa5));
    }

    #[test]
    fn direct_lxmf_link_preflight_enforces_state_mdu_capacity_cancel_and_timeout() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "direct_limit_319");
        let mut sender = python_lxmf_fixture_node();
        let (mut responder, destination) = python_lxmf_fixture_recipient_node();
        sender
            .register_peer(
                &python_lxmf_fixture_recipient_identity(),
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                99,
            )
            .unwrap();
        let mut rng = CounterRng::default();
        let (request, link_id) = sender.initiate_link(destination, 100, &mut rng).unwrap();
        let response = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
        let established = sender.ingest(
            response.actions.packets[0].bytes(),
            101,
            InterfaceId(3),
            &mut rng,
        );
        responder.ingest(
            established.actions.packets[0].bytes(),
            102,
            InterfaceId(7),
            &mut rng,
        );

        let mut wire = [0_u8; MAX_DIRECT_LXMF_WIRE];
        let composed = sender
            .prepare_basic_direct_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut wire,
            )
            .unwrap();
        let mut output = [0x91_u8; RNS_MTU];

        let entropy_before_missing = rng.0;
        assert_eq!(
            sender.prepare_direct_lxmf_link_data_into(
                composed,
                &wire,
                &LinkId::from([0xee; LXMF_DESTINATION_HASH_LENGTH]),
                110,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::LinkNotFound)
        );
        assert_eq!(rng.0, entropy_before_missing);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);

        let mut pending_sender = python_lxmf_fixture_node();
        pending_sender
            .register_peer(
                &python_lxmf_fixture_recipient_identity(),
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                99,
            )
            .unwrap();
        let (_, pending_link) = pending_sender
            .initiate_link(destination, 100, &mut rng)
            .unwrap();
        let entropy_before_inactive = rng.0;
        assert_eq!(
            pending_sender.prepare_direct_lxmf_link_data_into(
                composed,
                &wire,
                &pending_link,
                110,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::LinkNotActive)
        );
        assert_eq!(rng.0, entropy_before_inactive);
        assert_eq!(pending_sender.core.transport.link_data_receipt_count(), 0);

        let original_signalling = sender.core.transport.get_link(&link_id).unwrap().signalling;
        sender
            .core
            .transport
            .get_link_mut(&link_id)
            .unwrap()
            .signalling = rete_transport::signalling_bytes(300, 1);
        let reduced_mdu = rete_transport::compute_link_mdu(300);
        let entropy_before_mdu = rng.0;
        assert_eq!(
            sender.prepare_direct_lxmf_link_data_into(
                composed,
                &wire,
                &link_id,
                110,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::LinkMduExceeded {
                actual: MAX_DIRECT_LXMF_WIRE,
                maximum: reduced_mdu,
            })
        );
        assert_eq!(rng.0, entropy_before_mdu);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
        assert_eq!(sender.metrics().admission.link_payload_too_large, 1);
        sender
            .core
            .transport
            .get_link_mut(&link_id)
            .unwrap()
            .signalling = original_signalling;

        let mut receipts = Vec::new();
        for now in 110..114 {
            let prepared = sender
                .prepare_direct_lxmf_link_data_into(
                    composed,
                    &wire,
                    &link_id,
                    now,
                    &mut rng,
                    &mut output,
                )
                .unwrap();
            assert_eq!(prepared.target(), TxTarget::Only(InterfaceId(3)));
            receipts.push(prepared.receipt());
        }
        assert_eq!(sender.core.transport.link_data_receipt_count(), 4);
        let entropy_before_full = rng.0;
        assert_eq!(
            sender.prepare_direct_lxmf_link_data_into(
                composed,
                &wire,
                &link_id,
                114,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDirectLxmfLinkDataError::ReceiptTableFull { limit: 4 })
        );
        assert_eq!(rng.0, entropy_before_full);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 4);
        assert_eq!(sender.metrics().admission.link_data_receipt_table_full, 1);

        for receipt in receipts {
            assert!(sender.cancel_link_data_receipt(receipt));
            assert!(!sender.cancel_link_data_receipt(receipt));
        }
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);

        let expiring = sender
            .prepare_direct_lxmf_link_data_into(
                composed,
                &wire,
                &link_id,
                200,
                &mut rng,
                &mut output,
            )
            .unwrap();
        let expected = ReceiptCandidate {
            kind: ReceiptKind::LinkData,
            receipt: expiring.receipt(),
        };
        let mut sink = RecordingReceiptSink::default();
        let report = sender.tick_with_receipt_sink(231, &mut rng, &mut sink);
        assert_eq!(report.timed_out_receipts, 0);
        assert_eq!(report.timed_out_link_data_receipts, 1);
        assert!(!report.receipt_terminals_deferred);
        assert_eq!(sink.attempted, [expected]);
        assert_eq!(sink.terminals, [ReceiptTerminal::TimedOut(expected)]);
        assert_eq!(sender.core.transport.link_data_receipt_count(), 0);
    }

    #[test]
    fn direct_lxmf_keeps_the_destination_prefix_opportunistic_data_omits() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "opportunistic_over_296");
        let node = python_lxmf_fixture_node();
        let destination = fixture_hash(&fixture.destination_hash_hex);
        let title = hex::decode(&fixture.decoded.title_hex).unwrap();
        let content = hex::decode(&fixture.decoded.content_hex).unwrap();
        let expected_wire = hex::decode(&fixture.full_wire_hex).unwrap();
        let mut direct = [0x61_u8; MAX_DIRECT_LXMF_WIRE];

        let prepared = node
            .prepare_basic_direct_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &title,
                &content,
                &mut direct,
            )
            .expect("a message above the opportunistic boundary remains direct-packet sized");
        let wire_len = usize::from(prepared.wire_len());
        assert_eq!(&direct[..wire_len], expected_wire.as_slice());
        assert_eq!(
            &direct[..LXMF_DESTINATION_HASH_LENGTH],
            destination.as_ref()
        );
        assert!(direct[wire_len..].iter().all(|byte| *byte == 0x61));

        let mut opportunistic = [0x62_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        assert_eq!(
            node.prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &title,
                &content,
                &mut opportunistic,
            ),
            Err(PrepareBasicLxmfError::PayloadTooLarge {
                actual: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE + 1,
                maximum: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE,
            })
        );
        assert!(opportunistic.iter().all(|byte| *byte == 0x62));
    }

    #[test]
    fn basic_lxmf_rejections_leave_caller_output_unchanged() {
        let node = python_lxmf_fixture_node();
        let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");

        let mut invalid_time = [0x31_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        assert_eq!(
            node.prepare_basic_lxmf_into(&destination, 0, b"title", b"content", &mut invalid_time,),
            Err(PrepareBasicLxmfError::InvalidTimestamp)
        );
        assert!(invalid_time.iter().all(|byte| *byte == 0x31));

        let mut too_large = [0x52_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let content = vec![0x42; MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE + 1];
        assert_eq!(
            node.prepare_basic_lxmf_into(
                &destination,
                1_700_000_000_000,
                b"",
                &content,
                &mut too_large,
            ),
            Err(PrepareBasicLxmfError::PayloadTooLarge {
                actual: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE + 1,
                maximum: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE,
            })
        );
        assert!(too_large.iter().all(|byte| *byte == 0x52));

        let mut too_small = [0x73_u8; 8];
        assert!(matches!(
            node.prepare_basic_lxmf_into(
                &destination,
                1_700_000_000_000,
                b"title",
                b"content",
                &mut too_small,
            ),
            Err(PrepareBasicLxmfError::OutputTooSmall { available: 8, .. })
        ));
        assert!(too_small.iter().all(|byte| *byte == 0x73));

        let mut direct_too_small = [0x74_u8; 8];
        assert!(matches!(
            node.prepare_basic_direct_lxmf_into(
                &destination,
                1_700_000_000_000,
                b"title",
                b"content",
                &mut direct_too_small,
            ),
            Err(PrepareBasicLxmfError::OutputTooSmall { available: 8, .. })
        ));
        assert!(direct_too_small.iter().all(|byte| *byte == 0x74));
    }

    #[test]
    fn python_corpus_preserves_negative_and_zero_content_size_messages() {
        let corpus = python_lxmf_corpus();
        let node = python_lxmf_fixture_node();

        for (name, expected_content_size, expected_payload_len) in
            [("empty_binary", -1, 15), ("one_byte_content", 0, 16)]
        {
            let fixture = corpus_message(&corpus, name);
            assert_eq!(fixture.selection_content_size, expected_content_size);
            let destination = fixture_hash(&fixture.destination_hash_hex);
            let expected_wire = hex::decode(&fixture.full_wire_hex).unwrap();
            assert_eq!(
                expected_wire.len() - LXMF_WIRE_PREFIX_LENGTH,
                expected_payload_len
            );

            let mut carrier = [0xa4_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
            let prepared = node
                .prepare_basic_lxmf_into(
                    &destination,
                    corpus_timestamp_ms(fixture),
                    &hex::decode(&fixture.decoded.title_hex).unwrap(),
                    &hex::decode(&fixture.decoded.content_hex).unwrap(),
                    &mut carrier,
                )
                .expect("Python-compatible small payload prepares");
            let carrier_len = usize::from(prepared.carrier_len());
            assert_eq!(
                &carrier[..carrier_len],
                &expected_wire[LXMF_DESTINATION_HASH_LENGTH..]
            );
            assert!(carrier[carrier_len..].iter().all(|byte| *byte == 0xa4));
            let expected_message_id: [u8; 32] = hex::decode(&fixture.message_id_hex)
                .unwrap()
                .try_into()
                .unwrap();
            assert_eq!(prepared.message_id(), expected_message_id);
        }
    }

    #[test]
    fn python_corpus_proves_the_exact_295_content_size_boundary() {
        let corpus = python_lxmf_corpus();
        let at_limit = corpus_message(&corpus, "opportunistic_limit_295");
        let over_limit = corpus_message(&corpus, "opportunistic_over_296");
        assert_eq!(
            at_limit.selection_content_size,
            MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE as i64
        );
        assert_eq!(
            over_limit.selection_content_size,
            MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE as i64 + 1
        );

        let node = python_lxmf_fixture_node();
        let destination = fixture_hash(&at_limit.destination_hash_hex);
        let title = hex::decode(&at_limit.decoded.title_hex).unwrap();
        let content = hex::decode(&at_limit.decoded.content_hex).unwrap();
        let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let prepared = node
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(at_limit),
                &title,
                &content,
                &mut carrier,
            )
            .expect("Python's exact opportunistic boundary prepares");

        let expected_wire = hex::decode(&at_limit.full_wire_hex).unwrap();
        assert_eq!(usize::from(prepared.carrier_len()), carrier.len());
        assert_eq!(
            expected_wire.len(),
            carrier.len() + destination.as_ref().len()
        );
        assert_eq!(&carrier[..], &expected_wire[LXMF_DESTINATION_HASH_LENGTH..]);
        assert_eq!(
            &carrier[..LXMF_DESTINATION_HASH_LENGTH],
            fixture_hash(&at_limit.source_hash_hex).as_ref()
        );
        let expected_message_id: [u8; 32] = hex::decode(&at_limit.message_id_hex)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(prepared.message_id(), expected_message_id);

        let over_title = hex::decode(&over_limit.decoded.title_hex).unwrap();
        let over_content = hex::decode(&over_limit.decoded.content_hex).unwrap();
        let mut untouched = [0xa7_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        assert_eq!(
            node.prepare_basic_lxmf_into(
                &fixture_hash(&over_limit.destination_hash_hex),
                corpus_timestamp_ms(over_limit),
                &over_title,
                &over_content,
                &mut untouched,
            ),
            Err(PrepareBasicLxmfError::PayloadTooLarge {
                actual: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE + 1,
                maximum: MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE,
            })
        );
        assert!(untouched.iter().all(|byte| *byte == 0xa7));
    }

    #[test]
    fn basic_lxmf_requires_the_registered_local_delivery_source() {
        let node = python_lxmf_fixture_node_without_delivery();
        let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");
        let mut output = [0x4d_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];

        assert_eq!(
            node.prepare_basic_lxmf_into(
                &destination,
                1_700_000_000_000,
                b"title",
                b"content",
                &mut output,
            ),
            Err(PrepareBasicLxmfError::DeliveryDestinationUnavailable)
        );
        assert!(output.iter().all(|byte| *byte == 0x4d));
    }

    #[test]
    fn basic_lxmf_timestamp_subset_is_bounded_and_millisecond_distinct() {
        let node = python_lxmf_fixture_node();
        let destination = fixture_hash("021e68345db8a80c29d0c2f193baa5f4");
        let mut lower_output = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let mut upper_output = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];

        let lower = node
            .prepare_basic_lxmf_into(
                &destination,
                MAX_LXMF_TIMESTAMP_UNIX_MS - 1,
                b"",
                b"timestamp",
                &mut lower_output,
            )
            .expect("penultimate supported millisecond prepares");
        let upper = node
            .prepare_basic_lxmf_into(
                &destination,
                MAX_LXMF_TIMESTAMP_UNIX_MS,
                b"",
                b"timestamp",
                &mut upper_output,
            )
            .expect("maximum supported millisecond prepares");
        assert_ne!(lower.message_id(), upper.message_id());

        let mut untouched = [0xc1_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        assert_eq!(
            node.prepare_basic_lxmf_into(
                &destination,
                MAX_LXMF_TIMESTAMP_UNIX_MS + 1,
                b"",
                b"timestamp",
                &mut untouched,
            ),
            Err(PrepareBasicLxmfError::TimestampTooLarge {
                actual: MAX_LXMF_TIMESTAMP_UNIX_MS + 1,
                maximum: MAX_LXMF_TIMESTAMP_UNIX_MS,
            })
        );
        assert!(untouched.iter().all(|byte| *byte == 0xc1));
    }

    #[test]
    fn maximum_opportunistic_carrier_uses_the_narrow_header1_data_path() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "opportunistic_limit_295");
        let mut sender = python_lxmf_fixture_node();
        let mut destination_private_key = [0_u8; 64];
        destination_private_key[..32].fill(0x07);
        destination_private_key[32..].fill(0x08);
        let recipient = Identity::from_private_key(&destination_private_key).unwrap();
        assert_eq!(
            hex::encode(recipient.public_key()),
            corpus.fixture_identity.destination_public_key_hex
        );
        let destination = fixture_hash(&fixture.destination_hash_hex);
        sender
            .register_peer(
                &recipient,
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                1,
            )
            .unwrap();

        let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let prepared_lxmf = sender
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut carrier,
            )
            .unwrap();
        assert_eq!(
            usize::from(prepared_lxmf.carrier_len()),
            MAX_OPPORTUNISTIC_LXMF_CARRIER
        );

        let mut generic_rng = CounterRng::default();
        let mut generic_output = [0x65_u8; RNS_MTU];
        assert_eq!(
            sender.prepare_data_into(
                &destination,
                &carrier,
                2,
                &mut generic_rng,
                &mut generic_output,
            ),
            Err(PrepareDataError::PayloadTooLarge {
                actual: MAX_OPPORTUNISTIC_LXMF_CARRIER,
                maximum: MAX_DATA_PAYLOAD,
            })
        );
        assert_eq!(generic_rng.0, 0);
        assert!(generic_output.iter().all(|byte| *byte == 0x65));

        let mut rng = CounterRng::default();
        let mut packet_bytes = [0_u8; RNS_MTU];
        let prepared_data = sender
            .prepare_opportunistic_lxmf_data_into(
                prepared_lxmf,
                &carrier,
                2,
                &mut rng,
                &mut packet_bytes,
            )
            .expect("391-byte opportunistic carrier fits direct DATA");
        assert_eq!(usize::from(prepared_data.packet_len()), RNS_MTU - 1);
        let packet =
            Packet::parse(&packet_bytes[..usize::from(prepared_data.packet_len())]).unwrap();
        assert_eq!(packet.header_type, HeaderType::Header1);
        assert_eq!(packet.packet_type, PacketType::Data);
        assert_eq!(packet.destination_hash, destination.as_ref());
        // Rete's token decryptor writes the padded AES body before returning
        // the shorter unpadded length.
        let mut decrypted = [0_u8; RNS_MTU];
        let decrypted_len = recipient.decrypt(packet.payload, &mut decrypted).unwrap();
        assert_eq!(decrypted_len, carrier.len());
        assert_eq!(&decrypted[..decrypted_len], &carrier);
    }

    #[test]
    fn rehydrated_maximum_opportunistic_wire_uses_the_dedicated_data_path() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "opportunistic_limit_295");
        let mut sender = python_lxmf_fixture_node();
        let mut destination_private_key = [0_u8; 64];
        destination_private_key[..32].fill(0x07);
        destination_private_key[32..].fill(0x08);
        let recipient = Identity::from_private_key(&destination_private_key).unwrap();
        let destination = fixture_hash(&fixture.destination_hash_hex);
        sender
            .register_peer(
                &recipient,
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                1,
            )
            .unwrap();

        let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let prepared = sender
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut carrier,
            )
            .unwrap();
        let mut wire =
            Vec::with_capacity(LXMF_DESTINATION_HASH_LENGTH + MAX_OPPORTUNISTIC_LXMF_CARRIER);
        wire.extend_from_slice(destination.as_ref());
        wire.extend_from_slice(&carrier[..usize::from(prepared.carrier_len())]);

        let mut rng = CounterRng::default();
        let mut packet_bytes = [0_u8; RNS_MTU];
        let prepared_data = sender
            .prepare_rehydrated_opportunistic_lxmf_data_into(&wire, 2, &mut rng, &mut packet_bytes)
            .expect("durable exact wire must retain the Header-1 LXMF exception");
        let packet =
            Packet::parse(&packet_bytes[..usize::from(prepared_data.packet_len())]).unwrap();
        assert_eq!(packet.header_type, HeaderType::Header1);
        assert_eq!(packet.destination_hash, destination.as_ref());
        let mut decrypted = [0_u8; RNS_MTU];
        let decrypted_len = recipient.decrypt(packet.payload, &mut decrypted).unwrap();
        assert_eq!(
            &decrypted[..decrypted_len],
            &carrier[..usize::from(prepared.carrier_len())]
        );
    }

    #[test]
    fn rehydrated_opportunistic_wire_rejects_signature_mutation_before_state_change() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "opportunistic_limit_295");
        let (mut sender, destination) = python_lxmf_fixture_sender(fixture);
        let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let prepared = sender
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut carrier,
            )
            .unwrap();
        let mut wire =
            Vec::with_capacity(LXMF_DESTINATION_HASH_LENGTH + MAX_OPPORTUNISTIC_LXMF_CARRIER);
        wire.extend_from_slice(destination.as_ref());
        wire.extend_from_slice(&carrier[..usize::from(prepared.carrier_len())]);
        wire[LXMF_DESTINATION_HASH_LENGTH * 2] ^= 0x01;

        let mut rng = CounterRng::default();
        let mut output = [0x83_u8; RNS_MTU];
        assert_eq!(
            sender
                .prepare_rehydrated_opportunistic_lxmf_data_into(&wire, 2, &mut rng, &mut output,),
            Err(PrepareOpportunisticLxmfDataError::InvalidCompleteWire)
        );
        assert_eq!(rng.0, 0);
        assert!(output.iter().all(|byte| *byte == 0x83));
        assert_eq!(sender.core.transport.receipt_count(), 0);
    }

    #[test]
    fn opportunistic_lxmf_rejects_same_length_carrier_substitution_before_mutation() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "opportunistic_limit_295");
        let (mut sender, destination) = python_lxmf_fixture_sender(fixture);
        let title = hex::decode(&fixture.decoded.title_hex).unwrap();
        let content = hex::decode(&fixture.decoded.content_hex).unwrap();
        let mut original = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let prepared = sender
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &title,
                &content,
                &mut original,
            )
            .unwrap();
        let mut substituted = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let substituted_prepared = sender
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture) + 1_000,
                &title,
                &content,
                &mut substituted,
            )
            .unwrap();
        assert_eq!(prepared.carrier_len(), substituted_prepared.carrier_len());
        assert_eq!(
            &original[..LXMF_DESTINATION_HASH_LENGTH],
            &substituted[..LXMF_DESTINATION_HASH_LENGTH]
        );
        assert_ne!(original, substituted);

        let mut rng = CounterRng::default();
        let mut output = [0x6d_u8; RNS_MTU];
        assert_eq!(
            sender.prepare_opportunistic_lxmf_data_into(
                prepared,
                &substituted,
                2,
                &mut rng,
                &mut output,
            ),
            Err(PrepareOpportunisticLxmfDataError::CarrierDigestMismatch)
        );
        assert_eq!(rng.0, 0);
        assert!(output.iter().all(|byte| *byte == 0x6d));
        assert_eq!(sender.core.transport.receipt_count(), 0);
    }

    #[test]
    fn opportunistic_lxmf_rejects_post_prefix_mutation_before_state_mutation() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "opportunistic_limit_295");
        let (mut sender, destination) = python_lxmf_fixture_sender(fixture);
        let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let prepared = sender
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut carrier,
            )
            .unwrap();
        carrier[LXMF_DESTINATION_HASH_LENGTH] ^= 0x01;

        let mut rng = CounterRng::default();
        let mut output = [0xb6_u8; RNS_MTU];
        assert_eq!(
            sender.prepare_opportunistic_lxmf_data_into(
                prepared,
                &carrier,
                3,
                &mut rng,
                &mut output,
            ),
            Err(PrepareOpportunisticLxmfDataError::CarrierDigestMismatch)
        );
        assert_eq!(rng.0, 0);
        assert!(output.iter().all(|byte| *byte == 0xb6));
        assert_eq!(sender.core.transport.receipt_count(), 0);
    }

    #[test]
    fn maximum_opportunistic_carrier_fails_closed_on_a_header2_route() {
        let corpus = python_lxmf_corpus();
        let fixture = corpus_message(&corpus, "opportunistic_limit_295");
        let mut sender = python_lxmf_fixture_node();
        let mut destination_private_key = [0_u8; 64];
        destination_private_key[..32].fill(0x07);
        destination_private_key[32..].fill(0x08);
        let recipient = Identity::from_private_key(&destination_private_key).unwrap();
        let destination = fixture_hash(&fixture.destination_hash_hex);
        sender
            .register_peer(
                &recipient,
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                1,
            )
            .unwrap();
        sender.core.transport.insert_path(
            destination,
            rete_transport::Path::via_repeater(identity(0xfa).hash(), 2, 2),
        );

        let mut carrier = [0_u8; MAX_OPPORTUNISTIC_LXMF_CARRIER];
        let prepared_lxmf = sender
            .prepare_basic_lxmf_into(
                &destination,
                corpus_timestamp_ms(fixture),
                &hex::decode(&fixture.decoded.title_hex).unwrap(),
                &hex::decode(&fixture.decoded.content_hex).unwrap(),
                &mut carrier,
            )
            .unwrap();
        let mut rng = CounterRng::default();
        let mut output = [0x9b_u8; RNS_MTU];
        assert_eq!(
            sender.prepare_opportunistic_lxmf_data_into(
                prepared_lxmf,
                &carrier,
                3,
                &mut rng,
                &mut output,
            ),
            Err(PrepareOpportunisticLxmfDataError::Header2PayloadTooLarge {
                actual: MAX_OPPORTUNISTIC_LXMF_CARRIER,
                maximum: MAX_DATA_PAYLOAD,
            })
        );
        assert_eq!(rng.0, 0);
        assert!(output.iter().all(|byte| *byte == 0x9b));
        assert_eq!(sender.core.transport.receipt_count(), 0);
    }

    #[test]
    fn learned_destination_identity_is_copied_without_exposing_native_storage() {
        let peer = identity(201);
        let expected_public_key = peer.public_key();
        let peer_destination = TestNode::new(
            identity(201),
            "lxmf",
            &["delivery"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap()
        .destination_hash();
        let mut owner = node(200);

        owner
            .register_peer(&peer, "lxmf", &["delivery"], 1)
            .unwrap();

        assert_eq!(
            owner.recall_identity(&peer_destination),
            Some(expected_public_key)
        );
        assert_eq!(owner.recall_identity(&DestHash::from([0xee; 16])), None);
    }

    #[test]
    fn learned_identity_is_classified_only_by_its_exact_lxmf_delivery_hash() {
        let delivery_peer = identity(202);
        let other_peer = identity(203);
        let delivery_public_key = delivery_peer.public_key();
        let other_public_key = other_peer.public_key();
        let mut delivery_announcer = TestNode::new(
            delivery_peer,
            LXMF_APPLICATION_NAME,
            &[LXMF_DELIVERY_ASPECT],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let mut other_announcer = TestNode::new(
            other_peer,
            "nomadnetwork",
            &["node"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let delivery_destination = delivery_announcer.destination_hash();
        let other_destination = other_announcer.destination_hash();
        let mut owner = node(200);
        let mut rng = CounterRng::default();

        delivery_announcer
            .queue_announce(None, 1, &mut rng)
            .unwrap();
        let delivery_announce = delivery_announcer
            .flush_announces(1, &mut rng)
            .pop()
            .unwrap();
        let learned = owner.ingest(delivery_announce.bytes(), 1, InterfaceId(4), &mut rng);
        assert!(matches!(
            learned.actions.events.as_slice(),
            [ApplicationEvent::AnnounceReceived { destination, .. }]
                if destination == delivery_destination.as_bytes()
        ));

        other_announcer.queue_announce(None, 2, &mut rng).unwrap();
        let other_announce = other_announcer.flush_announces(2, &mut rng).pop().unwrap();
        let learned = owner.ingest(other_announce.bytes(), 2, InterfaceId(4), &mut rng);
        assert!(matches!(
            learned.actions.events.as_slice(),
            [ApplicationEvent::AnnounceReceived { destination, .. }]
                if destination == other_destination.as_bytes()
        ));

        assert_eq!(
            owner.recall_lxmf_delivery_identity(&delivery_destination),
            Some(delivery_public_key)
        );
        assert_eq!(
            owner.recall_identity(&other_destination),
            Some(other_public_key)
        );
        assert_eq!(
            owner.recall_lxmf_delivery_identity(&other_destination),
            None
        );
        assert_eq!(
            owner.recall_lxmf_delivery_identity(&DestHash::from([0xee; 16])),
            None
        );
    }

    fn link_request(
        initiator: &mut TestNode,
        responder: &TestNode,
        rng: &mut CounterRng,
    ) -> (TxPacket, LinkId) {
        initiator
            .register_peer(&identity(2), "reticulum", &["embedded"], 100)
            .unwrap();
        initiator
            .initiate_link(responder.destination_hash(), 100, rng)
            .unwrap()
    }

    fn establish_bound_link(
        initiator: &mut TestNode,
        responder: &mut TestNode,
        rng: &mut CounterRng,
    ) -> LinkId {
        let (request, link_id) = link_request(initiator, responder, rng);
        let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), rng);
        assert!(proof.actions.events.is_empty());
        assert_eq!(proof.actions.packets.len(), 1);
        assert_eq!(
            proof.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(7))
        );
        assert_eq!(
            Packet::parse(proof.actions.packets[0].bytes())
                .unwrap()
                .context,
            CONTEXT_LRPROOF
        );

        let established =
            initiator.ingest(proof.actions.packets[0].bytes(), 101, InterfaceId(3), rng);
        assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
        assert_eq!(initiator.link_rtt(&link_id), Some(1.0));
        assert_eq!(established.actions.packets.len(), 1);
        assert_eq!(
            established.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(3))
        );
        assert_eq!(
            Packet::parse(established.actions.packets[0].bytes())
                .unwrap()
                .context,
            CONTEXT_LRRTT
        );

        let active = responder.ingest(
            established.actions.packets[0].bytes(),
            102,
            InterfaceId(7),
            rng,
        );
        assert_eq!(active.disposition, IngressDisposition::Processed);
        assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));
        assert_eq!(responder.link_rtt(&link_id), Some(2.0));
        link_id
    }

    #[test]
    fn anonymous_request_is_canonical_exact_routed_and_times_out_only_after_dispatch() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
        let requested_at = 1_700_000_000.125_f64;

        let prepared = initiator
            .prepare_anonymous_request(&link_id, "/page/index.mu", requested_at, &mut rng)
            .unwrap();
        let handle = prepared.handle();
        assert_eq!(handle.link(), link_id.as_bytes());
        assert_eq!(prepared.packet().target(), TxTarget::Only(InterfaceId(3)));
        assert_eq!(prepared.packet().protocol_token(), None);
        assert_eq!(initiator.request_dispatch_count(), 1);

        let packet = Packet::parse(prepared.packet().bytes()).unwrap();
        assert_eq!(packet.context, rete_core::CONTEXT_REQUEST);
        assert_eq!(packet.dest_type, DestType::Link);
        assert_eq!(packet.destination_hash, link_id.as_bytes());
        assert_eq!(
            handle.request(),
            &packet.compute_hash()[..TRUNCATED_HASH_LEN]
        );
        let mut plaintext = [0_u8; RNS_MTU];
        let plaintext_len = responder
            .core
            .transport
            .get_link(&link_id)
            .unwrap()
            .decrypt(packet.payload, &mut plaintext)
            .unwrap();
        let (wire_time, path_hash, value) =
            rete_transport::parse_request_value(&plaintext[..plaintext_len]).unwrap();
        assert_eq!(wire_time.to_bits(), requested_at.to_bits());
        assert_eq!(path_hash, rete_transport::path_hash("/page/index.mu"));
        assert_eq!(value, [0xc0]);

        let before_dispatch = initiator.tick_at(10_000, MonotonicInstant::from_secs(103), &mut rng);
        assert!(
            !before_dispatch
                .events
                .as_slice()
                .iter()
                .any(|event| matches!(
                    event,
                    ApplicationEvent::RequestFailed { request, .. }
                        if request == handle.request()
                ))
        );
        assert_eq!(initiator.request_dispatch_count(), 1);

        assert_eq!(
            initiator.confirm_request_dispatch(handle, 20_000, true),
            Ok(RequestDispatchConfirmation::Confirmed)
        );
        let timed_out = initiator.tick_at(u64::MAX - 1, MonotonicInstant::from_secs(103), &mut rng);
        assert!(timed_out.events.as_slice().iter().any(|event| matches!(
            event,
            ApplicationEvent::RequestFailed {
                link,
                request,
                reason: ApplicationRequestFailReason::Timeout,
            } if link == handle.link() && request == handle.request()
        )));
        assert_eq!(initiator.request_dispatch_count(), 0);
    }

    #[test]
    fn request_dispatch_confirmation_cancel_and_response_cleanup_are_exact() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

        let response_request = initiator
            .prepare_anonymous_request(&link_id, "/page/index.mu", 100.0, &mut rng)
            .unwrap();
        let response_handle = response_request.handle();
        assert_eq!(
            initiator.confirm_request_dispatch(response_handle, 110, false),
            Ok(RequestDispatchConfirmation::NotFirstDispatch)
        );
        assert_eq!(
            initiator.cancel_confirmed_request(response_handle),
            Err(RequestDispatchError::NotConfirmed)
        );
        assert_eq!(
            initiator.confirm_request_dispatch(response_handle, 110, true),
            Ok(RequestDispatchConfirmation::Confirmed)
        );
        assert_eq!(
            initiator.confirm_request_dispatch(response_handle, 111, false),
            Ok(RequestDispatchConfirmation::NotFirstDispatch)
        );
        assert_eq!(
            initiator.confirm_request_dispatch(response_handle, 111, true),
            Err(RequestDispatchError::NotPrepared)
        );
        assert_eq!(
            initiator.cancel_prepared_request(response_handle),
            Err(RequestDispatchError::NotPrepared)
        );

        let request_id = rete_core::RequestId::from(*response_handle.request());
        let response = responder
            .core
            .send_response(&link_id, &request_id, b"hello micron", &mut rng)
            .unwrap();
        let response = resolve_origin_packet(response);
        let received = initiator.ingest(response.bytes(), 112, InterfaceId(3), &mut rng);
        assert!(
            received
                .actions
                .events
                .as_slice()
                .iter()
                .any(|event| matches!(
                    event,
                    ApplicationEvent::ResponseReceived {
                        link,
                        request,
                        data,
                    } if link == response_handle.link()
                        && request == response_handle.request()
                        && data == b"hello micron"
                ))
        );
        assert_eq!(initiator.request_dispatch_count(), 0);
        assert_eq!(
            initiator.cancel_request(response_handle),
            Err(RequestDispatchError::NotTracked)
        );
        assert_eq!(
            initiator.confirm_request_dispatch(response_handle, 113, false),
            Err(RequestDispatchError::NotTracked)
        );

        let prepared = initiator
            .prepare_anonymous_request(&link_id, "/page/prepared.mu", 120.0, &mut rng)
            .unwrap();
        assert_eq!(
            initiator.cancel_request(prepared.handle()),
            Ok(CanceledRequestDispatch::Prepared)
        );
        assert_eq!(initiator.request_dispatch_count(), 0);

        let confirmed = initiator
            .prepare_anonymous_request(&link_id, "/page/confirmed.mu", 121.0, &mut rng)
            .unwrap();
        assert_eq!(
            initiator.confirm_request_dispatch(confirmed.handle(), 122, true),
            Ok(RequestDispatchConfirmation::Confirmed)
        );
        assert_eq!(
            initiator.cancel_request(confirmed.handle()),
            Ok(CanceledRequestDispatch::Confirmed)
        );
        assert_eq!(initiator.request_dispatch_count(), 0);
    }

    #[test]
    fn request_preflight_bounds_storage_and_local_link_close_rolls_back() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
        let maximum = initiator.core.transport.get_link(&link_id).unwrap().mdu();

        let entropy_before_invalid = rng.0;
        assert_eq!(
            initiator.prepare_direct_request_value(
                &link_id,
                "/page/form.mu",
                Some(&[0xc0, 0xc0]),
                100.0,
                &mut rng,
            ),
            Err(PrepareDirectRequestError::InvalidRequestValue)
        );
        assert_eq!(rng.0, entropy_before_invalid);
        assert_eq!(initiator.request_dispatch_count(), 0);

        let oversized = vec![0xc0; maximum - SINGLE_PACKET_REQUEST_OVERHEAD + 1];
        let entropy_before_oversized = rng.0;
        assert_eq!(
            initiator.prepare_direct_request_value(
                &link_id,
                "/page/form.mu",
                Some(&oversized),
                100.0,
                &mut rng,
            ),
            Err(PrepareDirectRequestError::RequestTooLarge {
                actual: maximum + 1,
                maximum,
            })
        );
        assert_eq!(rng.0, entropy_before_oversized);
        assert_eq!(initiator.request_dispatch_count(), 0);

        let mut handles = Vec::new();
        for _ in 0..4 {
            handles.push(
                initiator
                    .prepare_anonymous_request(&link_id, "/page/index.mu", 101.0, &mut rng)
                    .unwrap()
                    .handle(),
            );
        }
        let entropy_before_full = rng.0;
        assert_eq!(
            initiator.prepare_anonymous_request(&link_id, "/page/index.mu", 101.0, &mut rng,),
            Err(PrepareDirectRequestError::DispatchTableFull { limit: 4 })
        );
        assert_eq!(rng.0, entropy_before_full);
        assert_eq!(initiator.request_dispatch_count(), 4);

        let closing_handle = handles[0];
        let closed = initiator.close_link(&link_id, &mut rng);
        assert!(closed.events.as_slice().iter().any(|event| matches!(
            event,
            ApplicationEvent::LinkClosed { link } if link == closing_handle.link()
        )));
        assert_eq!(initiator.request_dispatch_count(), 0);
        for handle in handles {
            assert_eq!(
                initiator.cancel_request(handle),
                Err(RequestDispatchError::NotTracked)
            );
        }
    }

    #[test]
    fn local_close_fails_before_link_removal_when_request_cannot_cancel() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);
        let prepared = initiator
            .prepare_anonymous_request(&link_id, "/page/index.mu", 100.0, &mut rng)
            .unwrap();
        let handle = prepared.handle();
        assert_eq!(
            initiator.confirm_request_dispatch(handle, 110, true),
            Ok(RequestDispatchConfirmation::Confirmed)
        );

        // Bypass the owning adapter to simulate native state becoming terminal
        // without its exact projected event reclaiming the retained control.
        let request_id = rete_core::RequestId::from(*handle.request());
        let response = responder
            .core
            .send_response(&link_id, &request_id, b"terminal", &mut rng)
            .unwrap();
        let terminal = initiator
            .core
            .handle_ingest(&response.data, 111, 3, &mut rng);
        assert!(matches!(
            terminal.events.as_slice(),
            [NativeNodeEvent::ResponseReceived {
                link_id: event_link,
                request_id: event_request,
                data,
            }] if *event_link == link_id
                && event_request.as_bytes() == handle.request()
                && data == b"terminal"
        ));
        assert_eq!(initiator.core.get_request_status(&request_id), None);
        assert_eq!(initiator.request_dispatch_count(), 1);

        let close_panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = initiator.close_link(&link_id, &mut rng);
        }));
        assert!(close_panicked.is_err());
        assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
        assert_eq!(initiator.request_dispatch_count(), 1);

        initiator.reclaim_native_request_terminals(&terminal.events);
        assert_eq!(initiator.request_dispatch_count(), 0);
        let closed = initiator.close_link(&link_id, &mut rng);
        assert!(matches!(
            closed.events.as_slice(),
            [ApplicationEvent::LinkClosed { link }] if link == link_id.as_bytes()
        ));
        assert_eq!(initiator.link_state(&link_id), None);
    }

    #[test]
    fn authenticated_malformed_lrrtt_closes_once_on_bound_interface() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
        let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
        assert_eq!(responder.link_state(&link_id), Some(LinkState::Handshake));
        assert!(proof.actions.events.is_empty());
        assert_eq!(proof.actions.packets.len(), 1);

        // Validating LRPROOF gives the initiator the negotiated session key.
        // Build nil as an encrypted LRRTT plaintext through that key, while
        // deliberately discarding the automatically generated valid LRRTT.
        let established = initiator.ingest(
            proof.actions.packets[0].bytes(),
            101,
            InterfaceId(3),
            &mut rng,
        );
        assert!(matches!(
            established.actions.events.as_slice(),
            [ApplicationEvent::LinkEstablished { link }] if *link == *link_id.as_bytes()
        ));
        let malformed = initiator
            .core
            .transport
            .build_lrrtt_packet(&link_id, &[0xc0], &mut rng)
            .unwrap();

        let counters_before = responder.metrics().transport;
        let closed = responder.ingest(&malformed, 102, InterfaceId(7), &mut rng);
        assert_eq!(closed.disposition, IngressDisposition::NativeInvalid);
        assert!(matches!(
            closed.actions.events.as_slice(),
            [ApplicationEvent::LinkClosed { link }] if *link == *link_id.as_bytes()
        ));
        assert_eq!(closed.actions.packets.len(), 1);
        assert_eq!(
            closed.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(7))
        );
        let linkclose = Packet::parse(closed.actions.packets[0].bytes()).unwrap();
        assert_eq!(linkclose.packet_type, PacketType::Data);
        assert_eq!(linkclose.dest_type, DestType::Link);
        assert_eq!(linkclose.destination_hash, link_id.as_ref());
        assert_eq!(linkclose.context, CONTEXT_LINKCLOSE);
        assert_eq!(responder.link_state(&link_id), None);
        assert_eq!(responder.link_rtt(&link_id), None);
        let counters_after = responder.metrics().transport;
        assert_eq!(
            counters_after.packets_dropped_invalid,
            counters_before.packets_dropped_invalid + 1
        );
        assert_eq!(
            counters_after.links_failed,
            counters_before.links_failed + 1
        );
        assert_eq!(
            counters_after.links_closed,
            counters_before.links_closed + 1
        );
        assert_eq!(
            counters_after.links_established,
            counters_before.links_established
        );

        // The close packet is authenticated with the retained Link key and is
        // accepted by its peer, while replaying malformed LRRTT cannot emit a
        // second close or lifecycle event after responder state was purged.
        let peer_closed = initiator.ingest(
            closed.actions.packets[0].bytes(),
            103,
            InterfaceId(3),
            &mut rng,
        );
        assert!(matches!(
            peer_closed.actions.events.as_slice(),
            [ApplicationEvent::LinkClosed { link }] if *link == *link_id.as_bytes()
        ));
        assert_eq!(initiator.link_state(&link_id), None);

        let replayed = responder.ingest(&malformed, 104, InterfaceId(7), &mut rng);
        assert!(replayed.actions.events.is_empty());
        assert!(replayed.actions.packets.is_empty());
        assert_eq!(responder.link_state(&link_id), None);
        let counters_replayed = responder.metrics().transport;
        assert_eq!(
            counters_replayed.packets_dropped_invalid,
            counters_after.packets_dropped_invalid
        );
        assert_eq!(counters_replayed.links_failed, counters_after.links_failed);
        assert_eq!(counters_replayed.links_closed, counters_after.links_closed);
        assert_eq!(
            counters_replayed.links_established,
            counters_after.links_established
        );
    }

    #[test]
    fn wrapper_rejects_wrong_hop_lrproof_before_dedup_and_accepts_correct_copy() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
        let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
        assert_eq!(proof.actions.packets.len(), 1);
        let proof_bytes = proof.actions.packets[0].bytes();
        assert_eq!(Packet::parse(proof_bytes).unwrap().hops, 0);

        // A registered direct path expects one hop after local ingress. The
        // modified wire byte produces two, but does not alter the proof hash.
        let mut wrong_hops = proof_bytes.to_vec();
        wrong_hops[1] = 1;
        let dedup_before = initiator.metrics().transport.packets_dropped_dedup;
        let rejected = initiator.ingest(&wrong_hops, 101, InterfaceId(3), &mut rng);
        assert_eq!(rejected.disposition, IngressDisposition::NativeInvalid);
        assert!(rejected.actions.events.is_empty());
        assert!(rejected.actions.packets.is_empty());
        assert_eq!(rejected.actions.unroutable_packets, 0);
        assert_eq!(initiator.link_state(&link_id), Some(LinkState::Handshake));
        assert_eq!(
            initiator.metrics().transport.packets_dropped_dedup,
            dedup_before
        );

        let established = initiator.ingest(proof_bytes, 102, InterfaceId(3), &mut rng);
        assert_eq!(established.disposition, IngressDisposition::Processed);
        assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
        assert_eq!(established.actions.packets.len(), 1);
        assert_eq!(
            established.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(3))
        );
        assert_eq!(
            Packet::parse(established.actions.packets[0].bytes())
                .unwrap()
                .context,
            CONTEXT_LRRTT
        );
    }

    #[test]
    fn wrapper_keepalive_roundtrip_is_exact_internal_repeatable_and_bound() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

        // LRPROOF takes one whole monotonic second in this fixture. The
        // compatibility tick supplies whole seconds, so round the precise
        // RTT-derived interval upward to the first representable due instant.
        let initiator_keepalive_micros = initiator
            .link_snapshot_for_conformance(&link_id)
            .unwrap()
            .keepalive_interval
            .as_micros();
        let initiator_keepalive = initiator_keepalive_micros.saturating_add(999_999) / 1_000_000;
        let request_at = 101 + initiator_keepalive;
        let tick = initiator.tick(request_at, &mut rng);
        let requests: Vec<_> = tick
            .packets
            .iter()
            .filter(|packet| {
                Packet::parse(packet.bytes())
                    .is_ok_and(|parsed| parsed.context == CONTEXT_KEEPALIVE)
            })
            .collect();
        assert_eq!(requests.len(), 1);
        let request = requests[0];
        assert_eq!(request.target(), TxTarget::Only(InterfaceId(3)));
        assert_eq!(request.bytes().len(), 20);
        let parsed_request = Packet::parse(request.bytes()).unwrap();
        assert_eq!(parsed_request.packet_type, PacketType::Data);
        assert_eq!(parsed_request.dest_type, DestType::Link);
        assert_eq!(parsed_request.destination_hash, link_id.as_ref());
        assert_eq!(parsed_request.payload, &[0xFF]);
        assert!(matches!(
            tick.events.as_slice(),
            [ApplicationEvent::Tick {
                closed_links: 0,
                ..
            }]
        ));

        let responder_dedup = responder.metrics().transport.packets_dropped_dedup;
        let response = responder.ingest(request.bytes(), request_at, InterfaceId(7), &mut rng);
        assert_eq!(response.disposition, IngressDisposition::Processed);
        assert!(response.actions.events.is_empty());
        assert_eq!(response.actions.packets.len(), 1);
        assert_eq!(
            response.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(7))
        );
        assert_eq!(response.actions.packets[0].bytes().len(), 20);
        let parsed_response = Packet::parse(response.actions.packets[0].bytes()).unwrap();
        assert_eq!(parsed_response.packet_type, PacketType::Data);
        assert_eq!(parsed_response.dest_type, DestType::Link);
        assert_eq!(parsed_response.destination_hash, link_id.as_ref());
        assert_eq!(parsed_response.payload, &[0xFE]);
        assert_eq!(
            responder.metrics().transport.packets_dropped_dedup,
            responder_dedup
        );

        let initiator_dedup = initiator.metrics().transport.packets_dropped_dedup;
        let consumed = initiator.ingest(
            response.actions.packets[0].bytes(),
            request_at + 1,
            InterfaceId(3),
            &mut rng,
        );
        assert_eq!(
            consumed.disposition,
            IngressDisposition::NoObservableOutcome
        );
        assert!(consumed.actions.events.is_empty());
        assert!(consumed.actions.packets.is_empty());
        assert_eq!(
            initiator.metrics().transport.packets_dropped_dedup,
            initiator_dedup
        );

        // Replaying the identical deterministic FF and FE remains valid Link
        // lifecycle traffic instead of falling into packet deduplication.
        let repeated_response =
            responder.ingest(request.bytes(), request_at + 2, InterfaceId(7), &mut rng);
        assert_eq!(repeated_response.disposition, IngressDisposition::Processed);
        assert!(repeated_response.actions.events.is_empty());
        assert_eq!(repeated_response.actions.packets.len(), 1);
        assert_eq!(
            repeated_response.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(7))
        );
        assert_eq!(
            repeated_response.actions.packets[0].bytes(),
            response.actions.packets[0].bytes()
        );
        assert_eq!(
            responder.metrics().transport.packets_dropped_dedup,
            responder_dedup
        );

        let repeated_consumed = initiator.ingest(
            repeated_response.actions.packets[0].bytes(),
            request_at + 3,
            InterfaceId(3),
            &mut rng,
        );
        assert_eq!(
            repeated_consumed.disposition,
            IngressDisposition::NoObservableOutcome
        );
        assert!(repeated_consumed.actions.events.is_empty());
        assert!(repeated_consumed.actions.packets.is_empty());
        assert_eq!(
            initiator.metrics().transport.packets_dropped_dedup,
            initiator_dedup
        );

        // The responder's RTT is two seconds in this fixture. Even after a
        // complete interval of inbound silence, it must never originate FF.
        let responder_keepalive_micros = responder
            .link_snapshot_for_conformance(&link_id)
            .unwrap()
            .keepalive_interval
            .as_micros();
        let responder_keepalive = responder_keepalive_micros.saturating_add(999_999) / 1_000_000;
        let responder_tick = responder.tick(request_at + 2 + responder_keepalive, &mut rng);
        assert!(responder_tick.packets.iter().all(|packet| {
            Packet::parse(packet.bytes()).is_ok_and(|parsed| parsed.context != CONTEXT_KEEPALIVE)
        }));
        assert!(matches!(
            responder_tick.events.as_slice(),
            [ApplicationEvent::Tick {
                closed_links: 0,
                ..
            }]
        ));
        assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));
    }

    #[test]
    fn wrapper_resolves_source_relative_routing_and_completes_link_lifecycle() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();

        let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
        assert_eq!(request.target, TxTarget::All);

        let response = responder.ingest(&request.bytes, 100, InterfaceId(7), &mut rng);
        assert_eq!(response.disposition, IngressDisposition::Processed);
        assert_eq!(
            response.metadata.wire_packet_type(),
            Some(PacketType::LinkRequest)
        );
        assert_eq!(response.metadata.emitted_packets(), 1);
        assert_eq!(response.metadata.generated_proof_actions(), 0);
        assert_eq!(response.metadata.generated_proof_tag(), None);
        assert!(response.actions.events.is_empty());
        assert_eq!(
            responder.metrics().ingress.premature_link_events_suppressed,
            0
        );
        assert_eq!(response.actions.packets.len(), 1);
        assert_eq!(
            response.actions.packets[0].target,
            TxTarget::Only(InterfaceId(7))
        );
        let proof = Packet::parse(&response.actions.packets[0].bytes).unwrap();
        assert_eq!(proof.context, CONTEXT_LRPROOF);

        let established = initiator.ingest(
            &response.actions.packets[0].bytes,
            101,
            InterfaceId(3),
            &mut rng,
        );
        assert_eq!(initiator.link_state(&link_id), Some(LinkState::Active));
        assert_eq!(established.actions.packets.len(), 1);
        assert_eq!(
            established.actions.packets[0].target,
            TxTarget::Only(InterfaceId(3))
        );
        let lrrtt = Packet::parse(&established.actions.packets[0].bytes).unwrap();
        assert_eq!(lrrtt.context, CONTEXT_LRRTT);

        let active = responder.ingest(
            &established.actions.packets[0].bytes,
            102,
            InterfaceId(7),
            &mut rng,
        );
        assert_eq!(active.disposition, IngressDisposition::Processed);
        assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));

        let oversized = [0xA5; rete_transport::LINK_MDU + 1];
        assert_eq!(
            initiator.send_link_data(&link_id, &oversized, 109, &mut rng),
            Err(EmbeddedSendError::LinkPayloadTooLarge {
                actual: rete_transport::LINK_MDU + 1,
                maximum: rete_transport::LINK_MDU,
            })
        );
        let data = initiator
            .send_link_data(&link_id, b"bounded link payload", 110, &mut rng)
            .unwrap();
        assert_eq!(data.target(), TxTarget::Only(InterfaceId(3)));
        let dedup_before = responder.metrics().transport.packets_dropped_dedup;
        let wrong_interface = responder.ingest(&data.bytes, 110, InterfaceId(8), &mut rng);
        assert_eq!(
            wrong_interface.disposition,
            IngressDisposition::NativeInvalid
        );
        assert!(wrong_interface.actions.events.is_empty());
        assert_eq!(
            responder.metrics().transport.packets_dropped_dedup,
            dedup_before
        );
        let received = responder.ingest(&data.bytes, 110, InterfaceId(7), &mut rng);
        assert!(matches!(
            received.actions.events.first(),
            Some(ApplicationEvent::LinkData { binding, data, .. })
                if binding.link() == link_id.as_bytes()
                    && binding.destination() == responder.destination_hash().as_bytes()
                    && binding.role() == ApplicationLinkRole::Responder
                    && data == b"bounded link payload"
        ));
        assert_eq!(
            responder.metrics().transport.packets_dropped_dedup,
            dedup_before
        );
        assert_eq!(initiator.metrics().admission.link_payload_too_large, 1);
    }

    #[test]
    fn link_data_binding_uses_the_registered_secondary_destination() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let delivery = responder
            .register_destination(
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                DestinationType::Single,
                Direction::In,
            )
            .unwrap();
        assert!(responder.set_accepts_links(&delivery, true));
        initiator
            .register_peer(
                &identity(2),
                LXMF_APPLICATION_NAME,
                &[LXMF_DELIVERY_ASPECT],
                0,
            )
            .unwrap();
        let mut rng = CounterRng::default();

        let (request, link_id) = initiator.initiate_link(delivery, 100, &mut rng).unwrap();
        let proof = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
        assert_eq!(proof.actions.packets.len(), 1);
        let established = initiator.ingest(
            proof.actions.packets[0].bytes(),
            101,
            InterfaceId(3),
            &mut rng,
        );
        assert_eq!(established.actions.packets.len(), 1);
        let active = responder.ingest(
            established.actions.packets[0].bytes(),
            102,
            InterfaceId(7),
            &mut rng,
        );
        assert_eq!(active.disposition, IngressDisposition::Processed);
        assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));

        let data = initiator
            .send_link_data(&link_id, b"secondary destination payload", 110, &mut rng)
            .unwrap();
        let received = responder.ingest(data.bytes(), 110, InterfaceId(7), &mut rng);
        assert!(matches!(
            received.actions.events.as_slice(),
            [ApplicationEvent::LinkData {
                binding,
                data,
                context: LINK_DATA_CONTEXT_NONE,
            }] if binding.link() == link_id.as_bytes()
                && binding.destination() == delivery.as_bytes()
                && binding.role() == ApplicationLinkRole::Responder
                && data == b"secondary destination payload"
        ));
    }

    #[test]
    fn responder_link_data_proof_is_exact_withheld_and_released_by_ownership() {
        let mut initiator = node(1);
        let mut responder = node(2);
        responder.set_inbound_proof_policy(InboundProofPolicy::Retain);
        let mut rng = CounterRng::default();
        let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

        let data = initiator
            .send_link_data(&link_id, b"durable direct LXMF carrier", 110, &mut rng)
            .unwrap();
        let packet_hash = Packet::parse(data.bytes()).unwrap().compute_hash();
        let preflight = responder
            .preflight_ingest(data.bytes(), InterfaceId(7))
            .unwrap();
        assert!(matches!(
            preflight.proof_expectation,
            Some(InboundProofExpectation::ResponderLinkData(
                ResponderLinkProofExpectation {
                    link_id: expected_link_id,
                    packet_hash: expected_packet_hash,
                    delivery: ResponderLinkProofDelivery::Retain,
                }
            )) if expected_link_id == link_id && expected_packet_hash == packet_hash
        ));

        let wrong_interface = responder.ingest(data.bytes(), 110, InterfaceId(8), &mut rng);
        assert_eq!(
            wrong_interface.disposition,
            IngressDisposition::NativeInvalid
        );
        assert!(wrong_interface.actions.events.is_empty());
        assert!(wrong_interface.actions.packets.is_empty());
        assert_eq!(wrong_interface.actions.retained_proof_count(), 0);

        let received = responder.ingest(data.bytes(), 110, InterfaceId(7), &mut rng);
        assert_eq!(received.disposition, IngressDisposition::Processed);
        assert_eq!(received.metadata.emitted_packets(), 0);
        assert_eq!(received.metadata.generated_proof_actions(), 0);
        assert!(received.actions.packets.is_empty());
        assert_eq!(received.actions.retained_proof_count(), 1);
        assert!(matches!(
            received.actions.events.as_slice(),
            [ApplicationEvent::LinkData {
                binding,
                data,
                context: LINK_DATA_CONTEXT_NONE,
            }] if binding.link() == link_id.as_bytes()
                && binding.destination() == responder.destination_hash().as_bytes()
                && binding.role() == ApplicationLinkRole::Responder
                && data == b"durable direct LXMF carrier"
        ));

        let retained = received.actions.events.retained_proof().unwrap();
        assert_eq!(retained.event_index(), 0);
        let proof_pointer = retained.proof.packet.bytes.as_ptr();
        let proof = &retained.proof.packet;
        assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));
        assert_eq!(proof.protocol_token(), None);
        assert_eq!(proof.bytes().len(), 115);
        let parsed = Packet::parse(proof.bytes()).unwrap();
        assert_eq!(parsed.flags, 0x0f);
        assert_eq!(parsed.hops, 0);
        assert_eq!(parsed.header_type, HeaderType::Header1);
        assert_eq!(parsed.packet_type, PacketType::Proof);
        assert_eq!(parsed.dest_type, DestType::Link);
        assert_eq!(parsed.destination_hash, link_id.as_ref());
        assert_eq!(parsed.context, CONTEXT_NONE);
        assert_eq!(parsed.payload.len(), 96);
        assert_eq!(&parsed.payload[..32], &packet_hash);
        let peer_signing_key = initiator
            .core
            .transport
            .get_link(&link_id)
            .unwrap()
            .peer_ed25519_pub;
        assert!(
            Identity::verify_raw_ed25519(&peer_signing_key, &packet_hash, &parsed.payload[32..96],)
                .is_ok()
        );

        let mut event_slots = [crate::ApplicationEventSlot::new()];
        let mut event_owner = crate::ApplicationEventOwner::new(&mut event_slots);
        event_owner
            .try_offer_actions(received.actions)
            .expect("the Link event and proof move into one owner");
        let lease = event_owner.lease_next().unwrap();
        assert!(lease.has_retained_proof());
        assert_eq!(lease.event().kind(), ApplicationEventKind::LinkData);

        let mut proof_slots = [crate::DelayedProofSlot::new()];
        let mut proof_owner = crate::DelayedProofOwner::new(&mut proof_slots);
        let acknowledged = lease
            .try_reserve_delayed(&mut proof_owner)
            .unwrap()
            .acknowledge_into_ready();
        assert_eq!(acknowledged.event().kind(), ApplicationEventKind::LinkData);
        drop(acknowledged.into_event());

        let released = proof_owner.lease_next().unwrap().release_actions();
        assert!(released.events.is_empty());
        assert_eq!(released.packets.len(), 1);
        assert_eq!(released.packets[0].bytes().as_ptr(), proof_pointer);
        assert_eq!(released.packets[0].target(), TxTarget::Only(InterfaceId(7)));
        assert_eq!(released.packets[0].bytes().len(), 115);
    }

    #[test]
    fn always_link_proof_is_immediate_exact_and_has_no_retained_owner() {
        let mut initiator = node(1);
        let mut responder = node(2);
        responder.set_inbound_proof_policy(InboundProofPolicy::Always);
        let mut rng = CounterRng::default();
        let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

        let data = initiator
            .send_link_data(&link_id, b"immediate direct LXMF carrier", 110, &mut rng)
            .unwrap();
        let packet_hash = Packet::parse(data.bytes()).unwrap().compute_hash();
        let preflight = responder
            .preflight_ingest(data.bytes(), InterfaceId(7))
            .unwrap();
        assert!(matches!(
            preflight.proof_expectation,
            Some(InboundProofExpectation::ResponderLinkData(
                ResponderLinkProofExpectation {
                    link_id: expected_link_id,
                    packet_hash: expected_packet_hash,
                    delivery: ResponderLinkProofDelivery::Immediate,
                }
            )) if expected_link_id == link_id && expected_packet_hash == packet_hash
        ));

        let received = responder.ingest(data.bytes(), 110, InterfaceId(7), &mut rng);
        assert_eq!(received.disposition, IngressDisposition::Processed);
        assert_eq!(received.metadata.emitted_packets(), 1);
        assert_eq!(received.metadata.generated_proof_actions(), 1);
        assert_eq!(received.actions.retained_proof_count(), 0);
        assert!(matches!(
            received.actions.events.as_slice(),
            [ApplicationEvent::LinkData {
                binding,
                data,
                context: LINK_DATA_CONTEXT_NONE,
            }] if binding.link() == link_id.as_bytes()
                && binding.destination() == responder.destination_hash().as_bytes()
                && binding.role() == ApplicationLinkRole::Responder
                && data == b"immediate direct LXMF carrier"
        ));
        assert_eq!(received.actions.packets.len(), 1);
        let proof = &received.actions.packets[0];
        assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));
        assert_eq!(proof.protocol_token(), None);
        let parsed = Packet::parse(proof.bytes()).unwrap();
        assert_eq!(parsed.flags, 0x0f);
        assert_eq!(parsed.hops, 0);
        assert_eq!(parsed.header_type, HeaderType::Header1);
        assert_eq!(parsed.packet_type, PacketType::Proof);
        assert_eq!(parsed.dest_type, DestType::Link);
        assert_eq!(parsed.destination_hash, link_id.as_ref());
        assert_eq!(parsed.context, CONTEXT_NONE);
        assert_eq!(parsed.payload.len(), 96);
        assert_eq!(&parsed.payload[..32], &packet_hash);
        let peer_signing_key = initiator
            .core
            .transport
            .get_link(&link_id)
            .unwrap()
            .peer_ed25519_pub;
        assert!(
            Identity::verify_raw_ed25519(&peer_signing_key, &packet_hash, &parsed.payload[32..96],)
                .is_ok()
        );

        let replay = responder.ingest(data.bytes(), 111, InterfaceId(7), &mut rng);
        assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
        assert!(replay.actions.events.is_empty());
        assert!(replay.actions.packets.is_empty());
        assert_eq!(replay.actions.retained_proof_count(), 0);

        let invalid = initiator
            .send_link_data(&link_id, b"wrong interface", 112, &mut rng)
            .unwrap();
        let invalid = responder.ingest(invalid.bytes(), 112, InterfaceId(8), &mut rng);
        assert_eq!(invalid.disposition, IngressDisposition::NativeInvalid);
        assert!(invalid.actions.events.is_empty());
        assert!(invalid.actions.packets.is_empty());
        assert_eq!(invalid.actions.retained_proof_count(), 0);
    }

    #[test]
    fn retained_link_proof_requires_role_context_binding_and_proof_policy() {
        let mut initiator = node(1);
        let mut responder = node(2);
        responder.set_inbound_proof_policy(InboundProofPolicy::Retain);
        let mut rng = CounterRng::default();
        let link_id = establish_bound_link(&mut initiator, &mut responder, &mut rng);

        let other_context = initiator
            .core
            .transport
            .build_link_data_packet(&link_id, b"other context", 0x5a, &mut rng)
            .unwrap();
        let other_context = responder.ingest(&other_context, 110, InterfaceId(7), &mut rng);
        assert_eq!(other_context.actions.retained_proof_count(), 0);
        assert!(other_context.actions.packets.is_empty());
        assert!(matches!(
            other_context.actions.events.as_slice(),
            [ApplicationEvent::LinkData {
                binding,
                context: 0x5a,
                ..
            }] if binding.role() == ApplicationLinkRole::Responder
        ));

        let reverse = responder
            .send_link_data(&link_id, b"initiator-side receive", 111, &mut rng)
            .unwrap();
        let reverse = initiator.ingest(reverse.bytes(), 111, InterfaceId(3), &mut rng);
        assert_eq!(reverse.actions.retained_proof_count(), 0);
        assert!(reverse.actions.packets.is_empty());
        assert!(matches!(
            reverse.actions.events.as_slice(),
            [ApplicationEvent::LinkData { binding, .. }]
                if binding.role() == ApplicationLinkRole::Initiator
        ));

        responder.set_inbound_proof_policy(InboundProofPolicy::Never);
        let non_retaining = initiator
            .send_link_data(&link_id, b"non-retaining destination", 112, &mut rng)
            .unwrap();
        let non_retaining = responder.ingest(non_retaining.bytes(), 112, InterfaceId(7), &mut rng);
        assert_eq!(non_retaining.actions.retained_proof_count(), 0);
        assert!(non_retaining.actions.packets.is_empty());
        assert!(matches!(
            non_retaining.actions.events.as_slice(),
            [ApplicationEvent::LinkData { binding, .. }]
                if binding.role() == ApplicationLinkRole::Responder
        ));

        responder.set_inbound_proof_policy(InboundProofPolicy::Retain);
        assert!(responder.set_accepts_links(&responder.destination_hash(), false));
        let links_disabled = initiator
            .send_link_data(&link_id, b"links now disabled", 113, &mut rng)
            .unwrap();
        let links_disabled =
            responder.ingest(links_disabled.bytes(), 113, InterfaceId(7), &mut rng);
        assert_eq!(links_disabled.disposition, IngressDisposition::Processed);
        assert_eq!(links_disabled.actions.retained_proof_count(), 1);
        assert!(links_disabled.actions.packets.is_empty());
        assert!(matches!(
            links_disabled.actions.events.as_slice(),
            [ApplicationEvent::LinkData {
                binding,
                data,
                context: LINK_DATA_CONTEXT_NONE,
            }] if binding.link() == link_id.as_bytes()
                && binding.role() == ApplicationLinkRole::Responder
                && data == b"links now disabled"
        ));

        assert!(responder.set_accepts_links(&responder.destination_hash(), true));
        responder
            .core
            .transport
            .get_link_mut(&link_id)
            .unwrap()
            .state = LinkState::Stale;
        let stale = initiator
            .send_link_data(&link_id, b"revive stale responder", 114, &mut rng)
            .unwrap();
        let stale = responder.ingest(stale.bytes(), 114, InterfaceId(7), &mut rng);
        assert_eq!(stale.actions.retained_proof_count(), 1);
        assert!(stale.actions.packets.is_empty());
        assert_eq!(responder.link_state(&link_id), Some(LinkState::Active));
    }

    #[test]
    fn inbound_link_capacity_rejects_before_emitting_a_false_proof() {
        let mut responder = TwoLinkNode::new(
            identity(9),
            "reticulum",
            &["embedded"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let responder_identity = identity(9);
        let destination = responder.destination_hash();
        let mut rng = CounterRng::default();
        let mut first_link_id = None;
        for (tag, now) in [(10, 1), (11, 2)] {
            let mut initiator = TwoLinkNode::new(
                identity(tag),
                "reticulum",
                &["initiator"],
                EmbeddedNodeConfig::endpoint(),
            )
            .unwrap();
            initiator
                .register_peer(&responder_identity, "reticulum", &["embedded"], 0)
                .unwrap();
            let (request, link_id) = initiator.initiate_link(destination, now, &mut rng).unwrap();
            first_link_id.get_or_insert(link_id);
            let result = responder.ingest(&request.bytes, now, InterfaceId(now as u8), &mut rng);
            assert_eq!(result.disposition, IngressDisposition::Processed);
        }
        assert!(
            !responder.can_initiate_link(),
            "inbound responder Links occupy the same owned-Link table as local initiators"
        );

        let mut overflow = TwoLinkNode::new(
            identity(12),
            "reticulum",
            &["overflow"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        overflow
            .register_peer(&responder_identity, "reticulum", &["embedded"], 0)
            .unwrap();
        let (overflow_request, overflow_link_id) =
            overflow.initiate_link(destination, 3, &mut rng).unwrap();
        let rng_before_rejection = rng.0;
        let received_before_rejection = responder.metrics().transport.packets_received;
        let overflow_result =
            responder.ingest(&overflow_request.bytes, 3, InterfaceId(3), &mut rng);
        assert_eq!(
            overflow_result.disposition,
            IngressDisposition::Rejected(IngressDropReason::OwnedLinkTableFull { limit: 2 })
        );
        assert!(overflow_result.actions.events.is_empty());
        assert!(overflow_result.actions.packets.is_empty());
        let rejected_metrics = responder.metrics();
        assert_eq!(rng.0, rng_before_rejection);
        assert_eq!(
            rejected_metrics.transport.packets_received,
            received_before_rejection
        );
        assert_eq!(rejected_metrics.ingress.owned_link_full, 1);
        assert_eq!(rejected_metrics.capacity.links.used, 2);

        let immediate_replay =
            responder.ingest(&overflow_request.bytes, 4, InterfaceId(3), &mut rng);
        assert_eq!(
            immediate_replay.disposition,
            IngressDisposition::NativeDuplicate
        );
        assert!(immediate_replay.actions.events.is_empty());
        assert!(immediate_replay.actions.packets.is_empty());
        assert_eq!(responder.metrics().ingress.owned_link_full, 1);
        assert_eq!(responder.metrics().capacity.links.used, 2);
        assert_eq!(rng.0, rng_before_rejection);

        let close = responder.close_link(&first_link_id.unwrap(), &mut rng);
        assert_eq!(close.packets.len(), 1);
        assert_eq!(close.events.len(), 1);
        assert_eq!(close.unroutable_packets, 0);
        assert_eq!(responder.metrics().capacity.links.used, 1);
        assert!(responder.can_initiate_link());

        let replay = responder.ingest(&overflow_request.bytes, 5, InterfaceId(3), &mut rng);
        assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
        assert!(replay.actions.events.is_empty());
        assert!(replay.actions.packets.is_empty());
        assert_eq!(responder.metrics().capacity.links.used, 1);

        let (fresh_request, fresh_link_id) =
            overflow.initiate_link(destination, 6, &mut rng).unwrap();
        assert_ne!(fresh_link_id, overflow_link_id);
        let fresh = responder.ingest(&fresh_request.bytes, 6, InterfaceId(3), &mut rng);
        assert_eq!(fresh.disposition, IngressDisposition::Processed);
        assert_eq!(fresh.actions.packets.len(), 1);
        assert_eq!(responder.metrics().capacity.links.used, 2);
    }

    #[test]
    fn inbound_handshake_timeout_recovers_embedded_capacity_without_close_packets() {
        let mut responder = TwoLinkNode::new(
            identity(30),
            "reticulum",
            &["embedded-timeout"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let responder_identity = identity(30);
        let destination = responder.destination_hash();
        let mut rng = CounterRng::default();

        for (tag, now) in [(31, 1), (32, 2)] {
            let mut initiator = TwoLinkNode::new(
                identity(tag),
                "reticulum",
                &["timeout-initiator"],
                EmbeddedNodeConfig::endpoint(),
            )
            .unwrap();
            initiator
                .register_peer(&responder_identity, "reticulum", &["embedded-timeout"], 0)
                .unwrap();
            let (request, _) = initiator.initiate_link(destination, now, &mut rng).unwrap();
            let admitted = responder.ingest(&request.bytes, now, InterfaceId(now as u8), &mut rng);
            assert_eq!(admitted.disposition, IngressDisposition::Processed);
            assert_eq!(admitted.actions.packets.len(), 1);
        }

        assert_eq!(responder.metrics().capacity.links.used, 2);
        assert!(!responder.can_initiate_link());
        let failed_before = responder.metrics().transport.links_failed;
        let closed_before = responder.metrics().transport.links_closed;

        let before_deadline = responder.tick(366, &mut rng);
        assert!(before_deadline.packets.is_empty());
        assert!(matches!(
            before_deadline.events.as_slice(),
            [ApplicationEvent::Tick {
                closed_links: 0,
                ..
            }]
        ));
        assert_eq!(responder.metrics().capacity.links.used, 2);

        // Direct ingress stores one post-ingress hop, so the first responder
        // expires at 1 + 360 + 6 seconds while the second remains retained.
        let first_deadline = responder.tick(367, &mut rng);
        assert!(first_deadline.packets.is_empty());
        assert!(matches!(
            first_deadline.events.as_slice(),
            [ApplicationEvent::Tick {
                closed_links: 1,
                ..
            }]
        ));
        assert_eq!(responder.metrics().capacity.links.used, 1);
        assert!(responder.can_initiate_link());

        let second_deadline = responder.tick(368, &mut rng);
        assert!(second_deadline.packets.is_empty());
        assert!(matches!(
            second_deadline.events.as_slice(),
            [ApplicationEvent::Tick {
                closed_links: 1,
                ..
            }]
        ));
        let final_metrics = responder.metrics();
        assert_eq!(final_metrics.capacity.links.used, 0);
        assert_eq!(final_metrics.transport.links_failed, failed_before);
        assert_eq!(
            final_metrics.transport.links_closed,
            closed_before.saturating_add(2)
        );
    }

    #[test]
    fn transport_rejects_arbitrary_header1_relay_link_without_interface_roles() {
        let mut relay = TestNode::new(
            identity(20),
            "reticulum",
            &["relay"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let mut initiator = node(21);
        let responder = node(22);
        let mut rng = CounterRng::default();

        let (request, _) = initiator
            .initiate_link(responder.destination_hash(), 1, &mut rng)
            .unwrap();
        let result = relay.ingest(&request.bytes, 1, InterfaceId(4), &mut rng);
        assert_eq!(
            result.disposition,
            IngressDisposition::Rejected(IngressDropReason::Header1RemoteLinkRequestDisabled)
        );
        assert_eq!(relay.metrics().transport.packets_received, 0);
    }

    #[test]
    fn header2_relay_link_capacity_is_typed_and_transactional() {
        let mut relay = TestNode::new(
            identity(30),
            "reticulum",
            &["relay"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let relay_identity = relay.identity_hash();
        let mut rng = CounterRng::default();
        let mut overflow_request = None;

        for index in 0..3_u8 {
            let responder_tag = 31 + index;
            let responder_identity = identity(responder_tag);
            let responder = node(responder_tag);
            let destination = responder.destination_hash();
            relay
                .register_peer(
                    &responder_identity,
                    "reticulum",
                    &["embedded"],
                    u64::from(index),
                )
                .unwrap();
            let mut relay_path = rete_transport::Path::direct(u64::from(index));
            relay_path.received_on = Some(9);
            assert!(relay.core.transport.insert_path(destination, relay_path));

            let mut initiator = node(40 + index);
            initiator
                .register_peer(
                    &responder_identity,
                    "reticulum",
                    &["embedded"],
                    u64::from(index),
                )
                .unwrap();
            let mut initiator_path =
                rete_transport::Path::via_repeater(relay_identity, 2, u64::from(index));
            initiator_path.received_on = Some(4);
            assert!(
                initiator
                    .core
                    .transport
                    .insert_path(destination, initiator_path)
            );
            let (request, _) = initiator
                .initiate_link(destination, u64::from(index + 1), &mut rng)
                .unwrap();
            assert_eq!(
                Packet::parse(request.bytes()).unwrap().transport_id,
                Some(relay_identity.as_ref())
            );

            let report = relay.ingest(
                request.bytes(),
                u64::from(index + 1),
                InterfaceId(4),
                &mut rng,
            );
            if index < 2 {
                assert_eq!(report.disposition, IngressDisposition::Processed);
                assert_eq!(report.actions.packets.len(), 1);
                assert_eq!(
                    report.actions.packets[0].target(),
                    TxTarget::Only(InterfaceId(9))
                );
            } else {
                assert_eq!(
                    report.disposition,
                    IngressDisposition::Rejected(IngressDropReason::RelayLinkTableFull {
                        limit: 2,
                    })
                );
                assert!(report.actions.packets.is_empty());
                overflow_request = Some(request);
            }
        }

        let metrics = relay.metrics();
        assert_eq!(metrics.capacity.relay_links.used, 2);
        assert_eq!(metrics.capacity.links.used, 0);
        assert_eq!(metrics.ingress.relay_link_full, 1);
        assert_eq!(metrics.transport.packets_received, 3);

        let replay = relay.ingest(
            overflow_request.as_ref().unwrap().bytes(),
            10,
            InterfaceId(4),
            &mut rng,
        );
        assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
        assert_eq!(relay.metrics().capacity.relay_links.used, 2);
    }

    #[test]
    fn header2_policy_filters_other_transports_and_admits_owned_local_termination() {
        let mut endpoint = node(23);
        let mut rng = CounterRng::default();
        let other_transport = [0xEE; rete_core::TRUNCATED_HASH_LEN];
        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(endpoint.destination_hash().as_ref())
            .context(0)
            .payload(b"not for this transport")
            .via(Some(&other_transport))
            .build()
            .unwrap();
        let rejected = endpoint.ingest(&raw[..len], 1, InterfaceId(1), &mut rng);
        assert!(matches!(
            rejected.disposition,
            IngressDisposition::Rejected(IngressDropReason::Header2NotAddressedToUs { .. })
        ));
        assert_eq!(endpoint.metrics().transport.packets_received, 0);

        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Link)
            .destination_hash(&[0x7e; rete_core::TRUNCATED_HASH_LEN])
            .context(CONTEXT_RESOURCE_ADV)
            .payload(b"foreign resource")
            .via(Some(&other_transport))
            .build()
            .unwrap();
        let foreign_resource = endpoint.ingest(&raw[..len], 2, InterfaceId(1), &mut rng);
        assert!(matches!(
            foreign_resource.disposition,
            IngressDisposition::Rejected(IngressDropReason::Header2NotAddressedToUs { .. })
        ));
        assert_eq!(endpoint.metrics().ingress.resource_disabled, 0);
        assert_eq!(endpoint.metrics().ingress.header2_filtered, 2);

        let plain_destination = endpoint
            .register_destination(
                "reticulum",
                &["plain"],
                DestinationType::Plain,
                Direction::In,
            )
            .unwrap();
        let endpoint_transport = endpoint.identity_hash();
        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(plain_destination.as_ref())
            .context(0)
            .payload(b"wrong destination type")
            .via(Some(endpoint_transport.as_bytes()))
            .build()
            .unwrap();
        let mismatched = endpoint.ingest(&raw[..len], 3, InterfaceId(1), &mut rng);
        assert_eq!(
            mismatched.disposition,
            IngressDisposition::NoObservableOutcome
        );
        assert!(mismatched.actions.events.is_empty());
        assert!(mismatched.actions.packets.is_empty());

        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Plain)
            .destination_hash(plain_destination.as_ref())
            .context(0)
            .payload(b"local plain payload")
            .via(Some(endpoint_transport.as_bytes()))
            .build()
            .unwrap();
        let admitted = endpoint.ingest(&raw[..len], 4, InterfaceId(1), &mut rng);
        assert!(matches!(
            admitted.actions.events.as_slice(),
            [ApplicationEvent::DataReceived { destination, payload }]
                if *destination == *plain_destination.as_bytes()
                    && payload == b"local plain payload"
        ));

        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Announce)
            .dest_type(DestType::Single)
            .destination_hash(&[0xA4; rete_core::TRUNCATED_HASH_LEN])
            .context(0)
            .payload(b"invalid but dispatchable announce")
            .via(Some(endpoint_transport.as_bytes()))
            .build()
            .unwrap();
        let announce = endpoint.ingest(&raw[..len], 5, InterfaceId(1), &mut rng);
        assert!(!matches!(
            announce.disposition,
            IngressDisposition::Rejected(_)
        ));

        let mut transport = TestNode::new(
            identity(24),
            "reticulum",
            &["embedded"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let plain_destination = transport
            .register_destination(
                "reticulum",
                &["transport-plain"],
                DestinationType::Plain,
                Direction::In,
            )
            .unwrap();
        let transport_identity = transport.identity_hash();
        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Plain)
            .destination_hash(plain_destination.as_ref())
            .context(0)
            .payload(b"must terminate locally")
            .via(Some(transport_identity.as_bytes()))
            .build()
            .unwrap();
        let admitted = transport.ingest(&raw[..len], 2, InterfaceId(1), &mut rng);
        assert_eq!(admitted.disposition, IngressDisposition::Processed);
        assert!(matches!(
            admitted.actions.events.as_slice(),
            [ApplicationEvent::DataReceived { destination, payload }]
                if *destination == *plain_destination.as_bytes()
                    && payload == b"must terminate locally"
        ));
        assert_eq!(transport.metrics().ingress.header2_filtered, 0);
        assert_eq!(transport.metrics().transport.packets_received, 1);
    }

    #[test]
    fn owned_header2_link_request_uses_canonical_local_admission() {
        let mut responder = TestNode::new(
            identity(26),
            "reticulum",
            &["embedded"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let destination = responder.destination_hash();
        let responder_identity = identity(26);
        let mut initiator = node(27);
        initiator
            .register_peer(&responder_identity, "reticulum", &["embedded"], 1)
            .unwrap();
        let mut path = rete_transport::Path::via_repeater(responder.identity_hash(), 1, 1);
        path.received_on = Some(3);
        assert!(initiator.core.transport.insert_path(destination, path));
        let mut rng = CounterRng::default();
        let (request, link_id) = initiator.initiate_link(destination, 2, &mut rng).unwrap();
        assert_eq!(request.target(), TxTarget::Only(InterfaceId(3)));
        let parsed = Packet::parse(request.bytes()).unwrap();
        assert_eq!(parsed.header_type, HeaderType::Header2);
        assert_eq!(
            parsed.transport_id,
            Some(responder.identity_hash().as_ref())
        );

        let report = responder.ingest(request.bytes(), 3, InterfaceId(8), &mut rng);
        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert!(report.actions.events.is_empty());
        assert_eq!(report.actions.packets.len(), 1);
        assert_eq!(
            report.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(8))
        );
        assert_eq!(responder.link_state(&link_id), Some(LinkState::Handshake));
        assert_eq!(responder.metrics().capacity.links.used, 1);
        assert_eq!(responder.metrics().capacity.relay_links.used, 0);
    }

    #[test]
    fn endpoint_never_forwards_an_unmatched_proof() {
        let mut endpoint = node(25);
        let mut rng = CounterRng::default();
        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Proof)
            .dest_type(DestType::Single)
            .destination_hash(&[0xA5; rete_core::TRUNCATED_HASH_LEN])
            .context(0)
            .payload(b"unknown proof")
            .build()
            .unwrap();
        let report = endpoint.ingest(&raw[..len], 1, InterfaceId(1), &mut rng);
        assert_eq!(report.disposition, IngressDisposition::NoObservableOutcome);
        assert!(report.actions.packets.is_empty());
        assert_eq!(endpoint.metrics().ingress.endpoint_forward_suppressed, 1);
    }

    #[test]
    fn endpoint_ingress_policy_retains_owned_exact_and_suppresses_propagation() {
        assert!(endpoint_retains_ingress_packet(PacketRouting::All));
        assert!(endpoint_retains_ingress_packet(
            PacketRouting::SourceInterface
        ));
        assert!(endpoint_retains_ingress_packet(
            PacketRouting::ExactInterface(9)
        ));
        assert!(endpoint_retains_ingress_packet(
            PacketRouting::BoundInterface(9)
        ));
        assert!(!endpoint_retains_ingress_packet(
            PacketRouting::AllExceptSource
        ));
    }

    #[test]
    fn origin_packet_resolution_preserves_absolute_routing() {
        let exact = resolve_origin_packet(OutboundPacket::new(
            vec![0xa5],
            PacketRouting::ExactInterface(7),
        ));
        assert_eq!(exact.bytes(), &[0xa5]);
        assert_eq!(exact.target(), TxTarget::Only(InterfaceId(7)));

        let bound = resolve_origin_packet(OutboundPacket::new(
            vec![0xb5],
            PacketRouting::BoundInterface(8),
        ));
        assert_eq!(bound.bytes(), &[0xb5]);
        assert_eq!(bound.target(), TxTarget::Only(InterfaceId(8)));

        let broadcast = resolve_origin_packet(OutboundPacket::broadcast(vec![0xb6]));
        assert_eq!(broadcast.bytes(), &[0xb6]);
        assert_eq!(broadcast.target(), TxTarget::All);
    }

    #[test]
    #[should_panic(expected = "pinned Rete origin API emitted source-relative routing")]
    fn origin_packet_resolution_rejects_source_context() {
        let _ = resolve_origin_packet(OutboundPacket::new(
            vec![0xc7],
            PacketRouting::SourceInterface,
        ));
    }

    #[test]
    fn tick_resolves_absolute_routes_and_counts_source_dependent_routes() {
        let actions = resolve_tick_actions(
            IngestOutcome {
                events: Vec::new(),
                packets: vec![
                    OutboundPacket::broadcast(vec![0xa1]),
                    OutboundPacket::new(vec![0xb2], PacketRouting::ExactInterface(9)),
                    OutboundPacket::new(vec![0xb3], PacketRouting::BoundInterface(8)),
                    OutboundPacket::new(vec![0xc3], PacketRouting::SourceInterface),
                    OutboundPacket::new(vec![0xd4], PacketRouting::AllExceptSource),
                ],
                rejection: None,
            },
            Vec::new(),
        );

        assert_eq!(actions.packets.len(), 3);
        assert_eq!(actions.packets[0].bytes(), &[0xa1]);
        assert_eq!(actions.packets[0].target(), TxTarget::All);
        assert_eq!(actions.packets[1].bytes(), &[0xb2]);
        assert_eq!(actions.packets[1].target(), TxTarget::Only(InterfaceId(9)));
        assert_eq!(actions.packets[2].bytes(), &[0xb3]);
        assert_eq!(actions.packets[2].target(), TxTarget::Only(InterfaceId(8)));
        assert_eq!(actions.unroutable_packets, 2);
    }

    #[test]
    fn forwarded_transport_proof_is_not_classified_as_locally_generated() {
        let mut transport = TestNode::new(
            identity(27),
            "reticulum",
            &["transport"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let mut rng = CounterRng::default();
        let mut destination_node = node(28);
        let destination = destination_node.destination_hash();
        destination_node.queue_announce(None, 0, &mut rng).unwrap();
        let announce = destination_node
            .flush_announces(0, &mut rng)
            .into_iter()
            .next()
            .expect("the destination announce must be ready immediately");
        let learned = transport.ingest(announce.bytes(), 0, InterfaceId(7), &mut rng);
        assert_eq!(learned.disposition, IngressDisposition::Processed);
        assert_eq!(
            transport.recall_identity(&destination),
            Some(identity(28).public_key())
        );
        assert_eq!(
            transport
                .route(&destination)
                .and_then(|route| route.received_on),
            Some(InterfaceId(7))
        );
        let mut data = [0u8; rete_core::MTU];
        let data_len = PacketBuilder::new(&mut data)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(0)
            .payload(b"establish reverse entry")
            .build()
            .unwrap();
        let covered_hash = Packet::parse(&data[..data_len]).unwrap().compute_hash();
        let forwarded_data = transport.ingest(&data[..data_len], 1, InterfaceId(1), &mut rng);
        assert_eq!(forwarded_data.disposition, IngressDisposition::Processed);
        assert_eq!(forwarded_data.actions.packets.len(), 1);
        assert_eq!(
            forwarded_data.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(7))
        );

        let mut raw = [0u8; rete_core::MTU];
        let mut proof_payload = [0u8; 96];
        proof_payload[..32].copy_from_slice(&covered_hash);
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Proof)
            .dest_type(DestType::Single)
            .destination_hash(&covered_hash[..rete_core::TRUNCATED_HASH_LEN])
            .context(0)
            .payload(&proof_payload)
            .build()
            .unwrap();

        let report = transport.ingest(&raw[..len], 2, InterfaceId(7), &mut rng);

        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert_eq!(report.actions.packets.len(), 1);
        assert_eq!(report.metadata.emitted_packets(), 1);
        assert_eq!(report.metadata.generated_proof_actions(), 0);
        assert_eq!(report.metadata.generated_proof_tag(), None);
        assert_eq!(
            report.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(1))
        );
    }

    #[test]
    fn transport_rejects_forwarding_before_reverse_state_is_lost() {
        let mut relay = TestNode::new(
            identity(26),
            "reticulum",
            &["relay"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let mut rng = CounterRng::default();

        for tag in 70..74 {
            let peer = identity(tag);
            let destination = node(tag).destination_hash();
            relay
                .register_peer(&peer, "reticulum", &["embedded"], u64::from(tag))
                .unwrap();
            let mut path = rete_transport::Path::direct(u64::from(tag));
            path.received_on = Some(7);
            assert!(relay.core.transport.insert_path(destination, path));
            let mut raw = [0u8; rete_core::MTU];
            let len = PacketBuilder::new(&mut raw)
                .packet_type(PacketType::Data)
                .dest_type(DestType::Single)
                .destination_hash(destination.as_ref())
                .context(0)
                .payload(&[tag])
                .build()
                .unwrap();
            let report = relay.ingest(&raw[..len], u64::from(tag), InterfaceId(tag), &mut rng);
            assert_eq!(report.disposition, IngressDisposition::Processed);
            assert_eq!(report.actions.packets.len(), 1);
            assert_eq!(
                report.actions.packets[0].target(),
                TxTarget::Only(InterfaceId(7))
            );
        }
        assert_eq!(relay.metrics().capacity.reverse_entries.used, 4);

        let overflow_tag = 74;
        let peer = identity(overflow_tag);
        let destination = node(overflow_tag).destination_hash();
        relay
            .register_peer(&peer, "reticulum", &["embedded"], u64::from(overflow_tag))
            .unwrap();
        let mut path = rete_transport::Path::direct(u64::from(overflow_tag));
        path.received_on = Some(7);
        assert!(relay.core.transport.insert_path(destination, path));
        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(0)
            .payload(&[overflow_tag])
            .build()
            .unwrap();
        let report = relay.ingest(
            &raw[..len],
            u64::from(overflow_tag),
            InterfaceId(overflow_tag),
            &mut rng,
        );
        assert!(matches!(
            report.disposition,
            IngressDisposition::Rejected(IngressDropReason::ReverseTableFull { limit: 4, .. })
        ));
        assert!(report.actions.packets.is_empty());
        assert_eq!(relay.metrics().transport.packets_received, 4);
        assert_eq!(relay.metrics().ingress.reverse_table_full, 1);
    }

    #[test]
    fn header1_reverse_shim_defers_unbound_peer_path_to_native_invalid() {
        let mut relay = TestNode::new(
            identity(75),
            "reticulum",
            &["relay"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let peer = identity(76);
        let destination = node(76).destination_hash();
        relay
            .register_peer(&peer, "reticulum", &["embedded"], 1)
            .unwrap();
        assert_eq!(
            relay
                .core
                .transport
                .get_path(&destination)
                .and_then(|path| path.received_on),
            None
        );

        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(0)
            .payload(b"no learned egress")
            .build()
            .unwrap();
        let mut rng = CounterRng::default();

        let report = relay.ingest(&raw[..len], 2, InterfaceId(6), &mut rng);
        assert_eq!(report.disposition, IngressDisposition::NativeInvalid);
        assert!(report.actions.events.is_empty());
        assert!(report.actions.packets.is_empty());
        assert_eq!(relay.metrics().capacity.reverse_entries.used, 0);
        assert_eq!(relay.metrics().ingress.native_invalid, 1);
    }

    #[test]
    fn header2_reverse_capacity_rejection_is_typed_and_deduplicated() {
        let mut relay = TestNode::new(
            identity(80),
            "reticulum",
            &["relay"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let relay_identity = relay.identity_hash();
        let mut rng = CounterRng::default();
        let mut overflow = Vec::new();

        for tag in 81..86_u8 {
            let peer = identity(tag);
            let destination = node(tag).destination_hash();
            relay
                .register_peer(&peer, "reticulum", &["embedded"], u64::from(tag))
                .unwrap();
            let mut path = rete_transport::Path::direct(u64::from(tag));
            path.received_on = Some(7);
            assert!(relay.core.transport.insert_path(destination, path));

            let mut raw = [0u8; rete_core::MTU];
            let len = PacketBuilder::new(&mut raw)
                .packet_type(PacketType::Data)
                .dest_type(DestType::Single)
                .destination_hash(destination.as_ref())
                .context(0)
                .payload(&[tag])
                .via(Some(relay_identity.as_bytes()))
                .build()
                .unwrap();
            let report = relay.ingest(&raw[..len], u64::from(tag), InterfaceId(6), &mut rng);
            if tag < 85 {
                assert_eq!(report.disposition, IngressDisposition::Processed);
                assert_eq!(report.actions.packets.len(), 1);
                assert_eq!(
                    report.actions.packets[0].target(),
                    TxTarget::Only(InterfaceId(7))
                );
            } else {
                assert!(matches!(
                    report.disposition,
                    IngressDisposition::Rejected(IngressDropReason::ReverseTableFull {
                        limit: 4,
                        ..
                    })
                ));
                assert!(report.actions.packets.is_empty());
                overflow.extend_from_slice(&raw[..len]);
            }
        }

        let metrics = relay.metrics();
        assert_eq!(metrics.capacity.reverse_entries.used, 4);
        assert_eq!(metrics.ingress.reverse_table_full, 1);
        assert_eq!(metrics.transport.packets_received, 5);

        let replay = relay.ingest(&overflow, 90, InterfaceId(6), &mut rng);
        assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
        assert_eq!(relay.metrics().capacity.reverse_entries.used, 4);
    }

    #[test]
    fn native_reverse_route_conflict_maps_to_stable_product_diagnostics() {
        let mut node = node(86);
        let truncated_hash = [0x5a; rete_core::TRUNCATED_HASH_LEN];
        let report = node.finish_ingest(
            IngestOutcome {
                events: Vec::new(),
                packets: Vec::new(),
                rejection: Some(IngestRejection::ReverseRouteConflict { truncated_hash }),
            },
            IngressPreflight {
                before_duplicate: 0,
                before_invalid: 0,
                metadata: IngressMetadata::default(),
                proof_expectation: None,
                local_path_request: None,
                path_response: None,
            },
            InterfaceId(1),
            TerminalCommitCounts::default(),
        );
        assert_eq!(
            report.disposition,
            IngressDisposition::Rejected(IngressDropReason::ReverseRouteConflict {
                truncated_hash,
            })
        );
        assert!(report.actions.events.is_empty());
        assert!(report.actions.packets.is_empty());
        assert_eq!(node.metrics().ingress.reverse_route_conflict, 1);
    }

    #[test]
    fn header1_reverse_shim_rejects_route_conflict_without_redirecting_state() {
        let mut relay = TestNode::new(
            identity(87),
            "reticulum",
            &["relay"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let peer = identity(88);
        let destination = node(88).destination_hash();
        relay
            .register_peer(&peer, "reticulum", &["embedded"], 1)
            .unwrap();
        let mut path = rete_transport::Path::direct(1);
        path.received_on = Some(7);
        assert!(relay.core.transport.insert_path(destination, path));
        let mut rng = CounterRng::default();

        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(0)
            .payload(b"stable reverse route")
            .build()
            .unwrap();
        let packet_hash = Packet::parse(&raw[..len]).unwrap().compute_hash();
        let key: [u8; rete_core::TRUNCATED_HASH_LEN] = packet_hash[..rete_core::TRUNCATED_HASH_LEN]
            .try_into()
            .unwrap();
        let first = relay.ingest(&raw[..len], 2, InterfaceId(6), &mut rng);
        assert_eq!(first.disposition, IngressDisposition::Processed);
        assert_eq!(first.actions.packets.len(), 1);
        assert_eq!(
            relay.core.transport.get_reverse(&key).map(|entry| (
                entry.received_on,
                entry.forwarded_to,
                entry.timestamp
            )),
            Some((6, 7, 2))
        );

        // Evict the original full hash from the rolling dedup window while
        // leaving the longer-lived reverse entry intact.
        for tag in 0..8_u8 {
            let mut filler = [0u8; rete_core::MTU];
            let filler_len = PacketBuilder::new(&mut filler)
                .packet_type(PacketType::Proof)
                .dest_type(DestType::Single)
                .destination_hash(&[tag; rete_core::TRUNCATED_HASH_LEN])
                .context(0)
                .payload(&[tag])
                .build()
                .unwrap();
            let _ = relay.ingest(
                &filler[..filler_len],
                u64::from(tag + 3),
                InterfaceId(9),
                &mut rng,
            );
        }

        let conflict = relay.ingest(&raw[..len], 20, InterfaceId(5), &mut rng);
        assert_eq!(
            conflict.disposition,
            IngressDisposition::Rejected(IngressDropReason::ReverseRouteConflict {
                truncated_hash: key,
            })
        );
        assert!(conflict.actions.packets.is_empty());
        assert_eq!(relay.metrics().ingress.reverse_route_conflict, 1);
        assert_eq!(relay.metrics().transport.packets_forwarded, 1);
        assert_eq!(
            relay.core.transport.get_reverse(&key).map(|entry| (
                entry.received_on,
                entry.forwarded_to,
                entry.timestamp
            )),
            Some((6, 7, 2))
        );

        let replay = relay.ingest(&raw[..len], 21, InterfaceId(5), &mut rng);
        assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
        assert_eq!(relay.metrics().transport.packets_forwarded, 1);
        assert_eq!(relay.metrics().capacity.reverse_entries.used, 1);
    }

    #[test]
    fn base_mtu_and_resource_gates_run_before_native_ingest() {
        let mut node = node(30);
        let mut rng = CounterRng::default();
        let oversized = [0u8; rete_core::MTU + 1];
        let oversized_result = node.ingest(&oversized, 1, InterfaceId(1), &mut rng);
        assert!(matches!(
            oversized_result.disposition,
            IngressDisposition::Rejected(IngressDropReason::PacketTooLong { .. })
        ));

        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Link)
            .destination_hash(&[0x44; rete_core::TRUNCATED_HASH_LEN])
            .context(CONTEXT_RESOURCE_ADV)
            .payload(&[0x55; 64])
            .build()
            .unwrap();
        let resource_result = node.ingest(&raw[..len], 2, InterfaceId(1), &mut rng);
        assert_eq!(
            resource_result.disposition,
            IngressDisposition::Rejected(IngressDropReason::ResourceIngressDisabled {
                context: CONTEXT_RESOURCE_ADV
            })
        );
        assert_eq!(node.metrics().transport.packets_received, 0);
    }

    #[test]
    fn channel_receipt_preflight_prevents_native_window_mutation() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
        let response = responder.ingest(&request.bytes, 100, InterfaceId(1), &mut rng);
        let established = initiator.ingest(
            &response.actions.packets[0].bytes,
            101,
            InterfaceId(2),
            &mut rng,
        );
        responder.ingest(
            &established.actions.packets[0].bytes,
            102,
            InterfaceId(1),
            &mut rng,
        );

        initiator
            .send_channel_message(&link_id, 1, b"one", 110, &mut rng)
            .unwrap();
        initiator
            .send_channel_message(&link_id, 2, b"two", 111, &mut rng)
            .unwrap();
        assert_eq!(initiator.metrics().capacity.channel_receipts.used, 2);
        assert_eq!(
            initiator.send_channel_message(&link_id, 3, b"three", 112, &mut rng),
            Err(EmbeddedSendError::ChannelReceiptTableFull { limit: 2 })
        );
        assert_eq!(initiator.metrics().capacity.channel_receipts.used, 2);
    }

    #[test]
    fn wrapper_tick_preflights_channel_retry_route_before_entropy_or_state() {
        let mut initiator = node(1);
        let responder = node(2);
        let mut rng = CounterRng::default();
        let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
        let request = Packet::parse(request.bytes()).unwrap();
        let responder_link =
            rete_transport::Link::from_request(link_id, request.payload, &mut rng, 100).unwrap();
        let responder_identity = identity(2);
        let proof = responder_link.build_proof(&responder_identity).unwrap();
        let link = initiator.core.transport.get_link_mut(&link_id).unwrap();
        link.validate_proof(&proof, &responder_identity).unwrap();
        link.activate(100);
        assert_eq!(link.bound_interface(), None);

        initiator
            .core
            .transport
            .send_channel_message(&link_id, 0x4242, b"route before retry", 200, &mut rng)
            .unwrap();
        let link = initiator.core.transport.get_link(&link_id).unwrap();
        let last_outbound = link.last_outbound;
        let window = link.channel().unwrap().window();
        let rng_before_tick = rng.0;

        let actions = initiator.tick(216, &mut rng);
        assert!(actions.packets.is_empty());
        assert_eq!(actions.unroutable_packets, 0);
        assert_eq!(rng.0, rng_before_tick);
        let link = initiator.core.transport.get_link(&link_id).unwrap();
        assert_eq!(link.last_outbound, last_outbound);
        assert_eq!(link.channel().unwrap().window(), window);
        assert_eq!(link.channel().unwrap().pending_count(), 1);
        assert_eq!(initiator.metrics().capacity.channel_receipts.used, 1);
        assert!(matches!(
            initiator
                .core
                .transport
                .pending_channel_maintenance(216)
                .as_slice(),
            [rete_transport::ChannelMaintenanceAction::Retransmit(_)]
        ));
    }

    #[test]
    fn channel_payload_preflight_runs_before_native_queue_mutation() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
        let response = responder.ingest(request.bytes(), 100, InterfaceId(1), &mut rng);
        let established = initiator.ingest(
            response.actions.packets[0].bytes(),
            101,
            InterfaceId(2),
            &mut rng,
        );
        responder.ingest(
            established.actions.packets[0].bytes(),
            102,
            InterfaceId(1),
            &mut rng,
        );

        let oversized = [0xA5; MAX_CHANNEL_PAYLOAD + 1];
        assert_eq!(
            initiator.send_channel_message(&link_id, 1, &oversized, 110, &mut rng),
            Err(EmbeddedSendError::ChannelPayloadTooLarge {
                actual: MAX_CHANNEL_PAYLOAD + 1,
                maximum: MAX_CHANNEL_PAYLOAD,
            })
        );
        initiator
            .send_channel_message(&link_id, 2, b"one", 111, &mut rng)
            .unwrap();
        initiator
            .send_channel_message(&link_id, 3, b"two", 112, &mut rng)
            .unwrap();
        assert_eq!(initiator.metrics().capacity.channel_receipts.used, 2);
        assert_eq!(initiator.metrics().admission.channel_payload_too_large, 1);
    }

    #[test]
    fn outbound_link_admission_is_mandatory_on_the_owning_type() {
        let mut node = node(40);
        let mut rng = CounterRng::default();
        assert!(node.can_initiate_link());
        for tag in [1, 2] {
            node.initiate_link(
                DestHash::from([tag; rete_core::TRUNCATED_HASH_LEN]),
                u64::from(tag),
                &mut rng,
            )
            .unwrap();
        }
        assert!(!node.can_initiate_link());
        assert_eq!(node.metrics().capacity.links.used, 2);
        let rng_before_rejection = rng.0;
        assert_eq!(
            node.initiate_link(
                DestHash::from([3; rete_core::TRUNCATED_HASH_LEN]),
                3,
                &mut rng,
            ),
            Err(LinkAdmissionError::LinkTableFull { limit: 2 })
        );
        assert_eq!(rng.0, rng_before_rejection);
        assert_eq!(node.metrics().admission.outbound_link_full, 1);
    }

    #[test]
    fn outbound_link_id_collision_preserves_state_and_is_counted() {
        let mut node = node(41);
        let destination = DestHash::from([0xA5; rete_core::TRUNCATED_HASH_LEN]);
        let mut first_rng = CounterRng::default();
        let mut repeated_rng = CounterRng::default();

        let (_, link_id) = node.initiate_link(destination, 1, &mut first_rng).unwrap();
        let retained_state = node.link_state(&link_id);

        assert_eq!(
            node.initiate_link(destination, 2, &mut repeated_rng),
            Err(LinkAdmissionError::LinkIdCollision)
        );
        assert_eq!(node.metrics().capacity.links.used, 1);
        assert_eq!(node.link_state(&link_id), retained_state);
        assert_eq!(node.metrics().admission.outbound_link_collision, 1);
        assert_eq!(node.metrics().admission.outbound_link_full, 0);
        assert_eq!(node.metrics().admission.outbound_link_not_retained, 0);
    }

    #[test]
    fn destination_and_receipt_quotas_preflight_growing_native_collections() {
        type ReceiptNode = EmbeddedNode<2, 2, 8, 2>;
        let mut sender = ReceiptNode::new(
            identity(41),
            "reticulum",
            &["sender"],
            EmbeddedNodeConfig {
                role: NodeRole::Endpoint,
                max_additional_destinations: 1,
            },
        )
        .unwrap();
        sender
            .register_destination(
                "reticulum",
                &["plain"],
                DestinationType::Plain,
                Direction::In,
            )
            .unwrap();
        assert_eq!(
            sender.register_destination(
                "reticulum",
                &["overflow"],
                DestinationType::Plain,
                Direction::In,
            ),
            Err(DestinationRegistrationError::LimitReached { limit: 1 })
        );

        let receiver = ReceiptNode::new(
            identity(42),
            "reticulum",
            &["receiver"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        sender
            .register_peer(&identity(42), "reticulum", &["receiver"], 0)
            .unwrap();
        let mut rng = CounterRng::default();
        for now in [1, 2] {
            sender
                .send_data(
                    &receiver.destination_hash(),
                    b"receipt bounded",
                    now,
                    &mut rng,
                )
                .unwrap();
        }
        assert_eq!(sender.metrics().capacity.receipts.used, 2);
        assert_eq!(
            sender.send_data(
                &receiver.destination_hash(),
                b"must not escape without a receipt",
                3,
                &mut rng,
            ),
            Err(EmbeddedSendError::ReceiptTableFull { limit: 2 })
        );
        assert_eq!(sender.metrics().admission.destination_limit, 1);
        assert_eq!(sender.metrics().admission.receipt_table_full, 1);

        let oversized = [0xA5; rete_core::ENCRYPTED_MDU + 1];
        assert_eq!(
            sender.send_data(&receiver.destination_hash(), &oversized, 4, &mut rng,),
            Err(EmbeddedSendError::DataPayloadTooLarge {
                actual: rete_core::ENCRYPTED_MDU + 1,
                maximum: rete_core::ENCRYPTED_MDU,
            })
        );
        assert_eq!(sender.metrics().admission.data_payload_too_large, 1);
    }

    #[test]
    fn caller_owned_data_preparation_returns_scalar_metadata_and_can_cancel() {
        type ReceiptNode = EmbeddedNode<2, 2, 8, 2>;
        let mut sender = ReceiptNode::new(
            identity(80),
            "reticulum",
            &["sender"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let receiver = ReceiptNode::new(
            identity(81),
            "reticulum",
            &["receiver"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        sender
            .register_peer(&identity(81), "reticulum", &["receiver"], 0)
            .unwrap();

        let mut rng = CounterRng::default();
        let mut output = [0u8; RNS_MTU];
        let prepared = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"caller-owned",
                1,
                &mut rng,
                &mut output,
            )
            .unwrap();

        let packet = Packet::parse(&output[..usize::from(prepared.packet_len())]).unwrap();
        assert_eq!(&packet.compute_hash(), prepared.receipt().as_bytes());
        assert_eq!(prepared.target(), TxTarget::All);
        assert_eq!(sender.metrics().capacity.receipts.used, 1);

        assert!(sender.cancel_data_receipt(prepared.receipt()));
        assert!(!sender.cancel_data_receipt(prepared.receipt()));
        assert_eq!(sender.metrics().capacity.receipts.used, 0);
    }

    #[test]
    fn receipt_sink_preserves_full_data_candidate_and_proof_retry_state() {
        let mut sender = node(84);
        let receiver = node(85);
        sender
            .register_peer(&identity(85), "reticulum", &["embedded"], 100)
            .unwrap();
        let mut rng = CounterRng::default();
        let mut output = [0u8; RNS_MTU];
        let prepared = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"terminal bridge",
                100,
                &mut rng,
                &mut output,
            )
            .unwrap();
        let proof = rete_transport::Transport::<
            rete_transport::HeaplessStorage<4, 2, 8, 2>,
        >::build_proof_packet(&identity(85), prepared.receipt().as_bytes())
        .unwrap();
        let expected = ReceiptCandidate {
            kind: ReceiptKind::Data,
            receipt: prepared.receipt(),
        };

        let mut invalid_proof = proof.clone();
        *invalid_proof.last_mut().unwrap() ^= 0xff;
        let mut sink = RecordingReceiptSink::default();
        let invalid = sender
            .ingest_with_receipt_sink(&invalid_proof, 101, InterfaceId(1), &mut rng, &mut sink)
            .unwrap();
        assert_eq!(invalid.disposition, IngressDisposition::NoObservableOutcome);
        assert_eq!(sink.attempted, [expected]);
        assert!(sink.terminals.is_empty());
        assert_eq!(sink.active_reservations, 0);
        assert_eq!(sender.metrics().capacity.receipts.used, 1);

        sink.refuse = true;
        let transport_before_refusal = sender.metrics().transport;
        assert!(matches!(
            sender.ingest_with_receipt_sink(&proof, 102, InterfaceId(1), &mut rng, &mut sink,),
            Err(ReceiptReservationUnavailable)
        ));
        assert_eq!(sender.metrics().transport, transport_before_refusal);
        assert_eq!(sender.metrics().capacity.receipts.used, 1);
        assert_eq!(
            sender.metrics().receipt_terminals.reservation_backpressure,
            1
        );

        sink.refuse = false;
        let delivered = sender
            .ingest_with_receipt_sink(&proof, 103, InterfaceId(1), &mut rng, &mut sink)
            .unwrap();
        assert_eq!(delivered.disposition, IngressDisposition::Processed);
        assert!(delivered.actions.events.is_empty());
        assert_eq!(sink.attempted, [expected, expected, expected]);
        assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
        assert_eq!(sink.active_reservations, 0);
        assert_eq!(sender.metrics().capacity.receipts.used, 0);
    }

    #[test]
    fn inbound_proof_policy_completes_announced_encrypted_data_receipt() {
        let mut sender = node(90);
        let mut receiver = node(91);
        receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
        let mut rng = CounterRng::default();

        receiver.queue_announce(None, 100, &mut rng).unwrap();
        let announce = receiver
            .flush_announces(100, &mut rng)
            .into_iter()
            .next()
            .expect("the queued announce must be ready immediately");
        let learned = sender.ingest(announce.bytes(), 100, InterfaceId(4), &mut rng);
        assert_eq!(learned.disposition, IngressDisposition::Processed);
        assert!(sender.route(&receiver.destination_hash()).is_some());

        let mut data = [0u8; RNS_MTU];
        let prepared = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"announced and proven",
                101,
                &mut rng,
                &mut data,
            )
            .unwrap();
        assert_eq!(prepared.target(), TxTarget::Only(InterfaceId(4)));
        let received = receiver.ingest(
            &data[..usize::from(prepared.packet_len())],
            101,
            InterfaceId(7),
            &mut rng,
        );
        assert_eq!(received.disposition, IngressDisposition::Processed);
        assert_eq!(received.metadata.wire_packet_type(), Some(PacketType::Data));
        assert_eq!(received.metadata.emitted_packets(), 1);
        assert_eq!(received.metadata.generated_proof_actions(), 1);
        assert_eq!(received.metadata.delivered_receipt_terminals(), 0);
        assert_eq!(received.metadata.timed_out_receipt_terminals(), 0);
        let proof_tag = received
            .metadata
            .generated_proof_tag()
            .expect("one direct proof action has a correlation tag");
        assert!(received.metadata.generated_proof_tags_consistent());
        assert!(!received.metadata.counts_saturated());
        assert!(received.actions.events.iter().any(|event| matches!(
            event,
            ApplicationEvent::DataReceived { payload, .. } if payload == b"announced and proven"
        )));
        assert_eq!(received.actions.packets.len(), 1);
        let proof = &received.actions.packets[0];
        assert_eq!(proof.target(), TxTarget::Only(InterfaceId(7)));
        assert_eq!(
            Packet::parse(proof.bytes()).unwrap().packet_type,
            PacketType::Proof
        );

        let expected = ReceiptCandidate {
            kind: ReceiptKind::Data,
            receipt: prepared.receipt(),
        };
        let mut sink = RecordingReceiptSink::default();
        let delivered = sender
            .ingest_with_receipt_sink(proof.bytes(), 102, InterfaceId(4), &mut rng, &mut sink)
            .unwrap();
        assert_eq!(delivered.disposition, IngressDisposition::Processed);
        assert_eq!(
            delivered.metadata.wire_packet_type(),
            Some(PacketType::Proof)
        );
        assert_eq!(delivered.metadata.emitted_packets(), 0);
        assert_eq!(delivered.metadata.generated_proof_actions(), 0);
        assert_eq!(delivered.metadata.delivered_receipt_terminals(), 1);
        assert_eq!(delivered.metadata.timed_out_receipt_terminals(), 0);
        assert_eq!(delivered.metadata.delivered_receipt_tag(), Some(proof_tag));
        assert!(delivered.metadata.delivered_receipt_tags_consistent());
        assert!(!delivered.metadata.counts_saturated());
        assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
        assert_eq!(sender.metrics().capacity.receipts.used, 0);
    }

    #[test]
    fn retained_inbound_proof_is_exact_immediate_proof_on_the_source_interface() {
        let mut sender = node(94);
        let mut immediate = node(95);
        let mut retained = node(95);
        immediate.set_inbound_proof_policy(InboundProofPolicy::Always);
        retained.set_inbound_proof_policy(InboundProofPolicy::Retain);
        sender
            .register_peer(&identity(95), "reticulum", &["embedded"], 100)
            .unwrap();

        let mut rng = CounterRng::default();
        let mut data = [0u8; RNS_MTU];
        let prepared = sender
            .prepare_data_into(
                &immediate.destination_hash(),
                b"retain until durable",
                101,
                &mut rng,
                &mut data,
            )
            .unwrap();
        let raw = &data[..usize::from(prepared.packet_len())];

        let mut immediate_report =
            immediate.ingest(raw, 101, InterfaceId(7), &mut CounterRng::default());
        assert_eq!(immediate_report.metadata.emitted_packets(), 1);
        assert_eq!(immediate_report.metadata.generated_proof_actions(), 1);
        let immediate_proof = immediate_report.actions.packets.pop().unwrap();
        assert_eq!(immediate_proof.target(), TxTarget::Only(InterfaceId(7)));

        let mut retained_report =
            retained.ingest(raw, 101, InterfaceId(23), &mut CounterRng::default());
        assert_eq!(retained_report.disposition, IngressDisposition::Processed);
        assert_eq!(retained_report.metadata.emitted_packets(), 0);
        assert_eq!(retained_report.metadata.generated_proof_actions(), 0);
        assert!(retained_report.actions.packets.is_empty());
        assert_eq!(retained_report.actions.retained_proof_count(), 1);
        let event = retained_report.actions.events.events.pop().unwrap();
        let event_debug = format!("{event:?}");
        assert!(!event_debug.contains("InterfaceId(23)"));

        let InboundDataProjection::Data(data) = project_inbound_data(event) else {
            panic!("retained destination DATA must remain projectable")
        };
        let (_, payload) = data.into_parts();
        assert_eq!(payload, b"retain until durable");
        let retained_proof = retained_report
            .actions
            .events
            .retained_proof
            .take()
            .unwrap();
        assert_eq!(retained_proof.event_index(), 0);
        let retained_proof_debug = format!("{retained_proof:?}");
        assert_eq!(
            retained_proof_debug,
            "RetainedApplicationProof { event_index: 0, proof_present: true }"
        );
        assert!(!retained_proof_debug.contains("InterfaceId"));
        assert!(!retained_proof_debug.contains('['));
        let (event_index, owner) = retained_proof.into_parts();
        assert_eq!(event_index, 0);

        let released = owner.into_packet();
        assert_eq!(released.target(), TxTarget::Only(InterfaceId(23)));
        assert_eq!(released.protocol_token(), None);
        assert_eq!(released.bytes(), immediate_proof.bytes());
        assert_eq!(
            retained
                .core
                .get_destination(&retained.destination_hash())
                .unwrap()
                .proof_strategy,
            rete_stack::ProofStrategy::ProveApp
        );
    }

    #[test]
    fn retained_undecryptable_data_is_no_outcome_then_duplicate() {
        let mut sender = node(102);
        let mut receiver = node(103);
        receiver.set_inbound_proof_policy(InboundProofPolicy::Retain);
        sender
            .register_peer(&identity(103), "reticulum", &["embedded"], 100)
            .unwrap();

        let mut rng = CounterRng::default();
        let mut data = [0u8; RNS_MTU];
        let prepared = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"corrupted retained ciphertext",
                101,
                &mut rng,
                &mut data,
            )
            .unwrap();
        let mut corrupted = data[..usize::from(prepared.packet_len())].to_vec();
        // Destination DATA uses an authenticated encrypted token; its final
        // bytes are the HMAC. Preserve the parseable packet shape while making
        // authentication fail.
        *corrupted.last_mut().unwrap() ^= 0xff;

        let before = receiver.metrics();
        let first = receiver.ingest(&corrupted, 101, InterfaceId(7), &mut rng);
        assert_eq!(first.disposition, IngressDisposition::NoObservableOutcome);
        assert!(first.actions.is_empty());
        let after = receiver.metrics();
        assert_eq!(
            after.transport.packets_received,
            before.transport.packets_received + 1
        );
        assert_eq!(after.ingress.seen, before.ingress.seen + 1);
        assert_eq!(after.ingress.admitted, before.ingress.admitted + 1);
        assert_eq!(
            after.ingress.native_no_outcome,
            before.ingress.native_no_outcome + 1
        );
        assert_eq!(
            after.transport.packets_dropped_invalid,
            before.transport.packets_dropped_invalid
        );
        assert_eq!(
            after.transport.crypto_failures,
            before.transport.crypto_failures
        );
        assert_eq!(after.ingress.rejected, before.ingress.rejected);
        assert_eq!(after.ingress.native_invalid, before.ingress.native_invalid);
        assert_eq!(
            after.ingress.retained_proof_invariant,
            before.ingress.retained_proof_invariant
        );

        let replay = receiver.ingest(&corrupted, 102, InterfaceId(7), &mut rng);
        assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
        assert!(replay.actions.is_empty());
    }

    #[test]
    fn retained_policy_is_isolated_to_the_selected_additional_destination() {
        let receiver_identity = identity(96);
        let mut receiver = TestNode::new(
            identity(96),
            "reticulum",
            &["primary"],
            EmbeddedNodeConfig::endpoint(),
        )
        .unwrap();
        let primary = receiver.destination_hash();
        let additional = receiver
            .register_destination(
                "lxmf",
                &["delivery"],
                DestinationType::Single,
                Direction::In,
            )
            .unwrap();
        receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
        receiver
            .set_destination_inbound_proof_policy(&additional, InboundProofPolicy::Retain)
            .unwrap();
        assert_eq!(
            receiver.set_destination_inbound_proof_policy(
                &DestHash::from([0xee; rete_core::TRUNCATED_HASH_LEN]),
                InboundProofPolicy::Retain,
            ),
            Err(InboundProofPolicyError::DestinationNotRegistered)
        );

        let mut sender = node(97);
        sender
            .register_peer(&receiver_identity, "reticulum", &["primary"], 100)
            .unwrap();
        sender
            .register_peer(&receiver_identity, "lxmf", &["delivery"], 100)
            .unwrap();
        let mut rng = CounterRng::default();
        let mut raw = [0u8; RNS_MTU];

        let prepared = sender
            .prepare_data_into(&primary, b"primary immediate", 101, &mut rng, &mut raw)
            .unwrap();
        let primary_report = receiver.ingest(
            &raw[..usize::from(prepared.packet_len())],
            101,
            InterfaceId(8),
            &mut rng,
        );
        assert_eq!(primary_report.actions.packets.len(), 1);
        assert_eq!(primary_report.actions.retained_proof_count(), 0);
        assert!(matches!(
            primary_report.actions.events.as_slice(),
            [ApplicationEvent::DataReceived { destination, .. }]
                if *destination == *primary.as_bytes()
        ));

        let prepared = sender
            .prepare_data_into(&additional, b"additional retained", 102, &mut rng, &mut raw)
            .unwrap();
        let mut additional_report = receiver.ingest(
            &raw[..usize::from(prepared.packet_len())],
            102,
            InterfaceId(9),
            &mut rng,
        );
        assert!(additional_report.actions.packets.is_empty());
        let [ApplicationEvent::DataReceived { destination, .. }] =
            additional_report.actions.events.as_slice()
        else {
            panic!("the selected additional destination must retain its proof")
        };
        assert_eq!(*destination, *additional.as_bytes());
        let retained_proof = additional_report
            .actions
            .events
            .retained_proof
            .take()
            .unwrap();
        assert_eq!(retained_proof.event_index(), 0);
        assert_eq!(
            retained_proof.into_parts().1.into_packet().target(),
            TxTarget::Only(InterfaceId(9))
        );
    }

    #[test]
    fn retained_policy_accepts_only_registered_inbound_single_destinations() {
        let mut receiver = node(99);
        let plain = receiver
            .register_destination(
                "reticulum",
                &["plain"],
                DestinationType::Plain,
                Direction::In,
            )
            .unwrap();
        let outbound = receiver
            .register_destination(
                "reticulum",
                &["outbound"],
                DestinationType::Single,
                Direction::Out,
            )
            .unwrap();

        for destination in [plain, outbound] {
            assert_eq!(
                receiver.set_destination_inbound_proof_policy(
                    &destination,
                    InboundProofPolicy::Retain,
                ),
                Err(InboundProofPolicyError::RetainRequiresInboundSingle)
            );
        }
        assert_eq!(
            receiver.set_destination_inbound_proof_policy(
                &DestHash::from([0xee; rete_core::TRUNCATED_HASH_LEN]),
                InboundProofPolicy::Retain,
            ),
            Err(InboundProofPolicyError::DestinationNotRegistered)
        );
        assert_eq!(
            receiver
                .core
                .get_destination(&plain)
                .unwrap()
                .proof_strategy,
            rete_stack::ProofStrategy::ProveNone
        );
        assert_eq!(
            receiver
                .core
                .get_destination(&outbound)
                .unwrap()
                .proof_strategy,
            rete_stack::ProofStrategy::ProveNone
        );
    }

    #[test]
    fn retained_preflight_is_only_direct_header1_single_data_and_marker_is_bounded() {
        let mut receiver = node(100);
        receiver.set_inbound_proof_policy(InboundProofPolicy::Retain);
        let destination = receiver.destination_hash();
        assert_eq!(
            receiver
                .core
                .get_destination(&destination)
                .unwrap()
                .proof_strategy,
            rete_stack::ProofStrategy::ProveApp
        );

        let mut raw = [0_u8; RNS_MTU];
        let direct_len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(CONTEXT_NONE)
            .payload(b"direct shape")
            .build()
            .unwrap();
        let direct = receiver
            .preflight_ingest(&raw[..direct_len], InterfaceId(1))
            .unwrap();
        assert!(direct.proof_expectation.is_some());

        let mut raw = [0_u8; RNS_MTU];
        let header2_len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(destination.as_ref())
            .context(CONTEXT_NONE)
            .payload(b"relayed shape")
            .via(Some(receiver.identity_hash().as_bytes()))
            .build()
            .unwrap();
        let header2 = receiver
            .preflight_ingest(&raw[..header2_len], InterfaceId(1))
            .unwrap();
        assert!(header2.proof_expectation.is_none());

        let mut raw = [0_u8; RNS_MTU];
        let plain_len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Plain)
            .destination_hash(destination.as_ref())
            .context(CONTEXT_NONE)
            .payload(b"wrong destination type")
            .build()
            .unwrap();
        let plain = receiver
            .preflight_ingest(&raw[..plain_len], InterfaceId(1))
            .unwrap();
        assert!(plain.proof_expectation.is_none());
    }

    #[test]
    fn retained_proof_interception_fails_closed_on_missing_malformed_or_multiple_actions() {
        type NativeStorage = rete_transport::HeaplessStorage<4, 2, 8, 2>;
        let packet_hash = [0xa5; 32];
        let expected = RetainedDestinationProofExpectation {
            destination: DestHash::from([0x42; rete_core::TRUNCATED_HASH_LEN]),
            packet_hash,
        };
        let proof_identity = identity(98);
        let wrong_hash = [0x5a; 32];
        let exact_bytes = rete_transport::Transport::<NativeStorage>::build_proof_packet(
            &proof_identity,
            &packet_hash,
        )
        .unwrap();
        let event = || NativeNodeEvent::DataReceived {
            dest_hash: expected.destination,
            payload: b"accepted plaintext".to_vec(),
        };
        let exact = || OutboundPacket::new(exact_bytes.clone(), PacketRouting::SourceInterface);

        let mut suppressed_duplicate_or_invalid = IngestOutcome {
            events: Vec::new(),
            packets: vec![
                exact(),
                OutboundPacket::new(
                    rete_transport::Transport::<NativeStorage>::build_proof_packet(
                        &proof_identity,
                        &wrong_hash,
                    )
                    .unwrap(),
                    PacketRouting::SourceInterface,
                ),
                OutboundPacket::new(vec![0xff], PacketRouting::SourceInterface),
                OutboundPacket::new(exact_bytes.clone(), PacketRouting::All),
                OutboundPacket::new(exact_bytes.clone(), PacketRouting::AllExceptSource),
                OutboundPacket::new(exact_bytes.clone(), PacketRouting::BoundInterface(7)),
                OutboundPacket::new(exact_bytes.clone(), PacketRouting::ExactInterface(7)),
            ],
            rejection: None,
        };
        suppress_inbound_proof_actions(
            &mut suppressed_duplicate_or_invalid,
            Some(&InboundProofExpectation::DestinationData(expected)),
            &proof_identity,
        );
        assert!(suppressed_duplicate_or_invalid.packets.is_empty());

        let mut no_data_event = IngestOutcome {
            events: Vec::new(),
            packets: vec![exact()],
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut no_data_event,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(0),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::DataEventCount { actual: 0 }
        );

        let mut no_native_outcome = IngestOutcome {
            events: Vec::new(),
            packets: Vec::new(),
            rejection: None,
        };
        assert!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut no_native_outcome,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(0),
                &proof_identity,
            )
            .unwrap()
            .is_none()
        );

        let mut multiple_data_events = IngestOutcome {
            events: vec![event(), event()],
            packets: vec![exact()],
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut multiple_data_events,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(0),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::DataEventCount { actual: 2 }
        );

        let mut wrong_data_event = IngestOutcome {
            events: vec![NativeNodeEvent::DataReceived {
                dest_hash: DestHash::from([0x24; rete_core::TRUNCATED_HASH_LEN]),
                payload: b"wrong destination".to_vec(),
            }],
            packets: vec![exact()],
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut wrong_data_event,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(0),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::DataEventDestination
        );

        let mut missing = IngestOutcome {
            events: vec![event()],
            packets: Vec::new(),
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut missing,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(1),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::ProofActionCount { actual: 0 }
        );
        assert!(missing.packets.is_empty());

        let mut malformed = IngestOutcome {
            events: vec![event()],
            packets: vec![OutboundPacket::new(
                vec![0xff],
                PacketRouting::SourceInterface,
            )],
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut malformed,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(2),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::MalformedProof
        );
        assert!(malformed.packets.is_empty());

        let mut multiple = IngestOutcome {
            events: vec![event()],
            packets: vec![exact(), exact()],
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut multiple,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(3),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::ProofActionCount { actual: 2 }
        );
        assert!(multiple.packets.is_empty());

        let mut wrong = IngestOutcome {
            events: vec![event()],
            packets: vec![OutboundPacket::new(
                rete_transport::Transport::<NativeStorage>::build_proof_packet(
                    &proof_identity,
                    &wrong_hash,
                )
                .unwrap(),
                PacketRouting::SourceInterface,
            )],
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut wrong,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(4),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::MismatchedProof
        );
        assert!(wrong.packets.is_empty());

        for routing in [
            PacketRouting::All,
            PacketRouting::AllExceptSource,
            PacketRouting::BoundInterface(4),
            PacketRouting::ExactInterface(4),
        ] {
            let mut wrong_routing = IngestOutcome {
                events: vec![event()],
                packets: vec![OutboundPacket::new(exact_bytes.clone(), routing)],
                rejection: None,
            };
            assert_eq!(
                intercept_inbound_proof_actions::<4, 2, 8, 2>(
                    &mut wrong_routing,
                    Some(InboundProofExpectation::DestinationData(expected)),
                    InterfaceId(4),
                    &proof_identity,
                )
                .unwrap_err(),
                RetainedProofInvariant::MismatchedProof
            );
            assert!(wrong_routing.packets.is_empty());
        }

        let mut mixed = IngestOutcome {
            events: vec![event()],
            packets: vec![
                exact(),
                OutboundPacket::new(
                    rete_transport::Transport::<NativeStorage>::build_proof_packet(
                        &proof_identity,
                        &wrong_hash,
                    )
                    .unwrap(),
                    PacketRouting::SourceInterface,
                ),
            ],
            rejection: None,
        };
        assert_eq!(
            intercept_inbound_proof_actions::<4, 2, 8, 2>(
                &mut mixed,
                Some(InboundProofExpectation::DestinationData(expected)),
                InterfaceId(5),
                &proof_identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::ProofActionCount { actual: 2 }
        );
        assert!(mixed.packets.is_empty());

        let mut indexed = IngestOutcome {
            events: vec![
                NativeNodeEvent::Tick {
                    expired_paths: 0,
                    closed_links: 0,
                },
                event(),
            ],
            packets: vec![exact()],
            rejection: None,
        };
        let retained_proof = intercept_inbound_proof_actions::<4, 2, 8, 2>(
            &mut indexed,
            Some(InboundProofExpectation::DestinationData(expected)),
            InterfaceId(6),
            &proof_identity,
        )
        .unwrap()
        .unwrap();
        assert_eq!(retained_proof.event_index(), 1);
        assert!(indexed.packets.is_empty());
    }

    #[test]
    fn retained_proof_classifier_requires_exact_authenticated_wire_shape() {
        type NativeStorage = rete_transport::HeaplessStorage<4, 2, 8, 2>;
        let packet_hash = [0xa5; 32];
        let expected = RetainedDestinationProofExpectation {
            destination: DestHash::from([0x42; rete_core::TRUNCATED_HASH_LEN]),
            packet_hash,
        };
        let proof_identity = identity(98);
        let exact_bytes = rete_transport::Transport::<NativeStorage>::build_proof_packet(
            &proof_identity,
            &packet_hash,
        )
        .unwrap();
        let classify_with_routing = |bytes: Vec<u8>, routing| {
            classify_retained_proof_candidate(
                &OutboundPacket::new(bytes, routing),
                expected,
                &proof_identity,
            )
        };
        let classify =
            |bytes: Vec<u8>| classify_with_routing(bytes, PacketRouting::SourceInterface);

        assert_eq!(classify(exact_bytes.clone()), RetainedProofCandidate::Exact);

        for routing in [
            PacketRouting::All,
            PacketRouting::AllExceptSource,
            PacketRouting::BoundInterface(7),
            PacketRouting::ExactInterface(7),
        ] {
            assert_eq!(
                classify_with_routing(exact_bytes.clone(), routing),
                RetainedProofCandidate::Mismatched
            );
            assert_eq!(
                classify_with_routing(vec![0xff], routing),
                RetainedProofCandidate::Malformed
            );
        }

        let mut non_proof = exact_bytes.clone();
        non_proof[0] &= !0x03;
        assert_eq!(
            classify_with_routing(non_proof, PacketRouting::All),
            RetainedProofCandidate::Unrelated
        );

        let mut wrong_hops = exact_bytes.clone();
        wrong_hops[1] = 1;
        assert_eq!(classify(wrong_hops), RetainedProofCandidate::Mismatched);

        for bit in [0x20, 0x10, 0x80] {
            let mut wrong_flags = exact_bytes.clone();
            wrong_flags[0] |= bit;
            assert_eq!(classify(wrong_flags), RetainedProofCandidate::Mismatched);
        }

        let mut wrong_context = exact_bytes.clone();
        wrong_context[2 + rete_core::TRUNCATED_HASH_LEN] = 1;
        assert_eq!(classify(wrong_context), RetainedProofCandidate::Mismatched);

        let mut wrong_length = exact_bytes.clone();
        wrong_length.pop();
        assert_eq!(classify(wrong_length), RetainedProofCandidate::Mismatched);

        let mut wrong_destination = exact_bytes.clone();
        wrong_destination[2] ^= 0xff;
        assert_eq!(
            classify(wrong_destination),
            RetainedProofCandidate::Mismatched
        );

        let payload_offset = 2 + rete_core::TRUNCATED_HASH_LEN + 1;
        let mut wrong_covered_hash = exact_bytes.clone();
        wrong_covered_hash[payload_offset] ^= 0xff;
        assert_eq!(
            classify(wrong_covered_hash),
            RetainedProofCandidate::Mismatched
        );

        let mut wrong_signature = exact_bytes.clone();
        *wrong_signature.last_mut().unwrap() ^= 0xff;
        assert_eq!(
            classify(wrong_signature),
            RetainedProofCandidate::Mismatched
        );

        let wrong_signer = rete_transport::Transport::<NativeStorage>::build_proof_packet(
            &identity(99),
            &packet_hash,
        )
        .unwrap();
        assert_eq!(classify(wrong_signer), RetainedProofCandidate::Mismatched);

        let mut token_node = node(97);
        let token_peer = identity(99);
        let token_destination = node(99).destination_hash();
        token_node
            .register_peer(&token_peer, "reticulum", &["embedded"], 100)
            .unwrap();
        let (mut tokenized, _) = try_initiate_heapless_link_at(
            &mut token_node.core,
            token_destination,
            100,
            MonotonicInstant::from_secs(100),
            &mut CounterRng::default(),
        )
        .unwrap();
        assert!(tokenized.protocol_token().is_some());
        tokenized.data = exact_bytes;
        tokenized.routing = PacketRouting::SourceInterface;
        assert_eq!(
            classify_retained_proof_candidate(&tokenized, expected, &proof_identity),
            RetainedProofCandidate::Mismatched
        );
    }

    #[test]
    fn responder_link_binding_consumes_every_native_proof_routing_fail_closed() {
        type NativeStorage = rete_transport::HeaplessStorage<4, 2, 8, 2>;
        let identity = identity(77);
        let link_id = LinkId::from([0x42; rete_core::TRUNCATED_HASH_LEN]);
        let packet_hash = [0xa5; 32];
        let interface = InterfaceId(7);
        let exact_bytes = rete_transport::Transport::<NativeStorage>::build_link_proof_packet(
            &identity,
            &packet_hash,
            &link_id,
        )
        .unwrap();
        let expectation = || ResponderLinkProofExpectation {
            link_id,
            packet_hash,
            delivery: ResponderLinkProofDelivery::Retain,
        };
        let event = || NativeNodeEvent::LinkData {
            link_id,
            data: b"bound event".to_vec(),
            context: CONTEXT_NONE,
        };

        let mut source_relative = IngestOutcome {
            events: vec![event()],
            packets: vec![OutboundPacket::new(
                exact_bytes.clone(),
                PacketRouting::SourceInterface,
            )],
            rejection: None,
        };
        let retained = bind_responder_link_proof::<4, 2, 8, 2>(
            &mut source_relative,
            expectation(),
            interface,
            &identity,
        )
        .unwrap()
        .unwrap();
        assert_eq!(retained.event_index(), 0);
        assert!(source_relative.packets.is_empty());

        for routing in [
            PacketRouting::BoundInterface(interface.0),
            PacketRouting::ExactInterface(interface.0),
            PacketRouting::All,
        ] {
            let mut wrong_routing = IngestOutcome {
                events: vec![event()],
                packets: vec![OutboundPacket::new(exact_bytes.clone(), routing)],
                rejection: None,
            };
            assert_eq!(
                bind_responder_link_proof::<4, 2, 8, 2>(
                    &mut wrong_routing,
                    expectation(),
                    interface,
                    &identity,
                )
                .unwrap_err(),
                RetainedProofInvariant::MismatchedProof
            );
            assert!(wrong_routing.packets.is_empty());
        }

        let mut malformed = IngestOutcome {
            events: vec![event()],
            packets: vec![OutboundPacket::new(
                vec![0xff],
                PacketRouting::BoundInterface(interface.0),
            )],
            rejection: None,
        };
        assert_eq!(
            bind_responder_link_proof::<4, 2, 8, 2>(
                &mut malformed,
                expectation(),
                interface,
                &identity,
            )
            .unwrap_err(),
            RetainedProofInvariant::MismatchedProof
        );
        assert!(malformed.packets.is_empty());
    }

    #[test]
    fn retained_proof_invariant_rejection_publishes_no_event_or_packet_owner() {
        let mut receiver = node(101);
        let destination = receiver.destination_hash();
        let report = receiver.finish_ingest(
            IngestOutcome {
                events: vec![NativeNodeEvent::DataReceived {
                    dest_hash: destination,
                    payload: b"must not publish".to_vec(),
                }],
                packets: Vec::new(),
                rejection: None,
            },
            IngressPreflight {
                before_duplicate: receiver.core.transport.stats().packets_dropped_dedup,
                before_invalid: receiver.core.transport.stats().packets_dropped_invalid,
                metadata: IngressMetadata::default(),
                proof_expectation: Some(InboundProofExpectation::DestinationData(
                    RetainedDestinationProofExpectation {
                        destination,
                        packet_hash: [0x31; 32],
                    },
                )),
                local_path_request: None,
                path_response: None,
            },
            InterfaceId(7),
            TerminalCommitCounts::default(),
        );
        assert_eq!(
            report.disposition,
            IngressDisposition::Rejected(IngressDropReason::RetainedProofInvariant(
                RetainedProofInvariant::ProofActionCount { actual: 0 }
            ))
        );
        assert!(report.actions.events.is_empty());
        assert!(report.actions.packets.is_empty());
        assert_eq!(report.actions.retained_proof_count(), 0);
        assert_eq!(receiver.metrics().ingress.retained_proof_invariant, 1);
    }

    #[test]
    fn inbound_proof_policy_defaults_to_never_and_can_be_disabled_again() {
        let mut sender = node(92);
        let mut receiver = node(93);
        let mut rng = CounterRng::default();

        receiver.queue_announce(None, 100, &mut rng).unwrap();
        let announce = receiver
            .flush_announces(100, &mut rng)
            .into_iter()
            .next()
            .expect("the queued announce must be ready immediately");
        let learned = sender.ingest(announce.bytes(), 100, InterfaceId(4), &mut rng);
        assert_eq!(learned.disposition, IngressDisposition::Processed);

        let mut data = [0u8; RNS_MTU];
        let first = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"default never",
                101,
                &mut rng,
                &mut data,
            )
            .unwrap();
        let received = receiver.ingest(
            &data[..usize::from(first.packet_len())],
            101,
            InterfaceId(7),
            &mut rng,
        );
        assert_eq!(received.disposition, IngressDisposition::Processed);
        assert!(received.actions.packets.is_empty());

        receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
        let second = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"always",
                102,
                &mut rng,
                &mut data,
            )
            .unwrap();
        let received = receiver.ingest(
            &data[..usize::from(second.packet_len())],
            102,
            InterfaceId(7),
            &mut rng,
        );
        assert_eq!(received.actions.packets.len(), 1);

        receiver.set_inbound_proof_policy(InboundProofPolicy::Never);
        let third = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"disabled again",
                103,
                &mut rng,
                &mut data,
            )
            .unwrap();
        let received = receiver.ingest(
            &data[..usize::from(third.packet_len())],
            103,
            InterfaceId(7),
            &mut rng,
        );
        assert_eq!(received.disposition, IngressDisposition::Processed);
        assert!(received.actions.packets.is_empty());
    }

    #[test]
    fn receipt_timeout_defers_until_product_sink_can_reserve() {
        let mut sender = node(86);
        let receiver = node(87);
        sender
            .register_peer(&identity(87), "reticulum", &["embedded"], 100)
            .unwrap();
        let mut rng = CounterRng::default();
        let mut output = [0u8; RNS_MTU];
        let prepared = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"eventual timeout",
                100,
                &mut rng,
                &mut output,
            )
            .unwrap();
        let expected = ReceiptCandidate {
            kind: ReceiptKind::Data,
            receipt: prepared.receipt(),
        };
        let mut sink = RecordingReceiptSink {
            refuse: true,
            ..RecordingReceiptSink::default()
        };

        let deferred = sender.tick_with_receipt_sink(131, &mut rng, &mut sink);
        assert_eq!(deferred.timed_out_receipts, 0);
        assert_eq!(deferred.timed_out_link_data_receipts, 0);
        assert_eq!(deferred.timed_out_receipt_tag, None);
        assert!(deferred.timed_out_receipt_tags_consistent);
        assert!(deferred.receipt_terminals_deferred);
        assert_eq!(sink.attempted, [expected]);
        assert!(sink.terminals.is_empty());
        assert_eq!(sender.metrics().capacity.receipts.used, 1);
        assert_eq!(
            sender.metrics().receipt_terminals.reservation_backpressure,
            1
        );

        sink.refuse = false;
        let completed = sender.tick_with_receipt_sink(132, &mut rng, &mut sink);
        assert_eq!(completed.timed_out_receipts, 1);
        assert_eq!(completed.timed_out_link_data_receipts, 0);
        assert_eq!(
            completed.timed_out_receipt_tag,
            Some(correlation_tag(prepared.receipt().as_bytes()))
        );
        assert!(completed.timed_out_receipt_tags_consistent);
        assert!(!completed.receipt_terminals_deferred);
        assert!(
            completed
                .actions
                .events
                .iter()
                .all(|event| !matches!(event, ApplicationEvent::ReceiptFailed { .. }))
        );
        assert_eq!(sink.attempted, [expected, expected]);
        assert_eq!(sink.terminals, [ReceiptTerminal::TimedOut(expected)]);
        assert_eq!(sink.active_reservations, 0);
        assert_eq!(sender.metrics().capacity.receipts.used, 0);
    }

    #[test]
    fn receipt_timeout_tags_report_a_mixed_maintenance_batch() {
        let mut sender = node(88);
        let receiver = node(89);
        sender
            .register_peer(&identity(89), "reticulum", &["embedded"], 100)
            .unwrap();
        let mut rng = CounterRng::default();
        let mut output = [0u8; RNS_MTU];
        let first = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"first timeout",
                100,
                &mut rng,
                &mut output,
            )
            .unwrap();
        let second = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"second timeout",
                100,
                &mut rng,
                &mut output,
            )
            .unwrap();
        let first_tag = correlation_tag(first.receipt().as_bytes());
        let second_tag = correlation_tag(second.receipt().as_bytes());
        assert_ne!(first_tag, second_tag);

        let mut sink = RecordingReceiptSink::default();
        let completed = sender.tick_with_receipt_sink(131, &mut rng, &mut sink);

        assert_eq!(completed.timed_out_receipts, 2);
        assert_eq!(completed.timed_out_link_data_receipts, 0);
        assert!(matches!(
            completed.timed_out_receipt_tag,
            Some(tag) if tag == first_tag || tag == second_tag
        ));
        assert!(!completed.timed_out_receipt_tags_consistent);
        assert!(!completed.receipt_terminals_deferred);
        assert_eq!(sink.terminals.len(), 2);
        assert_eq!(sender.metrics().capacity.receipts.used, 0);
    }

    #[test]
    fn receipt_sink_maps_channel_proof_candidate_without_native_types() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();
        let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
        let response = responder.ingest(request.bytes(), 100, InterfaceId(1), &mut rng);
        let established = initiator.ingest(
            response.actions.packets[0].bytes(),
            101,
            InterfaceId(2),
            &mut rng,
        );
        responder.ingest(
            established.actions.packets[0].bytes(),
            102,
            InterfaceId(1),
            &mut rng,
        );

        let message = initiator
            .send_channel_message(&link_id, 7, b"bounded channel proof", 110, &mut rng)
            .unwrap();
        assert_eq!(
            established.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(2))
        );
        assert_eq!(message.target(), TxTarget::Only(InterfaceId(2)));
        let initial_receipt = ReceiptId(Packet::parse(message.bytes()).unwrap().compute_hash());
        let initial_proof_actions =
            responder.ingest(message.bytes(), 110, InterfaceId(1), &mut rng);
        let initial_generated_tag = initial_proof_actions
            .metadata
            .generated_proof_tag()
            .expect("channel PROOF carries the explicit covered packet hash");
        assert_eq!(initial_proof_actions.metadata.generated_proof_actions(), 1);
        let initial_proof = initial_proof_actions
            .actions
            .packets
            .iter()
            .find(|packet| {
                Packet::parse(packet.bytes())
                    .is_ok_and(|packet| packet.packet_type == PacketType::Proof)
            })
            .unwrap();
        assert_eq!(initial_proof.target(), TxTarget::Only(InterfaceId(1)));
        assert_eq!(initiator.metrics().capacity.channel_receipts.used, 1);

        // Deliberately lose the initial proof. A retry uses fresh ciphertext
        // and atomically replaces the sole retained receipt/proof target.
        let retry_actions = initiator.tick(126, &mut rng);
        assert_eq!(retry_actions.packets.len(), 1);
        assert_eq!(retry_actions.unroutable_packets, 0);
        let retry = &retry_actions.packets[0];
        assert_eq!(retry.target(), TxTarget::Only(InterfaceId(2)));
        let retry_receipt = ReceiptId(Packet::parse(retry.bytes()).unwrap().compute_hash());
        assert_ne!(retry_receipt, initial_receipt);
        assert_eq!(initiator.metrics().capacity.channel_receipts.used, 1);

        let retry_proof_actions = responder.ingest(retry.bytes(), 126, InterfaceId(1), &mut rng);
        assert!(retry_proof_actions.actions.events.is_empty());
        assert_eq!(retry_proof_actions.metadata.generated_proof_actions(), 1);
        let retry_generated_tag = retry_proof_actions
            .metadata
            .generated_proof_tag()
            .expect("replacement channel PROOF carries the retry packet hash");
        assert_ne!(retry_generated_tag, initial_generated_tag);
        let retry_proof = retry_proof_actions
            .actions
            .packets
            .iter()
            .find(|packet| {
                Packet::parse(packet.bytes())
                    .is_ok_and(|packet| packet.packet_type == PacketType::Proof)
            })
            .unwrap();
        assert_eq!(retry_proof.target(), TxTarget::Only(InterfaceId(1)));

        let mut sink = RecordingReceiptSink::default();

        let obsolete = initiator
            .ingest_with_receipt_sink(
                initial_proof.bytes(),
                127,
                InterfaceId(2),
                &mut rng,
                &mut sink,
            )
            .unwrap();
        let expected = ReceiptCandidate {
            kind: ReceiptKind::Channel,
            receipt: retry_receipt,
        };
        assert_eq!(
            obsolete.disposition,
            IngressDisposition::NoObservableOutcome
        );
        assert_eq!(obsolete.metadata.delivered_receipt_terminals(), 0);
        assert!(sink.attempted.is_empty());
        assert!(sink.terminals.is_empty());
        assert_eq!(initiator.metrics().capacity.channel_receipts.used, 1);

        let report = initiator
            .ingest_with_receipt_sink(
                retry_proof.bytes(),
                128,
                InterfaceId(2),
                &mut rng,
                &mut sink,
            )
            .unwrap();
        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert_eq!(report.metadata.delivered_receipt_terminals(), 1);
        assert_eq!(report.metadata.timed_out_receipt_terminals(), 0);
        assert_eq!(
            report.metadata.delivered_receipt_tag(),
            Some(retry_generated_tag)
        );
        assert_eq!(sink.attempted, [expected]);
        assert_eq!(sink.terminals, [ReceiptTerminal::Delivered(expected)]);
        assert_eq!(initiator.metrics().capacity.channel_receipts.used, 0);
    }

    #[test]
    fn caller_owned_data_preflight_rejects_before_entropy_or_receipt_mutation() {
        let mut sender = node(82);
        let receiver = node(83);
        sender
            .register_peer(&identity(83), "reticulum", &["embedded"], 0)
            .unwrap();
        let mut rng = CounterRng::default();
        let rng_before = rng.0;
        let mut output = [0xA5; RNS_MTU];
        let oversized = [0x5A; MAX_DATA_PAYLOAD + 1];

        assert_eq!(
            sender.prepare_data_into(
                &receiver.destination_hash(),
                &oversized,
                1,
                &mut rng,
                &mut output,
            ),
            Err(PrepareDataError::PayloadTooLarge {
                actual: MAX_DATA_PAYLOAD + 1,
                maximum: MAX_DATA_PAYLOAD,
            })
        );
        assert_eq!(rng.0, rng_before);
        assert!(output.iter().all(|byte| *byte == 0xA5));
        assert_eq!(sender.metrics().capacity.receipts.used, 0);
    }

    #[test]
    fn link_request_policy_rejects_native_loose_forms_before_state_mutation() {
        let mut initiator = node(50);
        let mut responder = node(51);
        let responder_identity = identity(51);
        initiator
            .register_peer(&responder_identity, "reticulum", &["embedded"], 0)
            .unwrap();
        let mut rng = CounterRng::default();
        let (valid, _) = initiator
            .initiate_link(responder.destination_hash(), 1, &mut rng)
            .unwrap();

        assert!(responder.set_accepts_links(&responder.destination_hash(), false));
        let disabled = responder.ingest(&valid.bytes, 1, InterfaceId(1), &mut rng);
        assert_eq!(
            disabled.disposition,
            IngressDisposition::Rejected(IngressDropReason::DestinationDoesNotAcceptLinks)
        );
        assert_eq!(responder.metrics().capacity.links.used, 0);
        assert!(responder.set_accepts_links(&responder.destination_hash(), true));

        let mut loose = [0u8; rete_core::MTU];
        let loose_len = PacketBuilder::new(&mut loose)
            .packet_type(PacketType::LinkRequest)
            .dest_type(DestType::Single)
            .destination_hash(responder.destination_hash().as_ref())
            .context(0)
            .payload(&[0xA5; 65])
            .build()
            .unwrap();
        let rejected = responder.ingest(&loose[..loose_len], 2, InterfaceId(1), &mut rng);
        assert_eq!(
            rejected.disposition,
            IngressDisposition::Rejected(IngressDropReason::LinkRequestPayloadLength(65))
        );
        assert_eq!(responder.metrics().capacity.links.used, 0);
    }

    #[test]
    fn announce_queue_is_owned_and_bounded() {
        let mut node = node(60);
        let mut rng = CounterRng::default();
        let primary = node.destination_hash();
        let expected_public_key = identity(60).public_key();
        let delivery = node
            .register_destination(
                "lxmf",
                &["delivery"],
                DestinationType::Single,
                Direction::In,
            )
            .unwrap();
        node.queue_announce(None, 1, &mut rng).unwrap();
        node.queue_announce_for(&delivery, Some(b"bounded app data"), 2, &mut rng)
            .unwrap();
        assert_eq!(node.metrics().capacity.announces.used, 2);
        assert_eq!(
            node.queue_announce(None, 1, &mut rng),
            Err(AnnounceAdmissionError::QueueFull { limit: 2 })
        );
        assert_eq!(node.metrics().admission.announce_queue_full, 1);

        let packets = node.flush_announces(1, &mut rng);
        assert_eq!(packets.len(), 2);
        assert!(packets.iter().all(|packet| packet.target == TxTarget::All));
        let primary_announce = packets
            .iter()
            .find_map(|packet| {
                let announce = crate::parse_announce_packet(packet.bytes()).ok()?;
                (announce.packet.destination_hash == primary.as_bytes()).then_some(announce)
            })
            .expect("primary destination announce must be present and valid");
        assert_eq!(primary_announce.fields.pub_key, expected_public_key);
        assert_eq!(primary_announce.fields.app_data, None);
        let delivery_announce = packets
            .iter()
            .find_map(|packet| {
                let announce = crate::parse_announce_packet(packet.bytes()).ok()?;
                (announce.packet.destination_hash == delivery.as_bytes()).then_some(announce)
            })
            .expect("LXMF delivery announce must be present and valid");
        assert_eq!(delivery_announce.fields.pub_key, expected_public_key);
        assert_eq!(
            delivery_announce.fields.app_data,
            Some(b"bounded app data".as_slice())
        );
    }

    #[test]
    fn received_secondary_announce_enables_targeted_data_preparation() {
        let mut receiver = node(61);
        let delivery = receiver
            .register_destination(
                "lxmf",
                &["delivery"],
                DestinationType::Single,
                Direction::In,
            )
            .unwrap();
        let mut sender = node(62);
        let mut rng = CounterRng::default();

        receiver
            .queue_announce_for(&delivery, Some(b"lxmf app data"), 1, &mut rng)
            .unwrap();
        let announce = receiver
            .flush_announces(1, &mut rng)
            .into_iter()
            .next()
            .unwrap();
        let learned = sender.ingest(announce.bytes(), 1, InterfaceId(7), &mut rng);
        assert_eq!(learned.disposition, IngressDisposition::Processed);
        assert_eq!(
            sender.route(&delivery).unwrap().received_on,
            Some(InterfaceId(7))
        );

        let mut output = [0; RNS_MTU];
        let prepared = sender
            .prepare_data_into(
                &delivery,
                b"after secondary announce",
                2,
                &mut rng,
                &mut output,
            )
            .unwrap();
        assert_eq!(prepared.target(), TxTarget::Only(InterfaceId(7)));
    }

    #[test]
    fn tagged_transport_path_request_has_python_compatible_wire_shape() {
        let requester_identity = identity(72);
        let requester_hash = requester_identity.hash();
        let requester = TestNode::new(
            requester_identity,
            "reticulum",
            &["transport"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let requested = DestHash::from([0xa5; TRUNCATED_HASH_LEN]);
        let mut rng = CounterRng::default();
        let request = requester.request_path(&requested, &mut rng).unwrap();
        assert_eq!(request.target(), TxTarget::All);
        let packet = Packet::parse(request.bytes()).unwrap();
        assert_eq!(packet.header_type, HeaderType::Header1);
        assert_eq!(packet.packet_type, PacketType::Data);
        assert_eq!(packet.dest_type, DestType::Plain);
        assert_eq!(packet.context, CONTEXT_NONE);
        assert_eq!(
            packet.destination_hash,
            rete_transport::PATH_REQUEST_DEST.as_ref()
        );
        assert_eq!(packet.payload.len(), TRUNCATED_HASH_LEN * 3);
        assert_eq!(&packet.payload[..TRUNCATED_HASH_LEN], requested.as_ref());
        assert_eq!(
            &packet.payload[TRUNCATED_HASH_LEN..TRUNCATED_HASH_LEN * 2],
            requester_hash.as_ref()
        );
        assert_ne!(
            &packet.payload[TRUNCATED_HASH_LEN * 2..],
            &[0_u8; TRUNCATED_HASH_LEN]
        );
    }

    #[test]
    fn tagged_endpoint_path_request_has_python_compatible_wire_shape() {
        let requester = node(71);
        let requested = DestHash::from([0xa4; TRUNCATED_HASH_LEN]);
        let mut rng = CounterRng::default();
        let request = requester.request_path(&requested, &mut rng).unwrap();
        let packet = Packet::parse(request.bytes()).unwrap();
        assert_eq!(packet.header_type, HeaderType::Header1);
        assert_eq!(packet.packet_type, PacketType::Data);
        assert_eq!(packet.dest_type, DestType::Plain);
        assert_eq!(packet.context, CONTEXT_NONE);
        assert_eq!(
            packet.destination_hash,
            rete_transport::PATH_REQUEST_DEST.as_ref()
        );
        assert_eq!(packet.payload.len(), TRUNCATED_HASH_LEN * 2);
        assert_eq!(&packet.payload[..TRUNCATED_HASH_LEN], requested.as_ref());
        assert_ne!(
            &packet.payload[TRUNCATED_HASH_LEN..],
            &[0_u8; TRUNCATED_HASH_LEN]
        );
    }

    #[test]
    fn local_secondary_path_request_returns_exact_path_response_once() {
        let mut responder = TestNode::new(
            identity(73),
            "reticulum",
            &["transport"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let secondary = responder
            .register_destination(
                "lxmf",
                &["delivery"],
                DestinationType::Single,
                Direction::In,
            )
            .unwrap();
        responder
            .set_destination_announce_app_data(&secondary, Some(&[0x93, 0xc0, 0xc0, 0x90]))
            .unwrap();

        let mut requester = TestNode::new(
            identity(74),
            "reticulum",
            &["transport"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let mut rng = CounterRng::default();
        let request = requester.request_path(&secondary, &mut rng).unwrap();
        let first = responder.ingest(request.bytes(), 100, InterfaceId(7), &mut rng);
        assert_eq!(first.disposition, IngressDisposition::Processed);
        assert_eq!(first.actions.packets.len(), 1);
        assert_eq!(
            first.actions.packets[0].target(),
            TxTarget::Only(InterfaceId(7))
        );
        let response = crate::parse_announce_packet(first.actions.packets[0].bytes()).unwrap();
        assert_eq!(response.packet.destination_hash, secondary.as_ref());
        assert_eq!(response.packet.context, CONTEXT_PATH_RESPONSE);
        assert_eq!(
            response.fields.app_data,
            Some([0x93, 0xc0, 0xc0, 0x90].as_slice())
        );
        assert_eq!(responder.metrics().ingress.local_path_responses, 1);
        assert_eq!(responder.metrics().capacity.announces.used, 0);

        let learned = requester.ingest(
            first.actions.packets[0].bytes(),
            100,
            InterfaceId(7),
            &mut rng,
        );
        assert_eq!(learned.disposition, IngressDisposition::Processed);
        assert_eq!(
            requester.route(&secondary).unwrap().received_on,
            Some(InterfaceId(7))
        );
        assert!(learned.actions.packets.is_empty());
        assert!(requester.tick(100, &mut rng).packets.is_empty());
        assert_eq!(requester.metrics().capacity.announces.used, 0);

        let duplicate = responder.ingest(request.bytes(), 101, InterfaceId(7), &mut rng);
        assert_eq!(duplicate.disposition, IngressDisposition::NativeDuplicate);
        assert!(duplicate.actions.packets.is_empty());
        assert_eq!(responder.metrics().ingress.local_path_responses, 1);

        let fresh_request = requester.request_path(&secondary, &mut rng).unwrap();
        let fresh = responder.ingest(fresh_request.bytes(), 121, InterfaceId(7), &mut rng);
        assert_eq!(fresh.disposition, IngressDisposition::Processed);
        assert_eq!(fresh.actions.packets.len(), 1);
        assert_eq!(responder.metrics().ingress.local_path_responses, 2);
    }

    #[test]
    fn unknown_path_request_remains_forwarded_without_local_response() {
        let mut transport = TestNode::new(
            identity(75),
            "reticulum",
            &["transport"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let requester = TestNode::new(
            identity(76),
            "reticulum",
            &["requester"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let missing = DestHash::from([0xf5; TRUNCATED_HASH_LEN]);
        let mut rng = CounterRng::default();
        let request = requester.request_path(&missing, &mut rng).unwrap();
        let report = transport.ingest(request.bytes(), 200, InterfaceId(9), &mut rng);
        assert_eq!(report.disposition, IngressDisposition::Processed);
        assert_eq!(report.actions.packets.len(), 1);
        assert_eq!(report.actions.packets[0].target(), TxTarget::All);
        assert_eq!(report.actions.packets[0].bytes(), request.bytes());
        assert_eq!(transport.metrics().ingress.local_path_responses, 0);
    }
}
