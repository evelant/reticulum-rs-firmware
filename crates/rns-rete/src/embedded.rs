//! Owning, fail-closed firmware boundary around Rete's native `NodeCore`.
//!
//! Rete's heapless storage bounds several maps, but its public `NodeCore`
//! still permits oversized hosted ingress, silently loses some table
//! insertions, and exposes interface routing after the native ingress context
//! has been forgotten. This module owns the core and resolves that routing
//! while the relevant context is still available, admitting only the subset we
//! can make deterministic and observable on embedded targets.

use alloc::vec::Vec;

use rand_core::{CryptoRng, RngCore};
use rete_core::{
    CONTEXT_NONE, CONTEXT_RESOURCE, CONTEXT_RESOURCE_ADV, CONTEXT_RESOURCE_HMU,
    CONTEXT_RESOURCE_ICL, CONTEXT_RESOURCE_PRF, CONTEXT_RESOURCE_RCL, CONTEXT_RESOURCE_REQ,
    DestHash, DestType, HeaderType, Identity, IdentityHash, LinkId, Packet, PacketType,
};
use rete_stack::{
    DestinationType, Direction, IngestOutcome, IngestRejection, LinkTableKind, NodeCore, NodeEvent,
    OutboundPacket, PacketRouting, ReceiptToken,
};
use rete_transport::{HeaplessStorage, LinkState, SendError};

use crate::capacity::{
    HeaplessCapacitySnapshot, LinkAdmissionError, heapless_capacity_snapshot,
    try_initiate_heapless_link,
};

/// Largest application payload that can be queued in one base-MTU channel
/// envelope without a later packet-build failure.
pub const MAX_CHANNEL_PAYLOAD: usize =
    rete_transport::LINK_MDU - rete_transport::ENVELOPE_HEADER_SIZE;

/// Base Reticulum packet MTU used by the embedded radio profile.
pub const RNS_MTU: usize = rete_core::MTU;

/// Largest plaintext accepted by destination-DATA encryption at the base MTU.
pub const MAX_DATA_PAYLOAD: usize = rete_core::ENCRYPTED_MDU;

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
    /// A destination-DATA receipt expired without a valid proof.
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

/// Owned packet emitted by the protocol core for an interface actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxPacket {
    /// Complete Reticulum packet bytes.
    bytes: Vec<u8>,
    /// Interfaces on which the packet may be sent.
    target: TxTarget,
}

impl TxPacket {
    fn broadcast(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            target: TxTarget::All,
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

    /// Consume the packet without separating its bytes from its routing
    /// authorization.
    pub fn into_parts(self) -> (Vec<u8>, TxTarget) {
        (self.bytes, self.target)
    }
}

/// Resolve a packet created without ingress context.
///
/// The pinned native origin APIs may select all interfaces or one retained
/// exact interface. Source-relative routing would be an upstream contract
/// violation: broadcasting it would leak traffic onto unrelated transports,
/// while silently dropping it would strand already-mutated protocol state.
fn resolve_origin_packet(packet: OutboundPacket) -> TxPacket {
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
    }
}

/// Application events and resolved transmission actions from one core call.
#[derive(Debug, Default)]
#[must_use = "every protocol event, packet, and unroutable-action count must be drained or retained"]
pub struct NodeActions {
    /// Native Rete events. These remain provisional because several variants
    /// contain allocation-backed payloads.
    pub events: Vec<NodeEvent>,
    /// Packets with their interface target resolved while ingress context is
    /// still available.
    pub packets: Vec<TxPacket>,
    /// Native source-dependent actions dropped because no source interface was
    /// available. This should remain zero for timer-driven work.
    pub unroutable_packets: usize,
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

/// Result of projecting one native node event onto the inbound-DATA surface.
#[must_use = "the projected DATA or unchanged non-DATA event must be handled"]
pub enum InboundDataProjection {
    /// Decrypted destination DATA with its original payload allocation.
    Data(InboundData),
    /// Any other native event, returned unchanged to its caller.
    Other(NodeEvent),
}

impl core::fmt::Debug for InboundDataProjection {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Data(data) => formatter.debug_tuple("Data").field(data).finish(),
            Self::Other(_) => formatter.write_str("Other(..)"),
        }
    }
}

