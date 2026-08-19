//! Ingress, receipt, prepared-packet, proof, and request vocabulary.

use super::*;

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

pub(crate) const LXMF_DESTINATION_HASH_LENGTH: usize = TRUNCATED_HASH_LEN;
const LXMF_SIGNATURE_LENGTH: usize = 64;
pub(crate) const LXMF_SELECTION_OVERHEAD: usize = 16;
pub(crate) const LXMF_WIRE_PREFIX_LENGTH: usize =
    LXMF_DESTINATION_HASH_LENGTH * 2 + LXMF_SIGNATURE_LENGTH;
pub(crate) const LXMF_APPLICATION_NAME: &str = "lxmf";
pub(crate) const LXMF_DELIVERY_ASPECT: &str = "delivery";
pub(crate) const LXMF_DELIVERY_EXPANDED_NAME: &str = "lxmf.delivery";
/// Canonical Reticulum probe application name.
pub const RNSTRANSPORT_PROBE_APPLICATION_NAME: &str = "rnstransport";
/// Canonical Reticulum probe destination aspect.
pub const RNSTRANSPORT_PROBE_ASPECT: &str = "probe";
/// Canonical expanded Reticulum probe destination name.
pub const RNSTRANSPORT_PROBE_EXPANDED_NAME: &str = "rnstransport.probe";
pub(crate) const SINGLE_PACKET_REQUEST_OVERHEAD: usize = 1 + 9 + 2 + TRUNCATED_HASH_LEN;
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

    pub(crate) const fn without(self, interface: InterfaceId) -> Self {
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
pub struct ReceiptId(pub(crate) [u8; 32]);

impl ReceiptId {
    pub(crate) fn from_native(receipt: ReceiptToken) -> Self {
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
    pub(crate) kind: ReceiptKind,
    pub(crate) receipt: ReceiptId,
    pub(crate) ingress: Option<IngressObservation>,
}

impl ReceiptCandidate {
    pub(crate) fn from_native(
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
    pub(crate) packet_len: u16,
    pub(crate) target: TxTarget,
    pub(crate) receipt: ReceiptId,
    pub(crate) destination: DestHash,
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
    pub(crate) packet_len: u16,
    pub(crate) target: TxTarget,
    pub(crate) receipt: ReceiptId,
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
    pub(crate) carrier_len: u16,
    pub(crate) carrier_sha256: [u8; 32],
    pub(crate) message_id: [u8; 32],
    pub(crate) destination: DestHash,
}

/// Scalar result of composing one complete basic LXMF wire message for direct
/// Link DATA.
///
/// The caller retains the exact bytes in its output buffer. The private digest
/// binds a later Link-packet transaction to those bytes without exposing the
/// node identity or signing source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedBasicDirectLxmf {
    pub(crate) wire_len: u16,
    pub(crate) wire_sha256: [u8; 32],
    pub(crate) message_id: [u8; 32],
    pub(crate) destination: DestHash,
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

pub(crate) struct ComposedBasicLxmf {
    pub(crate) packed: Vec<u8>,
    pub(crate) payload_len: usize,
    pub(crate) message_id: [u8; 32],
}

/// Owned packet emitted by the protocol core for an interface actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxPacket {
    /// Complete Reticulum packet bytes.
    pub(crate) bytes: Vec<u8>,
    /// Interfaces on which the packet may be sent.
    pub(crate) target: TxTarget,
    /// Opaque marker returned after the selected interface accepts dispatch.
    pub(crate) protocol_token: Option<OutboundProtocolToken>,
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
    pub(crate) packet: TxPacket,
    pub(crate) evidence: InboundProofEvidence,
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
    pub(crate) fn new(packet: TxPacket, covered_packet_hash: [u8; 32]) -> Self {
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
    pub(crate) event_index: usize,
    pub(crate) proof: RetainedInboundProof,
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
    pub(crate) fn broadcast(bytes: Vec<u8>) -> Self {
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
    pub(crate) link: [u8; TRUNCATED_HASH_LEN],
    pub(crate) request: [u8; TRUNCATED_HASH_LEN],
}

impl RequestHandle {
    pub(crate) fn from_native(confirmation: &NativeRequestConfirmation) -> Self {
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
    pub(crate) packet: TxPacket,
    pub(crate) handle: RequestHandle,
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

pub(crate) enum NativeRequestDispatchAuthority {
    Prepared(NativeRequestConfirmation),
    Confirmed(NativeConfirmedRequestDispatch),
}

pub(crate) struct RequestDispatchEntry {
    pub(crate) handle: RequestHandle,
    pub(crate) authority: Option<NativeRequestDispatchAuthority>,
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
pub(crate) fn resolve_origin_packet(packet: OutboundPacket) -> TxPacket {
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

pub(crate) const fn direct_response_body_maximum(link_mdu: usize) -> Option<usize> {
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

pub(crate) fn map_native_response_error(error: SendError) -> PrepareResponseError {
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
