//! Fixed, transport-neutral outbound routing between one Reticulum node owner
//! and bounded per-interface actors.
//!
//! Node-core has already resolved `Only`, `All`, and `AllExcept` into one
//! selected [`PacketInterfaceId`] before a job reaches this crate. The router
//! therefore does not reinterpret Reticulum targets or perform parallel
//! fan-out. It validates that selected interface against an authoritative
//! fixed registry, stamps the current interface lease and configuration
//! snapshot, and moves the exact DATA or ordinary owner into that interface's
//! queue. Node-core remains the only component that may turn a completion into
//! the next serialized fan-out hop.
//!
//! This boundary deliberately knows nothing about LoRa, RNode, CAD, airtime,
//! frequencies, USB, BLE, Wi-Fi, or any concrete driver. An interface actor
//! receives only the queue capability for its fixed queue slot. Queue
//! pressure, offline interfaces, stale leases, crossed completions, and MTU
//! rejection all return the unchanged non-`Copy` owner to the caller.
//!
//! The same fixed fabric owns a transport-neutral native-packet ingress pool
//! for every actor. Concrete actors borrow mutable packet capacity only while
//! holding an [`AvailableIngressBuffer`], seal a validated packet length, and
//! submit that exact non-`Copy` owner with registry-issued provenance. The
//! router validates the observed queue and current lease before immutable
//! bytes reach node-core, then recycles the exact buffer to its original actor
//! queue.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::{
    array, fmt,
    future::poll_fn,
    mem,
    task::{Context, Poll},
};

use embassy_sync::{
    blocking_mutex::raw::RawMutex,
    channel::{Channel, TrySendError},
};
use reticulum_node_core::{
    InterfaceSet, OrdinaryPreparedPacket, OrdinaryTxCompletion, OrdinaryTxJob, PACKET_CAPACITY,
    PacketInterfaceId, PreparedPacket, RoutedTxJob, TxCompletion,
};

/// Stable index of one bounded actor queue within an interface fabric.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InterfaceQueueId(u16);

impl InterfaceQueueId {
    /// Construct a queue identifier from its fixed slot index.
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    /// Fixed slot index represented by this identifier.
    pub const fn get(self) -> u16 {
        self.0
    }

    fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Non-repeating configuration generation for one registry queue slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InterfaceGeneration(u64);

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
    const fn accepts_from(self, source: Self) -> bool {
        !matches!((source, self), (Self::Boundary, Self::Internal))
    }
}

/// Transport-neutral properties interpreted by an interface actor or future
/// route policy, never by a concrete-radio branch in this registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceProperties {
    logical_mtu: LogicalMtu,
    config: InterfaceConfigId,
    advertised_bitrate: Option<AdvertisedBitrate>,
    cost: InterfaceCost,
    topology: InterfaceTopology,
    announce_mode: AnnouncePropagationMode,
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
}

/// Generation-safe authority for one registered interface queue.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InterfaceLease {
    interface: PacketInterfaceId,
    queue: InterfaceQueueId,
    generation: InterfaceGeneration,
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
    const fn online(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// One generation-bound lifecycle request emitted by a concrete actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceLifecycleRequest {
    lease: InterfaceLease,
    state: InterfaceLifecycleState,
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
    request: InterfaceLifecycleRequest,
    result: Result<InterfaceDescriptor, InterfaceLifecycleRouteError>,
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
    queue: InterfaceQueueId,
    acknowledgement: InterfaceLifecycleAcknowledgement,
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
    lease: InterfaceLease,
    online: bool,
    properties: InterfaceProperties,
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

#[derive(Clone, Copy)]
struct RegistrySlot {
    last_generation: u64,
    descriptor: Option<InterfaceDescriptor>,
}

impl RegistrySlot {
    const fn vacant() -> Self {
        Self {
            last_generation: 0,
            descriptor: None,
        }
    }
}

/// Failure to register one interface in a fixed queue slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceRegistrationError {
    /// Queue index is outside the registry's fixed capacity.
    QueueOutsideRegistry(InterfaceQueueId),
    /// The requested queue already has an active registration.
    QueueOccupied(InterfaceDescriptor),
    /// Another queue already owns the stable Reticulum interface identity.
    DuplicateInterface {
        /// Interface identity already registered elsewhere.
        interface: PacketInterfaceId,
        /// Authoritative existing queue.
        queue: InterfaceQueueId,
    },
    /// The queue-local generation counter cannot advance safely.
    GenerationExhausted(InterfaceQueueId),
}

/// Failure to validate or mutate one generation-safe interface lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceLeaseError {
    /// Queue index is outside the registry's fixed capacity.
    QueueOutsideRegistry(InterfaceQueueId),
    /// Queue has no active interface registration.
    QueueVacant(InterfaceQueueId),
    /// Queue is active, but no longer under the supplied interface generation.
    Stale {
        /// Lease rejected by the registry.
        supplied: InterfaceLease,
        /// Current authoritative descriptor.
        current: InterfaceDescriptor,
    },
    /// Reconfiguration cannot allocate another generation.
    GenerationExhausted(InterfaceQueueId),
    /// Another queue already owns the replacement stable interface identity.
    DuplicateInterface {
        /// Interface identity already registered elsewhere.
        interface: PacketInterfaceId,
        /// Authoritative existing queue.
        queue: InterfaceQueueId,
    },
}

/// Failure to derive node-core's compact eligible-interface snapshot from the
/// authoritative registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EligibleInterfaceSetError {
    /// A registered identity cannot fit node-core's current 0-through-63
    /// compact route profile.
    InterfaceOutsideProfile(InterfaceDescriptor),
}

/// Failure to derive the exact egress set for an ingress-derived announce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnounceEgressSetError {
    /// The supplied ingress interface is not registered.
    SourceUnavailable(PacketInterfaceId),
    /// A registered identity cannot fit the compact route profile.
    InterfaceOutsideProfile(InterfaceDescriptor),
}

/// Fixed-capacity authoritative packet-interface registry.
///
/// Lookup is a bounded linear scan. The intended embedded profile has only a
/// handful of simultaneously enabled interfaces, so this keeps storage and
/// failure behavior explicit without allocating a map.
pub struct InterfaceRegistry<const SLOTS: usize> {
    slots: [RegistrySlot; SLOTS],
}

impl<const SLOTS: usize> InterfaceRegistry<SLOTS> {
    /// Construct an empty registry.
    pub const fn new() -> Self {
        const {
            assert!(SLOTS > 0, "interface registry must have at least one slot");
            assert!(
                SLOTS <= (u16::MAX as usize) + 1,
                "interface registry slots must fit InterfaceQueueId"
            );
        }
        Self {
            slots: [const { RegistrySlot::vacant() }; SLOTS],
        }
    }

    /// Fixed number of actor queue slots.
    pub const fn capacity(&self) -> usize {
        SLOTS
    }

    /// Register one stable interface identity in a vacant queue.
    pub fn register(
        &mut self,
        queue: InterfaceQueueId,
        interface: PacketInterfaceId,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceRegistrationError> {
        let index = queue.index();
        let Some(slot) = self.slots.get(index).copied() else {
            return Err(InterfaceRegistrationError::QueueOutsideRegistry(queue));
        };
        if let Some(descriptor) = slot.descriptor {
            return Err(InterfaceRegistrationError::QueueOccupied(descriptor));
        }
        if let Some(existing) = self.descriptor(interface) {
            return Err(InterfaceRegistrationError::DuplicateInterface {
                interface,
                queue: existing.lease.queue,
            });
        }
        let generation = slot
            .last_generation
            .checked_add(1)
            .ok_or(InterfaceRegistrationError::GenerationExhausted(queue))?;
        let descriptor = InterfaceDescriptor {
            lease: InterfaceLease {
                interface,
                queue,
                generation: InterfaceGeneration(generation),
            },
            online,
            properties,
        };
        self.slots[index] = RegistrySlot {
            last_generation: generation,
            descriptor: Some(descriptor),
        };
        Ok(descriptor)
    }

    /// Current descriptor for one stable Reticulum interface identity.
    pub fn descriptor(&self, interface: PacketInterfaceId) -> Option<InterfaceDescriptor> {
        self.slots
            .iter()
            .filter_map(|slot| slot.descriptor)
            .find(|descriptor| descriptor.lease.interface == interface)
    }

    /// Current descriptor assigned to one fixed queue slot.
    pub fn descriptor_at(&self, queue: InterfaceQueueId) -> Option<InterfaceDescriptor> {
        self.slots
            .get(queue.index())
            .and_then(|slot| slot.descriptor)
    }

    /// Derive the sole current online-interface snapshot supplied to
    /// node-core route resolution.
    ///
    /// Firmware must use this instead of maintaining a second enabled-ID
    /// list. Every registered identity is checked even when offline so a
    /// latent out-of-profile registration cannot become an unexpected route
    /// failure merely by transitioning online later.
    pub fn eligible_interfaces(&self) -> Result<InterfaceSet, EligibleInterfaceSetError> {
        let mut eligible = InterfaceSet::empty();
        for descriptor in self.slots.iter().filter_map(|slot| slot.descriptor) {
            let Some(with_interface) = eligible.with(descriptor.lease.interface) else {
                return Err(EligibleInterfaceSetError::InterfaceOutsideProfile(
                    descriptor,
                ));
            };
            if descriptor.online {
                eligible = with_interface;
            }
        }
        Ok(eligible)
    }

    /// Derive the exact currently-online egress set for an announce learned on
    /// `source`.
    ///
    /// Point-to-point topology excludes reflection to the source interface.
    /// The supported Reticulum mode matrix additionally blocks only
    /// boundary-to-internal propagation; internal-to-boundary and all Full
    /// interactions remain allowed.
    pub fn announce_egress_interfaces(
        &self,
        source: PacketInterfaceId,
    ) -> Result<InterfaceSet, AnnounceEgressSetError> {
        let Some(source_descriptor) = self.descriptor(source) else {
            return Err(AnnounceEgressSetError::SourceUnavailable(source));
        };
        let source_properties = source_descriptor.properties;
        let mut egress = InterfaceSet::empty();
        for descriptor in self.slots.iter().filter_map(|slot| slot.descriptor) {
            let interface = descriptor.lease.interface;
            let Some(with_interface) = egress.with(interface) else {
                return Err(AnnounceEgressSetError::InterfaceOutsideProfile(descriptor));
            };
            if !descriptor.online {
                continue;
            }
            if interface == source && source_properties.topology == InterfaceTopology::PointToPoint
            {
                continue;
            }
            if !descriptor
                .properties
                .announce_mode
                .accepts_from(source_properties.announce_mode)
            {
                continue;
            }
            egress = with_interface;
        }
        Ok(egress)
    }

    /// Validate one lease against the current authoritative record.
    pub fn validate(
        &self,
        lease: InterfaceLease,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        let Some(slot) = self.slots.get(lease.queue.index()) else {
            return Err(InterfaceLeaseError::QueueOutsideRegistry(lease.queue));
        };
        let Some(current) = slot.descriptor else {
            return Err(InterfaceLeaseError::QueueVacant(lease.queue));
        };
        if current.lease != lease {
            return Err(InterfaceLeaseError::Stale {
                supplied: lease,
                current,
            });
        }
        Ok(current)
    }

    /// Change whether a current lease accepts new outbound jobs.
    ///
    /// This does not change the generation. Previously accepted owners can
    /// still complete under the same exact lease.
    pub fn set_online(
        &mut self,
        lease: InterfaceLease,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        let mut descriptor = self.validate(lease)?;
        descriptor.online = online;
        self.slots[lease.queue.index()].descriptor = Some(descriptor);
        Ok(descriptor)
    }

    /// Atomically replace logical MTU and opaque actor configuration.
    ///
    /// Reconfiguration advances the queue generation. Jobs already accepted
    /// under the former generation remain uniquely owned, but their eventual
    /// completions are reported as stale for explicit recovery instead of
    /// being silently attributed to the new configuration.
    pub fn reconfigure(
        &mut self,
        lease: InterfaceLease,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        let current = self.validate(lease)?;
        let generation = lease
            .generation
            .0
            .checked_add(1)
            .ok_or(InterfaceLeaseError::GenerationExhausted(lease.queue))?;
        let descriptor = InterfaceDescriptor {
            lease: InterfaceLease {
                generation: InterfaceGeneration(generation),
                ..current.lease
            },
            online,
            properties,
        };
        self.slots[lease.queue.index()] = RegistrySlot {
            last_generation: generation,
            descriptor: Some(descriptor),
        };
        Ok(descriptor)
    }

    /// Remove a current interface registration while preserving its consumed
    /// generation so a later registration cannot resurrect an old lease.
    pub fn unregister(
        &mut self,
        lease: InterfaceLease,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        let descriptor = self.validate(lease)?;
        self.slots[lease.queue.index()].descriptor = None;
        Ok(descriptor)
    }
}

impl<const SLOTS: usize> Default for InterfaceRegistry<SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}

/// Immutable registry snapshot stamped onto one accepted outbound owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceDispatchContext {
    descriptor: InterfaceDescriptor,
}

impl InterfaceDispatchContext {
    /// Generation-safe interface authority used for this admission.
    pub const fn lease(self) -> InterfaceLease {
        self.descriptor.lease
    }

    /// Logical MTU validated at admission.
    pub const fn logical_mtu(self) -> LogicalMtu {
        self.descriptor.properties.logical_mtu
    }

    /// Opaque actor configuration identity selected at admission.
    pub const fn config(self) -> InterfaceConfigId {
        self.descriptor.properties.config
    }

    /// Advertised bitrate stamped at admission, if present.
    pub const fn advertised_bitrate(self) -> Option<AdvertisedBitrate> {
        self.descriptor.properties.advertised_bitrate
    }

    /// Relative interface cost stamped at admission.
    pub const fn cost(self) -> InterfaceCost {
        self.descriptor.properties.cost
    }
}

/// Generation-safe ingress provenance issued only through one fixed actor
/// queue capability.
///
/// Concrete actors retain this scalar beside their RX owner. They never ask a
/// local client or packet payload which Reticulum interface ID to report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceIngressAuthority {
    descriptor: InterfaceDescriptor,
}

impl InterfaceIngressAuthority {
    /// Generation-safe actor lease represented by this authority.
    pub const fn lease(self) -> InterfaceLease {
        self.descriptor.lease
    }
}

/// Optional transport-neutral physical-link values for one ingress packet.
///
/// Radio actors can supply whole-dB RSSI and SNR. Stream and other interfaces
/// leave this observation absent rather than fabricating radio measurements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

/// Internal queue envelope pairing one sealed fabric buffer with actor
/// provenance.
///
/// Construction stays inside the actor handoff so production callers cannot
/// bypass the observed queue with an arbitrary packet owner.
struct InterfaceIngress {
    authority: InterfaceIngressAuthority,
    packet: SealedIngressPacket,
}

impl InterfaceIngress {
    const fn lease(&self) -> InterfaceLease {
        self.authority.descriptor.lease
    }
}

/// Ingress packet whose actor lease matches the authoritative registry.
#[must_use = "validated ingress must be handed to the sole node owner"]
pub struct ValidatedIngress {
    descriptor: InterfaceDescriptor,
    packet: SealedIngressPacket,
}

impl ValidatedIngress {
    /// Stable Reticulum interface provenance safe to pass to node-core.
    pub const fn interface(&self) -> PacketInterfaceId {
        self.descriptor.lease.interface
    }

    /// Complete authoritative interface descriptor for diagnostics/policy.
    pub const fn descriptor(&self) -> InterfaceDescriptor {
        self.descriptor
    }

    /// Optional physical-link signal values supplied by the interface actor.
    pub const fn signal(&self) -> Option<IngressSignalObservation> {
        self.packet.signal()
    }

    /// Recover the unchanged completed native packet owner.
    pub fn into_packet(self) -> SealedIngressPacket {
        self.packet
    }
}

/// Stable identity of one reusable native-packet ingress buffer.
///
/// The queue component records the buffer's permanent actor origin. The slot
/// component is unique only within that queue.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IngressBufferId {
    queue: InterfaceQueueId,
    slot: usize,
}

impl IngressBufferId {
    /// Permanent interface queue to which this buffer must be recycled.
    pub const fn queue(self) -> InterfaceQueueId {
        self.queue
    }

    /// Fixed buffer slot within the queue-local ingress pool.
    pub const fn slot(self) -> usize {
        self.slot
    }
}

struct IngressBufferStorage {
    bytes: [u8; PACKET_CAPACITY],
}

impl IngressBufferStorage {
    const fn new() -> Self {
        Self {
            bytes: [0; PACKET_CAPACITY],
        }
    }
}

// Non-zero-sized so distinct static fabric instances have distinct addresses.
struct IngressFabricOrigin {
    _marker: u8,
}

impl IngressFabricOrigin {
    const fn new() -> Self {
        Self { _marker: 0 }
    }
}

/// Actor-owned mutable typestate for one reusable native RNS packet buffer.
///
/// This value is intentionally neither `Copy` nor `Clone`. Mutable packet
/// capacity is available only in this state. [`Self::seal`] consumes it and
/// produces a [`SealedIngressPacket`] whose initialized bytes are immutable.
#[must_use = "an available ingress buffer must be filled, retained, or recycled"]
pub struct AvailableIngressBuffer {
    id: IngressBufferId,
    origin: &'static IngressFabricOrigin,
    storage: &'static mut IngressBufferStorage,
}

impl AvailableIngressBuffer {
    fn new(
        queue: InterfaceQueueId,
        slot: usize,
        origin: &'static IngressFabricOrigin,
        storage: &'static mut IngressBufferStorage,
    ) -> Self {
        Self {
            id: IngressBufferId { queue, slot },
            origin,
            storage,
        }
    }

    /// Stable queue-local identity retained across every reuse cycle.
    pub const fn id(&self) -> IngressBufferId {
        self.id
    }

    /// Borrow the complete mutable native-packet capacity.
    ///
    /// Callers must initialize exactly the prefix later supplied to
    /// [`Self::seal`]. Bytes beyond that prefix remain private and are never
    /// exposed by the sealed typestate.
    pub fn capacity_mut(&mut self) -> &mut [u8] {
        &mut self.storage.bytes
    }

    /// Fixed native RNS packet capacity in bytes.
    pub const fn capacity(&self) -> usize {
        PACKET_CAPACITY
    }

    /// Seal a nonempty initialized prefix for immutable node ingestion.
    pub fn seal(self, packet_len: usize) -> Result<SealedIngressPacket, IngressPacketSealFailure> {
        self.seal_with_signal(packet_len, None)
    }