/// Consume one native event and project destination DATA without cloning.
pub fn project_inbound_data(event: NodeEvent) -> InboundDataProjection {
    match event {
        NodeEvent::DataReceived { dest_hash, payload } => {
            InboundDataProjection::Data(InboundData {
                destination: *dest_hash.as_bytes(),
                payload,
            })
        }
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

/// Automatic proof behavior for DATA received at the primary destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InboundProofPolicy {
    /// Never generate delivery proofs automatically.
    #[default]
    Never,
    /// Generate a delivery proof for every successfully decrypted DATA packet.
    Always,
}

impl InboundProofPolicy {
    fn into_rete(self) -> rete_stack::ProofStrategy {
        match self {
            Self::Never => rete_stack::ProofStrategy::ProveNone,
            Self::Always => rete_stack::ProofStrategy::ProveAll,
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
    pub receipt_table_full: u64,
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
        })
    }
    /// Primary destination hash.
    pub fn destination_hash(&self) -> DestHash {
        *self.core.dest_hash()
    }

    /// Local identity hash without allocating a formatted string.
    pub fn identity_hash(&self) -> IdentityHash {
        self.core.identity().hash()
    }

    /// Configured endpoint/transport role.
    pub const fn role(&self) -> NodeRole {
        self.role
    }

    /// Set automatic proof behavior for DATA received at the primary destination.
    pub fn set_inbound_proof_policy(&mut self, policy: InboundProofPolicy) {
        self.core.set_proof_strategy(policy.into_rete());
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

    /// Current state of one locally owned Link.
    pub fn link_state(&self, link_id: &LinkId) -> Option<LinkState> {
        self.core.transport.get_link(link_id).map(|link| link.state)
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
        let preflight = match self.preflight_ingest(raw, interface) {
            Ok(preflight) => preflight,
            Err(report) => return report,
        };
        let native = self.core.handle_ingest(raw, now, interface.0, rng);
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
        let preflight = match self.preflight_ingest(raw, interface) {
            Ok(preflight) => preflight,
            Err(report) => return Ok(report),
        };
        let mut native_sink = NativeReceiptSink::new(sink);
        let native = match self.core.handle_ingest_with_receipt_sink(
            raw,
            now,
            interface.0,
            rng,
            &mut native_sink,
        ) {
            Ok(native) => native,
            Err(rete_stack::ReceiptSinkFull) => {
                self.receipt_terminals.reservation_backpressure = self
                    .receipt_terminals
                    .reservation_backpressure
                    .saturating_add(1);
                return Err(ReceiptReservationUnavailable);
            }
        };
        Ok(self.finish_ingest(native, preflight, interface, native_sink.committed))
    }

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

        Ok(IngressPreflight {
            before_duplicate: self.core.transport.stats().packets_dropped_dedup,
            before_invalid: self.core.transport.stats().packets_dropped_invalid,
            metadata,
        })
    }

    fn finish_ingest(
        &mut self,
        mut native: IngestOutcome,
        preflight: IngressPreflight,
        interface: InterfaceId,
        terminal_commits: TerminalCommitCounts,
    ) -> IngressReport {
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
            };
            return self.reject(reason, preflight.metadata);
        }

        let unretained_link = native.events.iter().find_map(|event| match event {
            NodeEvent::LinkEstablished { link_id }
                if self.core.transport.get_link(link_id).is_none() =>
            {
                Some(*link_id)
            }
            _ => None,
        });
        if unretained_link.is_some() {
            self.ingress.link_state_not_retained =
                self.ingress.link_state_not_retained.saturating_add(1);
            return self.reject(IngressDropReason::LinkStateNotRetained, preflight.metadata);
        }

        // Rete currently emits LinkEstablished as soon as a responder has
        // produced LRPROOF, while its Link is still Handshake and unusable.
        // Firmware observes establishment only after the native state is Active.
        let before_events = native.events.len();
        native.events.retain(|event| match event {
            NodeEvent::LinkEstablished { link_id } => self
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

        self.ingress.admitted = self.ingress.admitted.saturating_add(1);
        let after = self.core.transport.stats();
        let disposition = if after.packets_dropped_dedup > preflight.before_duplicate {
            self.ingress.native_duplicate = self.ingress.native_duplicate.saturating_add(1);
            IngressDisposition::NativeDuplicate
        } else if after.packets_dropped_invalid > preflight.before_invalid {
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

        IngressReport {
            disposition,
            metadata,
            actions: resolve_ingest_actions(native, interface),
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
        if plaintext.len() > MAX_DATA_PAYLOAD {
            self.admission.data_payload_too_large =
                self.admission.data_payload_too_large.saturating_add(1);
            return Err(PrepareDataError::PayloadTooLarge {
                actual: plaintext.len(),
                maximum: MAX_DATA_PAYLOAD,
            });
        }
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
        Ok(TxPacket { bytes, target })
    }

    /// Initiate a locally owned Link only when Rete can retain its state.
    pub fn initiate_link<R: RngCore + CryptoRng>(
        &mut self,
        destination: DestHash,
        now: u64,
        rng: &mut R,
    ) -> Result<(TxPacket, LinkId), LinkAdmissionError> {
        match try_initiate_heapless_link(&mut self.core, destination, now, rng) {
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
                    LinkAdmissionError::Rete(_) => {}
                }
                Err(error)
            }
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
            link.last_outbound = now;
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
        if !self.core.queue_announce(app_data, rng, now) {
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
        let (packet, event) = self.core.close_link(link_id, rng);
        NodeActions {
            events: event.into_iter().collect(),
            packets: packet.into_iter().map(resolve_origin_packet).collect(),
            unroutable_packets: 0,
        }
    }

    /// Run timer maintenance. Broadcast and exact-interface output can be
    /// resolved without ingress context; source-dependent actions are dropped
    /// and counted rather than guessed onto an interface.
    pub fn tick<R: RngCore + CryptoRng>(&mut self, now: u64, rng: &mut R) -> NodeActions {
        let native = self.core.handle_tick(now, rng);
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
        let mut native_sink = NativeReceiptSink::new(sink);
        let native = self
            .core
            .handle_tick_with_receipt_sink(now, rng, &mut native_sink);
        debug_assert_eq!(native.failed_receipts, native_sink.committed.timed_out);
        if native.receipt_notifications_deferred {
            self.receipt_terminals.reservation_backpressure = self
                .receipt_terminals
                .reservation_backpressure
                .saturating_add(1);
        }
        ReceiptTickReport {
            actions: self.finish_tick(native.outcome),
            timed_out_receipts: native.failed_receipts,
            timed_out_receipt_tag: native_sink.committed.first_timed_out_tag,
            timed_out_receipt_tags_consistent: native_sink.committed.timed_out_tags_consistent,
            receipt_terminals_deferred: native.receipt_notifications_deferred,
        }
    }

    fn finish_tick(&mut self, native: IngestOutcome) -> NodeActions {
        let actions = resolve_tick_actions(native);
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

fn resolve_ingest_actions(native: IngestOutcome, source: InterfaceId) -> NodeActions {
    NodeActions {
        events: native.events,
        packets: native
            .packets
            .into_iter()
            .map(|packet| resolve_packet(packet, source))
            .collect(),
        unroutable_packets: 0,
    }
}

fn resolve_packet(packet: OutboundPacket, source: InterfaceId) -> TxPacket {
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
    }
}

fn resolve_tick_actions(native: IngestOutcome) -> NodeActions {
    let mut packets = Vec::with_capacity(native.packets.len());
    let mut unroutable_packets = 0;
    for packet in native.packets {
        match packet.routing {
            PacketRouting::All => packets.push(TxPacket::broadcast(packet.data)),
            PacketRouting::ExactInterface(interface) | PacketRouting::BoundInterface(interface) => {
                packets.push(TxPacket {
                    bytes: packet.data,
                    target: TxTarget::Only(InterfaceId(interface)),
                });
            }
            PacketRouting::SourceInterface | PacketRouting::AllExceptSource => {
                unroutable_packets += 1;
            }
        }
    }
    NodeActions {
        events: native.events,
        packets,
        unroutable_packets,
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec};

    use super::*;
    use rete_core::{CONTEXT_KEEPALIVE, CONTEXT_LRPROOF, CONTEXT_LRRTT, PacketBuilder};

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

    #[test]
    fn inbound_data_projection_moves_the_exact_payload_owner() {
        let expected_destination = [0x42; rete_core::TRUNCATED_HASH_LEN];
        let mut payload = Vec::with_capacity(64);
        payload.extend_from_slice(b"exact moved payload");
        let expected_pointer = payload.as_ptr();
        let expected_capacity = payload.capacity();

        let projection = project_inbound_data(NodeEvent::DataReceived {
            dest_hash: DestHash::from(expected_destination),
            payload,
        });
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
        let projection = project_inbound_data(NodeEvent::LinkData {
            link_id: expected_link,
            data: expected_data,
            context: 0x5a,
        });

        let InboundDataProjection::Other(NodeEvent::LinkData {
            link_id,
            data,
            context,
        }) = projection
        else {
            panic!("non-DATA event must return through the unchanged projection")
        };
        assert_eq!(link_id, expected_link);
        assert_eq!(context, 0x5a);
        assert_eq!(data, b"unchanged link data");
        assert_eq!(data.as_ptr(), expected_pointer);
        assert_eq!(data.capacity(), expected_capacity);
    }

    #[test]
    fn inbound_data_projection_preserves_maximum_encrypted_data() {
        let payload = vec![0xa5; MAX_DATA_PAYLOAD];
        let expected_pointer = payload.as_ptr();
        let projection = project_inbound_data(NodeEvent::DataReceived {
            dest_hash: DestHash::from([0x33; rete_core::TRUNCATED_HASH_LEN]),
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
        let projection = project_inbound_data(NodeEvent::DataReceived {
            dest_hash: DestHash::from([0x66; rete_core::TRUNCATED_HASH_LEN]),
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
        let data = project_inbound_data(NodeEvent::DataReceived {
            dest_hash: DestHash::from(destination),
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        });
        let data_debug = format!("{data:?}");
        assert!(data_debug.contains("payload_len: 4"));
        assert!(!data_debug.contains("222, 173, 190, 239"));

        let other = project_inbound_data(NodeEvent::LinkData {
            link_id: LinkId::from([0x24; rete_core::TRUNCATED_HASH_LEN]),
            data: vec![0xca, 0xfe, 0xba, 0xbe],
            context: 0x55,
        });
        let other_debug = format!("{other:?}");
        assert_eq!(other_debug, "Other(..)");
        assert!(!other_debug.contains("202, 254, 186, 190"));
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
        link_id
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

        // LRPROOF takes one whole monotonic second in this fixture, so Python's
        // RTT-derived keepalive interval determines the first initiator probe.
        let initiator_keepalive = rete_transport::compute_keepalive(1.0).0 as u64;
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
            [NodeEvent::Tick {
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
        let responder_keepalive = rete_transport::compute_keepalive(2.0).0 as u64;
        let responder_tick = responder.tick(request_at + 2 + responder_keepalive, &mut rng);
        assert!(responder_tick.packets.iter().all(|packet| {
            Packet::parse(packet.bytes()).is_ok_and(|parsed| parsed.context != CONTEXT_KEEPALIVE)
        }));
        assert!(matches!(
            responder_tick.events.as_slice(),
            [NodeEvent::Tick {
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
            1
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
            Some(NodeEvent::LinkData { link_id: id, data, .. })
                if *id == link_id && data == b"bounded link payload"
        ));
        assert_eq!(
            responder.metrics().transport.packets_dropped_dedup,
            dedup_before
        );
        assert_eq!(initiator.metrics().admission.link_payload_too_large, 1);
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
            [NodeEvent::DataReceived { dest_hash, payload }]
                if *dest_hash == plain_destination && payload == b"local plain payload"
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
            [NodeEvent::DataReceived { dest_hash, payload }]
                if *dest_hash == plain_destination && payload == b"must terminate locally"
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
        let exact = resolve_origin_packet(OutboundPacket {
            data: vec![0xa5],
            routing: PacketRouting::ExactInterface(7),
        });
        assert_eq!(exact.bytes(), &[0xa5]);
        assert_eq!(exact.target(), TxTarget::Only(InterfaceId(7)));

        let bound = resolve_origin_packet(OutboundPacket {
            data: vec![0xb5],
            routing: PacketRouting::BoundInterface(8),
        });
        assert_eq!(bound.bytes(), &[0xb5]);
        assert_eq!(bound.target(), TxTarget::Only(InterfaceId(8)));

        let broadcast = resolve_origin_packet(OutboundPacket::broadcast(vec![0xb6]));
        assert_eq!(broadcast.bytes(), &[0xb6]);
        assert_eq!(broadcast.target(), TxTarget::All);
    }

    #[test]
    #[should_panic(expected = "pinned Rete origin API emitted source-relative routing")]
    fn origin_packet_resolution_rejects_source_context() {
        let _ = resolve_origin_packet(OutboundPacket {
            data: vec![0xc7],
            routing: PacketRouting::SourceInterface,
        });
    }

    #[test]
    fn tick_resolves_absolute_routes_and_counts_source_dependent_routes() {
        let actions = resolve_tick_actions(IngestOutcome {
            events: Vec::new(),
            packets: vec![
                OutboundPacket::broadcast(vec![0xa1]),
                OutboundPacket {
                    data: vec![0xb2],
                    routing: PacketRouting::ExactInterface(9),
                },
                OutboundPacket {
                    data: vec![0xb3],
                    routing: PacketRouting::BoundInterface(8),
                },
                OutboundPacket {
                    data: vec![0xc3],
                    routing: PacketRouting::SourceInterface,
                },
                OutboundPacket {
                    data: vec![0xd4],
                    routing: PacketRouting::AllExceptSource,
                },
            ],
            rejection: None,
        });

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
        for tag in [1, 2] {
            node.initiate_link(
                DestHash::from([tag; rete_core::TRUNCATED_HASH_LEN]),
                u64::from(tag),
                &mut rng,
            )
            .unwrap();
        }
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
            NodeEvent::DataReceived { payload, .. } if payload == b"announced and proven"
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
                .all(|event| !matches!(event, NodeEvent::ReceiptFailed { .. }))
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
        node.queue_announce(None, 1, &mut rng).unwrap();
        node.queue_announce(Some(b"bounded app data"), 1, &mut rng)
            .unwrap();
        assert_eq!(node.metrics().capacity.announces.used, 2);
        assert_eq!(
            node.queue_announce(None, 1, &mut rng),
            Err(AnnounceAdmissionError::QueueFull { limit: 2 })
        );
        assert_eq!(node.metrics().admission.announce_queue_full, 1);

        let packets = node.flush_announces(1, &mut rng);
        assert!(!packets.is_empty());
        assert!(packets.iter().all(|packet| packet.target == TxTarget::All));
    }
}
