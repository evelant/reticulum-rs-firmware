//! Fixed external ownership for ordinary protocol-action packets.
//!
//! This pool is deliberately separate from the destination-DATA dispatch and
//! receipt ledger in the crate root. It stages already-created Rete
//! [`NodeActions`] without registering delivery attempts, and it retains no
//! RNode framing or radio-driver state.

use core::fmt;

use reticulum_rns_rete::{NodeActions, OutboundProtocolToken};

use crate::{
    InterfaceSet, MonotonicMillis, PACKET_CAPACITY, PacketInterfaceId, TxAuthorizationCandidate,
    TxAuthorizationPolicy, TxCompletionCode, TxLeaseDeadline, TxOwnerScope, TxPermitRequirements,
    TxPermitReservation, TxPolicyDecision, TxPolicyDenial, TxRouteError, TxRoutePlan, TxTarget,
};

/// Stable slot assigned to one ordinary-action packet buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OrdinaryPacketSlotId(u16);

impl OrdinaryPacketSlotId {
    /// Numeric slot index assigned by the ordinary-action owner.
    pub const fn get(self) -> u16 {
        self.0
    }

    fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Non-repeating generation assigned to one ordinary packet admission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OrdinaryPacketGeneration(u64);

impl OrdinaryPacketGeneration {
    /// Numeric owner-local generation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Copy-only identity of one exact ordinary packet dispatch generation.
///
/// The stable slot alone is insufficient because the externally allocated
/// packet buffer is reused. Carrying the generation lets a product correlate
/// router admission with the eventual interface-actor completion without
/// retaining packet bytes or depending on a concrete transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryDispatchToken {
    slot: OrdinaryPacketSlotId,
    generation: OrdinaryPacketGeneration,
}

impl OrdinaryDispatchToken {
    /// Stable external-buffer slot used by this dispatch.
    pub const fn slot_id(self) -> OrdinaryPacketSlotId {
        self.slot
    }

    /// Exact reuse generation of the stable slot.
    pub const fn generation(self) -> OrdinaryPacketGeneration {
        self.generation
    }
}

/// Interface-actor observation for one ordinary packet hop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryTransmissionOutcome {
    /// The concrete transport confirmed the complete logical packet was sent.
    Transmitted,
    /// Complete transmission was not confirmed.
    ///
    /// This includes pre-authorization rejection, expiry, cancellation,
    /// partial/faulted transmission, and recovery paths. It deliberately does
    /// not claim that zero bytes reached a physical transport.
    NotConfirmed,
}

/// Scalar metadata bound to one ordinary protocol-action packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryPreparedPacket {
    scope: TxOwnerScope,
    slot: OrdinaryPacketSlotId,
    generation: OrdinaryPacketGeneration,
    packet_len: u16,
    target: TxTarget,
    interface: PacketInterfaceId,
    remaining_interfaces: InterfaceSet,
    protocol_token: Option<OutboundProtocolToken>,
    deadline: TxLeaseDeadline,
}

impl OrdinaryPreparedPacket {
    /// Stable external-buffer slot containing the packet.
    pub const fn slot_id(self) -> OrdinaryPacketSlotId {
        self.slot
    }

    /// Generation that distinguishes this use of the stable slot.
    pub const fn generation(self) -> OrdinaryPacketGeneration {
        self.generation
    }

    /// Exact transport-neutral token for this packet-buffer generation.
    pub const fn dispatch_token(self) -> OrdinaryDispatchToken {
        OrdinaryDispatchToken {
            slot: self.slot,
            generation: self.generation,
        }
    }

    /// Complete native Reticulum packet length.
    pub const fn packet_len(self) -> u16 {
        self.packet_len
    }

    /// Exact interface target carried by the native protocol action.
    pub const fn target(self) -> TxTarget {
        self.target
    }

    /// Interface selected for the current serialized fan-out hop.
    pub const fn interface(self) -> PacketInterfaceId {
        self.interface
    }

    /// Interfaces still awaiting a serialized fan-out attempt.
    pub const fn remaining_interfaces(self) -> InterfaceSet {
        self.remaining_interfaces
    }

    /// Opaque LINKREQUEST or LRPROOF dispatch marker, when present.
    pub const fn protocol_token(self) -> Option<OutboundProtocolToken> {
        self.protocol_token
    }

    /// Monotonic deadline for this complete packet owner.
    pub const fn deadline(self) -> TxLeaseDeadline {
        self.deadline
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinaryBufferBinding {
    Unregistered,
    Available {
        scope: TxOwnerScope,
        slot: OrdinaryPacketSlotId,
    },
    Bound(OrdinaryPreparedPacket),
}

/// Caller-owned storage for one complete ordinary Reticulum packet.
///
/// The value is intentionally neither `Copy` nor `Clone`. Registration binds
/// it to one ordinary-action owner; admission then moves a unique mutable
/// borrow through [`OrdinaryTxJob`] until the route completes or expires.
pub struct OrdinaryPacketBuffer {
    bytes: [u8; PACKET_CAPACITY],
    binding: OrdinaryBufferBinding,
}

impl OrdinaryPacketBuffer {
    /// Construct one zero-filled, unregistered ordinary packet buffer.
    pub const fn new() -> Self {
        Self {
            bytes: [0; PACKET_CAPACITY],
            binding: OrdinaryBufferBinding::Unregistered,
        }
    }

    /// Stable slot assigned during registration, if any.
    pub const fn slot_id(&self) -> Option<OrdinaryPacketSlotId> {
        match self.binding {
            OrdinaryBufferBinding::Unregistered => None,
            OrdinaryBufferBinding::Available { slot, .. } => Some(slot),
            OrdinaryBufferBinding::Bound(packet) => Some(packet.slot),
        }
    }
}

impl Default for OrdinaryPacketBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Move-based parking for registered ordinary packet-buffer owners.
///
/// Each entry is indexed by the buffer's stable [`OrdinaryPacketSlotId`]. A
/// successful admission moves selected mutable references out of this value;
/// a terminal completion can later move the exact reference back with
/// [`Self::park_return`]. The lifetime of an admitted job is therefore the
/// lifetime of the parked buffer, not the short mutable borrow of this pool.
/// This is the required storage shape for permanent machines using statically
/// allocated packet buffers.
pub struct OrdinaryBufferPool<'buf, const PACKET_BUFFERS: usize> {
    parked: [Option<&'buf mut OrdinaryPacketBuffer>; PACKET_BUFFERS],
}

impl<'buf, const PACKET_BUFFERS: usize> OrdinaryBufferPool<'buf, PACKET_BUFFERS> {
    /// Construct an empty parking table.
    pub const fn new() -> Self {
        Self {
            parked: [const { None }; PACKET_BUFFERS],
        }
    }

    /// Number of exact packet-buffer owners currently parked for admission.
    pub fn parked_count(&self) -> usize {
        self.parked.iter().filter(|buffer| buffer.is_some()).count()
    }

    /// Whether no packet-buffer owner is currently parked.
    pub fn is_empty(&self) -> bool {
        self.parked.iter().all(Option::is_none)
    }

    /// Whether the stable slot currently contains a parked owner.
    pub fn contains_slot(&self, slot: OrdinaryPacketSlotId) -> bool {
        self.parked.get(slot.index()).is_some_and(Option::is_some)
    }

    fn validate_vacant_slot(
        &self,
        slot: OrdinaryPacketSlotId,
    ) -> Result<(), OrdinaryBufferPoolError> {
        let Some(parked) = self.parked.get(slot.index()) else {
            return Err(OrdinaryBufferPoolError::SlotOutsidePool {
                slot,
                limit: PACKET_BUFFERS,
            });
        };
        if parked.is_some() {
            return Err(OrdinaryBufferPoolError::SlotOccupied { slot });
        }
        Ok(())
    }
}

impl<'buf, const PACKET_BUFFERS: usize> Default for OrdinaryBufferPool<'buf, PACKET_BUFFERS> {
    fn default() -> Self {
        Self::new()
    }
}

/// Why an exact ordinary buffer owner could not be parked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryBufferPoolError {
    /// The buffer was never registered with an ordinary-action owner.
    Unregistered,
    /// The buffer remains bound to a live or abandoned packet generation.
    Busy,
    /// The buffer's stable slot does not fit this parking table.
    SlotOutsidePool {
        /// Stable slot carried by the supplied buffer owner.
        slot: OrdinaryPacketSlotId,
        /// Number of slots in this parking table.
        limit: usize,
    },
    /// Another exact owner is already parked at the supplied stable slot.
    SlotOccupied {
        /// Stable slot that is already occupied.
        slot: OrdinaryPacketSlotId,
    },
    /// The return metadata and the available buffer binding disagree.
    MetadataInvariant,
}

/// Failure to register an ordinary packet buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryBufferRegistrationError {
    /// Every configured ordinary slot is already registered.
    PoolFull {
        /// Configured ordinary-buffer limit.
        limit: usize,
    },
    /// The buffer is already registered or remains bound to a packet.
    AlreadyRegistered,
}

/// Why registration and initial parking could not complete atomically.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryRegisterAndParkError {
    /// The buffer could not be registered with this ordinary-action owner.
    Registration(OrdinaryBufferRegistrationError),
    /// The stable owner slot was not vacant in the supplied parking table.
    Parking(OrdinaryBufferPoolError),
}

