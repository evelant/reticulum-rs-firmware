//! Owning, fail-closed firmware boundary around Rete's native `NodeCore`.
//!
//! Rete's heapless storage bounds several maps, but its public `NodeCore`
//! still permits oversized hosted ingress, silently loses some table
//! insertions, and exposes source-relative routing after the source interface
//! has been forgotten. This module owns the core and admits only the subset we
//! can currently make deterministic and observable on embedded targets.

use alloc::vec::Vec;

use rand_core::{CryptoRng, RngCore};
use rete_core::{
    CONTEXT_RESOURCE, CONTEXT_RESOURCE_ADV, CONTEXT_RESOURCE_HMU, CONTEXT_RESOURCE_ICL,
    CONTEXT_RESOURCE_PRF, CONTEXT_RESOURCE_RCL, CONTEXT_RESOURCE_REQ, DestHash, DestType,
    HeaderType, Identity, IdentityHash, LinkId, Packet, PacketType,
};
use rete_stack::{
    DestinationType, Direction, IngestOutcome, NodeCore, NodeEvent, OutboundPacket, PacketRouting,
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

/// Interface identifier carried across the synchronous ingest boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceId(pub u8);

/// Firmware routing target with source-relative routing already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxTarget {
    /// Send on every eligible interface.
    All,
    /// Send only on the interface that supplied the triggering packet.
    Only(InterfaceId),
    /// Send on every eligible interface except the triggering interface.
    AllExcept(InterfaceId),
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

/// Application events and resolved transmission actions from one core call.
#[derive(Debug, Default)]
pub struct NodeActions {
    /// Native Rete events. These remain provisional because several variants
    /// contain allocation-backed payloads.
    pub events: Vec<NodeEvent>,
    /// Packets with their interface target resolved while ingress context is
    /// still available.
    pub packets: Vec<TxPacket>,
    /// Native source-relative actions dropped because no source interface was
    /// available. This should remain zero for timer-driven work.
    pub unroutable_packets: usize,
}

/// Whether this node only terminates traffic or also forwards ordinary RNS
/// traffic for other nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// Local endpoint only.
    Endpoint,
    /// Forward announces and ordinary packets. Relayed LINKREQUEST admission
    /// remains disabled until Rete exposes a transactional relay-table API.
    Transport,
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

    /// Transport profile with fail-closed relayed Link admission.
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
    /// Rete reported a local Link handshake but did not retain its state.
    LinkStateNotRetained,
    /// Relayed Link state cannot yet be admitted transactionally through
    /// Rete's public API.
    RelayedLinksDisabled,
    /// HEADER_2 LINKREQUEST dispatch is ambiguous or targets another
    /// transport. It is rejected rather than falling through to local state.
    UnsupportedHeader2LinkRequest,
    /// A non-announce HEADER_2 packet names another transport identity.
    Header2NotAddressedToUs { transport_id: IdentityHash },
    /// The pinned native dispatcher cannot safely terminate or route this
    /// HEADER_2 packet class in the configured role.
    UnsupportedHeader2Dispatch {
        packet_type: PacketType,
        destination_type: DestType,
    },
    /// Forwarding would require reverse state that cannot be retained.
    ReverseTableFull { limit: usize },
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
            Self::LinkStateNotRetained => {
                write!(
                    f,
                    "Rete emitted Link handshake output without retained state"
                )
            }
            Self::RelayedLinksDisabled => {
                write!(f, "relayed Links require transactional Rete admission")
            }
            Self::UnsupportedHeader2LinkRequest => {
                write!(f, "unsupported HEADER_2 LINKREQUEST dispatch")
            }
            Self::Header2NotAddressedToUs { transport_id } => {
                write!(f, "HEADER_2 packet targets transport {transport_id:?}")
            }
            Self::UnsupportedHeader2Dispatch {
                packet_type,
                destination_type,
            } => write!(
                f,
                "unsupported HEADER_2 {packet_type:?}/{destination_type:?} dispatch"
            ),
            Self::ReverseTableFull { limit } => {
                write!(f, "reverse-routing table is full (limit {limit})")
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
    /// Events and already-resolved transmission actions.
    pub actions: NodeActions,
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
    pub link_state_not_retained: u64,
    pub relayed_links_disabled: u64,
    pub native_duplicate: u64,
    pub native_invalid: u64,
    pub native_no_outcome: u64,
    pub premature_link_events_suppressed: u64,
    pub routing_invariant_drops: u64,
    pub header2_filtered: u64,
    pub reverse_table_full: u64,
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
    pub transport: TransportCounters,
    pub capacity: HeaplessCapacitySnapshot,
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

    /// Current state of one locally owned Link.
    pub fn link_state(&self, link_id: &LinkId) -> Option<LinkState> {
        self.core.transport.get_link(link_id).map(|link| link.state)
    }

    /// Allocation-free metrics and capacity snapshot.
    pub fn metrics(&self) -> EmbeddedNodeMetrics {
        EmbeddedNodeMetrics {
            ingress: self.ingress,
            admission: self.admission,
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
        self.ingress.seen = self.ingress.seen.saturating_add(1);

        if raw.len() > rete_core::MTU {
            return self.reject(IngressDropReason::PacketTooLong {
                actual: raw.len(),
                maximum: rete_core::MTU,
            });
        }

        let packet = match Packet::parse(raw) {
            Ok(packet) => packet,
            Err(error) => return self.reject(IngressDropReason::Malformed(error)),
        };

        if packet.dest_type == DestType::Link && is_resource_context(packet.context) {
            return self.reject(IngressDropReason::ResourceIngressDisabled {
                context: packet.context,
            });
        }

        if let Err(reason) = self.preflight_header2(&packet) {
            return self.reject(reason);
        }

        if let Err(reason) = self.preflight_h1_reverse_admission(&packet) {
            return self.reject(reason);
        }

        let inbound_link_id = if packet.packet_type == PacketType::LinkRequest {
            match self.preflight_link_request(&packet, raw) {
                Ok(link_id) => link_id,
                Err(reason) => return self.reject(reason),
            }
        } else {
            None
        };

        let before_duplicate = self.core.transport.stats().packets_dropped_dedup;
        let before_invalid = self.core.transport.stats().packets_dropped_invalid;
        let mut native = self.core.handle_ingest(raw, now, interface.0, rng);

        if let Some(link_id) = inbound_link_id {
            let announced_establishment = native.events.iter().any(|event| {
                matches!(event, NodeEvent::LinkEstablished { link_id: event_id } if *event_id == link_id)
            });
            if announced_establishment && self.core.transport.get_link(&link_id).is_none() {
                self.ingress.link_state_not_retained =
                    self.ingress.link_state_not_retained.saturating_add(1);
                return self.reject(IngressDropReason::LinkStateNotRetained);
            }
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
                .retain(|packet| packet.routing != PacketRouting::AllExceptSource);
            self.ingress.endpoint_forward_suppressed = self
                .ingress
                .endpoint_forward_suppressed
                .saturating_add((before_packets - native.packets.len()) as u64);
        }

        self.ingress.admitted = self.ingress.admitted.saturating_add(1);
        let after = self.core.transport.stats();
        let disposition = if after.packets_dropped_dedup > before_duplicate {
            self.ingress.native_duplicate = self.ingress.native_duplicate.saturating_add(1);
            IngressDisposition::NativeDuplicate
        } else if after.packets_dropped_invalid > before_invalid {
            self.ingress.native_invalid = self.ingress.native_invalid.saturating_add(1);
            IngressDisposition::NativeInvalid
        } else if native.events.is_empty() && native.packets.is_empty() {
            self.ingress.native_no_outcome = self.ingress.native_no_outcome.saturating_add(1);
            IngressDisposition::NoObservableOutcome
        } else {
            IngressDisposition::Processed
        };

        IngressReport {
            disposition,
            actions: resolve_ingest_actions(native, interface),
        }
    }

    fn preflight_link_request(
        &mut self,
        packet: &Packet<'_>,
        raw: &[u8],
    ) -> Result<Option<LinkId>, IngressDropReason> {
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
        let local_transport = self.core.transport.local_identity_hash();
        if packet.header_type == HeaderType::Header2 {
            let targeted_to_us = local_transport.is_some()
                && packet
                    .transport_id
                    .map(IdentityHash::from_slice)
                    .is_some_and(|transport| Some(transport) == local_transport);
            if targeted_to_us && !self.core.transport.is_local_destination(&destination) {
                return Err(IngressDropReason::RelayedLinksDisabled);
            }
            return Err(IngressDropReason::UnsupportedHeader2LinkRequest);
        }

        if !self.core.transport.is_local_destination(&destination) {
            return if self.role == NodeRole::Transport {
                Err(IngressDropReason::RelayedLinksDisabled)
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
        let is_existing = self.core.transport.get_link(&link_id).is_some();
        if !is_existing && self.core.transport.link_count() >= LINKS {
            // Preserve Rete/Python packet-filter semantics without performing
            // responder ECDH on a saturated node. Once capacity is available,
            // an exact replay will be rejected by native ingest while a fresh
            // LINKREQUEST with new ephemeral material can proceed.
            let packet_hash = packet.compute_hash();
            let _ = self.core.transport.is_duplicate(&packet_hash);
            return Err(IngressDropReason::OwnedLinkTableFull { limit: LINKS });
        }
        Ok(Some(link_id))
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
        // Rete instead enters its targeted relay branch when the ID is ours.
        if packet.packet_type == PacketType::Announce {
            return if self.role == NodeRole::Transport && transport_id == ours {
                Err(IngressDropReason::UnsupportedHeader2Dispatch {
                    packet_type: packet.packet_type,
                    destination_type: packet.dest_type,
                })
            } else {
                Ok(())
            };
        }

        if transport_id != ours {
            return Err(IngressDropReason::Header2NotAddressedToUs { transport_id });
        }

        // LINKREQUEST has stricter destination and table admission below.
        if packet.packet_type == PacketType::LinkRequest {
            return Ok(());
        }

        let destination = DestHash::from_slice(packet.destination_hash);
        match self.role {
            NodeRole::Endpoint => {
                let local_data = packet.packet_type == PacketType::Data
                    && packet.dest_type != DestType::Link
                    && self
                        .core
                        .get_destination(&destination)
                        .is_some_and(|registered| {
                            registered.direction == Direction::In
                                && destination_type_matches(packet.dest_type, registered.dest_type)
                        });
                let owned_link = packet.packet_type == PacketType::Data
                    && packet.dest_type == DestType::Link
                    && self
                        .core
                        .transport
                        .get_link(&LinkId::from_slice(destination.as_ref()))
                        .is_some();
                if local_data || owned_link || packet.packet_type == PacketType::Proof {
                    Ok(())
                } else {
                    Err(IngressDropReason::UnsupportedHeader2Dispatch {
                        packet_type: packet.packet_type,
                        destination_type: packet.dest_type,
                    })
                }
            }
            NodeRole::Transport => {
                let relayable = packet.packet_type == PacketType::Data
                    && packet.dest_type != DestType::Link
                    && destination != rete_transport::PATH_REQUEST_DEST
                    && !self.core.transport.is_local_destination(&destination)
                    && self.core.transport.get_path(&destination).is_some();
                if !relayable {
                    return Err(IngressDropReason::UnsupportedHeader2Dispatch {
                        packet_type: packet.packet_type,
                        destination_type: packet.dest_type,
                    });
                }
                self.preflight_reverse_admission(packet)
            }
        }
    }

    fn preflight_h1_reverse_admission(&self, packet: &Packet<'_>) -> Result<(), IngressDropReason> {
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
        self.preflight_reverse_admission(packet)
    }

    fn preflight_reverse_admission(&self, packet: &Packet<'_>) -> Result<(), IngressDropReason> {
        let packet_hash = packet.compute_hash();
        let mut key = [0u8; rete_core::TRUNCATED_HASH_LEN];
        key.copy_from_slice(&packet_hash[..rete_core::TRUNCATED_HASH_LEN]);
        if self.core.transport.get_reverse(&key).is_none()
            && self.core.transport.reverse_count() >= PATHS
        {
            return Err(IngressDropReason::ReverseTableFull { limit: PATHS });
        }
        Ok(())
    }

    fn reject(&mut self, reason: IngressDropReason) -> IngressReport {
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
            IngressDropReason::LinkStateNotRetained => {}
            IngressDropReason::RelayedLinksDisabled
            | IngressDropReason::UnsupportedHeader2LinkRequest => {
                self.ingress.relayed_links_disabled =
                    self.ingress.relayed_links_disabled.saturating_add(1);
            }
            IngressDropReason::Header2NotAddressedToUs { .. }
            | IngressDropReason::UnsupportedHeader2Dispatch { .. } => {
                self.ingress.header2_filtered = self.ingress.header2_filtered.saturating_add(1);
            }
            IngressDropReason::ReverseTableFull { .. } => {
                self.ingress.reverse_table_full = self.ingress.reverse_table_full.saturating_add(1);
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
            actions: NodeActions::default(),
        }
    }

    /// Build encrypted destination DATA only when its delivery receipt can be
    /// retained.
    pub fn send_data<R: RngCore + CryptoRng>(
        &mut self,
        destination: &DestHash,
        plaintext: &[u8],
        now: u64,
        rng: &mut R,
    ) -> Result<TxPacket, EmbeddedSendError> {
        if plaintext.len() > rete_core::ENCRYPTED_MDU {
            self.admission.data_payload_too_large =
                self.admission.data_payload_too_large.saturating_add(1);
            return Err(EmbeddedSendError::DataPayloadTooLarge {
                actual: plaintext.len(),
                maximum: rete_core::ENCRYPTED_MDU,
            });
        }
        if self.core.transport.receipt_count() >= PATHS {
            self.admission.receipt_table_full = self.admission.receipt_table_full.saturating_add(1);
            return Err(EmbeddedSendError::ReceiptTableFull { limit: PATHS });
        }
        let bytes = self
            .core
            .build_data_packet(destination, plaintext, rng, now)?;
        Ok(TxPacket::broadcast(bytes))
    }

    /// Initiate a locally owned Link only when Rete can retain its state.
    pub fn initiate_link<R: RngCore + CryptoRng>(
        &mut self,
        destination: DestHash,
        now: u64,
        rng: &mut R,
    ) -> Result<(TxPacket, LinkId), LinkAdmissionError> {
        match try_initiate_heapless_link(&mut self.core, destination, now, rng) {
            Ok((packet, link_id)) => Ok((TxPacket::broadcast(packet.data), link_id)),
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
        Ok(TxPacket::broadcast(packet.data))
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
        Ok(TxPacket::broadcast(packet.data))
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
            packets: packet
                .into_iter()
                .map(|packet| TxPacket::broadcast(packet.data))
                .collect(),
            unroutable_packets: 0,
        }
    }

    /// Run timer maintenance. Timer-driven native output is expected to be
    /// broadcast; any source-relative action is dropped and counted rather
    /// than guessed onto an interface.
    pub fn tick<R: RngCore + CryptoRng>(&mut self, now: u64, rng: &mut R) -> NodeActions {
        let native = self.core.handle_tick(now, rng);
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

fn destination_type_matches(wire: DestType, registered: DestinationType) -> bool {
    matches!(
        (wire, registered),
        (DestType::Single, DestinationType::Single)
            | (DestType::Group, DestinationType::Group)
            | (DestType::Plain, DestinationType::Plain)
            | (DestType::Link, DestinationType::Link)
    )
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
        if packet.routing == PacketRouting::All {
            packets.push(TxPacket::broadcast(packet.data));
        } else {
            unroutable_packets += 1;
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
    use super::*;
    use rete_core::{CONTEXT_LRPROOF, CONTEXT_LRRTT, PacketBuilder};

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

    type TestNode = EmbeddedNode<4, 2, 8, 2>;
    type TwoLinkNode = EmbeddedNode<4, 2, 8, 2>;

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

    #[test]
    fn wrapper_resolves_source_relative_routing_and_completes_link_lifecycle() {
        let mut initiator = node(1);
        let mut responder = node(2);
        let mut rng = CounterRng::default();

        let (request, link_id) = link_request(&mut initiator, &responder, &mut rng);
        assert_eq!(request.target, TxTarget::All);

        let response = responder.ingest(&request.bytes, 100, InterfaceId(7), &mut rng);
        assert_eq!(response.disposition, IngressDisposition::Processed);
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
        let received = responder.ingest(&data.bytes, 110, InterfaceId(7), &mut rng);
        assert!(matches!(
            received.actions.events.first(),
            Some(NodeEvent::LinkData { link_id: id, data, .. })
                if *id == link_id && data == b"bounded link payload"
        ));
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

        responder.close_link(&first_link_id.unwrap(), &mut rng);
        assert_eq!(responder.metrics().capacity.links.used, 1);

        let replay = responder.ingest(&overflow_request.bytes, 4, InterfaceId(3), &mut rng);
        assert_eq!(replay.disposition, IngressDisposition::NativeDuplicate);
        assert!(replay.actions.events.is_empty());
        assert!(replay.actions.packets.is_empty());
        assert_eq!(responder.metrics().capacity.links.used, 1);

        let (fresh_request, fresh_link_id) =
            overflow.initiate_link(destination, 5, &mut rng).unwrap();
        assert_ne!(fresh_link_id, overflow_link_id);
        let fresh = responder.ingest(&fresh_request.bytes, 5, InterfaceId(3), &mut rng);
        assert_eq!(fresh.disposition, IngressDisposition::Processed);
        assert_eq!(fresh.actions.packets.len(), 1);
        assert_eq!(responder.metrics().capacity.links.used, 2);
    }

    #[test]
    fn transport_rejects_relay_link_state_until_admission_is_transactional() {
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
            IngressDisposition::Rejected(IngressDropReason::RelayedLinksDisabled)
        );
        assert_eq!(relay.metrics().transport.packets_received, 0);
    }

    #[test]
    fn header2_policy_filters_other_transports_and_unsafe_local_termination() {
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
            .dest_type(DestType::Plain)
            .destination_hash(plain_destination.as_ref())
            .context(0)
            .payload(b"local plain payload")
            .via(Some(endpoint_transport.as_bytes()))
            .build()
            .unwrap();
        let admitted = endpoint.ingest(&raw[..len], 2, InterfaceId(1), &mut rng);
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
        let announce = endpoint.ingest(&raw[..len], 3, InterfaceId(1), &mut rng);
        assert!(!matches!(
            announce.disposition,
            IngressDisposition::Rejected(IngressDropReason::UnsupportedHeader2Dispatch { .. })
        ));

        let mut transport = TestNode::new(
            identity(24),
            "reticulum",
            &["embedded"],
            EmbeddedNodeConfig::transport(),
        )
        .unwrap();
        let transport_identity = transport.identity_hash();
        let mut raw = [0u8; rete_core::MTU];
        let len = PacketBuilder::new(&mut raw)
            .packet_type(PacketType::Data)
            .dest_type(DestType::Single)
            .destination_hash(transport.destination_hash().as_ref())
            .context(0)
            .payload(b"must terminate locally")
            .via(Some(transport_identity.as_bytes()))
            .build()
            .unwrap();
        let rejected = transport.ingest(&raw[..len], 2, InterfaceId(1), &mut rng);
        assert!(matches!(
            rejected.disposition,
            IngressDisposition::Rejected(IngressDropReason::UnsupportedHeader2Dispatch {
                packet_type: PacketType::Data,
                destination_type: DestType::Single,
            })
        ));
        assert_eq!(transport.metrics().ingress.header2_filtered, 1);
        assert_eq!(transport.metrics().transport.packets_received, 0);
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
        }
        assert_eq!(relay.metrics().capacity.reverse_entries.used, 4);

        let overflow_tag = 74;
        let peer = identity(overflow_tag);
        let destination = node(overflow_tag).destination_hash();
        relay
            .register_peer(&peer, "reticulum", &["embedded"], u64::from(overflow_tag))
            .unwrap();
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
        assert_eq!(
            report.disposition,
            IngressDisposition::Rejected(IngressDropReason::ReverseTableFull { limit: 4 })
        );
        assert!(report.actions.packets.is_empty());
        assert_eq!(relay.metrics().transport.packets_received, 4);
        assert_eq!(relay.metrics().ingress.reverse_table_full, 1);
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