    /// Seal a nonempty initialized prefix with optional physical-link signal
    /// values observed by this interface actor.
    pub fn seal_with_signal(
        self,
        packet_len: usize,
        signal: Option<IngressSignalObservation>,
    ) -> Result<SealedIngressPacket, IngressPacketSealFailure> {
        if packet_len == 0 {
            return Err(IngressPacketSealFailure {
                reason: IngressPacketLengthError::Zero,
                buffer: self,
            });
        }
        if packet_len > PACKET_CAPACITY {
            return Err(IngressPacketSealFailure {
                reason: IngressPacketLengthError::TooLong {
                    actual: packet_len,
                    maximum: PACKET_CAPACITY,
                },
                buffer: self,
            });
        }
        Ok(SealedIngressPacket {
            id: self.id,
            origin: self.origin,
            storage: self.storage,
            packet_len,
            signal,
        })
    }
}

/// Invalid native ingress packet length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressPacketLengthError {
    /// Native Reticulum packets cannot be empty.
    Zero,
    /// Supplied packet length exceeds node-core's complete-packet capacity.
    TooLong {
        /// Supplied byte length.
        actual: usize,
        /// Maximum complete native-packet length.
        maximum: usize,
    },
}

/// Failed available-to-sealed transition retaining the exact mutable owner.
#[must_use = "a seal failure retains the exact reusable ingress buffer"]
pub struct IngressPacketSealFailure {
    reason: IngressPacketLengthError,
    buffer: AvailableIngressBuffer,
}

impl fmt::Debug for IngressPacketSealFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngressPacketSealFailure")
            .field("reason", &self.reason)
            .field("buffer", &self.buffer.id)
            .finish()
    }
}

impl IngressPacketSealFailure {
    /// Length-validation reason the typestate transition failed.
    pub const fn reason(&self) -> IngressPacketLengthError {
        self.reason
    }

    /// Recover the unchanged mutable ingress-buffer owner.
    pub fn into_buffer(self) -> AvailableIngressBuffer {
        self.buffer
    }
}

/// Actor-completed native RNS packet with immutable initialized bytes.
///
/// This value remains the unique owner of its reusable backing storage until
/// the sole router returns it to the actor-side available-buffer channel.
#[must_use = "a sealed ingress packet must be submitted, retained, or recycled"]
pub struct SealedIngressPacket {
    id: IngressBufferId,
    origin: &'static IngressFabricOrigin,
    storage: &'static mut IngressBufferStorage,
    packet_len: usize,
    signal: Option<IngressSignalObservation>,
}

impl SealedIngressPacket {
    /// Stable queue-local identity of the exact reusable backing buffer.
    pub const fn id(&self) -> IngressBufferId {
        self.id
    }

    /// Validated nonzero native packet length.
    pub const fn len(&self) -> usize {
        self.packet_len
    }

    /// Whether the initialized packet prefix is empty.
    ///
    /// A successfully sealed packet is never empty.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Borrow exactly the initialized immutable native packet bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.storage.bytes[..self.packet_len]
    }

    /// Optional physical-link signal values supplied by the interface actor.
    pub const fn signal(&self) -> Option<IngressSignalObservation> {
        self.signal
    }

    fn recycle(self) -> AvailableIngressBuffer {
        AvailableIngressBuffer {
            id: self.id,
            origin: self.origin,
            storage: self.storage,
        }
    }
}

impl AsRef<[u8]> for SealedIngressPacket {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DataOwnerBinding {
    prepared: PreparedPacket,
    interface: PacketInterfaceId,
}

impl DataOwnerBinding {
    fn from_job(job: &RoutedTxJob<'_>) -> Self {
        Self {
            prepared: job.prepared(),
            interface: job.interface(),
        }
    }

    fn from_completion(completion: &TxCompletion<'_>) -> Self {
        Self {
            prepared: completion.prepared(),
            interface: completion.interface(),
        }
    }
}

/// One DATA job delivered only to its selected interface actor.
#[must_use = "an interface DATA owner must be processed or explicitly retained"]
pub struct InterfaceDataJob {
    context: InterfaceDispatchContext,
    binding: DataOwnerBinding,
    job: RoutedTxJob<'static>,
}

impl InterfaceDataJob {
    /// Registry/configuration snapshot under which this owner was accepted.
    pub const fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Consume the wrapper into the exact node-core job and its one-shot
    /// completion ticket.
    pub fn into_parts(self) -> (DataCompletionTicket, RoutedTxJob<'static>) {
        (
            DataCompletionTicket {
                context: self.context,
                binding: self.binding,
            },
            self.job,
        )
    }
}

/// One-shot capability that binds a DATA completion to the exact dispatched
/// owner and interface lease.
#[must_use = "a DATA completion ticket must return with its exact owner"]
pub struct DataCompletionTicket {
    context: InterfaceDispatchContext,
    binding: DataOwnerBinding,
}

impl DataCompletionTicket {
    /// Registry/configuration snapshot bound to this ticket.
    pub const fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Bind the exact owning node-core completion for return to the router.
    // The unchanged non-Copy owner must remain inline on mismatch; heap boxing
    // is not available on this portable ownership boundary.
    #[allow(clippy::result_large_err)]
    pub fn complete(
        self,
        completion: TxCompletion<'static>,
    ) -> Result<InterfaceTxCompletion, DataCompletionMismatch> {
        if DataOwnerBinding::from_completion(&completion) != self.binding {
            return Err(DataCompletionMismatch {
                ticket: self,
                completion,
            });
        }
        Ok(InterfaceTxCompletion {
            context: self.context,
            owner: InterfaceCompletionOwner::Data(completion),
        })
    }
}

/// Crossed DATA completion retaining both exact values unchanged.
#[must_use = "a crossed DATA completion and its ticket remain uniquely owned"]
pub struct DataCompletionMismatch {
    ticket: DataCompletionTicket,
    completion: TxCompletion<'static>,
}

impl fmt::Debug for DataCompletionMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataCompletionMismatch")
            .field("expected", &self.ticket.binding)
            .field(
                "supplied",
                &DataOwnerBinding::from_completion(&self.completion),
            )
            .finish()
    }
}

impl DataCompletionMismatch {
    /// Recover the unchanged ticket and owning completion for fail-closed
    /// handling.
    pub fn into_parts(self) -> (DataCompletionTicket, TxCompletion<'static>) {
        (self.ticket, self.completion)
    }
}

/// One ordinary protocol-action job delivered only to its selected interface
/// actor.
#[must_use = "an ordinary interface owner must be processed or explicitly retained"]
pub struct InterfaceOrdinaryJob {
    context: InterfaceDispatchContext,
    binding: OrdinaryPreparedPacket,
    job: OrdinaryTxJob<'static>,
}

impl InterfaceOrdinaryJob {
    /// Registry/configuration snapshot under which this owner was accepted.
    pub const fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Consume the wrapper into the exact ordinary job and its one-shot
    /// completion ticket.
    pub fn into_parts(self) -> (OrdinaryCompletionTicket, OrdinaryTxJob<'static>) {
        (
            OrdinaryCompletionTicket {
                context: self.context,
                binding: self.binding,
            },
            self.job,
        )
    }
}

/// One-shot capability binding an ordinary completion to its exact dispatched
/// owner and interface lease.
#[must_use = "an ordinary completion ticket must return with its exact owner"]
pub struct OrdinaryCompletionTicket {
    context: InterfaceDispatchContext,
    binding: OrdinaryPreparedPacket,
}

impl OrdinaryCompletionTicket {
    /// Registry/configuration snapshot bound to this ticket.
    pub const fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Bind the exact ordinary owning completion for return to the router.
    // The unchanged non-Copy owner must remain inline on mismatch; heap boxing
    // is not available on this portable ownership boundary.
    #[allow(clippy::result_large_err)]
    pub fn complete(
        self,
        completion: OrdinaryTxCompletion<'static>,
    ) -> Result<InterfaceTxCompletion, OrdinaryCompletionMismatch> {
        if completion.prepared() != self.binding {
            return Err(OrdinaryCompletionMismatch {
                ticket: self,
                completion,
            });
        }
        Ok(InterfaceTxCompletion {
            context: self.context,
            owner: InterfaceCompletionOwner::Ordinary(completion),
        })
    }
}

/// Crossed ordinary completion retaining both exact values unchanged.
#[must_use = "a crossed ordinary completion and its ticket remain uniquely owned"]
pub struct OrdinaryCompletionMismatch {
    ticket: OrdinaryCompletionTicket,
    completion: OrdinaryTxCompletion<'static>,
}

impl fmt::Debug for OrdinaryCompletionMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrdinaryCompletionMismatch")
            .field("expected", &self.ticket.binding)
            .field("supplied", &self.completion.prepared())
            .finish()
    }
}

impl OrdinaryCompletionMismatch {
    /// Recover the unchanged ticket and owning completion for fail-closed
    /// handling.
    pub fn into_parts(self) -> (OrdinaryCompletionTicket, OrdinaryTxCompletion<'static>) {
        (self.ticket, self.completion)
    }
}

/// Exact outbound owner accepted by one per-interface queue.
#[must_use = "an interface TX owner must remain uniquely owned"]
pub enum InterfaceTxJob {
    /// Destination-DATA owner with a receipt/attempt ledger entry.
    Data(InterfaceDataJob),
    /// Ordinary announce, proof, forwarding, Link, or Resource packet owner.
    Ordinary(InterfaceOrdinaryJob),
}

enum InterfaceCompletionOwner {
    Data(TxCompletion<'static>),
    Ordinary(OrdinaryTxCompletion<'static>),
}

/// Owning completion returned by one exact interface actor.
///
/// Its owner and context are private so only a matching one-shot DATA or
/// ordinary completion ticket can construct this envelope.
#[must_use = "an interface completion must return to the outbound router"]
pub struct InterfaceTxCompletion {
    context: InterfaceDispatchContext,
    owner: InterfaceCompletionOwner,
}

impl InterfaceTxCompletion {
    fn context(&self) -> InterfaceDispatchContext {
        self.context
    }

    /// Recover the exact node-owner completion while discarding only the
    /// already-inspected interface routing envelope.
    pub fn into_outbound(self) -> OutboundCompletion {
        match self.owner {
            InterfaceCompletionOwner::Data(completion) => OutboundCompletion::Data(completion),
            InterfaceCompletionOwner::Ordinary(completion) => {
                OutboundCompletion::Ordinary(completion)
            }
        }
    }
}

/// Exact completion ready for its node-core DATA or ordinary owner.
#[must_use = "an outbound completion must be reconciled by its node owner"]
pub enum OutboundCompletion {
    /// Destination-DATA owning completion.
    Data(TxCompletion<'static>),
    /// Ordinary protocol-action owning completion.
    Ordinary(OrdinaryTxCompletion<'static>),
}

impl fmt::Debug for OutboundCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(completion) => formatter
                .debug_tuple("Data")
                .field(&completion.prepared())
                .field(&completion.interface())
                .finish(),
            Self::Ordinary(completion) => formatter
                .debug_tuple("Ordinary")
                .field(&completion.prepared())
                .finish(),
        }
    }
}

struct InterfaceChannels<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex,
{
    jobs: Channel<M, InterfaceTxJob, QUEUE_DEPTH>,
    completions: Channel<M, InterfaceTxCompletion, QUEUE_DEPTH>,
    available_ingress: Channel<M, AvailableIngressBuffer, QUEUE_DEPTH>,
    completed_ingress: Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    lifecycle_requests: Channel<M, InterfaceLifecycleRequest, 1>,
    lifecycle_acknowledgements: Channel<M, InterfaceLifecycleAcknowledgement, 1>,
}

impl<M, const QUEUE_DEPTH: usize> InterfaceChannels<M, QUEUE_DEPTH>
where
    M: RawMutex,
{
    const fn new() -> Self {
        Self {
            jobs: Channel::new(),
            completions: Channel::new(),
            available_ingress: Channel::new(),
            completed_ingress: Channel::new(),
            lifecycle_requests: Channel::new(),
            lifecycle_acknowledgements: Channel::new(),
        }
    }
}

struct RouterQueue<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    jobs: &'static Channel<M, InterfaceTxJob, QUEUE_DEPTH>,
    completions: &'static Channel<M, InterfaceTxCompletion, QUEUE_DEPTH>,
    available_ingress: &'static Channel<M, AvailableIngressBuffer, QUEUE_DEPTH>,
    completed_ingress: &'static Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    lifecycle_requests: &'static Channel<M, InterfaceLifecycleRequest, 1>,
    lifecycle_acknowledgements: &'static Channel<M, InterfaceLifecycleAcknowledgement, 1>,
    ingress_origin: &'static IngressFabricOrigin,
}

/// Fixed storage for a registry and one bounded job/completion pair per actor.
///
/// Allocate this in static storage and call [`Self::split`] once. The returned
/// actor handles are non-`Clone` capabilities permanently bound to distinct
/// queue slots.
pub struct InterfaceFabric<M, const SLOTS: usize, const QUEUE_DEPTH: usize>
where
    M: RawMutex,
{
    registry: InterfaceRegistry<SLOTS>,
    channels: [InterfaceChannels<M, QUEUE_DEPTH>; SLOTS],
    ingress_origin: IngressFabricOrigin,
    ingress_buffers: [[IngressBufferStorage; QUEUE_DEPTH]; SLOTS],
}

impl<M, const SLOTS: usize, const QUEUE_DEPTH: usize> InterfaceFabric<M, SLOTS, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Construct empty static interface-fabric storage.
    pub const fn new() -> Self {
        const {
            assert!(QUEUE_DEPTH > 0, "interface queues must be nonempty");
        }
        Self {
            registry: InterfaceRegistry::new(),
            channels: [const { InterfaceChannels::new() }; SLOTS],
            ingress_origin: IngressFabricOrigin::new(),
            ingress_buffers: [const { [const { IngressBufferStorage::new() }; QUEUE_DEPTH] };
                SLOTS],
        }
    }

    /// Split the fabric into its sole router and one actor capability per
    /// fixed queue slot.
    pub fn split(
        &'static mut self,
    ) -> (
        OutboundRouter<M, SLOTS, QUEUE_DEPTH>,
        [InterfaceActorHandoff<M, QUEUE_DEPTH>; SLOTS],
    ) {
        let registry = mem::take(&mut self.registry);
        let channels: &'static [InterfaceChannels<M, QUEUE_DEPTH>; SLOTS] = &self.channels;
        let ingress_origin: &'static IngressFabricOrigin = &self.ingress_origin;
        let ingress_buffers: &'static mut [[IngressBufferStorage; QUEUE_DEPTH]; SLOTS] =
            &mut self.ingress_buffers;
        for (queue_index, buffers) in ingress_buffers.iter_mut().enumerate() {
            let queue = InterfaceQueueId(queue_index as u16);
            for (buffer_index, buffer) in buffers.iter_mut().enumerate() {
                let available =
                    AvailableIngressBuffer::new(queue, buffer_index, ingress_origin, buffer);
                match channels[queue_index].available_ingress.try_send(available) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        unreachable!("fresh ingress pool must fit its empty channel")
                    }
                }
            }
        }
        let queues = array::from_fn(|index| RouterQueue {
            jobs: &channels[index].jobs,
            completions: &channels[index].completions,
            available_ingress: &channels[index].available_ingress,
            completed_ingress: &channels[index].completed_ingress,
            lifecycle_requests: &channels[index].lifecycle_requests,
            lifecycle_acknowledgements: &channels[index].lifecycle_acknowledgements,
            ingress_origin,
        });
        let actors = array::from_fn(|index| InterfaceActorHandoff {
            queue: InterfaceQueueId(index as u16),
            jobs: &channels[index].jobs,
            completions: &channels[index].completions,
            available_ingress: &channels[index].available_ingress,
            completed_ingress: &channels[index].completed_ingress,
            lifecycle_requests: &channels[index].lifecycle_requests,
            lifecycle_acknowledgements: &channels[index].lifecycle_acknowledgements,
            ingress_origin,
        });
        (
            OutboundRouter {
                registry,
                queues,
                completion_cursor: 0,
                ingress_cursor: 0,
                lifecycle_cursor: 0,
            },
            actors,
        )
    }
}

impl<M, const SLOTS: usize, const QUEUE_DEPTH: usize> Default
    for InterfaceFabric<M, SLOTS, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Immediate actor-side rejection while enqueueing one lifecycle request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceLifecycleTryRequestError {
    /// The supplied lease belongs to another fixed actor queue.
    ForeignQueue {
        /// Queue permanently owned by this actor capability.
        actor: InterfaceQueueId,
        /// Queue named by the supplied lease.
        supplied: InterfaceQueueId,
    },
    /// Another exact lifecycle exchange is awaiting acknowledgement.
    ExchangePending {
        /// Exact earlier request still awaiting acknowledgement.
        pending: InterfaceLifecycleRequest,
        /// Exact request that was not enqueued.
        unsent: InterfaceLifecycleRequest,
    },
    /// The request queue is occupied; the payload is the exact unsent request.
    RequestQueueFull(InterfaceLifecycleRequest),
}

