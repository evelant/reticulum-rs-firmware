//! Owning, fail-closed firmware boundary around Rete's native `NodeCore`.
//!
//! Rete's heapless storage bounds several maps, but its public `NodeCore`
//! still permits oversized hosted ingress, silently loses some table
//! insertions, and exposes interface routing after the native ingress context
//! has been forgotten. This module owns the core and resolves that routing
//! while the relevant context is still available, admitting only the subset we
//! can make deterministic and observable on embedded targets.

use alloc::{collections::BTreeMap, vec::Vec};
use core::{mem::MaybeUninit, ptr::addr_of_mut};

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
use reticulum_lxmf_wire::{
    FIELD_TELEMETRY, MAX_ENCODED_SIDEBAND_LOCATION_TELEMETRY_BYTES, SidebandLocationTelemetry,
    encode_sideband_location_telemetry,
};
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
/// Messages above this boundary require a bounded RNS Resource path, which is
/// not implemented; they must not be truncated or sent opportunistically.
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
/// Canonical Reticulum probe application name.
pub const RNSTRANSPORT_PROBE_APPLICATION_NAME: &str = "rnstransport";
/// Canonical Reticulum probe destination aspect.
pub const RNSTRANSPORT_PROBE_ASPECT: &str = "probe";
/// Canonical expanded Reticulum probe destination name.
pub const RNSTRANSPORT_PROBE_EXPANDED_NAME: &str = "rnstransport.probe";
const SINGLE_PACKET_REQUEST_OVERHEAD: usize = 1 + 9 + 2 + TRUNCATED_HASH_LEN;
const RESPONSE_ARRAY_HEADER_LEN: usize = 1;
const RESPONSE_REQUEST_ID_HEADER_LEN: usize = 2;
const RESPONSE_REQUEST_ID_LEN: usize = TRUNCATED_HASH_LEN;
const RESPONSE_BIN8_HEADER_LEN: usize = 2;
const RESPONSE_BIN16_HEADER_LEN: usize = 3;
const RESPONSE_BIN8_MAX_LEN: usize = u8::MAX as usize;
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

/// Compact set of interfaces allowed to receive one ingress-derived broadcast.
///
/// Product routing policy computes the set from authoritative interface
/// metadata while exact ingress provenance is still available. The protocol
/// owner only carries the resulting identifiers; it does not know concrete
/// bearers or product interface roles. Identifiers above 63 are outside the
/// current embedded routing profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngressEgressSet(u64);

impl IngressEgressSet {
    /// Empty egress set, which suppresses the exact ingress-derived broadcast.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Construct a set from its complete compact bit representation.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Complete compact bit representation.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether no interface is allowed to receive the broadcast.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    const fn without(self, interface: InterfaceId) -> Self {
        if interface.0 >= 64 {
            return self;
        }
        Self(self.0 & !(1_u64 << interface.0))
    }
}

/// Optional physical-link signal values observed for one ingress packet.
///
/// These transport-neutral whole-dB values can be supplied by radio
/// interfaces. Stream and other interfaces leave the observation absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressSignalObservation {
    rssi_dbm: i16,
    snr_db: i16,
}

impl IngressSignalObservation {
    /// Construct one complete physical-link signal observation.
    pub const fn new(rssi_dbm: i16, snr_db: i16) -> Self {
        Self { rssi_dbm, snr_db }
    }

    /// Received signal strength in whole dBm.
    pub const fn rssi_dbm(self) -> i16 {
        self.rssi_dbm
    }

    /// Signal-to-noise ratio in whole dB.
    pub const fn snr_db(self) -> i16 {
        self.snr_db
    }
}

/// Whether a packet was injected by a local client or received from a remote
/// peer through an interface.
///
/// Rete currently retains a HEADER_1 compatibility path for locally injected
/// DATA. Remote interfaces must be distinguished before native ingest so a
/// shared-medium bystander cannot enter that local-origin forwarding path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressOrigin {
    /// Packet supplied by a trusted local client for routing by this node.
    LocalOrigin,
    /// Packet received from another Reticulum peer through an interface.
    RemoteInterface,
}

/// Transport-neutral provenance attached to one received application event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressObservation {
    interface: InterfaceId,
    signal: Option<IngressSignalObservation>,
    origin: IngressOrigin,
}

impl IngressObservation {
    /// Construct explicit remote-interface provenance.
    pub const fn remote(interface: InterfaceId, signal: Option<IngressSignalObservation>) -> Self {
        Self {
            interface,
            signal,
            origin: IngressOrigin::RemoteInterface,
        }
    }

    /// Construct provenance for one trusted local-origin injection.
    pub const fn local_origin(interface: InterfaceId) -> Self {
        Self {
            interface,
            signal: None,
            origin: IngressOrigin::LocalOrigin,
        }
    }

    /// Interface that supplied the packet.
    pub const fn interface(self) -> InterfaceId {
        self.interface
    }

    /// Physical-link signal values, when supplied by the interface.
    pub const fn signal(self) -> Option<IngressSignalObservation> {
        self.signal
    }

    /// Whether the packet came from a local injector or a remote peer.
    pub const fn origin(self) -> IngressOrigin {
        self.origin
    }
}

/// Media-aware policy for broadcasts derived synchronously from one ingress.
///
/// Shared-medium interfaces may need to repeat a packet on the ingress
/// interface to reach another peer. A point-to-point interface has only the
/// peer that supplied the packet, so reflecting that exact derived broadcast
/// cannot reach anyone new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IngressBroadcastScope {
    /// Preserve same-interface broadcast forwarding.
    #[default]
    SharedMedium,
    /// Exclude the one ingress interface from the exact derived broadcast.
    PointToPoint,
}

/// Product-supplied forwarding policy for broadcasts derived from one ingress.
///
/// Announce propagation and recursive path search are independent in Python
/// Reticulum, so each has its own optional egress selection. `None` for a
/// recursive search means that the ingress mode does not discover unknown
/// paths; `Some(empty)` means discovery is enabled but no eligible egress is
/// currently online, which still retains pending-request provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressBroadcastPolicy {
    scope: IngressBroadcastScope,
    announce_egress: Option<IngressEgressSet>,
    recursive_path_search_egress: Option<IngressEgressSet>,
}

impl IngressBroadcastPolicy {
    /// Preserve topology behavior without an additional announce boundary.
    pub const fn new(scope: IngressBroadcastScope) -> Self {
        Self {
            scope,
            announce_egress: None,
            recursive_path_search_egress: None,
        }
    }

    /// Restrict the exact ingress-derived announce to these interface IDs.
    ///
    /// An empty set suppresses that forwarding action while retaining normal
    /// ingress processing, including route and identity learning.
    pub const fn with_announce_egress(mut self, egress: IngressEgressSet) -> Self {
        self.announce_egress = Some(egress);
        self
    }

    /// Enable recursive unknown-path search and select its exact egress set.
    ///
    /// Passing an empty set deliberately retains pending discovery provenance
    /// without emitting a request, matching Boundary behavior when no
    /// Boundary or Gateway target is online.
    pub const fn with_recursive_path_search_egress(mut self, egress: IngressEgressSet) -> Self {
        self.recursive_path_search_egress = Some(egress);
        self
    }

    /// Media topology for ingress-derived broadcast reflection.
    pub const fn scope(self) -> IngressBroadcastScope {
        self.scope
    }

    /// Optional exact announce-egress restriction.
    pub const fn announce_egress(self) -> Option<IngressEgressSet> {
        self.announce_egress
    }

    /// Recursive search egress, or `None` when the ingress mode does not
    /// recursively discover unknown paths.
    pub const fn recursive_path_search_egress(self) -> Option<IngressEgressSet> {
        self.recursive_path_search_egress
    }
}

impl Default for IngressBroadcastPolicy {
    fn default() -> Self {
        Self::new(IngressBroadcastScope::SharedMedium)
    }
}

/// Firmware routing target with native interface routing already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxTarget {
    /// Send on every eligible interface.
    All,
    /// Send only on one exact interface selected from protocol or ingress state.
    Only(InterfaceId),
    /// Send on every eligible interface except the triggering interface.
    AllExcept(InterfaceId),
    /// Send only on the selected compact interface set.
    Selected(IngressEgressSet),
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
    ingress: Option<IngressObservation>,
}