/// Failed registration retaining the exact supplied packet-buffer owner.
#[must_use = "failed registration retains the supplied ordinary packet buffer"]
pub struct OrdinaryRegisterAndParkFailure<'buf> {
    reason: OrdinaryRegisterAndParkError,
    buffer: &'buf mut OrdinaryPacketBuffer,
}

impl<'buf> OrdinaryRegisterAndParkFailure<'buf> {
    /// Typed reason registration and parking did not complete.
    pub const fn reason(&self) -> OrdinaryRegisterAndParkError {
        self.reason
    }

    /// Recover the unchanged packet-buffer owner.
    pub fn into_buffer(self) -> &'buf mut OrdinaryPacketBuffer {
        self.buffer
    }
}

/// Read-only validation failure for a purportedly available ordinary buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryAvailableBufferError {
    /// The buffer was never registered.
    Unregistered,
    /// The buffer belongs to a different node owner or incarnation.
    ForeignOwner,
    /// The buffer remains bound to a live or abandoned ordinary packet.
    Busy,
    /// Buffer registration and owner metadata disagree.
    MetadataInvariant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinaryDispatchPhase {
    Routed,
    Authorized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OrdinaryDispatchRecord {
    packet: OrdinaryPreparedPacket,
    phase: OrdinaryDispatchPhase,
    may_have_transmitted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinarySlotState {
    Unregistered,
    Free,
    Active(OrdinaryDispatchRecord),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OrdinarySlot {
    state: OrdinarySlotState,
}

impl OrdinarySlot {
    const UNREGISTERED: Self = Self {
        state: OrdinarySlotState::Unregistered,
    };
}

/// Caller-supplied clocks and route snapshot for one whole action batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryActionAdmissionRequest {
    /// Current monotonic packet-owner clock sample.
    pub owner_now: MonotonicMillis,
    /// Deadline shared by every packet in this synchronous action batch.
    pub deadline: TxLeaseDeadline,
    /// Synchronous snapshot of interfaces eligible for packet fan-out.
    pub enabled_interfaces: InterfaceSet,
}

/// Why a complete ordinary-action batch could not be admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryActionAdmissionError {
    /// One native packet exceeds the fixed Reticulum MTU.
    PacketTooLarge {
        /// Zero-based packet position in the original action vector.
        packet_index: usize,
        /// Supplied packet length.
        actual: usize,
        /// Fixed packet-buffer capacity.
        maximum: usize,
    },
    /// Too few registered free slots exist for the complete batch.
    PoolFull {
        /// Packet owners required for atomic admission.
        needed: usize,
        /// Registered free owners visible before admission.
        available: usize,
    },
    /// The caller did not present enough parked buffer references.
    MissingBufferOwners {
        /// Packet owners required for atomic admission.
        needed: usize,
        /// Buffer references supplied by the caller.
        supplied: usize,
    },
    /// A selected caller buffer was never registered.
    UnregisteredBuffer {
        /// Position in the supplied slice, or stable pool-table index.
        buffer_index: usize,
    },
    /// A selected caller buffer belongs to another owner/incarnation.
    ForeignBuffer {
        /// Position in the supplied slice, or stable pool-table index.
        buffer_index: usize,
    },
    /// A selected caller buffer remains bound to another packet.
    BusyBuffer {
        /// Position in the supplied slice, or stable pool-table index.
        buffer_index: usize,
    },
    /// A selected buffer and its owner slot disagree.
    BufferInvariant {
        /// Position in the supplied slice, or stable pool-table index.
        buffer_index: usize,
    },
    /// The shared owner deadline was reached before reservation.
    DeadlineExpired {
        /// Current owner-clock sample.
        now: MonotonicMillis,
        /// Supplied owner deadline.
        deadline: TxLeaseDeadline,
    },
    /// The owner-local non-repeating generation space is exhausted.
    IdentifierExhausted,
    /// One target names an interface outside the compact route profile.
    InterfaceOutsideProfile {
        /// Zero-based packet position in the original action vector.
        packet_index: usize,
        /// Complete unsupported interface identifier.
        interface: PacketInterfaceId,
    },
    /// No enabled interface satisfies one packet's exact target.
    NoEligibleInterface {
        /// Zero-based packet position in the original action vector.
        packet_index: usize,
        /// Exact target that resolved to an empty route.
        target: TxTarget,
    },
}

/// Atomic admission failure retaining the original [`NodeActions`].
#[derive(Debug)]
#[must_use = "admission failure retains every original protocol action"]
pub struct OrdinaryActionAdmissionFailure {
    reason: OrdinaryActionAdmissionError,
    actions: NodeActions,
}

impl OrdinaryActionAdmissionFailure {
    /// Typed reason no packet owner was admitted.
    pub const fn reason(&self) -> OrdinaryActionAdmissionError {
        self.reason
    }

    /// Recover the original, unchanged protocol-action envelope.
    pub fn into_actions(self) -> NodeActions {
        self.actions
    }
}

/// Unique ownership of one native ordinary Reticulum packet.
///
/// The bytes are the complete RNS packet, not an RNode frame. Dropping this
/// owner leaves its fixed slot busy so ownership loss cannot silently reuse
/// the storage.
#[must_use = "dropping an ordinary TX job leaves its buffer slot busy"]
pub struct OrdinaryTxJob<'a> {
    buffer: &'a mut OrdinaryPacketBuffer,
    packet: OrdinaryPreparedPacket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OrdinaryPermitBinding {
    scope: TxOwnerScope,
    slot: OrdinaryPacketSlotId,
    generation: OrdinaryPacketGeneration,
    interface: PacketInterfaceId,
    requirements: TxPermitRequirements,
}

impl OrdinaryPermitBinding {
    fn from_job(job: &OrdinaryTxJob<'_>, requirements: TxPermitRequirements) -> Self {
        Self {
            scope: job.packet.scope,
            slot: job.packet.slot,
            generation: job.packet.generation,
            interface: job.packet.interface,
            requirements,
        }
    }
}

impl<'a> OrdinaryTxJob<'a> {
    /// Copy all scalar packet metadata.
    pub const fn prepared(&self) -> OrdinaryPreparedPacket {
        self.packet
    }

    /// Stable external-buffer slot.
    pub const fn slot_id(&self) -> OrdinaryPacketSlotId {
        self.packet.slot
    }

    /// Owner-local packet generation.
    pub const fn generation(&self) -> OrdinaryPacketGeneration {
        self.packet.generation
    }

    /// Complete native packet length.
    pub const fn packet_len(&self) -> u16 {
        self.packet.packet_len
    }

    /// Exact native interface target.
    pub const fn target(&self) -> TxTarget {
        self.packet.target
    }

    /// Interface selected for this fan-out hop.
    pub const fn interface(&self) -> PacketInterfaceId {
        self.packet.interface
    }

    /// Packet-owner deadline.
    pub const fn deadline(&self) -> TxLeaseDeadline {
        self.packet.deadline
    }

    /// Opaque LINKREQUEST or LRPROOF dispatch marker, when present.
    pub const fn protocol_token(&self) -> Option<OutboundProtocolToken> {
        self.packet.protocol_token
    }

    /// Consume routed ownership into a permit-pending owner and scalar request.
    pub fn begin_permit(
        self,
        requirements: TxPermitRequirements,
    ) -> (OrdinaryPermitPendingTx<'a>, OrdinaryTxPermitRequest) {
        let binding = OrdinaryPermitBinding::from_job(&self, requirements);
        (
            OrdinaryPermitPendingTx {
                job: self,
                requirements,
            },
            OrdinaryTxPermitRequest { binding },
        )
    }

    /// Mark this hop definitely unpermitted without a permit exchange.
    pub fn return_unpermitted(self) -> OrdinaryUnpermittedTx<'a> {
        OrdinaryUnpermittedTx {
            job: self,
            denial: None,
        }
    }

    /// Cancel this complete ordinary route before requesting authorization.
    pub fn cancel(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self,
            kind: OrdinaryCompletionKind::Unpermitted {
                code,
                denial: None,
                stop_fanout: true,
            },
        }
    }

    /// Convert an owner/driver fault into an owning quarantine return.
    pub fn recovery_fault(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self,
            kind: OrdinaryCompletionKind::RecoveryFault(code),
        }
    }
}

/// Non-packet actions plus fixed owners admitted from one native batch.
///
/// Packet owners are yielded in their original vector order. Events and the
/// unroutable count remain in the embedded [`NodeActions`] value and must be
/// explicitly taken or retained by the caller.
#[must_use = "every packet owner, event, and unroutable count must be drained or retained"]
pub struct OrdinaryActionBatch<'a, const PACKET_BUFFERS: usize> {
    non_packet_actions: NodeActions,
    packets: [Option<OrdinaryTxJob<'a>>; PACKET_BUFFERS],
    packet_count: usize,
    next_packet: usize,
}

impl<'a, const PACKET_BUFFERS: usize> OrdinaryActionBatch<'a, PACKET_BUFFERS> {
    /// Total packet count admitted atomically from the original envelope.
    pub const fn packet_count(&self) -> usize {
        self.packet_count
    }