/// Actor-side lifecycle exchange status or fail-closed correlation error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceLifecycleActorError {
    /// The supplied lease belongs to another fixed actor queue.
    ForeignQueue {
        /// Queue permanently owned by this actor capability.
        actor: InterfaceQueueId,
        /// Queue named by the supplied lease.
        supplied: InterfaceQueueId,
    },
    /// Another exact lifecycle request still awaits acknowledgement.
    ExchangePending(InterfaceLifecycleRequest),
    /// The bounded request queue was unexpectedly occupied while no exchange
    /// was locally pending.
    RequestQueueFull(InterfaceLifecycleRequest),
    /// No locally pending request exists to finish.
    NoPendingRequest,
    /// The supervisor rejected the exact request without changing registry
    /// eligibility.
    Rejected(InterfaceLifecycleRouteError),
    /// The acknowledgement did not name the request this call emitted.
    CrossedAcknowledgement {
        /// Exact request emitted by this call.
        expected: InterfaceLifecycleRequest,
        /// Different request named by the received acknowledgement.
        received: InterfaceLifecycleRequest,
    },
    /// An acknowledgement arrived while this actor had no pending exchange.
    UnexpectedAcknowledgement(InterfaceLifecycleRequest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InterfaceLifecycleActorPhase {
    Idle,
    Awaiting(InterfaceLifecycleRequest),
}

/// Split concrete-actor capability for generation-bound Ready/Offline
/// reporting.
#[must_use = "a permanent actor must retain its lifecycle capability"]
pub struct InterfaceLifecycleActorHandoff<M>
where
    M: RawMutex + 'static,
{
    queue: InterfaceQueueId,
    requests: &'static Channel<M, InterfaceLifecycleRequest, 1>,
    acknowledgements: &'static Channel<M, InterfaceLifecycleAcknowledgement, 1>,
    phase: InterfaceLifecycleActorPhase,
}

impl<M> InterfaceLifecycleActorHandoff<M>
where
    M: RawMutex + 'static,
{
    /// Fixed queue identity owned by this actor capability.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.queue
    }

    /// Immediately enqueue one generation-bound lifecycle request.
    pub fn try_request_state(
        &mut self,
        lease: InterfaceLease,
        state: InterfaceLifecycleState,
    ) -> Result<InterfaceLifecycleRequest, InterfaceLifecycleTryRequestError> {
        if lease.queue != self.queue {
            return Err(InterfaceLifecycleTryRequestError::ForeignQueue {
                actor: self.queue,
                supplied: lease.queue,
            });
        }
        let request = InterfaceLifecycleRequest { lease, state };
        if let InterfaceLifecycleActorPhase::Awaiting(pending) = self.phase {
            return Err(InterfaceLifecycleTryRequestError::ExchangePending {
                pending,
                unsent: request,
            });
        }
        match self.requests.try_send(request) {
            Ok(()) => {
                self.phase = InterfaceLifecycleActorPhase::Awaiting(request);
                Ok(request)
            }
            Err(TrySendError::Full(request)) => {
                Err(InterfaceLifecycleTryRequestError::RequestQueueFull(request))
            }
        }
    }

    /// Finish the locally pending exchange immediately.
    ///
    /// `Ok(None)` means an exchange is pending but its acknowledgement is not
    /// yet available. Idle actors receive [`InterfaceLifecycleActorError::NoPendingRequest`]
    /// instead of an ambiguous empty result.
    pub fn try_finish_request(
        &mut self,
    ) -> Result<Option<InterfaceDescriptor>, InterfaceLifecycleActorError> {
        if matches!(self.phase, InterfaceLifecycleActorPhase::Idle) {
            return match self.acknowledgements.try_receive() {
                Ok(acknowledgement) => {
                    Err(InterfaceLifecycleActorError::UnexpectedAcknowledgement(
                        acknowledgement.request,
                    ))
                }
                Err(_) => Err(InterfaceLifecycleActorError::NoPendingRequest),
            };
        }
        let Ok(acknowledgement) = self.acknowledgements.try_receive() else {
            return Ok(None);
        };
        self.resolve_pending_acknowledgement(acknowledgement)
            .map(Some)
    }

    /// Request one state and wait for the exact supervisor acknowledgement.
    ///
    /// A permanent actor must drive this single-flight exchange to completion;
    /// cancelling after request delivery deliberately leaves the exact
    /// pending exchange represented by this capability, with its request or
    /// acknowledgement retained in the bounded channels rather than silently
    /// accepting a later crossed transition.
    pub async fn request_state(
        &mut self,
        lease: InterfaceLease,
        state: InterfaceLifecycleState,
    ) -> Result<InterfaceDescriptor, InterfaceLifecycleActorError> {
        self.try_request_state(lease, state)
            .map_err(|reason| match reason {
                InterfaceLifecycleTryRequestError::ForeignQueue { actor, supplied } => {
                    InterfaceLifecycleActorError::ForeignQueue { actor, supplied }
                }
                InterfaceLifecycleTryRequestError::ExchangePending { pending, .. } => {
                    InterfaceLifecycleActorError::ExchangePending(pending)
                }
                InterfaceLifecycleTryRequestError::RequestQueueFull(request) => {
                    InterfaceLifecycleActorError::RequestQueueFull(request)
                }
            })?;
        self.finish_pending_request().await
    }

    /// Resume and finish one request retained in the actor phase after a
    /// cancelled acknowledgement wait.
    pub async fn finish_pending_request(
        &mut self,
    ) -> Result<InterfaceDescriptor, InterfaceLifecycleActorError> {
        let InterfaceLifecycleActorPhase::Awaiting(expected) = self.phase else {
            return Err(InterfaceLifecycleActorError::NoPendingRequest);
        };
        let acknowledgement = self.acknowledgements.receive().await;
        debug_assert_eq!(self.phase, InterfaceLifecycleActorPhase::Awaiting(expected));
        self.resolve_pending_acknowledgement(acknowledgement)
    }

    fn resolve_pending_acknowledgement(
        &mut self,
        acknowledgement: InterfaceLifecycleAcknowledgement,
    ) -> Result<InterfaceDescriptor, InterfaceLifecycleActorError> {
        let InterfaceLifecycleActorPhase::Awaiting(expected) = self.phase else {
            return Err(InterfaceLifecycleActorError::UnexpectedAcknowledgement(
                acknowledgement.request,
            ));
        };
        if acknowledgement.request != expected {
            return Err(InterfaceLifecycleActorError::CrossedAcknowledgement {
                expected,
                received: acknowledgement.request,
            });
        }
        self.phase = InterfaceLifecycleActorPhase::Idle;
        acknowledgement
            .result
            .map_err(InterfaceLifecycleActorError::Rejected)
    }
}

/// Sole concrete-actor capability for one fixed interface queue.
#[must_use = "dropping an actor handoff abandons its interface queue capability"]
pub struct InterfaceActorHandoff<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    queue: InterfaceQueueId,
    jobs: &'static Channel<M, InterfaceTxJob, QUEUE_DEPTH>,
    completions: &'static Channel<M, InterfaceTxCompletion, QUEUE_DEPTH>,
    available_ingress: &'static Channel<M, AvailableIngressBuffer, QUEUE_DEPTH>,
    completed_ingress: &'static Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    lifecycle_requests: &'static Channel<M, InterfaceLifecycleRequest, 1>,
    lifecycle_acknowledgements: &'static Channel<M, InterfaceLifecycleAcknowledgement, 1>,
    ingress_origin: &'static IngressFabricOrigin,
}

impl<M, const QUEUE_DEPTH: usize> InterfaceActorHandoff<M, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Fixed queue identity owned by this actor capability.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.queue
    }

    /// Consume the combined handle into independent TX, RX, and lifecycle
    /// actor capabilities bound to the same fixed queue.
    ///
    /// A permanent concrete interface task owns the RX and lifecycle values
    /// while it may pass the TX capability to a dedicated dispatcher.
    pub fn into_parts(
        self,
    ) -> (
        InterfaceTxActorHandoff<M, QUEUE_DEPTH>,
        InterfaceIngressActorHandoff<M, QUEUE_DEPTH>,
        InterfaceLifecycleActorHandoff<M>,
    ) {
        (
            InterfaceTxActorHandoff {
                queue: self.queue,
                jobs: self.jobs,
                completions: self.completions,
            },
            InterfaceIngressActorHandoff {
                queue: self.queue,
                available: self.available_ingress,
                completed: self.completed_ingress,
                origin: self.ingress_origin,
            },
            InterfaceLifecycleActorHandoff {
                queue: self.queue,
                requests: self.lifecycle_requests,
                acknowledgements: self.lifecycle_acknowledgements,
                phase: InterfaceLifecycleActorPhase::Idle,
            },
        )
    }

    /// Bind a registry-issued descriptor into this fixed actor's RX
    /// provenance capability.
    ///
    /// The queue check prevents an actor from reporting another actor's
    /// interface identity. The outbound router still validates the generation
    /// for every completed ingress envelope.
    pub fn bind_ingress(
        &self,
        descriptor: InterfaceDescriptor,
    ) -> Result<InterfaceIngressAuthority, ActorIngressBindingError> {
        if descriptor.lease.queue != self.queue {
            return Err(ActorIngressBindingError::ForeignQueue {
                expected: self.queue,
                supplied: descriptor.lease.queue,
            });
        }
        Ok(InterfaceIngressAuthority { descriptor })
    }

    /// Receive one reusable mutable native-packet buffer immediately.
    pub fn try_receive_ingress_buffer(&mut self) -> Option<AvailableIngressBuffer> {
        self.available_ingress.try_receive().ok()
    }

    /// Poll for one reusable mutable native-packet buffer.
    ///
    /// A pending poll only registers the current waker. Cancelling the
    /// surrounding future leaves every exact buffer owner queued.
    pub fn poll_receive_ingress_buffer(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<AvailableIngressBuffer> {
        self.available_ingress.poll_receive(context)
    }

    /// Wait cancellation-safely for one reusable mutable packet buffer.
    pub async fn receive_ingress_buffer(&mut self) -> AvailableIngressBuffer {
        poll_fn(|context| self.poll_receive_ingress_buffer(context)).await
    }

    /// Submit one sealed native packet with actor-bound provenance.
    ///
    /// The actor's fixed queue is checked independently against both the
    /// registry authority and the reusable buffer's permanent origin. A
    /// crossed capability or bounded pressure returns the exact sealed owner.
    pub fn try_send_ingress(
        &mut self,
        authority: InterfaceIngressAuthority,
        packet: SealedIngressPacket,
    ) -> Result<(), ActorIngressSendFailure> {
        try_send_actor_ingress(
            self.queue,
            self.ingress_origin,
            self.completed_ingress,
            authority,
            packet,
        )
    }

    /// Poll for advisory completed-ingress queue capacity.
    ///
    /// Readiness reserves no slot and owns no packet. The actor must retain
    /// its sealed packet and retry [`Self::try_send_ingress`] after waking.
    pub fn poll_ingress_send_capacity(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.completed_ingress.poll_ready_to_send(context)
    }

    /// Wait cancellation-safely for advisory completed-ingress capacity.
    pub async fn wait_ingress_send_capacity(&mut self) {
        poll_fn(|context| self.poll_ingress_send_capacity(context)).await
    }

    /// Receive this actor's oldest exact owner immediately, if queued.
    pub fn try_receive_job(&mut self) -> Option<InterfaceTxJob> {
        self.jobs.try_receive().ok()
    }

    /// Poll for this actor's oldest exact owner.
    ///
    /// A pending poll only registers the current waker. Cancelling the
    /// surrounding future therefore does not reserve or remove a queued owner.
    pub fn poll_receive_job(&mut self, context: &mut Context<'_>) -> Poll<InterfaceTxJob> {
        self.jobs.poll_receive(context)
    }

    /// Wait for this actor's oldest exact owner.
    ///
    /// This operation is cancellation-safe: dropping it while pending leaves
    /// every queued owner available to the next receive operation.
    pub async fn receive_job(&mut self) -> InterfaceTxJob {
        poll_fn(|context| self.poll_receive_job(context)).await
    }

    /// Return one exact completion without awaiting.
    ///
    /// Crossed queue capabilities and pressure both return the unchanged
    /// completion envelope.
    // The exact completion owner must be returned inline under pressure.
    #[allow(clippy::result_large_err)]
    pub fn try_send_completion(
        &mut self,
        completion: InterfaceTxCompletion,
    ) -> Result<(), ActorCompletionSendFailure> {
        if completion.context().lease().queue() != self.queue {
            return Err(ActorCompletionSendFailure {
                reason: ActorCompletionSendError::ForeignQueue {
                    expected: self.queue,
                    supplied: completion.context().lease().queue(),
                },
                completion,
            });
        }
        self.completions
            .try_send(completion)
            .map_err(
                |TrySendError::Full(completion)| ActorCompletionSendFailure {
                    reason: ActorCompletionSendError::QueueFull(self.queue),
                    completion,
                },
            )
    }

    /// Poll for advisory capacity in this actor's completion queue.
    ///
    /// Readiness neither reserves capacity nor moves an exact completion.
    /// An actor must retain its completion in persistent state and retry
    /// [`Self::try_send_completion`] after this poll reports ready.
    pub fn poll_completion_capacity(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.completions.poll_ready_to_send(context)
    }

    /// Wait cancellation-safely for advisory completion-queue capacity.
    ///
    /// This future owns no completion. Dropping it while pending or after a
    /// wake therefore leaves the actor's separately retained completion
    /// unchanged and does not reserve queue capacity.
    pub async fn wait_completion_capacity(&mut self) {
        poll_fn(|context| self.poll_completion_capacity(context)).await
    }

    /// Configured bounded depth of this actor's job and completion queues.
    pub const fn capacity(&self) -> usize {
        QUEUE_DEPTH
    }

    /// Current number of exact jobs waiting for this actor.
    pub fn pending_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// Current number of reusable ingress buffers waiting for this actor.
    pub fn available_ingress_buffers(&self) -> usize {
        self.available_ingress.len()
    }

    /// Current number of completed ingress packets waiting for the router.
    pub fn pending_ingress_packets(&self) -> usize {
        self.completed_ingress.len()
    }
}

/// TX-only concrete-actor capability for one fixed interface queue.
///
/// This is the outbound capability returned by
/// [`InterfaceActorHandoff::into_parts`] so a dedicated dispatcher need not
/// own any RX-buffer or lifecycle capability.
#[must_use = "dropping a TX actor handoff abandons its outbound queue capability"]
pub struct InterfaceTxActorHandoff<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    queue: InterfaceQueueId,
    jobs: &'static Channel<M, InterfaceTxJob, QUEUE_DEPTH>,
    completions: &'static Channel<M, InterfaceTxCompletion, QUEUE_DEPTH>,
}

impl<M, const QUEUE_DEPTH: usize> InterfaceTxActorHandoff<M, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Fixed queue identity owned by this TX capability.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.queue
    }

    /// Receive this actor's oldest exact outbound owner immediately.
    pub fn try_receive_job(&mut self) -> Option<InterfaceTxJob> {
        self.jobs.try_receive().ok()
    }

    /// Poll for this actor's oldest exact outbound owner.
    pub fn poll_receive_job(&mut self, context: &mut Context<'_>) -> Poll<InterfaceTxJob> {
        self.jobs.poll_receive(context)
    }

    /// Wait cancellation-safely for this actor's oldest outbound owner.
    pub async fn receive_job(&mut self) -> InterfaceTxJob {
        poll_fn(|context| self.poll_receive_job(context)).await
    }

    /// Return one exact completion without awaiting.
    #[allow(clippy::result_large_err)]
    pub fn try_send_completion(
        &mut self,
        completion: InterfaceTxCompletion,
    ) -> Result<(), ActorCompletionSendFailure> {
        if completion.context().lease().queue() != self.queue {
            return Err(ActorCompletionSendFailure {
                reason: ActorCompletionSendError::ForeignQueue {
                    expected: self.queue,
                    supplied: completion.context().lease().queue(),
                },
                completion,
            });
        }
        self.completions
            .try_send(completion)
            .map_err(
                |TrySendError::Full(completion)| ActorCompletionSendFailure {
                    reason: ActorCompletionSendError::QueueFull(self.queue),
                    completion,
                },
            )
    }

    /// Poll for advisory completion-queue capacity without moving an owner.
    pub fn poll_completion_capacity(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.completions.poll_ready_to_send(context)
    }

    /// Wait cancellation-safely for advisory completion-queue capacity.
    pub async fn wait_completion_capacity(&mut self) {
        poll_fn(|context| self.poll_completion_capacity(context)).await
    }

    /// Configured bounded outbound queue depth.
    pub const fn capacity(&self) -> usize {
        QUEUE_DEPTH
    }

    /// Current number of exact jobs waiting for this actor.
    pub fn pending_jobs(&self) -> usize {
        self.jobs.len()
    }
}

/// RX-only concrete-actor capability for one fixed interface queue.
///
/// The available-buffer receiver and completed-packet sender share one fixed
/// queue origin and cannot be cloned or constructed independently.
#[must_use = "dropping an ingress actor handoff abandons its RX pool capability"]
pub struct InterfaceIngressActorHandoff<M, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    queue: InterfaceQueueId,
    available: &'static Channel<M, AvailableIngressBuffer, QUEUE_DEPTH>,
    completed: &'static Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    origin: &'static IngressFabricOrigin,
}

impl<M, const QUEUE_DEPTH: usize> InterfaceIngressActorHandoff<M, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Fixed queue identity owned by this RX capability.
    pub const fn queue_id(&self) -> InterfaceQueueId {
        self.queue
    }

    /// Bind a registry descriptor to this fixed actor's RX provenance.
    pub fn bind_ingress(
        &self,
        descriptor: InterfaceDescriptor,
    ) -> Result<InterfaceIngressAuthority, ActorIngressBindingError> {
        if descriptor.lease.queue != self.queue {
            return Err(ActorIngressBindingError::ForeignQueue {
                expected: self.queue,
                supplied: descriptor.lease.queue,
            });
        }
        Ok(InterfaceIngressAuthority { descriptor })
    }

    /// Receive one reusable mutable native-packet buffer immediately.
    pub fn try_receive_buffer(&mut self) -> Option<AvailableIngressBuffer> {
        self.available.try_receive().ok()
    }

    /// Poll for one reusable mutable native-packet buffer.
    pub fn poll_receive_buffer(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<AvailableIngressBuffer> {
        self.available.poll_receive(context)
    }

    /// Wait cancellation-safely for one reusable mutable packet buffer.
    pub async fn receive_buffer(&mut self) -> AvailableIngressBuffer {
        poll_fn(|context| self.poll_receive_buffer(context)).await
    }

    /// Submit one exact sealed native packet with actor-bound provenance.
    pub fn try_send(
        &mut self,
        authority: InterfaceIngressAuthority,
        packet: SealedIngressPacket,
    ) -> Result<(), ActorIngressSendFailure> {
        try_send_actor_ingress(self.queue, self.origin, self.completed, authority, packet)
    }

    /// Poll for advisory completed-packet queue capacity.
    pub fn poll_ready_to_send(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.completed.poll_ready_to_send(context)
    }

    /// Wait cancellation-safely for advisory completed-packet capacity.
    pub async fn wait_ready_to_send(&mut self) {
        poll_fn(|context| self.poll_ready_to_send(context)).await
    }

    /// Configured reusable-buffer pool and completed-packet queue depth.
    pub const fn capacity(&self) -> usize {
        QUEUE_DEPTH
    }

    /// Current number of reusable buffers waiting for this actor.
    pub fn available_buffers(&self) -> usize {
        self.available.len()
    }

    /// Current number of completed packets waiting for the router.
    pub fn pending_packets(&self) -> usize {
        self.completed.len()
    }
}

