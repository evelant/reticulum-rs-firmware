use super::*;

/// Immutable registry snapshot stamped onto one accepted outbound owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceDispatchContext {
    pub(crate) descriptor: InterfaceDescriptor,
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
    pub(crate) descriptor: InterfaceDescriptor,
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
pub(crate) struct InterfaceIngress {
    pub(crate) authority: InterfaceIngressAuthority,
    pub(crate) packet: SealedIngressPacket,
}

impl InterfaceIngress {
    pub(crate) const fn lease(&self) -> InterfaceLease {
        self.authority.descriptor.lease
    }
}

/// Ingress packet whose actor lease matches the authoritative registry.
#[must_use = "validated ingress must be handed to the sole node owner"]
pub struct ValidatedIngress {
    pub(crate) descriptor: InterfaceDescriptor,
    pub(crate) packet: SealedIngressPacket,
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
    pub(crate) queue: InterfaceQueueId,
    pub(crate) slot: usize,
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

pub(crate) struct IngressBufferStorage {
    bytes: [u8; PACKET_CAPACITY],
}

impl IngressBufferStorage {
    pub(crate) const fn new() -> Self {
        Self {
            bytes: [0; PACKET_CAPACITY],
        }
    }
}

// Non-zero-sized so distinct static fabric instances have distinct addresses.
pub(crate) struct IngressFabricOrigin {
    _marker: u8,
}

impl IngressFabricOrigin {
    pub(crate) const fn new() -> Self {
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
    pub(crate) id: IngressBufferId,
    pub(crate) origin: &'static IngressFabricOrigin,
    pub(crate) storage: &'static mut IngressBufferStorage,
}

impl AvailableIngressBuffer {
    pub(crate) fn new(
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
    pub(crate) id: IngressBufferId,
    pub(crate) origin: &'static IngressFabricOrigin,
    pub(crate) storage: &'static mut IngressBufferStorage,
    pub(crate) packet_len: usize,
    pub(crate) signal: Option<IngressSignalObservation>,
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

    pub(crate) fn recycle(self) -> AvailableIngressBuffer {
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
