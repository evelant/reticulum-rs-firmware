use super::*;

/// Stable index of one bounded actor queue within an interface fabric.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InterfaceQueueId(pub(crate) u16);

impl InterfaceQueueId {
    /// Construct a queue identifier from its fixed slot index.
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    /// Fixed slot index represented by this identifier.
    pub const fn get(self) -> u16 {
        self.0
    }

    pub(crate) fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Non-repeating configuration generation for one registry queue slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InterfaceGeneration(pub(crate) u64);

impl InterfaceGeneration {
    /// Monotonic queue-local generation value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Opaque product configuration identity understood by an interface actor.
///
/// The router compares and transports this scalar but never interprets it.
/// A LoRa actor may bind it to one modem configuration while a future stream
/// actor may bind it to an entirely different configuration domain.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InterfaceConfigId(u32);

impl InterfaceConfigId {
    /// Construct an opaque interface configuration identity.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Raw product-owned configuration identity.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Nonzero logical packet MTU accepted by one Reticulum interface.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LogicalMtu(u16);

impl LogicalMtu {
    /// Construct a nonzero logical MTU.
    pub const fn try_new(bytes: u16) -> Result<Self, LogicalMtuError> {
        if bytes == 0 {
            Err(LogicalMtuError::Zero)
        } else {
            Ok(Self(bytes))
        }
    }

    /// Maximum complete native Reticulum packet length accepted by the actor.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Invalid logical interface MTU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalMtuError {
    /// A packet interface cannot advertise a zero-byte MTU.
    Zero,
}

/// Optional nonzero bitrate advertised by one interface actor.
///
/// This is capability/cost metadata only. The initial router does not choose
/// routes from it because node-core has already selected the current hop.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AdvertisedBitrate(u32);

impl AdvertisedBitrate {
    /// Construct a nonzero advertised bitrate in bits per second.
    pub const fn try_new(bits_per_second: u32) -> Result<Self, AdvertisedBitrateError> {
        if bits_per_second == 0 {
            Err(AdvertisedBitrateError::Zero)
        } else {
            Ok(Self(bits_per_second))
        }
    }

    /// Advertised bits per second.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Invalid advertised interface bitrate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdvertisedBitrateError {
    /// A present bitrate must be nonzero; use `None` when it is unknown.
    Zero,
}

/// Product-owned relative interface cost.
///
/// Lower values conventionally mean more preferable, but this first router
/// records rather than schedules by the value.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InterfaceCost(u16);

impl InterfaceCost {
    /// Construct a relative interface cost. Zero is a valid highest-priority
    /// value.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Raw product-owned relative cost.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Peer topology behind one Reticulum packet interface.
///
/// This is transport-neutral forwarding policy rather than a concrete bearer
/// type. A radio, stream, or future interface actor selects the topology that
/// matches the peers reachable through its one logical slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceTopology {
    /// Multiple peers can be reached through the same logical interface.
    ///
    /// A packet received from one peer may therefore legitimately need to be
    /// rebroadcast on the ingress interface to reach another peer.
    SharedMedium,
    /// The logical interface has exactly one remote peer.
    ///
    /// Rebroadcasting an ingress broadcast on the same interface can only
    /// reflect it to the peer that supplied it.
    PointToPoint,
}

/// Announce-propagation role of one Reticulum interface.
///
/// This initial product surface implements the `full`, `boundary`, and
/// `internal` subset of Reticulum's interface-mode matrix. It is independent
/// of concrete bearer type and separate from peer topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnouncePropagationMode {
    /// Default Reticulum interface behavior.
    Full,
    /// Interface facing a substantially different or public network segment.
    Boundary,
    /// Interface belonging to the private side of a boundary.
    Internal,
}

impl AnnouncePropagationMode {
    pub(crate) const fn accepts_from(self, source: Self) -> bool {
        !matches!((source, self), (Self::Boundary, Self::Internal))
    }
}

/// Recursive unknown-path search role of one Reticulum interface.
///
/// Python Reticulum applies this policy independently from announce
/// propagation. In particular, a Boundary request searches only Boundary and
/// Gateway interfaces; it does not search a Full interface merely because
/// ordinary announces can cross that pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecursivePathSearchMode {
    /// Full and point-to-point interfaces do not initiate recursive searches.
    Disabled,
    /// Internal, access-point and roaming interfaces search every other online
    /// interface.
    Unrestricted,
    /// Boundary interfaces search only Boundary and Gateway interfaces.
    Boundary,
    /// Gateway interfaces search every other online interface and are eligible
    /// targets of Boundary searches.
    Gateway,
}

impl RecursivePathSearchMode {
    pub(crate) const fn boundary_search_target(self) -> bool {
        matches!(self, Self::Boundary | Self::Gateway)
    }
}

/// Transport-neutral properties interpreted by an interface actor or future
/// route policy, never by a concrete-radio branch in this registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceProperties {
    pub(crate) logical_mtu: LogicalMtu,
    pub(crate) config: InterfaceConfigId,
    pub(crate) advertised_bitrate: Option<AdvertisedBitrate>,
    pub(crate) cost: InterfaceCost,
    pub(crate) topology: InterfaceTopology,
    pub(crate) announce_mode: AnnouncePropagationMode,
    pub(crate) recursive_path_search_mode: RecursivePathSearchMode,
}