fn try_send_actor_ingress<M, const QUEUE_DEPTH: usize>(
    expected: InterfaceQueueId,
    expected_origin: &'static IngressFabricOrigin,
    completed: &'static Channel<M, InterfaceIngress, QUEUE_DEPTH>,
    authority: InterfaceIngressAuthority,
    packet: SealedIngressPacket,
) -> Result<(), ActorIngressSendFailure>
where
    M: RawMutex + 'static,
{
    let ingress = InterfaceIngress { authority, packet };
    let authority_queue = ingress.lease().queue();
    if authority_queue != expected {
        return Err(ActorIngressSendFailure {
            reason: ActorIngressSendError::ForeignAuthorityQueue {
                expected,
                supplied: authority_queue,
            },
            ingress,
        });
    }
    let buffer_queue = ingress.packet.id.queue;
    if buffer_queue != expected {
        return Err(ActorIngressSendFailure {
            reason: ActorIngressSendError::ForeignBufferQueue {
                expected,
                supplied: buffer_queue,
            },
            ingress,
        });
    }
    if !core::ptr::eq(ingress.packet.origin, expected_origin) {
        return Err(ActorIngressSendFailure {
            reason: ActorIngressSendError::ForeignFabricOrigin(expected),
            ingress,
        });
    }
    let maximum = ingress.authority.descriptor.logical_mtu();
    if ingress.packet.len() > usize::from(maximum.get()) {
        return Err(ActorIngressSendFailure {
            reason: ActorIngressSendError::PacketExceedsBoundMtu {
                actual: ingress.packet.len(),
                maximum,
            },
            ingress,
        });
    }
    completed
        .try_send(ingress)
        .map_err(|TrySendError::Full(ingress)| ActorIngressSendFailure {
            reason: ActorIngressSendError::QueueFull(expected),
            ingress,
        })
}

/// Failure to bind registry-issued ingress provenance to one concrete actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorIngressBindingError {
    /// Descriptor belongs to a different fixed actor queue.
    ForeignQueue {
        /// Queue permanently owned by this actor capability.
        expected: InterfaceQueueId,
        /// Queue named by the supplied registry descriptor.
        supplied: InterfaceQueueId,
    },
}

/// Why an actor could not submit one sealed native ingress packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorIngressSendError {
    /// Registry authority belongs to a different fixed actor queue.
    ForeignAuthorityQueue {
        /// Queue owned by the actor capability used for submission.
        expected: InterfaceQueueId,
        /// Queue stamped on the supplied registry authority.
        supplied: InterfaceQueueId,
    },
    /// Reusable packet buffer originated from a different fixed actor pool.
    ForeignBufferQueue {
        /// Queue owned by the actor capability used for submission.
        expected: InterfaceQueueId,
        /// Permanent origin stamped on the supplied buffer.
        supplied: InterfaceQueueId,
    },
    /// Packet buffer was seeded by a different static interface fabric.
    ForeignFabricOrigin(InterfaceQueueId),
    /// Sealed packet exceeds the descriptor MTU bound into this actor's
    /// ingress authority.
    PacketExceedsBoundMtu {
        /// Sealed native-packet length.
        actual: usize,
        /// Actor-bound logical MTU.
        maximum: LogicalMtu,
    },
    /// The bounded completed-packet queue is full.
    QueueFull(InterfaceQueueId),
}

/// Actor ingress-send failure retaining the exact sealed packet and authority.
#[must_use = "actor ingress failure retains an exact owning packet"]
pub struct ActorIngressSendFailure {
    reason: ActorIngressSendError,
    ingress: InterfaceIngress,
}

impl fmt::Debug for ActorIngressSendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActorIngressSendFailure")
            .field("reason", &self.reason)
            .field("buffer", &self.ingress.packet.id)
            .finish_non_exhaustive()
    }
}

impl ActorIngressSendFailure {
    /// Typed reason the exact packet was not enqueued.
    pub const fn reason(&self) -> ActorIngressSendError {
        self.reason
    }

    /// Recover the unchanged actor authority and exact sealed packet owner.
    pub fn into_parts(self) -> (InterfaceIngressAuthority, SealedIngressPacket) {
        (self.ingress.authority, self.ingress.packet)
    }
}

/// Why an actor could not return an exact completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorCompletionSendError {
    /// Completion ticket was issued for another fixed actor queue.
    ForeignQueue {
        /// Queue owned by the actor capability used for return.
        expected: InterfaceQueueId,
        /// Queue stamped on the completion ticket.
        supplied: InterfaceQueueId,
    },
    /// The bounded completion queue is full.
    QueueFull(InterfaceQueueId),
}

/// Actor completion-send failure retaining the unchanged exact owner.
#[must_use = "actor send failure retains an exact owning completion"]
pub struct ActorCompletionSendFailure {
    reason: ActorCompletionSendError,
    completion: InterfaceTxCompletion,
}

impl fmt::Debug for ActorCompletionSendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActorCompletionSendFailure")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl ActorCompletionSendFailure {
    /// Typed reason the exact completion was not enqueued.
    pub const fn reason(&self) -> ActorCompletionSendError {
        self.reason
    }

    /// Recover the unchanged exact completion envelope.
    pub fn into_completion(self) -> InterfaceTxCompletion {
        self.completion
    }
}

/// Why an exact outbound job was not accepted by an interface actor queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteError {
    /// Selected Reticulum interface has no current registry record.
    UnknownInterface(PacketInterfaceId),
    /// Selected interface currently rejects new outbound work.
    Offline(InterfaceLease),
    /// Complete native packet exceeds the selected interface's logical MTU.
    PacketTooLarge {
        /// Selected generation-safe interface authority.
        lease: InterfaceLease,
        /// Complete native packet length.
        packet_len: u16,
        /// Logical interface limit.
        logical_mtu: LogicalMtu,
    },
    /// Selected actor's bounded job queue is full.
    QueueFull(InterfaceLease),
}

/// DATA routing failure retaining the unchanged node-core job.
#[must_use = "a DATA route failure retains its exact unique owner"]
pub struct DataRouteFailure {
    reason: RouteError,
    job: RoutedTxJob<'static>,
}

impl fmt::Debug for DataRouteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataRouteFailure")
            .field("reason", &self.reason)
            .field("prepared", &self.job.prepared())
            .field("interface", &self.job.interface())
            .finish()
    }
}

impl DataRouteFailure {
    /// Typed reason this job was not accepted.
    pub const fn reason(&self) -> RouteError {
        self.reason
    }

    /// Recover the unchanged exact DATA owner.
    pub fn into_job(self) -> RoutedTxJob<'static> {
        self.job
    }
}

/// Ordinary routing failure retaining the unchanged node-core job.
#[must_use = "an ordinary route failure retains its exact unique owner"]
pub struct OrdinaryRouteFailure {
    reason: RouteError,
    job: OrdinaryTxJob<'static>,
}

impl fmt::Debug for OrdinaryRouteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrdinaryRouteFailure")
            .field("reason", &self.reason)
            .field("prepared", &self.job.prepared())
            .finish()
    }
}

impl OrdinaryRouteFailure {
    /// Typed reason this job was not accepted.
    pub const fn reason(&self) -> RouteError {
        self.reason
    }

    /// Recover the unchanged exact ordinary owner.
    pub fn into_job(self) -> OrdinaryTxJob<'static> {
        self.job
    }
}

/// Scalar proof that one exact owner entered its selected interface queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchReceipt {
    context: InterfaceDispatchContext,
}

impl DispatchReceipt {
    /// Registry/configuration snapshot stamped onto the accepted owner.
    pub const fn context(self) -> InterfaceDispatchContext {
        self.context
    }
}

/// Why one completed native ingress packet cannot reach node-core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressRouteError {
    /// Registry authority names a different queue than the queue observed by
    /// the router's fair scan.
    ForeignAuthorityQueue {
        /// Queue from which the packet was dequeued.
        observed: InterfaceQueueId,
        /// Queue stamped on the actor's registry authority.
        supplied: InterfaceQueueId,
    },
    /// Reusable buffer belongs to a different fixed actor queue.
    ForeignBufferQueue {
        /// Queue from which the packet was dequeued.
        observed: InterfaceQueueId,
        /// Permanent queue origin stamped on the reusable buffer.
        supplied: InterfaceQueueId,
    },
    /// Reusable buffer was seeded by a different static interface fabric.
    ForeignFabricOrigin(InterfaceQueueId),
    /// Actor authority is vacant, outside capacity, or superseded.
    StaleLease(InterfaceLeaseError),
    /// Sealed packet exceeds the authoritative current registry MTU.
    PacketExceedsCurrentMtu {
        /// Sealed native-packet length.
        actual: usize,
        /// Current registry logical MTU.
        maximum: LogicalMtu,
    },
}

/// Ingress-routing failure retaining the exact sealed packet and provenance.
#[must_use = "a rejected ingress packet remains exactly owned"]
pub struct IngressRouteFailure {
    reason: IngressRouteError,
    ingress: InterfaceIngress,
}

impl fmt::Debug for IngressRouteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngressRouteFailure")
            .field("reason", &self.reason)
            .field("lease", &self.ingress.lease())
            .field("buffer", &self.ingress.packet.id)
            .finish_non_exhaustive()
    }
}

impl IngressRouteFailure {
    /// Typed reason this exact ingress packet was rejected.
    pub const fn reason(&self) -> IngressRouteError {
        self.reason
    }

    /// Recover the unchanged sealed packet for explicit recycling.
    pub fn into_packet(self) -> SealedIngressPacket {
        self.ingress.packet
    }
}

/// Why the sole router could not recycle one exact ingress buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressBufferReturnError {
    /// Buffer's fixed queue is outside this router's fabric capacity.
    QueueOutsideFabric(InterfaceQueueId),
    /// Buffer's queue-local slot is outside the configured pool depth.
    SlotOutsidePool(IngressBufferId),
    /// Buffer came from a different static interface fabric.
    ForeignFabricOrigin(InterfaceQueueId),
    /// Correctly paired pool accounting was violated because the actor's
    /// available-buffer queue was unexpectedly full.
    QueueFull(InterfaceQueueId),
}

/// Buffer-return failure retaining the exact sealed native packet owner.
#[must_use = "a failed ingress-buffer return retains the exact owner"]
pub struct IngressBufferReturnFailure {
    reason: IngressBufferReturnError,
    packet: SealedIngressPacket,
}

impl fmt::Debug for IngressBufferReturnFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngressBufferReturnFailure")
            .field("reason", &self.reason)
            .field("buffer", &self.packet.id)
            .finish_non_exhaustive()
    }
}

impl IngressBufferReturnFailure {
    /// Typed reason the exact reusable buffer was not returned.
    pub const fn reason(&self) -> IngressBufferReturnError {
        self.reason
    }

    /// Recover the unchanged sealed packet for explicit retry or quarantine.
    pub fn into_packet(self) -> SealedIngressPacket {
        self.packet
    }
}

/// Why a dequeued actor completion cannot be attributed to the current
/// authoritative registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionRouteError {
    /// Completion's ticket names a different fixed queue.
    ForeignQueue {
        /// Queue from which the completion was dequeued.
        observed: InterfaceQueueId,
        /// Queue stamped on the completion ticket.
        supplied: InterfaceQueueId,
    },
    /// Completion's lease is vacant, outside capacity, or superseded.
    StaleLease(InterfaceLeaseError),
}

/// Completion-routing failure retaining the exact owning completion.
#[must_use = "a stale or foreign completion remains exactly owned"]
pub struct CompletionRouteFailure {
    reason: CompletionRouteError,
    completion: InterfaceTxCompletion,
}

impl fmt::Debug for CompletionRouteFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletionRouteFailure")
            .field("reason", &self.reason)
            .field("context", &self.completion.context())
            .finish_non_exhaustive()
    }
}

impl CompletionRouteFailure {
    /// Typed reason this completion was not attributed to the current lease.
    pub const fn reason(&self) -> CompletionRouteError {
        self.reason
    }

    /// Recover the unchanged exact interface completion envelope.
    pub fn into_completion(self) -> InterfaceTxCompletion {
        self.completion
    }

    /// Convert this rejected routing envelope into an explicit node-owner
    /// recovery value.
    ///
    /// A superseded interface generation must not strand a valid node-core
    /// owner. The permanent runtime must pass the returned completion to the
    /// matching DATA or ordinary owner even though the interface observation
    /// is stale. Node-core then performs its normal generation-safe
    /// reconciliation and may emit a serialized `Next` hop.
    pub fn into_node_recovery(self) -> CompletionRecovery {
        CompletionRecovery {
            reason: self.reason,
            completion: self.completion.into_outbound(),
        }
    }
}

/// Explicit node-reconciliation path for a stale or foreign interface
/// completion envelope.
#[must_use = "recovery completion must be reconciled by its node owner"]
pub struct CompletionRecovery {
    reason: CompletionRouteError,
    completion: OutboundCompletion,
}

impl CompletionRecovery {
    /// Interface-routing observation that required explicit reconciliation.
    pub const fn reason(&self) -> CompletionRouteError {
        self.reason
    }

    /// Recover the exact DATA or ordinary owning completion for node-core.
    pub fn into_outbound(self) -> OutboundCompletion {
        self.completion
    }
}

/// Sole transport-neutral outbound owner router and completion demultiplexer.
pub struct OutboundRouter<M, const SLOTS: usize, const QUEUE_DEPTH: usize>
where
    M: RawMutex + 'static,
{
    registry: InterfaceRegistry<SLOTS>,
    queues: [RouterQueue<M, QUEUE_DEPTH>; SLOTS],
    completion_cursor: usize,
    ingress_cursor: usize,
    lifecycle_cursor: usize,
}

