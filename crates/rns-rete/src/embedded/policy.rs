//! Node policy, configuration, ingress reporting, and error vocabulary.

use super::*;

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
    pub(crate) fn into_rete(self) -> rete_stack::ProofStrategy {
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
    /// Bitmask of interface indices treated as shared-medium.
    ///
    /// Shared-medium routes (for example LoRa) are reserved against eviction by
    /// point-to-point announce churn in the bounded path table. Set one bit per
    /// shared-medium interface index.
    pub shared_medium_interfaces: u64,
}

impl EmbeddedNodeConfig {
    /// Conservative endpoint profile.
    pub const fn endpoint() -> Self {
        Self {
            role: NodeRole::Endpoint,
            max_additional_destinations: 4,
            shared_medium_interfaces: 0,
        }
    }

    /// Transport profile with fail-closed bounded relay admission.
    pub const fn transport() -> Self {
        Self {
            role: NodeRole::Transport,
            max_additional_destinations: 4,
            shared_medium_interfaces: 0,
        }
    }

    /// Mark additional shared-medium interface indices, returning the profile.
    pub const fn with_shared_medium_interfaces(mut self, mask: u64) -> Self {
        self.shared_medium_interfaces = mask;
        self
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
    pub(crate) wire_packet_type: Option<PacketType>,
    pub(crate) emitted_packets: u16,
    pub(crate) generated_proof_actions: u16,
    pub(crate) delivered_receipt_terminals: u16,
    pub(crate) timed_out_receipt_terminals: u16,
    pub(crate) generated_proof_tag: u64,
    pub(crate) delivered_receipt_tag: u64,
    pub(crate) generated_proof_tag_present: bool,
    pub(crate) delivered_receipt_tag_present: bool,
    pub(crate) generated_proof_tags_consistent: bool,
    pub(crate) delivered_receipt_tags_consistent: bool,
    pub(crate) counts_saturated: bool,
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
    pub(crate) fn parsed(packet: &Packet<'_>) -> Self {
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