    /// Packet owners not yet removed from this batch.
    pub const fn remaining_packet_count(&self) -> usize {
        self.packet_count - self.next_packet
    }

    /// Borrow the retained events and unroutable count.
    ///
    /// Its packet vector is always empty after successful admission.
    pub const fn non_packet_actions(&self) -> &NodeActions {
        &self.non_packet_actions
    }

    /// Move out the retained events and unroutable count.
    ///
    /// Its packet vector is empty. A second call returns an empty envelope.
    pub fn take_non_packet_actions(&mut self) -> NodeActions {
        core::mem::take(&mut self.non_packet_actions)
    }

    /// Remove the next unique packet owner in original action-vector order.
    pub fn take_next_packet(&mut self) -> Option<OrdinaryTxJob<'a>> {
        if self.next_packet >= self.packet_count {
            return None;
        }
        let index = self.next_packet;
        self.next_packet += 1;
        self.packets[index].take()
    }
}

/// Scalar occupancy of the independent ordinary-action pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryActionCapacitySnapshot {
    /// Slots registered with this owner incarnation.
    pub registered: usize,
    /// Registered slots bound to admitted packets.
    pub active: usize,
    /// Configured ordinary packet-owner limit.
    pub limit: usize,
}

/// Non-copy scalar request for one exact ordinary packet hop.
#[must_use = "an ordinary permit request must be resolved or retained with its pending owner"]
pub struct OrdinaryTxPermitRequest {
    binding: OrdinaryPermitBinding,
}

/// Why a valid ordinary permit request was denied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryTxPermitDenialReason {
    /// Policy rejected the otherwise valid request.
    Policy(TxPolicyDenial),
    /// The packet-owner deadline expired before authorization.
    DeadlineExpired,
}

/// Non-copy grant for one exact ordinary packet hop.
#[must_use = "an ordinary permit grant must resolve its matching pending owner"]
pub struct OrdinaryTxPermitGrant {
    binding: OrdinaryPermitBinding,
    reservation: TxPermitReservation,
}

impl OrdinaryTxPermitGrant {
    /// Exact reservation atomically consumed by this grant.
    pub const fn reservation(&self) -> TxPermitReservation {
        self.reservation
    }
}

/// Non-copy denial for one exact ordinary packet hop.
#[must_use = "an ordinary permit denial must resolve its matching pending owner"]
pub struct OrdinaryTxPermitDenial {
    binding: OrdinaryPermitBinding,
    reason: OrdinaryTxPermitDenialReason,
}

impl OrdinaryTxPermitDenial {
    /// Typed denial reason.
    pub const fn reason(&self) -> OrdinaryTxPermitDenialReason {
        self.reason
    }
}

/// Non-copy reply to one exact ordinary permit request.
#[must_use = "an ordinary permit reply must resolve its matching pending owner"]
pub enum OrdinaryTxPermitReply {
    /// Authorization succeeded and is conservatively possibly transmitted.
    Granted(OrdinaryTxPermitGrant),
    /// Authorization was denied before transport transmission.
    Denied(OrdinaryTxPermitDenial),
}

impl OrdinaryTxPermitReply {
    fn binding(&self) -> OrdinaryPermitBinding {
        match self {
            Self::Granted(grant) => grant.binding,
            Self::Denied(denial) => denial.binding,
        }
    }
}

/// Scalar ordinary-request validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryAuthorizationErrorKind {
    /// Request belongs to another ordinary owner or node incarnation.
    WrongOwner,
    /// Slot, packet generation, interface, or phase is stale.
    StaleOrUnknown,
    /// Private slot and packet-buffer metadata disagree.
    Invariant,
}

/// Authorization failure retaining the non-copy ordinary request.
#[must_use = "ordinary authorization failure retains the exact request"]
pub struct OrdinaryAuthorizationFailure {
    reason: OrdinaryAuthorizationErrorKind,
    request: OrdinaryTxPermitRequest,
}

impl OrdinaryAuthorizationFailure {
    /// Typed validation failure.
    pub const fn reason(&self) -> OrdinaryAuthorizationErrorKind {
        self.reason
    }

    /// Recover the unchanged request.
    pub fn into_request(self) -> OrdinaryTxPermitRequest {
        self.request
    }
}

/// Unique ordinary packet ownership while a scalar permit reply is outstanding.
#[must_use = "pending ordinary TX ownership must resolve a reply or enter recovery"]
pub struct OrdinaryPermitPendingTx<'a> {
    job: OrdinaryTxJob<'a>,
    requirements: TxPermitRequirements,
}

impl<'a> OrdinaryPermitPendingTx<'a> {
    /// Resolve a matching grant or denial into the corresponding typestate.
    #[allow(clippy::result_large_err)]
    pub fn resolve(
        self,
        reply: OrdinaryTxPermitReply,
        now: MonotonicMillis,
    ) -> Result<OrdinaryPermitResolution<'a>, OrdinaryPermitReplyMismatch<'a>> {
        let expected = OrdinaryPermitBinding::from_job(&self.job, self.requirements);
        if reply.binding() != expected {
            return Err(OrdinaryPermitReplyMismatch {
                pending: self,
                reply,
            });
        }
        match reply {
            OrdinaryTxPermitReply::Granted(grant) if now >= self.job.packet.deadline.instant() => {
                Ok(OrdinaryPermitResolution::Expired(
                    OrdinaryExpiredAuthorizedTx {
                        job: self.job,
                        grant,
                    },
                ))
            }
            OrdinaryTxPermitReply::Granted(grant) => {
                Ok(OrdinaryPermitResolution::Authorized(OrdinaryAuthorizedTx {
                    job: self.job,
                    grant,
                    frame_taken: false,
                }))
            }
            OrdinaryTxPermitReply::Denied(denial) => Ok(OrdinaryPermitResolution::Unpermitted(
                OrdinaryUnpermittedTx {
                    job: self.job,
                    denial: Some(denial.reason),
                },
            )),
        }
    }

    /// Convert a lost or corrupt permit exchange into an owning quarantine return.
    pub fn recovery_fault(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::RecoveryFault(code),
        }
    }

    /// Cancel before send only while retaining the exact unsent request.
    ///
    /// Once the request has been moved to a channel or permit server, this
    /// proof is unavailable and the pending owner can only enter recovery.
    // The allocation-free mismatch intentionally retains unique pending
    // ownership and the exact non-Copy request; neither side may be dropped.
    #[allow(clippy::result_large_err)]
    pub fn cancel_before_send(
        self,
        request: OrdinaryTxPermitRequest,
        code: TxCompletionCode,
    ) -> Result<OrdinaryTxCompletion<'a>, OrdinaryPermitCancelMismatch<'a>> {
        let expected = OrdinaryPermitBinding::from_job(&self.job, self.requirements);
        if request.binding != expected {
            return Err(OrdinaryPermitCancelMismatch {
                pending: self,
                request,
            });
        }
        Ok(OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::Unpermitted {
                code,
                denial: None,
                stop_fanout: true,
            },
        })
    }
}

/// Failed pre-send cancellation retaining pending ownership and both requests.
#[must_use = "a cancellation mismatch retains pending ownership and the request"]
pub struct OrdinaryPermitCancelMismatch<'a> {
    pending: OrdinaryPermitPendingTx<'a>,
    request: OrdinaryTxPermitRequest,
}

impl<'a> OrdinaryPermitCancelMismatch<'a> {
    /// Recover pending ownership and the unmatched request.
    pub fn into_parts(self) -> (OrdinaryPermitPendingTx<'a>, OrdinaryTxPermitRequest) {
        (self.pending, self.request)
    }
}

/// Successful ordinary permit-reply resolution.
#[must_use = "resolved ordinary TX ownership must be completed or retained"]
pub enum OrdinaryPermitResolution<'a> {
    /// Authorized owner and its one-shot frame capability.
    Authorized(OrdinaryAuthorizedTx<'a>),
    /// Grant arrived at or after its owner deadline and cannot expose bytes.
    Expired(OrdinaryExpiredAuthorizedTx<'a>),
    /// Definitely-unpermitted owner.
    Unpermitted(OrdinaryUnpermittedTx<'a>),
}

/// Reply mismatch retaining pending ownership and the unmatched reply.
#[must_use = "ordinary permit mismatch retains pending ownership and reply"]
pub struct OrdinaryPermitReplyMismatch<'a> {
    pending: OrdinaryPermitPendingTx<'a>,
    reply: OrdinaryTxPermitReply,
}

impl<'a> OrdinaryPermitReplyMismatch<'a> {
    /// Recover pending ownership and the unchanged unmatched reply.
    pub fn into_parts(self) -> (OrdinaryPermitPendingTx<'a>, OrdinaryTxPermitReply) {
        (self.pending, self.reply)
    }
}

/// Failure to expose an authorized ordinary packet frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryFrameError {
    /// The one-shot frame capability was already consumed.
    AlreadyTaken,
    /// The owner deadline was reached before frame bytes were borrowed.
    DeadlineExpired {
        /// Deadline that prohibited byte exposure.
        deadline: TxLeaseDeadline,
    },
    /// Grant and uniquely owned ordinary buffer metadata disagree.
    Invariant,
}

/// Borrowed ordinary packet bytes exposed only by an authorized typestate.
pub struct OrdinaryTxFrame<'a> {
    bytes: &'a [u8],
    packet: OrdinaryPreparedPacket,
}