impl InterfaceProperties {
    /// Construct a shared-medium transport-neutral property snapshot.
    ///
    /// Shared-medium preserves the original routing behavior. Point-to-point
    /// actors must opt in with [`Self::with_topology`].
    pub const fn new(
        logical_mtu: LogicalMtu,
        config: InterfaceConfigId,
        advertised_bitrate: Option<AdvertisedBitrate>,
        cost: InterfaceCost,
    ) -> Self {
        Self {
            logical_mtu,
            config,
            advertised_bitrate,
            cost,
            topology: InterfaceTopology::SharedMedium,
            announce_mode: AnnouncePropagationMode::Full,
            recursive_path_search_mode: RecursivePathSearchMode::Disabled,
        }
    }

    /// Select the peer topology behind this logical interface.
    pub const fn with_topology(mut self, topology: InterfaceTopology) -> Self {
        self.topology = topology;
        self
    }

    /// Select this interface's announce-propagation role.
    pub const fn with_announce_mode(mut self, mode: AnnouncePropagationMode) -> Self {
        self.announce_mode = mode;
        self
    }

    /// Select how unknown path requests received on this interface recurse.
    pub const fn with_recursive_path_search_mode(mut self, mode: RecursivePathSearchMode) -> Self {
        self.recursive_path_search_mode = mode;
        self
    }

    /// Maximum complete native Reticulum packet length.
    pub const fn logical_mtu(self) -> LogicalMtu {
        self.logical_mtu
    }

    /// Opaque actor configuration identity.
    pub const fn config(self) -> InterfaceConfigId {
        self.config
    }

    /// Advertised nonzero bitrate, or `None` when unknown/not meaningful.
    pub const fn advertised_bitrate(self) -> Option<AdvertisedBitrate> {
        self.advertised_bitrate
    }

    /// Product-owned relative interface cost.
    pub const fn cost(self) -> InterfaceCost {
        self.cost
    }

    /// Peer topology used for ingress-broadcast forwarding policy.
    pub const fn topology(self) -> InterfaceTopology {
        self.topology
    }

    /// Reticulum announce-propagation role.
    pub const fn announce_mode(self) -> AnnouncePropagationMode {
        self.announce_mode
    }

    /// Reticulum recursive unknown-path search role.
    pub const fn recursive_path_search_mode(self) -> RecursivePathSearchMode {
        self.recursive_path_search_mode
    }
}

/// Generation-safe authority for one registered interface queue.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InterfaceLease {
    pub(crate) interface: PacketInterfaceId,
    pub(crate) queue: InterfaceQueueId,
    pub(crate) generation: InterfaceGeneration,
}

/// Concrete actor lifecycle state requested from the authoritative registry.
///
/// `Ready` means the actor has completed transport-specific initialization and
/// may accept new routed owners. `Offline` removes it from new route
/// resolution without invalidating owners already accepted under the same
/// generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceLifecycleState {
    /// The concrete actor is ready to accept newly routed owners.
    Ready,
    /// The concrete actor must receive no newly routed owners.
    Offline,
}