impl ReceiptCandidate {
    fn from_native(
        candidate: rete_stack::ReceiptCandidate,
        ingress: Option<IngressObservation>,
    ) -> Self {
        let kind = match candidate.kind {
            rete_stack::ReceiptKind::Data => ReceiptKind::Data,
            rete_stack::ReceiptKind::LinkData => ReceiptKind::LinkData,
            rete_stack::ReceiptKind::Channel => ReceiptKind::Channel,
        };
        Self {
            kind,
            receipt: ReceiptId(candidate.packet_hash),
            ingress,
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

    /// Packet ingress that produced this candidate, when it came from ingress.
    ///
    /// Valid delivery proofs carry the exact interface and optional physical
    /// signal observation supplied to the receipt-aware ingest call. Timer
    /// expiration candidates have no ingress observation.
    pub const fn ingress(self) -> Option<IngressObservation> {
        self.ingress
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
    destination: DestHash,
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

/// Copy-only correlation evidence for one retained inbound delivery proof.
///
/// The covered packet hash is the hop-invariant token shared with the inbound
/// DATA packet. The remaining fields identify the exact proof packet that will
/// later re-enter the ordinary transmission path after durable application
/// ownership has been established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InboundProofEvidence {
    covered_packet_hash: [u8; 32],
    encoded_packet_sha256: [u8; 32],
    packet_len: u16,
    interface: InterfaceId,
}

impl InboundProofEvidence {
    fn from_packet(covered_packet_hash: [u8; 32], packet: &TxPacket) -> Self {
        let TxTarget::Only(interface) = packet.target else {
            unreachable!("a retained inbound proof targets its exact ingress interface")
        };
        Self {
            covered_packet_hash,
            encoded_packet_sha256: Sha256::digest(&packet.bytes).into(),
            packet_len: u16::try_from(packet.bytes.len())
                .expect("a retained Reticulum proof packet fits the base u16 MTU"),
            interface,
        }
    }

    /// Complete hash of the inbound DATA packet covered by this proof.
    pub const fn covered_packet_hash(self) -> [u8; 32] {
        self.covered_packet_hash
    }

    /// SHA-256 over every byte in the encoded proof packet.
    pub const fn encoded_packet_sha256(self) -> [u8; 32] {
        self.encoded_packet_sha256
    }

    /// Complete encoded proof-packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// Exact ingress interface on which the proof must be returned.
    pub const fn interface(self) -> InterfaceId {
        self.interface
    }
}

/// Exact application delivery proof withheld from ordinary ingress transmission.
///
/// The proof remains a single move-only owner until application policy allows
/// it to re-enter the ordinary transmission path. Its packet bytes, covered
/// hash and source interface are deliberately absent from `Debug` output.
#[must_use = "a retained inbound proof must be released or explicitly discarded"]
pub(crate) struct RetainedInboundProof {
    packet: TxPacket,
    evidence: InboundProofEvidence,
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
    fn new(packet: TxPacket, covered_packet_hash: [u8; 32]) -> Self {
        let evidence = InboundProofEvidence::from_packet(covered_packet_hash, &packet);
        Self { packet, evidence }
    }

    pub(crate) const fn evidence(&self) -> InboundProofEvidence {
        self.evidence
    }

    pub(crate) fn into_packet(self) -> TxPacket {
        self.packet
    }
}

#[cfg(test)]
pub(crate) fn retained_proof_for_owner_test(marker: u8) -> (RetainedInboundProof, *const u8) {
    let bytes = alloc::vec![marker; 37];
    let pointer = bytes.as_ptr();
    (
        RetainedInboundProof::new(
            TxPacket {
                bytes,
                target: TxTarget::Only(InterfaceId(marker)),
                protocol_token: None,
            },
            [marker; 32],
        ),
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

/// Failure to prepare one direct response for an exact retained request binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareResponseError {
    /// The binding's Link is no longer retained.
    LinkNotFound,
    /// The retained Link no longer matches the event-carried destination or role.
    LinkBindingMismatch,
    /// The retained Link is not active.
    LinkNotActive,
    /// The active Link has no authenticated interface binding.
    LinkInterfaceUnknown,
    /// The response body cannot fit in one packet at the negotiated Link MDU.
    ResponseTooLarge {
        /// Supplied response-body bytes.
        actual: usize,
        /// Largest response body that fits the negotiated Link MDU.
        maximum: usize,
    },
    /// Native response packet storage could not be allocated.
    AllocationFailed,
    /// Link encryption failed.
    Crypto,
    /// Rete could not build the response packet.
    PacketBuild,
    /// Rete returned a failure impossible after product-owned preflight.
    Invariant,
}

impl core::fmt::Display for PrepareResponseError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LinkNotFound => formatter.write_str("request Link is no longer retained"),
            Self::LinkBindingMismatch => {
                formatter.write_str("request binding does not match retained Link state")
            }
            Self::LinkNotActive => formatter.write_str("request Link is not active"),
            Self::LinkInterfaceUnknown => {
                formatter.write_str("request Link has no authenticated interface binding")
            }
            Self::ResponseTooLarge { actual, maximum } => write!(
                formatter,
                "response body length {actual} exceeds negotiated direct maximum {maximum}"
            ),
            Self::AllocationFailed => {
                formatter.write_str("response packet storage allocation failed")
            }
            Self::Crypto => formatter.write_str("response Link encryption failed"),
            Self::PacketBuild => formatter.write_str("response packet construction failed"),
            Self::Invariant => formatter.write_str("unexpected native response failure"),
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

/// Definitive result of reconciling one exact request dispatch owner.
///
/// Every variant guarantees that neither the adapter table nor Rete's native
/// pending-request table retains the supplied Link/request pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDispatchReconciliation {
    /// Neither ownership table retained the supplied request.
    Absent,
    /// Both ownership tables consistently retained and released a prepared request.
    ReclaimedPrepared,
    /// Both ownership tables consistently retained and released a confirmed request.
    ReclaimedConfirmed,
    /// At least one table retained the request, but their phases or presence disagreed.
    ReclaimedInconsistent,
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

const fn direct_response_body_maximum(link_mdu: usize) -> Option<usize> {
    debug_assert!(link_mdu <= rete_transport::LINK_MDU);
    let bin8_fixed = RESPONSE_ARRAY_HEADER_LEN
        + RESPONSE_REQUEST_ID_HEADER_LEN
        + RESPONSE_REQUEST_ID_LEN
        + RESPONSE_BIN8_HEADER_LEN;
    let bin16_fixed = RESPONSE_ARRAY_HEADER_LEN
        + RESPONSE_REQUEST_ID_HEADER_LEN
        + RESPONSE_REQUEST_ID_LEN
        + RESPONSE_BIN16_HEADER_LEN;
    if link_mdu < bin8_fixed {
        None
    } else if link_mdu < bin16_fixed + RESPONSE_BIN8_MAX_LEN + 1 {
        let bin8_maximum = link_mdu - bin8_fixed;
        Some(if bin8_maximum < RESPONSE_BIN8_MAX_LEN {
            bin8_maximum
        } else {
            RESPONSE_BIN8_MAX_LEN
        })
    } else {
        Some(link_mdu - bin16_fixed)
    }
}

fn map_native_response_error(error: SendError) -> PrepareResponseError {
    match error {
        SendError::LinkNotFound => PrepareResponseError::LinkNotFound,
        SendError::LinkNotActive => PrepareResponseError::LinkNotActive,
        SendError::LinkInterfaceUnknown => PrepareResponseError::LinkInterfaceUnknown,
        SendError::OutputAllocationFailed => PrepareResponseError::AllocationFailed,
        SendError::Crypto(_) => PrepareResponseError::Crypto,
        SendError::PacketBuild(_) => PrepareResponseError::PacketBuild,
        SendError::UnknownDestination
        | SendError::LinkAlreadyExists
        | SendError::LinkTableFull
        | SendError::KeepaliveRoleMismatch
        | SendError::WindowFull
        | SendError::ReceiptTableFull
        | SendError::ReceiptHashAlreadyTracked
        | SendError::InvalidRequestValue
        | SendError::ProtocolTokenExhausted
        | SendError::ProtocolTokenAssignmentFailed
        | SendError::ResourceLimit => PrepareResponseError::Invariant,
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
    /// A Link request carrying an encoded non-binary/string MessagePack value arrived.
    RequestValueReceived,
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
            Self::RequestValueReceived => "request_value_received",
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
/// application-event trust boundary. For a responder Link, `destination` is
/// the exact registered local destination at which the Link was accepted.
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
    ///
    /// This is an authoritative local destination when [`Self::role`] returns
    /// [`ApplicationLinkRole::Responder`]. For an initiator Link, it names the
    /// remote destination.
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
        /// Interface provenance and optional physical-link signal values.
        ingress: Option<IngressObservation>,
    },
    /// Decrypted DATA addressed to one local destination.
    DataReceived {
        /// Addressed destination hash.
        destination: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved plaintext owner.
        payload: Vec<u8>,
        /// Interface provenance and optional physical-link signal values.
        ingress: Option<IngressObservation>,
    },
    /// A valid proof covered one sent packet.
    ProofReceived {
        /// Complete covered packet hash.
        packet_hash: [u8; 32],
        /// Interface provenance and optional physical-link signal values.
        ingress: Option<IngressObservation>,
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
        /// Interface provenance and optional physical-link signal values.
        ingress: Option<IngressObservation>,
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
        /// Authoritative retained Link-to-destination association.
        ///
        /// Server-side dispatch can authorize the exact local destination by
        /// requiring [`ApplicationLinkRole::Responder`] and comparing
        /// [`ApplicationLinkBinding::destination`].
        binding: ApplicationLinkBinding,
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request-path hash bytes.
        path: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Exact moved request body.
        data: Vec<u8>,
    },
    /// A Link request carrying an encoded non-binary/string MessagePack value arrived.
    ///
    /// Binary and string request values continue through [`Self::RequestReceived`].
    /// This variant preserves `nil`, maps, arrays, scalars, and extension values
    /// without conflating canonical anonymous `nil` with an empty byte string.
    RequestValueReceived {
        /// Authoritative retained Link-to-destination association.
        ///
        /// Server-side dispatch can authorize the exact local destination by
        /// requiring [`ApplicationLinkRole::Responder`] and comparing
        /// [`ApplicationLinkBinding::destination`].
        binding: ApplicationLinkBinding,
        /// Stable request identifier bytes.
        request: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Stable request-path hash bytes.
        path: [u8; rete_core::TRUNCATED_HASH_LEN],
        /// Timestamp from the request wire format, in Unix seconds.
        requested_at: f64,
        /// Exact moved MessagePack encoding of the validated request value.
        encoded_value: Vec<u8>,
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
            Self::RequestValueReceived { .. } => ApplicationEventKind::RequestValueReceived,
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
                ingress,
            } => formatter
                .debug_struct("AnnounceReceived")
                .field("destination", destination)
                .field("identity", identity)
                .field("hops", hops)
                .field("app_data_len", &app_data.as_ref().map(Vec::len))
                .field("ingress", ingress)
                .finish(),
            Self::DataReceived {
                destination,
                payload,
                ingress,
            } => formatter
                .debug_struct("DataReceived")
                .field("destination", destination)
                .field("payload_len", &payload.len())
                .field("ingress", ingress)
                .finish(),
            Self::ProofReceived {
                packet_hash,
                ingress,
            } => formatter
                .debug_struct("ProofReceived")
                .field("packet_hash", packet_hash)
                .field("ingress", ingress)
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
                ingress,
            } => formatter
                .debug_struct("LinkData")
                .field("binding", binding)
                .field("data_len", &data.len())
                .field("context", context)
                .field("ingress", ingress)
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
                binding,
                request,
                path,
                data,
            } => formatter
                .debug_struct("RequestReceived")
                .field("binding", binding)
                .field("request", request)
                .field("path", path)
                .field("data_len", &data.len())
                .finish(),
            Self::RequestValueReceived {
                binding,
                request,
                path,
                requested_at,
                encoded_value,
            } => formatter
                .debug_struct("RequestValueReceived")
                .field("binding", binding)
                .field("request", request)
                .field("path", path)
                .field("requested_at", requested_at)
                .field("encoded_value_len", &encoded_value.len())
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

fn require_application_link_binding(
    link_id: &LinkId,
    binding: Option<ApplicationLinkBinding>,
) -> Result<ApplicationLinkBinding, ApplicationEventProjectionError> {
    match binding {
        Some(binding) if binding.link() == link_id.as_bytes() => Ok(binding),
        Some(_) | None => Err(ApplicationEventProjectionError::LinkStateNotRetained {
            link: *link_id.as_bytes(),
        }),
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
            ingress: None,
        },
        NativeNodeEvent::DataReceived { dest_hash, payload } => ApplicationEvent::DataReceived {
            destination: *dest_hash.as_bytes(),
            payload,
            ingress: None,
        },
        NativeNodeEvent::ProofReceived { packet_hash } => ApplicationEvent::ProofReceived {
            packet_hash,
            ingress: None,
        },
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
            binding: require_application_link_binding(&link_id, link_binding)?,
            data,
            context,
            ingress: None,
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
            binding: require_application_link_binding(&link_id, link_binding)?,
            request: *request_id.as_bytes(),
            path: *path_hash.as_bytes(),
            data,
        },
        NativeNodeEvent::RequestValueReceived {
            link_id,
            request_id,
            path_hash,
            requested_at,
            value,
        } => ApplicationEvent::RequestValueReceived {
            binding: require_application_link_binding(&link_id, link_binding)?,
            request: *request_id.as_bytes(),
            path: *path_hash.as_bytes(),
            requested_at,
            encoded_value: value,
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

    /// Attach the authoritative interface and optional physical-link signal
    /// values to application events produced by this one synchronous ingress.
    ///
    /// The interface owner calls this before the action envelope can be
    /// queued or separated from its ingress packet. Events that are not part of
    /// this bounded ingress-observation surface remain unchanged.
    pub fn attach_ingress_observation(&mut self, interface: u8, signal: Option<(i16, i16)>) {
        let signal =
            signal.map(|(rssi_dbm, snr_db)| IngressSignalObservation::new(rssi_dbm, snr_db));
        self.attach_exact_ingress_observation(IngressObservation::remote(
            InterfaceId(interface),
            signal,
        ));
    }

    fn attach_exact_ingress_observation(&mut self, observation: IngressObservation) {
        for event in &mut self.events.events {
            match event {
                ApplicationEvent::AnnounceReceived { ingress, .. }
                | ApplicationEvent::DataReceived { ingress, .. }
                | ApplicationEvent::ProofReceived { ingress, .. }
                | ApplicationEvent::LinkData { ingress, .. } => {
                    *ingress = Some(observation);
                }
                _ => {}
            }
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
    ingress: Option<IngressObservation>,
}

impl core::fmt::Debug for InboundData {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InboundData")
            .field("destination", &self.destination)
            .field("payload_len", &self.payload.len())
            .field("ingress", &self.ingress)
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

    /// Interface provenance and optional physical-link signal values.
    pub const fn ingress(&self) -> Option<IngressObservation> {
        self.ingress
    }

    /// Consume this value into its destination and exact moved payload owner.
    pub fn into_parts(self) -> ([u8; rete_core::TRUNCATED_HASH_LEN], Vec<u8>) {
        (self.destination, self.payload)
    }

    /// Consume this value into destination, exact payload owner, and ingress observation.
    pub fn into_observed_parts(
        self,
    ) -> (
        [u8; rete_core::TRUNCATED_HASH_LEN],
        Vec<u8>,
        Option<IngressObservation>,
    ) {
        (self.destination, self.payload, self.ingress)
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
            ingress,
        } => InboundDataProjection::Data(InboundData {
            destination,
            payload,
            ingress,
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
    /// Remote HEADER_1 DATA addressed beyond this node cannot use Rete's
    /// local-origin compatibility forwarding seam.
    Header1RemoteDataForwardingDisabled,
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
    /// A recursive unknown-path request could not retain requester provenance.
    DiscoveryPathTableFull { limit: usize },
    /// A known-path response could not enter the bounded announce queue.
    PathResponseQueueFull { destination: DestHash, limit: usize },
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
            Self::Header1RemoteDataForwardingDisabled => {
                write!(
                    f,
                    "remote HEADER_1 DATA cannot enter local-origin forwarding"
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
            Self::DiscoveryPathTableFull { limit } => {
                write!(f, "pending discovery path table is full (limit {limit})")
            }
            Self::PathResponseQueueFull { destination, limit } => {
                write!(
                    f,
                    "path response queue is full for {destination:02x?} (limit {limit})"
                )
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
    pub header1_remote_data_forwarding_disabled: u64,
    pub native_duplicate: u64,
    pub native_invalid: u64,
    pub native_no_outcome: u64,
    /// Unknown-path requests retained while no recursive egress was eligible.
    pub discovery_path_requests_without_egress: u64,
    /// Fresh path requests coalesced behind an already-pending destination.
    pub discovery_path_requests_coalesced: u64,
    /// Pending discovery entries removed after their 15-second timeout.
    pub discovery_path_requests_expired: u64,
    /// Matching newly accepted announces returned to the retained requester.
    pub discovery_path_responses: u64,
    /// Recursive searches rejected because bounded requester provenance was full.
    pub discovery_path_table_full: u64,
    /// Known-path responses rejected because the announce queue was full.
    pub path_response_queue_full: u64,
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

/// Copy-only retained route information without exposing Rete's mutable transport.
///
/// This is one current path-table entry, not evidence that its interface is
/// online or that the destination is presently reachable. `learned_at_seconds`
/// is when this retained candidate was installed or replaced.
/// `last_accessed_at_seconds` is Rete's LRU timestamp and is not peer activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteSnapshot {
    /// Destination addressed by this retained route.
    pub destination: DestHash,
    /// Next-hop transport identity, or `None` for a direct route.
    pub via: Option<IdentityHash>,
    /// Reticulum hop count. A direct route has one hop.
    pub hops: u8,
    /// Interface on which the accepted announce was received.
    ///
    /// `None` retains Reticulum's broadcast fallback for local transmission.
    pub received_on: Option<InterfaceId>,
    /// Monotonic whole seconds when this route candidate was retained.
    pub learned_at_seconds: u64,
    /// Monotonic whole seconds of Rete's last local LRU touch.
    pub last_accessed_at_seconds: u64,
    /// Duration after learning before periodic maintenance expires the route.
    pub expires_after_seconds: u64,
}

/// Combined adapter and native telemetry snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedNodeMetrics {
    pub ingress: IngressCounters,
    pub admission: AdmissionCounters,
    pub receipt_terminals: ReceiptTerminalCounters,
    pub transport: TransportCounters,
    pub capacity: HeaplessCapacitySnapshot,
    /// Pending recursive discovery provenance. This embedded-only table is
    /// bounded by the configured path capacity.
    pub pending_discovery_paths: crate::capacity::CapacityUse,
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

/// Failure to bind one registered local announce destination to an interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalAnnounceTargetError {
    /// The named destination is not registered on this node.
    DestinationNotRegistered,
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

/// Failure to alias one authenticated retained identity under its canonical
/// `rnstransport.probe` destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofProbeIdentityAliasError {
    /// No authenticated identity is retained for the supplied announced
    /// destination.
    SourceIdentityUnknown,
    /// The derived probe destination already names different public key
    /// material.
    DestinationIdentityConflict,
    /// The bounded native identity table did not retain the alias.
    IdentityCapacityUnavailable,
    /// Temporary identity-only snapshot storage could not be reserved.
    AllocationFailed,
}

impl core::fmt::Display for ProofProbeIdentityAliasError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SourceIdentityUnknown => {
                f.write_str("announced destination identity is not retained")
            }
            Self::DestinationIdentityConflict => {
                f.write_str("probe destination identity conflicts with retained material")
            }
            Self::IdentityCapacityUnavailable => {
                f.write_str("probe destination identity alias could not be retained")
            }
            Self::AllocationFailed => {
                f.write_str("probe destination identity alias allocation failed")
            }
        }
    }
}

/// Failure to compose one basic, unstamped LXMF message with the node-owned
/// identity and an optional Sideband-compatible location field.
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
    ChannelReceiptTableFull { limit: usize },
    ChannelPayloadTooLarge { actual: usize, maximum: usize },
    LinkPayloadTooLarge { actual: usize, maximum: usize },
    Rete(SendError),
}

impl core::fmt::Display for EmbeddedSendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ChannelReceiptTableFull { limit } => {
                write!(f, "channel receipt table is full (limit {limit})")
            }
            Self::ChannelPayloadTooLarge { actual, maximum } => {
                write!(f, "channel payload length {actual} exceeds {maximum}")
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
    announce_path_before: Option<[u8; 32]>,
    derived_broadcast: Option<IngressDerivedBroadcast>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngressDerivedBroadcast {
    Announce {
        destination: DestHash,
    },
    PathRequest {
        destination: DestHash,
        tag: [u8; TRUNCATED_HASH_LEN],
        tag_len: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngressDerivedTarget {
    AllExceptSource,
    SelectedAnnounce(IngressEgressSet),
    SelectedPathSearch(IngressEgressSet),
}

const DISCOVERY_PATH_REQUEST_TIMEOUT_SECONDS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingDiscoveryPath {
    destination: DestHash,
    requesting_interface: InterfaceId,
    expires_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoveryPathRequestAdmission {
    NotRecursive,
    Existing,
    Reserved { index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IngressDerivedOverride {
    packet_hash: [u8; 32],
    received_hops: u8,
    target: IngressDerivedTarget,
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
    ingress: Option<IngressObservation>,
    committed: TerminalCommitCounts,
}

impl<'a, S> NativeReceiptSink<'a, S> {
    fn new(sink: &'a mut S) -> Self {
        Self {
            sink,
            ingress: None,
            committed: TerminalCommitCounts::default(),
        }
    }

    fn with_ingress(sink: &'a mut S, ingress: IngressObservation) -> Self {
        Self {
            sink,
            ingress: Some(ingress),
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
        let candidate = ReceiptCandidate::from_native(candidate, self.ingress);
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
    pending_discovery_paths: [Option<PendingDiscoveryPath>; PATHS],
    local_announce_target: Option<(DestHash, InterfaceId)>,
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
            pending_discovery_paths: core::array::from_fn(|_| None),
            local_announce_target: None,
        })
    }

    /// Construct an owning node directly in caller-provided storage.
    ///
    /// This is the large-node startup path for embedded products. It avoids a
    /// complete native Rete node temporary and initializes the capacity-sized
    /// request-dispatch table one element at a time. On error, `destination`
    /// remains uninitialized and may be reused for another construction
    /// attempt.
    #[allow(
        unsafe_code,
        reason = "audited field projections initialize one MaybeUninit EmbeddedNode in place"
    )]
    #[inline(never)]
    pub fn new_in<'a>(
        destination: &'a mut MaybeUninit<Self>,
        identity: Identity,
        app_name: &str,
        aspects: &[&str],
        config: EmbeddedNodeConfig,
    ) -> Result<&'a mut Self, rete_core::Error> {
        let destination_ptr = destination.as_mut_ptr();

        // SAFETY: `destination_ptr` comes from the sole mutable borrow of the
        // complete uninitialized destination. `addr_of_mut!` does not create a
        // reference to the uninitialized field, and `MaybeUninit<T>` has the
        // same size and alignment as `T`. No other field is touched until the
        // native constructor succeeds, so its error contract leaves the whole
        // destination reusable.
        let core_destination = unsafe {
            &mut *addr_of_mut!((*destination_ptr).core)
                .cast::<MaybeUninit<NodeCore<HeaplessStorage<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>>>>()
        };
        let core = NodeCore::new_in(core_destination, identity, app_name, aspects)?;
        if config.role == NodeRole::Transport {
            core.enable_transport();
        }

        // No fallible operation follows native construction. Each write
        // targets one distinct field, and the capacity array is initialized
        // element-wise so no `[None; PATHS]` stack temporary is materialized.
        // SAFETY: the native constructor initialized `core`; every remaining
        // field is written exactly once through the exclusive destination
        // pointer before `assume_init_mut` exposes the complete value.
        unsafe {
            addr_of_mut!((*destination_ptr).role).write(config.role);
            addr_of_mut!((*destination_ptr).max_additional_destinations)
                .write(config.max_additional_destinations);
            addr_of_mut!((*destination_ptr).additional_destinations).write(0);
            addr_of_mut!((*destination_ptr).ingress).write(IngressCounters::default());
            addr_of_mut!((*destination_ptr).admission).write(AdmissionCounters::default());
            addr_of_mut!((*destination_ptr).receipt_terminals)
                .write(ReceiptTerminalCounters::default());

            let request_dispatches = addr_of_mut!((*destination_ptr).request_dispatches)
                .cast::<Option<RequestDispatchEntry>>();
            for index in 0..PATHS {
                request_dispatches.add(index).write(None);
            }
            let pending_discovery_paths = addr_of_mut!((*destination_ptr).pending_discovery_paths)
                .cast::<Option<PendingDiscoveryPath>>();
            for index in 0..PATHS {
                pending_discovery_paths.add(index).write(None);
            }
            addr_of_mut!((*destination_ptr).local_announce_target).write(None);

            Ok(destination.assume_init_mut())
        }
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

    /// Derive the canonical `rnstransport.probe` destination for any
    /// announce-known destination owned by the same retained identity.
    ///
    /// This is read-only and performs no path or identity-table mutation.
    /// `None` means the supplied destination has no authenticated retained
    /// identity.
    pub fn proof_probe_destination_for(
        &self,
        announced_destination: &DestHash,
    ) -> Option<DestHash> {
        let public_key = self.recall_identity(announced_destination)?;
        let identity_hash = rete_core::identity_hash(&public_key);
        Some(rete_core::destination_hash(
            RNSTRANSPORT_PROBE_EXPANDED_NAME,
            Some(&identity_hash),
        ))
    }

    /// Alias an authenticated retained identity under its canonical
    /// `rnstransport.probe` destination without creating a path.
    ///
    /// Rete encrypts destination DATA by destination-keyed identity lookup.
    /// An announce for another application destination authenticates the
    /// public key, but does not populate the canonical probe hash. This method
    /// copies that already-authenticated key through Rete's identity-only
    /// snapshot restore path. No direct path or route observation is invented.
    pub fn prepare_proof_probe_destination_for(
        &mut self,
        announced_destination: &DestHash,
    ) -> Result<DestHash, ProofProbeIdentityAliasError> {
        let public_key = self
            .recall_identity(announced_destination)
            .ok_or(ProofProbeIdentityAliasError::SourceIdentityUnknown)?;
        let identity_hash = rete_core::identity_hash(&public_key);
        let probe_destination =
            rete_core::destination_hash(RNSTRANSPORT_PROBE_EXPANDED_NAME, Some(&identity_hash));

        if let Some(retained) = self.recall_identity(&probe_destination) {
            return if retained == public_key {
                Ok(probe_destination)
            } else {
                Err(ProofProbeIdentityAliasError::DestinationIdentityConflict)
            };
        }

        let mut identities = Vec::new();
        identities
            .try_reserve_exact(1)
            .map_err(|_| ProofProbeIdentityAliasError::AllocationFailed)?;
        identities.push(rete_transport::IdentityEntry {
            dest_hash: probe_destination,
            pub_key: public_key,
        });
        self.core
            .transport
            .load_snapshot(&rete_transport::Snapshot {
                version: 1,
                paths: Vec::new(),
                identities,
            });

        match self.recall_identity(&probe_destination) {
            Some(retained) if retained == public_key => Ok(probe_destination),
            Some(_) => Err(ProofProbeIdentityAliasError::DestinationIdentityConflict),
            None => Err(ProofProbeIdentityAliasError::IdentityCapacityUnavailable),
        }
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

    /// Register the canonical Reticulum proof-probe responder destination.
    ///
    /// Python Reticulum's `rnprobe` utility addresses the inbound Single
    /// destination `rnstransport.probe`. The responder does not accept Links
    /// and emits an immediate delivery proof for every valid DATA packet.
    pub fn register_probe_destination(&mut self) -> Result<DestHash, DestinationRegistrationError> {
        let destination = self.register_destination(
            RNSTRANSPORT_PROBE_APPLICATION_NAME,
            &[RNSTRANSPORT_PROBE_ASPECT],
            DestinationType::Single,
            Direction::In,
        )?;
        let registered = self
            .core
            .get_destination_mut(&destination)
            .expect("a destination returned by registration must remain registered");
        registered.accepts_links = false;
        registered.set_proof_strategy(rete_stack::ProofStrategy::ProveAll);
        Ok(destination)
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

    /// Bind one local announce destination and every native retransmit to an exact interface.
    ///
    /// The override is destination-scoped. Other local announcements and
    /// ingress-derived broadcasts retain their native routing policy.
    pub fn set_local_announce_interface_target(
        &mut self,
        destination: &DestHash,
        interface: InterfaceId,
    ) -> Result<(), LocalAnnounceTargetError> {
        if self.core.get_destination_mut(destination).is_none() {
            return Err(LocalAnnounceTargetError::DestinationNotRegistered);
        }
        self.local_announce_target = Some((*destination, interface));
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

    fn project_route(destination: DestHash, path: &rete_transport::Path) -> RouteSnapshot {
        RouteSnapshot {
            destination,
            via: path.via,
            hops: path.hops,
            received_on: path.received_on.map(InterfaceId),
            learned_at_seconds: path.learned_at,
            last_accessed_at_seconds: path.last_accessed,
            expires_after_seconds: path.expiry_time(),
        }
    }

    /// Inspect one retained route without exposing mutable Rete state.
    pub fn route(&self, destination: &DestHash) -> Option<RouteSnapshot> {
        self.core
            .transport
            .get_path(destination)
            .map(|path| Self::project_route(*destination, path))
    }

    /// Visit a copy-only snapshot of every currently retained route.
    ///
    /// The callback runs synchronously under this immutable node borrow, so no
    /// route mutation can interleave with the traversal. Native map iteration
    /// order is deliberately unspecified; callers needing stable presentation
    /// must copy and sort by destination. The returned count is bounded by
    /// this node's `PATHS` const generic.
    pub fn visit_routes(&self, mut visitor: impl FnMut(RouteSnapshot)) -> usize {
        let mut visited = 0;
        for (destination, path) in self.core.transport.iter_paths() {
            visitor(Self::project_route(*destination, path));
            visited += 1;
        }
        visited
    }

    /// Number of currently retained native route entries.
    pub fn route_count(&self) -> usize {
        self.core.transport.path_count()
    }

    /// Whether an exact destination currently has a retained route.
    pub fn has_path(&self, destination: &DestHash) -> bool {
        self.core.transport.get_path(destination).is_some()
    }

    /// Remove one exact retained route.
    ///
    /// This is intentionally narrower than native operator-wide path-table
    /// controls. Delivery orchestration uses it to discard a destination route
    /// whose DATA receipt timed out before beginning fresh path discovery.
    /// The return value reports whether the route existed before removal.
    pub fn remove_path(&mut self, destination: &DestHash) -> bool {
        let retained = self.core.transport.get_path(destination).is_some();
        self.core.transport.remove_path(destination);
        retained
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
            pending_discovery_paths: crate::capacity::CapacityUse {
                used: self
                    .pending_discovery_paths
                    .iter()
                    .filter(|entry| entry.is_some())
                    .count(),
                limit: PATHS,
            },
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
        self.ingest_at_with_broadcast_scope(
            raw,
            now,
            link_now,
            interface,
            IngressBroadcastScope::SharedMedium,
            rng,
        )
    }

    /// Precise Link-clock ingress with explicit media-aware broadcast scope.
    pub fn ingest_at_with_broadcast_scope<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: u64,
        link_now: MonotonicInstant,
        interface: InterfaceId,
        broadcast_scope: IngressBroadcastScope,
        rng: &mut R,
    ) -> IngressReport {
        self.ingest_at_from_origin_with_broadcast_policy(
            raw,
            now,
            link_now,
            interface,
            IngressOrigin::RemoteInterface,
            IngressBroadcastPolicy::new(broadcast_scope),
            rng,
        )
    }

    /// Precise Link-clock ingress with media topology and an optional exact
    /// discovery-egress restriction.
    pub fn ingest_at_with_broadcast_policy<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: u64,
        link_now: MonotonicInstant,
        interface: InterfaceId,
        policy: IngressBroadcastPolicy,
        rng: &mut R,
    ) -> IngressReport {
        self.ingest_at_from_origin_with_broadcast_policy(
            raw,
            now,
            link_now,
            interface,
            IngressOrigin::RemoteInterface,
            policy,
            rng,
        )
    }

    /// Route one complete packet supplied by a trusted local client.
    ///
    /// Local-origin injection is deliberately separate from normal ingress:
    /// only this path may use Rete's temporary HEADER_1 forwarding
    /// compatibility seam. Physical and remote stream interfaces must use
    /// [`Self::ingest`] or another remote-ingress variant.
    pub fn ingest_local_origin<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: u64,
        interface: InterfaceId,
        rng: &mut R,
    ) -> IngressReport {
        self.ingest_at_from_origin_with_broadcast_policy(
            raw,
            now,
            MonotonicInstant::from_secs(now),
            interface,
            IngressOrigin::LocalOrigin,
            IngressBroadcastPolicy::default(),
            rng,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "origin-aware ingress keeps both clocks, interface, trust boundary, media policy and entropy owner explicit"
    )]
    fn ingest_at_from_origin_with_broadcast_policy<R: RngCore + CryptoRng>(
        &mut self,
        raw: &[u8],
        now: u64,
        link_now: MonotonicInstant,
        interface: InterfaceId,
        origin: IngressOrigin,
        policy: IngressBroadcastPolicy,
        rng: &mut R,
    ) -> IngressReport {
        let preflight = match self.preflight_ingest(raw, interface, origin) {
            Ok(preflight) => preflight,
            Err(report) => return report,
        };
        let discovery_admission =
            match self.prepare_discovery_path_request(&preflight, policy, interface, now) {
                Ok(admission) => admission,
                Err(reason) => return self.reject(reason, preflight.metadata),
            };
        self.arm_retained_proof(&preflight);
        let mut native = self
            .core
            .handle_ingest_at(raw, now, link_now, interface.0, rng);
        self.restore_retained_proof(&preflight);
        self.answer_local_path_request(&mut native, &preflight, now, rng);
        self.finish_discovery_path_request(&mut native, &preflight, policy, discovery_admission);
        self.answer_pending_discovery_path(&mut native, &preflight, now);
        self.finish_ingest(
            native,
            preflight,
            interface,
            policy,
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
        self.ingest_with_receipt_sink_at_with_broadcast_scope(
            raw,
            now,
            link_now,
            interface,
            IngressBroadcastScope::SharedMedium,
            rng,
            sink,
        )
    }

    /// Receipt-atomic ingress with explicit media-aware broadcast scope.
    #[allow(
        clippy::too_many_arguments,
        reason = "receipt-atomic ingress keeps each clock, interface, media policy, entropy owner and terminal sink explicit"
    )]
    pub fn ingest_with_receipt_sink_at_with_broadcast_scope<R, S>(
        &mut self,
        raw: &[u8],
        now: u64,
        link_now: MonotonicInstant,
        interface: InterfaceId,
        broadcast_scope: IngressBroadcastScope,
        rng: &mut R,
        sink: &mut S,
    ) -> Result<IngressReport, ReceiptReservationUnavailable>
    where
        R: RngCore + CryptoRng,
        S: ReceiptTerminalSink,
    {
        self.ingest_observed_with_receipt_sink_at_with_broadcast_scope(
            raw,
            now,
            link_now,
            IngressObservation::remote(interface, None),
            broadcast_scope,
            rng,
            sink,
        )
    }

    /// Receipt-atomic ingress with exact interface provenance and optional
    /// physical-link signal values.
    ///
    /// The observation is synchronously bound to both application events and
    /// a valid proof's terminal receipt. This keeps proof correlation and its
    /// final-hop signal measurement in the same transaction; callers must not
    /// reconstruct that association from a separate event stream.
    #[allow(
        clippy::too_many_arguments,
        reason = "observed receipt-atomic ingress keeps each clock, provenance, media policy, entropy owner and terminal sink explicit"
    )]
    pub fn ingest_observed_with_receipt_sink_at_with_broadcast_scope<R, S>(
        &mut self,
        raw: &[u8],
        now: u64,
        link_now: MonotonicInstant,
        ingress: IngressObservation,
        broadcast_scope: IngressBroadcastScope,
        rng: &mut R,
        sink: &mut S,
    ) -> Result<IngressReport, ReceiptReservationUnavailable>
    where
        R: RngCore + CryptoRng,
        S: ReceiptTerminalSink,
    {
        self.ingest_observed_with_receipt_sink_at_with_broadcast_policy(
            raw,
            now,
            link_now,
            ingress,
            IngressBroadcastPolicy::new(broadcast_scope),
            rng,
            sink,
        )
    }

    /// Receipt-atomic ingress with exact provenance, media topology and an
    /// optional exact discovery-egress restriction.
    #[allow(
        clippy::too_many_arguments,
        reason = "observed receipt-atomic ingress keeps each clock, provenance, routing policy, entropy owner and terminal sink explicit"
    )]
    pub fn ingest_observed_with_receipt_sink_at_with_broadcast_policy<R, S>(
        &mut self,
        raw: &[u8],
        now: u64,
        link_now: MonotonicInstant,
        ingress: IngressObservation,
        policy: IngressBroadcastPolicy,
        rng: &mut R,
        sink: &mut S,
    ) -> Result<IngressReport, ReceiptReservationUnavailable>
    where
        R: RngCore + CryptoRng,
        S: ReceiptTerminalSink,
    {
        let interface = ingress.interface();
        let preflight = match self.preflight_ingest(raw, interface, ingress.origin()) {
            Ok(preflight) => preflight,
            Err(report) => return Ok(report),
        };
        let discovery_admission =
            match self.prepare_discovery_path_request(&preflight, policy, interface, now) {
                Ok(admission) => admission,
                Err(reason) => {
                    let mut report = self.reject(reason, preflight.metadata);
                    report.actions.attach_exact_ingress_observation(ingress);
                    return Ok(report);
                }
            };
        let mut native_sink = NativeReceiptSink::with_ingress(sink, ingress);
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
        self.finish_discovery_path_request(&mut native, &preflight, policy, discovery_admission);
        self.answer_pending_discovery_path(&mut native, &preflight, now);
        let mut report =
            self.finish_ingest(native, preflight, interface, policy, native_sink.committed);
        report.actions.attach_exact_ingress_observation(ingress);
        Ok(report)
    }

    #[allow(
        clippy::result_large_err,
        reason = "the synchronous owning failure must return the exact allocation-backed ingress report"
    )]
    fn preflight_ingest(
        &mut self,
        raw: &[u8],
        interface: InterfaceId,
        origin: IngressOrigin,
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

        if let Err(reason) = self.preflight_h1_reverse_admission(&packet, interface, origin) {
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

        let derived_broadcast = ingress_derived_broadcast(&packet);
        let announce_path_before = match derived_broadcast {
            Some(IngressDerivedBroadcast::Announce { destination }) => self
                .core
                .transport
                .get_path(&destination)
                .and_then(|path| path.announce_raw.as_deref())
                .map(|raw| Sha256::digest(raw).into()),
            _ => None,
        };

        Ok(IngressPreflight {
            before_duplicate: self.core.transport.stats().packets_dropped_dedup,
            before_invalid: self.core.transport.stats().packets_dropped_invalid,
            metadata,
            proof_expectation,
            local_path_request: self.local_path_request(&packet),
            announce_path_before,
            derived_broadcast,
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
            || !rete_transport::is_path_request_ingress(packet)
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
            // Native Rete's exact destination-and-tag deduplication classified
            // this request as a duplicate, so it must not produce another
            // response.
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

    fn prepare_discovery_path_request(
        &mut self,
        preflight: &IngressPreflight,
        policy: IngressBroadcastPolicy,
        interface: InterfaceId,
        now: u64,
    ) -> Result<DiscoveryPathRequestAdmission, IngressDropReason> {
        self.expire_pending_discovery_paths(now);
        let Some(IngressDerivedBroadcast::PathRequest { destination, .. }) =
            preflight.derived_broadcast
        else {
            return Ok(DiscoveryPathRequestAdmission::NotRecursive);
        };
        if self.role != NodeRole::Transport
            || policy.recursive_path_search_egress().is_none()
            || self.core.transport.is_local_destination(&destination)
            || self.core.transport.get_path(&destination).is_some()
        {
            return Ok(DiscoveryPathRequestAdmission::NotRecursive);
        }

        if self
            .pending_discovery_paths
            .iter()
            .flatten()
            .any(|entry| entry.destination == destination)
        {
            return Ok(DiscoveryPathRequestAdmission::Existing);
        }

        let Some(index) = self
            .pending_discovery_paths
            .iter()
            .position(Option::is_none)
        else {
            return Err(IngressDropReason::DiscoveryPathTableFull { limit: PATHS });
        };
        self.pending_discovery_paths[index] = Some(PendingDiscoveryPath {
            destination,
            requesting_interface: interface,
            expires_at: now.saturating_add(DISCOVERY_PATH_REQUEST_TIMEOUT_SECONDS),
        });
        Ok(DiscoveryPathRequestAdmission::Reserved { index })
    }

    fn finish_discovery_path_request(
        &mut self,
        native: &mut IngestOutcome,
        preflight: &IngressPreflight,
        policy: IngressBroadcastPolicy,
        admission: DiscoveryPathRequestAdmission,
    ) {
        let Some(IngressDerivedBroadcast::PathRequest {
            destination,
            tag,
            tag_len,
        }) = preflight.derived_broadcast
        else {
            return;
        };
        let forwarded_index = native.packets.iter().rposition(|outbound| {
            is_forwarded_path_request_for_tag(
                &outbound.data,
                &destination,
                &tag[..usize::from(tag_len)],
            )
        });

        match admission {
            DiscoveryPathRequestAdmission::Reserved { index } => {
                if forwarded_index.is_none() {
                    if self.pending_discovery_paths[index]
                        .is_some_and(|entry| entry.destination == destination)
                    {
                        self.pending_discovery_paths[index] = None;
                    }
                } else if policy
                    .recursive_path_search_egress()
                    .is_some_and(IngressEgressSet::is_empty)
                {
                    self.ingress.discovery_path_requests_without_egress = self
                        .ingress
                        .discovery_path_requests_without_egress
                        .saturating_add(1);
                }
            }
            DiscoveryPathRequestAdmission::Existing => {
                if let Some(index) = forwarded_index {
                    native.packets.remove(index);
                    self.ingress.discovery_path_requests_coalesced = self
                        .ingress
                        .discovery_path_requests_coalesced
                        .saturating_add(1);
                }
            }
            DiscoveryPathRequestAdmission::NotRecursive => {
                if let Some(index) = forwarded_index {
                    native.packets.remove(index);
                }
            }
        }
    }

    fn answer_pending_discovery_path(
        &mut self,
        native: &mut IngestOutcome,
        preflight: &IngressPreflight,
        now: u64,
    ) {
        let Some(IngressDerivedBroadcast::Announce { destination }) = preflight.derived_broadcast
        else {
            return;
        };
        let accepted = native.events.iter().any(|event| {
            matches!(
                event,
                NativeNodeEvent::AnnounceReceived { dest_hash, .. } if *dest_hash == destination
            )
        });
        if !accepted {
            return;
        }
        let Some(path) = self.core.transport.get_path(&destination) else {
            return;
        };
        let Some(cached) = path.announce_raw.as_deref() else {
            return;
        };
        let cached_fingerprint: [u8; 32] = Sha256::digest(cached).into();
        if preflight.announce_path_before == Some(cached_fingerprint) {
            return;
        }
        let Some(requesting_interface) = self
            .pending_discovery_paths
            .iter()
            .flatten()
            .find(|entry| entry.destination == destination && now < entry.expires_at)
            .map(|entry| entry.requesting_interface)
        else {
            return;
        };
        let Some(response) = mark_cached_path_response(cached) else {
            return;
        };
        native.packets.push(OutboundPacket::new(
            response,
            PacketRouting::ExactInterface(requesting_interface.0),
        ));
        self.ingress.discovery_path_responses =
            self.ingress.discovery_path_responses.saturating_add(1);
    }

    fn expire_pending_discovery_paths(&mut self, now: u64) {
        for entry in &mut self.pending_discovery_paths {
            if entry.is_some_and(|pending| now >= pending.expires_at) {
                *entry = None;
                self.ingress.discovery_path_requests_expired = self
                    .ingress
                    .discovery_path_requests_expired
                    .saturating_add(1);
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
            NativeNodeEvent::LinkData { link_id, .. }
            | NativeNodeEvent::RequestReceived { link_id, .. }
            | NativeNodeEvent::RequestValueReceived { link_id, .. } => self
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
        policy: IngressBroadcastPolicy,
        terminal_commits: TerminalCommitCounts,
    ) -> IngressReport {
        self.reclaim_native_request_terminals(&native.events);
        if let Some(rejection) = native.rejection.take() {
            let reason = match rejection {
                IngestRejection::PathResponseQueueFull { dest_hash } => {
                    IngressDropReason::PathResponseQueueFull {
                        destination: dest_hash,
                        limit: ANNOUNCES,
                    }
                }
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
                | NativeNodeEvent::LinkData { link_id, .. }
                | NativeNodeEvent::RequestReceived { link_id, .. }
                | NativeNodeEvent::RequestValueReceived { link_id, .. } => link_id,
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
        let derived_override = if !native_duplicate && !native_invalid {
            preflight
                .derived_broadcast
                .and_then(|derived| ingress_derived_override(&native, derived, policy))
        } else {
            None
        };
        if let (Some(IngressDerivedBroadcast::Announce { .. }), Some(derived_override)) =
            (preflight.derived_broadcast, derived_override)
        {
            self.remove_retargeted_announce_retry(
                derived_override.packet_hash,
                derived_override.received_hops,
            );
        }
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
        let mut actions =
            resolve_ingest_actions(native, events, interface, retained_proof, derived_override);
        self.apply_local_announce_target(&mut actions);
        IngressReport {
            disposition,
            metadata,
            actions,
        }
    }

    fn apply_local_announce_target(&self, actions: &mut NodeActions) {
        let Some((destination, interface)) = self.local_announce_target else {
            return;
        };
        for packet in &mut actions.packets {
            if packet.target != TxTarget::All || packet.protocol_token.is_some() {
                continue;
            }
            let matches = Packet::parse(&packet.bytes).is_ok_and(|parsed| {
                parsed.packet_type == PacketType::Announce
                    && parsed.dest_type == DestType::Single
                    && parsed.destination_hash == destination.as_ref()
            });
            if matches {
                packet.target = TxTarget::Only(interface);
            }
        }
    }

    fn remove_retargeted_announce_retry(&mut self, packet_hash: [u8; 32], received_hops: u8) {
        let pending = self.core.transport.announce_count();
        let mut removed = false;
        for _ in 0..pending {
            let Some(announce) = self.core.transport.next_announce() else {
                break;
            };
            let is_exact_retry = !removed
                && !announce.local
                && !announce.block_rebroadcasts
                && announce.tx_count > 0
                && announce.received_hops == received_hops
                && Packet::parse(&announce.raw).is_ok_and(|packet| {
                    packet.packet_type == PacketType::Announce
                        && packet.compute_hash() == packet_hash
                });
            if is_exact_retry {
                removed = true;
            } else {
                let requeued = self.core.transport.queue_announce(announce);
                debug_assert!(requeued, "popped announce must fit its original queue");
            }
        }
        debug_assert!(
            removed,
            "an immediately forwarded received announce must retain one native retry"
        );
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
        origin: IngressOrigin,
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
        {
            return Ok(());
        }
        if origin == IngressOrigin::RemoteInterface {
            return Err(IngressDropReason::Header1RemoteDataForwardingDisabled);
        }
        if self.core.transport.get_path(&destination).is_none() {
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
            // rejection. Match that retry contract for the local-origin H1
            // bridge. A replay is allowed into native ingest solely so it is
            // classified as a duplicate.
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
            IngressDropReason::Header1RemoteDataForwardingDisabled => {
                self.ingress.header1_remote_data_forwarding_disabled = self
                    .ingress
                    .header1_remote_data_forwarding_disabled
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
            IngressDropReason::DiscoveryPathTableFull { .. } => {
                self.ingress.discovery_path_table_full =
                    self.ingress.discovery_path_table_full.saturating_add(1);
            }
            IngressDropReason::PathResponseQueueFull { .. } => {
                self.ingress.path_response_queue_full =
                    self.ingress.path_response_queue_full.saturating_add(1);
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
    /// This supports the interoperable bounded basic-message shape: a
    /// four-item payload containing a Unix timestamp, binary title, binary
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
        let composed =
            self.compose_basic_lxmf(destination, timestamp_unix_ms, title, content, None)?;
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

    /// Compose and sign one opportunistic LXMF carrier with a location snapshot.
    ///
    /// The location is encoded as Sideband's `FIELD_TELEMETRY` binary sensor
    /// map and is therefore covered by the LXMF message ID and signature.
    pub fn prepare_basic_lxmf_with_location_into(
        &self,
        destination: &DestHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        location: SidebandLocationTelemetry,
        output: &mut [u8],
    ) -> Result<PreparedBasicLxmf, PrepareBasicLxmfError> {
        let composed = self.compose_basic_lxmf(
            destination,
            timestamp_unix_ms,
            title,
            content,
            Some(location),
        )?;
        if composed.payload_len > LXMF_SELECTION_OVERHEAD + MAX_OPPORTUNISTIC_LXMF_CONTENT_SIZE {
            return Err(PrepareBasicLxmfError::PayloadTooLarge {
                actual: composed.payload_len - LXMF_SELECTION_OVERHEAD,
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
        let composed =
            self.compose_basic_lxmf(destination, timestamp_unix_ms, title, content, None)?;
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

    /// Compose and sign one complete direct-LXMF message with a location snapshot.
    pub fn prepare_basic_direct_lxmf_with_location_into(
        &self,
        destination: &DestHash,
        timestamp_unix_ms: u64,
        title: &[u8],
        content: &[u8],
        location: SidebandLocationTelemetry,
        output: &mut [u8],
    ) -> Result<PreparedBasicDirectLxmf, PrepareBasicLxmfError> {
        let composed = self.compose_basic_lxmf(
            destination,
            timestamp_unix_ms,
            title,
            content,
            Some(location),
        )?;
        if composed.payload_len > LXMF_SELECTION_OVERHEAD + MAX_DIRECT_LXMF_CONTENT_SIZE {
            return Err(PrepareBasicLxmfError::PayloadTooLarge {
                actual: composed.payload_len - LXMF_SELECTION_OVERHEAD,
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
        location: Option<SidebandLocationTelemetry>,
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
        let mut fields = BTreeMap::new();
        if let Some(location) = location {
            let mut telemetry = [0_u8; MAX_ENCODED_SIDEBAND_LOCATION_TELEMETRY_BYTES];
            let encoded = encode_sideband_location_telemetry(location, &mut telemetry)
                .map_err(|_| PrepareBasicLxmfError::Invariant)?;
            fields.insert(FIELD_TELEMETRY, telemetry[..encoded].to_vec());
        }
        let message = LXMessage::new(
            *destination,
            source,
            self.core.identity(),
            title,
            content,
            fields,
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
            destination: *destination,
        })
    }

    /// Park one still-outstanding destination-DATA receipt until an interface
    /// returns its owning transmission attempt.
    ///
    /// Rete's zero timeout means that maintenance does not expire the receipt.
    /// Proof validation remains active, so an unusually fast proof can still
    /// complete the exact attempt before its interface completion is
    /// reconciled. Product code must later arm or cancel every parked receipt.
    pub fn park_data_receipt(&mut self, prepared: PreparedData, parked_at: u64) -> bool {
        self.replace_data_receipt(prepared, parked_at, 0)
    }

    /// Restart one still-outstanding destination-DATA receipt at an interface
    /// transmission boundary retained by the product owner.
    ///
    /// Pinned Rete registers DATA receipts while constructing the packet and
    /// exposes no timestamp-update operation. The adapter therefore removes and
    /// immediately re-registers the same complete packet hash with the same
    /// destination key and native timeout. This operation is valid only while
    /// the exact prepared receipt is still outstanding; it never resurrects a
    /// delivered, timed-out, cancelled, or unknown receipt.
    ///
    /// Ordinary Link-DATA receipts cannot use this operation because the pinned
    /// dependency does not publicly expose their registration primitive.
    pub fn rearm_data_receipt(&mut self, prepared: PreparedData, sent_at: u64) -> bool {
        self.replace_data_receipt(prepared, sent_at, rete_transport::RECEIPT_TIMEOUT)
    }

    fn replace_data_receipt(&mut self, prepared: PreparedData, sent_at: u64, timeout: u64) -> bool {
        let receipt = prepared.receipt();
        let Some(destination_public_key) = self
            .core
            .transport
            .recall_identity(&prepared.destination)
            .copied()
        else {
            return false;
        };
        if self
            .core
            .transport
            .receipt_status(receipt.as_bytes())
            .is_none()
            || !self.core.transport.cancel_receipt(receipt.as_bytes())
        {
            return false;
        }

        self.core
            .transport
            .register_receipt(
                *receipt.as_bytes(),
                destination_public_key,
                sent_at,
                timeout,
            )
            .is_ok()
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
                let request = rete_core::RequestId::from(*handle.request());
                let link = LinkId::from(*handle.link());
                let _ = self.core.reclaim_request_dispatch(&request, &link);
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
        let phase = match entry
            .authority
            .ok_or(RequestDispatchError::NativeStateMismatch)?
        {
            NativeRequestDispatchAuthority::Prepared(confirmation) => (
                CanceledRequestDispatch::Prepared,
                self.core.cancel_prepared_request(confirmation),
            ),
            NativeRequestDispatchAuthority::Confirmed(confirmed) => (
                CanceledRequestDispatch::Confirmed,
                self.core.cancel_confirmed_request(confirmed),
            ),
        };
        if phase.1 {
            Ok(phase.0)
        } else {
            let request = rete_core::RequestId::from(*handle.request());
            let link = LinkId::from(*handle.link());
            let _ = self.core.reclaim_request_dispatch(&request, &link);
            Err(RequestDispatchError::NativeStateMismatch)
        }
    }

    /// Reclaim one exact request from both adapter and native ownership tables.
    ///
    /// This phase-agnostic operation is reserved for invariant recovery after
    /// router evidence or native dispatch confirmation disagrees with retained
    /// state. It is allocation-free, idempotent, and cannot fail partway
    /// through cleanup. Unrelated requests, including siblings on the same
    /// Link, remain untouched.
    pub fn reconcile_request_dispatch(
        &mut self,
        handle: RequestHandle,
    ) -> RequestDispatchReconciliation {
        let adapter = self
            .request_dispatches
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.handle == handle))
            .and_then(|index| self.request_dispatches[index].take())
            .and_then(|entry| entry.authority)
            .map(|authority| match authority {
                NativeRequestDispatchAuthority::Prepared(_) => CanceledRequestDispatch::Prepared,
                NativeRequestDispatchAuthority::Confirmed(_) => CanceledRequestDispatch::Confirmed,
            });
        let request = rete_core::RequestId::from(*handle.request());
        let link = LinkId::from(*handle.link());
        let native =
            self.core
                .reclaim_request_dispatch(&request, &link)
                .map(|status| match status {
                    rete_stack::RequestStatus::Prepared => Some(CanceledRequestDispatch::Prepared),
                    rete_stack::RequestStatus::Sent => Some(CanceledRequestDispatch::Confirmed),
                    _ => None,
                });
        match (adapter, native) {
            (None, None) => RequestDispatchReconciliation::Absent,
            (
                Some(CanceledRequestDispatch::Prepared),
                Some(Some(CanceledRequestDispatch::Prepared)),
            ) => RequestDispatchReconciliation::ReclaimedPrepared,
            (
                Some(CanceledRequestDispatch::Confirmed),
                Some(Some(CanceledRequestDispatch::Confirmed)),
            ) => RequestDispatchReconciliation::ReclaimedConfirmed,
            _ => RequestDispatchReconciliation::ReclaimedInconsistent,
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
        if self.core.cancel_prepared_request(confirmation) {
            Ok(())
        } else {
            let request = rete_core::RequestId::from(*handle.request());
            let link = LinkId::from(*handle.link());
            let _ = self.core.reclaim_request_dispatch(&request, &link);
            Err(RequestDispatchError::NativeStateMismatch)
        }
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
        if self.core.cancel_confirmed_request(confirmed) {
            Ok(())
        } else {
            let request = rete_core::RequestId::from(*handle.request());
            let link = LinkId::from(*handle.link());
            let _ = self.core.reclaim_request_dispatch(&request, &link);
            Err(RequestDispatchError::NativeStateMismatch)
        }
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

    /// Prepare one direct response for an exact retained inbound request binding.
    ///
    /// The event-carried binding is revalidated against current native Link
    /// destination, role, lifecycle, interface and negotiated MDU state before
    /// encryption consumes entropy. The returned packet owns no receipt,
    /// protocol token or cancellation authority; callers need only preserve it
    /// until the ordinary interface path accepts or explicitly rejects it.
    pub fn prepare_response<R: RngCore + CryptoRng>(
        &mut self,
        binding: &ApplicationLinkBinding,
        request: &[u8; TRUNCATED_HASH_LEN],
        data: &[u8],
        now: u64,
        rng: &mut R,
    ) -> Result<TxPacket, PrepareResponseError> {
        let link_id = LinkId::from(*binding.link());
        let link = self
            .core
            .transport
            .get_link(&link_id)
            .ok_or(PrepareResponseError::LinkNotFound)?;
        if link.destination_hash.as_bytes() != binding.destination()
            || ApplicationLinkRole::from_retained(link.role) != binding.role()
        {
            return Err(PrepareResponseError::LinkBindingMismatch);
        }
        if link.state != LinkState::Active {
            return Err(PrepareResponseError::LinkNotActive);
        }
        if link.bound_interface().is_none() {
            return Err(PrepareResponseError::LinkInterfaceUnknown);
        }
        let link_mdu = core::cmp::min(link.mdu(), rete_transport::LINK_MDU);
        let maximum = direct_response_body_maximum(link_mdu).ok_or(
            PrepareResponseError::ResponseTooLarge {
                actual: data.len(),
                maximum: 0,
            },
        )?;
        if data.len() > maximum {
            return Err(PrepareResponseError::ResponseTooLarge {
                actual: data.len(),
                maximum,
            });
        }

        let request_id = rete_core::RequestId::from(*request);
        let packet = self
            .core
            .send_response(&link_id, &request_id, data, rng)
            .map_err(map_native_response_error)?;
        if let Some(link) = self.core.transport.get_link_mut(&link_id) {
            link.last_outbound = MonotonicInstant::from_secs(now);
        }
        let packet = resolve_origin_packet(packet);
        debug_assert_eq!(packet.protocol_token(), None);
        Ok(packet)
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

    /// Flush queued announces while preserving any exact interface attached
    /// to a delayed cached path response.
    pub fn flush_announces<R: RngCore>(&mut self, now: u64, rng: &mut R) -> Vec<TxPacket> {
        let mut actions = NodeActions::without_retained_proofs(
            Default::default(),
            self.core
                .flush_announces(now, rng)
                .into_iter()
                .map(resolve_origin_packet)
                .collect(),
            0,
        );
        self.apply_local_announce_target(&mut actions);
        actions.packets
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
        self.expire_pending_discovery_paths(now);
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
        self.expire_pending_discovery_paths(now);
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
        let mut actions = resolve_tick_actions(native, events);
        self.apply_local_announce_target(&mut actions);
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

fn is_forwarded_path_request_for_tag(raw: &[u8], destination: &DestHash, tag: &[u8]) -> bool {
    Packet::parse(raw).is_ok_and(|packet| {
        packet.header_type == HeaderType::Header1
            && packet.packet_type == PacketType::Data
            && packet.dest_type == DestType::Plain
            && packet.context == CONTEXT_NONE
            && packet.destination_hash == rete_transport::PATH_REQUEST_DEST.as_ref()
            && packet.payload.len() == TRUNCATED_HASH_LEN * 2 + tag.len()
            && &packet.payload[..TRUNCATED_HASH_LEN] == destination.as_ref()
            && &packet.payload[TRUNCATED_HASH_LEN * 2..] == tag
    })
}

fn mark_cached_path_response(raw: &[u8]) -> Option<Vec<u8>> {
    let packet = Packet::parse(raw).ok()?;
    if packet.header_type != HeaderType::Header2
        || packet.packet_type != PacketType::Announce
        || packet.dest_type != DestType::Single
    {
        return None;
    }
    let context_offset = 2 + (2 * TRUNCATED_HASH_LEN);
    let mut response = raw.to_vec();
    *response.get_mut(context_offset)? = CONTEXT_PATH_RESPONSE;
    Some(response)
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
        proof: RetainedInboundProof::new(resolve_packet(packet, source), expected.packet_hash),
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
    let proof = RetainedInboundProof::new(
        TxPacket {
            bytes,
            target: TxTarget::Only(source),
            protocol_token: None,
        },
        expected.packet_hash,
    );
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
    mut derived_override: Option<IngressDerivedOverride>,
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
            .filter_map(|packet| {
                if derived_override.is_some_and(|candidate| candidate.matches(&packet)) {
                    let candidate = derived_override
                        .take()
                        .expect("a matching derived override remains present");
                    return resolve_packet_with_derived_target(packet, source, candidate.target);
                }
                Some(resolve_packet(packet, source))
            })
            .collect(),
        unroutable_packets: 0,
    }
}

impl IngressDerivedOverride {
    fn matches(self, packet: &OutboundPacket) -> bool {
        Packet::parse(&packet.data).is_ok_and(|parsed| parsed.compute_hash() == self.packet_hash)
    }
}

fn ingress_derived_broadcast(packet: &Packet<'_>) -> Option<IngressDerivedBroadcast> {
    if packet.packet_type == PacketType::Announce && packet.dest_type == DestType::Single {
        return Some(IngressDerivedBroadcast::Announce {
            destination: DestHash::from_slice(packet.destination_hash),
        });
    }
    if rete_transport::is_path_request_ingress(packet) && packet.payload.len() > TRUNCATED_HASH_LEN
    {
        let tag_start = if packet.payload.len() > TRUNCATED_HASH_LEN * 2 {
            TRUNCATED_HASH_LEN * 2
        } else {
            TRUNCATED_HASH_LEN
        };
        let tag_bytes = packet.payload.get(tag_start..)?;
        if tag_bytes.is_empty() {
            return None;
        }
        let tag_len = core::cmp::min(tag_bytes.len(), TRUNCATED_HASH_LEN);
        let mut tag = [0_u8; TRUNCATED_HASH_LEN];
        tag[..tag_len].copy_from_slice(&tag_bytes[..tag_len]);
        return Some(IngressDerivedBroadcast::PathRequest {
            destination: DestHash::from_slice(&packet.payload[..TRUNCATED_HASH_LEN]),
            tag,
            tag_len: tag_len as u8,
        });
    }
    None
}

fn ingress_derived_override(
    native: &IngestOutcome,
    derived: IngressDerivedBroadcast,
    policy: IngressBroadcastPolicy,
) -> Option<IngressDerivedOverride> {
    let target = match derived {
        IngressDerivedBroadcast::Announce { .. } => match policy.announce_egress() {
            Some(egress) => IngressDerivedTarget::SelectedAnnounce(egress),
            None if policy.scope() == IngressBroadcastScope::PointToPoint => {
                IngressDerivedTarget::AllExceptSource
            }
            None => return None,
        },
        IngressDerivedBroadcast::PathRequest { .. } => {
            IngressDerivedTarget::SelectedPathSearch(policy.recursive_path_search_egress()?)
        }
    };
    let matching = native.packets.iter().rposition(|outbound| {
        if outbound.routing != PacketRouting::All {
            return false;
        }
        let Ok(packet) = Packet::parse(&outbound.data) else {
            return false;
        };
        match derived {
            IngressDerivedBroadcast::Announce { destination, .. } => {
                packet.packet_type == PacketType::Announce
                    && packet.destination_hash == destination.as_ref()
            }
            IngressDerivedBroadcast::PathRequest {
                destination,
                tag,
                tag_len,
            } => is_forwarded_path_request_for_tag(
                &outbound.data,
                &destination,
                &tag[..usize::from(tag_len)],
            ),
        }
    });
    let index = matching?;
    let forwarded = Packet::parse(&native.packets[index].data)
        .expect("the selected derived broadcast was already parsed");
    Some(IngressDerivedOverride {
        packet_hash: forwarded.compute_hash(),
        received_hops: forwarded.hops,
        target,
    })
}

fn resolve_packet_with_derived_target(
    packet: OutboundPacket,
    source: InterfaceId,
    target: IngressDerivedTarget,
) -> Option<TxPacket> {
    let protocol_token = packet.protocol_token();
    let target = match target {
        IngressDerivedTarget::AllExceptSource => TxTarget::AllExcept(source),
        IngressDerivedTarget::SelectedAnnounce(egress) if egress.is_empty() => return None,
        IngressDerivedTarget::SelectedAnnounce(egress) => TxTarget::Selected(egress),
        IngressDerivedTarget::SelectedPathSearch(egress) => {
            let egress = egress.without(source);
            if egress.is_empty() {
                return None;
            }
            TxTarget::Selected(egress)
        }
    };
    Some(TxPacket {
        bytes: packet.data,
        target,
        protocol_token,
    })
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
mod tests;