impl OrdinaryTxFrame<'_> {
    /// Complete native Reticulum packet bytes.
    pub const fn bytes(&self) -> &[u8] {
        self.bytes
    }

    /// Metadata for these exact packet bytes and selected interface.
    pub const fn prepared(&self) -> OrdinaryPreparedPacket {
        self.packet
    }
}

/// Unique ordinary packet ownership after authorization.
#[must_use = "authorized ordinary TX ownership is conservatively possibly transmitted"]
pub struct OrdinaryAuthorizedTx<'a> {
    job: OrdinaryTxJob<'a>,
    grant: OrdinaryTxPermitGrant,
    frame_taken: bool,
}

impl<'a> OrdinaryAuthorizedTx<'a> {
    /// Exact reservation atomically consumed by this authorization.
    pub const fn reservation(&self) -> TxPermitReservation {
        self.grant.reservation
    }

    /// Borrow the authorized complete ordinary packet once before its deadline.
    pub fn frame(
        &mut self,
        now: MonotonicMillis,
    ) -> Result<OrdinaryTxFrame<'_>, OrdinaryFrameError> {
        if self.frame_taken {
            return Err(OrdinaryFrameError::AlreadyTaken);
        }
        let binding = OrdinaryPermitBinding::from_job(&self.job, self.grant.binding.requirements);
        if binding != self.grant.binding
            || self.job.buffer.binding != OrdinaryBufferBinding::Bound(self.job.packet)
        {
            return Err(OrdinaryFrameError::Invariant);
        }
        if now >= self.job.packet.deadline.instant() {
            self.frame_taken = true;
            return Err(OrdinaryFrameError::DeadlineExpired {
                deadline: self.job.packet.deadline,
            });
        }
        let bytes = self
            .job
            .buffer
            .bytes
            .get(..usize::from(self.job.packet.packet_len))
            .ok_or(OrdinaryFrameError::Invariant)?;
        self.frame_taken = true;
        Ok(OrdinaryTxFrame {
            bytes,
            packet: self.job.packet,
        })
    }

    /// Complete this authorized hop without a confirmed complete transmission.
    ///
    /// Use [`Self::complete_transmitted`] only when the concrete interface
    /// actor observed successful completion of the entire logical packet.
    pub fn complete(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::Authorized {
                code,
                stop_fanout: false,
                transmission: OrdinaryTransmissionOutcome::NotConfirmed,
            },
        }
    }

    /// Complete this authorized hop after the concrete interface confirmed the
    /// entire logical packet was transmitted.
    pub fn complete_transmitted(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::Authorized {
                code,
                stop_fanout: false,
                transmission: OrdinaryTransmissionOutcome::Transmitted,
            },
        }
    }

    /// Stop later fan-out while retaining this grant's authorized classification.
    pub fn cancel(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::Authorized {
                code,
                stop_fanout: true,
                transmission: OrdinaryTransmissionOutcome::NotConfirmed,
            },
        }
    }

    /// Convert a driver fault into an owning quarantine return.
    pub fn recovery_fault(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::RecoveryFault(code),
        }
    }
}

/// Authorized ordinary owner whose delayed grant can no longer expose bytes.
#[must_use = "an expired ordinary authorized owner must return to its owner"]
pub struct OrdinaryExpiredAuthorizedTx<'a> {
    job: OrdinaryTxJob<'a>,
    grant: OrdinaryTxPermitGrant,
}

impl<'a> OrdinaryExpiredAuthorizedTx<'a> {
    /// Expired deadline that prevented frame exposure.
    pub const fn deadline(&self) -> TxLeaseDeadline {
        self.job.packet.deadline
    }

    /// Exact reservation consumed before this delayed reply was observed.
    pub const fn reservation(&self) -> TxPermitReservation {
        self.grant.reservation
    }

    /// Complete this delayed grant as possibly transmitted.
    pub fn complete(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        let _ = self.grant.binding;
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::Authorized {
                code,
                stop_fanout: false,
                transmission: OrdinaryTransmissionOutcome::NotConfirmed,
            },
        }
    }

    /// Stop later fan-out while retaining this consumed grant's classification.
    pub fn cancel(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        let _ = self.grant.binding;
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::Authorized {
                code,
                stop_fanout: true,
                transmission: OrdinaryTransmissionOutcome::NotConfirmed,
            },
        }
    }

    /// Convert a control-plane fault into an owning quarantine return.
    pub fn recovery_fault(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        let _ = self.grant.binding;
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::RecoveryFault(code),
        }
    }
}

/// Unique ordinary packet ownership for a definitely unpermitted hop.
#[must_use = "an unpermitted ordinary TX owner must return to its owner"]
pub struct OrdinaryUnpermittedTx<'a> {
    job: OrdinaryTxJob<'a>,
    denial: Option<OrdinaryTxPermitDenialReason>,
}

impl<'a> OrdinaryUnpermittedTx<'a> {
    /// Permit denial that produced this owner, if policy handling was reached.
    pub const fn denial(&self) -> Option<OrdinaryTxPermitDenialReason> {
        self.denial
    }

    /// Complete this definitely-unpermitted hop and allow later route fan-out.
    pub fn complete(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::Unpermitted {
                code,
                denial: self.denial,
                stop_fanout: false,
            },
        }
    }

    /// Cancel the complete remaining route while retaining prior TX history.
    pub fn cancel(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::Unpermitted {
                code,
                denial: self.denial,
                stop_fanout: true,
            },
        }
    }

    /// Convert a control-plane fault into an owning quarantine return.
    pub fn recovery_fault(self, code: TxCompletionCode) -> OrdinaryTxCompletion<'a> {
        OrdinaryTxCompletion {
            job: self.job,
            kind: OrdinaryCompletionKind::RecoveryFault(code),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinaryCompletionKind {
    Unpermitted {
        code: TxCompletionCode,
        denial: Option<OrdinaryTxPermitDenialReason>,
        stop_fanout: bool,
    },
    Authorized {
        code: TxCompletionCode,
        stop_fanout: bool,
        transmission: OrdinaryTransmissionOutcome,
    },
    RecoveryFault(TxCompletionCode),
}

/// Owning completion for one exact ordinary packet hop.
#[must_use = "an ordinary TX completion must return to its sole owner"]
pub struct OrdinaryTxCompletion<'a> {
    job: OrdinaryTxJob<'a>,
    kind: OrdinaryCompletionKind,
}

impl OrdinaryTxCompletion<'_> {
    /// Metadata for the exact packet owner carried by this completion.
    pub const fn prepared(&self) -> OrdinaryPreparedPacket {
        self.job.packet
    }

    /// Concrete actor's terminal transmission observation for this exact hop.
    pub const fn transmission_outcome(&self) -> OrdinaryTransmissionOutcome {
        match self.kind {
            OrdinaryCompletionKind::Authorized { transmission, .. } => transmission,
            OrdinaryCompletionKind::Unpermitted { .. }
            | OrdinaryCompletionKind::RecoveryFault(_) => OrdinaryTransmissionOutcome::NotConfirmed,
        }
    }
}

/// Why one owning ordinary completion could not be correlated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryCompletionError {
    /// The completion belongs to another ordinary owner or incarnation.
    ForeignOwner,
    /// The stable slot no longer carries this packet generation.
    StaleGeneration,
}

/// Completion validation failure retaining the unchanged completion.
#[must_use = "completion failure retains the unchanged ordinary completion"]
pub struct OrdinaryCompletionFailure<'a> {
    reason: OrdinaryCompletionError,
    completion: OrdinaryTxCompletion<'a>,
}

impl fmt::Debug for OrdinaryCompletionFailure<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrdinaryCompletionFailure")
            .field("reason", &self.reason)
            .field("packet", &self.completion.job.packet)
            .finish()
    }
}

impl<'a> OrdinaryCompletionFailure<'a> {
    /// Typed completion validation failure.
    pub const fn reason(&self) -> OrdinaryCompletionError {
        self.reason
    }

    /// Recover the unchanged owning completion.
    pub fn into_completion(self) -> OrdinaryTxCompletion<'a> {
        self.completion
    }
}

/// Why a same-owner ordinary completion was quarantined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryQuarantineReason {
    /// Authoritative slot metadata and the unique buffer owner disagree.
    MetadataInvariant,
    /// Owning completion class disagrees with the authorization phase.
    CompletionClassInvariant,
    /// Dispatcher or control-plane recovery fault after ownership transfer.
    RecoveryFault(TxCompletionCode),
}

/// Retained unique ordinary owner for a same-generation invariant or fault.
#[must_use = "an ordinary quarantine must be retained for explicit recovery"]
pub struct OrdinaryTxQuarantine<'a> {
    completion: OrdinaryTxCompletion<'a>,
    reason: OrdinaryQuarantineReason,
}

impl<'a> OrdinaryTxQuarantine<'a> {
    /// Typed reason this exact owner cannot return to the reusable pool.
    pub const fn reason(&self) -> OrdinaryQuarantineReason {
        self.reason
    }

    /// Recover the unchanged owning completion for explicit recovery storage.
    pub fn into_completion(self) -> OrdinaryTxCompletion<'a> {
        self.completion
    }
}