impl InterfaceLifecycleState {
    pub(crate) const fn online(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// One generation-bound lifecycle request emitted by a concrete actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceLifecycleRequest {
    pub(crate) lease: InterfaceLease,
    pub(crate) state: InterfaceLifecycleState,
}

impl InterfaceLifecycleRequest {
    /// Exact registry generation the actor is reporting about.
    pub const fn lease(self) -> InterfaceLease {
        self.lease
    }

    /// Actor-observed lifecycle state to apply.
    pub const fn state(self) -> InterfaceLifecycleState {
        self.state
    }
}

/// Why an actor lifecycle request did not change registry eligibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceLifecycleRouteError {
    /// The request arrived through a different fixed actor queue than its
    /// supplied lease names.
    ForeignQueue {
        /// Queue through which the router observed the request.
        observed: InterfaceQueueId,
        /// Queue named by the supplied lease.
        supplied: InterfaceQueueId,
    },
    /// The supplied generation is not the current authoritative lease.
    InvalidLease(InterfaceLeaseError),
}

/// Supervisor acknowledgement for one exact actor lifecycle request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceLifecycleAcknowledgement {
    pub(crate) request: InterfaceLifecycleRequest,
    pub(crate) result: Result<InterfaceDescriptor, InterfaceLifecycleRouteError>,
}

impl InterfaceLifecycleAcknowledgement {
    /// Exact request being acknowledged.
    pub const fn request(self) -> InterfaceLifecycleRequest {
        self.request
    }

    /// Applied authoritative descriptor or typed fail-closed rejection.
    pub const fn result(self) -> Result<InterfaceDescriptor, InterfaceLifecycleRouteError> {
        self.result
    }
}

/// One lifecycle request consumed, validated, applied or rejected, and
/// acknowledged by the router.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceLifecycleTransition {
    pub(crate) queue: InterfaceQueueId,
    pub(crate) acknowledgement: InterfaceLifecycleAcknowledgement,
}

impl InterfaceLifecycleTransition {
    /// Fixed actor queue through which the request was observed.
    pub const fn queue(self) -> InterfaceQueueId {
        self.queue
    }

    /// Exact acknowledgement already returned to the actor.
    pub const fn acknowledgement(self) -> InterfaceLifecycleAcknowledgement {
        self.acknowledgement
    }
}

impl InterfaceLease {
    /// Stable Reticulum packet-interface identity.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Fixed bounded actor queue selected by this lease.
    pub const fn queue(self) -> InterfaceQueueId {
        self.queue
    }

    /// Queue-local configuration generation.
    pub const fn generation(self) -> InterfaceGeneration {
        self.generation
    }
}

/// Complete authoritative registry record for one interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceDescriptor {
    pub(crate) lease: InterfaceLease,
    pub(crate) online: bool,
    pub(crate) properties: InterfaceProperties,
}

impl InterfaceDescriptor {
    /// Generation-safe interface authority.
    pub const fn lease(self) -> InterfaceLease {
        self.lease
    }

    /// Whether new outbound jobs may enter this interface queue.
    ///
    /// Jobs accepted before an online-to-offline transition retain their
    /// exact lease and may still return a completion.
    pub const fn is_online(self) -> bool {
        self.online
    }

    /// Maximum complete native packet length accepted by this interface.
    pub const fn logical_mtu(self) -> LogicalMtu {
        self.properties.logical_mtu
    }

    /// Opaque product configuration identity supplied to the actor.
    pub const fn config(self) -> InterfaceConfigId {
        self.properties.config
    }

    /// Advertised nonzero bitrate, if known and meaningful for this actor.
    pub const fn advertised_bitrate(self) -> Option<AdvertisedBitrate> {
        self.properties.advertised_bitrate
    }

    /// Product-owned relative interface cost.
    pub const fn cost(self) -> InterfaceCost {
        self.properties.cost
    }

    /// Complete transport-neutral actor property snapshot.
    pub const fn properties(self) -> InterfaceProperties {
        self.properties
    }
}
