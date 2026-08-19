//! Owning, fail-closed firmware boundary around Rete's native `NodeCore`.
//!
//! Rete's heapless storage bounds several maps, but its public `NodeCore`
//! still permits oversized hosted ingress, silently loses some table
//! insertions, and exposes interface routing after the native ingress context
//! has been forgotten. This module owns the core and resolves that routing
//! while the relevant context is still available, admitting only the subset we
//! can make deterministic and observable on embedded targets.

pub(crate) use alloc::{collections::BTreeMap, vec::Vec};
pub(crate) use core::{mem::MaybeUninit, ptr::addr_of_mut};

pub(crate) use rand_core::{CryptoRng, RngCore};
pub(crate) use rete_core::{
    CONTEXT_NONE, CONTEXT_PATH_RESPONSE, CONTEXT_RESOURCE, CONTEXT_RESOURCE_ADV,
    CONTEXT_RESOURCE_HMU, CONTEXT_RESOURCE_ICL, CONTEXT_RESOURCE_PRF, CONTEXT_RESOURCE_RCL,
    CONTEXT_RESOURCE_REQ, DestHash, DestType, HeaderType, Identity, IdentityHash, LinkId,
    MonotonicInstant, Packet, PacketBuilder, PacketType, TRUNCATED_HASH_LEN,
};
pub(crate) use rete_lxmf_core::{LXMessage, LxmfMessageError};
pub(crate) use rete_stack::node_core::{OutboundDispatchInterval, OutboundProtocolToken};
pub(crate) use rete_stack::{
    ConfirmedRequestDispatch as NativeConfirmedRequestDispatch, DestinationType, Direction,
    IngestOutcome, IngestRejection, LinkTableKind, NodeCore, NodeEvent as NativeNodeEvent,
    OutboundPacket, PacketRouting, PreparedRequestConfirmation as NativeRequestConfirmation,
    ReceiptToken, RequestDispatchError as NativeRequestDispatchError,
    RequestFailReason as NativeRequestFailReason,
};
pub(crate) use rete_transport::{HeaplessStorage, LinkRole, LinkState, SendError, Transport};
pub(crate) use reticulum_lxmf_wire::{
    FIELD_TELEMETRY, MAX_ENCODED_SIDEBAND_LOCATION_TELEMETRY_BYTES, SidebandLocationTelemetry,
    encode_sideband_location_telemetry,
};
pub(crate) use sha2::{Digest, Sha256};

pub(crate) use crate::capacity::{
    HeaplessCapacitySnapshot, LinkAdmissionError, heapless_capacity_snapshot,
    try_initiate_heapless_link_at,
};

mod types;
pub use types::*;
mod application_event;
pub use application_event::*;
mod node_actions;
pub use node_actions::*;
mod policy;
pub use policy::*;

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
    /// Mark the configured shared-medium interface indices on the inner
    /// transport so announce learning records the correct media class.
    fn apply_shared_medium_interfaces(
        core: &mut NodeCore<HeaplessStorage<PATHS, ANNOUNCES, DEDUPLICATION, LINKS>>,
        mask: u64,
    ) {
        for iface in 0..64u8 {
            if mask & (1u64 << iface) != 0 {
                core.transport.mark_interface_shared_medium(iface);
            }
        }
    }

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
        Self::apply_shared_medium_interfaces(&mut core, config.shared_medium_interfaces);
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
        Self::apply_shared_medium_interfaces(core, config.shared_medium_interfaces);

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
                .and_then(|path| path.announce_raw.as_ref().map(|raw| raw.as_slice()))
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
        let Some(cached) = path.announce_raw.as_ref().map(|raw| raw.as_slice()) else {
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