/// Why an ordinary packet owner returned to the available pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryPacketReturnReason {
    /// Every interface in the admission-time route snapshot was attempted.
    RouteComplete,
    /// The inclusive owner deadline stopped remaining fan-out.
    DeadlineExpired,
    /// The caller explicitly cancelled this complete remaining route.
    Cancelled,
}

/// One exact ordinary packet buffer returned to the caller-owned pool.
#[must_use = "the returned ordinary packet buffer must be parked for reuse"]
pub struct OrdinaryPacketReturn<'a> {
    buffer: &'a mut OrdinaryPacketBuffer,
    packet: OrdinaryPreparedPacket,
    reason: OrdinaryPacketReturnReason,
    may_have_transmitted: bool,
}

impl<'a> OrdinaryPacketReturn<'a> {
    /// Metadata of the final attempted or expired route position.
    pub const fn prepared(&self) -> OrdinaryPreparedPacket {
        self.packet
    }

    /// Why this exact packet owner returned instead of continuing fan-out.
    pub const fn reason(&self) -> OrdinaryPacketReturnReason {
        self.reason
    }

    /// Whether this packet generation received any earlier or current grant.
    pub const fn may_have_transmitted(&self) -> bool {
        self.may_have_transmitted
    }

    /// Recover the now-available registered packet buffer.
    pub fn into_buffer(self) -> &'a mut OrdinaryPacketBuffer {
        self.buffer
    }
}

/// Failed terminal-return parking retaining the complete owning return.
#[must_use = "failed parking retains the exact ordinary packet return"]
pub struct OrdinaryPacketReturnParkFailure<'buf> {
    reason: OrdinaryBufferPoolError,
    returned: OrdinaryPacketReturn<'buf>,
}

impl<'buf> OrdinaryPacketReturnParkFailure<'buf> {
    /// Typed reason the exact returned owner was not parked.
    pub const fn reason(&self) -> OrdinaryBufferPoolError {
        self.reason
    }

    /// Recover the complete unchanged owning return.
    pub fn into_return(self) -> OrdinaryPacketReturn<'buf> {
        self.returned
    }
}

impl<'buf, const PACKET_BUFFERS: usize> OrdinaryBufferPool<'buf, PACKET_BUFFERS> {
    /// Park one terminal owning return at its registered stable slot.
    ///
    /// Validation is non-mutating. On every error the failure retains the
    /// complete [`OrdinaryPacketReturn`], including its exact mutable buffer
    /// owner and scalar completion metadata.
    pub fn park_return(
        &mut self,
        returned: OrdinaryPacketReturn<'buf>,
    ) -> Result<OrdinaryPacketSlotId, OrdinaryPacketReturnParkFailure<'buf>> {
        let expected = returned.packet;
        let slot = match returned.buffer.binding {
            OrdinaryBufferBinding::Unregistered => {
                return Err(OrdinaryPacketReturnParkFailure {
                    reason: OrdinaryBufferPoolError::Unregistered,
                    returned,
                });
            }
            OrdinaryBufferBinding::Bound(_) => {
                return Err(OrdinaryPacketReturnParkFailure {
                    reason: OrdinaryBufferPoolError::Busy,
                    returned,
                });
            }
            OrdinaryBufferBinding::Available { scope, slot }
                if scope == expected.scope && slot == expected.slot =>
            {
                slot
            }
            OrdinaryBufferBinding::Available { .. } => {
                return Err(OrdinaryPacketReturnParkFailure {
                    reason: OrdinaryBufferPoolError::MetadataInvariant,
                    returned,
                });
            }
        };
        if let Err(reason) = self.validate_vacant_slot(slot) {
            return Err(OrdinaryPacketReturnParkFailure { reason, returned });
        }

        let OrdinaryPacketReturn { buffer, .. } = returned;
        self.parked[slot.index()] = Some(buffer);
        Ok(slot)
    }
}