impl<M, const SLOTS: usize, const QUEUE_DEPTH: usize> OutboundRouter<M, SLOTS, QUEUE_DEPTH>
where
    M: RawMutex + 'static,
{
    /// Immutable authoritative interface registry.
    pub const fn registry(&self) -> &InterfaceRegistry<SLOTS> {
        &self.registry
    }

    /// Derive the authoritative online interface snapshot for node-core route
    /// resolution.
    pub fn eligible_interfaces(&self) -> Result<InterfaceSet, EligibleInterfaceSetError> {
        self.registry.eligible_interfaces()
    }

    /// Derive the exact currently-online egress set for an announce learned on
    /// one registered ingress interface.
    pub fn announce_egress_interfaces(
        &self,
        source: PacketInterfaceId,
    ) -> Result<InterfaceSet, AnnounceEgressSetError> {
        self.registry.announce_egress_interfaces(source)
    }

    /// Consume, validate, apply or reject, and acknowledge at most one actor
    /// lifecycle request in bounded round-robin order.
    ///
    /// The acknowledgement queue is checked before removing the request, so a
    /// cancelled or wedged actor cannot cause a state transition whose exact
    /// result the fabric cannot retain. The router is the sole acknowledgement
    /// producer, making the subsequent send infallible while capacity remains
    /// reserved by this synchronous method.
    pub fn try_process_lifecycle(&mut self) -> Option<InterfaceLifecycleTransition> {
        for offset in 0..SLOTS {
            let index = (self.lifecycle_cursor + offset) % SLOTS;
            let queue = &self.queues[index];
            if queue.lifecycle_acknowledgements.is_full() {
                continue;
            }
            let Ok(request) = queue.lifecycle_requests.try_receive() else {
                continue;
            };
            self.lifecycle_cursor = (index + 1) % SLOTS;
            let observed = InterfaceQueueId(index as u16);
            let supplied = request.lease.queue;
            let result = if supplied != observed {
                Err(InterfaceLifecycleRouteError::ForeignQueue { observed, supplied })
            } else {
                self.registry
                    .set_online(request.lease, request.state.online())
                    .map_err(InterfaceLifecycleRouteError::InvalidLease)
            };
            let acknowledgement = InterfaceLifecycleAcknowledgement { request, result };
            match queue.lifecycle_acknowledgements.try_send(acknowledgement) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    unreachable!("sole lifecycle acknowledgement producer reserved capacity")
                }
            }
            return Some(InterfaceLifecycleTransition {
                queue: observed,
                acknowledgement,
            });
        }
        None
    }

    /// Dequeue and validate one exact completed native packet in bounded
    /// round-robin order.
    ///
    /// The observed queue, reusable-buffer origin, static fabric origin, and
    /// current registry lease must all agree. An online-to-offline transition
    /// remains valid because the actor already completed RX under this lease.
    /// Every rejection retains the exact sealed packet for recycling.
    #[allow(clippy::result_large_err)]
    pub fn try_receive_ingress(&mut self) -> Result<Option<ValidatedIngress>, IngressRouteFailure> {
        for offset in 0..SLOTS {
            let index = (self.ingress_cursor + offset) % SLOTS;
            let Ok(ingress) = self.queues[index].completed_ingress.try_receive() else {
                continue;
            };
            self.ingress_cursor = (index + 1) % SLOTS;
            return self.route_ingress(index, ingress).map(Some);
        }
        Ok(None)
    }

    /// Poll for one exact completed native packet in bounded round-robin
    /// order.
    ///
    /// A pending poll registers the current waker on every actor queue without
    /// reserving or removing any packet owner.
    #[allow(clippy::result_large_err)]
    pub fn poll_receive_ingress(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<ValidatedIngress, IngressRouteFailure>> {
        for offset in 0..SLOTS {
            let index = (self.ingress_cursor + offset) % SLOTS;
            let Poll::Ready(ingress) = self.queues[index].completed_ingress.poll_receive(context)
            else {
                continue;
            };
            self.ingress_cursor = (index + 1) % SLOTS;
            return Poll::Ready(self.route_ingress(index, ingress));
        }
        Poll::Pending
    }

    /// Wait cancellation-safely for the next exact completed native packet.
    #[allow(clippy::result_large_err)]
    pub async fn receive_ingress(&mut self) -> Result<ValidatedIngress, IngressRouteFailure> {
        poll_fn(|context| self.poll_receive_ingress(context)).await
    }

    /// Recycle one exact sealed packet buffer to its original fixed actor
    /// queue without awaiting.
    ///
    /// Correctly paired pool accounting guarantees capacity: one actor receive
    /// creates the only available-queue slot needed for its eventual return.
    /// The method remains typed and fail-closed so crossed fabrics or broken
    /// accounting never lose the non-`Copy` owner.
    #[allow(clippy::result_large_err)]
    pub fn try_return_ingress_buffer(
        &mut self,
        packet: SealedIngressPacket,
    ) -> Result<(), IngressBufferReturnFailure> {
        let id = packet.id;
        let Some(queue) = self.queues.get(id.queue.index()) else {
            return Err(IngressBufferReturnFailure {
                reason: IngressBufferReturnError::QueueOutsideFabric(id.queue),
                packet,
            });
        };
        if id.slot >= QUEUE_DEPTH {
            return Err(IngressBufferReturnFailure {
                reason: IngressBufferReturnError::SlotOutsidePool(id),
                packet,
            });
        }
        if !core::ptr::eq(packet.origin, queue.ingress_origin) {
            return Err(IngressBufferReturnFailure {
                reason: IngressBufferReturnError::ForeignFabricOrigin(id.queue),
                packet,
            });
        }
        let packet_len = packet.packet_len;
        let signal = packet.signal;
        match queue.available_ingress.try_send(packet.recycle()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(buffer)) => Err(IngressBufferReturnFailure {
                reason: IngressBufferReturnError::QueueFull(id.queue),
                packet: SealedIngressPacket {
                    id: buffer.id,
                    origin: buffer.origin,
                    storage: buffer.storage,
                    packet_len,
                    signal,
                },
            }),
        }
    }

    /// Register one stable interface identity in a vacant actor queue.
    pub fn register(
        &mut self,
        queue: InterfaceQueueId,
        interface: PacketInterfaceId,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceRegistrationError> {
        self.registry.register(queue, interface, properties, online)
    }

    /// Change whether a current interface accepts newly routed owners.
    pub fn set_online(
        &mut self,
        lease: InterfaceLease,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.registry.set_online(lease, online)
    }

    /// Replace MTU/configuration under a new queue-local generation.
    pub fn reconfigure(
        &mut self,
        lease: InterfaceLease,
        properties: InterfaceProperties,
        online: bool,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.registry.reconfigure(lease, properties, online)
    }

    /// Remove one current registry lease.
    pub fn unregister(
        &mut self,
        lease: InterfaceLease,
    ) -> Result<InterfaceDescriptor, InterfaceLeaseError> {
        self.registry.unregister(lease)
    }

    /// Route one exact destination-DATA owner to its selected interface.
    pub fn try_route_data(
        &mut self,
        job: RoutedTxJob<'static>,
    ) -> Result<DispatchReceipt, DataRouteFailure> {
        let interface = job.interface();
        let packet_len = job.packet_len();
        let descriptor = match self.validate_route(interface, packet_len) {
            Ok(descriptor) => descriptor,
            Err(reason) => return Err(DataRouteFailure { reason, job }),
        };
        let context = InterfaceDispatchContext { descriptor };
        let message = InterfaceTxJob::Data(InterfaceDataJob {
            context,
            binding: DataOwnerBinding::from_job(&job),
            job,
        });
        let queue = &self.queues[descriptor.lease.queue.index()];
        match queue.jobs.try_send(message) {
            Ok(()) => Ok(DispatchReceipt { context }),
            Err(TrySendError::Full(InterfaceTxJob::Data(job))) => Err(DataRouteFailure {
                reason: RouteError::QueueFull(descriptor.lease),
                job: job.job,
            }),
            Err(TrySendError::Full(InterfaceTxJob::Ordinary(_))) => {
                unreachable!("DATA enqueue returned an ordinary owner")
            }
        }
    }

    /// Route one exact ordinary protocol-action owner to its selected
    /// interface.
    pub fn try_route_ordinary(
        &mut self,
        job: OrdinaryTxJob<'static>,
    ) -> Result<DispatchReceipt, OrdinaryRouteFailure> {
        let interface = job.interface();
        let packet_len = job.packet_len();
        let descriptor = match self.validate_route(interface, packet_len) {
            Ok(descriptor) => descriptor,
            Err(reason) => return Err(OrdinaryRouteFailure { reason, job }),
        };
        let context = InterfaceDispatchContext { descriptor };
        let binding = job.prepared();
        let message = InterfaceTxJob::Ordinary(InterfaceOrdinaryJob {
            context,
            binding,
            job,
        });
        let queue = &self.queues[descriptor.lease.queue.index()];
        match queue.jobs.try_send(message) {
            Ok(()) => Ok(DispatchReceipt { context }),
            Err(TrySendError::Full(InterfaceTxJob::Ordinary(job))) => Err(OrdinaryRouteFailure {
                reason: RouteError::QueueFull(descriptor.lease),
                job: job.job,
            }),
            Err(TrySendError::Full(InterfaceTxJob::Data(_))) => {
                unreachable!("ordinary enqueue returned a DATA owner")
            }
        }
    }

    /// Poll until the selected interface queue can accept one owner, or until
    /// current registry state proves that routing would fail.
    ///
    /// Readiness is advisory. The caller must still use `try_route_data` or
    /// `try_route_ordinary`, which revalidates the registry generation, online
    /// state, MTU, and queue capacity while moving the exact owner.
    pub fn poll_route_capacity(
        &mut self,
        interface: PacketInterfaceId,
        packet_len: u16,
        context: &mut Context<'_>,
    ) -> Poll<Result<InterfaceDescriptor, RouteError>> {
        let descriptor = match self.validate_route(interface, packet_len) {
            Ok(descriptor) => descriptor,
            Err(reason) => return Poll::Ready(Err(reason)),
        };
        let queue = &self.queues[descriptor.lease.queue.index()];
        queue
            .jobs
            .poll_ready_to_send(context)
            .map(|()| Ok(descriptor))
    }

    /// Wait cancellation-safely for advisory capacity on one selected
    /// interface queue.
    ///
    /// Dropping this future while pending only removes a waker registration;
    /// it neither reserves queue capacity nor moves a packet owner.
    pub async fn wait_route_capacity(
        &mut self,
        interface: PacketInterfaceId,
        packet_len: u16,
    ) -> Result<InterfaceDescriptor, RouteError> {
        poll_fn(|context| self.poll_route_capacity(interface, packet_len, context)).await
    }

    /// Dequeue one exact actor completion in bounded round-robin order.
    ///
    /// Offline state does not invalidate previously accepted jobs. A changed
    /// generation does, and returns a retained stale-completion failure.
    // A stale result must retain the exact non-Copy completion without heap
    // allocation so the node recovery path can reconcile it.
    #[allow(clippy::result_large_err)]
    pub fn try_receive_completion(
        &mut self,
    ) -> Result<Option<OutboundCompletion>, CompletionRouteFailure> {
        for offset in 0..SLOTS {
            let index = (self.completion_cursor + offset) % SLOTS;
            let Ok(completion) = self.queues[index].completions.try_receive() else {
                continue;
            };
            self.completion_cursor = (index + 1) % SLOTS;
            let observed = InterfaceQueueId(index as u16);
            let supplied = completion.context().lease().queue();
            if supplied != observed {
                return Err(CompletionRouteFailure {
                    reason: CompletionRouteError::ForeignQueue { observed, supplied },
                    completion,
                });
            }
            if let Err(reason) = self.registry.validate(completion.context().lease()) {
                return Err(CompletionRouteFailure {
                    reason: CompletionRouteError::StaleLease(reason),
                    completion,
                });
            }
            return Ok(Some(completion.into_outbound()));
        }
        Ok(None)
    }

    /// Poll for one exact actor completion in bounded round-robin order.
    ///
    /// A pending poll registers the current waker on every actor completion
    /// queue without reserving or removing an owner. A ready poll moves exactly
    /// one completion and performs the same lease validation as
    /// [`Self::try_receive_completion`].
    #[allow(clippy::result_large_err)]
    pub fn poll_receive_completion(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<OutboundCompletion, CompletionRouteFailure>> {
        for offset in 0..SLOTS {
            let index = (self.completion_cursor + offset) % SLOTS;
            let Poll::Ready(completion) = self.queues[index].completions.poll_receive(context)
            else {
                continue;
            };
            self.completion_cursor = (index + 1) % SLOTS;
            return Poll::Ready(self.route_completion(index, completion));
        }
        Poll::Pending
    }

    /// Wait cancellation-safely for the next exact actor completion.
    ///
    /// Dropping this future while pending leaves every completion queued. A
    /// stale completion remains an owning error and must take the explicit
    /// node-recovery path.
    #[allow(clippy::result_large_err)]
    pub async fn receive_completion(
        &mut self,
    ) -> Result<OutboundCompletion, CompletionRouteFailure> {
        poll_fn(|context| self.poll_receive_completion(context)).await
    }

    #[allow(clippy::result_large_err)]
    fn route_ingress(
        &self,
        index: usize,
        ingress: InterfaceIngress,
    ) -> Result<ValidatedIngress, IngressRouteFailure> {
        let observed = InterfaceQueueId(index as u16);
        let authority_queue = ingress.lease().queue();
        if authority_queue != observed {
            return Err(IngressRouteFailure {
                reason: IngressRouteError::ForeignAuthorityQueue {
                    observed,
                    supplied: authority_queue,
                },
                ingress,
            });
        }
        let buffer_queue = ingress.packet.id.queue;
        if buffer_queue != observed {
            return Err(IngressRouteFailure {
                reason: IngressRouteError::ForeignBufferQueue {
                    observed,
                    supplied: buffer_queue,
                },
                ingress,
            });
        }
        if !core::ptr::eq(ingress.packet.origin, self.queues[index].ingress_origin) {
            return Err(IngressRouteFailure {
                reason: IngressRouteError::ForeignFabricOrigin(observed),
                ingress,
            });
        }
        match self.registry.validate(ingress.authority.lease()) {
            Ok(descriptor) => {
                let maximum = descriptor.logical_mtu();
                if ingress.packet.len() > usize::from(maximum.get()) {
                    return Err(IngressRouteFailure {
                        reason: IngressRouteError::PacketExceedsCurrentMtu {
                            actual: ingress.packet.len(),
                            maximum,
                        },
                        ingress,
                    });
                }
                Ok(ValidatedIngress {
                    descriptor,
                    packet: ingress.packet,
                })
            }
            Err(reason) => Err(IngressRouteFailure {
                reason: IngressRouteError::StaleLease(reason),
                ingress,
            }),
        }
    }

    #[allow(clippy::result_large_err)]
    fn route_completion(
        &self,
        index: usize,
        completion: InterfaceTxCompletion,
    ) -> Result<OutboundCompletion, CompletionRouteFailure> {
        let observed = InterfaceQueueId(index as u16);
        let supplied = completion.context().lease().queue();
        if supplied != observed {
            return Err(CompletionRouteFailure {
                reason: CompletionRouteError::ForeignQueue { observed, supplied },
                completion,
            });
        }
        if let Err(reason) = self.registry.validate(completion.context().lease()) {
            return Err(CompletionRouteFailure {
                reason: CompletionRouteError::StaleLease(reason),
                completion,
            });
        }
        Ok(completion.into_outbound())
    }

    fn validate_route(
        &self,
        interface: PacketInterfaceId,
        packet_len: u16,
    ) -> Result<InterfaceDescriptor, RouteError> {
        let Some(descriptor) = self.registry.descriptor(interface) else {
            return Err(RouteError::UnknownInterface(interface));
        };
        if !descriptor.online {
            return Err(RouteError::Offline(descriptor.lease));
        }
        if packet_len > descriptor.logical_mtu().get() {
            return Err(RouteError::PacketTooLarge {
                lease: descriptor.lease,
                packet_len,
                logical_mtu: descriptor.logical_mtu(),
            });
        }
        Ok(descriptor)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        future::Future,
        task::{Context, Poll},
    };
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rand_core::{CryptoRng, RngCore};
    use reticulum_node_core::{
        DestinationHash, InterfaceSet, MonotonicMillis, MonotonicSeconds, NodeConfig, NodeCore,
        NodeIdentity, NodeInstanceId, OrdinaryActionAdmissionRequest, OrdinaryBufferPool,
        OrdinaryCompletionDisposition, OrdinaryPacketBuffer, PACKET_CAPACITY, PrepareDataRequest,
        TxCompletionCode, TxCompletionDisposition, TxLeaseDeadline, TxPacketBuffer, TxTarget,
    };
    use reticulum_rns_rete::{
        EmbeddedNode, EmbeddedNodeConfig, InboundProofPolicy, InterfaceId as RnsInterfaceId,
        NodeActions,
    };
    use std::{
        boxed::Box,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Wake, Waker},
    };

    use super::*;

    type TestNode<const BUFFERS: usize> = NodeCore<8, 4, 8, 8, BUFFERS>;

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

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

    fn identity(tag: u8) -> NodeIdentity {
        NodeIdentity::from_private_key(&[tag; 64]).expect("test identity must be valid")
    }

    fn node<const BUFFERS: usize>(tag: u8, aspect: &str) -> TestNode<BUFFERS> {
        TestNode::new(
            identity(tag),
            "reticulum",
            &[aspect],
            NodeInstanceId::new([tag.wrapping_add(0x80); 16]),
            NodeConfig::endpoint(),
        )
        .expect("test node must construct")
    }

    fn interfaces(ids: &[u8]) -> InterfaceSet {
        ids.iter().fold(InterfaceSet::empty(), |set, id| {
            set.with(PacketInterfaceId::new(*id))
                .expect("test interface must fit compact profile")
        })
    }

    fn register_peer<const BUFFERS: usize>(sender: &mut TestNode<BUFFERS>, tag: u8, aspect: &str) {
        sender
            .register_peer(
                &identity(tag),
                "reticulum",
                &[aspect],
                MonotonicSeconds::new(0),
            )
            .expect("test peer must register");
    }

    fn prepare(
        sender: &mut TestNode<3>,
        buffer: &'static mut TxPacketBuffer,
        destination: DestinationHash,
        enabled_interfaces: InterfaceSet,
        rng: &mut CounterRng,
    ) -> RoutedTxJob<'static> {
        match sender.prepare_data_into_slot(
            buffer,
            PrepareDataRequest {
                destination,
                plaintext: b"interface router",
                rns_now: MonotonicSeconds::new(1),
                owner_now: MonotonicMillis::new(10),
                deadline: TxLeaseDeadline::new(MonotonicMillis::new(10_000)),
                enabled_interfaces,
            },
            rng,
        ) {
            Ok(job) => job,
            Err(failure) => panic!("test preparation failed: {:?}", failure.reason()),
        }
    }

    fn fabric<const DEPTH: usize>() -> (
        OutboundRouter<NoopRawMutex, 2, DEPTH>,
        [InterfaceActorHandoff<NoopRawMutex, DEPTH>; 2],
    ) {
        Box::leak(Box::new(InterfaceFabric::new())).split()
    }

    fn configure<const DEPTH: usize>(
        router: &mut OutboundRouter<NoopRawMutex, 2, DEPTH>,
        actors: &[InterfaceActorHandoff<NoopRawMutex, DEPTH>; 2],
    ) -> (InterfaceDescriptor, InterfaceDescriptor) {
        let mtu = LogicalMtu::try_new(500).expect("test MTU must be nonzero");
        let first_properties = InterfaceProperties::new(
            mtu,
            InterfaceConfigId::new(20),
            AdvertisedBitrate::try_new(1_000_000).ok(),
            InterfaceCost::new(10),
        );
        let second_properties = InterfaceProperties::new(
            mtu,
            InterfaceConfigId::new(90),
            AdvertisedBitrate::try_new(5_400).ok(),
            InterfaceCost::new(80),
        );
        let first = router
            .register(
                actors[0].queue_id(),
                PacketInterfaceId::new(2),
                first_properties,
                true,
            )
            .expect("first interface must register");
        let second = router
            .register(
                actors[1].queue_id(),
                PacketInterfaceId::new(9),
                second_properties,
                true,
            )
            .expect("second interface must register");
        (first, second)
    }

    fn setup_sender() -> (TestNode<3>, DestinationHash, CounterRng) {
        let mut sender = node::<3>(1, "interface-router");
        let receiver = node::<3>(2, "peer");
        register_peer(&mut sender, 2, "peer");
        let destination = receiver.destination_hash();
        (sender, destination, CounterRng(3))
    }

    #[test]
    fn announce_egress_uses_exact_source_and_mode_pair_without_bearer_knowledge() {
        let mtu = LogicalMtu::try_new(500).expect("test MTU must be nonzero");
        let properties = |config| {
            InterfaceProperties::new(
                mtu,
                InterfaceConfigId::new(config),
                None,
                InterfaceCost::new(0),
            )
        };
        let mut registry = InterfaceRegistry::<3>::new();
        registry
            .register(
                InterfaceQueueId::new(0),
                PacketInterfaceId::new(1),
                properties(1).with_announce_mode(AnnouncePropagationMode::Internal),
                true,
            )
            .expect("internal shared-medium interface registers");
        registry
            .register(
                InterfaceQueueId::new(1),
                PacketInterfaceId::new(2),
                properties(2)
                    .with_topology(InterfaceTopology::PointToPoint)
                    .with_announce_mode(AnnouncePropagationMode::Boundary),
                true,
            )
            .expect("boundary point-to-point interface registers");
        registry
            .register(
                InterfaceQueueId::new(2),
                PacketInterfaceId::new(3),
                properties(3).with_announce_mode(AnnouncePropagationMode::Full),
                true,
            )
            .expect("full interface registers");

        assert_eq!(
            registry
                .announce_egress_interfaces(PacketInterfaceId::new(2))
                .expect("registered boundary source resolves"),
            interfaces(&[3]),
            "a boundary announce cannot enter Internal and cannot reflect to its point-to-point source"
        );
        assert_eq!(
            registry
                .announce_egress_interfaces(PacketInterfaceId::new(1))
                .expect("registered internal source resolves"),
            interfaces(&[1, 2, 3]),
            "an Internal announce can repeat on its shared medium and cross the boundary"
        );
        assert_eq!(
            registry.announce_egress_interfaces(PacketInterfaceId::new(9)),
            Err(AnnounceEgressSetError::SourceUnavailable(
                PacketInterfaceId::new(9)
            ))
        );
    }

    fn rns_identity(tag: u8) -> reticulum_rns_rete::Identity {
        reticulum_rns_rete::identity_from_private_key(&[tag; 64])
            .expect("RNS test identity must be valid")
    }

    fn rns_node(tag: u8) -> EmbeddedNode<4, 4, 8, 2> {
        EmbeddedNode::new(
            rns_identity(tag),
            "reticulum",
            &["interface-router-ordinary"],
            EmbeddedNodeConfig::endpoint(),
        )
        .expect("RNS test node must construct")
    }

    fn source_only_actions(source_interface: u8, rng: &mut CounterRng) -> NodeActions {
        let mut sender = rns_node(41);
        let mut receiver = rns_node(42);
        receiver.set_inbound_proof_policy(InboundProofPolicy::Always);
        receiver
            .queue_announce(None, 1, rng)
            .expect("receiver announce must queue");
        let announce = receiver.flush_announces(1, rng);
        sender.ingest(announce[0].bytes(), 1, RnsInterfaceId(2), rng);
        let mut encoded = [0u8; PACKET_CAPACITY];
        let prepared = sender
            .prepare_data_into(
                &receiver.destination_hash(),
                b"ordinary proof",
                2,
                rng,
                &mut encoded,
            )
            .expect("test DATA packet must prepare");
        receiver
            .ingest(
                &encoded[..usize::from(prepared.packet_len())],
                2,
                RnsInterfaceId(source_interface),
                rng,
            )
            .actions
    }

    fn seal_bytes(mut buffer: AvailableIngressBuffer, bytes: &[u8]) -> SealedIngressPacket {
        buffer.capacity_mut()[..bytes.len()].copy_from_slice(bytes);
        buffer
            .seal(bytes.len())
            .expect("test ingress length must fit")
    }

    #[test]
    fn cancelled_woken_receive_leaves_exact_job_queued() {
        let (mut router, mut actors) = fabric::<1>();
        configure(&mut router, &actors);
        let (mut sender, destination, mut rng) = setup_sender();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender
            .register_packet_buffer(buffer)
            .expect("buffer must register");
        let job = prepare(&mut sender, buffer, destination, interfaces(&[9]), &mut rng);
        let expected = job.prepared();

        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&wake_counter));
        let mut context = Context::from_waker(&waker);
        let mut receive = Box::pin(actors[1].receive_job());
        assert!(matches!(receive.as_mut().poll(&mut context), Poll::Pending));

        router
            .try_route_data(job)
            .expect("queued owner must wake the pending receiver");
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        drop(receive);

        assert_eq!(actors[1].pending_jobs(), 1);
        let InterfaceTxJob::Data(received) = actors[1]
            .try_receive_job()
            .expect("cancellation must leave the exact owner queued")
        else {
            panic!("cancellation changed the queued owner family")
        };
        let (ticket, received) = received.into_parts();
        assert_eq!(received.prepared(), expected);
        let completion = ticket
            .complete(
                received
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(90)),
            )
            .expect("cancelled receive must preserve the ticket binding");
        actors[1]
            .try_send_completion(completion)
            .expect("completion must return through the exact actor");
        let Some(OutboundCompletion::Data(completion)) = router
            .try_receive_completion()
            .expect("current completion lease must remain valid")
        else {
            panic!("cancelled receive did not return the DATA completion")
        };
        assert!(matches!(
            sender.complete_tx(completion, MonotonicMillis::new(20)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    #[test]
    fn ready_poll_returns_exact_non_copy_owner() {
        let (mut router, mut actors) = fabric::<1>();
        configure(&mut router, &actors);
        let (mut sender, destination, mut rng) = setup_sender();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender
            .register_packet_buffer(buffer)
            .expect("buffer must register");
        let job = prepare(&mut sender, buffer, destination, interfaces(&[9]), &mut rng);
        let expected = job.prepared();
        router
            .try_route_data(job)
            .expect("exact owner must fit the actor queue");

        let waker = Waker::from(Arc::new(WakeCounter::default()));
        let mut context = Context::from_waker(&waker);
        let Poll::Ready(InterfaceTxJob::Data(received)) = actors[1].poll_receive_job(&mut context)
        else {
            panic!("ready polling did not return the queued DATA owner")
        };
        assert_eq!(actors[1].pending_jobs(), 0);
        let (ticket, received) = received.into_parts();
        assert_eq!(received.prepared(), expected);
        let completion = ticket
            .complete(
                received
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(91)),
            )
            .expect("polled owner must retain its exact ticket binding");
        actors[1]
            .try_send_completion(completion)
            .expect("completion must return through the exact actor");
        let Some(OutboundCompletion::Data(completion)) = router
            .try_receive_completion()
            .expect("current completion lease must remain valid")
        else {
            panic!("ready poll did not return the DATA completion")
        };
        assert!(matches!(
            sender.complete_tx(completion, MonotonicMillis::new(20)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    #[test]
    fn route_capacity_and_completion_polls_wake_without_reserving_owners() {
        let (mut router, mut actors) = fabric::<1>();
        configure(&mut router, &actors);
        let (mut sender, destination, mut rng) = setup_sender();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender
            .register_packet_buffer(buffer)
            .expect("buffer must register");
        let job = prepare(&mut sender, buffer, destination, interfaces(&[9]), &mut rng);
        let expected = job.prepared();
        let packet_len = job.packet_len();
        router
            .try_route_data(job)
            .expect("first owner must fill the second actor queue");

        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&wake_counter));
        let mut context = Context::from_waker(&waker);
        assert!(matches!(
            router.poll_route_capacity(PacketInterfaceId::new(9), packet_len, &mut context),
            Poll::Pending
        ));
        let InterfaceTxJob::Data(job) = actors[1]
            .try_receive_job()
            .expect("actor dequeue must return the exact queued owner")
        else {
            panic!("DATA owner changed family")
        };
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        assert!(matches!(
            router.poll_route_capacity(PacketInterfaceId::new(9), packet_len, &mut context),
            Poll::Ready(Ok(descriptor))
                if descriptor.lease().interface() == PacketInterfaceId::new(9)
        ));

        let before_completion = wake_counter.0.load(Ordering::SeqCst);
        assert!(matches!(
            router.poll_receive_completion(&mut context),
            Poll::Pending
        ));
        let (ticket, job) = job.into_parts();
        assert_eq!(job.prepared(), expected);
        let completion = ticket
            .complete(job.return_unpermitted().complete(TxCompletionCode::new(92)))
            .expect("ticket must bind the exact DATA owner");
        actors[1]
            .try_send_completion(completion)
            .expect("completion must fit");
        assert!(wake_counter.0.load(Ordering::SeqCst) > before_completion);

        // Abandon the pending poll and use the immediate API. The completion
        // must still be queued because a pending poll only registered a waker.
        let Some(OutboundCompletion::Data(completion)) = router
            .try_receive_completion()
            .expect("current completion must remain routable")
        else {
            panic!("cancelled completion poll removed or changed the owner")
        };
        assert!(matches!(
            sender.complete_tx(completion, MonotonicMillis::new(20)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    #[test]
    fn completion_capacity_wait_is_advisory_and_preserves_the_rejected_owner() {
        let (mut router, mut actors) = fabric::<1>();
        configure(&mut router, &actors);
        let (mut sender, destination, mut rng) = setup_sender();
        let first_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let second_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender
            .register_packet_buffer(first_buffer)
            .expect("first buffer must register");
        sender
            .register_packet_buffer(second_buffer)
            .expect("second buffer must register");

        let first = prepare(
            &mut sender,
            first_buffer,
            destination,
            interfaces(&[9]),
            &mut rng,
        );
        router
            .try_route_data(first)
            .expect("first owner must enter the actor queue");
        let InterfaceTxJob::Data(first) = actors[1]
            .try_receive_job()
            .expect("actor must receive the first owner")
        else {
            panic!("first owner changed family")
        };

        let second = prepare(
            &mut sender,
            second_buffer,
            destination,
            interfaces(&[9]),
            &mut rng,
        );
        router
            .try_route_data(second)
            .expect("second owner must enter the vacated actor queue");
        let InterfaceTxJob::Data(second) = actors[1]
            .try_receive_job()
            .expect("actor must receive the second owner")
        else {
            panic!("second owner changed family")
        };

        let (first_ticket, first) = first.into_parts();
        let first_completion = first_ticket
            .complete(
                first
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(93)),
            )
            .expect("first ticket must bind its exact owner");
        actors[1]
            .try_send_completion(first_completion)
            .expect("first completion must fill the actor queue");

        let (second_ticket, second) = second.into_parts();
        let expected_second = second.prepared();
        let second_completion = second_ticket
            .complete(
                second
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(94)),
            )
            .expect("second ticket must bind its exact owner");
        let rejected = actors[1]
            .try_send_completion(second_completion)
            .expect_err("full completion queue must return the second owner");
        assert_eq!(
            rejected.reason(),
            ActorCompletionSendError::QueueFull(actors[1].queue_id())
        );
        let retained_second = rejected.into_completion();

        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&wake_counter));
        let mut context = Context::from_waker(&waker);
        let mut capacity_wait = Box::pin(actors[1].wait_completion_capacity());
        assert!(matches!(
            capacity_wait.as_mut().poll(&mut context),
            Poll::Pending
        ));

        let Some(OutboundCompletion::Data(first_completion)) = router
            .try_receive_completion()
            .expect("first completion lease must remain current")
        else {
            panic!("first completion changed family")
        };
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);

        // Cancel after the registered wait was woken but before polling it
        // again. The wait owns no completion and cannot reserve the capacity.
        drop(capacity_wait);
        assert!(matches!(
            actors[1].poll_completion_capacity(&mut context),
            Poll::Ready(())
        ));
        assert!(matches!(
            sender.complete_tx(first_completion, MonotonicMillis::new(20)),
            Ok(TxCompletionDisposition::Available(_))
        ));

        actors[1]
            .try_send_completion(retained_second)
            .expect("cancelled advisory wait must leave capacity unreserved");
        let Some(OutboundCompletion::Data(second_completion)) = router
            .try_receive_completion()
            .expect("second completion lease must remain current")
        else {
            panic!("second completion changed family")
        };
        assert_eq!(second_completion.prepared(), expected_second);
        assert!(matches!(
            sender.complete_tx(second_completion, MonotonicMillis::new(20)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    #[test]
    fn only_routes_to_exact_actor_and_foreign_return_is_rejected_unchanged() {
        let (mut router, mut actors) = fabric::<1>();
        configure(&mut router, &actors);
        let mut node = node::<3>(51, "interface-router-ordinary");
        let mut owner = node
            .take_ordinary_action_owner::<1>()
            .expect("ordinary owner must be issued once");
        let mut pool = OrdinaryBufferPool::<1>::new();
        if let Err(failure) =
            owner.register_and_park(&mut pool, Box::leak(Box::new(OrdinaryPacketBuffer::new())))
        {
            panic!(
                "ordinary buffer did not register and park: {:?}",
                failure.reason()
            );
        }
        let mut rng = CounterRng(70);
        let actions = source_only_actions(9, &mut rng);
        let mut batch = owner
            .admit_from_pool(
                actions,
                &mut pool,
                OrdinaryActionAdmissionRequest {
                    owner_now: MonotonicMillis::new(10),
                    deadline: TxLeaseDeadline::new(MonotonicMillis::new(10_000)),
                    enabled_interfaces: interfaces(&[2, 9]),
                },
            )
            .expect("source-relative ordinary action must admit");
        let job = batch
            .take_next_packet()
            .expect("proof action must own one packet");
        assert_eq!(job.target(), TxTarget::Only(PacketInterfaceId::new(9)));

        let receipt = router.try_route_ordinary(job).expect("Only route must fit");
        assert_eq!(
            receipt.context().lease().interface(),
            PacketInterfaceId::new(9)
        );
        assert!(actors[0].try_receive_job().is_none());
        let InterfaceTxJob::Ordinary(job) = actors[1]
            .try_receive_job()
            .expect("second actor must receive exact owner")
        else {
            panic!("ordinary route changed owner family")
        };
        let (ticket, job) = job.into_parts();
        let completion = ticket
            .complete(job.return_unpermitted().complete(TxCompletionCode::new(1)))
            .expect("exact completion must match ticket");
        let crossed = actors[0]
            .try_send_completion(completion)
            .expect_err("foreign actor must reject completion");
        assert_eq!(
            crossed.reason(),
            ActorCompletionSendError::ForeignQueue {
                expected: actors[0].queue_id(),
                supplied: actors[1].queue_id(),
            }
        );
        actors[1]
            .try_send_completion(crossed.into_completion())
            .expect("correct actor must return exact completion");
        let Some(OutboundCompletion::Ordinary(completion)) = router
            .try_receive_completion()
            .expect("current lease must accept completion")
        else {
            panic!("router did not return ordinary completion")
        };
        assert!(matches!(
            owner.complete_tx(completion, MonotonicMillis::new(20)),
            Ok(OrdinaryCompletionDisposition::Returned(_))
        ));
    }

    #[test]
    fn all_remains_serialized_and_node_core_selects_next_actor() {
        let (mut router, mut actors) = fabric::<1>();
        configure(&mut router, &actors);
        let (mut sender, destination, mut rng) = setup_sender();
        let buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender
            .register_packet_buffer(buffer)
            .expect("buffer must register");
        let first = prepare(
            &mut sender,
            buffer,
            destination,
            interfaces(&[2, 9]),
            &mut rng,
        );
        assert_eq!(first.target(), TxTarget::All);
        assert_eq!(first.interface(), PacketInterfaceId::new(2));
        router.try_route_data(first).expect("first hop must fit");
        assert!(actors[1].try_receive_job().is_none());

        let InterfaceTxJob::Data(first) = actors[0]
            .try_receive_job()
            .expect("first actor must receive first hop")
        else {
            panic!("first hop changed owner family")
        };
        let (ticket, first) = first.into_parts();
        let completion = ticket
            .complete(
                first
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(2)),
            )
            .expect("first completion must match");
        actors[0]
            .try_send_completion(completion)
            .expect("first completion must fit");
        let Some(OutboundCompletion::Data(completion)) = router
            .try_receive_completion()
            .expect("first completion lease must remain current")
        else {
            panic!("first completion missing")
        };
        let next = match sender.complete_tx(completion, MonotonicMillis::new(20)) {
            Ok(TxCompletionDisposition::Next(next)) => next,
            Ok(_) => panic!("All target did not produce serialized Next"),
            Err(failure) => panic!("first completion did not reconcile: {:?}", failure.reason()),
        };
        assert_eq!(next.interface(), PacketInterfaceId::new(9));
        router.try_route_data(next).expect("Next hop must fit");
        assert!(actors[0].try_receive_job().is_none());

        let InterfaceTxJob::Data(second) = actors[1]
            .try_receive_job()
            .expect("second actor must receive Next")
        else {
            panic!("Next changed owner family")
        };
        let (ticket, second) = second.into_parts();
        let completion = ticket
            .complete(
                second
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(3)),
            )
            .expect("second completion must match");
        actors[1]
            .try_send_completion(completion)
            .expect("second completion must fit");
        let Some(OutboundCompletion::Data(completion)) = router
            .try_receive_completion()
            .expect("second completion lease must remain current")
        else {
            panic!("second completion missing")
        };
        assert!(matches!(
            sender.complete_tx(completion, MonotonicMillis::new(30)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    #[test]
    fn offline_and_queue_pressure_return_exact_data_owners() {
        let (mut router, actors) = fabric::<1>();
        let (_, second) = configure(&mut router, &actors);
        let (mut sender, destination, mut rng) = setup_sender();
        let first_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let second_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let third_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender
            .register_packet_buffer(first_buffer)
            .expect("first buffer must register");
        sender
            .register_packet_buffer(second_buffer)
            .expect("second buffer must register");
        sender
            .register_packet_buffer(third_buffer)
            .expect("third buffer must register");

        let first = prepare(
            &mut sender,
            first_buffer,
            destination,
            interfaces(&[9]),
            &mut rng,
        );
        router
            .try_route_data(first)
            .expect("first queue slot must fit");
        let pressured = prepare(
            &mut sender,
            second_buffer,
            destination,
            interfaces(&[9]),
            &mut rng,
        );
        let pressured_slot = pressured.slot_id();
        let failure = router
            .try_route_data(pressured)
            .expect_err("depth-one queue must retain pressure");
        assert_eq!(failure.reason(), RouteError::QueueFull(second.lease()));
        assert_eq!(failure.into_job().slot_id(), pressured_slot);

        router
            .set_online(second.lease(), false)
            .expect("current lease must go offline");
        let offline = prepare(
            &mut sender,
            third_buffer,
            destination,
            interfaces(&[9]),
            &mut rng,
        );
        let offline_slot = offline.slot_id();
        let failure = router
            .try_route_data(offline)
            .expect_err("offline interface must reject new owner");
        assert_eq!(failure.reason(), RouteError::Offline(second.lease()));
        assert_eq!(failure.into_job().slot_id(), offline_slot);
    }

    #[test]
    fn offline_inflight_completion_is_valid_but_reconfigured_generation_is_retained_stale() {
        let (mut router, mut actors) = fabric::<2>();
        let (_, second) = configure(&mut router, &actors);
        let (mut sender, destination, mut rng) = setup_sender();
        let first_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        let stale_buffer = Box::leak(Box::new(TxPacketBuffer::new()));
        sender
            .register_packet_buffer(first_buffer)
            .expect("first buffer must register");
        sender
            .register_packet_buffer(stale_buffer)
            .expect("stale buffer must register");

        let first = prepare(
            &mut sender,
            first_buffer,
            destination,
            interfaces(&[9]),
            &mut rng,
        );
        router.try_route_data(first).expect("first owner must fit");
        router
            .set_online(second.lease(), false)
            .expect("current interface must go offline");
        let InterfaceTxJob::Data(first) = actors[1]
            .try_receive_job()
            .expect("accepted owner remains queued")
        else {
            panic!("accepted owner changed family")
        };
        let (ticket, first) = first.into_parts();
        let completion = ticket
            .complete(
                first
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(4)),
            )
            .expect("exact completion must match");
        actors[1]
            .try_send_completion(completion)
            .expect("offline actor may finish accepted owner");
        let Ok(Some(OutboundCompletion::Data(completion))) = router.try_receive_completion() else {
            panic!("offline in-flight completion did not return to node owner")
        };
        assert!(matches!(
            sender.complete_tx(completion, MonotonicMillis::new(20)),
            Ok(TxCompletionDisposition::Available(_))
        ));

        router
            .set_online(second.lease(), true)
            .expect("same generation must return online");
        let stale = prepare(
            &mut sender,
            stale_buffer,
            destination,
            interfaces(&[9]),
            &mut rng,
        );
        router
            .try_route_data(stale)
            .expect("stale candidate must queue");
        let InterfaceTxJob::Data(stale) = actors[1]
            .try_receive_job()
            .expect("actor must receive stale candidate")
        else {
            panic!("stale candidate changed family")
        };
        let (ticket, stale) = stale.into_parts();
        let old_lease = ticket.context().lease();
        let completion = ticket
            .complete(
                stale
                    .return_unpermitted()
                    .complete(TxCompletionCode::new(5)),
            )
            .expect("exact completion must match old ticket");
        let current = router
            .reconfigure(
                old_lease,
                InterfaceProperties::new(
                    LogicalMtu::try_new(480).expect("MTU must be nonzero"),
                    InterfaceConfigId::new(91),
                    AdvertisedBitrate::try_new(7_200).ok(),
                    InterfaceCost::new(75),
                ),
                true,
            )
            .expect("reconfiguration must advance generation");
        assert_ne!(current.lease().generation(), old_lease.generation());
        actors[1]
            .try_send_completion(completion)
            .expect("old actor ticket still belongs to its queue");
        let failure = router
            .try_receive_completion()
            .expect_err("superseded generation must be retained stale");
        assert!(matches!(
            failure.reason(),
            CompletionRouteError::StaleLease(InterfaceLeaseError::Stale { supplied, current: observed })
                if supplied == old_lease && observed == current
        ));
        let recovery = failure.into_node_recovery();
        assert!(matches!(
            recovery.reason(),
            CompletionRouteError::StaleLease(InterfaceLeaseError::Stale { .. })
        ));
        let OutboundCompletion::Data(completion) = recovery.into_outbound() else {
            panic!("stale failure changed owner family")
        };
        assert_eq!(completion.interface(), PacketInterfaceId::new(9));
        assert!(matches!(
            sender.complete_tx(completion, MonotonicMillis::new(30)),
            Ok(TxCompletionDisposition::Available(_))
        ));
    }

    #[test]
    fn registry_is_the_enabled_snapshot_and_actor_capability_is_queue_bound() {
        let (mut router, actors) = fabric::<1>();
        let (first, second) = configure(&mut router, &actors);
        assert_eq!(
            first.advertised_bitrate().map(AdvertisedBitrate::get),
            Some(1_000_000)
        );
        assert_eq!(first.cost(), InterfaceCost::new(10));
        let eligible = router
            .eligible_interfaces()
            .expect("configured IDs must fit compact profile");
        assert!(eligible.contains(PacketInterfaceId::new(2)));
        assert!(eligible.contains(PacketInterfaceId::new(9)));

        router
            .set_online(second.lease(), false)
            .expect("second lease must go offline");
        let eligible = router
            .eligible_interfaces()
            .expect("offline ID must still fit compact profile");
        assert!(eligible.contains(PacketInterfaceId::new(2)));
        assert!(!eligible.contains(PacketInterfaceId::new(9)));

        assert_eq!(
            actors[1].bind_ingress(first),
            Err(ActorIngressBindingError::ForeignQueue {
                expected: actors[1].queue_id(),
                supplied: actors[0].queue_id(),
            })
        );
        let authority = actors[0]
            .bind_ingress(first)
            .expect("matching actor must bind registry provenance");
        assert_eq!(authority.lease(), first.lease());
    }

    #[test]
    fn native_ingress_roundtrip_preserves_provenance_bytes_and_exact_reuse() {
        let (mut router, mut actors) = fabric::<1>();
        let (first, _) = configure(&mut router, &actors);
        assert_eq!(actors[0].available_ingress_buffers(), 1);
        let authority = actors[0]
            .bind_ingress(first)
            .expect("matching actor must bind ingress authority");
        let mut buffer = actors[0]
            .try_receive_ingress_buffer()
            .expect("split must seed the first reusable buffer");
        let id = buffer.id();
        buffer.capacity_mut()[..b"native ingress".len()].copy_from_slice(b"native ingress");
        let signal = IngressSignalObservation::new(-93, 5);
        let packet = buffer
            .seal_with_signal(b"native ingress".len(), Some(signal))
            .expect("valid signalled ingress must seal");
        assert_eq!(packet.id(), id);
        assert_eq!(packet.as_bytes(), b"native ingress");
        assert_eq!(packet.signal(), Some(signal));
        actors[0]
            .try_send_ingress(authority, packet)
            .expect("current ingress must enter the actor queue");

        let validated = router
            .try_receive_ingress()
            .expect("current ingress must validate")
            .expect("completed ingress must be queued");
        assert_eq!(validated.interface(), first.lease().interface());
        assert_eq!(validated.descriptor(), first);
        assert_eq!(validated.signal(), Some(signal));
        let packet = validated.into_packet();
        assert_eq!(packet.id(), id);
        assert_eq!(packet.as_ref(), b"native ingress");
        router
            .try_return_ingress_buffer(packet)
            .expect("paired pool accounting must admit exact return");

        let reused = actors[0]
            .try_receive_ingress_buffer()
            .expect("returned exact buffer must be reusable");
        assert_eq!(reused.id(), id);
        assert_eq!(actors[0].available_ingress_buffers(), 0);
        let packet = seal_bytes(reused, b"again");
        assert_eq!(packet.as_bytes(), b"again");
        assert_eq!(packet.signal(), None);
        router
            .try_return_ingress_buffer(packet)
            .expect("unsubmitted acquired buffer may be explicitly recycled");
        assert_eq!(actors[0].available_ingress_buffers(), 1);
    }

    #[test]
    fn split_tx_and_ingress_capabilities_share_origin_across_two_slots() {
        let (mut router, actors) = fabric::<1>();
        let (first, second) = configure(&mut router, &actors);
        let [first_actor, second_actor] = actors;
        let (first_tx, mut first_rx, _first_lifecycle) = first_actor.into_parts();
        let (second_tx, mut second_rx, _second_lifecycle) = second_actor.into_parts();
        assert_eq!(first_tx.queue_id(), first.lease().queue());
        assert_eq!(first_rx.queue_id(), first.lease().queue());
        assert_eq!(second_tx.queue_id(), second.lease().queue());
        assert_eq!(second_rx.queue_id(), second.lease().queue());
        assert_eq!(first_rx.available_buffers(), 1);
        assert_eq!(second_rx.available_buffers(), 1);

        let first_authority = first_rx
            .bind_ingress(first)
            .expect("first split RX capability must bind its descriptor");
        let second_authority = second_rx
            .bind_ingress(second)
            .expect("second split RX capability must bind its descriptor");
        let first_buffer = first_rx
            .try_receive_buffer()
            .expect("first split pool must be seeded");
        first_rx
            .try_send(first_authority, seal_bytes(first_buffer, b"first"))
            .expect("first split endpoint must share router origin");
        let second_buffer = second_rx
            .try_receive_buffer()
            .expect("second split pool must be seeded");
        second_rx
            .try_send(second_authority, seal_bytes(second_buffer, b"second"))
            .expect("second split endpoint must share router origin");

        let first_packet = router
            .try_receive_ingress()
            .expect("first packet must validate")
            .expect("first packet must be queued")
            .into_packet();
        let second_packet = router
            .try_receive_ingress()
            .expect("second packet must validate")
            .expect("second packet must be queued")
            .into_packet();
        assert_eq!(first_packet.as_bytes(), b"first");
        assert_eq!(second_packet.as_bytes(), b"second");
        router
            .try_return_ingress_buffer(first_packet)
            .expect("first common-origin buffer must return");
        router
            .try_return_ingress_buffer(second_packet)
            .expect("second common-origin buffer must return");
        assert_eq!(first_rx.available_buffers(), 1);
        assert_eq!(second_rx.available_buffers(), 1);
    }

    #[test]
    fn stale_ingress_retains_exact_packet_for_recycle() {
        let (mut router, mut actors) = fabric::<1>();
        let (first, _) = configure(&mut router, &actors);
        let authority = actors[0]
            .bind_ingress(first)
            .expect("matching actor must bind old authority");
        let packet = seal_bytes(
            actors[0]
                .try_receive_ingress_buffer()
                .expect("pool must be seeded"),
            b"stale but owned",
        );
        let id = packet.id();
        actors[0]
            .try_send_ingress(authority, packet)
            .expect("old authority is current at actor submission");
        let current = router
            .reconfigure(
                first.lease(),
                InterfaceProperties::new(
                    first.logical_mtu(),
                    InterfaceConfigId::new(22),
                    first.advertised_bitrate(),
                    first.cost(),
                ),
                true,
            )
            .expect("reconfiguration must advance generation");

        let failure = match router.try_receive_ingress() {
            Err(failure) => failure,
            Ok(_) => panic!("superseded ingress authority reached node-core"),
        };
        assert!(matches!(
            failure.reason(),
            IngressRouteError::StaleLease(InterfaceLeaseError::Stale { supplied, current: observed })
                if supplied == first.lease() && observed == current
        ));
        let packet = failure.into_packet();
        assert_eq!(packet.id(), id);
        assert_eq!(packet.as_bytes(), b"stale but owned");
        router
            .try_return_ingress_buffer(packet)
            .expect("stale provenance must not prevent buffer recycle");
        let recovered = actors[0]
            .try_receive_ingress_buffer()
            .expect("stale exact buffer must return to its actor");
        assert_eq!(recovered.id(), id);
        router
            .try_return_ingress_buffer(seal_bytes(recovered, b"recycled"))
            .expect("recovered buffer remains usable");
    }

    #[test]
    fn actor_and_router_reject_crossed_ingress_authority_and_queue() {
        let (mut router, mut actors) = fabric::<2>();
        let (first, second) = configure(&mut router, &actors);
        let first_authority = actors[0]
            .bind_ingress(first)
            .expect("first authority must bind");
        let second_authority = actors[1]
            .bind_ingress(second)
            .expect("second authority must bind");

        let second_packet = seal_bytes(
            actors[1]
                .try_receive_ingress_buffer()
                .expect("second pool must be seeded"),
            b"wrong authority",
        );
        let second_id = second_packet.id();
        let failure = actors[1]
            .try_send_ingress(first_authority, second_packet)
            .expect_err("actor must reject authority for another queue");
        assert_eq!(
            failure.reason(),
            ActorIngressSendError::ForeignAuthorityQueue {
                expected: second.lease().queue(),
                supplied: first.lease().queue(),
            }
        );
        let (_, second_packet) = failure.into_parts();
        assert_eq!(second_packet.id(), second_id);
        router
            .try_return_ingress_buffer(second_packet)
            .expect("crossed-authority packet must recycle to its own pool");

        let first_packet = seal_bytes(
            actors[0]
                .try_receive_ingress_buffer()
                .expect("first pool must be seeded"),
            b"wrong actor",
        );
        let first_id = first_packet.id();
        let failure = actors[1]
            .try_send_ingress(second_authority, first_packet)
            .expect_err("actor must reject a buffer from another queue");
        assert_eq!(
            failure.reason(),
            ActorIngressSendError::ForeignBufferQueue {
                expected: second.lease().queue(),
                supplied: first.lease().queue(),
            }
        );
        let (_, first_packet) = failure.into_parts();
        assert_eq!(first_packet.id(), first_id);
        router
            .try_return_ingress_buffer(first_packet)
            .expect("crossed-buffer packet must recycle to its own pool");

        let injected = seal_bytes(
            actors[1]
                .try_receive_ingress_buffer()
                .expect("second buffer must return"),
            b"router defense",
        );
        if actors[1]
            .completed_ingress
            .try_send(InterfaceIngress {
                authority: first_authority,
                packet: injected,
            })
            .is_err()
        {
            panic!("test-only crossed injection must fit");
        }
        let failure = match router.try_receive_ingress() {
            Err(failure) => failure,
            Ok(_) => panic!("router accepted crossed authority"),
        };
        assert_eq!(
            failure.reason(),
            IngressRouteError::ForeignAuthorityQueue {
                observed: second.lease().queue(),
                supplied: first.lease().queue(),
            }
        );
        router
            .try_return_ingress_buffer(failure.into_packet())
            .expect("router-rejected packet must retain its return origin");

        let injected = seal_bytes(
            actors[0]
                .try_receive_ingress_buffer()
                .expect("first buffer must remain reusable"),
            b"router buffer defense",
        );
        if actors[1]
            .completed_ingress
            .try_send(InterfaceIngress {
                authority: second_authority,
                packet: injected,
            })
            .is_err()
        {
            panic!("test-only crossed-buffer injection must fit");
        }
        let failure = match router.try_receive_ingress() {
            Err(failure) => failure,
            Ok(_) => panic!("router accepted crossed buffer origin"),
        };
        assert_eq!(
            failure.reason(),
            IngressRouteError::ForeignBufferQueue {
                observed: second.lease().queue(),
                supplied: first.lease().queue(),
            }
        );
        router
            .try_return_ingress_buffer(failure.into_packet())
            .expect("crossed-buffer rejection must preserve original return queue");
    }

    #[test]
    fn crossed_static_fabric_return_fails_closed_with_exact_owner() {
        let first_fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
            Box::leak(Box::new(InterfaceFabric::new()));
        let second_fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
            Box::leak(Box::new(InterfaceFabric::new()));
        let (mut first_router, [mut first_actor]) = first_fabric.split();
        let (mut second_router, [second_actor]) = second_fabric.split();
        let properties = InterfaceProperties::new(
            LogicalMtu::try_new(500).expect("MTU must be nonzero"),
            InterfaceConfigId::new(1),
            None,
            InterfaceCost::new(0),
        );
        first_router
            .register(
                first_actor.queue_id(),
                PacketInterfaceId::new(1),
                properties,
                true,
            )
            .expect("first fabric must register");
        second_router
            .register(
                second_actor.queue_id(),
                PacketInterfaceId::new(1),
                properties,
                true,
            )
            .expect("second fabric must register independently");
        let packet = seal_bytes(
            first_actor
                .try_receive_ingress_buffer()
                .expect("first fabric pool must be seeded"),
            b"foreign fabric",
        );
        let id = packet.id();
        let failure = second_router
            .try_return_ingress_buffer(packet)
            .expect_err("another fabric must reject the exact buffer owner");
        assert_eq!(
            failure.reason(),
            IngressBufferReturnError::ForeignFabricOrigin(id.queue())
        );
        let packet = failure.into_packet();
        assert_eq!(packet.id(), id);
        assert_eq!(packet.as_bytes(), b"foreign fabric");
        first_router
            .try_return_ingress_buffer(packet)
            .expect("origin router must accept the recovered exact owner");
        assert_eq!(first_actor.available_ingress_buffers(), 1);
    }

    #[test]
    fn ingress_poll_and_wait_are_pressure_and_cancellation_safe() {
        let (mut router, mut actors) = fabric::<1>();
        let (first, _) = configure(&mut router, &actors);
        let authority = actors[0]
            .bind_ingress(first)
            .expect("matching authority must bind");
        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&wake_counter));
        let mut context = Context::from_waker(&waker);

        let buffer = actors[0]
            .try_receive_ingress_buffer()
            .expect("pool must be seeded");
        let id = buffer.id();
        let mut wait_for_buffer = Box::pin(actors[0].receive_ingress_buffer());
        assert!(matches!(
            wait_for_buffer.as_mut().poll(&mut context),
            Poll::Pending
        ));
        drop(wait_for_buffer);

        let mut wait_for_ingress = Box::pin(router.receive_ingress());
        assert!(matches!(
            wait_for_ingress.as_mut().poll(&mut context),
            Poll::Pending
        ));
        actors[0]
            .try_send_ingress(authority, seal_bytes(buffer, b"wake router"))
            .expect("completed packet must wake router wait");
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        drop(wait_for_ingress);
        let packet = router
            .try_receive_ingress()
            .expect("cancelled wait must leave exact packet queued")
            .expect("packet must remain queued")
            .into_packet();
        assert_eq!(packet.id(), id);
        router
            .try_return_ingress_buffer(packet)
            .expect("packet must return after cancelled router wait");
        let recovered = actors[0]
            .try_receive_ingress_buffer()
            .expect("cancelled actor wait must not reserve returned buffer");
        assert_eq!(recovered.id(), id);

        actors[0]
            .try_send_ingress(authority, seal_bytes(recovered, b"fill queue"))
            .expect("completed queue must accept its sole packet");
        let mut wait_for_capacity = Box::pin(actors[0].wait_ingress_send_capacity());
        assert!(matches!(
            wait_for_capacity.as_mut().poll(&mut context),
            Poll::Pending
        ));
        let packet = router
            .try_receive_ingress()
            .expect("router must release completed-queue pressure")
            .expect("pressure packet must exist")
            .into_packet();
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        drop(wait_for_capacity);
        router
            .try_return_ingress_buffer(packet)
            .expect("capacity wait cancellation must not affect buffer return");
        assert_eq!(actors[0].available_ingress_buffers(), 1);
    }

    #[test]
    fn completed_ingress_remains_valid_after_online_to_offline_transition() {
        let (mut router, mut actors) = fabric::<1>();
        let (first, _) = configure(&mut router, &actors);
        let authority = actors[0]
            .bind_ingress(first)
            .expect("matching authority must bind");
        let buffer = actors[0]
            .try_receive_ingress_buffer()
            .expect("pool must be seeded");
        actors[0]
            .try_send_ingress(authority, seal_bytes(buffer, b"already received"))
            .expect("completed RX must queue while online");
        let offline = router
            .set_online(first.lease(), false)
            .expect("current lease must transition offline");
        let validated = router
            .try_receive_ingress()
            .expect("offline transition must not stale completed RX")
            .expect("completed packet must remain queued");
        assert_eq!(validated.descriptor(), offline);
        assert!(!validated.descriptor().is_online());
        let packet = validated.into_packet();
        assert_eq!(packet.as_bytes(), b"already received");
        router
            .try_return_ingress_buffer(packet)
            .expect("offline ingress buffer must recycle normally");
    }

    #[test]
    fn ingress_seal_enforces_nonzero_native_packet_capacity() {
        let (mut router, mut actors) = fabric::<1>();
        configure(&mut router, &actors);
        let buffer = actors[0]
            .try_receive_ingress_buffer()
            .expect("pool must be seeded");
        let id = buffer.id();
        let failure = match buffer.seal(0) {
            Err(failure) => failure,
            Ok(_) => panic!("zero-length native packet was accepted"),
        };
        assert_eq!(failure.reason(), IngressPacketLengthError::Zero);
        let buffer = failure.into_buffer();
        assert_eq!(buffer.id(), id);
        let failure = match buffer.seal(PACKET_CAPACITY + 1) {
            Err(failure) => failure,
            Ok(_) => panic!("oversized native packet was accepted"),
        };
        assert_eq!(
            failure.reason(),
            IngressPacketLengthError::TooLong {
                actual: PACKET_CAPACITY + 1,
                maximum: PACKET_CAPACITY,
            }
        );
        let mut buffer = failure.into_buffer();
        buffer.capacity_mut().fill(0x5a);
        let packet = buffer
            .seal(PACKET_CAPACITY)
            .expect("full native packet capacity must be valid");
        assert_eq!(packet.len(), PACKET_CAPACITY);
        assert!(!packet.is_empty());
        assert!(packet.as_bytes().iter().all(|byte| *byte == 0x5a));
        router
            .try_return_ingress_buffer(packet)
            .expect("maximum-length exact buffer must recycle");
    }

    #[test]
    fn actor_rejects_packet_above_bound_logical_mtu_with_exact_owner() {
        let fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
            Box::leak(Box::new(InterfaceFabric::new()));
        let (mut router, [actor]) = fabric.split();
        let queue = actor.queue_id();
        let descriptor = router
            .register(
                queue,
                PacketInterfaceId::new(1),
                InterfaceProperties::new(
                    LogicalMtu::try_new(8).expect("test MTU must be nonzero"),
                    InterfaceConfigId::new(1),
                    None,
                    InterfaceCost::new(0),
                ),
                true,
            )
            .expect("test interface must register");
        let (_tx, mut ingress_actor, _lifecycle) = actor.into_parts();
        let authority = ingress_actor
            .bind_ingress(descriptor)
            .expect("same-queue descriptor must bind");
        let packet = seal_bytes(
            ingress_actor
                .try_receive_buffer()
                .expect("ingress pool must be seeded"),
            b"ninebytes",
        );
        let id = packet.id();

        let failure = ingress_actor
            .try_send(authority, packet)
            .expect_err("actor must enforce its bound logical MTU");
        assert_eq!(
            failure.reason(),
            ActorIngressSendError::PacketExceedsBoundMtu {
                actual: 9,
                maximum: LogicalMtu::try_new(8).expect("test MTU must be nonzero"),
            }
        );
        let (returned_authority, packet) = failure.into_parts();
        assert_eq!(returned_authority, authority);
        assert_eq!(packet.id(), id);
        assert_eq!(packet.as_bytes(), b"ninebytes");
        assert!(
            router
                .try_receive_ingress()
                .expect("empty ingress scan must succeed")
                .is_none()
        );
        router
            .try_return_ingress_buffer(packet)
            .expect("actor-rejected exact buffer must recycle");
        assert_eq!(ingress_actor.available_buffers(), 1);
    }

    #[test]
    fn router_rechecks_current_logical_mtu_and_retains_exact_owner() {
        let current_fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
            Box::leak(Box::new(InterfaceFabric::new()));
        let authority_fabric: &'static mut InterfaceFabric<NoopRawMutex, 1, 1> =
            Box::leak(Box::new(InterfaceFabric::new()));
        let (mut current_router, [mut current_actor]) = current_fabric.split();
        let (mut authority_router, [authority_actor]) = authority_fabric.split();
        let current_descriptor = current_router
            .register(
                current_actor.queue_id(),
                PacketInterfaceId::new(1),
                InterfaceProperties::new(
                    LogicalMtu::try_new(8).expect("test MTU must be nonzero"),
                    InterfaceConfigId::new(1),
                    None,
                    InterfaceCost::new(0),
                ),
                true,
            )
            .expect("current interface must register");
        let foreign_descriptor = authority_router
            .register(
                authority_actor.queue_id(),
                PacketInterfaceId::new(1),
                InterfaceProperties::new(
                    LogicalMtu::try_new(500).expect("test MTU must be nonzero"),
                    InterfaceConfigId::new(99),
                    None,
                    InterfaceCost::new(0),
                ),
                true,
            )
            .expect("authority fixture must register");
        assert_eq!(foreign_descriptor.lease(), current_descriptor.lease());
        let authority = current_actor
            .bind_ingress(foreign_descriptor)
            .expect("same lease-shaped descriptor must bind to the queue");
        let packet = seal_bytes(
            current_actor
                .try_receive_ingress_buffer()
                .expect("current pool must be seeded"),
            b"ninebytes",
        );
        let id = packet.id();
        current_actor
            .try_send_ingress(authority, packet)
            .expect("actor-bound larger MTU admits the defense-in-depth fixture");

        let failure = match current_router.try_receive_ingress() {
            Err(failure) => failure,
            Ok(_) => panic!("router accepted a packet above its authoritative current MTU"),
        };
        assert_eq!(
            failure.reason(),
            IngressRouteError::PacketExceedsCurrentMtu {
                actual: 9,
                maximum: LogicalMtu::try_new(8).expect("test MTU must be nonzero"),
            }
        );
        let packet = failure.into_packet();
        assert_eq!(packet.id(), id);
        assert_eq!(packet.as_bytes(), b"ninebytes");
        current_router
            .try_return_ingress_buffer(packet)
            .expect("router-rejected exact buffer must recycle");
        assert_eq!(current_actor.available_ingress_buffers(), 1);
    }

    #[test]
    fn ingress_receive_scan_is_fair_across_two_slots() {
        let (mut router, mut actors) = fabric::<2>();
        let (first, second) = configure(&mut router, &actors);
        let first_authority = actors[0]
            .bind_ingress(first)
            .expect("first authority must bind");
        let second_authority = actors[1]
            .bind_ingress(second)
            .expect("second authority must bind");
        let first_a = actors[0]
            .try_receive_ingress_buffer()
            .expect("first pool must have buffer A");
        actors[0]
            .try_send_ingress(first_authority, seal_bytes(first_a, b"first-a"))
            .expect("first A must queue");
        let second_a = actors[1]
            .try_receive_ingress_buffer()
            .expect("second pool must have buffer A");
        actors[1]
            .try_send_ingress(second_authority, seal_bytes(second_a, b"second-a"))
            .expect("second A must queue");

        let first_packet = router
            .try_receive_ingress()
            .expect("first scan must validate")
            .expect("first scan must find a packet");
        assert_eq!(first_packet.interface(), first.lease().interface());
        let first_b = actors[0]
            .try_receive_ingress_buffer()
            .expect("first pool must have buffer B");
        actors[0]
            .try_send_ingress(first_authority, seal_bytes(first_b, b"first-b"))
            .expect("first B must queue behind first scan");

        let second_packet = router
            .try_receive_ingress()
            .expect("second scan must validate")
            .expect("second scan must find a packet");
        assert_eq!(second_packet.interface(), second.lease().interface());
        let second_a = second_packet.into_packet();
        assert_eq!(second_a.as_bytes(), b"second-a");
        let next_first = router
            .try_receive_ingress()
            .expect("third scan must validate")
            .expect("third scan must find first B");
        assert_eq!(next_first.interface(), first.lease().interface());
        let first_a = first_packet.into_packet();
        let first_b = next_first.into_packet();
        router
            .try_return_ingress_buffer(first_a)
            .expect("first A must return");
        router
            .try_return_ingress_buffer(first_b)
            .expect("first B must return");
        router
            .try_return_ingress_buffer(second_a)
            .expect("second A must return");
    }

    #[test]
    fn lifecycle_requests_apply_ready_and_offline_under_the_same_generation() {
        let (mut router, actors) = fabric::<1>();
        let (first, second) = configure(&mut router, &actors);
        let [first_actor, second_actor] = actors;
        router
            .set_online(first.lease(), false)
            .expect("first current lease must transition offline");
        router
            .set_online(second.lease(), false)
            .expect("second current lease must transition offline");
        let (_, _, mut first_lifecycle) = first_actor.into_parts();
        let (_, _, mut second_lifecycle) = second_actor.into_parts();

        let first_request = first_lifecycle
            .try_request_state(first.lease(), InterfaceLifecycleState::Ready)
            .expect("first actor request queue must be empty");
        let second_request = second_lifecycle
            .try_request_state(second.lease(), InterfaceLifecycleState::Ready)
            .expect("second actor request queue must be empty");

        let first_transition = router
            .try_process_lifecycle()
            .expect("first fair lifecycle pass must progress");
        assert_eq!(first_transition.queue(), first.lease().queue());
        assert_eq!(first_transition.acknowledgement().request(), first_request);
        assert!(
            first_transition
                .acknowledgement()
                .result()
                .expect("first ready request must apply")
                .is_online()
        );
        assert_eq!(
            first_lifecycle
                .try_finish_request()
                .expect("first acknowledgement must match")
                .expect("first acknowledgement must be retained"),
            first_transition
                .acknowledgement()
                .result()
                .expect("first transition already applied")
        );

        let second_transition = router
            .try_process_lifecycle()
            .expect("cursor must advance to the second actor");
        assert_eq!(second_transition.queue(), second.lease().queue());
        assert_eq!(
            second_transition.acknowledgement().request(),
            second_request
        );
        assert_eq!(
            second_lifecycle
                .try_finish_request()
                .expect("second acknowledgement must match")
                .expect("second acknowledgement must be retained"),
            second_transition
                .acknowledgement()
                .result()
                .expect("second transition already applied")
        );

        let offline_request = first_lifecycle
            .try_request_state(first.lease(), InterfaceLifecycleState::Offline)
            .expect("acknowledged actor can request another transition");
        let offline_transition = router
            .try_process_lifecycle()
            .expect("offline request must progress");
        assert_eq!(
            offline_transition.acknowledgement().request(),
            offline_request
        );
        assert!(
            !offline_transition
                .acknowledgement()
                .result()
                .expect("current offline request must apply")
                .is_online()
        );
        assert!(
            !first_lifecycle
                .try_finish_request()
                .expect("offline acknowledgement must match")
                .expect("offline acknowledgement must be retained")
                .is_online()
        );
        assert!(
            router
                .registry()
                .descriptor(second.lease().interface())
                .expect("second interface remains registered")
                .is_online()
        );
    }

    #[test]
    fn lifecycle_requests_reject_crossed_queues_and_stale_generations() {
        let (mut router, actors) = fabric::<1>();
        let (first, second) = configure(&mut router, &actors);
        let [first_actor, second_actor] = actors;
        let (_, _, mut first_lifecycle) = first_actor.into_parts();
        let (_, _, _second_lifecycle) = second_actor.into_parts();

        assert_eq!(
            first_lifecycle.try_request_state(second.lease(), InterfaceLifecycleState::Offline,),
            Err(InterfaceLifecycleTryRequestError::ForeignQueue {
                actor: first.lease().queue(),
                supplied: second.lease().queue(),
            })
        );

        // Inject a malformed envelope behind the public actor guard to prove
        // the router independently treats its observed channel as authority.
        let crossed = InterfaceLifecycleRequest {
            lease: second.lease(),
            state: InterfaceLifecycleState::Offline,
        };
        first_lifecycle
            .requests
            .try_send(crossed)
            .expect("test injection queue must be empty");
        let unsent = InterfaceLifecycleRequest {
            lease: first.lease(),
            state: InterfaceLifecycleState::Ready,
        };
        assert_eq!(
            first_lifecycle.try_request_state(first.lease(), InterfaceLifecycleState::Ready),
            Err(InterfaceLifecycleTryRequestError::RequestQueueFull(unsent))
        );
        let crossed_transition = router
            .try_process_lifecycle()
            .expect("crossed request must be acknowledged as rejected");
        assert_eq!(
            crossed_transition.acknowledgement().result(),
            Err(InterfaceLifecycleRouteError::ForeignQueue {
                observed: first.lease().queue(),
                supplied: second.lease().queue(),
            })
        );
        assert!(
            router
                .registry()
                .descriptor(second.lease().interface())
                .expect("crossed request must not remove the second interface")
                .is_online()
        );
        assert_eq!(
            first_lifecycle.try_finish_request(),
            Err(InterfaceLifecycleActorError::UnexpectedAcknowledgement(
                crossed
            ))
        );

        let replacement = router
            .reconfigure(first.lease(), first.properties(), false)
            .expect("current first lease must reconfigure");
        first_lifecycle
            .try_request_state(first.lease(), InterfaceLifecycleState::Ready)
            .expect("stale request still names the actor's fixed queue");
        let stale_transition = router
            .try_process_lifecycle()
            .expect("stale request must be acknowledged as rejected");
        assert!(matches!(
            stale_transition.acknowledgement().result(),
            Err(InterfaceLifecycleRouteError::InvalidLease(
                InterfaceLeaseError::Stale { supplied, current }
            )) if supplied == first.lease() && current == replacement
        ));
        assert!(
            !router
                .registry()
                .descriptor(first.lease().interface())
                .expect("replacement remains registered")
                .is_online()
        );
        assert!(matches!(
            first_lifecycle.try_finish_request(),
            Err(InterfaceLifecycleActorError::Rejected(
                InterfaceLifecycleRouteError::InvalidLease(InterfaceLeaseError::Stale {
                    supplied,
                    current,
                })
            )) if supplied == first.lease() && current == replacement
        ));
    }

    #[test]
    fn lifecycle_acknowledgement_pressure_retains_the_next_request() {
        let (mut router, actors) = fabric::<1>();
        let (first, _) = configure(&mut router, &actors);
        let [first_actor, _second_actor] = actors;
        let (_, _, mut lifecycle) = first_actor.into_parts();

        let first_request = lifecycle
            .try_request_state(first.lease(), InterfaceLifecycleState::Offline)
            .expect("first request must enqueue");
        router
            .try_process_lifecycle()
            .expect("first request must be applied and acknowledged");
        let second_request = InterfaceLifecycleRequest {
            lease: first.lease(),
            state: InterfaceLifecycleState::Ready,
        };
        assert_eq!(
            lifecycle.try_request_state(first.lease(), InterfaceLifecycleState::Ready),
            Err(InterfaceLifecycleTryRequestError::ExchangePending {
                pending: first_request,
                unsent: second_request,
            })
        );
        lifecycle
            .requests
            .try_send(second_request)
            .expect("private injection isolates router acknowledgement pressure");

        assert_eq!(router.try_process_lifecycle(), None);
        assert!(
            !router
                .registry()
                .descriptor(first.lease().interface())
                .expect("first interface remains registered")
                .is_online()
        );
        assert!(
            !lifecycle
                .try_finish_request()
                .expect("first acknowledgement must match")
                .expect("first acknowledgement remains exact")
                .is_online()
        );
        assert!(
            router
                .try_process_lifecycle()
                .expect("retained second request progresses after ack capacity returns")
                .acknowledgement()
                .result()
                .expect("second request is current")
                .is_online()
        );
        assert_eq!(
            lifecycle.try_finish_request(),
            Err(InterfaceLifecycleActorError::UnexpectedAcknowledgement(
                second_request
            ))
        );
    }

    #[test]
    fn crossed_acknowledgement_preserves_the_pending_single_flight_exchange() {
        let (mut router, actors) = fabric::<1>();
        let (first, _) = configure(&mut router, &actors);
        let [first_actor, _second_actor] = actors;
        let (_, _, mut lifecycle) = first_actor.into_parts();
        let pending = lifecycle
            .try_request_state(first.lease(), InterfaceLifecycleState::Offline)
            .expect("first request must enqueue");
        let crossed = InterfaceLifecycleRequest {
            lease: first.lease(),
            state: InterfaceLifecycleState::Ready,
        };
        lifecycle
            .acknowledgements
            .try_send(InterfaceLifecycleAcknowledgement {
                request: crossed,
                result: Ok(first),
            })
            .expect("private crossed acknowledgement injection must fit");

        assert_eq!(
            lifecycle.try_finish_request(),
            Err(InterfaceLifecycleActorError::CrossedAcknowledgement {
                expected: pending,
                received: crossed,
            })
        );
        assert_eq!(
            lifecycle.try_request_state(first.lease(), InterfaceLifecycleState::Ready),
            Err(InterfaceLifecycleTryRequestError::ExchangePending {
                pending,
                unsent: crossed,
            })
        );

        let transition = router
            .try_process_lifecycle()
            .expect("original pending request must still progress");
        assert_eq!(transition.acknowledgement().request(), pending);
        assert!(
            !lifecycle
                .try_finish_request()
                .expect("original acknowledgement must correlate")
                .expect("original acknowledgement must be retained")
                .is_online()
        );
        lifecycle
            .try_request_state(first.lease(), InterfaceLifecycleState::Ready)
            .expect("exact completion must reopen the single-flight phase");
    }

    #[test]
    fn cancelled_async_wait_can_resume_the_exact_single_flight_exchange() {
        let (mut router, actors) = fabric::<1>();
        let (first, _) = configure(&mut router, &actors);
        let [first_actor, _second_actor] = actors;
        let (_, _, mut lifecycle) = first_actor.into_parts();
        assert_eq!(
            lifecycle.try_finish_request(),
            Err(InterfaceLifecycleActorError::NoPendingRequest)
        );
        let pending = InterfaceLifecycleRequest {
            lease: first.lease(),
            state: InterfaceLifecycleState::Offline,
        };
        let unsent = InterfaceLifecycleRequest {
            lease: first.lease(),
            state: InterfaceLifecycleState::Ready,
        };

        let mut future =
            Box::pin(lifecycle.request_state(first.lease(), InterfaceLifecycleState::Offline));
        let mut context = core::task::Context::from_waker(core::task::Waker::noop());
        assert!(matches!(
            core::future::Future::poll(future.as_mut(), &mut context),
            core::task::Poll::Pending
        ));
        let transition = router
            .try_process_lifecycle()
            .expect("polled request must be visible to the router");
        assert_eq!(transition.acknowledgement().request(), pending);
        drop(future);

        assert_eq!(
            lifecycle.try_request_state(first.lease(), InterfaceLifecycleState::Ready),
            Err(InterfaceLifecycleTryRequestError::ExchangePending { pending, unsent })
        );
        assert!(
            !lifecycle
                .try_finish_request()
                .expect("cancelled wait must preserve exact correlation")
                .expect("router acknowledgement must remain retained")
                .is_online()
        );
        lifecycle
            .try_request_state(first.lease(), InterfaceLifecycleState::Ready)
            .expect("resumed completion must reopen the single-flight phase");
    }

    #[test]
    fn registry_rejects_duplicate_ids_and_preserves_generation_across_vacancy() {
        let mut registry = InterfaceRegistry::<2>::new();
        let mtu = LogicalMtu::try_new(500).expect("MTU must be nonzero");
        let first = registry
            .register(
                InterfaceQueueId::new(0),
                PacketInterfaceId::new(7),
                InterfaceProperties::new(
                    mtu,
                    InterfaceConfigId::new(1),
                    None,
                    InterfaceCost::new(0),
                ),
                true,
            )
            .expect("first registration must fit");
        assert_eq!(
            registry.register(
                InterfaceQueueId::new(1),
                PacketInterfaceId::new(7),
                InterfaceProperties::new(
                    mtu,
                    InterfaceConfigId::new(2),
                    None,
                    InterfaceCost::new(1),
                ),
                true,
            ),
            Err(InterfaceRegistrationError::DuplicateInterface {
                interface: PacketInterfaceId::new(7),
                queue: InterfaceQueueId::new(0),
            })
        );
        registry
            .unregister(first.lease())
            .expect("current lease must unregister");
        let replacement = registry
            .register(
                InterfaceQueueId::new(0),
                PacketInterfaceId::new(8),
                InterfaceProperties::new(
                    mtu,
                    InterfaceConfigId::new(3),
                    None,
                    InterfaceCost::new(2),
                ),
                true,
            )
            .expect("vacant queue must re-register");
        assert!(replacement.lease().generation() > first.lease().generation());
        assert!(matches!(
            registry.validate(first.lease()),
            Err(InterfaceLeaseError::Stale { .. })
        ));

        let mut outside = InterfaceRegistry::<1>::new();
        let outside_descriptor = outside
            .register(
                InterfaceQueueId::new(0),
                PacketInterfaceId::new(64),
                InterfaceProperties::new(
                    mtu,
                    InterfaceConfigId::new(4),
                    None,
                    InterfaceCost::new(3),
                ),
                false,
            )
            .expect("registry can represent native one-byte interface IDs");
        assert_eq!(
            outside.eligible_interfaces(),
            Err(EligibleInterfaceSetError::InterfaceOutsideProfile(
                outside_descriptor
            ))
        );
    }
}