/// Result of reconciling one ordinary fan-out hop.
#[must_use = "the next job or returned buffer must remain uniquely owned"]
pub enum OrdinaryCompletionDisposition<'a> {
    /// Continue the same packet generation on the next selected interface.
    Next(OrdinaryTxJob<'a>),
    /// The exact registered buffer returned to the caller-owned pool.
    Returned(OrdinaryPacketReturn<'a>),
    /// Same-owner invariant or recovery fault retaining its unique owner.
    Quarantined(OrdinaryTxQuarantine<'a>),
}

/// Independent fixed owner for allocation-backed ordinary protocol actions.
///
/// `PACKET_BUFFERS` bounds only ordinary announce, proof, forwarding, Link,
/// and Resource action packets. It neither consumes nor exposes the DATA
/// receipt/attempt capacity owned by [`crate::NodeCore`]. The 500-byte arrays
/// remain in caller-owned [`OrdinaryPacketBuffer`] values.
pub struct OrdinaryActionOwner<const PACKET_BUFFERS: usize> {
    scope: TxOwnerScope,
    slots: [OrdinarySlot; PACKET_BUFFERS],
    next_registration: usize,
    generation: u64,
}

/// Failure to issue a node incarnation's ordinary-action owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryActionOwnerClaimError {
    /// This exact node incarnation already issued its sole ordinary pool.
    AlreadyClaimed,
}

impl<const PACKET_BUFFERS: usize> OrdinaryActionOwner<PACKET_BUFFERS> {
    /// Construct only after `NodeCore` has consumed its one-shot claim.
    pub(super) const fn new(scope: TxOwnerScope) -> Self {
        const {
            assert!(
                PACKET_BUFFERS <= (u16::MAX as usize) + 1,
                "ordinary-action packet slots must fit the 16-bit namespace"
            );
        }
        Self {
            scope,
            slots: [OrdinarySlot::UNREGISTERED; PACKET_BUFFERS],
            next_registration: 0,
            generation: 0,
        }
    }

    /// Opaque node-incarnation binding carried by this ordinary owner.
    ///
    /// This copy-only value lets a permanent aggregate reject an owner from a
    /// different [`crate::NodeCore`] without exposing any mutable packet-owner
    /// state.
    pub const fn owner_scope(&self) -> TxOwnerScope {
        self.scope
    }

    /// Register one externally allocated ordinary packet buffer.
    pub fn register_packet_buffer(
        &mut self,
        buffer: &mut OrdinaryPacketBuffer,
    ) -> Result<OrdinaryPacketSlotId, OrdinaryBufferRegistrationError> {
        if !matches!(buffer.binding, OrdinaryBufferBinding::Unregistered) {
            return Err(OrdinaryBufferRegistrationError::AlreadyRegistered);
        }
        let (index, slot) = self.next_registration_slot()?;
        self.slots[index].state = OrdinarySlotState::Free;
        buffer.binding = OrdinaryBufferBinding::Available {
            scope: self.scope,
            slot,
        };
        self.next_registration = Self::next_slot_index(index);
        Ok(slot)
    }

    /// Atomically register and park one externally allocated packet buffer.
    ///
    /// The parking slot is checked before either owner metadata or the buffer
    /// binding changes. Every failure therefore retains the exact supplied
    /// mutable buffer owner and leaves its prior binding unchanged.
    pub fn register_and_park<'buf>(
        &mut self,
        pool: &mut OrdinaryBufferPool<'buf, PACKET_BUFFERS>,
        buffer: &'buf mut OrdinaryPacketBuffer,
    ) -> Result<OrdinaryPacketSlotId, OrdinaryRegisterAndParkFailure<'buf>> {
        if !matches!(buffer.binding, OrdinaryBufferBinding::Unregistered) {
            return Err(OrdinaryRegisterAndParkFailure {
                reason: OrdinaryRegisterAndParkError::Registration(
                    OrdinaryBufferRegistrationError::AlreadyRegistered,
                ),
                buffer,
            });
        }
        let (index, slot) = match self.next_registration_slot() {
            Ok(candidate) => candidate,
            Err(reason) => {
                return Err(OrdinaryRegisterAndParkFailure {
                    reason: OrdinaryRegisterAndParkError::Registration(reason),
                    buffer,
                });
            }
        };
        if let Err(reason) = pool.validate_vacant_slot(slot) {
            return Err(OrdinaryRegisterAndParkFailure {
                reason: OrdinaryRegisterAndParkError::Parking(reason),
                buffer,
            });
        }

        self.slots[index].state = OrdinarySlotState::Free;
        buffer.binding = OrdinaryBufferBinding::Available {
            scope: self.scope,
            slot,
        };
        self.next_registration = Self::next_slot_index(index);
        pool.parked[index] = Some(buffer);
        Ok(slot)
    }

    /// Validate one caller-owned buffer without changing either side.
    pub fn validate_available_buffer(
        &self,
        buffer: &OrdinaryPacketBuffer,
    ) -> Result<OrdinaryPacketSlotId, OrdinaryAvailableBufferError> {
        let (scope, slot) = match buffer.binding {
            OrdinaryBufferBinding::Unregistered => {
                return Err(OrdinaryAvailableBufferError::Unregistered);
            }
            OrdinaryBufferBinding::Available { scope, slot } => (scope, slot),
            OrdinaryBufferBinding::Bound(packet) => {
                return if packet.scope == self.scope {
                    Err(OrdinaryAvailableBufferError::Busy)
                } else {
                    Err(OrdinaryAvailableBufferError::ForeignOwner)
                };
            }
        };
        if scope != self.scope {
            return Err(OrdinaryAvailableBufferError::ForeignOwner);
        }
        if slot.index() >= PACKET_BUFFERS
            || !matches!(self.slots[slot.index()].state, OrdinarySlotState::Free)
        {
            return Err(OrdinaryAvailableBufferError::MetadataInvariant);
        }
        Ok(slot)
    }

    /// Atomically convert every packet in one native action envelope.
    ///
    /// The method validates all packet lengths and routes, the complete free
    /// capacity, every selected registered buffer, the deadline, and the full
    /// generation range before changing owner metadata or packet bytes. Any
    /// failure therefore returns the original `NodeActions` unchanged.
    /// Success performs exactly one native-`Vec`-to-fixed-array copy per
    /// packet, in vector order. The first `actions.packets.len()` entries of
    /// `buffers` are selected in caller order; later entries are untouched.
    ///
    /// This borrowed-slice form is useful for bounded, short-lived stack
    /// scopes. A permanent machine with static buffers must use
    /// [`Self::admit_from_pool`]; borrowing a static slice here would also make
    /// the slice itself unavailable for later owner recycling.
    #[allow(
        clippy::result_large_err,
        reason = "atomic admission failure must return the exact action owner without allocation"
    )]
    pub fn admit<'a>(
        &mut self,
        actions: NodeActions,
        buffers: &'a mut [&mut OrdinaryPacketBuffer],
        request: OrdinaryActionAdmissionRequest,
    ) -> Result<OrdinaryActionBatch<'a, PACKET_BUFFERS>, OrdinaryActionAdmissionFailure> {
        let packet_count = actions.packets.len();
        macro_rules! fail {
            ($reason:expr) => {
                return Err(OrdinaryActionAdmissionFailure {
                    reason: $reason,
                    actions,
                })
            };
        }

        for (packet_index, packet) in actions.packets.iter().enumerate() {
            if let Err(error) = Self::validate_packet_len(packet_index, packet.bytes().len()) {
                fail!(error);
            }
            let target = TxTarget::from_rns(packet.target());
            if let Err(error) = TxRoutePlan::resolve(target, request.enabled_interfaces) {
                fail!(Self::route_admission_error(packet_index, error));
            }
        }

        if packet_count == 0 {
            return Ok(OrdinaryActionBatch {
                non_packet_actions: actions,
                packets: core::array::from_fn(|_| None),
                packet_count: 0,
                next_packet: 0,
            });
        }

        if request.owner_now >= request.deadline.instant() {
            fail!(OrdinaryActionAdmissionError::DeadlineExpired {
                now: request.owner_now,
                deadline: request.deadline,
            });
        }

        let available = self
            .slots
            .iter()
            .filter(|slot| matches!(slot.state, OrdinarySlotState::Free))
            .count();
        if available < packet_count {
            fail!(OrdinaryActionAdmissionError::PoolFull {
                needed: packet_count,
                available,
            });
        }
        if buffers.len() < packet_count {
            fail!(OrdinaryActionAdmissionError::MissingBufferOwners {
                needed: packet_count,
                supplied: buffers.len(),
            });
        }

        let Ok(packet_count_u64) = u64::try_from(packet_count) else {
            fail!(OrdinaryActionAdmissionError::IdentifierExhausted);
        };
        let Some(final_generation) = self.generation.checked_add(packet_count_u64) else {
            fail!(OrdinaryActionAdmissionError::IdentifierExhausted);
        };

        let mut selected_slots: [Option<OrdinaryPacketSlotId>; PACKET_BUFFERS] =
            core::array::from_fn(|_| None);
        for (buffer_index, buffer) in buffers.iter().take(packet_count).enumerate() {
            let slot = match self.validate_available_buffer(buffer) {
                Ok(slot) => slot,
                Err(OrdinaryAvailableBufferError::Unregistered) => {
                    fail!(OrdinaryActionAdmissionError::UnregisteredBuffer { buffer_index });
                }
                Err(OrdinaryAvailableBufferError::ForeignOwner) => {
                    fail!(OrdinaryActionAdmissionError::ForeignBuffer { buffer_index });
                }
                Err(OrdinaryAvailableBufferError::Busy) => {
                    fail!(OrdinaryActionAdmissionError::BusyBuffer { buffer_index });
                }
                Err(OrdinaryAvailableBufferError::MetadataInvariant) => {
                    fail!(OrdinaryActionAdmissionError::BufferInvariant { buffer_index });
                }
            };
            if selected_slots[..buffer_index].contains(&Some(slot)) {
                fail!(OrdinaryActionAdmissionError::BufferInvariant { buffer_index });
            }
            selected_slots[buffer_index] = Some(slot);
        }

        let first_generation = self.generation + 1;
        let mut prepared_packets: [Option<OrdinaryPreparedPacket>; PACKET_BUFFERS] =
            core::array::from_fn(|_| None);
        for (packet_index, packet) in actions.packets.iter().enumerate() {
            let target = TxTarget::from_rns(packet.target());
            let mut route = TxRoutePlan::resolve(target, request.enabled_interfaces)
                .expect("preflight proved every ordinary route is nonempty");
            let interface = route
                .take_next()
                .expect("a successfully resolved route has a first interface");
            let slot = selected_slots[packet_index]
                .expect("preflight assigned one unique slot per packet");
            let generation = OrdinaryPacketGeneration(
                first_generation
                    + u64::try_from(packet_index)
                        .expect("packet index already converted as part of packet count"),
            );
            prepared_packets[packet_index] = Some(OrdinaryPreparedPacket {
                scope: self.scope,
                slot,
                generation,
                packet_len: u16::try_from(packet.bytes().len())
                    .expect("preflight proved an RNS-MTU packet length fits u16"),
                target,
                interface,
                remaining_interfaces: route.remaining(),
                protocol_token: packet.protocol_token(),
                deadline: request.deadline,
            });
        }

        // Reserve the complete fixed-owner batch before copying the first
        // allocation-backed native packet. Every operation after this point is
        // infallible for the preflighted inputs; an internal panic remains
        // fail-closed with all selected buffers bound rather than partly free.
        for (packet_index, buffer_owner) in buffers.iter_mut().take(packet_count).enumerate() {
            let prepared = prepared_packets[packet_index]
                .expect("preflight prepared one metadata owner per packet");
            buffer_owner.binding = OrdinaryBufferBinding::Bound(prepared);
            self.slots[prepared.slot.index()].state =
                OrdinarySlotState::Active(OrdinaryDispatchRecord {
                    packet: prepared,
                    phase: OrdinaryDispatchPhase::Routed,
                    may_have_transmitted: false,
                });
        }
        self.generation = final_generation;

        let mut non_packet_actions = actions;
        let packets = core::mem::take(&mut non_packet_actions.packets);
        let mut jobs: [Option<OrdinaryTxJob<'a>>; PACKET_BUFFERS] = core::array::from_fn(|_| None);

        for (packet_index, (packet, buffer_owner)) in
            packets.into_iter().zip(buffers.iter_mut()).enumerate()
        {
            let (bytes, native_target, protocol_token) = packet.into_parts();
            let prepared = prepared_packets[packet_index]
                .expect("reservation retained one metadata owner per packet");
            debug_assert_eq!(usize::from(prepared.packet_len), bytes.len());
            debug_assert_eq!(prepared.target, TxTarget::from_rns(native_target));
            debug_assert_eq!(prepared.protocol_token, protocol_token);
            let buffer: &'a mut OrdinaryPacketBuffer = buffer_owner;
            buffer.bytes[..bytes.len()].copy_from_slice(&bytes);
            jobs[packet_index] = Some(OrdinaryTxJob {
                buffer,
                packet: prepared,
            });
        }

        Ok(OrdinaryActionBatch {
            non_packet_actions,
            packets: jobs,
            packet_count,
            next_packet: 0,
        })
    }

    /// Atomically admit one native action envelope from move-based parking.
    ///
    /// This is the reusable permanent-machine counterpart to [`Self::admit`].
    /// Its mutable borrow of `pool` is short: successful jobs borrow the
    /// underlying buffers for `'buf`, while the pool itself is immediately
    /// available to park unrelated completions. Buffers are selected in stable
    /// slot order. Every admission check completes before any owner is removed,
    /// so an error leaves all parked pointers, owner metadata, packet bytes, and
    /// the original [`NodeActions`] unchanged.
    #[allow(
        clippy::result_large_err,
        reason = "atomic admission failure must return the exact action owner without allocation"
    )]
    pub fn admit_from_pool<'buf>(
        &mut self,
        actions: NodeActions,
        pool: &mut OrdinaryBufferPool<'buf, PACKET_BUFFERS>,
        request: OrdinaryActionAdmissionRequest,
    ) -> Result<OrdinaryActionBatch<'buf, PACKET_BUFFERS>, OrdinaryActionAdmissionFailure> {
        let packet_count = actions.packets.len();
        macro_rules! fail {
            ($reason:expr) => {
                return Err(OrdinaryActionAdmissionFailure {
                    reason: $reason,
                    actions,
                })
            };
        }

        for (packet_index, packet) in actions.packets.iter().enumerate() {
            if let Err(error) = Self::validate_packet_len(packet_index, packet.bytes().len()) {
                fail!(error);
            }
            let target = TxTarget::from_rns(packet.target());
            if let Err(error) = TxRoutePlan::resolve(target, request.enabled_interfaces) {
                fail!(Self::route_admission_error(packet_index, error));
            }
        }

        if packet_count == 0 {
            return Ok(OrdinaryActionBatch {
                non_packet_actions: actions,
                packets: core::array::from_fn(|_| None),
                packet_count: 0,
                next_packet: 0,
            });
        }

        if request.owner_now >= request.deadline.instant() {
            fail!(OrdinaryActionAdmissionError::DeadlineExpired {
                now: request.owner_now,
                deadline: request.deadline,
            });
        }

        let available = self
            .slots
            .iter()
            .filter(|slot| matches!(slot.state, OrdinarySlotState::Free))
            .count();
        if available < packet_count {
            fail!(OrdinaryActionAdmissionError::PoolFull {
                needed: packet_count,
                available,
            });
        }
        let supplied = pool.parked_count();
        if supplied < packet_count {
            fail!(OrdinaryActionAdmissionError::MissingBufferOwners {
                needed: packet_count,
                supplied,
            });
        }

        let Ok(packet_count_u64) = u64::try_from(packet_count) else {
            fail!(OrdinaryActionAdmissionError::IdentifierExhausted);
        };
        let Some(final_generation) = self.generation.checked_add(packet_count_u64) else {
            fail!(OrdinaryActionAdmissionError::IdentifierExhausted);
        };

        let mut selected_pool_indices: [Option<usize>; PACKET_BUFFERS] =
            core::array::from_fn(|_| None);
        let mut selected_slots: [Option<OrdinaryPacketSlotId>; PACKET_BUFFERS] =
            core::array::from_fn(|_| None);
        let mut selected_count = 0;
        for (pool_index, buffer) in pool.parked.iter().enumerate() {
            if selected_count == packet_count {
                break;
            }
            let Some(buffer) = buffer.as_deref() else {
                continue;
            };
            let slot = match self.validate_available_buffer(buffer) {
                Ok(slot) => slot,
                Err(OrdinaryAvailableBufferError::Unregistered) => {
                    fail!(OrdinaryActionAdmissionError::UnregisteredBuffer {
                        buffer_index: pool_index,
                    });
                }
                Err(OrdinaryAvailableBufferError::ForeignOwner) => {
                    fail!(OrdinaryActionAdmissionError::ForeignBuffer {
                        buffer_index: pool_index,
                    });
                }
                Err(OrdinaryAvailableBufferError::Busy) => {
                    fail!(OrdinaryActionAdmissionError::BusyBuffer {
                        buffer_index: pool_index,
                    });
                }
                Err(OrdinaryAvailableBufferError::MetadataInvariant) => {
                    fail!(OrdinaryActionAdmissionError::BufferInvariant {
                        buffer_index: pool_index,
                    });
                }
            };
            if slot.index() != pool_index || selected_slots[..selected_count].contains(&Some(slot))
            {
                fail!(OrdinaryActionAdmissionError::BufferInvariant {
                    buffer_index: pool_index,
                });
            }
            selected_pool_indices[selected_count] = Some(pool_index);
            selected_slots[selected_count] = Some(slot);
            selected_count += 1;
        }
        if selected_count < packet_count {
            fail!(OrdinaryActionAdmissionError::MissingBufferOwners {
                needed: packet_count,
                supplied: selected_count,
            });
        }

        let first_generation = self.generation + 1;
        let mut prepared_packets: [Option<OrdinaryPreparedPacket>; PACKET_BUFFERS] =
            core::array::from_fn(|_| None);
        for (packet_index, packet) in actions.packets.iter().enumerate() {
            let target = TxTarget::from_rns(packet.target());
            let mut route = TxRoutePlan::resolve(target, request.enabled_interfaces)
                .expect("preflight proved every ordinary route is nonempty");
            let interface = route
                .take_next()
                .expect("a successfully resolved route has a first interface");
            let slot = selected_slots[packet_index]
                .expect("preflight assigned one unique slot per packet");
            let generation = OrdinaryPacketGeneration(
                first_generation
                    + u64::try_from(packet_index)
                        .expect("packet index already converted as part of packet count"),
            );
            prepared_packets[packet_index] = Some(OrdinaryPreparedPacket {
                scope: self.scope,
                slot,
                generation,
                packet_len: u16::try_from(packet.bytes().len())
                    .expect("preflight proved an RNS-MTU packet length fits u16"),
                target,
                interface,
                remaining_interfaces: route.remaining(),
                protocol_token: packet.protocol_token(),
                deadline: request.deadline,
            });
        }

        // Reserve every selected owner before copying the first packet. From
        // here onward operations are infallible for the preflighted values.
        for packet_index in 0..packet_count {
            let pool_index = selected_pool_indices[packet_index]
                .expect("preflight selected one parked owner per packet");
            let prepared = prepared_packets[packet_index]
                .expect("preflight prepared one metadata owner per packet");
            let buffer = pool.parked[pool_index]
                .as_deref_mut()
                .expect("selected owner remained parked through preflight");
            buffer.binding = OrdinaryBufferBinding::Bound(prepared);
            self.slots[prepared.slot.index()].state =
                OrdinarySlotState::Active(OrdinaryDispatchRecord {
                    packet: prepared,
                    phase: OrdinaryDispatchPhase::Routed,
                    may_have_transmitted: false,
                });
        }
        self.generation = final_generation;

        let mut non_packet_actions = actions;
        let packets = core::mem::take(&mut non_packet_actions.packets);
        let mut jobs: [Option<OrdinaryTxJob<'buf>>; PACKET_BUFFERS] =
            core::array::from_fn(|_| None);
        for (packet_index, packet) in packets.into_iter().enumerate() {
            let pool_index = selected_pool_indices[packet_index]
                .expect("preflight selected one parked owner per packet");
            let buffer = pool.parked[pool_index]
                .take()
                .expect("selected owner remained parked until ownership transfer");
            let (bytes, native_target, protocol_token) = packet.into_parts();
            let prepared = prepared_packets[packet_index]
                .expect("reservation retained one metadata owner per packet");
            debug_assert_eq!(usize::from(prepared.packet_len), bytes.len());
            debug_assert_eq!(prepared.target, TxTarget::from_rns(native_target));
            debug_assert_eq!(prepared.protocol_token, protocol_token);
            buffer.bytes[..bytes.len()].copy_from_slice(&bytes);
            jobs[packet_index] = Some(OrdinaryTxJob {
                buffer,
                packet: prepared,
            });
        }

        Ok(OrdinaryActionBatch {
            non_packet_actions,
            packets: jobs,
            packet_count,
            next_packet: 0,
        })
    }

    /// Validate one ordinary permit request and invoke the shared policy.
    ///
    /// A covering reservation is the authorization linearization point: the
    /// authoritative slot becomes `Authorized` and permanently records
    /// possible transmission before the non-copy grant leaves this owner.
    /// Deadline and policy denials leave the hop definitely unpermitted.
    pub fn authorize_tx<P: TxAuthorizationPolicy>(
        &mut self,
        request: OrdinaryTxPermitRequest,
        now: MonotonicMillis,
        policy: &mut P,
    ) -> Result<OrdinaryTxPermitReply, OrdinaryAuthorizationFailure> {
        let binding = request.binding;
        if binding.scope != self.scope {
            return Err(OrdinaryAuthorizationFailure {
                reason: OrdinaryAuthorizationErrorKind::WrongOwner,
                request,
            });
        }
        let Some(state) = self.slots.get(binding.slot.index()).map(|slot| slot.state) else {
            return Err(OrdinaryAuthorizationFailure {
                reason: OrdinaryAuthorizationErrorKind::StaleOrUnknown,
                request,
            });
        };
        let OrdinarySlotState::Active(mut record) = state else {
            return Err(OrdinaryAuthorizationFailure {
                reason: OrdinaryAuthorizationErrorKind::StaleOrUnknown,
                request,
            });
        };
        if record.packet.scope != self.scope || record.packet.slot != binding.slot {
            return Err(OrdinaryAuthorizationFailure {
                reason: OrdinaryAuthorizationErrorKind::Invariant,
                request,
            });
        }
        if record.packet.generation != binding.generation
            || record.packet.interface != binding.interface
        {
            return Err(OrdinaryAuthorizationFailure {
                reason: OrdinaryAuthorizationErrorKind::StaleOrUnknown,
                request,
            });
        }
        if !matches!(record.phase, OrdinaryDispatchPhase::Routed) {
            return Err(OrdinaryAuthorizationFailure {
                reason: OrdinaryAuthorizationErrorKind::StaleOrUnknown,
                request,
            });
        }

        if now >= record.packet.deadline.instant() {
            return Ok(OrdinaryTxPermitReply::Denied(OrdinaryTxPermitDenial {
                binding,
                reason: OrdinaryTxPermitDenialReason::DeadlineExpired,
            }));
        }

        let candidate = TxAuthorizationCandidate {
            interface: record.packet.interface,
            packet_len: record.packet.packet_len,
            requirements: binding.requirements,
            now,
            deadline: record.packet.deadline,
            may_have_transmitted: record.may_have_transmitted,
        };
        match policy.authorize(candidate) {
            TxPolicyDecision::Deny(reason) => {
                Ok(OrdinaryTxPermitReply::Denied(OrdinaryTxPermitDenial {
                    binding,
                    reason: OrdinaryTxPermitDenialReason::Policy(reason),
                }))
            }
            TxPolicyDecision::Authorize(reservation)
                if reservation.covers(binding.requirements) =>
            {
                record.phase = OrdinaryDispatchPhase::Authorized;
                record.may_have_transmitted = true;
                self.slots[binding.slot.index()].state = OrdinarySlotState::Active(record);
                Ok(OrdinaryTxPermitReply::Granted(OrdinaryTxPermitGrant {
                    binding,
                    reservation,
                }))
            }
            TxPolicyDecision::Authorize(_) => {
                Ok(OrdinaryTxPermitReply::Denied(OrdinaryTxPermitDenial {
                    binding,
                    reason: OrdinaryTxPermitDenialReason::Policy(
                        TxPolicyDenial::CapacityUnavailable,
                    ),
                }))
            }
        }
    }

    /// Reconcile one typed ordinary completion into fan-out, release, or quarantine.
    ///
    /// A definitive final-hop outcome returns `RouteComplete` even at the exact
    /// deadline. The deadline suppresses only an unattempted remaining hop.
    /// Explicit cancellation suppresses every remaining interface while
    /// preserving whether any earlier or current grant may have transmitted.
    pub fn complete_tx<'a>(
        &mut self,
        completion: OrdinaryTxCompletion<'a>,
        now: MonotonicMillis,
    ) -> Result<OrdinaryCompletionDisposition<'a>, OrdinaryCompletionFailure<'a>> {
        let job = &completion.job;
        if job.packet.scope != self.scope {
            return Err(OrdinaryCompletionFailure {
                reason: OrdinaryCompletionError::ForeignOwner,
                completion,
            });
        }
        let index = job.packet.slot.index();
        let Some(state) = self.slots.get(index).map(|slot| slot.state) else {
            return Err(OrdinaryCompletionFailure {
                reason: OrdinaryCompletionError::StaleGeneration,
                completion,
            });
        };
        let OrdinarySlotState::Active(record) = state else {
            return Err(OrdinaryCompletionFailure {
                reason: OrdinaryCompletionError::StaleGeneration,
                completion,
            });
        };
        if record.packet.generation != job.packet.generation {
            return Err(OrdinaryCompletionFailure {
                reason: OrdinaryCompletionError::StaleGeneration,
                completion,
            });
        }
        if record.packet != job.packet
            || job.buffer.binding != OrdinaryBufferBinding::Bound(job.packet)
        {
            return Ok(OrdinaryCompletionDisposition::Quarantined(
                OrdinaryTxQuarantine {
                    completion,
                    reason: OrdinaryQuarantineReason::MetadataInvariant,
                },
            ));
        }

        let (completion_authorized, stop_fanout) = match completion.kind {
            OrdinaryCompletionKind::Unpermitted {
                code,
                denial,
                stop_fanout,
            } => {
                let _ = (code.get(), denial);
                (false, stop_fanout)
            }
            OrdinaryCompletionKind::Authorized {
                code, stop_fanout, ..
            } => {
                let _ = code.get();
                (true, stop_fanout)
            }
            OrdinaryCompletionKind::RecoveryFault(code) => {
                return Ok(OrdinaryCompletionDisposition::Quarantined(
                    OrdinaryTxQuarantine {
                        completion,
                        reason: OrdinaryQuarantineReason::RecoveryFault(code),
                    },
                ));
            }
        };
        let expected_authorized = matches!(record.phase, OrdinaryDispatchPhase::Authorized);
        if completion_authorized != expected_authorized {
            return Ok(OrdinaryCompletionDisposition::Quarantined(
                OrdinaryTxQuarantine {
                    completion,
                    reason: OrdinaryQuarantineReason::CompletionClassInvariant,
                },
            ));
        }

        let mut remaining = record.packet.remaining_interfaces;
        if remaining.is_empty() {
            return Ok(OrdinaryCompletionDisposition::Returned(self.release_job(
                completion.job,
                OrdinaryPacketReturnReason::RouteComplete,
                record.may_have_transmitted,
            )));
        }

        if stop_fanout {
            return Ok(OrdinaryCompletionDisposition::Returned(self.release_job(
                completion.job,
                OrdinaryPacketReturnReason::Cancelled,
                record.may_have_transmitted,
            )));
        }

        if now >= record.packet.deadline.instant() {
            return Ok(OrdinaryCompletionDisposition::Returned(self.release_job(
                completion.job,
                OrdinaryPacketReturnReason::DeadlineExpired,
                record.may_have_transmitted,
            )));
        }

        let interface = remaining
            .take_first()
            .expect("nonempty remaining route has a selected interface");
        let packet = OrdinaryPreparedPacket {
            interface,
            remaining_interfaces: remaining,
            ..record.packet
        };
        self.slots[packet.slot.index()].state = OrdinarySlotState::Active(OrdinaryDispatchRecord {
            packet,
            phase: OrdinaryDispatchPhase::Routed,
            may_have_transmitted: record.may_have_transmitted,
        });
        completion.job.buffer.binding = OrdinaryBufferBinding::Bound(packet);
        Ok(OrdinaryCompletionDisposition::Next(OrdinaryTxJob {
            buffer: completion.job.buffer,
            packet,
        }))
    }

    /// Read ordinary owner occupancy without observing DATA ledger capacity.
    pub fn capacities(&self) -> OrdinaryActionCapacitySnapshot {
        let mut registered = 0;
        let mut active = 0;
        for slot in &self.slots {
            match slot.state {
                OrdinarySlotState::Unregistered => {}
                OrdinarySlotState::Free => registered += 1,
                OrdinarySlotState::Active(_) => {
                    registered += 1;
                    active += 1;
                }
            }
        }
        OrdinaryActionCapacitySnapshot {
            registered,
            active,
            limit: PACKET_BUFFERS,
        }
    }

    /// Earliest deadline among all active ordinary packet owners.
    ///
    /// This is a bounded wake source only. Reaching the deadline never
    /// fabricates, releases, or reclaims a missing unique buffer owner.
    pub fn next_deadline(&self) -> Option<TxLeaseDeadline> {
        self.slots
            .iter()
            .filter_map(|slot| match slot.state {
                OrdinarySlotState::Active(record) => Some(record.packet.deadline),
                OrdinarySlotState::Unregistered | OrdinarySlotState::Free => None,
            })
            .min()
    }

    fn release_job<'a>(
        &mut self,
        job: OrdinaryTxJob<'a>,
        reason: OrdinaryPacketReturnReason,
        may_have_transmitted: bool,
    ) -> OrdinaryPacketReturn<'a> {
        let packet = job.packet;
        self.slots[packet.slot.index()].state = OrdinarySlotState::Free;
        job.buffer.binding = OrdinaryBufferBinding::Available {
            scope: self.scope,
            slot: packet.slot,
        };
        OrdinaryPacketReturn {
            buffer: job.buffer,
            packet,
            reason,
            may_have_transmitted,
        }
    }

    fn route_admission_error(
        packet_index: usize,
        error: TxRouteError,
    ) -> OrdinaryActionAdmissionError {
        match error {
            TxRouteError::InterfaceOutsideProfile { interface } => {
                OrdinaryActionAdmissionError::InterfaceOutsideProfile {
                    packet_index,
                    interface,
                }
            }
            TxRouteError::NoEligibleInterface { target } => {
                OrdinaryActionAdmissionError::NoEligibleInterface {
                    packet_index,
                    target,
                }
            }
        }
    }

    fn validate_packet_len(
        packet_index: usize,
        packet_len: usize,
    ) -> Result<(), OrdinaryActionAdmissionError> {
        if packet_len > PACKET_CAPACITY {
            Err(OrdinaryActionAdmissionError::PacketTooLarge {
                packet_index,
                actual: packet_len,
                maximum: PACKET_CAPACITY,
            })
        } else {
            Ok(())
        }
    }

    fn next_registration_slot(
        &self,
    ) -> Result<(usize, OrdinaryPacketSlotId), OrdinaryBufferRegistrationError> {
        if PACKET_BUFFERS == 0 {
            return Err(OrdinaryBufferRegistrationError::PoolFull {
                limit: PACKET_BUFFERS,
            });
        }

        let mut index = self.next_registration;
        let mut free = None;
        for _ in 0..PACKET_BUFFERS {
            if matches!(self.slots[index].state, OrdinarySlotState::Unregistered) {
                free = Some(index);
                break;
            }
            index = Self::next_slot_index(index);
        }
        let index = free.ok_or(OrdinaryBufferRegistrationError::PoolFull {
            limit: PACKET_BUFFERS,
        })?;
        let raw = u16::try_from(index).map_err(|_| OrdinaryBufferRegistrationError::PoolFull {
            limit: PACKET_BUFFERS,
        })?;
        Ok((index, OrdinaryPacketSlotId(raw)))
    }

    fn next_slot_index(index: usize) -> usize {
        if index + 1 == PACKET_BUFFERS {
            0
        } else {
            index + 1
        }
    }
}

#[cfg(test)]
mod tests;
